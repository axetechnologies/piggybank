//! Transparent MCP proxy that wraps another MCP server and auto-compresses
//! large tool responses.
//!
//! Spawns a child MCP server as a subprocess, forwards JSON-RPC messages over
//! its stdin/stdout, and optionally compresses large text responses using
//! piggybank-core before returning them to the caller.
//!
//! Usage:
//!   piggybank proxy [--threshold <bytes>] [--store-dir <path>] -- <command> [args...]
//!
//! Environment variables:
//!   PIGGYBANK_PROXY_FULL_TOOLS=1  Pass tools/list through unmodified (no description trimming).
//!   PIGGYBANK_MIN_BYTES=N         Override the compression threshold (same as --threshold).
//!   PIGGYBANK_SKIP_TOOLS=a,b,c    Comma-separated tool names whose responses are never compressed.

use piggybank_core::harvest;
use piggybank_core::harvest::{HarvestEvent, Harvester};
use piggybank_core::{Session, Store, TextOptions};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const DEFAULT_THRESHOLD: usize = 4096;
const VIEW_VERSION: u8 = 1;
/// Maximum bytes kept for each tool description when trimming tools/list.
const TOOL_DESC_CAP: usize = 200;
/// Maximum bytes kept for the entire inputSchema block when trimming.
const SCHEMA_CAP: usize = 300;

fn encode_view(kind: &str, compressed: &[u8]) -> String {
    format!(
        "BOOM:{}:{}\n{}",
        VIEW_VERSION,
        kind,
        String::from_utf8_lossy(compressed)
    )
}

/// Per-tool compression statistics accumulated during the proxy session.
#[derive(Default, Clone)]
pub struct ToolStats {
    pub calls: u64,
    pub original_bytes: u64,
    pub compressed_bytes: u64,
    pub compressed_calls: u64,
}

impl ToolStats {
    fn savings_bytes(&self) -> u64 {
        self.original_bytes.saturating_sub(self.compressed_bytes)
    }

    fn ratio(&self) -> f64 {
        if self.original_bytes == 0 {
            1.0
        } else {
            self.compressed_bytes as f64 / self.original_bytes as f64
        }
    }
}

/// Whether the child process uses newline-delimited or Content-Length framing.
#[derive(Clone, Copy)]
enum Framing {
    Newline,
    ContentLength,
}

struct ProxyState {
    child_stdin: ChildStdin,
    child_stdout: BufReader<ChildStdout>,
    child_framing: Framing,
    child_tools: Vec<Value>,
    next_child_id: u64,
    store: Store,
    session: Session,
    threshold: usize,
    harvester: Harvester,
    skip_tools: HashSet<String>,
    per_tool_stats: HashMap<String, ToolStats>,
    full_tools: bool,
    server_name: String,
}

fn spawn_child(command: &str, args: &[String]) -> io::Result<Child> {
    Command::new(command)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
}

fn send_to_child(state: &mut ProxyState, msg: &Value) -> io::Result<()> {
    let body =
        serde_json::to_string(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    match state.child_framing {
        Framing::Newline => {
            writeln!(state.child_stdin, "{}", body)?;
            state.child_stdin.flush()
        }
        Framing::ContentLength => {
            let header = format!("Content-Length: {}\r\n\r\n", body.len());
            state.child_stdin.write_all(header.as_bytes())?;
            state.child_stdin.write_all(body.as_bytes())?;
            state.child_stdin.flush()
        }
    }
}

/// Read one JSON-RPC message from the child, supporting both newline-delimited
/// and Content-Length framing. Transparently auto-detects framing from the
/// first response. Handles messages of any size including 10 MB+.
fn read_from_child(state: &mut ProxyState) -> io::Result<Value> {
    match state.child_framing {
        Framing::ContentLength => read_content_length_frame(&mut state.child_stdout),
        Framing::Newline => {
            // Peek at first non-empty line.  If it looks like an HTTP-style
            // header switch to Content-Length mode for this and future reads.
            loop {
                let mut line = String::new();
                let n = state.child_stdout.read_line(&mut line)?;
                if n == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "child process closed stdout",
                    ));
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                // Detect Content-Length framing on the first substantive line.
                if trimmed.to_ascii_lowercase().starts_with("content-length:") {
                    state.child_framing = Framing::ContentLength;
                    // The line we just read is the first header.  We need to
                    // parse it together with the rest of the header block, then
                    // read the body.
                    return read_content_length_body(&mut state.child_stdout, trimmed);
                }
                return serde_json::from_str(trimmed)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e));
            }
        }
    }
}

