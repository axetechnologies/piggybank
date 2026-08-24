use crate::markers::{strip_pua, PUA};
use crate::Store;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::io::{self, Error, ErrorKind};
use std::path::{Path, PathBuf};

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
    pub fn compress(&self, key: &str, content: &[u8]) -> io::Result<Vec<u8>> {
        let new_id = self.store.put(content)?;
        let previous_id = self.last_seen.borrow().get(key).cloned();
        self.last_seen
            .borrow_mut()
            .insert(key.to_string(), new_id.clone());
        self.persist()?;

        match previous_id {
            None => Ok(content.to_vec()),
            Some(prev_id) if prev_id == new_id => Ok(unchanged_marker(&prev_id).into_bytes()),
            Some(prev_id) => {
                let old_text = utf8(self.store.get(&prev_id)?)?;
                let new_text = std::str::from_utf8(content)
                    .map_err(|e| Error::new(ErrorKind::InvalidData, e))?;
                let old_lines: Vec<&str> = old_text.split('\n').collect();
                let new_lines: Vec<&str> = new_text.split('\n').collect();
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
    /// first-sight passthrough and returned as-is.
    pub fn decompress(&self, compressed: &[u8]) -> io::Result<Vec<u8>> {
        let Ok(text) = std::str::from_utf8(compressed) else {
            return Ok(compressed.to_vec());
        };

        if let Some(id) = strip_pua(text, "BOOMERANG:UNCHANGED:") {
            return self.store.get(id);
        }

        if let Some((first, rest)) = text.split_once('\n') {
            if let Some(prev_id) = strip_pua(first, "BOOMERANG:DIFF:") {
                let old_text = utf8(self.store.get(prev_id)?)?;
                let old_lines: Vec<&str> = old_text.split('\n').collect();
                let ops = parse_diff(rest)
                    .ok_or_else(|| Error::new(ErrorKind::InvalidData, "malformed diff"))?;
                return Ok(apply_diff(&old_lines, &ops).join("\n").into_bytes());
            }
        }

        Ok(compressed.to_vec())
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
}
