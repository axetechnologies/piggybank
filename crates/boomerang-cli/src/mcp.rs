//! A hand-rolled MCP (Model Context Protocol) server over stdio.
//!
//! No SDK, no async runtime — MCP over stdio is newline-delimited JSON-RPC
//! 2.0, so this is a blocking read-a-line/dispatch/write-a-line loop. That's
//! the whole transport. Implements just enough of the protocol to be a real
//! MCP server: `initialize`, `tools/list`, `tools/call`, and silently
//! ignoring notifications (messages with no `id`, which get no response).
//!
//! Eight tools, named as clean verbs — the MCP server name already
//! provides the namespace (`mcp__boomerang__compress`, etc.):
//!
//! - `compress` — auto-detects JSON (lossless columnar) vs
//!   text/logs (dedup + elision); pass `key` for session-aware diffing
//!   against whatever was last compressed under that key.
//! - `decompress` — full reconstruction of a compressed view,
//!   given the `kind` `compress` returned alongside it.
//! - `verify` — confirm a compressed view's references still
//!   resolve, without doing the full reconstruction `decompress` does.
//! - `retrieve` — fetch the exact original bytes behind a
//!   reference id embedded in a compressed view, plus when that content
//!   first entered the store (by any caller sharing it, not just this one).
//! - `changed` — fingerprint check: compare a sha256 hash against
//!   the last compressed content under a key, without resending it.
//! - `compress_append` — streaming append: send only new bytes,
//!   Boomerang concatenates with prior content under the key and returns
//!   only the delta. Designed for tailing logs or polling builds.
//! - `stats` — entry count and total bytes held in the store.

use boomerang_core::{Session, Store, TextOptions};
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

const VIEW_VERSION: u8 = 1;

fn encode_view(kind: &str, compressed: &[u8]) -> String {
    format!("BOOM:{}:{}\n{}", VIEW_VERSION, kind, String::from_utf8_lossy(compressed))
}

fn decode_view(view: &str) -> Result<(&str, &str), String> {
    let newline = view.find('\n').ok_or("invalid view: missing header")?;
    let header = &view[..newline];
    let body = &view[newline + 1..];
    let parts: Vec<&str> = header.splitn(3, ':').collect();
    if parts.len() != 3 || parts[0] != "BOOM" {
        return Err("invalid view: expected BOOM:<version>:<kind> header".into());
    }
    let version: u8 = parts[1].parse().map_err(|_| "invalid view: bad version")?;
    if version != VIEW_VERSION {
        return Err(format!("unsupported view version: {version} (expected {VIEW_VERSION})"));
    }
    Ok((parts[2], body))
}

struct ServerState {
    store: Store,
    session: Session,
    total_original: AtomicU64,
    total_compressed: AtomicU64,
    compress_calls: AtomicU64,
}

pub fn serve(store_dir: &str) -> io::Result<()> {
    let state = ServerState {
        store: Store::open(store_dir)?,
        session: Session::open(store_dir)?,
        total_original: AtomicU64::new(0),
        total_compressed: AtomicU64::new(0),
        compress_calls: AtomicU64::new(0),
    };

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            continue; // malformed line on a line-oriented transport: drop it, don't crash the server
        };

        // A message with no "id" is a notification (e.g.
        // notifications/initialized) — per JSON-RPC, it gets no response.
        let Some(id) = request.get("id").cloned() else {
            continue;
        };
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");

        let response = match method {
            "initialize" => ok(id, initialize_result()),
            "tools/list" => ok(id, json!({ "tools": tool_defs() })),
            // Caught, not just called: this is a long-running process
            // serving a whole session's worth of requests. An unexpected
            // panic in one request's handling (a bug we haven't found yet,
            // an edge case in the compressors) must not take the entire
            // server down and drop every other in-flight conversation with
            // it - it should fail that one request and keep serving. Sound
            // here specifically because catching a panic mid-`RefCell`
            // borrow is safe: unwinding still runs the guard's destructor,
            // so the borrow flag is left consistent either way.
            "tools/call" => {
                let id_for_panic = id.clone();
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    handle_tools_call(&state, id, &request)
                }))
                .unwrap_or_else(|_| {
                    err(
                        id_for_panic,
                        -32603,
                        "internal error: request handler panicked",
                    )
                })
            }
            other => err(id, -32601, &format!("method not found: {other}")),
        };

        writeln!(stdout, "{}", serde_json::to_string(&response)?)?;
        stdout.flush()?;
    }
    Ok(())
}

