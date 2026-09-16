//! Per-tool and per-key retrieve-rate tracking with adaptive thresholds.
//!
//! Persists counters to `{store_dir}/.piggybank-metrics.json` using the
//! same atomic-rename write pattern as the analytics file. All I/O errors
//! are silently swallowed — metrics must never crash the MCP server.
//!
//! Adaptive threshold: when a tool's retrieve_rate over the last 50 events
//! exceeds 0.30, its `suggested_min_bytes` rises to 2048. `compress_budget`
//! honours this when a `tool` hint param is supplied.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

const METRICS_FILE: &str = ".piggybank-metrics.json";

/// Look back at this many compress/retrieve events per tool.
const RECENT_WINDOW: usize = 50;

/// Retrieve-rate threshold above which we suggest a higher min_bytes.
const ADAPTIVE_RATE_THRESHOLD: f64 = 0.30;

/// Suggested min_bytes when retrieve_rate is high.
pub const ADAPTIVE_MIN_BYTES_HIGH: usize = 2048;

// ── Internal data ─────────────────────────────────────────────────────────────

#[derive(Default, Clone)]
struct ToolEntry {
    compressions: u64,
    retrieves: u64,
    bytes_saved: i64,
    bytes_retrieved: u64,
    /// Circular event window: 0 = compress, 1 = retrieve.
    recent: Vec<u8>,
}

impl ToolEntry {
    fn retrieve_rate(&self) -> f64 {
        if self.compressions == 0 {
            return 0.0;
        }
        self.retrieves as f64 / self.compressions as f64
    }

    fn recent_retrieve_rate(&self) -> f64 {
        if self.recent.is_empty() {
            return 0.0;
        }
        let retrieves = self.recent.iter().filter(|&&x| x == 1).count();
        retrieves as f64 / self.recent.len() as f64
    }

    pub fn suggested_min_bytes(&self) -> usize {
        if self.recent_retrieve_rate() > ADAPTIVE_RATE_THRESHOLD {
            ADAPTIVE_MIN_BYTES_HIGH
        } else {
            0 // 0 = use system default
        }
    }

    fn push_event(&mut self, is_retrieve: bool) {
        if self.recent.len() >= RECENT_WINDOW {
            self.recent.remove(0);
        }
        self.recent.push(if is_retrieve { 1 } else { 0 });
    }
}

#[derive(Default, Clone)]
struct KeyEntry {
    compressions: u64,
    retrieves: u64,
    bytes_saved: i64,
    bytes_retrieved: u64,
}

struct Inner {
    tools: HashMap<String, ToolEntry>,
    keys: HashMap<String, KeyEntry>,
    masked_total: u64,
}

// ── Public API ────────────────────────────────────────────────────────────────

pub struct MetricsStore {
    path: PathBuf,
    inner: Mutex<Inner>,
}

impl MetricsStore {
    pub fn open(store_dir: &str) -> Self {
        let path = std::path::Path::new(store_dir).join(METRICS_FILE);
        let inner = load_from_file(&path);
        MetricsStore {
            path,
            inner: Mutex::new(inner),
        }
    }

    /// Record a compression event. `bytes_saved` may be negative if compressed > original.
    pub fn record_compress(&self, tool: Option<&str>, key: Option<&str>, bytes_saved: i64) {
        let Ok(mut g) = self.inner.lock() else { return };
        if let Some(t) = tool {
            let e = g.tools.entry(t.to_string()).or_default();
            e.compressions += 1;
            e.bytes_saved += bytes_saved;
            e.push_event(false);
        }
        if let Some(k) = key {
            let e = g.keys.entry(k.to_string()).or_default();
            e.compressions += 1;
            e.bytes_saved += bytes_saved;
        }
        persist(&self.path, &g);
    }

    /// Record a retrieve event.
    pub fn record_retrieve(&self, tool: Option<&str>, key: Option<&str>, bytes_retrieved: u64) {
        let Ok(mut g) = self.inner.lock() else { return };
        if let Some(t) = tool {
            let e = g.tools.entry(t.to_string()).or_default();
            e.retrieves += 1;
            e.bytes_retrieved += bytes_retrieved;
            e.push_event(true);
        }
        if let Some(k) = key {
            let e = g.keys.entry(k.to_string()).or_default();
            e.retrieves += 1;
            e.bytes_retrieved += bytes_retrieved;
        }
        persist(&self.path, &g);
    }

