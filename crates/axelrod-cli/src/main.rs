use std::env;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

fn config_path() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".axelrod").join("config.json")
}

struct Config {
    api_url: String,
    api_key: Option<String>,
}

fn load_config() -> Config {
    let path = config_path();
    let mut api_url = "https://axelrod.network".to_string();
    let mut api_key: Option<String> = env::var("AXELROD_KEY").ok();

    if let Ok(data) = fs::read_to_string(&path) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
            if let Some(u) = v.get("api_url").and_then(|x| x.as_str()) {
                api_url = u.to_string();
            }
            if api_key.is_none() {
                if let Some(k) = v.get("api_key").and_then(|x| x.as_str()) {
                    api_key = Some(k.to_string());
                }
            }
        }
    }

    Config { api_url, api_key }
}

fn save_config(api_url: &str, api_key: &str) -> Result<(), String> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let obj = serde_json::json!({
        "api_url": api_url,
        "api_key": api_key,
    });
    fs::write(&path, serde_json::to_string_pretty(&obj).unwrap())
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn require_key(cfg: &Config) -> Result<&str, String> {
    cfg.api_key
        .as_deref()
        .ok_or_else(|| "no API key — run `axelrod login` or set AXELROD_KEY".to_string())
}

fn agent(_cfg: &Config) -> ureq::Agent {
    ureq::AgentBuilder::new().build()
}

fn auth_header(key: &str) -> String {
    format!("Bearer {}", key)
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("login") => cmd_login(&args),
        Some("datasets") => cmd_datasets(&args),
        Some("pull") => cmd_pull(&args),
        Some("push") => cmd_push(&args),
        Some("preview") => cmd_preview(&args),
        Some("stats") => cmd_stats(&args),
        Some("ingest") => cmd_ingest(&args),
        _ => {
            usage();
            ExitCode::FAILURE
        }
    }
}

fn usage() {
    eprintln!("usage: axelrod login");
    eprintln!("       axelrod datasets");
    eprintln!("       axelrod pull <name> [--output <path>]");
    eprintln!("       axelrod push <name> <file>");
    eprintln!("       axelrod preview <name> [--rows <N>]");
    eprintln!("       axelrod stats [<name>]");
    eprintln!("       axelrod ingest <file>");
}

