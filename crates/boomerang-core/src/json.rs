use crate::Store;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Error, ErrorKind, Result as IoResult};

const TABLE_MARKER: &str = "__boomerang_table__";
const INTERN_MARKER: &str = "__boomerang_intern__";
const REF_MARKER: &str = "__boomerang_ref__";
/// General namespace all three markers above live under. Any object key in
/// genuine *input* data that happens to start with this gets escaped before
/// compression (see `escape_reserved_keys`) so it can never be mistaken for
/// a marker we generated — without this, input containing e.g. a literal
/// `"__boomerang_table__": true` key would silently decompress to the wrong
/// value, since decompress_value can't tell "ours" from "coincidentally
/// identical." Confirmed as a real bug before this fix existed, not a
/// theoretical one.
const RESERVED_NAMESPACE: &str = "__boomerang_";
/// Escape prefix added to any pre-existing key starting with
/// `RESERVED_NAMESPACE`. Deliberately itself starts with
/// `RESERVED_NAMESPACE`, which is what makes the scheme handle "the input
/// already has a key that looks escaped" correctly: encoding always adds
/// exactly one layer, decoding always removes exactly one, so it's a clean
/// bijection regardless of how many `__boomerang_`-looking layers the
/// original key already had.
const ESCAPE_PREFIX: &str = "__boomerang_esc_";

/// Below this serialized length, a subtree is never worth interning — the
/// `{"__boomerang_ref__":N}` marker itself costs ~20+ bytes, so replacing
/// something smaller than that with a ref is pure overhead even if it
/// repeats. This is a cheap pre-filter; the real guarantee is the
/// size check in `intern_value` below, which verifies interning actually
/// shrank the output before committing to it.
///
/// The dedup key below is `value.to_string()`, which costs time
/// proportional to a subtree's own size, called once per node — a naive
/// worst-case analysis suggests this could go quadratic on deeply nested
/// documents. Measured instead of assumed: on a real API-response shape
/// scaled from 20KB to 4.2MB (200x), wall time went from 15ms to 2.0s
/// (~134x) — roughly linear to mildly superlinear in practice, not the
/// quadratic blowup the shape alone implies, because most subtrees in
/// real JSON are small. Still faster than headroom's ~1.6s on a file
/// 200x smaller. Revisit with an incrementally-computed structural hash
/// (combine child hashes bottom-up instead of re-serializing) only if a
/// real workload's shape ever makes this bite.
const MIN_INTERN_LEN: usize = 32;

/// Compress JSON losslessly, in two independent, composable passes:
///
/// 1. **Columnarize** — homogeneous arrays of objects (the shape of almost
///    every API response or tool-result list) become a table: keys written
///    once, then rows of values, instead of repeating every key name once
///    per element.
/// 2. **Intern** — after columnarization, find any subtree (object, array,
///    or string) that appears more than once *anywhere* in the result —
///    e.g. the same nested "author" object showing up in ten different
///    commits — and replace every occurrence but the first with a short
///    reference into a dictionary. Columnarization alone only dedupes key
///    *names* across rows; this is what catches repeated *values*, which
///    is where most of the size lives in real API responses (the same
///    user/account/metadata object attached to many records).
///
/// Neither pass ever calls into `Store`: this is all removable redundancy,
/// not information loss, so there's nothing to hold back for retrieval.
pub fn compress_json(input: &[u8]) -> serde_json::Result<Vec<u8>> {
    let value: Value = serde_json::from_slice(input)?;
    let escaped = escape_reserved_keys(&value);
    let columnarized = compress_value(&escaped);
    let interned = intern_value(&columnarized);
    serde_json::to_vec(&interned)
}

pub fn decompress_json(input: &[u8]) -> serde_json::Result<Vec<u8>> {
    let value: Value = serde_json::from_slice(input)?;
    let uninterned = unintern_value(&value);
    let restored = decompress_value(&uninterned);
    let unescaped = unescape_reserved_keys(&restored);
    serde_json::to_vec(&unescaped)
}