/// Read a full Content-Length frame from `reader`.
/// Expects: one or more `Name: value\r\n` headers, then `\r\n`, then body.
fn read_content_length_frame<R: BufRead>(reader: &mut R) -> io::Result<Value> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut header = String::new();
        let n = reader.read_line(&mut header)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "child closed stdout while reading CL headers",
            ));
        }
        let trimmed = header.trim();
        if trimmed.is_empty() {
            break; // blank line separates headers from body
        }
        let lower = trimmed.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("content-length:") {
            let n: usize = rest.trim().parse().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid Content-Length value")
            })?;
            content_length = Some(n);
        }
        // Ignore other headers (Content-Type, etc.)
    }
    let len = content_length.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Content-Length header missing in framed message",
        )
    })?;
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Resume parsing a Content-Length frame when the first header line has already
/// been read (as `first_header`).  Reads remaining headers and the body.
fn read_content_length_body<R: BufRead>(reader: &mut R, first_header: &str) -> io::Result<Value> {
    let lower = first_header.to_ascii_lowercase();
    let mut content_length: Option<usize> =
        if let Some(rest) = lower.strip_prefix("content-length:") {
            rest.trim().parse().ok()
        } else {
            None
        };
    loop {
        let mut header = String::new();
        let n = reader.read_line(&mut header)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "child closed stdout while reading CL headers",
            ));
        }
        let trimmed = header.trim();
        if trimmed.is_empty() {
            break;
        }
        let lower = trimmed.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("content-length:") {
            if let Ok(n) = rest.trim().parse() {
                content_length = Some(n);
            }
        }
    }
    let len = content_length.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "Content-Length header missing")
    })?;
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// The eight piggybank tool names — used for collision detection and dispatch.
const PB_TOOL_NAMES: &[&str] = &[
    "compress",
    "decompress",
    "verify",
    "retrieve",
    "changed",
    "compress_budget",
    "compress_append",
    "stats",
];

fn piggybank_tool_defs() -> Vec<Value> {
    serde_json::from_value(json!([
        {
            "name": "compress",
            "description": "Compress content before it reaches an LLM. Auto-detects JSON vs text/logs.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "content": { "type": "string", "description": "The raw content to compress." },
                    "key": { "type": "string", "description": "Optional stable identifier for session-aware diffing." }
                },
                "required": ["content"]
            }
        },
        {
            "name": "decompress",
            "description": "Reconstruct the exact original content from an opaque view returned by compress.",
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
            "description": "Confirm a compressed view's references still resolve in the store.",
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
            "description": "Fetch the exact original bytes behind a reference id embedded in a compressed view.",
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
            "description": "Check whether content under a session key has changed without sending the content itself.",
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
            "description": "Budget-constrained compression with a hard byte ceiling.",
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
            "description": "Append-only streaming compression: send only new bytes.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "key": { "type": "string", "description": "Stable identifier for the stream being tailed." },
                    "content": { "type": "string", "description": "Only the new bytes since the last append call." }
                },
                "required": ["key", "content"]
            }
        },
        {
            "name": "stats",
            "description": "Report store size and lifetime compression savings.",
            "inputSchema": { "type": "object", "properties": {} }
        },
    ]))
    .expect("static tool defs are valid JSON")
}

/// Return the first sentence of `s` (up to and including the first `.` or `\n`).
fn first_sentence_of(s: &str) -> &str {
    for (i, ch) in s.char_indices() {
        if ch == '.' || ch == '\n' {
            return &s[..=i];
        }
    }
    s
}

/// Trim a single tool entry for LLM consumption:
/// - description → first sentence, capped at TOOL_DESC_CAP bytes
/// - inputSchema → keep "type" and property names with only "type", drop
///   verbose "description" fields inside properties; truncate if still large
fn trim_tool(tool: &Value) -> Value {
    let mut t = tool.clone();
    if let Some(desc) = t.get("description").and_then(Value::as_str) {
        let short = first_sentence_of(desc);
        let short = if short.len() > TOOL_DESC_CAP {
            &short[..TOOL_DESC_CAP]
        } else {
            short
        };
        t["description"] = json!(short);
    }
    if let Some(schema) = t.get("inputSchema").cloned() {
        let slim = slim_schema(&schema);
        t["inputSchema"] = slim;
    }
    t
}

/// Reduce an inputSchema object to just required fields and property names+types.
fn slim_schema(schema: &Value) -> Value {
    let mut out = json!({ "type": schema.get("type").cloned().unwrap_or(json!("object")) });
    if let Some(required) = schema.get("required") {
        out["required"] = required.clone();
    }
    if let Some(props) = schema.get("properties").and_then(Value::as_object) {
        let mut slim_props = serde_json::Map::new();
        for (k, v) in props {
            let prop_type = v.get("type").cloned().unwrap_or(json!("string"));
            slim_props.insert(k.clone(), json!({ "type": prop_type }));
        }
        out["properties"] = Value::Object(slim_props);
    }
    // If the slimmed schema is still large, truncate its JSON representation.
    let s = out.to_string();
    if s.len() > SCHEMA_CAP {
        // Return a minimal placeholder so the tool is still callable.
        json!({ "type": "object" })
    } else {
        out
    }
}

