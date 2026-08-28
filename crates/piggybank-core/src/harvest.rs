use serde::Serialize;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now() -> u64 {
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

const HTTP_FLUSH_EVERY: usize = 10;

enum Sink {
    File(Box<dyn Write + Send>),
    Http { url: String, buffer: Vec<Vec<u8>> },
    Null,
}

pub struct Harvester {
    sink: Mutex<Sink>,
    session_id: String,
}

impl Harvester {
    pub fn new_file(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            sink: Mutex::new(Sink::File(Box::new(file))),
            session_id: generate_session_id(),
        })
    }

    pub fn new_http(url: &str) -> Self {
        Self {
            sink: Mutex::new(Sink::Http {
                url: url.to_string(),
                buffer: Vec::new(),
            }),
            session_id: generate_session_id(),
        }
    }

    pub fn new_null() -> Self {
        Self {
            sink: Mutex::new(Sink::Null),
            session_id: generate_session_id(),
        }
    }

    pub fn log(&self, event: HarvestEvent) {
        let Ok(line) = serde_json::to_vec(&event) else {
            return;
        };
        let Ok(mut sink) = self.sink.lock() else {
            return;
        };
        match &mut *sink {
            Sink::File(w) => {
                let mut data = line;
                data.push(b'\n');
                let _ = w.write_all(&data);
                let _ = w.flush();
            }
            Sink::Http { url, buffer } => {
                buffer.push(line);
                if buffer.len() >= HTTP_FLUSH_EVERY {
                    let batch: Vec<Vec<u8>> = std::mem::take(buffer);
                    let url_clone = url.clone();
                    std::thread::spawn(move || {
                        http_post_jsonl(&url_clone, &batch);
                    });
                }
            }
            Sink::Null => {}
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    fn flush_http_buffer(sink: &mut Sink) {
        if let Sink::Http { url, buffer } = sink {
            if !buffer.is_empty() {
                let batch: Vec<Vec<u8>> = std::mem::take(buffer);
                let url_clone = url.clone();
                std::thread::spawn(move || {
                    http_post_jsonl(&url_clone, &batch);
                });
            }
        }
    }
}

impl Drop for Harvester {
    fn drop(&mut self) {
        if let Ok(mut sink) = self.sink.lock() {
            Harvester::flush_http_buffer(&mut sink);
        }
    }
}

fn http_post_jsonl(url: &str, batch: &[Vec<u8>]) {
    let body: Vec<u8> = batch
        .iter()
        .flat_map(|line| {
            let mut v = line.clone();
            v.push(b'\n');
            v
        })
        .collect();

    let auth_key = std::env::var("AXELROD_KEY").unwrap_or_default();

    if let Err(e) = http_post(url, &body, &auth_key) {
        eprintln!("piggybank harvest: HTTP POST failed ({url}): {e}");
    }
}

fn http_post(url: &str, body: &[u8], auth_key: &str) -> io::Result<()> {
    let (host, port, path) = parse_url(url)?;

    let addr = format!("{host}:{port}");
    let mut stream = TcpStream::connect(&addr)?;

    let auth_header = if auth_key.is_empty() {
        String::new()
    } else {
        format!("Authorization: Bearer {auth_key}\r\n")
    };

    let request = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: {host}\r\n\
         Content-Type: application/x-ndjson\r\n\
         {auth_header}\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n",
        len = body.len()
    );

    stream.write_all(request.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;

    // Read response to confirm delivery; discard on error.
    let mut resp = Vec::new();
    let _ = std::io::Read::read_to_end(&mut stream, &mut resp);

    Ok(())
}

fn parse_url(url: &str) -> io::Result<(String, u16, String)> {
    let url = url.trim();
    let (rest, default_port) = if let Some(r) = url.strip_prefix("https://") {
        (r, 443u16)
    } else if let Some(r) = url.strip_prefix("http://") {
        (r, 80u16)
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unsupported URL scheme: {url}"),
        ));
    };

    let (authority, path) = match rest.find('/') {
        Some(pos) => (&rest[..pos], rest[pos..].to_string()),
        None => (rest, "/".to_string()),
    };

    let (host, port) = if let Some(pos) = authority.rfind(':') {
        let port: u16 = authority[pos + 1..]
            .parse()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid port in URL"))?;
        (authority[..pos].to_string(), port)
    } else {
        (authority.to_string(), default_port)
    };

    Ok((host, port, path))
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
        let path = std::env::temp_dir().join(format!(
            "piggybank-harvest-test-{}.jsonl",
            std::process::id()
        ));
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
