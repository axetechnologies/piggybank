//! Format-aware compressors for structured text output.
//!
//! Each compressor keeps signal lines verbatim (errors, failures, warnings,
//! summaries, diff hunks) and collapses noise lines (progress, PASSED tests,
//! Compiling chatter, unchanged context) into ELIDE markers.
//!
//! The output is identical in marker format to `compress_text` — every ELIDE
//! ref is stored in the same store and `decompress_text` reconstructs the
//! exact original bytes. Format-aware compression just makes smarter choices
//! about WHICH lines to elide, rather than doing a head/tail cut.
//!
//! Important contract for all compressors in this module:
//! - Use `split('\n')` (not `.lines()`) to preserve trailing newlines.
//! - Do NOT call `unescape_lines` on the output — that is `decompress_text`'s
//!   job. The compressed output must be in the same "half-cooked" escaped form
//!   as `compress_text` output.
//! - Signal lines pushed to `out` come directly from the escaped input split,
//!   so they are in escaped form already.
//! - Noise lines are stored via `store.put` (in escaped form) and replaced
//!   with `PIGGYBANK:ELIDE:N:id` markers.

use crate::markers::{escape_lines, PUA};
use crate::Store;

// ── Format enum ─────────────────────────────────────────────────────────────

/// A recognised structured-text format that benefits from format-aware
/// compression. Detection is cheap: heuristics on the first ~40 lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// pytest / unittest terminal output.
    Pytest,
    /// cargo build, cargo test, rustc output.
    Cargo,
    /// `git status` output.
    GitStatus,
    /// `git diff` output.
    GitDiff,
    /// `git log --oneline` — already dense, passed through verbatim.
    GitLogOneline,
    /// npm install / pip install / cargo install progress logs.
    PackageManager,
    /// Newline-delimited JSON (JSONL / ndjson).
    JsonLines,
}

// ── Detection ────────────────────────────────────────────────────────────────

/// Detect the format of `input` from the first ~40 lines.
/// Returns `None` when no known format matches.
pub fn detect_format(input: &str) -> Option<Format> {
    let lines: Vec<&str> = input.lines().take(40).collect();
    detect_from_lines(&lines)
}

fn detect_from_lines(lines: &[&str]) -> Option<Format> {
    if lines.is_empty() {
        return None;
    }
    if is_pytest(lines) {
        return Some(Format::Pytest);
    }
    if is_cargo(lines) {
        return Some(Format::Cargo);
    }
    if is_git_diff(lines) {
        return Some(Format::GitDiff);
    }
    if is_git_status(lines) {
        return Some(Format::GitStatus);
    }
    if is_git_log_oneline(lines) {
        return Some(Format::GitLogOneline);
    }
    if is_package_manager(lines) {
        return Some(Format::PackageManager);
    }
    if is_jsonl(lines) {
        return Some(Format::JsonLines);
    }
    None
}

fn is_pytest(lines: &[&str]) -> bool {
    let has_session = lines
        .iter()
        .any(|l| l.starts_with("===") && l.contains("test session starts"));
    let has_test_lines = lines.iter().any(|l| {
        (l.contains("::test_") && (l.contains("PASSED") || l.contains("FAILED")))
            || l.starts_with("PASSED")
            || l.starts_with("FAILED")
    });
    has_session || has_test_lines
}

fn is_cargo(lines: &[&str]) -> bool {
    lines.iter().any(|l| {
        l.starts_with("   Compiling ")
            || l.starts_with("   Finished ")
            || l.starts_with("   Running ")
            || l.starts_with("test result:")
            || (l.starts_with("test ") && (l.contains(" ... ok") || l.contains(" ... FAILED")))
            || l.starts_with("error[E")
            || l.starts_with("warning[")
    })
}

fn is_git_diff(lines: &[&str]) -> bool {
    lines.iter().any(|l| {
        l.starts_with("diff --git ")
            || (l.starts_with("--- a/") && lines.iter().any(|l2| l2.starts_with("+++ b/")))
            || l.starts_with("@@ ")
    })
}

fn is_git_status(lines: &[&str]) -> bool {
    lines.iter().any(|l| {
        l == &"Changes to be committed:"
            || l == &"Changes not staged for commit:"
            || l == &"Untracked files:"
            || l.starts_with("On branch ")
            || l.starts_with("\tmodified:")
            || l.starts_with("\tnew file:")
    })
}

