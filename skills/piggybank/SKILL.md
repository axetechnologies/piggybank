---
name: piggybank
description: >-
  Context compression MCP for AI agents. Saves tokens and money by
  deduplicating repeated content across tool calls. Use when context windows
  are filling up, when tool results contain large repeated blocks, when you
  want to measure token savings, or when the user mentions piggybank,
  compression, context budget, or token cost.
metadata:
  source: https://github.com/memjar/piggybank/tree/main/skills/piggybank
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

Piggybank runs as a stdio MCP server. If not already configured, add it:

```bash
# One-line install (downloads binary + configures Claude Code):
curl -sSL https://raw.githubusercontent.com/memjar/piggybank/main/install.sh | bash

# Or via npx (no install needed):
# Add to ~/.claude.json under mcpServers:
{
  "piggybank": {
    "command": "npx",
    "args": ["-y", "piggybank-mcp"],
    "type": "stdio",
    "env": {}
  }
}
```

Then restart Claude Code.

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

## Wire Format

Compressed output uses inline markers like `BOOMERANG:ELIDE:<ref-id>` for elided sections. These are stable, permanent references into the local store. The `retrieve` tool resolves any marker back to its original content.
