// ledger.rs — compaction ledger
//
// Parses a Claude Code transcript JSONL, identifies large tool_result blocks,
// stores them content-addressed, and emits a compact boomerang index so the
// post-compaction model can retrieve exact bytes.
//
// Index format (one line per entry, capped at MAX_LINES / MAX_BYTES):
//   BOOMERANG:CREF:<sha256> <tool_name> <first 80 chars of content>
//
// Claude Code transcript JSONL format (one JSON object per line):
//   {"role":"user","content":[{"type":"tool_result","tool_use_id":"...","content":"..."}]}
//   {"role":"assistant","content":[{"type":"tool_use","name":"...","input":{...}}]}
//   or flat message objects

use crate::Store;
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// One extracted tool result that is large enough to be ledgered.
#[derive(Debug)]
pub struct LedgerEntry {
    pub sha: String,
    pub tool_name: String,
    pub preview: String,
}

pub struct LedgerOptions {
    /// Minimum byte size of tool result content to ledger (default 2048)
    pub min_bytes: usize,
    /// Maximum number of entries to emit in the index
    pub max_lines: usize,
    /// Maximum total bytes for the index output
    pub max_index_bytes: usize,
}

impl Default for LedgerOptions {
    fn default() -> Self {
        LedgerOptions {
            min_bytes: 2048,
            max_lines: 40,
            max_index_bytes: 4096,
        }
    }
}

/// Parse a transcript JSONL file, store large tool results content-addressed,
/// and return ledger entries for the index.
pub fn build_ledger(
    transcript_path: &Path,
    store: &Store,
    opts: &LedgerOptions,
) -> std::io::Result<Vec<LedgerEntry>> {
    let file = std::fs::File::open(transcript_path)?;
    let reader = BufReader::new(file);
    let mut entries: Vec<LedgerEntry> = Vec::new();

    for line in reader.lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let val: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        extract_from_message(&val, store, opts, &mut entries)?;
    }

    Ok(entries)
}

