# Piggybank — Ghost Briefing

You have a context compression MCP server installed and running in this session. It saves tokens and money by deduplicating repeated content across tool calls. This doc explains what it is, what's been verified, and what needs your attention.

## What it does

Piggybank sits between you and the LLM. When tool results come back (file reads, API responses, build output), it compresses them before they hit the context window. Nothing is ever lost — every compressed reference resolves back to the exact original bytes.

Single Rust binary. Sub-5ms for typical payloads. No dependencies.

### 8 MCP tools

| Tool | What it does |
|------|-------------|
| `compress` | Auto-detects JSON (columnar tables) or text (dedup + elision). Pass `key` for session diffing. |
| `decompress` | Full reconstruction from a compressed view. |
| `verify` | Confirm all references still resolve without full reconstruction. |
| `retrieve` | Fetch exact original bytes behind any reference ID. |
| `changed` | SHA256 fingerprint check — skip recompression if content hasn't changed. |
| `compress_append` | Streaming append for logs/builds — only new lines appear each call. |
| `compress_budget` | Budget-constrained compression — fit within N bytes, anomaly-ranked (errors survive tight budgets). |
| `stats` | Lifetime savings: bytes in, bytes out, cost estimates. |

### Transparent proxy

`piggybank proxy [--threshold N] -- <child-mcp-command>` wraps any MCP server and auto-compresses responses over the threshold. AI-agnostic — works without the LLM choosing to call tools.

### Training data harvester

`harvest.rs` defines JSONL event types for compress, decompress, tool calls, sub-agent spawns/completions, context transfers, and session summaries. Not yet wired into the proxy's auto-compress path — that's pending work.

## What's been verified

Enterprise verification completed 2026-08-27. 72 tool calls, ~13 minutes of automated testing.

### Results

- **98/98 unit tests pass** (including proptest fuzzing for arbitrary JSON/text)
- **Round-trip fidelity**: all content types pass — small JSON, large JSON (600 items), unicode/emoji/CJK, mixed line endings, binary content, empty input, adversarial BOOMERANG markers
- **MCP protocol compliance**: all 8 tools return spec-compliant responses, error handling uses `isError: true`, path traversal rejected at store level
- **Proxy**: starts clean, merges tool lists (16 tools — 8 original + 8 `pb_` prefixed), passes tool calls through correctly
- **Performance**: <2ms for <3KB, <5ms for <10KB, ~17ms for 30KB
- **Edge cases**: diff on re-compress, unchanged detection, stats accumulation, verify with missing refs

### One design note

JSON compression normalizes key ordering (canonical JSON). Semantically lossless — all values preserved exactly — but not byte-identical whitespace. This is intentional: the columnar encoding requires canonical form.

## What needs your attention

### Pressure test these areas

1. **Use it in a real session.** Compress tool output over 2KB with `compress` using the source as the key. Work from the compressed view. Decompress if you need exact bytes. Does the workflow feel natural? Does it get in the way?

2. **`changed` tool flow.** After compressing content under a key, call `changed` with the SHA256 hash. Verify `known: true, changed: false`. Then compress different content under the same key. Call `changed` with the old hash — should return `changed: true`.

3. **`compress_budget` under pressure.** Take a large tool result (10KB+), compress with a 500-byte budget. Are errors and important lines preserved? Are normal lines elided? Retrieve the elided refs and verify they contain the exact original content.

4. **`compress_append` for build output.** Run a build, compress with key `"build"`. Run it again with `compress_append`. Only new lines should appear. Does the delta make sense?

5. **Proxy mode.** If you can test `piggybank proxy --threshold 2048 -- piggybank mcp serve`, verify that tools/list merges correctly and auto-compression kicks in for large responses.

6. **Cross-session store sharing.** The store at `~/.axe/boomerang-store` persists across sessions. Content compressed in one session should be retrievable in the next. Verify this works.

### Harden and polish

Review the codebase at `~/Desktop/piggybank` for:

1. **Error messages.** Are MCP error responses clear enough for an LLM to recover from? Check `handle_*` functions in `crates/piggybank-cli/src/mcp.rs`.

2. **Usage string formatting.** The `usage()` function in `main.rs` has spacing issues — some lines are missing spaces after `piggybank` (e.g., `piggybank<compress-log` instead of `piggybank <compress-log`). Fix these.

3. **Proxy robustness.** `crates/piggybank-cli/src/proxy.rs` — what happens if the child process dies mid-request? Is there graceful error handling or does the proxy hang?

4. **Harvester wiring.** `crates/piggybank-core/src/harvest.rs` defines events but they aren't emitted from the proxy yet. Wire `HarvestEvent::ToolCall` into the proxy's auto-compress path. Wire `HarvestEvent::Compress` into the MCP server's compress handler.

5. **Store directory creation.** If `--store-dir` points to a path that doesn't exist yet, does `Store::open` create it or error? It should create it.

6. **BRAIN_KEY in config.** The MCP server config in `~/.claude.json` has a BRAIN_KEY env var. The `mcp.rs` file has a `BRAIN_URL` constant pointing to `brain.axe.onl`. Verify this telemetry path works or remove it if it's dead code.

7. **GC safety.** `piggybank gc` is CLI-only, never exposed over MCP. Verify there's no MCP handler that could trigger deletion. Check that GC respects in-use references.

## Repo state

- GitHub: `axetechnologies/piggybank` (public), main branch, commit `c3ed981`
- npm: `piggybank-mcp@0.1.1`
- Binary: `~/Desktop/piggybank/target/release/piggybank`
- Store: `~/.axe/boomerang-store`
- All code committed and pushed. Clean working tree.

## Automatic usage rules

When piggybank is available, use it proactively:

1. **Tool output over 2KB**: compress with the source as key
2. **Re-reading the same file**: call `changed` first — skip if unchanged
3. **Build/test output**: use `compress_append` with stable key
4. **End of session**: call `stats` and include savings in summary
5. **Long conversations**: use `compress_budget` on the biggest content
