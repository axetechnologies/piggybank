use serde::Serialize;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg_attr(not(test), allow(dead_code))]
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn generate_session_id() -> String {
    let pid = std::process::id();
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:08x}", (pid as u128 ^ ts) as u32)
}

#[derive(Serialize)]
#[serde(tag = "event")]
pub enum HarvestEvent {
    #[serde(rename = "compress")]
    Compress {
        ts: u64,
        session_id: String,
        key: Option<String>,
        original_bytes: usize,
        compressed_bytes: usize,
        ratio: f64,
        content_type: String,
    },
    #[serde(rename = "decompress")]
    Decompress {
        ts: u64,
        session_id: String,
        view_bytes: usize,
        restored_bytes: usize,
    },
    #[serde(rename = "tool_call")]
    ToolCall {
        ts: u64,
        session_id: String,
        server: String,
        tool: String,
        result_bytes: usize,
        auto_compressed: bool,
        compression_ratio: Option<f64>,
    },
    #[serde(rename = "subagent_spawn")]
    SubagentSpawn {
        ts: u64,
        session_id: String,
        agent_id: String,
        agent_type: String,
        prompt_bytes: usize,
        parent_context_bytes: usize,
    },
    #[serde(rename = "subagent_complete")]
    SubagentComplete {
        ts: u64,
        session_id: String,
        agent_id: String,
        agent_type: String,
        result_bytes: usize,
        tool_calls: u64,
        duration_ms: u64,
        input_tokens: u64,
        output_tokens: u64,
    },
    #[serde(rename = "context_transfer")]
    ContextTransfer {
        ts: u64,
        session_id: String,
        direction: String, // "parent_to_child", "child_to_parent"
        bytes: usize,
        compressed_bytes: Option<usize>,
        agent_id: String,
    },
    #[serde(rename = "session_end")]
    SessionEnd {
        ts: u64,
        session_id: String,
        total_calls: u64,
        total_original_bytes: u64,
        total_compressed_bytes: u64,
        saved_pct: f64,
        subagents_spawned: u64,
        subagent_total_tokens: u64,
    },
}

pub struct Harvester {
    sink: Mutex<Box<dyn Write + Send>>,
    session_id: String,
}

impl Harvester {
    pub fn new_file(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            sink: Mutex::new(Box::new(file)),
            session_id: generate_session_id(),
        })
    }

    pub fn new_http(url: &str) -> Self {
        // TODO: POST buffered JSONL batch to `url` on flush/drop.
        let _ = url;
        let buf: Vec<u8> = Vec::new();
        Self {
            sink: Mutex::new(Box::new(std::io::Cursor::new(buf))),
            session_id: generate_session_id(),
        }
    }

    pub fn new_null() -> Self {
        Self {
            sink: Mutex::new(Box::new(std::io::sink())),
            session_id: generate_session_id(),
        }
    }

    pub fn log(&self, event: HarvestEvent) {
        let Ok(mut line) = serde_json::to_vec(&event) else {
            return;
        };
        line.push(b'\n');
        if let Ok(mut sink) = self.sink.lock() {
            let _ = sink.write_all(&line);
            let _ = sink.flush();
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn null_harvester_accepts_all_event_types_without_panic() {
        let h = Harvester::new_null();
        h.log(HarvestEvent::Compress {
            ts: now(),
            session_id: h.session_id().to_string(),
            key: None,
            original_bytes: 100,
            compressed_bytes: 50,
            ratio: 0.5,
            content_type: "json".to_string(),
        });
        h.log(HarvestEvent::Decompress {
            ts: now(),
            session_id: h.session_id().to_string(),
            view_bytes: 50,
            restored_bytes: 100,
        });
        h.log(HarvestEvent::ToolCall {
            ts: now(),
            session_id: h.session_id().to_string(),
            server: "test".to_string(),
            tool: "read_file".to_string(),
            result_bytes: 200,
            auto_compressed: false,
            compression_ratio: None,
        });
        h.log(HarvestEvent::SessionEnd {
            ts: now(),
            session_id: h.session_id().to_string(),
            total_calls: 1,
            total_original_bytes: 100,
            total_compressed_bytes: 50,
            saved_pct: 50.0,
            subagents_spawned: 0,
            subagent_total_tokens: 0,
        });
    }

    #[test]
    fn file_harvester_writes_valid_jsonl() {
        let path = std::env::temp_dir()
            .join(format!("piggybank-harvest-test-{}.jsonl", std::process::id()));
        {
            let h = Harvester::new_file(&path).unwrap();
            h.log(HarvestEvent::Compress {
                ts: 1_000_000,
                session_id: "abcd1234".to_string(),
                key: Some("k1".to_string()),
                original_bytes: 400,
                compressed_bytes: 200,
                ratio: 0.5,
                content_type: "text".to_string(),
            });
            h.log(HarvestEvent::SessionEnd {
                ts: 1_000_001,
                session_id: "abcd1234".to_string(),
                total_calls: 1,
                total_original_bytes: 400,
                total_compressed_bytes: 200,
                saved_pct: 50.0,
                subagents_spawned: 0,
                subagent_total_tokens: 0,
            });
        }

        let content = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);

        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["event"], "compress");
        assert_eq!(first["original_bytes"], 400);
        assert_eq!(first["content_type"], "text");
        assert_eq!(first["key"], "k1");

        let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["event"], "session_end");
        assert_eq!(second["total_calls"], 1);

        fs::remove_file(&path).ok();
    }

    #[test]
    fn session_id_is_8_hex_chars() {
        let id = generate_session_id();
        assert_eq!(id.len(), 8, "session_id must be 8 characters");
        assert!(
            id.chars().all(|c| c.is_ascii_hexdigit()),
            "session_id must be lowercase hex: {id}"
        );
    }
}
