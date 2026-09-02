---
name: piggybank
description: >-
  Context compression MCP for AI agents. Saves tokens and money by
  deduplicating repeated content across tool calls. Use when context windows
  are filling up, when tool results contain large repeated blocks, when you
  want to measure token savings, or when the user mentions piggybank,
  compression, context budget, or token cost.
metadata:
  source: https://github.com/axetechnologies/piggybank/tree/main/skills/piggybank
mcp:
  piggybank:
    command: npx
    args: ["-y", "piggybank-mcp"]
    type: stdio
    env: {}
---

# Piggybank — Context Compression for AI Agents

Every token an agent sends to an LLM costs money. Piggybank saves those tokens — and tracks exactly how many it saved, what that cost in dollars, and how the savings compound over time.

Sub-5ms, single binary, zero dependencies. Nothing is ever lost: every compressed reference resolves back to the exact original bytes.

## MCP Setup

Piggybank runs as a stdio MCP server. Compatible with any MCP-capable AI coding tool (Claude Code, Cursor, Windsurf, ChatGPT, Copilot, etc).

### Quick install (binary + auto-configure):
```bash
curl -sSL https://raw.githubusercontent.com/axetechnologies/piggybank/main/install.sh | bash
```

### Or via npx (no install needed):
Add to your MCP configuration (e.g. `~/.claude.json`, `.cursor/mcp.json`, or equivalent):
```json
{
  "piggybank": {
    "command": "npx",
    "args": ["-y", "piggybank-mcp"],
    "type": "stdio"
  }
}
```

Then restart your AI coding tool.

## Available Tools

### `compress`
Compress JSON or text content. Returns a compressed view with deduplication references. Use on large tool results, API responses, or repeated content blocks.

### `decompress`
Restore compressed content to its original bytes. Lossless — byte-for-byte identical to the original.

### `compress_budget`
Compress content to fit within a specific token budget. Specify max tokens and piggybank removes the least-important content first while preserving structure.

### `compress_append`
Incrementally compress new content against an existing compressed session. Only the diff is stored — ideal for streaming or multi-turn conversations.

### `retrieve`
Fetch the exact original bytes behind a reference ID embedded in a compressed view (e.g. `BOOMERANG:ELIDE:...` markers). Always succeeds for any ref piggybank returned.

### `stats`
Show compression statistics: total bytes in, bytes out, savings percentage, dollar cost saved, number of sessions.

### `verify`
Verify that a compressed payload can be fully decompressed and matches the original. Use to confirm round-trip fidelity.

### `changed`
Check which parts of content have changed since the last compression under a given session key. Returns only the delta.

## When to Use Piggybank

- **Large tool results**: API responses, file reads, search results — compress before they hit the context window
- **Repeated content**: Same file read multiple times, same API called repeatedly — piggybank deduplicates automatically
- **Budget management**: When approaching context limits, use `compress_budget` to fit within a token cap
- **Session continuity**: Use `compress_append` across turns to maintain context without re-sending everything
- **Cost tracking**: Use `stats` to measure actual token savings across a session

## Automatic Usage Rules

When piggybank is available as an MCP server, the AI assistant MUST use it proactively in these situations without waiting for the user to ask:

1. **Tool output over 2KB**: Compress it with `compress` using the source as the key (file path, URL, command). Work from the compressed view.
2. **Re-reading the same file**: Call `changed` first with the file's sha256. If `known: true, changed: false`, skip the re-read — the content hasn't changed.
3. **Build/test/deploy output**: Use `compress_append` with a stable key (e.g. `"build"`, `"test"`). Only new lines appear in each view.
4. **End of session**: Call `stats` and include savings in the summary when compression was used.
5. **Long conversations**: When context is getting large, use `compress_budget` on the biggest pending content.

These rules apply regardless of which AI tool is running (Claude Code, Cursor, ChatGPT, Copilot, etc). The goal is invisible efficiency — the user sees faster responses and lower costs without thinking about compression.

## Subagent / Multi-Agent Usage

A PostToolUse hook only compresses content **on the wire between a tool and the model that called it**. It cannot shrink content a subagent already pulled into its own context and is now carrying forward turn after turn — a subagent that reads a large source (a PDF, an image, a long build log) once and then reasons over the raw bytes for the next 40 turns pays for that raw content 40 times over, regardless of any hook. This is the actual cause of a subagent burning tens of thousands of tokens rebuilding something like a PDF: not a missing compression on the read, but re-holding decompressed content it should have referenced instead.

Two things must both be true for a spawned subagent to actually benefit:

1. **The orchestrator must load and tell it to.** A subagent starts with no memory of this skill unless the orchestrating agent explicitly says so in the spawn prompt. Any prompt that spawns a subagent doing large-asset work (PDFs, images, long source dumps, build/log output, multi-file review) should include a line like: *"Piggybank MCP tools are available — call `compress` immediately after any read over ~2KB, work from the compressed view, and use `retrieve`/`decompress` only for the specific ref you need, not the whole document."*
2. **The subagent must not re-embed what it can reference.** Once content has a ref (from `compress`) or a session key (from `compress_append`), the subagent should carry the *ref* forward in its own reasoning, not the *bytes* — and only call `retrieve` for the slice it is about to act on. Re-decompressing the full original "just in case" defeats the compression immediately. This is empowering, not limiting: the subagent can hold references to far more source material than would ever fit in its context raw, and pull the exact byte range it needs on demand.

Concretely, for a PDF-building, image-review, or document-audit subagent:
- **Read once, compress immediately.** Don't re-read the same source across iterations — call `changed` first; if unchanged, work from the last compressed view already in context.
- **Never round-trip a full asset through the model just to move it.** Copying or transforming a large file (e.g. embedding a page image into a new PDF) should go store-to-file or tool-to-tool where possible; only pull the exact bytes through the model's context when the model itself must read or reason over that content.
- **Report savings back.** Have the subagent include its own `stats` output in its final report so the orchestrator (and the user) can see the reduction, not just assume it happened.

The failure mode to avoid is a subagent that calls `compress` once, correctly, then never looks at the compressed view again and asks for the raw content back "to be safe" — that's compression with no savings. The `retrieve`/`decompress` tools succeeding on every ref, always, is exactly what makes it safe to *not* hold the raw bytes: nothing is ever actually at risk of being lost.

## Wire Format

Compressed output uses inline markers like `BOOMERANG:ELIDE:<ref-id>` for elided sections. These are stable, permanent references into the local store. The `retrieve` tool resolves any marker back to its original content.
