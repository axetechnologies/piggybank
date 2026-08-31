#!/usr/bin/env bash
# hooks/post-tool-use-compress.sh
# PostToolUse hook: compress large tool outputs before they enter the context window.
#
# Contract (code.claude.com/docs/en/hooks): replacement goes in
#   {"hookSpecificOutput": {"hookEventName": "PostToolUse", "updatedToolOutput": <tool-native shape>}}
# For Bash the native shape is {"stdout", "stderr", "interrupted", "isImage"}.
# Currently rewrites Bash only; other tools pass through until their shapes are verified.
#
# Threshold: PIGGYBANK_HOOK_THRESHOLD bytes (default 4096).

set -euo pipefail

PIGGYBANK="${PIGGYBANK_BIN:-$HOME/.axe/bin/piggybank}"
[ -x "$PIGGYBANK" ] || PIGGYBANK=$(command -v piggybank || echo "$HOME/.piggybank/bin/piggybank")
STORE_DIR="${PIGGYBANK_STORE_DIR:-$HOME/.piggybank/store}"
THRESHOLD="${PIGGYBANK_HOOK_THRESHOLD:-4096}"

payload=$(cat)
echo "$(date +%T) invoked bytes=${#payload}" >> /tmp/piggybank-hook-invocations.log || true

tool_name=$(printf '%s' "$payload" | python3 -c \
  "import sys,json; print(json.load(sys.stdin).get('tool_name',''))" 2>/dev/null || echo "")

# Only Bash rewriting is verified against the harness contract so far
[ "$tool_name" = "Bash" ] || exit 0

output=$(printf '%s' "$payload" | python3 -c "
import sys, json
d = json.load(sys.stdin)
r = d.get('tool_response', {})
if isinstance(r, dict):
    text = (r.get('stdout') or r.get('output') or r.get('content') or r.get('text') or '')
    if isinstance(text, list):
        text = ' '.join(str(x.get('text', x)) if isinstance(x, dict) else str(x) for x in text)
    print(text)
elif isinstance(r, str):
    print(r)
else:
    print('')
" 2>/dev/null || echo "")

byte_count=${#output}
if [ "$byte_count" -le "$THRESHOLD" ]; then
    exit 0
fi

key=$(printf '%s' "$payload" | python3 -c "
import sys, json
inp = json.load(sys.stdin).get('tool_input', {})
print(inp.get('command') or inp.get('file_path') or inp.get('url') or inp.get('pattern') or '')
" 2>/dev/null || echo "")

tmp=$(mktemp "${TMPDIR:-/tmp}/piggybank-hook-XXXXXX")
trap 'rm -f "$tmp"' EXIT
printf '%s' "$output" > "$tmp"

if [ -n "$key" ] && [ "${#key}" -le 512 ]; then
    compressed=$("$PIGGYBANK" compress-session "$key" "$tmp" "$STORE_DIR" 2>/dev/null || echo "")
else
    compressed=$("$PIGGYBANK" compress-log "$tmp" "$STORE_DIR" 2>/dev/null || echo "")
fi

# Fall through if compression failed or did not save meaningful space
if [ -z "$compressed" ] || [ "${#compressed}" -ge "$byte_count" ]; then
    exit 0
fi

saved=$((byte_count - ${#compressed}))
echo "$(date +%T) rewrote ${byte_count}->${#compressed}" >> /tmp/piggybank-hook-invocations.log || true

python3 -c "
import json, sys
header, body = sys.argv[1], sys.argv[2]
print(json.dumps({
  'hookSpecificOutput': {
    'hookEventName': 'PostToolUse',
    'updatedToolOutput': {
      'stdout': header + '\n' + body,
      'stderr': '',
      'interrupted': False,
      'isImage': False
    }
  }
}))
" "[piggybank: ${byte_count} -> ${#compressed} bytes, saved ${saved}B. Full output stored; 'piggybank decompress-session' or the retrieve MCP tool restores it.]" "$compressed"