/// Below this serialized length, cross-call promotion is never attempted -
/// higher bar than `MIN_INTERN_LEN` because this costs an actual `Store`
/// round-trip (a filesystem stat/read), not just a marker-size comparison.
/// Set comfortably above the marker's own fixed cost
/// (`{"__boomerang_cref__":"<64-hex-char id>"}` is 89 bytes) - this is only
/// a cheap pre-filter, same role `MIN_INTERN_LEN` plays for `intern_value`;
/// the actual guarantee is the exact size check in `cross_call_intern`
/// below. Getting this wrong is a real bug, not just a missed optimization:
/// an earlier version left this at 64, *below* the marker's own cost, so
/// anything in the 64-89 byte range unconditionally grew on promotion -
/// caught by actually reading compressed output during end-to-end testing,
/// not by trusting size numbers alone.
const CROSS_CALL_MIN_LEN: usize = 100;

const CROSS_CALL_REF_MARKER: &str = "__boomerang_cref__";

/// Cross-call, cross-document sibling of `compress_json`: everything it
/// does, plus persistent structural memory via `store`. An agent that reads
/// a similar or identical JSON blob across *separate* `compress` calls —
/// polling a status endpoint, re-fetching a metadata object that hasn't
/// changed, seeing the same paginated user record on page 2 as page 1 — has
/// each repeat cost nothing once the first occurrence is on record,
/// regardless of which document it was originally seen in. `Session`
/// already gives text this kind of memory (diff against last seen, keyed
/// by a caller-supplied key); this is the JSON-shaped, key-*free* version -
/// content-addressing means the same value is recognized wherever it
/// reappears, not just under one tracked key. compress_json itself stays a
/// pure, store-free function for callers (like the plain CLI) that
/// genuinely want a single, stateless, self-contained transform.
///
/// Mechanism: after the existing within-document passes, walk the result
/// bottom-up; any object whose serialized form clears `CROSS_CALL_MIN_LEN`
/// gets looked up in `store` by content hash. Seen before (by *any* prior
/// call against this store, not just this document) → replaced with a
/// `{"__boomerang_cref__": id}` reference. Not seen before → left inline,
/// but written to `store` so the *next* call recognizes it. First sight of
/// a large value costs nothing extra beyond a store write; every repeat
/// after that costs almost nothing.
///
/// One real characteristic of "bottom-up" worth being explicit about,
/// found by testing repeated identical large documents rather than
/// assumed: a deeply-nested structure doesn't fully collapse to one
/// reference on its very next repeat. A promoted child produces a *new*
/// shape for its parent (inline child → `{"__boomerang_cref__":...}` is a
/// different value than before), so the parent itself is "first sight"
/// again at that point and only converges on ITS next repeat. A 200-row
/// table (table-wrapper → rows-array, two meaningful levels) measured at
/// 60195 → 28867 → 357 → 224 bytes across four identical repeats — real
/// savings from the second call on, full convergence by the third or
/// fourth for something this deep. Shallower repeats (a single large field
/// a few levels down, not the whole top-level document) converge
/// immediately on the second call, as in this module's own tests.
pub fn compress_json_with_store(input: &[u8], store: &Store) -> IoResult<Vec<u8>> {
    let value: Value = serde_json::from_slice(input).map_err(json_io_err)?;
    let escaped = escape_reserved_keys(&value);
    let columnarized = compress_value(&escaped);
    let interned = intern_value(&columnarized);
    let cross_call = cross_call_intern(&interned, store)?;
    serde_json::to_vec(&cross_call).map_err(json_io_err)
}

pub fn decompress_json_with_store(input: &[u8], store: &Store) -> IoResult<Vec<u8>> {
    let value: Value = serde_json::from_slice(input).map_err(json_io_err)?;
    let resolved = resolve_cross_call_refs(&value, store)?;
    let uninterned = unintern_value(&resolved);
    let restored = decompress_value(&uninterned);
    let unescaped = unescape_reserved_keys(&restored);
    serde_json::to_vec(&unescaped).map_err(json_io_err)
}

fn json_io_err(e: serde_json::Error) -> Error {
    Error::new(ErrorKind::InvalidData, e)
}

