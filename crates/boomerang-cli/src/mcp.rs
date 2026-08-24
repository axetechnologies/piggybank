//! A hand-rolled MCP (Model Context Protocol) server over stdio.
//!
//! No SDK, no async runtime — MCP over stdio is newline-delimited JSON-RPC
//! 2.0, so this is a blocking read-a-line/dispatch/write-a-line loop. That's
//! the whole transport. Implements just enough of the protocol to be a real
//! MCP server: `initialize`, `tools/list`, `tools/call`, and silently
//! ignoring notifications (messages with no `id`, which get no response).
//!
//! Four tools. Three deliberately named to mirror headroom's own MCP
//! surface (`headroom_compress` / `_retrieve` / `_stats`) for a direct,
//! same-shape comparison, plus a fourth headroom's own surface doesn't
//! expose — `boomerang_decompress`, full reconstruction rather than
//! per-reference retrieval, since with three genuinely different marker
//! schemes underneath, guessing which one produced a given view is a real
//! footgun; the caller already has `kind` from `compress`, so it just asks
//! for it back:
//!
//! - `boomerang_compress` — auto-detects JSON (lossless columnar) vs
//!   text/logs (dedup + elision); pass `key` for session-aware diffing
//!   against whatever was last compressed under that key.
//! - `boomerang_decompress` — full reconstruction of a compressed view,
//!   given the `kind` `compress` returned alongside it.
//! - `boomerang_retrieve` — fetch the exact original bytes behind a
//!   reference id embedded in a compressed view.
//! - `boomerang_stats` — entry count and total bytes held in the store.

use boomerang_core::{Session, Store, TextOptions};
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

struct ServerState {
    store: Store,
    session: Session,
}

pub fn serve(store_dir: &str) -> io::Result<()> {
    let state = ServerState {
        store: Store::open(store_dir)?,
        session: Session::open(store_dir)?,
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
            "name": "boomerang_compress",
            "description": "Compress content before it reaches an LLM. Auto-detects JSON (lossless columnar compression: repeated keys/values in arrays-of-objects amortized) vs text/logs (consecutive-line dedup + middle elision for large blocks, exact recovery via boomerang_retrieve). Pass `key` (e.g. a file path) to diff against whatever was last compressed under that same key in this session, instead of resending it in full.",
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
            "name": "boomerang_decompress",
            "description": "Reconstruct the exact original content from a compressed view returned by boomerang_compress. Requires `kind` (the value boomerang_compress returned alongside `compressed`) since the three compressors use distinct, non-overlapping marker schemes and guessing which one produced a given view is unreliable in the general case - pass back what compress told you.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "compressed": { "type": "string", "description": "The compressed view text, exactly as returned by boomerang_compress." },
                    "kind": { "type": "string", "description": "One of \"json\", \"text\", or \"session\" - the `kind` field from the matching boomerang_compress call." }
                },
                "required": ["compressed", "kind"]
            }
        },
        {
            "name": "boomerang_retrieve",
            "description": "Fetch the exact original bytes behind a reference id embedded in a compressed view (e.g. the id inside a BOOMERANG:ELIDE:... marker). Nothing boomerang_compress writes to its store is ever discarded, so this always succeeds for a ref it actually returned.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ref": { "type": "string", "description": "The content-store reference id (sha256 hex)." }
                },
                "required": ["ref"]
            }
        },
        {
            "name": "boomerang_stats",
            "description": "Report how many entries and how many bytes are currently held in the content store.",
            "inputSchema": { "type": "object", "properties": {} }
        },
    ])
}

fn handle_tools_call(state: &ServerState, id: Value, request: &Value) -> Value {
    let params = request.get("params").cloned().unwrap_or(json!({}));
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

    let result = match name {
        "boomerang_compress" => handle_compress(state, &arguments),
        "boomerang_decompress" => handle_decompress(state, &arguments),
        "boomerang_retrieve" => handle_retrieve(state, &arguments),
        "boomerang_stats" => handle_stats(state),
        other => Err(format!("unknown tool: {other}")),
    };

    // MCP tool-level failures are a normal result with isError:true, not a
    // JSON-RPC error — the JSON-RPC error object is reserved for protocol
    // problems (unknown method, malformed request), not tool business logic.
    match result {
        Ok(value) => ok(
            id,
            json!({ "content": [{ "type": "text", "text": value.to_string() }] }),
        ),
        Err(message) => ok(
            id,
            json!({ "content": [{ "type": "text", "text": message }], "isError": true }),
        ),
    }
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
        return Ok(json!({
            "compressed": String::from_utf8_lossy(&compressed),
            "kind": "json",
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

    Ok(json!({
        "compressed": String::from_utf8_lossy(&compressed),
        "kind": kind,
    }))
}

fn handle_decompress(state: &ServerState, args: &Value) -> Result<Value, String> {
    let compressed = args
        .get("compressed")
        .and_then(Value::as_str)
        .ok_or("missing 'compressed' argument")?;
    let kind = args
        .get("kind")
        .and_then(Value::as_str)
        .ok_or("missing 'kind' argument (one of: json, text, session)")?;

    let restored = match kind {
        // Must be the store-backed variant, matching handle_compress: a
        // "json" view from this server may contain a cross-call reference
        // (__boomerang_cref__) that plain decompress_json doesn't know how
        // to resolve - it would pass it through unchanged as if it were
        // ordinary data instead of erroring, which is exactly the kind of
        // silent-wrong-output failure this project treats as a real bug.
        "json" => boomerang_core::decompress_json_with_store(compressed.as_bytes(), &state.store)
            .map_err(|e| e.to_string())?,
        "text" => boomerang_core::decompress_text(&state.store, compressed.as_bytes())
            .map_err(|e| e.to_string())?,
        "session" => state
            .session
            .decompress(compressed.as_bytes())
            .map_err(|e| e.to_string())?,
        other => {
            return Err(format!(
                "unknown kind: {other} (expected json, text, or session)"
            ))
        }
    };

    Ok(json!({ "content": String::from_utf8_lossy(&restored) }))
}

fn handle_retrieve(state: &ServerState, args: &Value) -> Result<Value, String> {
    let reference = args
        .get("ref")
        .and_then(Value::as_str)
        .ok_or("missing 'ref' argument")?;
    let bytes = state.store.get(reference).map_err(|e| e.to_string())?;
    Ok(json!({ "content": String::from_utf8_lossy(&bytes) }))
}

fn handle_stats(state: &ServerState) -> Result<Value, String> {
    let stats = state.store.stats().map_err(|e| e.to_string())?;
    Ok(json!({ "store_entries": stats.entries, "store_bytes": stats.bytes }))
}
