# piggybank-mcp

Context compression MCP server for AI agents. Every token saved is money back in the piggybank.

Sub-5ms, single binary, zero dependencies. Nothing is ever lost: every compressed reference resolves back to the exact original bytes.

## Setup

Run once to install the MCP server, hooks, and configure Claude Code automatically:

```bash
npx piggybank-mcp init
```

This command:
- Adds `piggybank` to `mcpServers` in `~/.claude.json`
- Installs hook scripts into `~/.piggybank/hooks/`
- Merges `PostToolUse` and `PreCompact` hook entries into `~/.claude/settings.json` (idempotently — no duplicates)
- Backs up settings files before modifying them
- Prints every change made

To preview changes without applying them:

```bash
npx piggybank-mcp init --dry-run
```

To remove everything piggybank installed:

```bash
npx piggybank-mcp init --uninstall
```

## Install (manual)

```bash
npx -y piggybank-mcp
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

## Hook Configuration

The `init` command configures two hooks automatically. You can also install them manually.

**PostToolUse** (compresses large tool outputs before they enter the context window):

```json
{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": ".*",
        "hooks": [{"type": "command", "command": "~/.piggybank/hooks/post-tool-use-compress.sh"}]
      }
    ]
  }
}
```

**PreCompact** (builds a compaction ledger so content is retrievable after compaction):

```json
{
  "hooks": {
    "PreCompact": [
      {
        "hooks": [{"type": "command", "command": "~/.piggybank/hooks/pre-compact-budget.sh"}]
      }
    ]
  }
}
```

### Hook environment variables

| Variable | Default | Description |
|---|---|---|
| `PIGGYBANK_MIN_BYTES` | `2048` | Minimum bytes before compression fires |
| `PIGGYBANK_TOOLS` | (all) | Comma-separated allowlist of tool names |
| `PIGGYBANK_SKIP_TOOLS` | (none) | Comma-separated denylist of tool names |
| `PIGGYBANK_TOOL_THRESHOLD_<TOOL>` | `PIGGYBANK_MIN_BYTES` | Per-tool threshold override |
| `PIGGYBANK_STORE_DIR` | `~/.piggybank/store` | Content-addressed store location |

## Tools

- **compress** — Compress JSON or text with deduplication
- **decompress** — Restore compressed content (lossless, byte-for-byte)
- **compress_budget** — Compress to fit a specific token budget
- **compress_append** — Incremental compression against existing session
- **retrieve** — Fetch original bytes behind a reference ID
- **stats** — Compression statistics and dollar cost saved
- **verify** — Verify round-trip fidelity
- **changed** — Delta detection since last compression

## CLI subcommands

```
piggybank init [--dry-run] [--uninstall] [--store-dir <path>]
piggybank ledger <transcript.jsonl> [--store-dir <path>] [--min-bytes <N>]
piggybank mcp serve [--store-dir <path>] [--gc-days <N>]
piggybank statusline [--store-dir <path>] [--plain]
```

## License

MIT