fn cross_call_intern(value: &Value, store: &Store) -> IoResult<Value> {
    let recursed = match value {
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(cross_call_intern(item, store)?);
            }
            Value::Array(out)
        }
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k.clone(), cross_call_intern(v, store)?);
            }
            Value::Object(out)
        }
        other => return Ok(other.clone()), // scalars: never worth a store round trip
    };

    let serialized = serde_json::to_vec(&recursed).map_err(json_io_err)?;
    if serialized.len() < CROSS_CALL_MIN_LEN {
        return Ok(recursed);
    }
    let (id, already_existed) = store.put_check_existing(&serialized)?;
    let marker = json!({ CROSS_CALL_REF_MARKER: &id });
    let marker_len = serde_json::to_vec(&marker).map_err(json_io_err)?.len();
    // Same "pay for itself" guarantee as intern_value's size check: even
    // past the pre-filter above, only actually commit to the reference if
    // it's smaller than what it replaces. CROSS_CALL_MIN_LEN alone isn't
    // sufficient - it's a fixed threshold, but the marker's cost is also
    // fixed, so this is the check that's actually always correct.
    if already_existed && marker_len < serialized.len() {
        Ok(marker)
    } else {
        Ok(recursed)
    }
}

/// Reverses `cross_call_intern`. Resolves recursively: a value promoted
/// while it *contained* an already-promoted child (a ref inside a ref) is
/// stored with that nested reference embedded, so resolving one level isn't
/// enough — keep resolving whatever comes back until no marker remains.
fn resolve_cross_call_refs(value: &Value, store: &Store) -> IoResult<Value> {
    if let Value::Object(map) = value {
        if map.len() == 1 {
            if let Some(id) = map.get(CROSS_CALL_REF_MARKER).and_then(Value::as_str) {
                let bytes = store.get(id)?;
                let resolved: Value = serde_json::from_slice(&bytes).map_err(json_io_err)?;
                return resolve_cross_call_refs(&resolved, store);
            }
        }
    }

    match value {
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(resolve_cross_call_refs(item, store)?);
            }
            Ok(Value::Array(out))
        }
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k.clone(), resolve_cross_call_refs(v, store)?);
            }
            Ok(Value::Object(out))
        }
        other => Ok(other.clone()),
    }
}

/// Rename any object key starting with `RESERVED_NAMESPACE` by prepending
/// `ESCAPE_PREFIX`, recursively, everywhere in the tree. Runs before
/// columnarize/intern so those passes only ever see a tree where the
/// reserved namespace is exclusively ours. A no-op (and free) for the vast
/// majority of real JSON, which never touches this namespace.
fn escape_reserved_keys(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(escape_reserved_keys).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    let key = if k.starts_with(RESERVED_NAMESPACE) {
                        format!("{ESCAPE_PREFIX}{k}")
                    } else {
                        k.clone()
                    };
                    (key, escape_reserved_keys(v))
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Reverses `escape_reserved_keys` exactly. Runs after decompress_value has
/// already resolved every real marker, so any key still carrying
/// `ESCAPE_PREFIX` at this point was added by escape_reserved_keys and
/// nothing else — safe to strip unconditionally.
fn unescape_reserved_keys(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(unescape_reserved_keys).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    let key = k.strip_prefix(ESCAPE_PREFIX).unwrap_or(k).to_string();
                    (key, unescape_reserved_keys(v))
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

fn compress_value(value: &Value) -> Value {
    match value {
        Value::Array(items) if items.len() >= 2 => match columnarize(items) {
            Some(table) => table,
            None => Value::Array(items.iter().map(compress_value).collect()),
        },
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), compress_value(v)))
                .collect(),
        ),
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

    Some(json!({
        TABLE_MARKER: true,
        "keys": keys,
        "rows": rows,
    }))
}

