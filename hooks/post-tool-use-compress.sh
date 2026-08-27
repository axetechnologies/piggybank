#!/usr/bin/env bash
# hooks/post-tool-use-compress.sh
# PostToolUse hook: compress large tool outputs before they enter the context window.
#
# Triggers for: Bash, Read, Grep, WebFetch
# Threshold: PIGGYBANK_HOOK_THRESHOLD bytes (default 100000, matching BASH_MAX_OUTPUT_LENGTH)
#
# Claude Code passes hook data via stdin as JSON:
#   {session_id, transcript_path, hook_event_name, tool_name, tool_input, tool_response}
# To replace the tool output, print {"output": "..."} to stdout and exit 0.

set -euo pipefail

PIGGYBANK="${PIGGYBANK_BIN:-$HOME/.piggybank/bin/piggybank}"
STORE_DIR="${PIGGYBANK_STORE_DIR:-$HOME/.piggybank/store}"
THRESHOLD="${PIGGYBANK_HOOK_THRESHOLD:-100000}"

# Read full JSON payload from stdin
payload=$(cat)

# Extract tool name — fast-path exit if not a large-output tool
tool_name=$(printf '%s' "$payload" | python3 -c \
  "import sys,json; print(json.load(sys.stdin).get('tool_name',''))" 2>/dev/null || echo "")

case "$tool_name" in
    Bash|Read|Grep|WebFetch) ;;
    *) exit 0 ;;
esac

# Extract the output text from tool_response (handles string or dict response)
output=$(printf '%s' "$payload" | python3 -c "
import sys, json
d = json.load(sys.stdin)
r = d.get('tool_response', {})
if isinstance(r, dict):
    text = (r.get('output') or r.get('content') or r.get('text') or '')
    if isinstance(text, list):
        text = ' '.join(str(x.get('text', x)) if isinstance(x, dict) else str(x) for x in text)
    print(text)
elif isinstance(r, str):
    print(r)
else:
    print('')
" 2>/dev/null || echo "")

# Threshold check — exit 0 passes through unchanged
byte_count=${#output}
if [ "$byte_count" -le "$THRESHOLD" ]; then
    exit 0
fi

# Derive a stable session key from the tool input (command / file path / url / pattern)
key=$(printf '%s' "$payload" | python3 -c "
import sys, json
inp = json.load(sys.stdin).get('tool_input', {})
print(inp.get('command') or inp.get('file_path') or inp.get('url') or inp.get('pattern') or '')
" 2>/dev/null || echo "")

# Write to tempfile and compress
tmp=$(mktemp "${TMPDIR:-/tmp}/piggybank-hook-XXXXXX")
trap 'rm -f "$tmp"' EXIT
printf '%s' "$output" > "$tmp"

if [ -n "$key" ] && [ "${#key}" -le 512 ]; then
    compressed=$("$PIGGYBANK" compress-session "$key" "$tmp" "$STORE_DIR" 2>/dev/null || echo "")
else
    compressed=$("$PIGGYBANK" compress-log "$tmp" "$STORE_DIR" 2>/dev/null || echo "")
fi

# Fall through if compression failed or did not save space
if [ -z "$compressed" ] || [ "${#compressed}" -ge "$byte_count" ]; then
    exit 0
fi

saved=$((byte_count - ${#compressed}))
header="[piggybank: ${byte_count} -> ${#compressed} bytes, saved ${saved}B. Use 'piggybank decompress' to restore.]"

# Print replacement output as JSON and exit 0
python3 -c "
import json, sys
header, body = sys.argv[1], sys.argv[2]
print(json.dumps({'output': header + '\n' + body}))
" "$header" "$compressed"