fn ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn err(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": "2024-11-05",
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "boomerang", "version": env!("CARGO_PKG_VERSION") },
    })
}

fn tool_defs() -> Value {
    json!([
        {
            "name": "compress",
            "description": "Compress content before it reaches an LLM. Auto-detects JSON (lossless columnar compression: repeated keys/values in arrays-of-objects amortized) vs text/logs (consecutive-line dedup + middle elision for large blocks, exact recovery via retrieve). Pass `key` (e.g. a file path) to diff against whatever was last compressed under that same key in this session, instead of resending it in full.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "content": { "type": "string", "description": "The raw content to compress." },
                    "key": { "type": "string", "description": "Optional stable identifier (e.g. a file path) for session-aware diffing." }
                },
                "required": ["content"]
            }
        },
        {
            "name": "decompress",
            "description": "Reconstruct the exact original content from an opaque view returned by compress. The view encodes everything needed for reconstruction - just pass it back.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "view": { "type": "string", "description": "The opaque view string returned by compress." }
                },
                "required": ["view"]
            }
        },
        {
            "name": "verify",
            "description": "Confirm a compressed view's references still resolve in the store, without full reconstruction. Cheaper than decompress when you only need to know reconstruction is still possible. Returns ok, checked_refs, and missing_refs.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "view": { "type": "string", "description": "The opaque view string returned by compress." }
                },
                "required": ["view"]
            }
        },
        {
            "name": "retrieve",
            "description": "Fetch the exact original bytes behind a reference id embedded in a compressed view (e.g. the id inside a BOOMERANG:ELIDE:... marker). Nothing compress writes to its store is ever discarded, so this always succeeds for a ref it actually returned. Response includes first_seen_unix - when this exact content first entered the store, by any caller, not just this one (null if written before provenance tracking existed).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ref": { "type": "string", "description": "The content-store reference id (sha256 hex)." }
                },
                "required": ["ref"]
            }
        },
        {
            "name": "changed",
            "description": "Check whether content under a session key has changed without sending the content itself. Pass the sha256 hex hash of your current content (64 lowercase hex characters); Boomerang compares it against the hash stored from the last compress call under that key. Returns {changed, known}: known is false if the key has never been compressed; changed is true only when known and the hashes differ. Use before a full compress call to avoid resending unchanged content — a 64-byte check instead of a multi-KB upload.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "key": { "type": "string", "description": "The key previously used with compress." },
                    "hash": { "type": "string", "description": "The sha256 hex hash of your current content." }
                },
                "required": ["key", "hash"]
            }
        },
        {
            "name": "compress_budget",
            "description": "Budget-constrained compression: 'I have N bytes of context budget — give me the highest information density you can fit.' Compresses content with a hard byte ceiling. If normal compression already fits, returns that. Otherwise progressively elides more of the middle (stored for recovery via retrieve) until the view fits. Elided content is never lost — retrieve any reference id to get the exact original bytes back. Use when context window space is scarce and you need the most important parts of large output.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "content": { "type": "string", "description": "The raw content to compress." },
                    "max_bytes": { "type": "integer", "description": "Maximum byte size for the compressed view.", "minimum": 1 }
                },
                "required": ["content", "max_bytes"]
            }
        },
        {
            "name": "compress_append",
            "description": "Append-only streaming compression: send only *new* bytes (e.g. new log lines since last poll) and Boomerang concatenates them with prior content under the same key. The full accumulated content is stored (so compress/decompress against this key see everything), but the returned view contains only the new bytes — the caller already has prior content in context. First call under a key behaves like compress with no prior state. Designed for tailing logs, polling builds, or any streaming output where resending the full history each call wastes context budget.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "key": { "type": "string", "description": "Stable identifier for the stream being tailed (e.g. 'build-log', a file path)." },
                    "content": { "type": "string", "description": "Only the new bytes since the last append call." }
                },
                "required": ["key", "content"]
            }
        },
        {
            "name": "stats",
            "description": "Report store size and lifetime compression savings: total calls, bytes in, bytes out, bytes saved, and savings percentage since activation.",
            "inputSchema": { "type": "object", "properties": {} }
        },
    ])
}

