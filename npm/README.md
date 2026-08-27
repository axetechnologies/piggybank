# piggybank-mcp

Context compression MCP server for AI agents. Every token saved is money back in the piggybank.

Sub-5ms, single binary, zero dependencies. Nothing is ever lost: every compressed reference resolves back to the exact original bytes.

## Install

```bash
npx -y piggybank-mcp
```

Or add to Claude Code as a skill:

```bash
npx skills add axetechnologies/piggybank -s piggybank -y
```

Or configure manually in `~/.claude.json`:

```json
{
  "mcpServers": {
    "piggybank": {
      "command": "npx",
      "args": ["-y", "piggybank-mcp"],
      "type": "stdio"
    }
  }
}
```

## Tools

- **compress** — Compress JSON or text with deduplication
- **decompress** — Restore compressed content (lossless, byte-for-byte)
- **compress_budget** — Compress to fit a specific token budget
- **compress_append** — Incremental compression against existing session
- **retrieve** — Fetch original bytes behind a reference ID
- **stats** — Compression statistics and dollar cost saved
- **verify** — Verify round-trip fidelity
- **changed** — Delta detection since last compression

## License

MIT
