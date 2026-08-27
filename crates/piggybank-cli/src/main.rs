mod mcp;
mod proxy;

use piggybank_core::{Session, Store, TextOptions};
use std::env;
use std::fs;
use std::io::{self, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("compress") => run_json(&args, piggybank_core::compress_json),
        Some("decompress") => run_json(&args, piggybank_core::decompress_json),
        Some("compress-log") => run_text(&args, true),
        Some("decompress-log") => run_text(&args, false),
        Some("compress-session") => run_session_compress(&args),
        Some("decompress-session") => run_session_decompress(&args),
        Some("mcp") if args.get(2).map(String::as_str) == Some("serve") => run_mcp_serve(&args),
        Some("gc") => run_gc(&args),
        Some("proxy") => run_proxy(&args),
        _ => {
            usage();
            ExitCode::FAILURE
        }
    }
}

fn usage() {
    eprintln!(
        "usage: piggybank <compress|decompress> <file>                       # JSON, lossless"
    );
    eprintln!("       piggybank <compress-log|decompress-log> <file> [store-dir]  # text/logs");
    eprintln!("       piggybank compress-session <key> <file> [store-dir]        # diff vs. last seen under <key>");
    eprintln!("       piggybank decompress-session <file> [store-dir]");
    eprintln!("       piggybank mcp serve [--store-dir <path>] [--gc-days <N>]   # MCP server over stdio (auto-GC: default 7d, 0=off)");
    eprintln!("       piggybank gc <store-dir> --older-than-days <N> [--dry-run] # delete content first seen more than N days ago");
    eprintln!("                                                                    # (explicit, human-invoked only - never exposed over MCP)");
    eprintln!("       piggybank proxy [--threshold <bytes>] [--store-dir <path>] -- <cmd> [args...]");
    eprintln!("                                                                    # transparent MCP proxy with auto-compression");
}

fn run_proxy(args: &[String]) -> ExitCode {
    // args[0] is "piggybank", args[1] is "proxy"; pass everything from args[2] onward.
    let proxy_args: Vec<String> = args[2..].to_vec();
    match proxy::run_proxy_from_args(&proxy_args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("piggybank proxy error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_gc(args: &[String]) -> ExitCode {
    let Some(store_dir) = args.get(2) else {
        usage();
        return ExitCode::FAILURE;
    };
    let Some(days) = args
        .iter()
        .position(|a| a == "--older-than-days")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse::<u64>().ok())
    else {
        eprintln!("error: --older-than-days <N> is required");
        usage();
        return ExitCode::FAILURE;
    };
    let dry_run = args.iter().any(|a| a == "--dry-run");

    let store = match Store::open(store_dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error opening store {store_dir}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let now = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs(),
        Err(e) => {
            eprintln!("error reading system clock: {e}");
            return ExitCode::FAILURE;
        }
    };
    let cutoff = now.saturating_sub(days * 24 * 60 * 60);

    match store.gc(cutoff, dry_run) {
        Ok(result) => {
            let verb = if dry_run { "would delete" } else { "deleted" };
            println!(
                "{verb} {} entries, freeing {} bytes ({} entries skipped: no recorded age)",
                result.deleted, result.freed_bytes, result.skipped_no_provenance
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_mcp_serve(args: &[String]) -> ExitCode {
    let store_dir = args
        .iter()
        .position(|a| a == "--store-dir")
        .and_then(|i| args.get(i + 1).cloned())
        .or_else(|| {
            env::var("HOME")
                .ok()
                .map(|home| format!("{home}/.axe/boomerang-store"))
        })
        .unwrap_or_else(|| ".piggybank-store".to_string());

    let gc_days: u64 = args
        .iter()
        .position(|a| a == "--gc-days")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(7);

    // --harvest enables file harvesting; the path defaults to {store_dir}/harvest.jsonl.
    // --harvest-url enables HTTP harvesting (mutually exclusive with --harvest; url wins).
    let harvest_path: Option<String> = args
        .iter()
        .position(|a| a == "--harvest")
        .map(|i| {
            args.get(i + 1)
                .cloned()
                .unwrap_or_else(|| format!("{store_dir}/harvest.jsonl"))
        });

    let harvest_url: Option<String> = args
        .iter()
        .position(|a| a == "--harvest-url")
        .and_then(|i| args.get(i + 1).cloned());

    match mcp::serve(&store_dir, gc_days, harvest_path.as_deref(), harvest_url.as_deref()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mcp server error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Write bytes to stdout exactly as-is — no added newline, no UTF-8
/// re-encoding. `println!` would do both, and either one breaks the exact
/// round-trip this whole tool exists to guarantee.
fn write_stdout(bytes: &[u8]) {
    io::stdout().write_all(bytes).expect("write to stdout");
}

fn run_json(args: &[String], f: fn(&[u8]) -> serde_json::Result<Vec<u8>>) -> ExitCode {
    let Some(path) = args.get(2) else {
        usage();
        return ExitCode::FAILURE;
    };
    let input = match fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error reading {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    match f(&input) {
        Ok(output) => {
            write_stdout(&output);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_text(args: &[String], compressing: bool) -> ExitCode {
    let Some(path) = args.get(2) else {
        usage();
        return ExitCode::FAILURE;
    };
    let store_dir = args
        .get(3)
        .cloned()
        .unwrap_or_else(|| ".piggybank-store".to_string());
    let store = match Store::open(&store_dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error opening store {store_dir}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let input = match fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error reading {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let result = if compressing {
        piggybank_core::compress_text(&store, &input, &TextOptions::default())
    } else {
        piggybank_core::decompress_text(&store, &input)
    };
    match result {
        Ok(output) => {
            write_stdout(&output);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_session_compress(args: &[String]) -> ExitCode {
    let (Some(key), Some(path)) = (args.get(2), args.get(3)) else {
        usage();
        return ExitCode::FAILURE;
    };
    let store_dir = args
        .get(4)
        .cloned()
        .unwrap_or_else(|| ".piggybank-store".to_string());
    let session = match Session::open(&store_dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error opening session at {store_dir}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let input = match fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error reading {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    match session.compress(key, &input) {
        Ok(output) => {
            write_stdout(&output);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_session_decompress(args: &[String]) -> ExitCode {
    let Some(path) = args.get(2) else {
        usage();
        return ExitCode::FAILURE;
    };
    let store_dir = args
        .get(3)
        .cloned()
        .unwrap_or_else(|| ".piggybank-store".to_string());
    let session = match Session::open(&store_dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error opening session at {store_dir}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let input = match fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error reading {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    match session.decompress(&input) {
        Ok(output) => {
            write_stdout(&output);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
