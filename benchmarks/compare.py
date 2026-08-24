#!/usr/bin/env python3
"""Boomerang vs headroom, on the same real corpus.

Corpus (benchmarks/corpus/) is not synthetic or curated to flatter either
tool:
  - api_response.json  real GitHub API response (memjar/boomerang commits)
  - build_log.txt       real `cargo build --workspace -v` output
  - git_diff.txt         real `git log -p --all` from this repo's own history

Token counts use tiktoken's cl100k_base for every measurement (raw
content, headroom's compressed output, boomerang's compressed output) -
one neutral yardstick, not either tool's internal accounting.

headroom's compressor is history-aware: by default it never touches the
last `protect_recent=4` messages, and skips anything under
`min_tokens_to_compress=250` tokens - so each corpus file is wrapped as
an older tool_result in a realistic multi-turn transcript, using
Anthropic's real tool_result wire shape (a user-role message with a list
of tool_result content blocks), which is specifically what its router
detects to distinguish genuine tool output from human-typed text. A flat
string on a bare role doesn't match that detector and silently falls
back to the conservative "protected" path - an earlier version of this
script did that and understated headroom's real compression.

headroom's semantic compressor (Kompress-v2-base, a 274MB HF model) loads
in the background on first use; a request inside the first ~20s "fails
open" (returns the content unchanged) rather than blocking. That's a real
operational characteristic, not a bug to route around - so this harness
reports it explicitly as a one-time warmup cost, separate from
steady-state per-file compression latency once the model is loaded. All
headroom measurements run in a single persistent subprocess (one model
load, all corpus files) rather than one subprocess per file, matching how
it's actually deployed (a long-lived proxy, not a fresh process per
request).

boomerang has no such warmup: it's a single static binary, so its first
compress call and its hundredth cost the same. That's not asserted here,
it's measured below.

boomerang has no message-history layer yet (see README) - it operates on
raw content directly, via its actual CLI subprocess, the way a user would
really invoke it today.

Usage: benchmarks/compare.py  (run from the repo root)
"""

import json
import subprocess
import sys
import time
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
CORPUS_DIR = REPO_ROOT / "benchmarks" / "corpus"
BOOMERANG_BIN = REPO_ROOT / "target" / "debug" / "boomerang"
HEADROOM_PYTHON = Path.home() / ".local" / "pipx" / "venvs" / "headroom-ai" / "bin" / "python"

import tiktoken  # noqa: E402

ENC = tiktoken.get_encoding("cl100k_base")


def tokens(text: str) -> int:
    return len(ENC.encode(text, disallowed_special=()))


HEADROOM_DRIVER = r"""
import json, sys, time
from headroom import compress
from headroom.transforms.kompress_compressor import KompressCompressor

# headroom's own compress() API requires *some* model name (it uses this for
# token counting and context-window bookkeeping, not for routing/compression
# decisions, which are content-driven). This is a property of headroom's
# API being tested, not of boomerang, which has zero model/provider coupling
# anywhere in its own source - grep crates/ if you want to confirm that
# yourself. Swap this for any model headroom's tokenizer registry supports;
# it doesn't change what's being compared.
HEADROOM_TARGET_MODEL = "claude-sonnet-4-5-20250929"
HEADROOM_TARGET_MODEL_LIMIT = 200000

files = json.loads(sys.stdin.read())

warm = KompressCompressor()
warm.ensure_background_load()
t0 = time.monotonic()
while not warm.is_ready() and time.monotonic() - t0 < 120:
    time.sleep(1)
warmup_s = time.monotonic() - t0

def wrap(target_content):
    return [
        {"role": "system", "content": "You are a coding agent working in a git repository."},
        {"role": "user", "content": "Check the recent activity and build status, then summarize."},
        {"role": "assistant", "content": [{"type": "tool_use", "id": "toolu_01", "name": "run_command", "input": {}}]},
        {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_01", "content": target_content}]},
        {"role": "assistant", "content": "Got it, let me look at one more thing."},
        {"role": "user", "content": "Also confirm the tests pass."},
        {"role": "assistant", "content": [{"type": "tool_use", "id": "toolu_02", "name": "run_tests", "input": {}}]},
        {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_02", "content": "test result: ok. 17 passed; 0 failed."}]},
        {"role": "assistant", "content": "All 17 tests pass."},
    ]

results = {"warmup_s": warmup_s, "files": {}}
for name, content in files.items():
    messages = wrap(content)
    start = time.monotonic()
    result = compress(messages, model=HEADROOM_TARGET_MODEL, model_limit=HEADROOM_TARGET_MODEL_LIMIT)
    elapsed = time.monotonic() - start
    block = next(b for b in result.messages[3]["content"] if isinstance(b, dict) and b.get("type") == "tool_result")
    target = block["content"]
    if not isinstance(target, str):
        target = json.dumps(target)
    results["files"][name] = {
        "compressed_text": target,
        "elapsed_s": elapsed,
        "transforms_applied": result.transforms_applied,
    }

print(json.dumps(results))
"""