    /// Accumulate masked secret count from a compress call.
    pub fn record_masked(&self, count: u64) {
        if count == 0 {
            return;
        }
        let Ok(mut g) = self.inner.lock() else { return };
        g.masked_total += count;
        persist(&self.path, &g);
    }

    /// Return the adaptive suggested_min_bytes for a tool (0 = system default).
    pub fn suggested_min_bytes(&self, tool: &str) -> usize {
        let Ok(g) = self.inner.lock() else { return 0 };
        g.tools
            .get(tool)
            .map(|e| e.suggested_min_bytes())
            .unwrap_or(0)
    }

    /// Build the `by_tool` / `by_key` / `masked_total` section for the stats response.
    pub fn stats_json(&self) -> Value {
        let Ok(g) = self.inner.lock() else {
            return json!({});
        };

        let mut tools_vec: Vec<(&String, &ToolEntry)> = g.tools.iter().collect();
        tools_vec.sort_by(|a, b| b.1.compressions.cmp(&a.1.compressions));
        let by_tool: Vec<Value> = tools_vec
            .iter()
            .take(20)
            .map(|(name, e)| {
                json!({
                    "tool": name,
                    "compressions": e.compressions,
                    "retrieves": e.retrieves,
                    "retrieve_rate": format!("{:.3}", e.retrieve_rate()),
                    "recent_retrieve_rate": format!("{:.3}", e.recent_retrieve_rate()),
                    "net_saved_bytes": e.bytes_saved.saturating_sub(e.bytes_retrieved as i64),
                    "suggested_min_bytes": e.suggested_min_bytes(),
                })
            })
            .collect();

        let mut keys_vec: Vec<(&String, &KeyEntry)> = g.keys.iter().collect();
        keys_vec.sort_by(|a, b| b.1.compressions.cmp(&a.1.compressions));
        let by_key: Vec<Value> = keys_vec
            .iter()
            .take(20)
            .map(|(name, e)| {
                let rr = if e.compressions > 0 {
                    e.retrieves as f64 / e.compressions as f64
                } else {
                    0.0
                };
                json!({
                    "key": name,
                    "compressions": e.compressions,
                    "retrieves": e.retrieves,
                    "retrieve_rate": format!("{:.3}", rr),
                    "net_saved_bytes": e.bytes_saved.saturating_sub(e.bytes_retrieved as i64),
                })
            })
            .collect();

        json!({
            "by_tool": by_tool,
            "by_key": by_key,
            "masked_total": g.masked_total,
        })
    }
}

// ── Private helpers ───────────────────────────────────────────────────────────

fn load_from_file(path: &std::path::Path) -> Inner {
    let mut inner = Inner {
        tools: HashMap::new(),
        keys: HashMap::new(),
        masked_total: 0,
    };
    let Ok(raw) = std::fs::read_to_string(path) else {
        return inner;
    };
    let Ok(v): Result<Value, _> = serde_json::from_str(&raw) else {
        return inner;
    };

    if let Some(obj) = v.get("tools").and_then(Value::as_object) {
        for (name, entry) in obj {
            let recent = if let Some(arr) = entry["recent"].as_array() {
                let full: Vec<u8> = arr
                    .iter()
                    .filter_map(|x| x.as_u64().map(|n| n as u8))
                    .collect();
                let start = full.len().saturating_sub(RECENT_WINDOW);
                full[start..].to_vec()
            } else {
                Vec::new()
            };
            let te = ToolEntry {
                compressions: entry["compressions"].as_u64().unwrap_or(0),
                retrieves: entry["retrieves"].as_u64().unwrap_or(0),
                bytes_saved: entry["bytes_saved"].as_i64().unwrap_or(0),
                bytes_retrieved: entry["bytes_retrieved"].as_u64().unwrap_or(0),
                recent,
            };
            inner.tools.insert(name.clone(), te);
        }
    }

    if let Some(obj) = v.get("keys").and_then(Value::as_object) {
        for (name, entry) in obj {
            inner.keys.insert(
                name.clone(),
                KeyEntry {
                    compressions: entry["compressions"].as_u64().unwrap_or(0),
                    retrieves: entry["retrieves"].as_u64().unwrap_or(0),
                    bytes_saved: entry["bytes_saved"].as_i64().unwrap_or(0),
                    bytes_retrieved: entry["bytes_retrieved"].as_u64().unwrap_or(0),
                },
            );
        }
    }

    inner.masked_total = v.get("masked_total").and_then(Value::as_u64).unwrap_or(0);
    inner
}

