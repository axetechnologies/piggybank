use crate::markers::{escape_lines, strip_pua, unescape_lines, PUA};
use crate::Store;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::io::{self, Error, ErrorKind};
use std::path::{Path, PathBuf};

/// Cap on `lcs_diff`'s DP table size ((old_lines+1)*(new_lines+1) usize
/// cells) before `compress` gives up on diffing and falls back to sending
/// full content instead. Measured, not guessed: 5,000x5,000 lines (25M
/// cells) cost ~250MB peak RSS; 15,000x15,000 (225M cells) cost ~2GB. This
/// cap (~16M cells) keeps worst-case memory in the ~150-250MB range while
/// still covering the large majority of real files/tool output. Losing the
/// diff on huge files costs compression ratio, never correctness.
const MAX_DIFF_CELLS: u64 = 16_000_000;

/// A session tracks, per logical key (typically a file path), the content
/// id of whatever was last shown under that key. The next time the same
/// key is compressed, only what changed is sent — a diff against the
/// previous version, or nothing at all if it's byte-identical — instead of
/// resending content an agent has already seen this session.
///
/// Keyed by a caller-supplied key rather than by content, so a renamed
/// file with unchanged content is still recognized as unchanged *if the
/// caller passes the new path* — recognizing that case for free (without
/// the caller telling us) would need content-addressed lookup across all
/// keys, which is a real extension but not needed for the common case of
/// "the same file, re-read."
pub struct Session {
    store: Store,
    last_seen: RefCell<HashMap<String, String>>,
    state_path: Option<PathBuf>,
}

impl Session {
    /// In-memory only — state does not survive past this `Session` value.
    /// Fine for embedding in a single long-lived process (e.g. an MCP
    /// server); not useful across separate CLI invocations, since each one
    /// starts a fresh process. Use `open` for that.
    pub fn new(store: Store) -> Self {
        Self {
            store,
            last_seen: RefCell::new(HashMap::new()),
            state_path: None,
        }
    }