fn handle_tools_call(state: &ServerState, id: Value, request: &Value) -> Value {
    let params = request.get("params").cloned().unwrap_or(json!({}));
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

    let result = match name {
        "compress" => handle_compress(state, &arguments),
        "decompress" => handle_decompress(state, &arguments),
        "verify" => handle_verify(state, &arguments),
        "retrieve" => handle_retrieve(state, &arguments),
        "compress_budget" => handle_compress_budget(state, &arguments),
        "changed" => handle_changed(state, &arguments),
        "compress_append" => handle_compress_append(state, &arguments),
        "stats" => handle_stats(state),
        other => Err(format!("unknown tool: {other}")),
    };

    // MCP tool-level failures are a normal result with isError:true, not a
    // JSON-RPC error — the JSON-RPC error object is reserved for protocol
    // problems (unknown method, malformed request), not tool business logic.
    match result {
        Ok(value) => {
            // Plain-string results (e.g. decompress reconstructing raw text) are
            // returned as-is; structured results are JSON-serialized.
            let text = match value {
                Value::String(s) => s,
                other => other.to_string(),
            };
            ok(
                id,
                json!({ "content": [{ "type": "text", "text": text }] }),
            )
        }
        Err(message) => ok(
            id,
            json!({ "content": [{ "type": "text", "text": message }], "isError": true }),
        ),
    }
}

fn record_and_savings(state: &ServerState, original: usize, compressed: usize) -> Value {
    state.total_original.fetch_add(original as u64, Relaxed);
    state.total_compressed.fetch_add(compressed as u64, Relaxed);
    let calls = state.compress_calls.fetch_add(1, Relaxed) + 1;
    let tot_orig = state.total_original.load(Relaxed);
    let tot_comp = state.total_compressed.load(Relaxed);
    let saved = tot_orig.saturating_sub(tot_comp);
    let pct = if tot_orig > 0 { (saved as f64 / tot_orig as f64) * 100.0 } else { 0.0 };
    json!({
        "lifetime_calls": calls,
        "lifetime_original_bytes": tot_orig,
        "lifetime_compressed_bytes": tot_comp,
        "lifetime_saved_bytes": saved,
        "lifetime_saved_pct": format!("{pct:.1}%"),
    })
}

fn handle_compress(state: &ServerState, args: &Value) -> Result<Value, String> {
    let content = args
        .get("content")
        .and_then(Value::as_str)
        .ok_or("missing 'content' argument")?;
    let key = args.get("key").and_then(Value::as_str);

    // The store-backed variant: JSON gets the same cross-call memory text
    // already has via Session, but content-addressed rather than
    // key-addressed - a repeated large value (a status endpoint's payload,
    // a metadata object re-fetched unchanged) collapses to near-nothing the
    // moment it's seen again, in ANY later call, not just under one key.
    if let Ok(compressed) =
        boomerang_core::compress_json_with_store(content.as_bytes(), &state.store)
    {
        let savings = record_and_savings(state, content.len(), compressed.len());
        return Ok(json!({
            "view": encode_view("json", &compressed),
            "original_bytes": content.len(),
            "compressed_bytes": compressed.len(),
            "savings": savings,
        }));
    }

    let (compressed, kind) = match key {
        Some(k) => (
            state
                .session
                .compress(k, content.as_bytes())
                .map_err(|e| e.to_string())?,
            "session",
        ),
        None => (
            boomerang_core::compress_text(
                &state.store,
                content.as_bytes(),
                &TextOptions::default(),
            )
            .map_err(|e| e.to_string())?,
            "text",
        ),
    };

    let savings = record_and_savings(state, content.len(), compressed.len());
    Ok(json!({
        "view": encode_view(kind, &compressed),
        "original_bytes": content.len(),
        "compressed_bytes": compressed.len(),
        "savings": savings,
    }))
}

fn handle_decompress(state: &ServerState, args: &Value) -> Result<Value, String> {
    let view = args
        .get("view")
        .and_then(Value::as_str)
        .ok_or("missing 'view' argument")?;
    let (kind, body) = decode_view(view)?;

    let restored = dispatch_decompress(state, kind, body)?;
    Ok(Value::String(String::from_utf8_lossy(&restored).into_owned()))
}

fn handle_verify(state: &ServerState, args: &Value) -> Result<Value, String> {
    let view = args
        .get("view")
        .and_then(Value::as_str)
        .ok_or("missing 'view' argument")?;
    let (kind, body) = decode_view(view)?;

    let result = match kind {
        "json" => boomerang_core::verify_json_with_store(body.as_bytes(), &state.store)
            .map_err(|e| e.to_string())?,
        "text" => boomerang_core::verify_text_with_store(&state.store, body.as_bytes())
            .map_err(|e| e.to_string())?,
        "session" => state
            .session
            .verify(body.as_bytes())
            .map_err(|e| e.to_string())?,
        other => {
            return Err(format!(
                "unknown kind: {other} (expected json, text, or session)"
            ))
        }
    };

    Ok(json!({
        "ok": result.ok,
        "checked_refs": result.checked_refs,
        "missing_refs": result.missing_refs,
    }))
}

