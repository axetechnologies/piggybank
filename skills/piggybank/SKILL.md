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

## Wire Format

Compressed output uses inline markers like `BOOMERANG:ELIDE:<ref-id>` for elided sections. These are stable, permanent references into the local store. The `retrieve` tool resolves any marker back to its original content.
