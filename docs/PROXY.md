# piggybank proxy

`piggybank proxy` wraps any MCP server as a transparent stdio proxy and
auto-compresses large tool responses before they reach the LLM.  It also
injects the eight built-in piggybank tools (`compress`, `decompress`, etc.)
into every session, so the LLM can explicitly compress content too.

## Why use the proxy?

A server with 900+ tools (like `axe-local`) sends enormous `tools/list`
payloads and returns multi-kilobyte responses for many calls.  The proxy
handles both problems:

- **tools/list trimming** — by default, each tool's description is shortened
  to its first sentence and the `inputSchema` is slimmed to property names and
  types.  Set `PIGGYBANK_PROXY_FULL_TOOLS=1` to pass the list through
  unmodified.
- **per-call compression** — responses above the threshold (default 4096 bytes)
  are compressed with piggybank's store-backed algorithm.  The LLM calls
  `decompress` to retrieve the original when it needs to read it.

## Quick start

Replace your MCP server entry with `piggybank proxy -- <original command>`.

### Claude Code `claude_desktop_config.json` example

Before (direct `axe-local` entry):
```json
{
  "mcpServers": {
    "axe-local": {
      "command": "/path/to/axe-mcp",
      "args": ["serve"]
    }
  }
}
```

After (wrapped by piggybank proxy):
```json
{
  "mcpServers": {
    "axe-local": {
      "command": "piggybank",
      "args": [
        "proxy",
        "--threshold", "4096",
        "--store-dir", "/Users/you/.piggybank/store",
        "--",
        "/path/to/axe-mcp",
        "serve"
      ]
    }
  }
}
```

The proxy is transparent: the LLM still sees all the original tools, plus the
eight piggybank tools appended at the end.

## Command-line reference

```
piggybank proxy [OPTIONS] -- <command> [args...]
```

| Option | Default | Description |
|---|---|---|
| `--threshold <N>` | `4096` | Compress responses larger than N bytes |
| `--store-dir <path>` | `~/.piggybank/store` | Piggybank content store directory |
| `--harvest <path>` | off | Log harvest events to a JSONL file |
| `--harvest-url <url>` | off | Stream harvest events to an HTTP endpoint |
| `--stats-file <path>` | off | Write per-tool compression stats JSON on exit |
| `--stats` | off | Same as `--stats-file -` (prints to stderr on exit) |
| `--full-tools` | off | Pass `tools/list` through unmodified (no trimming) |

## Environment variables

| Variable | Description |
|---|---|
| `PIGGYBANK_MIN_BYTES` | Same as `--threshold` (integer) |
| `PIGGYBANK_PROXY_FULL_TOOLS=1` | Same as `--full-tools` |
| `PIGGYBANK_SKIP_TOOLS` | Comma-separated tool names to never compress |

Example — skip two noisy tools and print stats on exit:

```bash
PIGGYBANK_SKIP_TOOLS=browser_screenshot,data_query \
piggybank proxy --stats -- axe-mcp serve
```

## Framing

The proxy auto-detects whether the child uses newline-delimited or
`Content-Length`-framed JSON-RPC.  Both are supported with no configuration.
Messages of any size (10 MB+) are handled correctly.

## Per-tool stats

Pass `--stats-file stats.json` to get a JSON report on exit:

```json
{
  "summary": {
    "total_calls": 42,
    "total_original_bytes": 512000,
    "total_compressed_bytes": 61440,
    "total_saved_bytes": 450560
  },
  "per_tool": [
    {
      "tool": "fleet_docker_logs",
      "calls": 8,
      "compressed_calls": 7,
      "original_bytes": 380000,
      "compressed_bytes": 42000,
      "saved_bytes": 338000,
      "ratio": 0.111
    }
  ]
}
```

Tools are sorted by bytes saved (largest savings first).

## Child crash handling

If the child process dies mid-session, the proxy sends a JSON-RPC error frame
(`code: -32000`) for the in-flight request and exits cleanly.  The caller
receives a descriptive error message rather than a silent hang.

## Tool name collisions

If a child tool has the same name as a built-in piggybank tool (e.g. a child
server that already has a `compress` tool), the piggybank tool is automatically
renamed with a `pb_` prefix (`pb_compress`, etc.) so there is no conflict.