fn persist(path: &PathBuf, inner: &Inner) {
    let mut tools_obj = serde_json::Map::new();
    for (name, e) in &inner.tools {
        tools_obj.insert(
            name.clone(),
            json!({
                "compressions": e.compressions,
                "retrieves": e.retrieves,
                "bytes_saved": e.bytes_saved,
                "bytes_retrieved": e.bytes_retrieved,
                "recent": e.recent.iter().map(|&x| x as u64).collect::<Vec<_>>(),
            }),
        );
    }

    let mut keys_obj = serde_json::Map::new();
    for (name, e) in &inner.keys {
        keys_obj.insert(
            name.clone(),
            json!({
                "compressions": e.compressions,
                "retrieves": e.retrieves,
                "bytes_saved": e.bytes_saved,
                "bytes_retrieved": e.bytes_retrieved,
            }),
        );
    }

    let data = json!({
        "tools": tools_obj,
        "keys": keys_obj,
        "masked_total": inner.masked_total,
    });

    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    if std::fs::write(&tmp, data.to_string().as_bytes()).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_store() -> (std::path::PathBuf, MetricsStore) {
        let dir = std::env::temp_dir().join(format!(
            "pb-metrics-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let ms = MetricsStore::open(dir.to_str().unwrap());
        (dir, ms)
    }

    #[test]
    fn record_and_retrieve_rate_computed() {
        let (dir, ms) = make_store();
        ms.record_compress(Some("Bash"), None, 1000);
        ms.record_compress(Some("Bash"), None, 1000);
        ms.record_retrieve(Some("Bash"), None, 200);

        let stats = ms.stats_json();
        let by_tool = stats["by_tool"].as_array().unwrap();
        let bash = by_tool.iter().find(|e| e["tool"] == "Bash").unwrap();
        assert_eq!(bash["compressions"], 2);
        assert_eq!(bash["retrieves"], 1);
        // retrieve_rate = 1/2 = 0.500
        assert_eq!(bash["retrieve_rate"], "0.500");

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn suggested_min_bytes_elevated_when_rate_high() {
        let (dir, ms) = make_store();
        // Push 50 events, 20 retrieves and 30 compressions → rate = 20/50 = 0.4 > 0.3
        for _ in 0..30 {
            ms.record_compress(Some("Read"), None, 500);
        }
        for _ in 0..20 {
            ms.record_retrieve(Some("Read"), None, 100);
        }
        assert_eq!(ms.suggested_min_bytes("Read"), ADAPTIVE_MIN_BYTES_HIGH);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn suggested_min_bytes_zero_when_rate_low() {
        let (dir, ms) = make_store();
        for _ in 0..40 {
            ms.record_compress(Some("Write"), None, 500);
        }
        for _ in 0..5 {
            ms.record_retrieve(Some("Write"), None, 100);
        }
        assert_eq!(ms.suggested_min_bytes("Write"), 0);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn masked_total_accumulates() {
        let (dir, ms) = make_store();
        ms.record_masked(3);
        ms.record_masked(2);
        let stats = ms.stats_json();
        assert_eq!(stats["masked_total"], 5);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn persists_and_loads() {
        let dir = std::env::temp_dir().join(format!("pb-metrics-persist-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        {
            let ms = MetricsStore::open(dir.to_str().unwrap());
            ms.record_compress(Some("Bash"), Some("path/to/file"), 800);
            ms.record_retrieve(Some("Bash"), Some("path/to/file"), 100);
            ms.record_masked(7);
        }
        // Re-open and check persistence
        let ms2 = MetricsStore::open(dir.to_str().unwrap());
        let stats = ms2.stats_json();
        assert_eq!(stats["masked_total"], 7);
        let by_tool = stats["by_tool"].as_array().unwrap();
        let bash = by_tool.iter().find(|e| e["tool"] == "Bash").unwrap();
        assert_eq!(bash["compressions"], 1);
        assert_eq!(bash["retrieves"], 1);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn recent_window_capped_at_50() {
        let (dir, ms) = make_store();
        for _ in 0..70 {
            ms.record_compress(Some("Agent"), None, 10);
        }
        let g = ms.inner.lock().unwrap();
        assert!(g.tools["Agent"].recent.len() <= RECENT_WINDOW);
        drop(g);
        std::fs::remove_dir_all(dir).ok();
    }
}
