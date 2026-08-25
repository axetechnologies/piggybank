use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;

mod json;
mod markers;
mod session;
mod text;
pub use json::{
    compress_json, compress_json_with_store, decompress_json, decompress_json_with_store,
};
pub use session::Session;
pub use text::{compress_text, decompress_text, TextOptions};

/// Content-addressed blob store. Every write is keyed by the sha256 of its
/// bytes, so writing the same content twice is a no-op and nothing already
/// on disk is ever overwritten. This is the one primitive that makes
/// compression reversible: a compressor never has to decide what's safe to
/// throw away, because nothing it stores here is ever thrown away.
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn open(root: impl Into<PathBuf>) -> std::io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn put(&self, bytes: &[u8]) -> std::io::Result<String> {
        self.put_check_existing(bytes).map(|(id, _)| id)
    }

    /// Like `put`, but also reports whether this exact content was already
    /// present *before* this call — the primitive cross-call deduplication
    /// needs: "have I seen this value before, anywhere, ever" rather than
    /// just "make sure it's stored." See `json::compress_json_with_store`.
    pub fn put_check_existing(&self, bytes: &[u8]) -> std::io::Result<(String, bool)> {
        let id = hex::encode(Sha256::digest(bytes));
        let path = self.root.join(&id);
        let already_existed = path.exists();
        if !already_existed {
            // fs::write alone isn't atomic - a process killed mid-write
            // (OOM, crash, power loss) could leave a truncated file at
            // `path` whose name (the content hash, computed from the
            // intended bytes up front) no longer matches what's actually
            // on disk, and nothing on read would ever detect that
            // mismatch. Write to a temp file first, then atomically rename
            // into place: a reader only ever sees either nothing or the
            // complete, correct content, never a partial write. The temp
            // name is namespaced by both the content id and this process's
            // pid, so concurrent `put`s of the *same* content from
            // different processes can't collide on the temp path, and
            // whichever rename lands last still leaves matching content
            // (same hash implies same bytes).
            let tmp_path = self.root.join(format!("{id}.tmp.{}", std::process::id()));
            fs::write(&tmp_path, bytes)?;
            fs::rename(&tmp_path, &path)?;
            // Best-effort audit trail: record when this content was first
            // ever seen. Deliberately not part of the atomic write above
            // and deliberately not allowed to fail this call - the content
            // write is the correctness-critical path (get() must see it),
            // provenance is a secondary record about it. A disk-full or
            // permissions hiccup on the log must not turn a successful
            // store write into a failed one.
            self.record_first_seen(&id);
        }
        Ok((id, already_existed))
    }

    /// Append `{"id":..., "first_seen_unix":...}` to `.provenance.jsonl`.
    /// Best-effort: errors are swallowed, never propagated - see the call
    /// site's comment for why. Only ever called for genuinely new content
    /// (once per distinct id, since `put_check_existing` only reaches this
    /// when `already_existed` was false), so this is a true first-sight
    /// record, not a per-access log.
    fn record_first_seen(&self, id: &str) {
        let Ok(duration) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        else {
            return;
        };
        let record = serde_json::json!({ "id": id, "first_seen_unix": duration.as_secs() });
        let line = format!("{record}\n");
        if let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join(".provenance.jsonl"))
        {
            use std::io::Write;
            let _ = file.write_all(line.as_bytes());
        }
    }

    /// Look up when `id` was first ever written to this store. `Ok(None)`
    /// covers both "no such content" and "written before provenance
    /// tracking existed" - a linear scan of an append-only log, which is
    /// fine at the scale this is meant for (an audit lookup, not a hot
    /// path); revisit only if a real long-lived store's log size makes
    /// that untrue.
    pub fn first_seen(&self, id: &str) -> std::io::Result<Option<u64>> {
        if !is_valid_id(id) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid content id",
            ));
        }
        let path = self.root.join(".provenance.jsonl");
        let contents = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        for line in contents.lines() {
            let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
                continue; // a torn last line from a concurrent writer, or similar - skip, don't fail the lookup
            };
            if record.get("id").and_then(|v| v.as_str()) == Some(id) {
                return Ok(record.get("first_seen_unix").and_then(|v| v.as_u64()));
            }
        }
        Ok(None)
    }

    /// Fetch content by id. Confirmed real, not theoretical: before this
    /// validation existed, `get("../../../../etc/passwd")` or an absolute
    /// path (`PathBuf::join` doesn't sanitize either — an absolute joined
    /// path *replaces* the base entirely) read arbitrary files off the
    /// filesystem, reachable straight through the MCP `boomerang_retrieve`
    /// tool. `put` always generates its own id via `hex::encode(sha256)`,
    /// so it's never handed an externally-controlled path fragment — only
    /// `get` needed this, but it needed it badly.
    pub fn get(&self, id: &str) -> std::io::Result<Vec<u8>> {
        if !is_valid_id(id) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid content id",
            ));
        }
        fs::read(self.root.join(id))
    }

    /// Count entries and total bytes actually held in the store. Filters to
    /// files whose names look like our sha256-hex ids so sidecar files
    /// living in the same directory (e.g. `Session`'s `.session.json`)
    /// don't get counted as content.
    pub fn stats(&self) -> std::io::Result<StoreStats> {
        let mut entries = 0usize;
        let mut bytes = 0u64;
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let is_content_id = name.to_str().is_some_and(is_valid_id);
            if is_content_id && entry.file_type()?.is_file() {
                entries += 1;
                bytes += entry.metadata()?.len();
            }
        }
        Ok(StoreStats { entries, bytes })
    }
}

