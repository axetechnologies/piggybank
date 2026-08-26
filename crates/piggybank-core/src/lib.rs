use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;

mod json;
mod markers;
mod session;
mod text;
pub use json::{
    compress_json, compress_json_with_store, decompress_json, decompress_json_with_store,
    verify_json_with_store,
};
pub use session::Session;
pub use text::{
    compress_text, compress_text_budget, decompress_text, verify_text_with_store, TextOptions,
};

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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        }
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
    /// filesystem, reachable straight through the MCP `piggybank_retrieve`
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

    /// Check whether `id` is present, without reading its content - a
    /// filesystem stat, not a read. Cheaper than `get` for a caller that
    /// only needs to confirm reachability, not the bytes themselves - see
    /// `VerifyResult`, which is built entirely on this instead of `get`.
    pub fn exists(&self, id: &str) -> std::io::Result<bool> {
        if !is_valid_id(id) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid content id",
            ));
        }
        Ok(self.root.join(id).exists())
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

    /// Delete content first seen before `older_than_unix` (Unix seconds).
    /// Age-based, deliberately: the only policy `Store` can apply safely.
    /// It has no way to know whether some compressed view sitting in an
    /// agent's conversation history, a log file, or another store entirely
    /// still depends on a given id — a true reachability sweep would need
    /// that global picture, which nothing here has or could have. Age is a
    /// blunt instrument compared to that, but it's a *safe* one: it never
    /// second-guesses content, only how long it's been sitting there.
    /// `piggybank_verify` exists precisely so a caller can check, before or
    /// after a GC run, whether something it still cares about survived.
    ///
    /// Entries with no provenance record (written before provenance
    /// tracking existed, or if the log was ever lost) are never deleted -
    /// with no recorded age, there's no basis to decide they're eligible,
    /// and "don't destroy what you can't date" is the conservative choice.
    ///
    /// `dry_run: true` reports exactly what *would* be deleted - which ids,
    /// how many bytes - without touching anything. This is deliberately
    /// never wired into the MCP server: an agent should not be able to
    /// delete shared, possibly-multi-tenant store content on its own
    /// initiative. It's a CLI-only, explicit, human-invoked operation.
    ///
    /// One thing age alone can't see: a stored blob's own bytes can
    /// literally embed another blob's id (`json.rs`'s cross-call
    /// interning does this - a promoted value can reference an
    /// already-promoted child). Confirmed as a real bug, not a
    /// theoretical one, before this protection existed: deleting an old
    /// referenced blob while keeping a newer blob that references it left
    /// the survivor internally broken, even though GC never touched it and
    /// it passed its own age check. `Store` deliberately doesn't know
    /// about any compressor's marker format, so protection here is
    /// generic: any surviving blob's raw bytes are scanned for another
    /// id appearing as a plain substring, and a hit protects that id
    /// regardless of its own age - propagated to a fixed point, since a
    /// newly-protected blob can itself reference yet another old one.
    pub fn gc(&self, older_than_unix: u64, dry_run: bool) -> std::io::Result<GcResult> {
        let provenance_path = self.root.join(".provenance.jsonl");
        let mut ages: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        if let Ok(contents) = fs::read_to_string(&provenance_path) {
            for line in contents.lines() {
                if let Ok(record) = serde_json::from_str::<serde_json::Value>(line) {
                    if let (Some(id), Some(ts)) = (
                        record.get("id").and_then(|v| v.as_str()),
                        record.get("first_seen_unix").and_then(|v| v.as_u64()),
                    ) {
                        ages.insert(id.to_string(), ts);
                    }
                }
            }
        }

        let mut result = GcResult::default();
        let mut sizes: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        let mut candidates: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut survivors: Vec<String> = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(id) = name.to_str().filter(|s| is_valid_id(s)) else {
                continue;
            };
            sizes.insert(id.to_string(), entry.metadata()?.len());
            match ages.get(id) {
                Some(&first_seen) if first_seen < older_than_unix => {
                    candidates.insert(id.to_string());
                }
                Some(_) => survivors.push(id.to_string()),
                None => {
                    result.skipped_no_provenance += 1;
                    survivors.push(id.to_string()); // no recorded age -> never eligible, always a survivor
                }
            }
        }

        // Fixed-point protection: scan every survivor's content for any
        // remaining candidate id; a match protects it (removes it from
        // candidates, adds it to the worklist so ITS content gets scanned
        // too, since it's now a survivor in its own right).
        let mut worklist = survivors;
        while let Some(survivor_id) = worklist.pop() {
            let Ok(bytes) = fs::read(self.root.join(&survivor_id)) else {
                continue; // shouldn't happen for something we just listed, but not fatal to the sweep
            };
            let text = String::from_utf8_lossy(&bytes);
            let referenced: Vec<String> = candidates
                .iter()
                .filter(|id| text.contains(id.as_str()))
                .cloned()
                .collect();
            for id in referenced {
                candidates.remove(&id);
                worklist.push(id);
            }
        }

        let mut deleted_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        for id in candidates {
            let size = *sizes.get(&id).unwrap_or(&0);
            if !dry_run {
                fs::remove_file(self.root.join(&id))?;
            }
            result.deleted += 1;
            result.freed_bytes += size;
            deleted_ids.insert(id);
        }

        if !dry_run && !deleted_ids.is_empty() {
            // Rewrite the provenance log without the deleted ids' records,
            // via the same write-temp-then-atomic-rename pattern put() uses
            // - a reader must never see a torn/partial log.
            let remaining: String = ages
                .iter()
                .filter(|(id, _)| !deleted_ids.contains(id.as_str()))
                .map(|(id, ts)| {
                    format!(
                        "{}\n",
                        serde_json::json!({ "id": id, "first_seen_unix": ts })
                    )
                })
                .collect();
            let tmp_path = self
                .root
                .join(format!(".provenance.jsonl.tmp.{}", std::process::id()));
            fs::write(&tmp_path, remaining)?;
            fs::rename(&tmp_path, &provenance_path)?;
        }

        Ok(result)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcResult {
    pub deleted: usize,
    pub freed_bytes: u64,
    /// Entries with no provenance record - never deleted, see `gc`'s doc
    /// comment for why. Surfaced so an operator can see how much of the
    /// store this GC policy simply can't make a decision about.
    pub skipped_no_provenance: usize,
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

/// Result of checking a compressed view's reference chain against a store,
/// without fully decompressing it. Every one of the three compressors'
/// `verify` functions builds this the same way: walk the view for
/// store-backed reference markers, `Store::exists` each one (a stat, not a
/// read), and report what's missing rather than failing outright - a
/// caller can decide whether a few missing refs matter for what it's about
/// to do (e.g. a GC dry run) versus needing everything present (e.g. about
/// to promise a client full reconstruction is possible).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VerifyResult {
    pub ok: bool,
    pub checked_refs: usize,
    pub missing_refs: Vec<String>,
}

