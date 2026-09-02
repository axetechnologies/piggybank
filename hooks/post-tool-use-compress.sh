#!/usr/bin/env bash
# hooks/post-tool-use-compress.sh
# PostToolUse hook: compress large tool outputs before they enter the context window.
#
# Contract (code.claude.com/docs/en/hooks): replacement goes in
#   {"hookSpecificOutput": {"hookEventName": "PostToolUse", "updatedToolOutput": <tool-native shape>}}
# Native shapes handled here (captured from live payloads):
#   Bash: {"stdout", "stderr", "interrupted", "isImage"}
#   Read: {"type": "text", "file": {"filePath", "content", "numLines", "startLine", "totalLines"}}
# Grep/WebFetch pass through until their native shapes are captured and verified.
#
# Compression policy (subagent-burn aware):
#   - Keys are SESSION-SCOPED. The store is shared across sessions and subagents; an
#     unscoped key would hand session B an "UNCHANGED" ref for content only session A saw.
#   - Second sight within a session (re-read file, re-run command) -> diff/UNCHANGED view.
#   - Bash first sight larger than PIGGYBANK_FIRST_SIGHT_THRESHOLD -> dedup+elision view
#     (head/tail kept, middle elided behind a retrieve ref). Read is NEVER compressed on
#     first sight: the model needs the content it asked for.
#   - Every rewrite appends {"d": "...", "saved": N} to $STORE_DIR/hook-savings.jsonl,
#     which `piggybank statusline` aggregates.
#
# Env: PIGGYBANK_BIN, PIGGYBANK_STORE_DIR, PIGGYBANK_HOOK_THRESHOLD (default 4096),
#      PIGGYBANK_FIRST_SIGHT_THRESHOLD (default 16384).

set -euo pipefail

PIGGYBANK="${PIGGYBANK_BIN:-$HOME/.axe/bin/piggybank}"
[ -x "$PIGGYBANK" ] || PIGGYBANK=$(command -v piggybank || echo "$HOME/.piggybank/bin/piggybank")
STORE_DIR="${PIGGYBANK_STORE_DIR:-$HOME/.piggybank/store}"
THRESHOLD="${PIGGYBANK_HOOK_THRESHOLD:-4096}"
FS_THRESHOLD="${PIGGYBANK_FIRST_SIGHT_THRESHOLD:-16384}"

payload=$(cat)

tool_name=$(printf '%s' "$payload" | python3 -c \
  "import sys,json; print(json.load(sys.stdin).get('tool_name',''))" 2>/dev/null || echo "")

case "$tool_name" in
    Bash|Read) ;;
    *) exit 0 ;;
esac

output=$(printf '%s' "$payload" | python3 -c "
import sys, json
d = json.load(sys.stdin)
r = d.get('tool_response', {})
tool = d.get('tool_name', '')
text = ''
if tool == 'Read' and isinstance(r, dict):
    text = (r.get('file') or {}).get('content') or ''
elif isinstance(r, dict):
    text = r.get('stdout') or r.get('output') or r.get('content') or r.get('text') or ''
    if isinstance(text, list):
        text = ' '.join(str(x.get('text', x)) if isinstance(x, dict) else str(x) for x in text)
elif isinstance(r, str):
    text = r
print(text)
" 2>/dev/null || echo "")

byte_count=${#output}
if [ "$byte_count" -le "$THRESHOLD" ]; then
    exit 0
fi

key=$(printf '%s' "$payload" | python3 -c "
import sys, json
d = json.load(sys.stdin)
inp = d.get('tool_input', {})
base = inp.get('command') or inp.get('file_path') or inp.get('url') or inp.get('pattern') or ''
sid = d.get('session_id') or 'nosession'
print(f'{sid}:{base}' if base else '')
" 2>/dev/null || echo "")

tmp=$(mktemp "${TMPDIR:-/tmp}/piggybank-hook-XXXXXX")
trap 'rm -f "$tmp"' EXIT
printf '%s' "$output" > "$tmp"

compressed=""
if [ -n "$key" ] && [ "${#key}" -le 600 ]; then
    compressed=$("$PIGGYBANK" compress-session "$key" "$tmp" "$STORE_DIR" 2>/dev/null || echo "")
fi

# First sight (or no session savings): big Bash outputs still get dedup+elision.
if { [ -z "$compressed" ] || [ "${#compressed}" -ge "$byte_count" ]; } \
   && [ "$tool_name" = "Bash" ] && [ "$byte_count" -gt "$FS_THRESHOLD" ]; then
    compressed=$("$PIGGYBANK" compress-log "$tmp" "$STORE_DIR" 2>/dev/null || echo "")
fi

if [ -z "$compressed" ] || [ "${#compressed}" -ge "$byte_count" ]; then
    exit 0
fi

saved=$((byte_count - ${#compressed}))
printf '{"d":"%s","saved":%s,"tool":"%s"}\n' "$(date +%F)" "$saved" "$tool_name" \
    >> "$STORE_DIR/hook-savings.jsonl" 2>/dev/null || true

header="[piggybank compressed: ${byte_count}->${#compressed}B, ref stored]"

printf '%s' "$payload" | python3 -c "
import json, sys
header, body = sys.argv[1], sys.argv[2]
d = json.load(sys.stdin)
tool = d.get('tool_name', '')
r = d.get('tool_response', {})
if tool == 'Read':
    f = dict(r.get('file') or {})
    f['content'] = header + '\n' + body
    updated = {'type': 'text', 'file': f}
else:
    updated = {
        'stdout': header + '\n' + body,
        'stderr': (r.get('stderr') or '') if isinstance(r, dict) else '',
        'interrupted': bool(r.get('interrupted')) if isinstance(r, dict) else False,
        'isImage': bool(r.get('isImage')) if isinstance(r, dict) else False,
    }
print(json.dumps({'hookSpecificOutput': {
    'hookEventName': 'PostToolUse',
    'updatedToolOutput': updated,
}}))
" "$header" "$compressed"
