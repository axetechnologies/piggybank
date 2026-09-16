//! Integration tests for the MCP proxy.
//!
//! Spawns the compiled `piggybank` binary wrapping `tests/fake_mcp_server.py`
//! and exercises the proxy over stdio.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

/// Build path to the compiled piggybank binary.
fn piggybank_bin() -> PathBuf {
    let mut p = std::env::current_exe().expect("current_exe");
    // Walk up from target/<profile>/deps/<test> to target/<profile>/
    p.pop(); // <test>
    p.pop(); // deps
    p.push("piggybank");
    p
}

/// Path to the fake MCP server script.
fn fake_server() -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(manifest).join("tests/fake_mcp_server.py")
}

struct ProxyProcess {
    child: Child,
    stdin: Option<std::process::ChildStdin>,
    stdout: BufReader<std::process::ChildStdout>,
    next_id: u64,
    #[allow(dead_code)]
    store_dir: tempfile::TempDir,
}

impl ProxyProcess {
    fn start(extra_args: &[&str]) -> Self {
        Self::start_with_env(extra_args, &[])
    }

    fn start_with_env(extra_args: &[&str], env: &[(&str, &str)]) -> Self {
        let bin = piggybank_bin();
        let server = fake_server();
        let store_dir = tempfile::tempdir().expect("tempdir");

        let mut cmd = Command::new(&bin);
        cmd.arg("proxy")
            .arg("--store-dir")
            .arg(store_dir.path())
            .args(extra_args)
            .arg("--")
            .arg("python3")
            .arg(&server)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());

        for (k, v) in env {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn().expect("spawn piggybank proxy");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        ProxyProcess {
            child,
            stdin: Some(stdin),
            stdout,
            next_id: 1,
            store_dir,
        }
    }

    fn send(&mut self, msg: Value) {
        let line = serde_json::to_string(&msg).unwrap() + "\n";
        if let Some(ref mut stdin) = self.stdin {
            stdin.write_all(line.as_bytes()).unwrap();
            stdin.flush().unwrap();
        }
    }

    /// Close stdin (signals EOF to the proxy) and wait for it to exit cleanly.
    fn close_and_wait(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.wait();
    }

    fn recv(&mut self) -> Value {
        let mut line = String::new();
        loop {
            line.clear();
            self.stdout.read_line(&mut line).unwrap();
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                return serde_json::from_str(trimmed).expect("valid JSON from proxy");
            }
        }
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        self.recv()
    }

    fn initialize(&mut self) -> Value {
        let r = self.request(
            "initialize",
            json!({ "protocolVersion": "2024-11-05", "clientInfo": { "name": "test", "version": "0" } }),
        );
        // send initialized notification
        self.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        r
    }

    fn tools_list(&mut self) -> Vec<Value> {
        let r = self.request("tools/list", json!({}));
        r["result"]["tools"].as_array().cloned().unwrap_or_default()
    }

    fn call_tool(&mut self, name: &str, args: Value) -> Value {
        self.request("tools/call", json!({ "name": name, "arguments": args }))
    }
}