/// Trim a list of tools for LLM consumption (unless full-tools mode is active).
fn trim_tools_list(tools: &[Value]) -> Vec<Value> {
    tools.iter().map(trim_tool).collect()
}

/// Merge child tools with piggybank tools.
/// Child tools keep their names. If a child tool name collides with a
/// piggybank tool name, the piggybank tool gets prefixed with `pb_`.
fn merge_tools(child_tools: &[Value], full_tools: bool) -> Vec<Value> {
    let child_names: HashSet<&str> = child_tools
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .collect();

    let pb_defs = piggybank_tool_defs();
    let child_display: Vec<Value> = if full_tools {
        child_tools.to_vec()
    } else {
        trim_tools_list(child_tools)
    };
    let mut merged: Vec<Value> = child_display;

    for mut pb_tool in pb_defs {
        let name = pb_tool
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if child_names.contains(name.as_str()) {
            let prefixed = format!("pb_{}", name);
            if let Some(obj) = pb_tool.as_object_mut() {
                obj.insert("name".to_string(), json!(prefixed));
            }
        }
        merged.push(pb_tool);
    }

    merged
}

/// Resolve the actual piggybank tool name from the called name.
/// Returns Some(canonical_pb_name) if this is a piggybank tool call.
fn resolve_pb_tool<'a>(name: &'a str, child_tool_names: &HashSet<&str>) -> Option<&'a str> {
    // Direct match (no collision)
    if PB_TOOL_NAMES.contains(&name) && !child_tool_names.contains(name) {
        return Some(name);
    }
    // Prefixed match (collision case)
    if let Some(stripped) = name.strip_prefix("pb_") {
        if PB_TOOL_NAMES.contains(&stripped) {
            return Some(stripped);
        }
    }
    None
}

fn handle_pb_compress(state: &ProxyState, args: &Value) -> Result<Value, String> {
    let content = args
        .get("content")
        .and_then(Value::as_str)
        .ok_or("missing 'content' argument")?;
    let key = args.get("key").and_then(Value::as_str);

    if let Ok(compressed) =
        piggybank_core::compress_json_with_store(content.as_bytes(), &state.store)
    {
        if let Some(k) = key {
            state.session.record_content_hash(k, content.as_bytes());
        }
        return Ok(json!({
            "view": encode_view("json", &compressed),
            "original_bytes": content.len(),
            "compressed_bytes": compressed.len(),
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
            piggybank_core::compress_text(
                &state.store,
                content.as_bytes(),
                &TextOptions::default(),
            )
            .map_err(|e| e.to_string())?,
            "text",
        ),
    };

    Ok(json!({
        "view": encode_view(kind, &compressed),
        "original_bytes": content.len(),
        "compressed_bytes": compressed.len(),
    }))
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
        return Err(format!(
            "unsupported view version: {version} (expected {VIEW_VERSION})"
        ));
    }
    Ok((parts[2], body))
}

fn dispatch_decompress(state: &ProxyState, kind: &str, body: &str) -> Result<Vec<u8>, String> {
    match kind {
        "json" => piggybank_core::decompress_json_with_store(body.as_bytes(), &state.store)
            .map_err(|e| e.to_string()),
        "text" => piggybank_core::decompress_text(&state.store, body.as_bytes())
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

fn handle_pb_tool(state: &ProxyState, canonical_name: &str, args: &Value) -> Result<Value, String> {
    match canonical_name {
        "compress" => handle_pb_compress(state, args),
        "decompress" => {
            let view = args
                .get("view")
                .and_then(Value::as_str)
                .ok_or("missing 'view' argument")?;
            let (kind, body) = decode_view(view)?;
            let restored = dispatch_decompress(state, kind, body)?;
            Ok(Value::String(
                String::from_utf8_lossy(&restored).into_owned(),
            ))
        }
        "verify" => {
            let view = args
                .get("view")
                .and_then(Value::as_str)
                .ok_or("missing 'view' argument")?;
            let (kind, body) = decode_view(view)?;
            let result = match kind {
                "json" => piggybank_core::verify_json_with_store(body.as_bytes(), &state.store)
                    .map_err(|e| e.to_string())?,
                "text" => piggybank_core::verify_text_with_store(&state.store, body.as_bytes())
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
        "retrieve" => {
            let reference = args
                .get("ref")
                .and_then(Value::as_str)
                .ok_or("missing 'ref' argument")?;
            let bytes = state.store.get(reference).map_err(|e| e.to_string())?;
            let first_seen_unix = state.store.first_seen(reference).ok().flatten();
            Ok(json!({
                "content": String::from_utf8_lossy(&bytes),
                "first_seen_unix": first_seen_unix,
            }))
        }
        "changed" => {
            let key = args
                .get("key")
                .and_then(Value::as_str)
                .ok_or("missing 'key' argument")?;
            let hash = args
                .get("hash")
                .and_then(Value::as_str)
                .ok_or("missing 'hash' argument")?;
            let (changed, known) = state
                .session
                .check_changed(key, hash)
                .map_err(|e| e.to_string())?;
            Ok(json!({ "changed": changed, "known": known }))
        }
        "compress_budget" => {
            let content = args
                .get("content")
                .and_then(Value::as_str)
                .ok_or("missing 'content' argument")?;
            let max_bytes =
                args.get("max_bytes")
                    .and_then(Value::as_u64)
                    .ok_or("missing or invalid 'max_bytes' argument")? as usize;
            if max_bytes == 0 {
                return Err("max_bytes must be >= 1".into());
            }
            let compressed =
                piggybank_core::compress_text_budget(&state.store, content.as_bytes(), max_bytes)
                    .map_err(|e| e.to_string())?;
            let within_budget = compressed.len() <= max_bytes;
            Ok(json!({
                "view": encode_view("text", &compressed),
                "original_bytes": content.len(),
                "compressed_bytes": compressed.len(),
                "within_budget": within_budget,
            }))
        }
        "compress_append" => {
            let key = args
                .get("key")
                .and_then(Value::as_str)
                .ok_or("missing 'key' argument")?;
            let content = args
                .get("content")
                .and_then(Value::as_str)
                .ok_or("missing 'content' argument")?;
            let view_bytes = state
                .session
                .append(key, content.as_bytes())
                .map_err(|e| e.to_string())?;
            Ok(json!({
                "view": encode_view("text", &view_bytes),
                "appended_bytes": content.len(),
                "view_bytes": view_bytes.len(),
            }))
        }
        "stats" => {
            let stats = state.store.stats().map_err(|e| e.to_string())?;
            Ok(json!({
                "store_entries": stats.entries,
                "store_bytes": stats.bytes,
            }))
        }
        other => Err(format!("unknown piggybank tool: {other}")),
    }
}

fn pb_tool_response(id: Value, result: Result<Value, String>) -> Value {
    match result {
        Ok(value) => {
            let text = match value {
                Value::String(s) => s,
                other => other.to_string(),
            };
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "content": [{ "type": "text", "text": text }] }
            })
        }
        Err(message) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "content": [{ "type": "text", "text": message }], "isError": true }
        }),
    }
}