    /// Open a session whose per-key "last seen" state is persisted on disk
    /// alongside the store, so it survives process restarts — including
    /// separate CLI invocations, which is the only way a CLI can
    /// meaningfully demonstrate diffing against a prior run at all.
    pub fn open(store_dir: impl AsRef<Path>) -> io::Result<Self> {
        let dir = store_dir.as_ref();
        let store = Store::open(dir)?;
        let state_path = dir.join(".session.json");
        let last_seen = fs::read(&state_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Ok(Self {
            store,
            last_seen: RefCell::new(last_seen),
            state_path: Some(state_path),
        })
    }

    fn persist(&self) -> io::Result<()> {
        let Some(path) = &self.state_path else {
            return Ok(());
        };
        let bytes = serde_json::to_vec(&*self.last_seen.borrow())
            .map_err(|e| Error::new(ErrorKind::InvalidData, e))?;
        fs::write(path, bytes)
    }

    /// Compress `content` under `key` relative to whatever this session
    /// last saw under that same key.
    ///
    /// - First time this key is seen: content passes through unchanged
    ///   (there's nothing to diff against yet) and gets recorded.
    /// - Identical to last time: a single short "unchanged" marker.
    /// - Changed: a compact diff against the previous version, which
    ///   `decompress` can replay against the stored previous content to
    ///   reconstruct the new content exactly.
    ///
    /// The *view* returned here (not what's written to `store`, which is
    /// always the raw original) is escaped line-by-line before it goes out,
    /// and unescaped by `decompress` — see `markers::escape_lines`. Without
    /// this, genuine content containing a line shaped like one of our own
    /// SAME/DELETE/INSERT/UNCHANGED/DIFF markers would be misinterpreted on
    /// the way back out, the same class of bug this closed in `text.rs`.
    pub fn compress(&self, key: &str, content: &[u8]) -> io::Result<Vec<u8>> {
        let new_id = self.store.put(content)?;
        let previous_id = self.last_seen.borrow().get(key).cloned();
        self.last_seen
            .borrow_mut()
            .insert(key.to_string(), new_id.clone());
        self.persist()?;

        match previous_id {
            None => Ok(escape_for_view(content)),
            Some(prev_id) if prev_id == new_id => Ok(unchanged_marker(&prev_id).into_bytes()),
            Some(prev_id) => {
                let old_text = utf8(self.store.get(&prev_id)?)?;
                let new_text = std::str::from_utf8(content)
                    .map_err(|e| Error::new(ErrorKind::InvalidData, e))?;
                let old_escaped = escape_lines(&old_text);
                let new_escaped = escape_lines(new_text);
                let old_lines: Vec<&str> = old_escaped.split('\n').collect();
                let new_lines: Vec<&str> = new_escaped.split('\n').collect();

                // lcs_diff's DP table is (old_lines+1)*(new_lines+1) usize
                // cells - a function of line *counts* only, not how similar
                // the two versions are. A 15k-line file with a single
                // changed line costs the same ~2GB as two maximally
                // different 15k-line files, measured, not assumed. Past
                // this cap, skip the diff and fall back to the always-safe,
                // always-correct option: send the full (escaped) content,
                // same as first sight. Costs the compression win on very
                // large changed files; never costs correctness or risks OOM.
                let cell_count = (old_lines.len() as u64 + 1) * (new_lines.len() as u64 + 1);
                if cell_count > MAX_DIFF_CELLS {
                    return Ok(escape_for_view(content));
                }

                let ops = lcs_diff(&old_lines, &new_lines);
                let mut out = diff_marker(&prev_id);
                out.push('\n');
                out.push_str(&format_diff(&ops));
                Ok(out.into_bytes())
            }
        }
    }

    /// Reconstruct exactly what `compress` was given, using the same store
    /// the session (or an equivalent one opened on the same directory) has
    /// been writing to. Content with no recognized marker is assumed to be
    /// first-sight passthrough — unescaped and returned.
    pub fn decompress(&self, compressed: &[u8]) -> io::Result<Vec<u8>> {
        let Ok(text) = std::str::from_utf8(compressed) else {
            return Ok(compressed.to_vec());
        };

        if let Some(id) = strip_pua(text, "BOOMERANG:UNCHANGED:") {
            // Store content is always raw/unescaped - nothing to undo.
            return self.store.get(id);
        }

        if let Some((first, rest)) = text.split_once('\n') {
            if let Some(prev_id) = strip_pua(first, "BOOMERANG:DIFF:") {
                let old_text = utf8(self.store.get(prev_id)?)?;
                // Must match exactly what compress()'s diff branch built
                // old_lines from, or the diff ops won't line up.
                let old_escaped = escape_lines(&old_text);
                let old_lines: Vec<&str> = old_escaped.split('\n').collect();
                let ops = parse_diff(rest)
                    .ok_or_else(|| Error::new(ErrorKind::InvalidData, "malformed diff"))?;
                let reconstructed = apply_diff(&old_lines, &ops).join("\n");
                return Ok(unescape_lines(&reconstructed).into_bytes());
            }
        }

        Ok(unescape_lines(text).into_bytes())
    }

    /// Confirm the previous version a compressed view depends on (an
    /// UNCHANGED or DIFF marker's referenced id) still resolves in the
    /// store, without replaying the diff or fetching the full content -
    /// `Store::exists`, not `Store::get`. First-sight passthrough views
    /// reference nothing and always verify clean.
    pub fn verify(&self, compressed: &[u8]) -> io::Result<crate::VerifyResult> {
        let mut result = crate::VerifyResult::default();
        let Ok(text) = std::str::from_utf8(compressed) else {
            return Ok(result.finish());
        };
        if let Some(id) = strip_pua(text, "BOOMERANG:UNCHANGED:") {
            result.check(&self.store, id)?;
        } else if let Some((first, _rest)) = text.split_once('\n') {
            if let Some(prev_id) = strip_pua(first, "BOOMERANG:DIFF:") {
                result.check(&self.store, prev_id)?;
            }
        }
        Ok(result.finish())
    }

    /// Append-only compression: the caller sends only *new* bytes, and
    /// Boomerang concatenates them with whatever was last stored under
    /// `key`. The full accumulated content is written to the store (so
    /// a future `compress` or `decompress` against this key sees
    /// everything), but the returned view contains only the new bytes
    /// — the caller already has the prior content in context and
    /// doesn't need it resent.
    ///
    /// First call under a key behaves identically to `compress` with
    /// no prior state: the new content IS the full content.
    ///
    /// The separator between old and new content is `\n` — append is
    /// a line-oriented operation (logs, build output, streaming
    /// results). Non-line-oriented content should use `compress` with
    /// session diffing instead.
    pub fn append(&self, key: &str, new_content: &[u8]) -> io::Result<Vec<u8>> {
        let previous_id = self.last_seen.borrow().get(key).cloned();
        let full = match previous_id {
            Some(prev_id) => {
                let mut old = self.store.get(&prev_id)?;
                if !old.is_empty() && !new_content.is_empty() {
                    old.push(b'\n');
                }
                old.extend_from_slice(new_content);
                old
            }
            None => new_content.to_vec(),
        };
        let new_id = self.store.put(&full)?;
        self.last_seen
            .borrow_mut()
            .insert(key.to_string(), new_id);
        self.persist()?;
        Ok(escape_for_view(new_content))
    }

    /// Check whether the content last compressed under `key` matches a
    /// caller-supplied sha256 hex hash, without the caller needing to
    /// resend the content itself. Returns `(changed, known)`:
    ///
    /// - `known`: whether `key` has ever been compressed in this session.
    /// - `changed`: `true` only when `key` is known AND the stored hash
    ///   differs from `hash`. `false` for unknown keys (nothing to compare
    ///   against) and for matching hashes.
    ///
    /// `hash` must be a 64-character lowercase hex string (the format
    /// `Store::put` produces). Anything else is rejected as invalid rather
    /// than silently compared — a non-hex string can never match a real
    /// store id, so "not changed" would be a lie and "changed" would be
    /// misleading; better to tell the caller their input is wrong.
    pub fn check_changed(&self, key: &str, hash: &str) -> Result<(bool, bool), &'static str> {
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("invalid hash: expected 64 lowercase hex characters (sha256)");
        }
        match self.last_seen.borrow().get(key) {
            None => Ok((false, false)),
            Some(stored) => Ok((stored != hash, true)),
        }
    }
}