fn dispatch_decompress(state: &ServerState, kind: &str, body: &str) -> Result<Vec<u8>, String> {
    match kind {
        "json" => boomerang_core::decompress_json_with_store(body.as_bytes(), &state.store)
            .map_err(|e| e.to_string()),
        "text" => boomerang_core::decompress_text(&state.store, body.as_bytes())
            .map_err(|e| e.to_string()),
        "session" => state
            .session
            .decompress(body.as_bytes())
            .map_err(|e| e.to_string()),
        other => Err(format!(
            "unknown kind: {other} (expected json, text, or session)"
        )),
    }
}

fn handle_retrieve(state: &ServerState, args: &Value) -> Result<Value, String> {
    let reference = args
        .get("ref")
        .and_then(Value::as_str)
        .ok_or("missing 'ref' argument")?;
    let bytes = state.store.get(reference).map_err(|e| e.to_string())?;
    // Best-effort provenance: when this content first entered the store,
    // Unix seconds, however it got there (this caller or another one
    // entirely sharing the same store - see the cross-call memory tests).
    // Missing for content written before provenance tracking existed;
    // never fails the retrieve itself.
    let first_seen_unix = state.store.first_seen(reference).ok().flatten();
    Ok(json!({ "content": String::from_utf8_lossy(&bytes), "first_seen_unix": first_seen_unix }))
}

fn handle_compress_budget(state: &ServerState, args: &Value) -> Result<Value, String> {
    let content = args
        .get("content")
        .and_then(Value::as_str)
        .ok_or("missing 'content' argument")?;
    let max_bytes = args
        .get("max_bytes")
        .and_then(Value::as_u64)
        .ok_or("missing or invalid 'max_bytes' argument")? as usize;
    if max_bytes == 0 {
        return Err("max_bytes must be >= 1".into());
    }
    let compressed =
        boomerang_core::compress_text_budget(&state.store, content.as_bytes(), max_bytes)
            .map_err(|e| e.to_string())?;
    let within_budget = compressed.len() <= max_bytes;
    let savings = record_and_savings(state, content.len(), compressed.len());
    Ok(json!({
        "view": encode_view("text", &compressed),
        "original_bytes": content.len(),
        "compressed_bytes": compressed.len(),
        "within_budget": within_budget,
        "savings": savings,
    }))
}

fn handle_compress_append(state: &ServerState, args: &Value) -> Result<Value, String> {
    let key = args
        .get("key")
        .and_then(Value::as_str)
        .ok_or("missing 'key' argument")?;
    let content = args
        .get("content")
        .and_then(Value::as_str)
        .ok_or("missing 'content' argument")?;
    let new_bytes = content.as_bytes();
    let view_bytes = state
        .session
        .append(key, new_bytes)
        .map_err(|e| e.to_string())?;
    let savings = record_and_savings(state, new_bytes.len(), view_bytes.len());
    Ok(json!({
        "view": encode_view("text", &view_bytes),
        "appended_bytes": new_bytes.len(),
        "view_bytes": view_bytes.len(),
        "savings": savings,
    }))
}

fn handle_changed(state: &ServerState, args: &Value) -> Result<Value, String> {
    let key = args.get("key").and_then(Value::as_str).ok_or("missing 'key' argument")?;
    let hash = args.get("hash").and_then(Value::as_str).ok_or("missing 'hash' argument")?;
    let (changed, known) = state.session.check_changed(key, hash).map_err(|e| e.to_string())?;
    Ok(json!({ "changed": changed, "known": known }))
}

fn handle_stats(state: &ServerState) -> Result<Value, String> {
    let stats = state.store.stats().map_err(|e| e.to_string())?;
    let tot_orig = state.total_original.load(Relaxed);
    let tot_comp = state.total_compressed.load(Relaxed);
    let saved = tot_orig.saturating_sub(tot_comp);
    let pct = if tot_orig > 0 { (saved as f64 / tot_orig as f64) * 100.0 } else { 0.0 };
    Ok(json!({
        "store_entries": stats.entries,
        "store_bytes": stats.bytes,
        "lifetime_calls": state.compress_calls.load(Relaxed),
        "lifetime_original_bytes": tot_orig,
        "lifetime_compressed_bytes": tot_comp,
        "lifetime_saved_bytes": saved,
        "lifetime_saved_pct": format!("{pct:.1}%"),
    }))
}