fn cmd_login(_args: &[String]) -> ExitCode {
    let cfg = load_config();

    print!("API URL [{}]: ", cfg.api_url);
    io::stdout().flush().ok();
    let mut url_input = String::new();
    io::stdin().read_line(&mut url_input).ok();
    let url_input = url_input.trim();
    let api_url = if url_input.is_empty() {
        cfg.api_url.clone()
    } else {
        url_input.to_string()
    };

    print!("API key: ");
    io::stdout().flush().ok();
    let mut key_input = String::new();
    io::stdin().read_line(&mut key_input).ok();
    let api_key = key_input.trim().to_string();

    if api_key.is_empty() {
        eprintln!("error: API key cannot be empty");
        return ExitCode::FAILURE;
    }

    match save_config(&api_url, &api_key) {
        Ok(()) => {
            println!("saved to {}", config_path().display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error saving config: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_datasets(_args: &[String]) -> ExitCode {
    let cfg = load_config();
    let key = match require_key(&cfg) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let url = format!("{}/datasets", cfg.api_url);
    let resp = match agent(&cfg)
        .get(&url)
        .set("Authorization", &auth_header(key))
        .call()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let body: serde_json::Value = match resp.into_json() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error parsing response: {e}");
            return ExitCode::FAILURE;
        }
    };

    let datasets = match body.as_array() {
        Some(a) => a,
        None => {
            println!("{}", serde_json::to_string_pretty(&body).unwrap());
            return ExitCode::SUCCESS;
        }
    };

    if datasets.is_empty() {
        println!("no datasets found");
        return ExitCode::SUCCESS;
    }

    let name_w = datasets
        .iter()
        .filter_map(|d| d.get("name").and_then(|n| n.as_str()))
        .map(|s| s.len())
        .max()
        .unwrap_or(4)
        .max(4);

    println!("{:<name_w$}  {:>10}  {}", "NAME", "ROWS", "UPDATED");
    println!("{}", "-".repeat(name_w + 24));
    for d in datasets {
        let name = d.get("name").and_then(|v| v.as_str()).unwrap_or("-");
        let rows = d
            .get("rows")
            .and_then(|v| v.as_u64())
            .map(|n| n.to_string())
            .unwrap_or_else(|| "-".to_string());
        let updated = d
            .get("updated_at")
            .or_else(|| d.get("updated"))
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        println!("{:<name_w$}  {:>10}  {}", name, rows, updated);
    }

    ExitCode::SUCCESS
}

fn cmd_pull(args: &[String]) -> ExitCode {
    let Some(name) = args.get(2) else {
        eprintln!("usage: axelrod pull <name> [--output <path>]");
        return ExitCode::FAILURE;
    };

    let output_path = args
        .iter()
        .position(|a| a == "--output")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| format!("{}.jsonl", name));

    let cfg = load_config();
    let key = match require_key(&cfg) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let url = format!("{}/datasets/{}/download", cfg.api_url, name);
    let resp = match agent(&cfg)
        .get(&url)
        .set("Authorization", &auth_header(key))
        .call()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut file = match File::create(&output_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error creating {output_path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut reader = resp.into_reader();
    let mut buf = [0u8; 65536];
    let mut total: u64 = 0;
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if let Err(e) = file.write_all(&buf[..n]) {
                    eprintln!("error writing: {e}");
                    return ExitCode::FAILURE;
                }
                total += n as u64;
                eprint!("\r{} bytes written", total);
            }
            Err(e) => {
                eprintln!("\nerror reading response: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    eprintln!("\r{} bytes written", total);
    println!("saved to {output_path}");
    ExitCode::SUCCESS
}

fn cmd_push(args: &[String]) -> ExitCode {
    let (Some(name), Some(file_path)) = (args.get(2), args.get(3)) else {
        eprintln!("usage: axelrod push <name> <file>");
        return ExitCode::FAILURE;
    };

    let data = match fs::read(file_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error reading {file_path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let cfg = load_config();
    let key = match require_key(&cfg) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!("uploading {} bytes...", data.len());
    let url = format!("{}/datasets/{}/upload", cfg.api_url, name);
    let resp = match agent(&cfg)
        .post(&url)
        .set("Authorization", &auth_header(key))
        .set("Content-Type", "application/x-ndjson")
        .send_bytes(&data)
    {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            let body = r.into_string().unwrap_or_default();
            eprintln!("error {code}: {body}");
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let body = resp.into_string().unwrap_or_default();
    println!("{body}");
    ExitCode::SUCCESS
}

fn cmd_preview(args: &[String]) -> ExitCode {
    let Some(name) = args.get(2) else {
        eprintln!("usage: axelrod preview <name> [--rows <N>]");
        return ExitCode::FAILURE;
    };

    let rows: u32 = args
        .iter()
        .position(|a| a == "--rows")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);

    let cfg = load_config();
    let key = match require_key(&cfg) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let url = format!("{}/datasets/{}/sample?n={}", cfg.api_url, name, rows);
    let resp = match agent(&cfg)
        .get(&url)
        .set("Authorization", &auth_header(key))
        .call()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let body = resp.into_string().unwrap_or_default();
    for line in body.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<serde_json::Value>(line) {
            Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap()),
            Err(_) => println!("{}", line),
        }
    }
    ExitCode::SUCCESS
}

fn cmd_stats(args: &[String]) -> ExitCode {
    let cfg = load_config();
    let key = match require_key(&cfg) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let url = if let Some(name) = args.get(2) {
        format!("{}/datasets/{}/stats", cfg.api_url, name)
    } else {
        format!("{}/datasets", cfg.api_url)
    };

    let resp = match agent(&cfg)
        .get(&url)
        .set("Authorization", &auth_header(key))
        .call()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let body: serde_json::Value = match resp.into_json() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error parsing response: {e}");
            return ExitCode::FAILURE;
        }
    };

    if let Some(obj) = body.as_object() {
        let key_w = obj.keys().map(|k| k.len()).max().unwrap_or(3).max(3);
        println!("{:<key_w$}  VALUE", "KEY");
        println!("{}", "-".repeat(key_w + 20));
        for (k, v) in obj {
            let val = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            println!("{:<key_w$}  {}", k, val);
        }
    } else if let Some(arr) = body.as_array() {
        let name_w = arr
            .iter()
            .filter_map(|d| d.get("name").and_then(|n| n.as_str()))
            .map(|s| s.len())
            .max()
            .unwrap_or(4)
            .max(4);
        println!("{:<name_w$}  {:>10}  {:>12}", "NAME", "ROWS", "SIZE");
        println!("{}", "-".repeat(name_w + 26));
        for d in arr {
            let name = d.get("name").and_then(|v| v.as_str()).unwrap_or("-");
            let rows = d
                .get("rows")
                .and_then(|v| v.as_u64())
                .map(|n| n.to_string())
                .unwrap_or_else(|| "-".to_string());
            let size = d
                .get("size_bytes")
                .or_else(|| d.get("size"))
                .and_then(|v| v.as_u64())
                .map(|n| format!("{} B", n))
                .unwrap_or_else(|| "-".to_string());
            println!("{:<name_w$}  {:>10}  {:>12}", name, rows, size);
        }
    } else {
        println!("{}", serde_json::to_string_pretty(&body).unwrap());
    }

    ExitCode::SUCCESS
}

fn cmd_ingest(args: &[String]) -> ExitCode {
    let Some(file_path) = args.get(2) else {
        eprintln!("usage: axelrod ingest <file>");
        return ExitCode::FAILURE;
    };

    let data = match fs::read(file_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error reading {file_path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let line_count = data.iter().filter(|&&b| b == b'\n').count();
    println!("ingesting {} bytes (~{} lines)...", data.len(), line_count);

    let cfg = load_config();
    let key = match require_key(&cfg) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let url = format!("{}/ingest", cfg.api_url);
    let resp = match agent(&cfg)
        .post(&url)
        .set("Authorization", &auth_header(key))
        .set("Content-Type", "application/x-ndjson")
        .send_bytes(&data)
    {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            let body = r.into_string().unwrap_or_default();
            eprintln!("error {code}: {body}");
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let body = resp.into_string().unwrap_or_default();
    println!("{body}");
    ExitCode::SUCCESS
}
