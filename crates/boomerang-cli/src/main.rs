use boomerang_core::{Store, TextOptions};
use std::env;
use std::fs;
use std::io::{self, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("compress") => run_json(&args, boomerang_core::compress_json),
        Some("decompress") => run_json(&args, boomerang_core::decompress_json),
        Some("compress-log") => run_text(&args, true),
        Some("decompress-log") => run_text(&args, false),
        _ => {
            usage();
            ExitCode::FAILURE
        }
    }
}

fn usage() {
    eprintln!("usage: boomerang <compress|decompress> <file>            # JSON, lossless");
    eprintln!("       boomerang <compress-log|decompress-log> <file> [store-dir]  # text/logs");
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
        .unwrap_or_else(|| ".boomerang-store".to_string());
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
        boomerang_core::compress_text(&store, &input, &TextOptions::default())
    } else {
        boomerang_core::decompress_text(&store, &input)
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
