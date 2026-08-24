# Boomerang

Micro context-compression for AI agents. A much lighter, more robust take on
[headroom](https://github.com/chopratejas/headroom): same core idea (shrink
what an agent reads before it hits the LLM, never actually lose anything),
built as a single static Rust binary instead of a Python package that pulls
in `torch`+`transformers`.

## Why this exists

`headroom-ai`'s pipx venv broke because Homebrew retired the Python version
it was pinned to — a rotted interpreter symlink took down an MCP server with
one line in a config file. That's not a headroom-specific bug, it's what
happens when a context-compression layer — something that sits between an
agent and its own perception of the world — depends on a large, versioned
runtime. Boomerang's answer: no interpreter, no downloaded model weights, no
venv. One binary.

## The name

Headroom compresses; Boomerang returns. The mechanism this project is built
around is content-addressed, reversible retrieval — nothing is ever deleted,
only set aside, and it comes back the moment something asks for it by
reference. A boomerang doesn't need to be fetched — it's already coming back.

## Design

Two primitives, nothing else load-bearing:

1. **A view** — a smaller representation of a document.
2. **Recovery** — the original, byte-for-byte, on demand, forever.

Everything else (proxy, MCP server, per-agent wrappers) is product surface
around those two. See [`crates/boomerang-core`](crates/boomerang-core) for
the current state:

- `Store` — a content-addressed blob store (`sha256(bytes) -> bytes` on
  disk). Writing the same content twice is a no-op. This is what makes
  compression reversible: a compressor never decides what's safe to discard,
  because nothing it stores here is ever discarded.
- `compress_json` / `decompress_json` — **lossless** structural compression.
  Homogeneous arrays of objects (the shape of almost every API response or
  tool-result list) become a columnar table: keys written once, then rows of
  values, instead of repeating every key name per element. This alone
  doesn't need `Store` at all — there's nothing to hold back for later,
  because no information is lost.
- `compress_text` / `decompress_text` — dedup of consecutive repeated lines
  (only when the collapse actually pays for itself) plus middle-elision past
  a line-count threshold, with the elided span held in `Store` and recovered
  exactly on retrieval.
- `Session` — diffs against whatever was last compressed under a given key
  (e.g. a file path): first sight passes through, an identical re-read
  collapses to a short marker, a changed re-read gets a compact line diff
  (a hand-rolled LCS differ) replayable against the stored previous version.
  `Session::open` persists state to disk so it survives process restarts.

`boomerang mcp serve` ([`crates/boomerang-cli/src/mcp.rs`](crates/boomerang-cli/src/mcp.rs))
exposes all of it over stdio as a hand-rolled MCP server — no SDK, no async
runtime, just newline-delimited JSON-RPC — with three tools named to mirror
headroom's own surface: `boomerang_compress`, `boomerang_retrieve`,
`boomerang_stats`.

Not built yet:

- An HTTP proxy binary, sharing the same core crate.
- Message-history awareness at the proxy/MCP layer (right now `Session`
  diffs raw content by caller-supplied key; it doesn't yet auto-detect
  conversation structure the way headroom's router does).

Explicitly *not* planned for v0: output-token compression (touching what the
model writes, not just what it reads) and any ML-based prose compressor —
both are correctness/complexity risks that the deterministic core doesn't
need in order to capture most of the win.

## The one invariant

```
retrieve(store.put(x)) == x
```

byte-for-byte, always. Tested as a hard property in
`crates/boomerang-core/src/lib.rs`, not eyeballed.

## Status

All three compressors (`Store`, JSON, text/log, `Session` diff) are built,
tested (17 unit tests, all round-trip-verified), and exposed over a real MCP
server (`boomerang mcp serve`), CI-checked on every push
(fmt/clippy/test/release build). No HTTP proxy yet, no conversation-structure
awareness at the MCP layer yet.

See [`benchmarks/RESULTS.md`](benchmarks/RESULTS.md) for a real (not
synthetic) comparison against headroom: boomerang wins decisively on
structural/repetitive content (68–98% reduction on a build log and a git
diff, 100–400x faster, zero warmup) and loses narrowly on a JSON case with
mostly-unique per-row data, where headroom's ML-based semantic compressor
found more to trim than structural dedup could. It's a mixed result, reported
as one — see that file for the full analysis and its limitations.
