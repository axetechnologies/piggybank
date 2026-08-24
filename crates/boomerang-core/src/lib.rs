use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;

mod text;
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
        let id = hex::encode(Sha256::digest(bytes));
        let path = self.root.join(&id);
        if !path.exists() {
            fs::write(&path, bytes)?;
        }
        Ok(id)
    }

    pub fn get(&self, id: &str) -> std::io::Result<Vec<u8>> {
        fs::read(self.root.join(id))
    }
}

const TABLE_MARKER: &str = "__boomerang_table__";

/// Compress JSON losslessly. Homogeneous arrays of objects — the shape of
/// almost every API response or tool-result list — become a columnar
/// table: keys written once, then rows of values, instead of repeating
/// every key name once per element. Everything else compresses structurally
/// (recursing into nested arrays/objects looking for tables to build).
///
/// This never calls into `Store`: JSON structural redundancy is removable
/// without discarding any information, so there's nothing to hold back for
/// later retrieval. `Store` exists for the lossy compressors (logs, diffs)
/// that come after this.
pub fn compress_json(input: &[u8]) -> serde_json::Result<Vec<u8>> {
    let value: Value = serde_json::from_slice(input)?;
    serde_json::to_vec(&compress_value(&value))
}

pub fn decompress_json(input: &[u8]) -> serde_json::Result<Vec<u8>> {
    let value: Value = serde_json::from_slice(input)?;
    serde_json::to_vec(&decompress_value(&value))
}

fn compress_value(value: &Value) -> Value {
    match value {
        Value::Array(items) if items.len() >= 2 => match columnarize(items) {
            Some(table) => table,
            None => Value::Array(items.iter().map(compress_value).collect()),
        },
        Value::Object(map) => {
            Value::Object(map.iter().map(|(k, v)| (k.clone(), compress_value(v))).collect())
        }
        other => other.clone(),
    }
}

/// Turn a homogeneous array of objects (identical key set on every element)
/// into `{ "__boomerang_table__": true, "keys": [...], "rows": [[...], ...] }`.
/// Returns `None` if the array isn't homogeneous — the caller falls back to
/// compressing each element independently rather than forcing a bad fit.
fn columnarize(items: &[Value]) -> Option<Value> {
    let first_obj = items[0].as_object()?;
    let keys: Vec<String> = first_obj.keys().cloned().collect();

    let mut rows = Vec::with_capacity(items.len());
    for item in items {
        let obj = item.as_object()?;
        if obj.len() != keys.len() {
            return None;
        }
        let mut row = Vec::with_capacity(keys.len());
        for k in &keys {
            row.push(compress_value(obj.get(k)?));
        }
        rows.push(Value::Array(row));
    }

    Some(serde_json::json!({
        TABLE_MARKER: true,
        "keys": keys,
        "rows": rows,
    }))
}

fn decompress_value(value: &Value) -> Value {
    match value {
        Value::Object(map) if map.get(TABLE_MARKER) == Some(&Value::Bool(true)) => {
            let keys = map.get("keys").and_then(Value::as_array).cloned().unwrap_or_default();
            let rows = map.get("rows").and_then(Value::as_array).cloned().unwrap_or_default();
            let items = rows
                .into_iter()
                .map(|row| {
                    let row = row.as_array().cloned().unwrap_or_default();
                    let obj: serde_json::Map<String, Value> = keys
                        .iter()
                        .zip(row)
                        .map(|(k, v)| (k.as_str().unwrap_or_default().to_string(), decompress_value(&v)))
                        .collect();
                    Value::Object(obj)
                })
                .collect();
            Value::Array(items)
        }
        Value::Array(items) => Value::Array(items.iter().map(decompress_value).collect()),
        Value::Object(map) => {
            Value::Object(map.iter().map(|(k, v)| (k.clone(), decompress_value(v))).collect())
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one invariant that matters more than any compression ratio:
    /// compress -> decompress must reproduce the exact same JSON value.
    fn assert_round_trips(input: &str) {
        let original: Value = serde_json::from_str(input).unwrap();
        let compressed = compress_json(input.as_bytes()).unwrap();
        let decompressed = decompress_json(&compressed).unwrap();
        let restored: Value = serde_json::from_slice(&decompressed).unwrap();
        assert_eq!(original, restored, "round-trip must be exact");
    }

    #[test]
    fn homogeneous_array_round_trips_and_shrinks() {
        let input = r#"[
            {"id": 1, "status": "healthy", "region": "us-east"},
            {"id": 2, "status": "healthy", "region": "us-east"},
            {"id": 3, "status": "degraded", "region": "us-west"}
        ]"#;
        let compressed = compress_json(input.as_bytes()).unwrap();
        assert!(compressed.len() < input.len(), "table form should be smaller");
        assert_round_trips(input);
    }

    #[test]
    fn heterogeneous_array_still_round_trips() {
        assert_round_trips(r#"[{"a": 1}, {"b": 2, "c": 3}, "not an object"]"#);
    }

    #[test]
    fn nested_object_round_trips() {
        assert_round_trips(r#"{"meta": {"ok": true}, "items": [{"x": 1}, {"x": 2}]}"#);
    }

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
}
