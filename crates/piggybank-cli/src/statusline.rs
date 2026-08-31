//! `piggybank statusline` — one line of felt savings for a shell prompt or agent statusline.
//!
//! Aggregates two sources living in the store directory:
//! - `.piggybank-analytics.json` — lifetime totals recorded by the MCP server
//! - `hook-savings.jsonl` — per-rewrite `{"d":"YYYY-MM-DD","saved":N,...}` lines
//!   appended by the PostToolUse hook
//!
//! Token and dollar figures use the same byte-based heuristic as the MCP stats path;
//! keep these constants in sync with `mcp.rs` until a real tokenizer lands.

use serde_json::Value;
use std::path::Path;
use std::process::ExitCode;

const BYTES_PER_TOKEN: f64 = 4.0;
const DEFAULT_RATE_PER_MTOK: f64 = 3.0;

#[derive(Debug, Default, PartialEq)]
pub struct SavingsStats {
    pub lifetime_bytes: u64,
    pub today_bytes: u64,
}

impl SavingsStats {
    pub fn tokens(bytes: u64) -> f64 {
        bytes as f64 / BYTES_PER_TOKEN
    }

    pub fn dollars(bytes: u64) -> f64 {
        Self::tokens(bytes) / 1_000_000.0 * DEFAULT_RATE_PER_MTOK
    }
}

/// Gather savings from a store directory. Missing or malformed files count as zero —
/// a statusline must never fail the prompt it is embedded in.
pub fn compute_stats(store_dir: &Path, today: &str) -> SavingsStats {
    let mut stats = SavingsStats::default();

    if let Ok(raw) = std::fs::read_to_string(store_dir.join(".piggybank-analytics.json")) {
        if let Ok(v) = serde_json::from_str::<Value>(&raw) {
            let orig = v["total_original"].as_u64().unwrap_or(0);
            let comp = v["total_compressed"].as_u64().unwrap_or(0);
            let appended = v["append_bytes_avoided"].as_u64().unwrap_or(0);
            stats.lifetime_bytes += orig.saturating_sub(comp) + appended;
        }
    }

    if let Ok(raw) = std::fs::read_to_string(store_dir.join("hook-savings.jsonl")) {
        for line in raw.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let saved = v["saved"].as_u64().unwrap_or(0);
            stats.lifetime_bytes += saved;
            if v["d"].as_str() == Some(today) {
                stats.today_bytes += saved;
            }
        }
    }

    stats
}

fn fmt_tokens(bytes: u64) -> String {
    let t = SavingsStats::tokens(bytes);
    if t >= 1_000_000.0 {
        format!("{:.1}M", t / 1_000_000.0)
    } else if t >= 1_000.0 {
        format!("{:.1}k", t / 1_000.0)
    } else {
        format!("{t:.0}")
    }
}

pub fn render(stats: &SavingsStats, plain: bool) -> String {
    let icon = if plain { "pb" } else { "\u{1F437}" };
    format!(
        "{icon} today {} tok (${:.2}) · lifetime {} tok (${:.2})",
        fmt_tokens(stats.today_bytes),
        SavingsStats::dollars(stats.today_bytes),
        fmt_tokens(stats.lifetime_bytes),
        SavingsStats::dollars(stats.lifetime_bytes),
    )
}

fn today_string() -> String {
    // %F from `date` keeps this consistent with what the hook writes, without pulling
    // a chrono dependency into the CLI for one timestamp.
    std::process::Command::new("date")
        .arg("+%F")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

pub fn run_statusline(args: &[String]) -> ExitCode {
    let store_dir = args
        .iter()
        .position(|a| a == "--store-dir")
        .and_then(|i| args.get(i + 1).cloned())
        .or_else(|| {
            std::env::var("PIGGYBANK_STORE_DIR").ok().or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|home| format!("{home}/.piggybank/store"))
            })
        })
        .unwrap_or_else(|| ".piggybank-store".to_string());
    let plain = args.iter().any(|a| a == "--plain");

    let stats = compute_stats(Path::new(&store_dir), &today_string());
    println!("{}", render(&stats, plain));
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "piggybank-statusline-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn empty_store_is_all_zero() {
        let dir = temp_store();
        let stats = compute_stats(&dir, "2026-08-31");
        assert_eq!(stats, SavingsStats::default());
    }

    #[test]
    fn aggregates_analytics_and_hook_lines() {
        let dir = temp_store();
        std::fs::write(
            dir.join(".piggybank-analytics.json"),
            r#"{"total_original":10000,"total_compressed":4000,"append_bytes_avoided":500}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("hook-savings.jsonl"),
            concat!(
                "{\"d\":\"2026-08-31\",\"saved\":1000,\"tool\":\"Bash\"}\n",
                "{\"d\":\"2026-08-30\",\"saved\":2000,\"tool\":\"Read\"}\n",
                "not json at all\n",
            ),
        )
        .unwrap();

        let stats = compute_stats(&dir, "2026-08-31");
        // lifetime = (10000-4000) + 500 + 1000 + 2000
        assert_eq!(stats.lifetime_bytes, 9500);
        assert_eq!(stats.today_bytes, 1000);
    }

    #[test]
    fn malformed_analytics_counts_zero() {
        let dir = temp_store();
        std::fs::write(dir.join(".piggybank-analytics.json"), "{broken").unwrap();
        let stats = compute_stats(&dir, "2026-08-31");
        assert_eq!(stats.lifetime_bytes, 0);
    }

    #[test]
    fn render_formats_scales() {
        let stats = SavingsStats {
            lifetime_bytes: 8_000_000, // 2.0M tokens
            today_bytes: 8_000,        // 2.0k tokens
        };
        let line = render(&stats, true);
        assert!(line.contains("today 2.0k tok"), "{line}");
        assert!(line.contains("lifetime 2.0M tok"), "{line}");
        assert!(line.starts_with("pb "), "{line}");
    }
}
