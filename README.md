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

Not built yet, in order:

- Log/text compressor (dedup repeated lines, elide-with-reference for large
  blocks) — the first place `Store` actually gets used for something lossy.
- Diff-against-session-cache for files an agent has already seen.
- An MCP stdio server (`compress` / `retrieve` / `stats`) exposing all of
  the above.
- An HTTP proxy binary, sharing the same core crate.

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

Early scaffold. `boomerang-core` has `Store` and the JSON compressor with
round-trip tests. `boomerang-cli` is a minimal `compress`/`decompress` file
CLI for exercising the core by hand. No MCP server, no proxy, no benchmarks
against headroom yet — see `benchmarks/` (not yet created) for that
comparison once there's something worth measuring.