/// A valid content id is exactly what `hex::encode(Sha256::digest(_))`
/// always produces: 64 lowercase hex characters, nothing else — no `/`, no
/// `..`, no leading `/` (which would make it absolute). Shared by `get` and
/// `stats` so "what counts as a real id" can't drift between the two.
fn is_valid_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

pub struct StoreStats {
    pub entries: usize,
    pub bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_put_get_round_trips_and_dedupes() {
        let dir = std::env::temp_dir().join(format!("boomerang-test-{}", std::process::id()));
        let store = Store::open(&dir).unwrap();
        let id1 = store.put(b"hello world").unwrap();
        let id2 = store.put(b"hello world").unwrap(); // identical content -> identical id, no rewrite
        assert_eq!(id1, id2);
        assert_eq!(store.get(&id1).unwrap(), b"hello world");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn store_stats_counts_content_ignores_non_id_files() {
        let dir = std::env::temp_dir().join(format!("boomerang-stats-test-{}", std::process::id()));
        let store = Store::open(&dir).unwrap();
        store.put(b"hello").unwrap();
        store.put(b"world!!").unwrap();
        store.put(b"hello").unwrap(); // dedup: same id as the first, not a third entry
        fs::write(dir.join(".session.json"), b"{}").unwrap(); // sidecar, must be ignored

        let stats = store.stats().unwrap();
        assert_eq!(stats.entries, 2);
        assert_eq!(stats.bytes, 5 + 7);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn put_leaves_no_orphaned_temp_file_and_content_is_readable_immediately() {
        // put() now writes to a temp file and renames into place (fixed
        // fs::write alone not being atomic - see put()'s doc comment).
        // Confirms the happy path leaves exactly the final content file
        // plus the provenance sidecar, no leftover .tmp, and get() sees it
        // right away.
        let dir =
            std::env::temp_dir().join(format!("boomerang-atomic-test-{}", std::process::id()));
        let store = Store::open(&dir).unwrap();
        let id = store.put(b"atomic write check").unwrap();
        assert_eq!(store.get(&id).unwrap(), b"atomic write check");

        let mut entries: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        entries.sort();
        let mut expected = vec![id, ".provenance.jsonl".to_string()];
        expected.sort();
        assert_eq!(
            entries, expected,
            "no leftover .tmp file after a successful put"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn get_rejects_path_traversal_and_absolute_paths() {
        // Confirmed real (not theoretical) before this validation existed:
        // reachable straight through the MCP boomerang_retrieve tool with
        // an attacker-controlled `ref`, this read files far outside the
        // store directory.
        let dir =
            std::env::temp_dir().join(format!("boomerang-traversal-test-{}", std::process::id()));
        let store = Store::open(&dir).unwrap();
        store.put(b"legitimate content").unwrap();

        for malicious_id in [
            "../../../../../../etc/passwd",
            "/etc/passwd",
            "..",
            "",
            "not-hex-but-64-characters-long-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
        ] {
            let result = store.get(malicious_id);
            assert!(
                result.is_err(),
                "get({malicious_id:?}) must be rejected, not attempt a filesystem read"
            );
            assert_eq!(
                result.unwrap_err().kind(),
                std::io::ErrorKind::InvalidInput,
                "must be rejected as invalid input, not fail for some other reason"
            );
        }
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn first_seen_records_new_content_and_is_stable_across_duplicate_puts() {
        let dir =
            std::env::temp_dir().join(format!("boomerang-provenance-test-{}", std::process::id()));
        let store = Store::open(&dir).unwrap();

        // Content never written: no record, not an error.
        let never_written = "a".repeat(64);
        assert_eq!(store.first_seen(&never_written).unwrap(), None);

        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let id = store.put(b"provenance check").unwrap();
        let after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let first_seen = store.first_seen(&id).unwrap().expect("must be recorded");
        assert!(
            (before..=after).contains(&first_seen),
            "recorded timestamp {first_seen} must fall within [{before}, {after}]"
        );

        // Re-putting the same content must not create a second record with
        // a later timestamp - it's "first seen," not "last seen."
        std::thread::sleep(std::time::Duration::from_millis(1100));
        store.put(b"provenance check").unwrap();
        assert_eq!(
            store.first_seen(&id).unwrap(),
            Some(first_seen),
            "re-putting identical content must not change its first-seen record"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn first_seen_rejects_invalid_ids_same_as_get() {
        let dir = std::env::temp_dir().join(format!(
            "boomerang-provenance-validation-test-{}",
            std::process::id()
        ));
        let store = Store::open(&dir).unwrap();
        let result = store.first_seen("../../../etc/passwd");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
        fs::remove_dir_all(&dir).ok();
    }
}