fn decompress_value(value: &Value) -> Value {
    match value {
        Value::Object(map) if map.get(TABLE_MARKER) == Some(&Value::Bool(true)) => {
            let keys = map
                .get("keys")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let rows = map
                .get("rows")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let items = rows
                .into_iter()
                .map(|row| {
                    let row = row.as_array().cloned().unwrap_or_default();
                    let obj: serde_json::Map<String, Value> = keys
                        .iter()
                        .zip(row)
                        .map(|(k, v)| {
                            (
                                k.as_str().unwrap_or_default().to_string(),
                                decompress_value(&v),
                            )
                        })
                        .collect();
                    Value::Object(obj)
                })
                .collect();
            Value::Array(items)
        }
        Value::Array(items) => Value::Array(items.iter().map(decompress_value).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), decompress_value(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Find every subtree that appears more than once in `value` and replace
/// all but the first occurrence with a `{"__boomerang_ref__": i}` marker
/// pointing into a dictionary. Bottom-up: children are considered before
/// their parents, so a nested repeat (e.g. a string inside a repeated
/// object) gets its own, smaller reference rather than being duplicated
/// once per copy of the containing object.
fn intern_value(value: &Value) -> Value {
    let mut counts: HashMap<String, usize> = HashMap::new();
    count_subtrees(value, &mut counts);

    let mut dict_index: HashMap<String, usize> = HashMap::new();
    let mut dict_values: Vec<Value> = Vec::new();
    let rewritten = rewrite_interned(value, &counts, &mut dict_index, &mut dict_values);

    if dict_values.is_empty() {
        return value.clone();
    }

    let candidate = json!({ INTERN_MARKER: true, "dict": dict_values, "value": rewritten });

    // Only commit to this if it actually shrank the output. Same "pay for
    // itself" discipline as the text compressor's line-dedup: a transform
    // that repeats often but stays under MIN_INTERN_LEN, or has enough
    // dictionary/wrapper overhead to erase its own savings, must not win.
    let candidate_len = serde_json::to_string(&candidate).map(|s| s.len());
    let original_len = serde_json::to_string(value).map(|s| s.len());
    match (candidate_len, original_len) {
        (Ok(c), Ok(o)) if c < o => candidate,
        _ => value.clone(),
    }
}

fn count_subtrees(value: &Value, counts: &mut HashMap<String, usize>) {
    match value {
        Value::Array(items) => {
            for item in items {
                count_subtrees(item, counts);
            }
        }
        Value::Object(map) => {
            for v in map.values() {
                count_subtrees(v, counts);
            }
        }
        _ => {}
    }
    // Map serializes via BTreeMap (no "preserve_order" feature), so this is
    // a canonical, order-independent key for structural equality.
    let key = value.to_string();
    if key.len() >= MIN_INTERN_LEN {
        *counts.entry(key).or_insert(0) += 1;
    }
}

fn rewrite_interned(
    value: &Value,
    counts: &HashMap<String, usize>,
    dict_index: &mut HashMap<String, usize>,
    dict_values: &mut Vec<Value>,
) -> Value {
    let rewritten_children = match value {
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| rewrite_interned(v, counts, dict_index, dict_values))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        rewrite_interned(v, counts, dict_index, dict_values),
                    )
                })
                .collect(),
        ),
        other => other.clone(),
    };

    let key = value.to_string(); // original, pre-rewrite form — must match count_subtrees' key
    if key.len() < MIN_INTERN_LEN || counts.get(&key).copied().unwrap_or(0) < 2 {
        return rewritten_children;
    }

    let index = match dict_index.get(&key) {
        Some(&i) => i,
        None => {
            let i = dict_values.len();
            dict_values.push(rewritten_children);
            dict_index.insert(key, i);
            i
        }
    };
    json!({ REF_MARKER: index })
}

fn unintern_value(value: &Value) -> Value {
    let Some(map) = value.as_object() else {
        return value.clone();
    };
    if map.get(INTERN_MARKER) != Some(&Value::Bool(true)) {
        return value.clone();
    }
    let dict = map
        .get("dict")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    // dict[i] can only reference dict[j] for j < i (refs are assigned in
    // the post-order traversal that built them — a subtree's index is
    // always assigned strictly after all of its descendants'), so
    // resolving strictly in index order guarantees every reference a dict
    // entry contains is already resolved by the time we get to it.
    let mut resolved: Vec<Value> = Vec::with_capacity(dict.len());
    for entry in &dict {
        resolved.push(substitute_refs(entry, &resolved));
    }

    let wrapped = map.get("value").cloned().unwrap_or(Value::Null);
    substitute_refs(&wrapped, &resolved)
}

