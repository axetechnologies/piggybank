# Piggybank

**Formerly Boomerang.** Context-compression for AI agents — every token saved is money back in the piggybank.

A lightweight, self-contained Rust binary that sits between an agent and its LLM, compressing what the agent reads before it hits the context window. Nothing is ever lost: every compressed reference resolves back to the exact original bytes, byte-for-byte, always.

## Why "Piggybank"?

Every token an agent sends to an LLM costs money. Piggybank saves those tokens — and tracks exactly how many it saved, what that cost in dollars, and how the savings compound over time. The name is the value proposition: this is where your token budget goes to grow.

The project was originally called Boomerang ("compress and it comes back"). The rename reflects what matters: not the mechanism (reversible compression), but the outcome (measurable cost savings on every API call).

> Wire-format markers (`BOOMERANG:ELIDE`, `__boomerang_table__`, etc.) are intentionally preserved for backward compatibility with existing compressed content.

## What it does

Sub-5ms, 426KB binary. No interpreter, no venv, no model weights, no dependencies that can rot. One binary, linked only against `libSystem` on macOS (fully static with musl on Linux).

### Eight MCP tools

| Tool | Purpose |
|------|---------|
| `compress` | Auto-detect JSON (lossless columnar) or text (dedup + elision). Pass `key` for session-aware diffing. |
| `decompress` | Full reconstruction from a compressed view. |
| `verify` | Confirm all references in a compressed view still resolve (stat, not read). |
| `retrieve` | Fetch exact original bytes behind any reference id, plus `first_seen_unix` provenance. |
| `changed` | Fingerprint check: send a sha256 hash, skip recompression if content hasn't changed. |
| `compress_append` | Streaming append: send only new bytes for log tailing / build polling. |
| `compress_budget` | Budget-constrained: "I have N bytes of budget — give me maximum information density." |
| `stats` | Store metrics, token savings analytics, and cost estimates. |

### Token savings analytics

The `stats` tool tracks cumulative savings across the session:

- **Tokens saved** from compression, skipped resends, and append-mode deltas
- **Cost estimates** at configurable $/MTok rates (default $3/MTok)
- **Efficiency metrics**: compression ratio, budget enforcement count, skip rate

### Three compression strategies

1. **JSON** — Homogeneous arrays become columnar tables (keys once, then rows). Value interning deduplicates repeated subtrees across the entire document. Cross-call content-addressing means the second fetch of unchanged data costs nearly nothing.

2. **Text/logs** — Consecutive duplicate lines collapsed with counts. Large blocks middle-elided with the elided span held in the store for exact recovery. Anomalous lines (errors, warnings) kept verbatim.

3. **Session diffing** — First sight passes through; identical re-reads collapse to a short marker; changed re-reads get a compact LCS-based line diff replayable against the stored previous version.

## Beyond compression

The compression is built on a content-addressed, atomically-written, crash-safe blob store. Four properties that follow from this:

- **Shared memory across agents that never coordinate.** Two independent `piggybank mcp serve` processes sharing a `--store-dir` compound each other's compression history automatically. No communication required.
- **Provenance.** Every write records `first_seen_unix` — "what did any agent see, and when" is always answerable.
- **Integrity verification without reconstruction.** `verify` walks reference chains with `stat` calls, not reads — cheaper than full `decompress` when you only need "is this still reconstructable?"
- **Retention, honestly scoped.** `piggybank gc <store-dir> --older-than-days N [--dry-run]` — age-based, CLI-only, never exposed over MCP. An agent cannot delete shared content on its own initiative.

## Setup

Add to your MCP config (`~/.claude.json` or project `.mcp.json`):

```json
{
  "mcpServers": {
    "piggybank": {
      "command": "piggybank",
      "args": ["mcp", "serve", "--store-dir", "/path/to/store"]
    }
  }
}
```

Or let it default to `~/.piggybank/store`.

## The invariant

```
retrieve(store.put(x)) == x
```

Byte-for-byte, always. Tested as a hard property (proptest fuzzing, 76 tests), not eyeballed.

## Design principles

- **Lossless round-trip is non-negotiable.** `decompress(compress(x)) == x`. Budget-constrained compression is the one exception, and even there the elided content is recoverable via `retrieve`.
- **Pay-for-itself discipline.** No transform ever grows the output. If compression doesn't save bytes, it's skipped.
- **Light over clever.** Simple, obviously-correct implementations. Textbook LCS over Myers, linear scans over indexes, proptest over assumptions.
- **No new dependencies without justification.** Four crate dependencies total (`serde`, `serde_json`, `sha2`, `hex`).

## Status

All eight tools built, tested (76 tests including proptests), and exposed over a hand-rolled MCP server (no SDK, no async runtime — just newline-delimited JSON-RPC over stdio). Release binary is size-tuned (`opt-level = "z"`, LTO, single codegen unit, stripped) while keeping `panic=unwind` for per-request resilience in the long-running server.

See [`docs/ROADMAP.md`](docs/ROADMAP.md) for planned features.
