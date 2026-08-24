use std::env;
use std::fs;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("compress") => run(&args, boomerang_core::compress_json),
        Some("decompress") => run(&args, boomerang_core::decompress_json),
        _ => {
            eprintln!("usage: boomerang <compress|decompress> <file>");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String], f: fn(&[u8]) -> serde_json::Result<Vec<u8>>) -> ExitCode {
    let Some(path) = args.get(2) else {
        eprintln!("usage: boomerang <compress|decompress> <file>");
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
            println!("{}", String::from_utf8_lossy(&output));
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