/// Build a JSON-RPC error frame for child-death situations.
fn child_error_frame(id: Value, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32000,
            "message": message
        }
    })
}

/// If the response's content[0].text exceeds the threshold and the tool is not
/// in the skip list, compress it and replace the text with a compressed view.
/// Updates per-tool stats in `state`.
fn maybe_compress_response(state: &mut ProxyState, tool_name: &str, response: &mut Value) {
    // Never compress error frames.
    if response.get("error").is_some() {
        return;
    }

    let threshold = state.threshold;

    // Skip-list check.
    if state.skip_tools.contains(tool_name) {
        return;
    }

    let text = response
        .get("result")
        .and_then(|r| r.get("content"))
        .and_then(|c| c.get(0))
        .and_then(|item| item.get("text"))
        .and_then(Value::as_str)
        .map(|s| s.to_string());

    // Record original bytes regardless of whether we compress.
    let original_bytes = text.as_ref().map(|t| t.len()).unwrap_or(0);
    let entry = state
        .per_tool_stats
        .entry(tool_name.to_string())
        .or_default();
    entry.calls += 1;
    entry.original_bytes += original_bytes as u64;

    let text = match text {
        Some(t) if t.len() > threshold => t,
        _ => {
            // Below threshold: track as-is.
            let entry = state.per_tool_stats.get_mut(tool_name).unwrap();
            entry.compressed_bytes += original_bytes as u64;
            return;
        }
    };

    // Try JSON compression first, then fall back to text.
    let view = if let Ok(compressed) =
        piggybank_core::compress_json_with_store(text.as_bytes(), &state.store)
    {
        encode_view("json", &compressed)
    } else {
        match piggybank_core::compress_text(&state.store, text.as_bytes(), &TextOptions::default())
        {
            Ok(compressed) => encode_view("text", &compressed),
            Err(_) => {
                // Compression failed; pass through unchanged.
                let entry = state.per_tool_stats.get_mut(tool_name).unwrap();
                entry.compressed_bytes += original_bytes as u64;
                return;
            }
        }
    };

    let compressed_bytes = view.len();

    // Only replace if compression actually saved bytes.
    if compressed_bytes >= original_bytes {
        let entry = state.per_tool_stats.get_mut(tool_name).unwrap();
        entry.compressed_bytes += original_bytes as u64;
        return;
    }

    if let Some(result) = response.get_mut("result") {
        if let Some(content) = result.get_mut("content") {
            if let Some(item) = content.get_mut(0) {
                if let Some(obj) = item.as_object_mut() {
                    obj.insert("text".to_string(), json!(view));
                }
            }
        }
        if let Some(obj) = result.as_object_mut() {
            obj.insert("_piggybank_compressed".to_string(), json!(true));
            obj.insert("_original_bytes".to_string(), json!(original_bytes));
        }
    }

    let entry = state.per_tool_stats.get_mut(tool_name).unwrap();
    entry.compressed_bytes += compressed_bytes as u64;
    entry.compressed_calls += 1;
}