fn substitute_refs(value: &Value, resolved: &[Value]) -> Value {
    if let Value::Object(map) = value {
        if map.len() == 1 {
            if let Some(idx) = map.get(REF_MARKER).and_then(Value::as_u64) {
                return resolved[idx as usize].clone();
            }
        }
    }
    match value {
        Value::Array(items) => {
            Value::Array(items.iter().map(|v| substitute_refs(v, resolved)).collect())
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), substitute_refs(v, resolved)))
                .collect(),
        ),
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
        assert!(
            compressed.len() < input.len(),
            "table form should be smaller"
        );
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
    fn repeated_nested_object_gets_interned_and_shrinks_dramatically() {
        // The actual shape that motivated this: a GitHub-style API response
        // where the same ~large author object is attached to every row.
        let author = r#"{"login":"jlewis","id":123456,"node_id":"MDQ6VXNlcjE","avatar_url":"https://avatars.githubusercontent.com/u/123456","gravatar_id":"","url":"https://api.github.com/users/jlewis","html_url":"https://github.com/jlewis","type":"User","site_admin":false}"#;
        let input = format!(
            r#"[
                {{"sha":"aaa","author":{author},"committer":{author}}},
                {{"sha":"bbb","author":{author},"committer":{author}}},
                {{"sha":"ccc","author":{author},"committer":{author}}}
            ]"#
        );
        let compressed = compress_json(input.as_bytes()).unwrap();
        assert!(
            compressed.len() < input.len() / 2,
            "6 copies of a ~230-byte object should collapse well below half the input size: {} vs {}",
            compressed.len(),
            input.len()
        );
        // Prove it's actually interned, not coincidentally small: the
        // login value should appear far fewer times than the 6 raw copies.
        let compressed_text = String::from_utf8(compressed.clone()).unwrap();
        assert!(compressed_text.matches("jlewis").count() < 6);
        assert_round_trips(&input);
    }

    #[test]
    fn short_repeated_values_are_not_interned() {
        // Below MIN_INTERN_LEN and/or not worth the ref overhead - must be
        // a no-op, not a transform that grows the output.
        let input = r#"[{"ok":true},{"ok":true},{"ok":true}]"#;
        assert_round_trips(input);
    }

    #[test]
    fn input_containing_a_literal_table_marker_round_trips_unchanged() {
        // Confirmed real bug before escape_reserved_keys existed: this
        // exact input silently decompressed to {"metadata":[{"a":1}]}
        // instead of itself, because decompress_value couldn't tell "a
        // marker we generated" from "genuine data shaped like one."
        let input = r#"{"metadata": {"__boomerang_table__": true, "keys": ["a"], "rows": [[1]]}}"#;
        assert_round_trips(input);
    }

    #[test]
    fn input_containing_a_literal_intern_and_ref_marker_round_trips_unchanged() {
        let input =
            r#"{"__boomerang_intern__": true, "dict": ["x"], "value": {"__boomerang_ref__": 0}}"#;
        assert_round_trips(input);
    }

    #[test]
    fn input_key_that_already_looks_escaped_still_round_trips() {
        // Exercises the "escaping the escaper" case: a key that already
        // starts with ESCAPE_PREFIX (and therefore also with
        // RESERVED_NAMESPACE, since the prefix itself lives in that
        // namespace) must still come back out exactly as it went in.
        let input =
            r#"{"__boomerang_esc_something": "value", "__boomerang_esc___boomerang_table__": 1}"#;
        assert_round_trips(input);
    }

    #[test]
    fn reserved_looking_key_nested_inside_a_columnarized_table_round_trips() {
        // The escape pass must run before columnarize/intern see the tree,
        // not just at the top level - otherwise a reserved-looking key
        // nested inside array elements would still collide.
        let input = r#"[
            {"id": 1, "__boomerang_ref__": "not actually a ref"},
            {"id": 2, "__boomerang_ref__": "not actually a ref"}
        ]"#;
        assert_round_trips(input);
    }

    #[test]
    fn nested_repeat_inside_an_interned_parent_still_round_trips() {
        // A repeated substring/object appears both as its own top-level
        // repeat AND nested inside a larger repeated object - dict entries
        // referencing other dict entries.
        let inner = "\"a very specific repeated string that is definitely long enough to intern on its own merits\"";
        let input = format!(
            r#"{{
                "standalone_a": {inner},
                "standalone_b": {inner},
                "wrapped": [
                    {{"note": {inner}, "id": 1}},
                    {{"note": {inner}, "id": 2}}
                ]
            }}"#
        );
        assert_round_trips(&input);
    }

    fn temp_store() -> crate::Store {
        let dir = std::env::temp_dir().join(format!(
            "boomerang-json-store-test-{:?}-{}",
            std::time::SystemTime::now(),
            std::process::id()
        ));
        crate::Store::open(&dir).unwrap()
    }

    fn assert_round_trips_with_store(store: &crate::Store, input: &str) {
        let original: Value = serde_json::from_str(input).unwrap();
        let compressed = compress_json_with_store(input.as_bytes(), store).unwrap();
        let decompressed = decompress_json_with_store(&compressed, store).unwrap();
        let restored: Value = serde_json::from_slice(&decompressed).unwrap();
        assert_eq!(original, restored, "round-trip must be exact");
    }

    #[test]
    fn with_store_single_call_round_trips_same_as_without() {
        let store = temp_store();
        assert_round_trips_with_store(&store, r#"[{"id":1,"status":"ok"},{"id":2,"status":"ok"}]"#);
    }

    #[test]
    fn repeated_large_value_across_separate_calls_shrinks_dramatically() {
        // The actual capability: an agent polling the same status endpoint,
        // or re-fetching a metadata object that hasn't changed, across
        // SEPARATE compress calls (simulated here as two documents that
        // share one large object but are otherwise unrelated in shape).
        let store = temp_store();
        let shared_object = format!(
            r#"{{"account_id":"acct_9f2a1c","plan":"enterprise","region":"us-east-1","limits":{{"requests_per_min":10000,"seats":250}},"metadata":"{}"}}"#,
            "x".repeat(120)
        );

        let doc1 = format!(r#"{{"kind":"page1","owner":{shared_object}}}"#);
        let doc2 =
            format!(r#"{{"kind":"page2","different_shape":[1,2,3],"owner":{shared_object}}}"#);

        let compressed1 = compress_json_with_store(doc1.as_bytes(), &store).unwrap();
        let compressed2 = compress_json_with_store(doc2.as_bytes(), &store).unwrap();

        assert!(
            compressed2.len() < doc2.len() / 2,
            "second document's shared object should collapse well below half the size: {} vs {}",
            compressed2.len(),
            doc2.len()
        );
        // Prove it's actually cross-call interned, not coincidentally
        // small: the account id should not appear verbatim in call 2's view.
        assert!(!String::from_utf8_lossy(&compressed2).contains("acct_9f2a1c"));

        let restored1: Value =
            serde_json::from_slice(&decompress_json_with_store(&compressed1, &store).unwrap())
                .unwrap();
        let restored2: Value =
            serde_json::from_slice(&decompress_json_with_store(&compressed2, &store).unwrap())
                .unwrap();
        assert_eq!(restored1, serde_json::from_str::<Value>(&doc1).unwrap());
        assert_eq!(restored2, serde_json::from_str::<Value>(&doc2).unwrap());
    }

    #[test]
    fn cross_call_ref_nested_inside_another_cross_call_ref_round_trips() {
        // A value promoted on call 2 can itself CONTAIN an already-promoted
        // reference from call 1 - resolve_cross_call_refs must keep
        // resolving until no marker remains, not just one level.
        let store = temp_store();
        let inner = format!(r#"{{"payload":"{}"}}"#, "y".repeat(100));
        let middle = format!(
            r#"{{"wraps_inner":{inner},"tag":"middle-{}"}}"#,
            "z".repeat(80)
        );

        compress_json_with_store(inner.as_bytes(), &store).unwrap(); // call 1: promote inner
        compress_json_with_store(inner.as_bytes(), &store).unwrap(); // call 2: inner -> cref
        let outer_doc = format!(r#"{{"a":{middle},"b":{middle}}}"#); // middle repeats -> promoted too, containing inner's cref
        let compressed3 = compress_json_with_store(outer_doc.as_bytes(), &store).unwrap();
        // call 4: outer_doc again - "a" and "b" now both resolve through a
        // cref-inside-a-cref chain
        let compressed4 = compress_json_with_store(outer_doc.as_bytes(), &store).unwrap();

        let restored3: Value =
            serde_json::from_slice(&decompress_json_with_store(&compressed3, &store).unwrap())
                .unwrap();
        let restored4: Value =
            serde_json::from_slice(&decompress_json_with_store(&compressed4, &store).unwrap())
                .unwrap();
        let expected: Value = serde_json::from_str(&outer_doc).unwrap();
        assert_eq!(restored3, expected);
        assert_eq!(restored4, expected);
    }

    #[test]
    fn deeply_nested_repeated_document_converges_over_a_few_repeats() {
        // Locks in the convergence characteristic documented on
        // compress_json_with_store: a multi-level document doesn't
        // collapse to a single reference on its very next repeat (a
        // promoted child produces a NEW shape for its parent, so the
        // parent is "first sight" again at that point) - it converges over
        // a few identical repeats, monotonically shrinking, and every call
        // still round-trips exactly regardless of where in the convergence
        // it is.
        let store = temp_store();
        // Two meaningful nesting levels: outer object -> "items" array ->
        // each item large enough to matter.
        let items: Vec<String> = (0..30)
            .map(|i| {
                format!(
                    r#"{{"id":{i},"payload":"row payload padding {}"}}"#,
                    "p".repeat(40)
                )
            })
            .collect();
        let doc = format!(r#"{{"kind":"page","items":[{}]}}"#, items.join(","));
        let expected: Value = serde_json::from_str(&doc).unwrap();

        let mut sizes = Vec::new();
        for _ in 0..5 {
            let compressed = compress_json_with_store(doc.as_bytes(), &store).unwrap();
            let restored: Value =
                serde_json::from_slice(&decompress_json_with_store(&compressed, &store).unwrap())
                    .unwrap();
            assert_eq!(
                restored, expected,
                "must round-trip exactly at every stage of convergence"
            );
            sizes.push(compressed.len());
        }

        assert!(
            sizes.windows(2).all(|w| w[1] <= w[0]),
            "size must never increase across identical repeats: {sizes:?}"
        );
        assert!(
            sizes[4] < sizes[0] / 10,
            "must have converged to near-nothing by the 5th identical repeat: {sizes:?}"
        );
    }

    #[test]
    fn input_containing_a_literal_cref_marker_round_trips_unchanged() {
        // Same collision class already fixed for the other three markers -
        // escape_reserved_keys protects this one too automatically, since
        // CROSS_CALL_REF_MARKER lives in the same reserved namespace.
        let store = temp_store();
        let input = r#"{"metadata": {"__boomerang_cref__": "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"}}"#;
        assert_round_trips_with_store(&store, input);
    }

    mod proptests {
        use super::*;
        use proptest::prelude::*;

        /// Object keys, deliberately biased toward our reserved namespace
        /// (and toward duplicating a handful of short names, so generated
        /// objects actually have repeated key sets worth columnarizing)
        /// rather than uniformly random strings, which would rarely
        /// exercise the collision-prone paths this module cares most about.
        fn arb_key() -> impl Strategy<Value = String> {
            prop_oneof![
                3 => "[a-z]{1,4}",
                1 => "__boomerang_[a-z_]{0,10}",
                1 => "__boomerang_esc_[a-z_]{0,10}",
            ]
        }

        fn arb_leaf() -> impl Strategy<Value = Value> {
            prop_oneof![
                Just(Value::Null),
                any::<bool>().prop_map(Value::Bool),
                any::<i32>().prop_map(|n| json!(n)),
                "[a-zA-Z0-9 ]{0,12}".prop_map(Value::String),
            ]
        }

        fn arb_json() -> impl Strategy<Value = Value> {
            arb_leaf().prop_recursive(4, 64, 6, |inner| {
                prop_oneof![
                    prop::collection::vec(inner.clone(), 0..5).prop_map(Value::Array),
                    prop::collection::vec((arb_key(), inner), 0..5).prop_map(|pairs| {
                        // later entries win on duplicate keys, matching how
                        // a real JSON object would already have collapsed them
                        Value::Object(pairs.into_iter().collect())
                    }),
                ]
            })
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(512))]

            /// The one invariant, checked against generated input instead of
            /// hand-picked cases - this is what actually caught the reserved-key
            /// collision bug's sibling cases before they could ship.
            #[test]
            fn arbitrary_json_round_trips(value in arb_json()) {
                let input = serde_json::to_vec(&value).unwrap();
                let compressed = compress_json(&input).unwrap();
                let decompressed = decompress_json(&compressed).unwrap();
                let restored: Value = serde_json::from_slice(&decompressed).unwrap();
                prop_assert_eq!(value, restored);
            }

            /// Same invariant, but a whole SEQUENCE of generated documents
            /// through one evolving store - the shape that actually
            /// exercises cross_call_intern/resolve_cross_call_refs (a
            /// single document never does, since promotion only fires on
            /// a repeat).
            #[test]
            fn arbitrary_json_sequence_with_store_round_trips(values in prop::collection::vec(arb_json(), 1..8)) {
                let store = temp_store();
                for value in values {
                    let input = serde_json::to_vec(&value).unwrap();
                    let compressed = compress_json_with_store(&input, &store).unwrap();
                    let decompressed = decompress_json_with_store(&compressed, &store).unwrap();
                    let restored: Value = serde_json::from_slice(&decompressed).unwrap();
                    prop_assert_eq!(value, restored);
                }
            }
        }
    }
}