impl Drop for ProxyProcess {
    fn drop(&mut self) {
        // Close stdin first (triggers clean proxy shutdown via EOF).
        drop(self.stdin.take());
        // Give it 500ms to exit, then force-kill.
        let start = std::time::Instant::now();
        loop {
            if self.child.try_wait().map(|s| s.is_some()).unwrap_or(true) {
                break;
            }
            if start.elapsed().as_millis() > 500 {
                let _ = self.child.kill();
                let _ = self.child.wait();
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // store_dir cleaned up by TempDir
    }
}

fn python3_available() -> bool {
    Command::new("python3")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn proxy_initialize_returns_piggybank_info() {
    if !python3_available() {
        eprintln!("skip: python3 not found");
        return;
    }
    let mut p = ProxyProcess::start(&[]);
    let resp = p.initialize();
    let info = &resp["result"]["serverInfo"];
    assert_eq!(info["name"].as_str(), Some("piggybank-proxy"));
    // version should match Cargo.toml
    let ver = info["version"].as_str().unwrap_or("");
    assert!(!ver.is_empty());
}

#[test]
fn proxy_tools_list_includes_child_and_pb_tools() {
    if !python3_available() {
        eprintln!("skip: python3 not found");
        return;
    }
    let mut p = ProxyProcess::start(&[]);
    p.initialize();
    let tools = p.tools_list();
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    // Child tools present
    assert!(names.contains(&"echo"), "echo tool missing: {:?}", names);
    assert!(names.contains(&"big_response"), "big_response missing");
    // Piggybank tools present
    assert!(names.contains(&"compress"), "compress missing");
    assert!(names.contains(&"decompress"), "decompress missing");
    assert!(names.contains(&"stats"), "stats missing");
}

#[test]
fn proxy_tools_list_trims_descriptions_by_default() {
    if !python3_available() {
        eprintln!("skip: python3 not found");
        return;
    }
    let mut p = ProxyProcess::start(&[]);
    p.initialize();
    let tools = p.tools_list();
    for tool in &tools {
        // Skip piggybank tools (their descs are already short).
        let name = tool["name"].as_str().unwrap_or("");
        if [
            "compress",
            "decompress",
            "verify",
            "retrieve",
            "changed",
            "compress_budget",
            "compress_append",
            "stats",
        ]
        .contains(&name)
        {
            continue;
        }
        let desc = tool["description"].as_str().unwrap_or("");
        // Trimmed description should not contain a second sentence (no second period after first).
        let first_dot = desc.find('.');
        if let Some(pos) = first_dot {
            // Everything after the first '.' should be at the very end.
            assert_eq!(
                pos + 1,
                desc.len(),
                "tool '{}' description has text after first period: {:?}",
                name,
                desc
            );
        }
    }
}

#[test]
fn proxy_tools_list_full_tools_passthrough() {
    if !python3_available() {
        eprintln!("skip: python3 not found");
        return;
    }
    // With --full-tools, child descriptions must pass through unmodified.
    let mut p = ProxyProcess::start(&["--full-tools"]);
    p.initialize();
    let tools = p.tools_list();
    let echo = tools
        .iter()
        .find(|t| t["name"] == "echo")
        .expect("echo tool");
    let desc = echo["description"].as_str().unwrap_or("");
    // The fake server's echo description: "Echo the input back. Returns whatever text you send."
    // With full-tools, the full description should appear.
    assert!(
        desc.contains("Returns whatever"),
        "full-tools should preserve description, got: {:?}",
        desc
    );
}

#[test]
fn proxy_echo_tool_round_trips() {
    if !python3_available() {
        eprintln!("skip: python3 not found");
        return;
    }
    let mut p = ProxyProcess::start(&[]);
    p.initialize();
    let resp = p.call_tool("echo", json!({ "text": "hello proxy" }));
    let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert_eq!(text, "hello proxy");
}

#[test]
fn proxy_compresses_big_response() {
    if !python3_available() {
        eprintln!("skip: python3 not found");
        return;
    }
    // Threshold is default 4096; request 20000 bytes.
    let mut p = ProxyProcess::start(&[]);
    p.initialize();
    let resp = p.call_tool("big_response", json!({ "size": 20000 }));
    // Response should be marked as compressed.
    let compressed = resp["result"]["_piggybank_compressed"]
        .as_bool()
        .unwrap_or(false);
    assert!(
        compressed,
        "large response should have been compressed: {:?}",
        resp["result"]
    );
    // View should start with the BOOM header.
    let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.starts_with("BOOM:"),
        "compressed view should start with BOOM: got {:?}",
        &text[..50.min(text.len())]
    );
}

#[test]
fn proxy_skip_list_prevents_compression() {
    if !python3_available() {
        eprintln!("skip: python3 not found");
        return;
    }
    // With big_response in the skip list, it must NOT be compressed.
    // Pass via cmd.env() to avoid polluting the shared process environment.
    let mut p = ProxyProcess::start_with_env(&[], &[("PIGGYBANK_SKIP_TOOLS", "big_response")]);
    p.initialize();
    let resp = p.call_tool("big_response", json!({ "size": 20000 }));
    let compressed = resp["result"]["_piggybank_compressed"]
        .as_bool()
        .unwrap_or(false);
    assert!(!compressed, "skip-listed tool should not be compressed");
}

#[test]
fn proxy_never_compresses_error_responses() {
    if !python3_available() {
        eprintln!("skip: python3 not found");
        return;
    }
    let mut p = ProxyProcess::start(&[]);
    p.initialize();
    let resp = p.call_tool("error_tool", json!({}));
    // isError must be preserved.
    let is_error = resp["result"]["isError"].as_bool().unwrap_or(false);
    assert!(is_error, "isError flag must pass through");
    // Must not be compressed.
    let compressed = resp["result"]["_piggybank_compressed"]
        .as_bool()
        .unwrap_or(false);
    assert!(!compressed, "error responses must not be compressed");
}

#[test]
fn proxy_pb_compress_tool_works_through_proxy() {
    if !python3_available() {
        eprintln!("skip: python3 not found");
        return;
    }
    let mut p = ProxyProcess::start(&[]);
    p.initialize();
    let content = "hello ".repeat(1000);
    let resp = p.call_tool("compress", json!({ "content": content }));
    let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
    // The text is a JSON object with a 'view' field — it's stringified.
    assert!(
        text.contains("view") || text.starts_with("BOOM:") || text.contains("original_bytes"),
        "compress tool should return a view: {:?}",
        &text[..100.min(text.len())]
    );
}

#[test]
fn proxy_stats_file_written() {
    if !python3_available() {
        eprintln!("skip: python3 not found");
        return;
    }
    let stats_path =
        std::env::temp_dir().join(format!("pb_proxy_test_stats_{}.json", std::process::id()));
    let stats_str = stats_path.to_str().unwrap().to_string();
    let _ = std::fs::remove_file(&stats_path);

    {
        let mut p = ProxyProcess::start(&["--stats-file", &stats_str]);
        p.initialize();
        p.call_tool("echo", json!({ "text": "hi" }));
        // Close stdin and wait for the proxy to exit cleanly so it can flush stats.
        p.close_and_wait();
    }

    assert!(stats_path.exists(), "stats file should be written on exit");
    let data: Value = serde_json::from_str(&std::fs::read_to_string(&stats_path).unwrap())
        .expect("valid stats JSON");
    assert!(data["summary"]["total_calls"].as_u64().unwrap_or(0) >= 1);
    assert!(data["per_tool"].is_array());
    let _ = std::fs::remove_file(&stats_path);
}
