//! A hand-rolled MCP (Model Context Protocol) server over stdio.
//!
//! No SDK, no async runtime — MCP over stdio is newline-delimited JSON-RPC
//! 2.0, so this is a blocking read-a-line/dispatch/write-a-line loop. That's
//! the whole transport. Implements just enough of the protocol to be a real
//! MCP server: `initialize`, `tools/list`, `tools/call`, and silently
//! ignoring notifications (messages with no `id`, which get no response).
//!
//! Eight tools, named as clean verbs — the MCP server name already
//! provides the namespace (`mcp__piggybank__compress`, etc.):
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
//!   Piggybank concatenates with prior content under the key and returns
//!   only the delta. Designed for tailing logs or polling builds.
//! - `stats` — entry count and total bytes held in the store.

use piggybank_core::harvest;
use piggybank_core::harvest::{HarvestEvent, Harvester};
use piggybank_core::{Session, Store, TextOptions};
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

const BRAIN_URL: &str = "https://brain.axe.onl/api/save";
const REPORT_EVERY_N_CALLS: u64 = 10;

const VIEW_VERSION: u8 = 1;

fn encode_view(kind: &str, compressed: &[u8]) -> String {
    format!(
        "BOOM:{}:{}\n{}",
        VIEW_VERSION,
        kind,
        String::from_utf8_lossy(compressed)
    )
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

const BYTES_PER_TOKEN: f64 = 4.0;
const DEFAULT_RATE_PER_MTOK: f64 = 3.0;

struct ServerState {
    store: Store,
    session: Session,
    total_original: AtomicU64,
    total_compressed: AtomicU64,
    compress_calls: AtomicU64,
    skipped_resends: AtomicU64,
    append_bytes_avoided: AtomicU64,
    budget_enforcements: AtomicU64,
    analytics_path: std::path::PathBuf,
    hostname: String,
    last_reported_calls: AtomicU64,
    harvester: Harvester,
}

impl ServerState {
    fn persist_analytics(&self) {
        let data = serde_json::json!({
            "total_original": self.total_original.load(Relaxed),
            "total_compressed": self.total_compressed.load(Relaxed),
            "compress_calls": self.compress_calls.load(Relaxed),
            "skipped_resends": self.skipped_resends.load(Relaxed),
            "append_bytes_avoided": self.append_bytes_avoided.load(Relaxed),
            "budget_enforcements": self.budget_enforcements.load(Relaxed),
        });
        let tmp = self
            .analytics_path
            .with_extension(format!("json.{}.tmp", std::process::id()));
        if std::fs::write(&tmp, data.to_string().as_bytes()).is_ok() {
            let _ = std::fs::rename(&tmp, &self.analytics_path);
        }

        let calls = self.compress_calls.load(Relaxed);
        let last = self.last_reported_calls.load(Relaxed);
        if calls >= last + REPORT_EVERY_N_CALLS {
            self.last_reported_calls.store(calls, Relaxed);
            self.report_to_brain();
        }
    }

    fn report_to_brain(&self) {
        let brain_key = std::env::var("BRAIN_KEY")
            .or_else(|_| std::env::var("AXE_FLEET_KEY"))
            .unwrap_or_default();
        if brain_key.is_empty() {
            return;
        }

        let tot_orig = self.total_original.load(Relaxed);
        let tot_comp = self.total_compressed.load(Relaxed);
        let saved = tot_orig.saturating_sub(tot_comp);
        let append_avoided = self.append_bytes_avoided.load(Relaxed);
        let total_bytes_saved = saved + append_avoided;
        let tokens_saved = total_bytes_saved as f64 / BYTES_PER_TOKEN;
        let cost_saved = tokens_saved * DEFAULT_RATE_PER_MTOK / 1_000_000.0;
        let calls = self.compress_calls.load(Relaxed);
        let pct = if tot_orig > 0 {
            (saved as f64 / tot_orig as f64) * 100.0
        } else {
            0.0
        };

        let content = format!(
            "compress_calls: {calls} | original: {tot_orig}B | compressed: {tot_comp}B | \
             saved: {saved}B ({pct:.1}%) | append_avoided: {append_avoided}B | \
             tokens_saved: {tokens_saved:.0} | cost_saved: ${cost_saved:.4} | \
             skipped_resends: {} | budget_enforcements: {}",
            self.skipped_resends.load(Relaxed),
            self.budget_enforcements.load(Relaxed),
        );

        let payload = json!({
            "title": format!("piggybank stats — {}", self.hostname),
            "content": content,
            "tags": "piggybank,stats,fleet,telemetry",
            "agent": format!("piggybank@{}", self.hostname),
            "key": format!("piggybank:stats:{}", self.hostname),
            "value": json!({
                "hostname": self.hostname,
                "compress_calls": calls,
                "total_original_bytes": tot_orig,
                "total_compressed_bytes": tot_comp,
                "total_saved_bytes": saved,
                "saved_pct": format!("{pct:.1}"),
                "append_bytes_avoided": append_avoided,
                "tokens_saved": tokens_saved as u64,
                "cost_saved_usd": format!("{cost_saved:.6}"),
                "skipped_resends": self.skipped_resends.load(Relaxed),
                "budget_enforcements": self.budget_enforcements.load(Relaxed),
            }).to_string(),
        });

        let payload_str = payload.to_string();
        let brain_key_clone = brain_key.clone();
        std::thread::spawn(move || {
            let _ = std::process::Command::new("curl")
                .args([
                    "-sf",
                    "-X",
                    "POST",
                    BRAIN_URL,
                    "-H",
                    "Content-Type: application/json",
                    "-H",
                    &format!("X-AXE-Key: {brain_key_clone}"),
                    "-d",
                    &payload_str,
                    "--max-time",
                    "5",
                ])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        });
    }
}

fn gethostname() -> String {
    std::process::Command::new("hostname")
        .arg("-s")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn load_counter(v: &Value, key: &str) -> AtomicU64 {
    AtomicU64::new(v.get(key).and_then(|v| v.as_u64()).unwrap_or(0))
}

pub fn serve(
    store_dir: &str,
    gc_days: u64,
    harvest_path: Option<&str>,
    harvest_url: Option<&str>,
) -> io::Result<()> {
    let analytics_path = std::path::Path::new(store_dir).join(".piggybank-analytics.json");
    let saved: Value = std::fs::read(&analytics_path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null);
    let store = Store::open(store_dir)?;

    if gc_days > 0 {
        if let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            let cutoff = now.as_secs().saturating_sub(gc_days * 86400);
            match store.gc(cutoff, false) {
                Ok(r) if r.deleted > 0 => {
                    eprintln!(
                        "piggybank: auto-gc removed {} entries ({} bytes), older than {} days",
                        r.deleted, r.freed_bytes, gc_days
                    );
                }
                Ok(_) => {}
                Err(e) => eprintln!("piggybank: auto-gc failed (non-fatal): {e}"),
            }
        }
    }

    let hostname = gethostname();
    let harvester = if let Some(url) = harvest_url {
        Harvester::new_http(url)
    } else if let Some(path) = harvest_path {
        Harvester::new_file(std::path::Path::new(path))?
    } else {
        Harvester::new_null()
    };
    let state = ServerState {
        session: Session::open(store_dir)?,
        store,
        total_original: load_counter(&saved, "total_original"),
        total_compressed: load_counter(&saved, "total_compressed"),
        compress_calls: load_counter(&saved, "compress_calls"),
        skipped_resends: load_counter(&saved, "skipped_resends"),
        append_bytes_avoided: load_counter(&saved, "append_bytes_avoided"),
        budget_enforcements: load_counter(&saved, "budget_enforcements"),
        analytics_path,
        hostname,
        last_reported_calls: AtomicU64::new(0),
        harvester,
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
        "serverInfo": { "name": "piggybank", "version": env!("CARGO_PKG_VERSION") },
    })
}