fn next_child_id(state: &mut ProxyState) -> u64 {
    let id = state.next_child_id;
    state.next_child_id += 1;
    id
}

fn handle_message(state: &mut ProxyState, msg: &Value) -> io::Result<Option<Value>> {
    // Never alter notifications (no id field): forward and return None.
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");

    match method {
        "initialize" => {
            // Reply to caller with our own server info.
            let caller_id = match id {
                Some(ref i) => i.clone(),
                None => return Ok(None), // notification, no response
            };

            let our_response = json!({
                "jsonrpc": "2.0",
                "id": caller_id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "piggybank-proxy", "version": env!("CARGO_PKG_VERSION") },
                }
            });

            // Forward initialize to child, read its response, discard it
            // (we already sent our own above).
            let child_id = next_child_id(state);
            let child_init = json!({
                "jsonrpc": "2.0",
                "id": child_id,
                "method": "initialize",
                "params": msg.get("params").cloned().unwrap_or(json!({}))
            });
            send_to_child(state, &child_init)?;
            // Read child's initialize response (discard result, just drain it).
            let _child_resp = read_from_child(state)?;

            // Send initialized notification to child.
            let initialized_notif = json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            });
            send_to_child(state, &initialized_notif)?;

            Ok(Some(our_response))
        }

        "notifications/initialized" => {
            // Already handled above; if client sends this separately, drop it.
            Ok(None)
        }

        "tools/list" => {
            let caller_id = match id {
                Some(ref i) => i.clone(),
                None => return Ok(None),
            };

            // Ask child for its tools.
            let child_id = next_child_id(state);
            let child_req = json!({
                "jsonrpc": "2.0",
                "id": child_id,
                "method": "tools/list",
                "params": {}
            });
            send_to_child(state, &child_req)?;
            let child_resp = match read_from_child(state) {
                Ok(v) => v,
                Err(e) => {
                    return Ok(Some(child_error_frame(
                        caller_id,
                        &format!("child process died during tools/list: {e}"),
                    )));
                }
            };

            let child_tools: Vec<Value> = child_resp
                .get("result")
                .and_then(|r| r.get("tools"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();

            state.child_tools = child_tools.clone();
            let merged = merge_tools(&child_tools, state.full_tools);

            Ok(Some(json!({
                "jsonrpc": "2.0",
                "id": caller_id,
                "result": { "tools": merged }
            })))
        }

        "tools/call" => {
            let caller_id = match id {
                Some(ref i) => i.clone(),
                None => return Ok(None),
            };

            let params = msg.get("params").cloned().unwrap_or(json!({}));
            let tool_name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

            let child_tool_names: HashSet<&str> = state
                .child_tools
                .iter()
                .filter_map(|t| t.get("name").and_then(Value::as_str))
                .collect();

            if let Some(canonical) = resolve_pb_tool(&tool_name, &child_tool_names) {
                // Handle locally — never compress pb tool responses.
                let result = handle_pb_tool(state, canonical, &arguments);
                Ok(Some(pb_tool_response(caller_id, result)))
            } else {
                // Forward to child.
                let child_id = next_child_id(state);
                let child_req = json!({
                    "jsonrpc": "2.0",
                    "id": child_id,
                    "method": "tools/call",
                    "params": params
                });
                send_to_child(state, &child_req)?;
                let mut child_resp = match read_from_child(state) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!(
                            "piggybank-proxy: child died during tools/call({tool_name}): {e}"
                        );
                        return Ok(Some(child_error_frame(
                            caller_id,
                            &format!("child process died during tools/call: {e}"),
                        )));
                    }
                };

                // Never compress error results.
                let is_error = child_resp
                    .get("result")
                    .and_then(|r| r.get("isError"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    || child_resp.get("error").is_some();

                let pre_bytes = child_resp
                    .get("result")
                    .and_then(|r| r.get("content"))
                    .and_then(|c| c.get(0))
                    .and_then(|item| item.get("text"))
                    .and_then(Value::as_str)
                    .map(|s| s.len());

                if !is_error {
                    maybe_compress_response(state, &tool_name, &mut child_resp);
                } else {
                    // Track the call even when not compressing.
                    let entry = state.per_tool_stats.entry(tool_name.clone()).or_default();
                    entry.calls += 1;
                    let b = pre_bytes.unwrap_or(0) as u64;
                    entry.original_bytes += b;
                    entry.compressed_bytes += b;
                }

                let post_bytes = child_resp
                    .get("result")
                    .and_then(|r| r.get("content"))
                    .and_then(|c| c.get(0))
                    .and_then(|item| item.get("text"))
                    .and_then(Value::as_str)
                    .map(|s| s.len());
                let auto_compressed = child_resp
                    .get("result")
                    .and_then(|r| r.get("_piggybank_compressed"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let result_bytes = post_bytes.unwrap_or(0);
                let compression_ratio = if auto_compressed {
                    match (pre_bytes, post_bytes) {
                        (Some(orig), Some(comp)) if orig > 0 => Some(comp as f64 / orig as f64),
                        _ => None,
                    }
                } else {
                    None
                };
                state.harvester.log(HarvestEvent::ToolCall {
                    ts: harvest::now(),
                    session_id: state.harvester.session_id().to_string(),
                    server: state.server_name.clone(),
                    tool: tool_name.clone(),
                    result_bytes,
                    auto_compressed,
                    compression_ratio,
                });

                // Replace child's id with caller's id in the response.
                if let Some(obj) = child_resp.as_object_mut() {
                    obj.insert("id".to_string(), caller_id);
                }

                Ok(Some(child_resp))
            }
        }

        _ => {
            // Forward all other methods transparently to child.
            // Never alter JSON-RPC ids on forwarded requests.
            match id {
                None => {
                    // Notification: forward, no response.
                    send_to_child(state, msg)?;
                    Ok(None)
                }
                Some(caller_id) => {
                    let child_id = next_child_id(state);
                    let mut forwarded = msg.clone();
                    if let Some(obj) = forwarded.as_object_mut() {
                        obj.insert("id".to_string(), json!(child_id));
                    }
                    send_to_child(state, &forwarded)?;
                    let mut child_resp = match read_from_child(state) {
                        Ok(v) => v,
                        Err(e) => {
                            return Ok(Some(child_error_frame(
                                caller_id,
                                &format!("child process died: {e}"),
                            )));
                        }
                    };
                    if let Some(obj) = child_resp.as_object_mut() {
                        obj.insert("id".to_string(), caller_id);
                    }
                    Ok(Some(child_resp))
                }
            }
        }
    }
}

/// Configuration for `run_proxy`.
pub struct ProxyConfig {
    pub threshold: usize,
    pub store_dir: std::path::PathBuf,
    pub harvester: Harvester,
    pub skip_tools: HashSet<String>,
    pub full_tools: bool,
    pub stats_file: Option<String>,
}

/// Write per-tool stats as JSON to a file (or stderr if path is "-").
pub fn write_stats(stats: &HashMap<String, ToolStats>, path: &str) {
    let entries: Vec<Value> = {
        let mut pairs: Vec<(&String, &ToolStats)> = stats.iter().collect();
        pairs.sort_by(|a, b| b.1.savings_bytes().cmp(&a.1.savings_bytes()));
        pairs
            .into_iter()
            .map(|(name, s)| {
                json!({
                    "tool": name,
                    "calls": s.calls,
                    "compressed_calls": s.compressed_calls,
                    "original_bytes": s.original_bytes,
                    "compressed_bytes": s.compressed_bytes,
                    "saved_bytes": s.savings_bytes(),
                    "ratio": (s.ratio() * 1000.0).round() / 1000.0,
                })
            })
            .collect()
    };
    let total_orig: u64 = stats.values().map(|s| s.original_bytes).sum();
    let total_comp: u64 = stats.values().map(|s| s.compressed_bytes).sum();
    let total_calls: u64 = stats.values().map(|s| s.calls).sum();
    let out = json!({
        "summary": {
            "total_calls": total_calls,
            "total_original_bytes": total_orig,
            "total_compressed_bytes": total_comp,
            "total_saved_bytes": total_orig.saturating_sub(total_comp),
        },
        "per_tool": entries,
    });
    let text = serde_json::to_string_pretty(&out).unwrap_or_default();
    if path == "-" {
        eprintln!("{text}");
    } else if let Err(e) = std::fs::write(path, &text) {
        eprintln!("piggybank-proxy: failed to write stats to {path}: {e}");
    }
}

pub fn run_proxy(command: &str, args: &[String], cfg: ProxyConfig) -> io::Result<()> {
    let mut child = spawn_child(command, args).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("failed to spawn child process '{}': {}", command, e),
        )
    })?;

    let store = Store::open(&cfg.store_dir)?;
    let session = Session::open(&cfg.store_dir)?;

    // Use the command basename as the server name for harvest events.
    let server_name = Path::new(command)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(command)
        .to_string();

    let mut state = ProxyState {
        child_stdin: child.stdin.take().expect("child stdin must be piped"),
        child_stdout: BufReader::new(child.stdout.take().expect("child stdout must be piped")),
        child_framing: Framing::Newline,
        child_tools: vec![],
        next_child_id: 1000,
        store,
        session,
        threshold: cfg.threshold,
        harvester: cfg.harvester,
        skip_tools: cfg.skip_tools,
        per_tool_stats: HashMap::new(),
        full_tools: cfg.full_tools,
        server_name,
    };

    let stdin = io::stdin();
    let stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("piggybank-proxy: malformed JSON from caller: {e}");
                continue;
            }
        };

        let response = match handle_message(&mut state, &msg) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("piggybank-proxy: IO error handling message: {e}");
                break;
            }
        };

        if let Some(resp) = response {
            let serialized = serde_json::to_string(&resp)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            writeln!(stdout.lock(), "{}", serialized)?;
            stdout.lock().flush()?;
        }
    }

    // Write stats before exit.
    if let Some(ref path) = cfg.stats_file {
        write_stats(&state.per_tool_stats, path);
    }

    // Clean up: drop child stdin to signal EOF, then wait for child to exit.
    drop(state.child_stdin);
    let _ = child.wait();
    Ok(())
}

