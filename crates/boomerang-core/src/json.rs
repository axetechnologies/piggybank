use serde_json::{json, Value};
use std::collections::HashMap;

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
        }
    }
}