/// Escape `content`'s lines for use as a compressed view, if it's UTF-8
/// text; non-UTF-8 content has no line structure to exploit and passes
/// through untouched (matches `decompress`'s own non-UTF-8 short-circuit,
/// which never attempts to unescape it either).
fn escape_for_view(content: &[u8]) -> Vec<u8> {
    match std::str::from_utf8(content) {
        Ok(text) => escape_lines(text).into_bytes(),
        Err(_) => content.to_vec(),
    }
}

fn utf8(bytes: Vec<u8>) -> io::Result<String> {
    String::from_utf8(bytes).map_err(|e| Error::new(ErrorKind::InvalidData, e))
}

fn unchanged_marker(id: &str) -> String {
    format!("{PUA}BOOMERANG:UNCHANGED:{id}{PUA}")
}

fn diff_marker(id: &str) -> String {
    format!("{PUA}BOOMERANG:DIFF:{id}{PUA}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DiffOp {
    Same(usize),
    Delete(usize),
    Insert(Vec<String>),
}

/// Line-level diff via the textbook LCS dynamic-program: `dp[i][j]` is the
/// length of the longest common subsequence of `old[i..]` and `new[j..]`.
/// O(N*M) time and space — the simple, obviously-correct version, not the
/// O(ND) Myers algorithm `git diff` actually uses. Fine for the file/log
/// sizes an agent session deals with; if that ever stops being true, this
/// is the one function that needs replacing, behind the same op-list
/// interface.
fn lcs_diff(old: &[&str], new: &[&str]) -> Vec<DiffOp> {
    let (n, m) = (old.len(), new.len());
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if old[i] == new[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }

    let mut ops: Vec<DiffOp> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if old[i] == new[j] {
            push_same(&mut ops);
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            push_delete(&mut ops);
            i += 1;
        } else {
            push_insert(&mut ops, new[j]);
            j += 1;
        }
    }
    while i < n {
        push_delete(&mut ops);
        i += 1;
    }
    while j < m {
        push_insert(&mut ops, new[j]);
        j += 1;
    }
    ops
}

fn push_same(ops: &mut Vec<DiffOp>) {
    if let Some(DiffOp::Same(c)) = ops.last_mut() {
        *c += 1;
    } else {
        ops.push(DiffOp::Same(1));
    }
}

fn push_delete(ops: &mut Vec<DiffOp>) {
    if let Some(DiffOp::Delete(c)) = ops.last_mut() {
        *c += 1;
    } else {
        ops.push(DiffOp::Delete(1));
    }
}