/// Recursively extract tool_result blocks from a message object.
fn extract_from_message(
    val: &Value,
    store: &Store,
    opts: &LedgerOptions,
    entries: &mut Vec<LedgerEntry>,
) -> std::io::Result<()> {
    match val {
        Value::Object(obj) => {
            // Check if this object is a tool_result block
            if obj.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                let content = extract_content_str(obj.get("content"));
                let tool_name = obj
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                if content.len() >= opts.min_bytes {
                    let sha = store.put(content.as_bytes())?;
                    let preview = make_preview(&content);
                    entries.push(LedgerEntry {
                        sha,
                        tool_name,
                        preview,
                    });
                }
                return Ok(());
            }

            // Look for tool_response at top level (PostToolUse hook format)
            if obj.contains_key("tool_response") && obj.contains_key("tool_name") {
                let tool_name = obj
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let response = obj.get("tool_response").unwrap();
                let content = response_to_text(response);
                if content.len() >= opts.min_bytes {
                    let sha = store.put(content.as_bytes())?;
                    let preview = make_preview(&content);
                    entries.push(LedgerEntry {
                        sha,
                        tool_name,
                        preview,
                    });
                }
                return Ok(());
            }

            // Recurse into "content" array (standard message format).
            // If found, return early — don't also recurse into all other values
            // to avoid double-counting the same tool_result blocks.
            if let Some(Value::Array(content_arr)) = obj.get("content") {
                for item in content_arr {
                    extract_from_message(item, store, opts, entries)?;
                }
                return Ok(());
            }

            // Unknown structure: recurse into all nested objects/arrays
            for (_k, v) in obj {
                if matches!(v, Value::Object(_) | Value::Array(_)) {
                    extract_from_message(v, store, opts, entries)?;
                }
            }
        }
        Value::Array(arr) => {
            for item in arr {
                extract_from_message(item, store, opts, entries)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Extract text content from a tool_result's "content" field, which may be
/// a string, an array of content blocks, or a JSON object.
fn extract_content_str(val: Option<&Value>) -> String {
    match val {
        None => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|item| {
                if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                    item.get("text").and_then(|t| t.as_str()).map(String::from)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Extract text from a tool_response value (Bash stdout, Read file.content, etc.)
fn response_to_text(val: &Value) -> String {
    match val {
        Value::String(s) => s.clone(),
        Value::Object(obj) => {
            // Bash
            if let Some(s) = obj.get("stdout").and_then(|v| v.as_str()) {
                if !s.is_empty() {
                    return s.to_string();
                }
            }
            // Read
            if let Some(content) = obj
                .get("file")
                .and_then(|f| f.get("content"))
                .and_then(|c| c.as_str())
            {
                return content.to_string();
            }
            // Generic: content/output/text/result
            for key in &["content", "output", "text", "result"] {
                if let Some(s) = obj.get(*key).and_then(|v| v.as_str()) {
                    if !s.is_empty() {
                        return s.to_string();
                    }
                }
            }
            serde_json::to_string(val).unwrap_or_default()
        }
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn make_preview(content: &str) -> String {
    let first: String = content.chars().take(80).collect();
    // Collapse whitespace/newlines into spaces for readability
    first.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Format ledger entries as the BOOMERANG:CREF index string.
pub fn format_index(entries: &[LedgerEntry], opts: &LedgerOptions) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut total_bytes = 0usize;

    for entry in entries.iter().take(opts.max_lines) {
        let line = format!(
            "BOOMERANG:CREF:{} {} {}",
            entry.sha, entry.tool_name, entry.preview
        );
        let line_bytes = line.len() + 1; // +1 for newline
        if total_bytes + line_bytes > opts.max_index_bytes {
            break;
        }
        total_bytes += line_bytes;
        lines.push(line);
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use std::io::Write;
    use tempfile::TempDir;

    fn temp_store() -> (TempDir, Store) {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path()).unwrap();
        (dir, store)
    }

    fn write_transcript(dir: &TempDir, lines: &[&str]) -> std::path::PathBuf {
        let path = dir.path().join("transcript.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
        path
    }

    #[test]
    fn test_ledger_parses_tool_result_string() {
        let (tmpdir, store) = temp_store();
        let content = "x".repeat(3000);
        let line = format!(
            r#"{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"tu_1","tool_name":"Bash","content":"{content}"}}]}}"#,
        );
        let path = write_transcript(&tmpdir, &[&line]);
        let opts = LedgerOptions {
            min_bytes: 100,
            ..Default::default()
        };
        let entries = build_ledger(&path, &store, &opts).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].tool_name, "Bash");
        assert!(!entries[0].sha.is_empty());
    }

    #[test]
    fn test_ledger_skips_small_content() {
        let (tmpdir, store) = temp_store();
        let content = "small";
        let line = format!(
            r#"{{"role":"user","content":[{{"type":"tool_result","tool_name":"Read","content":"{content}"}}]}}"#,
        );
        let path = write_transcript(&tmpdir, &[&line]);
        let opts = LedgerOptions {
            min_bytes: 2048,
            ..Default::default()
        };
        let entries = build_ledger(&path, &store, &opts).unwrap();
        assert_eq!(entries.len(), 0);
    }

    #[test]
    fn test_ledger_handles_array_content() {
        let (tmpdir, store) = temp_store();
        let text = "some long text ".repeat(200);
        let line = serde_json::json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_name": "WebFetch",
                "content": [{"type": "text", "text": text}]
            }]
        })
        .to_string();
        let path = write_transcript(&tmpdir, &[&line]);
        let opts = LedgerOptions {
            min_bytes: 100,
            ..Default::default()
        };
        let entries = build_ledger(&path, &store, &opts).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].tool_name, "WebFetch");
    }

    #[test]
    fn test_ledger_multiple_results() {
        let (tmpdir, store) = temp_store();
        let long = "y".repeat(3000);
        let line1 = format!(
            r#"{{"role":"user","content":[{{"type":"tool_result","tool_name":"Bash","content":"{long}"}}]}}"#,
        );
        let line2 = format!(
            r#"{{"role":"user","content":[{{"type":"tool_result","tool_name":"Read","content":"{long}"}}]}}"#,
        );
        let path = write_transcript(&tmpdir, &[&line1, &line2]);
        let opts = LedgerOptions {
            min_bytes: 100,
            ..Default::default()
        };
        let entries = build_ledger(&path, &store, &opts).unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn test_format_index_caps_at_max_lines() {
        let entries: Vec<LedgerEntry> = (0..50)
            .map(|i| LedgerEntry {
                sha: format!("{:064x}", i),
                tool_name: "Bash".to_string(),
                preview: "preview text".to_string(),
            })
            .collect();
        let opts = LedgerOptions {
            max_lines: 40,
            max_index_bytes: 100_000,
            ..Default::default()
        };
        let index = format_index(&entries, &opts);
        let count = index.lines().count();
        assert!(count <= 40, "expected <= 40 lines, got {count}");
    }

    #[test]
    fn test_format_index_caps_at_max_bytes() {
        let entries: Vec<LedgerEntry> = (0..20)
            .map(|i| LedgerEntry {
                sha: format!("{:064x}", i),
                tool_name: "Bash".to_string(),
                preview: "x".repeat(80),
            })
            .collect();
        let opts = LedgerOptions {
            max_lines: 40,
            max_index_bytes: 300,
            ..Default::default()
        };
        let index = format_index(&entries, &opts);
        assert!(
            index.len() <= 300 + 200,
            "index too large: {} bytes",
            index.len()
        );
    }

    #[test]
    fn test_ledger_empty_file() {
        let (tmpdir, store) = temp_store();
        let path = write_transcript(&tmpdir, &[]);
        let opts = LedgerOptions::default();
        let entries = build_ledger(&path, &store, &opts).unwrap();
        assert_eq!(entries.len(), 0);
    }

    #[test]
    fn test_ledger_ignores_invalid_json_lines() {
        let (tmpdir, store) = temp_store();
        let path = write_transcript(&tmpdir, &["not json", "also not json"]);
        let opts = LedgerOptions::default();
        let entries = build_ledger(&path, &store, &opts).unwrap();
        assert_eq!(entries.len(), 0);
    }
}