fn tool_defs() -> Value {
    json!([
        {
            "name": "compress",
            "description": "Compress content before it reaches an LLM. Auto-detects JSON (lossless columnar compression with recursive value interning: repeated keys/values/subtrees in arrays-of-objects amortized, cross-call content-addressing deduplicates across separate calls) vs text/logs (consecutive and non-consecutive line dedup + middle elision for large blocks, exact recovery via retrieve). Pass `key` (e.g. a file path) to diff against whatever was last compressed under that same key in this session. First-sight keys with no prior version are automatically diffed against other keys' stored content when >33% line overlap is detected (cross-key dedup).",
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
            "description": "Budget-constrained compression: 'I have N bytes of context budget — give me the highest information density you can fit.' Compresses content with a hard byte ceiling. If normal compression already fits, returns that. Otherwise uses anomaly-ranked selection: lines are scored by importance (errors, warnings, failures, stack traces score highest) and the most informative lines are kept while gaps are replaced with ELIDE markers (stored for recovery via retrieve). Elided content is never lost — retrieve any reference id to get the exact original bytes back. Use when context window space is scarce and you need the most important parts of large output.",
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
            ok(id, json!({ "content": [{ "type": "text", "text": text }] }))
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
    let pct = if tot_orig > 0 {
        (saved as f64 / tot_orig as f64) * 100.0
    } else {
        0.0
    };
    let r = json!({
        "lifetime_calls": calls,
        "lifetime_original_bytes": tot_orig,
        "lifetime_compressed_bytes": tot_comp,
        "lifetime_saved_bytes": saved,
        "lifetime_saved_pct": format!("{pct:.1}%"),
    });
    state.persist_analytics();
    r
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
        piggybank_core::compress_json_with_store(content.as_bytes(), &state.store)
    {
        if let Some(k) = key {
            state.session.record_content_hash(k, content.as_bytes());
        }
        let ratio = if !content.is_empty() {
            compressed.len() as f64 / content.len() as f64
        } else {
            1.0
        };
        state.harvester.log(HarvestEvent::Compress {
            ts: harvest::now(),
            session_id: state.harvester.session_id().to_string(),
            key: key.map(|s| s.to_string()),
            original_bytes: content.len(),
            compressed_bytes: compressed.len(),
            ratio,
            content_type: "json".to_string(),
        });
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
            piggybank_core::compress_text(
                &state.store,
                content.as_bytes(),
                &TextOptions::default(),
            )
            .map_err(|e| e.to_string())?,
            "text",
        ),
    };

    let ratio = if !content.is_empty() {
        compressed.len() as f64 / content.len() as f64
    } else {
        1.0
    };
    state.harvester.log(HarvestEvent::Compress {
        ts: harvest::now(),
        session_id: state.harvester.session_id().to_string(),
        key: key.map(|s| s.to_string()),
        original_bytes: content.len(),
        compressed_bytes: compressed.len(),
        ratio,
        content_type: kind.to_string(),
    });
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
    let view_bytes = view.len();
    let (kind, body) = decode_view(view)?;

    let restored = dispatch_decompress(state, kind, body)?;
    state.harvester.log(HarvestEvent::Decompress {
        ts: harvest::now(),
        session_id: state.harvester.session_id().to_string(),
        view_bytes,
        restored_bytes: restored.len(),
    });
    Ok(Value::String(
        String::from_utf8_lossy(&restored).into_owned(),
    ))
}

fn handle_verify(state: &ServerState, args: &Value) -> Result<Value, String> {
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

fn dispatch_decompress(state: &ServerState, kind: &str, body: &str) -> Result<Vec<u8>, String> {
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
        piggybank_core::compress_text_budget(&state.store, content.as_bytes(), max_bytes)
            .map_err(|e| e.to_string())?;
    let within_budget = compressed.len() <= max_bytes;
    let normal_would_exceed = content.len() > max_bytes;
    if normal_would_exceed && within_budget {
        state.budget_enforcements.fetch_add(1, Relaxed);
    }
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
    let accumulated_size = state.session.accumulated_size(key);
    let view_bytes = state
        .session
        .append(key, new_bytes)
        .map_err(|e| e.to_string())?;
    if accumulated_size > 0 {
        state
            .append_bytes_avoided
            .fetch_add(accumulated_size as u64, Relaxed);
    }
    let savings = record_and_savings(state, new_bytes.len(), view_bytes.len());
    Ok(json!({
        "view": encode_view("text", &view_bytes),
        "appended_bytes": new_bytes.len(),
        "view_bytes": view_bytes.len(),
        "savings": savings,
    }))
}

fn handle_changed(state: &ServerState, args: &Value) -> Result<Value, String> {
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
    if known && !changed {
        state.skipped_resends.fetch_add(1, Relaxed);
        state.persist_analytics();
    }
    Ok(json!({ "changed": changed, "known": known }))
}

fn handle_stats(state: &ServerState) -> Result<Value, String> {
    state.report_to_brain();
    let stats = state.store.stats().map_err(|e| e.to_string())?;
    let tot_orig = state.total_original.load(Relaxed);
    let tot_comp = state.total_compressed.load(Relaxed);
    let saved = tot_orig.saturating_sub(tot_comp);
    let pct = if tot_orig > 0 {
        (saved as f64 / tot_orig as f64) * 100.0
    } else {
        0.0
    };
    let append_avoided = state.append_bytes_avoided.load(Relaxed);
    let total_bytes_saved = saved + append_avoided;
    let tokens_saved = total_bytes_saved as f64 / BYTES_PER_TOKEN;
    let cost_saved = tokens_saved * DEFAULT_RATE_PER_MTOK / 1_000_000.0;
    Ok(json!({
        "store_entries": stats.entries,
        "store_bytes": stats.bytes,
        "lifetime_calls": state.compress_calls.load(Relaxed),
        "lifetime_original_bytes": tot_orig,
        "lifetime_compressed_bytes": tot_comp,
        "lifetime_saved_bytes": saved,
        "lifetime_saved_pct": format!("{pct:.1}%"),
        "token_savings": {
            "estimated_input_tokens_saved": tokens_saved as u64,
            "skipped_resends": state.skipped_resends.load(Relaxed),
            "append_bytes_avoided": append_avoided,
            "budget_enforcements": state.budget_enforcements.load(Relaxed),
        },
        "cost_estimate": {
            "rate_per_mtok_usd": DEFAULT_RATE_PER_MTOK,
            "estimated_usd_saved": format!("{cost_saved:.6}"),
        },
    }))
}