fn push_insert(ops: &mut Vec<DiffOp>, line: &str) {
    if let Some(DiffOp::Insert(lines)) = ops.last_mut() {
        lines.push(line.to_string());
    } else {
        ops.push(DiffOp::Insert(vec![line.to_string()]));
    }
}

fn apply_diff(old: &[&str], ops: &[DiffOp]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    for op in ops {
        match op {
            DiffOp::Same(n) => {
                out.extend(old[i..i + n].iter().map(|s| s.to_string()));
                i += n;
            }
            DiffOp::Delete(n) => i += n,
            DiffOp::Insert(lines) => out.extend(lines.iter().cloned()),
        }
    }
    out
}

fn format_diff(ops: &[DiffOp]) -> String {
    let mut out: Vec<String> = Vec::new();
    for op in ops {
        match op {
            DiffOp::Same(n) => out.push(format!("{PUA}BOOMERANG:SAME:{n}{PUA}")),
            DiffOp::Delete(n) => out.push(format!("{PUA}BOOMERANG:DELETE:{n}{PUA}")),
            DiffOp::Insert(lines) => {
                out.push(format!("{PUA}BOOMERANG:INSERT:{}{PUA}", lines.len()));
                out.extend(lines.iter().cloned());
            }
        }
    }
    out.join("\n")
}