fn is_git_log_oneline(lines: &[&str]) -> bool {
    let matching = lines
        .iter()
        .take(10)
        .filter(|l| !l.is_empty())
        .filter(|l| {
            let mut parts = l.splitn(2, ' ');
            if let Some(hash) = parts.next() {
                hash.len() >= 7
                    && hash.len() <= 12
                    && hash.chars().all(|c| c.is_ascii_hexdigit())
                    && parts.next().is_some()
            } else {
                false
            }
        })
        .count();
    let non_empty = lines.iter().filter(|l| !l.is_empty()).count();
    non_empty > 1 && matching == non_empty.min(10)
}

fn is_package_manager(lines: &[&str]) -> bool {
    lines.iter().any(|l| {
        l.starts_with("npm warn ")
            || l.starts_with("npm notice ")
            || l.starts_with("Downloading ")
            || l.starts_with("Collecting ")
            || l.starts_with("Requirement already satisfied")
            || l.starts_with("Successfully installed ")
            || (l.contains("added ") && l.contains("packages"))
    })
}

fn is_jsonl(lines: &[&str]) -> bool {
    let non_empty: Vec<&str> = lines.iter().filter(|l| !l.is_empty()).copied().collect();
    if non_empty.len() < 2 {
        return false;
    }
    let json_count = non_empty
        .iter()
        .filter(|l| {
            let t = l.trim();
            (t.starts_with('{') && t.ends_with('}')) || (t.starts_with('[') && t.ends_with(']'))
        })
        .count();
    json_count * 2 >= non_empty.len()
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Compress `input` using format-specific selection logic for `format`.
///
/// Output is in the same marker format as `compress_text` (escaped form, not
/// yet unescaped — `decompress_text` finalises with its own `unescape_lines`).
/// Every elided run is stored in `store`; the round-trip invariant holds.
pub fn compress_with_format(
    store: &Store,
    input: &[u8],
    format: Format,
) -> std::io::Result<Vec<u8>> {
    let text = match std::str::from_utf8(input) {
        Ok(t) => t,
        Err(_) => {
            // Non-UTF-8: store whole and return a RAW marker.
            let id = store.put(input)?;
            return Ok(format!("{PUA}PIGGYBANK:RAW:{id}{PUA}").into_bytes());
        }
    };

    match format {
        Format::Pytest => compress_pytest(store, text),
        Format::Cargo => compress_cargo(store, text),
        Format::GitDiff => compress_git_diff(store, text),
        Format::GitStatus => Ok(input.to_vec()), // already compact
        Format::GitLogOneline => Ok(input.to_vec()),
        Format::PackageManager => compress_package_manager(store, text),
        Format::JsonLines => compress_jsonl(store, text),
    }
}

// ── Elide helper ─────────────────────────────────────────────────────────────

fn elide_marker(line_count: usize, id: &str) -> String {
    format!("{PUA}PIGGYBANK:ELIDE:{line_count}:{id}{PUA}")
}

/// Store `lines` (already in escaped form) and return an ELIDE marker string,
/// or `None` if lines is empty.
fn store_and_elide(store: &Store, lines: &[&str]) -> std::io::Result<Option<String>> {
    if lines.is_empty() {
        return Ok(None);
    }
    let raw = lines.join("\n");
    let id = store.put(raw.as_bytes())?;
    Ok(Some(elide_marker(lines.len(), &id)))
}

// ── pytest / unittest ─────────────────────────────────────────────────────────

fn compress_pytest(store: &Store, text: &str) -> std::io::Result<Vec<u8>> {
    let escaped = escape_lines(text);
    // split('\n') not lines() — preserves trailing newlines.
    let lines: Vec<&str> = escaped.split('\n').collect();
    let mut out: Vec<String> = Vec::new();
    let mut noise_buf: Vec<&str> = Vec::new();

    macro_rules! flush_noise {
        () => {
            if let Some(marker) = store_and_elide(store, &noise_buf)? {
                out.push(marker);
            }
            noise_buf.clear();
        };
    }

    for line in &lines {
        // Separator/header/failure lines are always signal.
        let is_signal = line.starts_with("===")
            || line.starts_with("---")
            || line.starts_with("___")
            || line.starts_with("FAILED ")
            || line.starts_with("ERROR ")
            // Traceback body lines (indented or continuation)
            || line.starts_with("    ")
            || line.starts_with(">")
            || line.starts_with("E ")
            || line.starts_with("short test summary");

        // Noise: PASSED lines and progress dot-lines.
        let is_noise = !is_signal
            && (line.contains(" PASSED")
                || (!line.trim().is_empty()
                    && line.trim().len() < 80
                    && line
                        .trim()
                        .chars()
                        .all(|c| ".FEs []%0123456789".contains(c))));

        if is_noise {
            noise_buf.push(line);
        } else {
            flush_noise!();
            out.push(line.to_string());
        }
    }
    flush_noise!();

    // Output is in escaped form (same as compress_text output).
    Ok(out.join("\n").into_bytes())
}

// ── cargo build/test / rustc ──────────────────────────────────────────────────

fn compress_cargo(store: &Store, text: &str) -> std::io::Result<Vec<u8>> {
    let escaped = escape_lines(text);
    let lines: Vec<&str> = escaped.split('\n').collect();
    let mut out: Vec<String> = Vec::new();
    let mut compiling_buf: Vec<&str> = Vec::new();
    let mut test_ok_buf: Vec<&str> = Vec::new();

    macro_rules! flush_compiling {
        () => {
            if let Some(marker) = store_and_elide(store, &compiling_buf)? {
                out.push(marker);
            }
            compiling_buf.clear();
        };
    }
    macro_rules! flush_test_ok {
        () => {
            if let Some(marker) = store_and_elide(store, &test_ok_buf)? {
                out.push(marker);
            }
            test_ok_buf.clear();
        };
    }

    for line in &lines {
        if line.starts_with("   Compiling ") || line.starts_with("   Downloading ") {
            flush_test_ok!();
            compiling_buf.push(line);
            continue;
        }
        if line.starts_with("test ") && line.ends_with(" ... ok") {
            flush_compiling!();
            test_ok_buf.push(line);
            continue;
        }
        flush_compiling!();
        flush_test_ok!();
        out.push(line.to_string());
    }
    flush_compiling!();
    flush_test_ok!();

    Ok(out.join("\n").into_bytes())
}

// ── git diff ─────────────────────────────────────────────────────────────────

fn compress_git_diff(store: &Store, text: &str) -> std::io::Result<Vec<u8>> {
    let escaped = escape_lines(text);
    let lines: Vec<&str> = escaped.split('\n').collect();
    let mut out: Vec<String> = Vec::new();
    let mut ctx_buf: Vec<&str> = Vec::new();

    const KEEP_CTX: usize = 3;

    macro_rules! flush_ctx {
        () => {
            if ctx_buf.len() <= KEEP_CTX * 2 {
                out.extend(ctx_buf.iter().map(|s| s.to_string()));
            } else {
                out.extend(ctx_buf[..KEEP_CTX].iter().map(|s| s.to_string()));
                let middle = &ctx_buf[KEEP_CTX..ctx_buf.len() - KEEP_CTX];
                if let Some(marker) = store_and_elide(store, middle)? {
                    out.push(marker);
                }
                out.extend(
                    ctx_buf[ctx_buf.len() - KEEP_CTX..]
                        .iter()
                        .map(|s| s.to_string()),
                );
            }
            ctx_buf.clear();
        };
    }

    for line in &lines {
        if line.starts_with(' ') {
            ctx_buf.push(line);
        } else {
            flush_ctx!();
            out.push(line.to_string());
        }
    }
    flush_ctx!();

    Ok(out.join("\n").into_bytes())
}

// ── Package managers ──────────────────────────────────────────────────────────

fn compress_package_manager(store: &Store, text: &str) -> std::io::Result<Vec<u8>> {
    let escaped = escape_lines(text);
    let lines: Vec<&str> = escaped.split('\n').collect();
    let mut out: Vec<String> = Vec::new();
    let mut progress_buf: Vec<&str> = Vec::new();

    macro_rules! flush_progress {
        () => {
            if let Some(marker) = store_and_elide(store, &progress_buf)? {
                out.push(marker);
            }
            progress_buf.clear();
        };
    }

    for line in &lines {
        let lower = line.to_ascii_lowercase();
        let is_signal = lower.contains("error")
            || lower.contains("warn")
            || lower.contains("successfully installed")
            || lower.contains("successfully downloaded")
            || (lower.contains("added ") && lower.contains("packages"))
            || lower.starts_with("requirement already satisfied")
            || lower.contains("failed")
            || lower.contains("vulnerability")
            || lower.contains("vulnerabilities");

        let is_progress = !is_signal
            && (lower.starts_with("downloading")
                || lower.starts_with("installing")
                || lower.starts_with("collecting")
                || lower.starts_with("npm notice")
                || lower.starts_with("obtaining")
                || lower.starts_with("using cached"));

        if is_progress {
            progress_buf.push(line);
        } else {
            flush_progress!();
            out.push(line.to_string());
        }
    }
    flush_progress!();

    Ok(out.join("\n").into_bytes())
}

// ── JSON-lines ────────────────────────────────────────────────────────────────

fn compress_jsonl(store: &Store, text: &str) -> std::io::Result<Vec<u8>> {
    let escaped = escape_lines(text);
    let lines: Vec<&str> = escaped.split('\n').collect();

    // Find schema from first non-empty JSON object line.
    let schema_keys: Option<Vec<String>> = lines
        .iter()
        .find(|l| !l.trim().is_empty())
        .and_then(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok())
        .and_then(|v| v.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()));

    let Some(keys) = schema_keys else {
        return Ok(text.as_bytes().to_vec());
    };

    let mut out: Vec<String> = Vec::new();
    let mut batch_buf: Vec<&str> = Vec::new();
    let mut first = true;

    macro_rules! flush_batch {
        () => {
            if let Some(marker) = store_and_elide(store, &batch_buf)? {
                out.push(marker);
            }
            batch_buf.clear();
        };
    }

    for line in &lines {
        // Preserve empty lines (including trailing newline's empty element) verbatim.
        if line.trim().is_empty() {
            flush_batch!();
            out.push(line.to_string());
            continue;
        }
        if first {
            out.push(line.to_string());
            first = false;
            continue;
        }
        let same_schema = serde_json::from_str::<serde_json::Value>(line.trim())
            .ok()
            .and_then(|v| v.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()))
            .map(|k| k == keys)
            .unwrap_or(false);

        if same_schema {
            batch_buf.push(line);
        } else {
            flush_batch!();
            out.push(line.to_string());
        }
    }
    flush_batch!();

    Ok(out.join("\n").into_bytes())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decompress_text;

    fn temp_store() -> Store {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "piggybank-format-test-{}-{}",
            std::process::id(),
            n,
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Store::open(&dir).unwrap()
    }

    fn assert_round_trips(store: &Store, input: &[u8], format: Format) -> Vec<u8> {
        let compressed = compress_with_format(store, input, format).unwrap();
        let restored = decompress_text(store, &compressed).unwrap();
        assert_eq!(
            restored, input,
            "compress_with_format/decompress_text must reconstruct original bytes exactly"
        );
        compressed
    }

    // ── pytest ──────────────────────────────────────────────────────────────

    #[test]
    fn pytest_round_trips() {
        let store = temp_store();
        let input = include_str!("../tests/fixtures/pytest_output.txt");
        assert_round_trips(&store, input.as_bytes(), Format::Pytest);
    }

    #[test]
    fn pytest_keeps_failures_verbatim() {
        let store = temp_store();
        let input = include_str!("../tests/fixtures/pytest_output.txt");
        let compressed = compress_with_format(&store, input.as_bytes(), Format::Pytest).unwrap();
        let compressed_text = String::from_utf8(compressed).unwrap();
        assert!(
            compressed_text.contains("AssertionError"),
            "failure traceback must survive pytest compression"
        );
    }

    #[test]
    fn pytest_collapses_passed_lines() {
        let store = temp_store();
        let input = include_str!("../tests/fixtures/pytest_output.txt");
        let passed_count = input.lines().filter(|l| l.contains(" PASSED")).count();
        if passed_count > 3 {
            let compressed =
                compress_with_format(&store, input.as_bytes(), Format::Pytest).unwrap();
            let compressed_text = String::from_utf8(compressed).unwrap();
            let compressed_passed = compressed_text.matches(" PASSED").count();
            assert!(
                compressed_passed < passed_count,
                "PASSED lines ({passed_count}) should be collapsed; compressed has {compressed_passed}"
            );
        }
    }

    // ── cargo ────────────────────────────────────────────────────────────────

    #[test]
    fn cargo_round_trips() {
        let store = temp_store();
        let input = include_str!("../tests/fixtures/cargo_output.txt");
        assert_round_trips(&store, input.as_bytes(), Format::Cargo);
    }

    #[test]
    fn cargo_keeps_errors_verbatim() {
        let store = temp_store();
        let input = include_str!("../tests/fixtures/cargo_output.txt");
        let compressed = compress_with_format(&store, input.as_bytes(), Format::Cargo).unwrap();
        let compressed_text = String::from_utf8(compressed).unwrap();
        assert!(
            compressed_text.contains("error[E"),
            "rustc error with code span must survive cargo compression"
        );
    }

    #[test]
    fn cargo_collapses_compiling_lines() {
        let store = temp_store();
        let input = include_str!("../tests/fixtures/cargo_output.txt");
        let compiling_count = input
            .lines()
            .filter(|l| l.starts_with("   Compiling "))
            .count();
        if compiling_count > 2 {
            let compressed = compress_with_format(&store, input.as_bytes(), Format::Cargo).unwrap();
            let compressed_text = String::from_utf8(compressed).unwrap();
            let compressed_compiling = compressed_text.matches("   Compiling ").count();
            assert!(
                compressed_compiling < compiling_count,
                "Compiling lines should be collapsed: {compiling_count} → {compressed_compiling}"
            );
        }
    }

    // ── git diff ─────────────────────────────────────────────────────────────

    #[test]
    fn git_diff_round_trips() {
        let store = temp_store();
        let input = include_str!("../tests/fixtures/git_diff.txt");
        assert_round_trips(&store, input.as_bytes(), Format::GitDiff);
    }

    #[test]
    fn git_diff_keeps_hunks_verbatim() {
        let store = temp_store();
        let input = include_str!("../tests/fixtures/git_diff.txt");
        let compressed = compress_with_format(&store, input.as_bytes(), Format::GitDiff).unwrap();
        let compressed_text = String::from_utf8(compressed).unwrap();
        assert!(
            compressed_text.contains("diff --git"),
            "diff header must survive"
        );
        assert!(compressed_text.contains("@@ "), "hunk header must survive");
    }

    // ── git status ───────────────────────────────────────────────────────────

    #[test]
    fn git_status_round_trips() {
        let store = temp_store();
        let input = include_str!("../tests/fixtures/git_status.txt");
        assert_round_trips(&store, input.as_bytes(), Format::GitStatus);
    }

    // ── package manager ──────────────────────────────────────────────────────

    #[test]
    fn package_manager_round_trips() {
        let store = temp_store();
        let input = include_str!("../tests/fixtures/npm_install.txt");
        assert_round_trips(&store, input.as_bytes(), Format::PackageManager);
    }

    #[test]
    fn package_manager_keeps_warnings() {
        let store = temp_store();
        let input = include_str!("../tests/fixtures/npm_install.txt");
        let compressed =
            compress_with_format(&store, input.as_bytes(), Format::PackageManager).unwrap();
        let compressed_text = String::from_utf8(compressed).unwrap();
        if input.contains("npm warn") {
            assert!(
                compressed_text.contains("npm warn"),
                "npm warn lines must survive package-manager compression"
            );
        }
    }

    // ── jsonl ─────────────────────────────────────────────────────────────────

    #[test]
    fn jsonl_round_trips() {
        let store = temp_store();
        let input = include_str!("../tests/fixtures/jsonl_logs.txt");
        assert_round_trips(&store, input.as_bytes(), Format::JsonLines);
    }

    #[test]
    fn jsonl_collapses_same_schema_records() {
        let store = temp_store();
        let input = include_str!("../tests/fixtures/jsonl_logs.txt");
        let record_count = input.lines().filter(|l| l.trim().starts_with('{')).count();
        if record_count > 3 {
            let compressed =
                compress_with_format(&store, input.as_bytes(), Format::JsonLines).unwrap();
            let compressed_records = compressed.iter().filter(|&&b| b == b'{').count();
            assert!(
                compressed_records < record_count,
                "same-schema JSONL records should be collapsed: {record_count} → {compressed_records}"
            );
        }
    }

    // ── detection ────────────────────────────────────────────────────────────

    #[test]
    fn detect_pytest() {
        let input = include_str!("../tests/fixtures/pytest_output.txt");
        assert_eq!(detect_format(input), Some(Format::Pytest));
    }

    #[test]
    fn detect_cargo() {
        let input = include_str!("../tests/fixtures/cargo_output.txt");
        assert_eq!(detect_format(input), Some(Format::Cargo));
    }

    #[test]
    fn detect_git_diff() {
        let input = include_str!("../tests/fixtures/git_diff.txt");
        assert_eq!(detect_format(input), Some(Format::GitDiff));
    }

    #[test]
    fn detect_git_status() {
        let input = include_str!("../tests/fixtures/git_status.txt");
        assert_eq!(detect_format(input), Some(Format::GitStatus));
    }

    #[test]
    fn detect_npm_install() {
        let input = include_str!("../tests/fixtures/npm_install.txt");
        assert_eq!(detect_format(input), Some(Format::PackageManager));
    }

    #[test]
    fn detect_jsonl() {
        let input = include_str!("../tests/fixtures/jsonl_logs.txt");
        assert_eq!(detect_format(input), Some(Format::JsonLines));
    }

    #[test]
    fn detect_returns_none_for_plain_text() {
        assert_eq!(
            detect_format("just some random\nplain text\nno special format"),
            None
        );
    }
}