impl VerifyResult {
    pub(crate) fn check(&mut self, store: &Store, id: &str) -> std::io::Result<()> {
        self.checked_refs += 1;
        if !store.exists(id)? {
            self.missing_refs.push(id.to_string());
        }
        Ok(())
    }

    pub(crate) fn finish(mut self) -> Self {
        self.ok = self.missing_refs.is_empty();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_put_get_round_trips_and_dedupes() {
        let dir = std::env::temp_dir().join(format!("piggybank-test-{}", std::process::id()));
        let store = Store::open(&dir).unwrap();
        let id1 = store.put(b"hello world").unwrap();
        let id2 = store.put(b"hello world").unwrap(); // identical content -> identical id, no rewrite
        assert_eq!(id1, id2);
        assert_eq!(store.get(&id1).unwrap(), b"hello world");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn store_stats_counts_content_ignores_non_id_files() {
        let dir = std::env::temp_dir().join(format!("piggybank-stats-test-{}", std::process::id()));
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
            std::env::temp_dir().join(format!("piggybank-atomic-test-{}", std::process::id()));
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
        // reachable straight through the MCP piggybank_retrieve tool with
        // an attacker-controlled `ref`, this read files far outside the
        // store directory.
        let dir =
            std::env::temp_dir().join(format!("piggybank-traversal-test-{}", std::process::id()));
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
            std::env::temp_dir().join(format!("piggybank-provenance-test-{}", std::process::id()));
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
            "piggybank-provenance-validation-test-{}",
            std::process::id()
        ));
        let store = Store::open(&dir).unwrap();
        let result = store.first_seen("../../../etc/passwd");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn exists_matches_get_availability_and_rejects_invalid_ids() {
        let dir =
            std::env::temp_dir().join(format!("piggybank-exists-test-{}", std::process::id()));
        let store = Store::open(&dir).unwrap();

        let never_written = "b".repeat(64);
        assert!(!store.exists(&never_written).unwrap());

        let id = store.put(b"exists check").unwrap();
        assert!(store.exists(&id).unwrap());

        let result = store.exists("../../../etc/passwd");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidInput);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn gc_deletes_only_content_older_than_cutoff_with_a_recorded_age() {
        let dir = std::env::temp_dir().join(format!("piggybank-gc-test-{}", std::process::id()));
        let store = Store::open(&dir).unwrap();

        let old_id = store.put(b"old content").unwrap();
        let new_id = store.put(b"new content").unwrap();

        // Backdate old_id's provenance record; leave new_id far in the
        // future so it's unambiguously "not old" regardless of when this
        // test actually runs.
        let provenance_path = dir.join(".provenance.jsonl");
        let rewritten = format!(
            "{}\n{}\n",
            serde_json::json!({"id": old_id, "first_seen_unix": 1000u64}),
            serde_json::json!({"id": new_id, "first_seen_unix": 9_999_999_999u64}),
        );
        fs::write(&provenance_path, rewritten).unwrap();

        // Content with no provenance record at all - written directly,
        // bypassing put() - simulates something stored before provenance
        // tracking existed. Must survive: no recorded age, no basis to
        // decide it's eligible.
        let unrecorded_content: &[u8] = b"unrecorded content";
        let unrecorded_id = hex::encode(Sha256::digest(unrecorded_content));
        fs::write(dir.join(&unrecorded_id), unrecorded_content).unwrap();

        let result = store.gc(5000, false).unwrap();
        assert_eq!(result.deleted, 1);
        assert_eq!(result.freed_bytes, "old content".len() as u64);
        assert_eq!(result.skipped_no_provenance, 1);

        assert!(!store.exists(&old_id).unwrap(), "old content must be gone");
        assert!(store.exists(&new_id).unwrap(), "new content must survive");
        assert!(
            store.exists(&unrecorded_id).unwrap(),
            "content with no recorded age must survive"
        );

        // The provenance log itself must have dropped the deleted entry
        // but kept the survivor's record intact.
        assert_eq!(store.first_seen(&old_id).unwrap(), None);
        assert_eq!(store.first_seen(&new_id).unwrap(), Some(9_999_999_999));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn gc_dry_run_reports_without_deleting_or_rewriting_provenance() {
        let dir =
            std::env::temp_dir().join(format!("piggybank-gc-dryrun-test-{}", std::process::id()));
        let store = Store::open(&dir).unwrap();
        let id = store.put(b"should survive the dry run").unwrap();
        fs::write(
            dir.join(".provenance.jsonl"),
            format!(
                "{}\n",
                serde_json::json!({"id": id, "first_seen_unix": 1000u64})
            ),
        )
        .unwrap();

        let result = store.gc(999_999_999, true).unwrap();
        assert_eq!(
            result.deleted, 1,
            "dry run still reports what would be deleted"
        );
        assert!(
            store.exists(&id).unwrap(),
            "dry run must not actually delete anything"
        );
        assert_eq!(
            store.first_seen(&id).unwrap(),
            Some(1000),
            "dry run must not rewrite provenance either"
        );

        fs::remove_dir_all(&dir).ok();
    }
}