fn parse_diff(text: &str) -> Option<Vec<DiffOp>> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut ops = Vec::new();
    let mut idx = 0;
    while idx < lines.len() {
        let line = lines[idx];
        if let Some(n) = strip_pua(line, "BOOMERANG:SAME:").and_then(|s| s.parse().ok()) {
            ops.push(DiffOp::Same(n));
            idx += 1;
        } else if let Some(n) = strip_pua(line, "BOOMERANG:DELETE:").and_then(|s| s.parse().ok()) {
            ops.push(DiffOp::Delete(n));
            idx += 1;
        } else {
            let n = strip_pua(line, "BOOMERANG:INSERT:").and_then(|s| s.parse::<usize>().ok())?;
            idx += 1;
            if idx + n > lines.len() {
                return None;
            }
            let content: Vec<String> = lines[idx..idx + n].iter().map(|s| s.to_string()).collect();
            ops.push(DiffOp::Insert(content));
            idx += n;
        }
    }
    Some(ops)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;

    fn temp_store() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "boomerang-session-test-{:?}-{}",
            std::time::SystemTime::now(),
            std::process::id()
        ));
        Store::open(&dir).unwrap()
    }

    fn lines_of(s: &str) -> Vec<&str> {
        s.split('\n').collect()
    }

    fn assert_diff_round_trips(old: &str, new: &str) {
        let old_lines = lines_of(old);
        let new_lines = lines_of(new);
        let ops = lcs_diff(&old_lines, &new_lines);
        let restored = apply_diff(&old_lines, &ops).join("\n");
        assert_eq!(
            restored, new,
            "apply_diff(old, lcs_diff(old, new)) must equal new"
        );

        let formatted = format_diff(&ops);
        let parsed = parse_diff(&formatted).expect("format_diff output must parse back");
        assert_eq!(
            parsed, ops,
            "format_diff/parse_diff must round-trip the op list"
        );
    }

    #[test]
    fn lcs_diff_round_trips_various_shapes() {
        assert_diff_round_trips("a\nb\nc", "a\nb\nc"); // identical
        assert_diff_round_trips("a\nb\nc", "x\ny\nz"); // fully disjoint
        assert_diff_round_trips("a\nb\nc", "a\nb\nnew\nc"); // pure insert
        assert_diff_round_trips("a\nb\nc\nd", "a\nd"); // pure delete
        assert_diff_round_trips("a\nb\nc", "a\nB\nc"); // single-line edit
        assert_diff_round_trips("", "a\nb"); // empty old
        assert_diff_round_trips("a\nb", ""); // empty new
        assert_diff_round_trips("", ""); // both empty
    }

    #[test]
    fn first_sight_of_a_key_passes_through_unchanged() {
        let session = Session::new(temp_store());
        let content = b"fn main() {}\n";
        let compressed = session.compress("src/main.rs", content).unwrap();
        assert_eq!(compressed, content, "nothing to diff against yet");
        assert_eq!(session.decompress(&compressed).unwrap(), content);
    }

    #[test]
    fn identical_reread_collapses_to_a_short_marker() {
        let session = Session::new(temp_store());
        let content: Vec<u8> = (0..200)
            .map(|i| format!("line {i}\n"))
            .collect::<String>()
            .into_bytes();

        let first = session.compress("src/big.rs", &content).unwrap();
        assert_eq!(first, content);

        let second = session.compress("src/big.rs", &content).unwrap();
        assert!(
            second.len() < content.len() / 10,
            "unchanged re-read should collapse to a tiny marker: {} vs {}",
            second.len(),
            content.len()
        );
        assert_eq!(session.decompress(&second).unwrap(), content);
    }

    #[test]
    fn small_edit_produces_a_small_diff_and_round_trips() {
        let session = Session::new(temp_store());
        let lines: Vec<String> = (0..200).map(|i| format!("line {i}")).collect();
        let original = lines.join("\n");
        session.compress("src/big.rs", original.as_bytes()).unwrap();

        let mut edited_lines = lines.clone();
        edited_lines[100] = "line 100 -- CHANGED".to_string();
        let edited = edited_lines.join("\n");

        let diff_view = session.compress("src/big.rs", edited.as_bytes()).unwrap();
        assert!(
            diff_view.len() < edited.len() / 4,
            "one-line change in a 200-line file should compress far below the full file: {} vs {}",
            diff_view.len(),
            edited.len()
        );

        let restored = session.decompress(&diff_view).unwrap();
        assert_eq!(
            restored,
            edited.into_bytes(),
            "diff must reconstruct the edited content exactly"
        );
    }

    #[test]
    fn different_keys_are_independent() {
        let session = Session::new(temp_store());
        session.compress("a.rs", b"content a").unwrap();
        // b.rs has never been seen -> passthrough, not a diff against a.rs.
        let compressed = session.compress("b.rs", b"content b").unwrap();
        assert_eq!(compressed, b"content b");
    }

    #[test]
    fn first_sight_content_that_looks_like_a_marker_round_trips() {
        // First-sight passthrough is the case escape_for_view exists for:
        // content that happens to look like one of our own markers must
        // not be misinterpreted the next time it's decompressed.
        let session = Session::new(temp_store());
        let content = format!("real line\n{PUA}BOOMERANG:UNCHANGED:deadbeef{PUA}\nanother line");
        let compressed = session.compress("weird.txt", content.as_bytes()).unwrap();
        let restored = session.decompress(&compressed).unwrap();
        assert_eq!(restored, content.into_bytes());
    }

    #[test]
    fn diff_content_containing_marker_looking_lines_round_trips() {
        // The new version introduces a line that looks like a SAME/DELETE
        // marker - it ends up inside format_diff's output verbatim (as an
        // Insert), and must not be misread as a real op on decompress.
        let session = Session::new(temp_store());
        let old = (0..20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        session.compress("f.txt", old.as_bytes()).unwrap();

        let mut new_lines: Vec<String> = (0..20).map(|i| format!("line {i}")).collect();
        new_lines.insert(10, format!("{PUA}BOOMERANG:SAME:99{PUA}"));
        new_lines.insert(15, format!("{PUA}BOOMERANG:DELETE:99{PUA}"));
        let new = new_lines.join("\n");

        let compressed = session.compress("f.txt", new.as_bytes()).unwrap();
        let restored = session.decompress(&compressed).unwrap();
        assert_eq!(restored, new.into_bytes());
    }

    #[test]
    fn diff_above_the_cell_cap_falls_back_to_full_content_and_still_round_trips() {
        // Confirmed by measurement (see MAX_DIFF_CELLS' doc comment) that
        // lcs_diff's memory cost is a function of line counts alone, not
        // similarity - a huge file with a single changed line costs the
        // same as two huge maximally-different files. Pick line counts
        // whose product clears the cap even though the content is nearly
        // identical, and confirm compress() takes the safe fallback (no
        // DIFF marker) rather than attempting the diff, while still
        // reconstructing exactly.
        let side = (MAX_DIFF_CELLS as f64).sqrt() as usize + 500;
        let old_lines: Vec<String> = (0..side).map(|i| format!("line {i}")).collect();
        let old = old_lines.join("\n");
        let mut new_lines = old_lines.clone();
        new_lines[side / 2] = "one changed line".to_string();
        let new = new_lines.join("\n");

        let session = Session::new(temp_store());
        session.compress("big.txt", old.as_bytes()).unwrap();
        let compressed = session.compress("big.txt", new.as_bytes()).unwrap();

        let compressed_text = String::from_utf8(compressed.clone()).unwrap();
        assert!(
            strip_pua(
                compressed_text.lines().next().unwrap_or(""),
                "BOOMERANG:DIFF:"
            )
            .is_none(),
            "must not have attempted a diff above the cell cap"
        );

        let restored = session.decompress(&compressed).unwrap();
        assert_eq!(restored, new.into_bytes());
    }

    #[test]
    fn verify_passes_for_unchanged_and_diff_markers() {
        let session = Session::new(temp_store());
        let original = (0..20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        session.compress("f.txt", original.as_bytes()).unwrap();

        // Identical re-read -> UNCHANGED marker.
        let unchanged_view = session.compress("f.txt", original.as_bytes()).unwrap();
        let result = session.verify(&unchanged_view).unwrap();
        assert!(result.ok);
        assert_eq!(result.checked_refs, 1);

        // Real edit -> DIFF marker.
        let edited = format!("{original}\none more line");
        let diff_view = session.compress("f.txt", edited.as_bytes()).unwrap();
        let result = session.verify(&diff_view).unwrap();
        assert!(result.ok);
        assert_eq!(result.checked_refs, 1);
    }

    #[test]
    fn verify_on_first_sight_is_trivially_ok() {
        let session = Session::new(temp_store());
        let view = session
            .compress("new-key.txt", b"never seen before")
            .unwrap();
        let result = session.verify(&view).unwrap();
        assert!(result.ok);
        assert_eq!(result.checked_refs, 0);
    }

    #[test]
    fn check_changed_unknown_key_reports_not_known() {
        let session = Session::new(temp_store());
        let hash = "a".repeat(64);
        let (changed, known) = session.check_changed("never-seen.txt", &hash).unwrap();
        assert!(!known);
        assert!(!changed);
    }

    #[test]
    fn check_changed_matching_hash_reports_not_changed() {
        let session = Session::new(temp_store());
        let content = b"hello world";
        session.compress("f.txt", content).unwrap();
        let hash = hex::encode(sha2::Sha256::digest(content));
        let (changed, known) = session.check_changed("f.txt", &hash).unwrap();
        assert!(known);
        assert!(!changed);
    }

    #[test]
    fn check_changed_different_hash_reports_changed() {
        let session = Session::new(temp_store());
        session.compress("f.txt", b"version 1").unwrap();
        let different_hash = hex::encode(sha2::Sha256::digest(b"version 2"));
        let (changed, known) = session.check_changed("f.txt", &different_hash).unwrap();
        assert!(known);
        assert!(changed);
    }

    #[test]
    fn append_first_call_stores_and_returns_content() {
        let session = Session::new(temp_store());
        let view = session.append("build.log", b"line 1\nline 2").unwrap();
        assert_eq!(view, b"line 1\nline 2");
    }

    #[test]
    fn append_accumulates_across_calls() {
        let session = Session::new(temp_store());
        session.append("build.log", b"line 1").unwrap();
        let view2 = session.append("build.log", b"line 2").unwrap();
        assert_eq!(view2, b"line 2", "only the new bytes are returned");

        let view3 = session.append("build.log", b"line 3").unwrap();
        assert_eq!(view3, b"line 3");

        // The full accumulated content is accessible via compress+decompress.
        let full_view = session.compress("build.log", b"line 1\nline 2\nline 3").unwrap();
        let full_text = String::from_utf8_lossy(&full_view);
        assert!(
            full_text.contains("UNCHANGED"),
            "accumulated content should match, producing an UNCHANGED marker"
        );
    }

    #[test]
    fn append_with_empty_new_content_does_not_add_separator() {
        let session = Session::new(temp_store());
        session.append("f.log", b"first").unwrap();
        session.append("f.log", b"").unwrap();

        // Verify the stored content is just "first" (no trailing newline).
        let unchanged = session.compress("f.log", b"first").unwrap();
        let text = String::from_utf8_lossy(&unchanged);
        assert!(text.contains("UNCHANGED"));
    }

    #[test]
    fn append_escapes_marker_looking_content() {
        let session = Session::new(temp_store());
        let tricky = format!("{PUA}BOOMERANG:UNCHANGED:deadbeef{PUA}");
        let view = session.append("f.log", tricky.as_bytes()).unwrap();
        // The returned view must be escaped so it won't be misinterpreted.
        assert_ne!(view, tricky.as_bytes(), "marker-like content must be escaped");
    }

    #[test]
    fn append_different_keys_are_independent() {
        let session = Session::new(temp_store());
        session.append("a.log", b"alpha").unwrap();
        let view = session.append("b.log", b"beta").unwrap();
        assert_eq!(view, b"beta", "b.log should have no prior content");
    }

    #[test]
    fn check_changed_rejects_invalid_hashes() {
        let session = Session::new(temp_store());
        session.compress("f.txt", b"content").unwrap();
        assert!(session.check_changed("f.txt", "too-short").is_err());
        assert!(session.check_changed("f.txt", "").is_err());
        assert!(session
            .check_changed("f.txt", &"g".repeat(64))
            .is_err());
        assert!(session
            .check_changed("f.txt", &"g".repeat(64))
            .is_err()); // 'g' is not a hex digit
    }

    mod proptests {
        use super::*;
        use proptest::prelude::*;

        /// A handful of keys reused across ops (not unique per-op), so
        /// generated sequences actually exercise first-sight/unchanged/diff
        /// transitions on the same key repeatedly - a sequence of all-new
        /// keys would only ever hit the trivial first-sight path.
        fn arb_key() -> impl Strategy<Value = String> {
            prop_oneof!["a.txt", "b.txt", "c.txt"].prop_map(String::from)
        }

        fn arb_line() -> impl Strategy<Value = String> {
            prop_oneof![
                4 => "[a-zA-Z0-9 ]{0,15}",
                1 => Just(format!("{PUA}BOOMERANG:UNCHANGED:deadbeefdeadbeefdeadbeefdeadbeefdeadbeef{PUA}")),
                1 => Just(format!("{PUA}BOOMERANG:DIFF:deadbeefdeadbeefdeadbeefdeadbeefdeadbeef{PUA}")),
                1 => Just(format!("{PUA}BOOMERANG:SAME:3{PUA}")),
            ]
        }

        fn arb_content() -> impl Strategy<Value = String> {
            prop::collection::vec(arb_line(), 0..10).prop_map(|lines| lines.join("\n"))
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            /// Every step's compress -> decompress must round-trip exactly,
            /// checked immediately after each op against a session whose
            /// state keeps evolving underneath it - the property the whole
            /// diff-against-last-seen mechanism exists to guarantee.
            #[test]
            fn arbitrary_op_sequence_round_trips(
                ops in prop::collection::vec((arb_key(), arb_content()), 1..12)
            ) {
                let session = Session::new(temp_store());
                for (key, content) in ops {
                    let compressed = session.compress(&key, content.as_bytes()).unwrap();
                    let restored = session.decompress(&compressed).unwrap();
                    prop_assert_eq!(restored, content.into_bytes());
                }
            }

            /// Append accumulates content correctly: after N appends,
            /// the stored content under the key must equal all chunks
            /// joined by newline separators, and compress against that
            /// full content must return UNCHANGED.
            #[test]
            fn append_accumulates_correctly(
                chunks in prop::collection::vec(arb_content(), 1..8)
            ) {
                let session = Session::new(temp_store());
                let key = "stream.log";
                let mut expected = String::new();
                for (i, chunk) in chunks.iter().enumerate() {
                    if i > 0 && !expected.is_empty() && !chunk.is_empty() {
                        expected.push('\n');
                    }
                    expected.push_str(chunk);
                    session.append(key, chunk.as_bytes()).unwrap();
                }
                let view = session.compress(key, expected.as_bytes()).unwrap();
                let text = String::from_utf8(view).unwrap();
                prop_assert!(
                    text.contains("UNCHANGED") || chunks.iter().all(|c| c.is_empty()),
                    "accumulated content must match what compress sees"
                );
            }

            /// Same property as json.rs's/text.rs's: verify must never
            /// false-flag a view produced against a store that's still
            /// fully intact.
            #[test]
            fn verify_never_false_flags_an_intact_store(
                ops in prop::collection::vec((arb_key(), arb_content()), 1..12)
            ) {
                let session = Session::new(temp_store());
                for (key, content) in ops {
                    let compressed = session.compress(&key, content.as_bytes()).unwrap();
                    let result = session.verify(&compressed).unwrap();
                    prop_assert!(result.ok);
                    prop_assert!(result.missing_refs.is_empty());
                }
            }
        }
    }
}