/// Parse proxy subcommand args and invoke run_proxy.
///
/// Expected format:
///   [--threshold <bytes>] [--store-dir <path>] [--harvest <path>] [--harvest-url <url>]
///   [--stats-file <path>] [--full-tools] -- <command> [args...]
pub fn run_proxy_from_args(all_args: &[String]) -> io::Result<()> {
    let mut threshold = std::env::var("PIGGYBANK_MIN_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_THRESHOLD);
    let mut store_dir: Option<String> = None;
    let mut harvest_path: Option<String> = None;
    let mut harvest_url: Option<String> = None;
    let mut stats_file: Option<String> = None;
    let mut full_tools = std::env::var("PIGGYBANK_PROXY_FULL_TOOLS").as_deref() == Ok("1");
    let skip_tools: HashSet<String> = std::env::var("PIGGYBANK_SKIP_TOOLS")
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| s.trim().to_string())
        .collect();

    // Find the `--` separator.
    let sep_pos = all_args.iter().position(|a| a == "--").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "missing '--' separator: usage: piggybank proxy [--threshold <N>] [--store-dir <path>] -- <command> [args...]",
        )
    })?;

    let proxy_args = &all_args[..sep_pos];
    let child_argv = &all_args[sep_pos + 1..];

    if child_argv.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no command specified after '--'",
        ));
    }

    let mut i = 0;
    while i < proxy_args.len() {
        match proxy_args[i].as_str() {
            "--threshold" => {
                i += 1;
                threshold = proxy_args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "--threshold requires a numeric value",
                        )
                    })?;
            }
            "--store-dir" => {
                i += 1;
                store_dir = Some(
                    proxy_args
                        .get(i)
                        .ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "--store-dir requires a path",
                            )
                        })?
                        .clone(),
                );
            }
            "--harvest" => {
                i += 1;
                harvest_path = Some(
                    proxy_args
                        .get(i)
                        .ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "--harvest requires a file path",
                            )
                        })?
                        .clone(),
                );
            }
            "--harvest-url" => {
                i += 1;
                harvest_url = Some(
                    proxy_args
                        .get(i)
                        .ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "--harvest-url requires a URL",
                            )
                        })?
                        .clone(),
                );
            }
            "--stats-file" => {
                i += 1;
                stats_file = Some(
                    proxy_args
                        .get(i)
                        .ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "--stats-file requires a path (use '-' for stderr)",
                            )
                        })?
                        .clone(),
                );
            }
            "--stats" => {
                stats_file = Some("-".to_string());
            }
            "--full-tools" => {
                full_tools = true;
            }
            other => {
                eprintln!("piggybank-proxy: unknown option '{other}', ignoring");
            }
        }
        i += 1;
    }

    let store_dir = store_dir
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|home| format!("{home}/.piggybank/store"))
        })
        .unwrap_or_else(|| ".piggybank-store".to_string());

    let harvester = if let Some(url) = harvest_url {
        Harvester::new_http(&url)
    } else if let Some(path) = harvest_path {
        Harvester::new_file(Path::new(&path))?
    } else {
        Harvester::new_null()
    };

    let command = &child_argv[0];
    let args = &child_argv[1..];

    run_proxy(
        command,
        args,
        ProxyConfig {
            threshold,
            store_dir: Path::new(&store_dir).to_path_buf(),
            harvester,
            skip_tools,
            full_tools,
            stats_file,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_tools_no_collision() {
        let child_tools = vec![
            json!({ "name": "list_files", "description": "list files" }),
            json!({ "name": "read_file", "description": "read a file" }),
        ];
        let merged = merge_tools(&child_tools, true);
        let names: Vec<&str> = merged
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .collect();
        // All 8 pb tools plus 2 child tools = 10
        assert_eq!(merged.len(), 10);
        // Child tools keep their names
        assert!(names.contains(&"list_files"));
        assert!(names.contains(&"read_file"));
        // PB tools keep their names (no collision)
        assert!(names.contains(&"compress"));
        assert!(names.contains(&"stats"));
        // No pb_ prefix in this case
        assert!(!names.iter().any(|n| n.starts_with("pb_")));
    }

    #[test]
    fn merge_tools_with_collision() {
        let child_tools = vec![json!({ "name": "compress", "description": "child compress" })];
        let merged = merge_tools(&child_tools, true);
        let names: Vec<&str> = merged
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .collect();
        // Child "compress" kept as-is
        assert!(names.contains(&"compress"));
        // PB compress renamed to pb_compress
        assert!(names.contains(&"pb_compress"));
        // Other PB tools unaffected
        assert!(names.contains(&"decompress"));
        assert!(names.contains(&"stats"));
    }

    #[test]
    fn resolve_pb_tool_no_collision() {
        let child_names = HashSet::new();
        assert_eq!(resolve_pb_tool("compress", &child_names), Some("compress"));
        assert_eq!(resolve_pb_tool("stats", &child_names), Some("stats"));
        assert_eq!(resolve_pb_tool("list_files", &child_names), None);
    }

    #[test]
    fn resolve_pb_tool_with_collision() {
        let mut child_names = HashSet::new();
        child_names.insert("compress");
        // Direct name is taken by child
        assert_eq!(resolve_pb_tool("compress", &child_names), None);
        // Prefixed name routes to pb tool
        assert_eq!(
            resolve_pb_tool("pb_compress", &child_names),
            Some("compress")
        );
        // Other pb tools unaffected
        assert_eq!(resolve_pb_tool("stats", &child_names), Some("stats"));
    }

    #[test]
    fn trim_tool_trims_description_at_first_sentence() {
        let tool = json!({
            "name": "example",
            "description": "First sentence. Second sentence with more detail.",
            "inputSchema": { "type": "object", "properties": {} }
        });
        let trimmed = trim_tool(&tool);
        let desc = trimmed.get("description").and_then(Value::as_str).unwrap();
        assert_eq!(desc, "First sentence.");
    }

    #[test]
    fn trim_tool_preserves_no_period() {
        let tool = json!({
            "name": "example",
            "description": "No period here",
            "inputSchema": { "type": "object", "properties": {} }
        });
        let trimmed = trim_tool(&tool);
        let desc = trimmed.get("description").and_then(Value::as_str).unwrap();
        assert_eq!(desc, "No period here");
    }

    #[test]
    fn slim_schema_drops_descriptions() {
        let schema = json!({
            "type": "object",
            "required": ["x"],
            "properties": {
                "x": { "type": "string", "description": "very verbose description" },
                "y": { "type": "integer", "description": "another verbose one" }
            }
        });
        let slim = slim_schema(&schema);
        let props = slim.get("properties").unwrap();
        // Property descriptions are stripped.
        assert!(props["x"].get("description").is_none());
        assert_eq!(props["x"]["type"].as_str(), Some("string"));
        assert_eq!(props["y"]["type"].as_str(), Some("integer"));
    }

    #[test]
    fn first_sentence_basic() {
        assert_eq!(first_sentence_of("Hello world. More text."), "Hello world.");
        assert_eq!(first_sentence_of("No period"), "No period");
        assert_eq!(first_sentence_of("Line one\nLine two"), "Line one\n");
    }

    #[test]
    fn run_proxy_from_args_missing_separator() {
        let args: Vec<String> = vec!["--threshold".into(), "2048".into()];
        let err = run_proxy_from_args(&args).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("'--'"));
    }

    #[test]
    fn run_proxy_from_args_missing_command() {
        let args: Vec<String> = vec!["--".into()];
        let err = run_proxy_from_args(&args).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("no command"));
    }

    #[test]
    fn run_proxy_from_args_bad_child_command_fails() {
        let args: Vec<String> = vec!["--".into(), "this-binary-does-not-exist-xyz123".into()];
        let err = run_proxy_from_args(&args).unwrap_err();
        // Should fail at spawn, not at arg parsing.
        assert_ne!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn write_stats_empty() {
        let stats: HashMap<String, ToolStats> = HashMap::new();
        // Should not panic.
        write_stats(&stats, "-");
    }

    #[test]
    fn tool_stats_ratio_empty() {
        let s = ToolStats::default();
        assert!((s.ratio() - 1.0).abs() < f64::EPSILON);
    }
}