def run_headroom_all(corpus: dict[str, str]) -> dict:
    proc = subprocess.run(
        [str(HEADROOM_PYTHON), "-c", HEADROOM_DRIVER],
        input=json.dumps(corpus),
        capture_output=True,
        text=True,
        timeout=180,
    )
    if proc.returncode != 0:
        sys.exit(f"headroom driver failed:\n{proc.stderr}")
    return json.loads(proc.stdout.strip().splitlines()[-1])


def run_boomerang(path: Path) -> dict:
    is_json = path.suffix == ".json"
    subcmd = "compress" if is_json else "compress-log"
    store_dir = REPO_ROOT / "benchmarks" / ".bench-store"
    args = [str(BOOMERANG_BIN), subcmd, str(path)]
    if not is_json:
        args.append(str(store_dir))

    start = time.monotonic()
    proc = subprocess.run(args, capture_output=True, text=True, timeout=60)
    elapsed = time.monotonic() - start
    if proc.returncode != 0:
        return {"error": proc.stderr.strip(), "elapsed_s": elapsed}
    compressed_text = proc.stdout

    decompress_args = (
        [str(BOOMERANG_BIN), "decompress", "/dev/stdin"]
        if is_json
        else [str(BOOMERANG_BIN), "decompress-log", "/dev/stdin", str(store_dir)]
    )
    decompress_proc = subprocess.run(
        decompress_args, input=compressed_text, capture_output=True, text=True, timeout=60
    )
    if is_json:
        # JSON round-trips to an equivalent Value, not identical bytes
        # (whitespace/key-order aren't semantic) - compare parsed, like the
        # library's own round-trip tests do.
        fidelity_ok = json.loads(decompress_proc.stdout) == json.loads(path.read_text())
    else:
        fidelity_ok = decompress_proc.stdout == path.read_text()

    return {"compressed_text": compressed_text, "elapsed_s": elapsed, "fidelity_ok": fidelity_ok}


def main() -> None:
    if not BOOMERANG_BIN.exists():
        sys.exit(f"boomerang binary not found at {BOOMERANG_BIN} - run `cargo build -p boomerang-cli` first")
    if not HEADROOM_PYTHON.exists():
        sys.exit(f"headroom-ai venv python not found at {HEADROOM_PYTHON}")

    paths = sorted(CORPUS_DIR.glob("*"))
    corpus = {p.name: p.read_text() for p in paths}

    print("warming up headroom's Kompress model (one-time, ~274MB on first ever run)...", file=sys.stderr)
    hr_all = run_headroom_all(corpus)
    print(f"headroom warmup: {hr_all['warmup_s']:.1f}s", file=sys.stderr)

    rows = []
    for path in paths:
        raw = corpus[path.name]
        raw_tokens = tokens(raw)
        hr = hr_all["files"][path.name]
        bm = run_boomerang(path)

        row = {
            "file": path.name,
            "raw_bytes": len(raw.encode()),
            "raw_tokens": raw_tokens,
            "headroom_tokens": tokens(hr["compressed_text"]),
            "headroom_elapsed_ms": round(hr["elapsed_s"] * 1000, 1),
            "headroom_transforms": hr["transforms_applied"],
        }
        row["headroom_reduction_pct"] = round(100 * (1 - row["headroom_tokens"] / raw_tokens), 1)

        if "error" in bm:
            row["boomerang_error"] = bm["error"]
        else:
            row["boomerang_tokens"] = tokens(bm["compressed_text"])
            row["boomerang_reduction_pct"] = round(100 * (1 - row["boomerang_tokens"] / raw_tokens), 1)
            row["boomerang_elapsed_ms"] = round(bm["elapsed_s"] * 1000, 1)
            row["boomerang_fidelity_ok"] = bm["fidelity_ok"]

        rows.append(row)
        print(json.dumps(row, indent=2))
        print("---")

    out_path = REPO_ROOT / "benchmarks" / "results.json"
    out_path.write_text(json.dumps({"headroom_warmup_s": hr_all["warmup_s"], "results": rows}, indent=2))
    print(f"\nwrote {out_path}")


if __name__ == "__main__":
    main()
