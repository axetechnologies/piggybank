#!/usr/bin/env bash
# hooks/post-tool-use-compress.sh
# PostToolUse hook: compress large tool outputs before they enter the context window.
#
# Fires for ALL tools (Bash, Read, Grep, Glob, WebFetch, WebSearch, Agent,
# mcp__* tools, and any future tool) with per-tool byte thresholds and
# allow/deny control via environment variables.
#
# Environment variables:
#   PIGGYBANK_BIN               path to piggybank binary
#   PIGGYBANK_STORE_DIR         store directory (default ~/.piggybank/store)
#   PIGGYBANK_MIN_BYTES         default minimum bytes to compress (default 2048)
#   PIGGYBANK_HOOK_THRESHOLD    alias for PIGGYBANK_MIN_BYTES (legacy compat)
#   PIGGYBANK_FIRST_SIGHT_THRESHOLD   Bash-only first-sight elision threshold (default 16384)
#   PIGGYBANK_TOOLS             comma-separated allowlist of tool names (default: all)
#   PIGGYBANK_SKIP_TOOLS        comma-separated denylist of tool names (default: none)
#   PIGGYBANK_TOOL_THRESHOLD_<TOOLNAME>  per-tool byte threshold override
#                               (e.g. PIGGYBANK_TOOL_THRESHOLD_GREP=1024)
#                               TOOLNAME is uppercased with non-alphanumeric chars -> _
#
# Contract (Claude Code PostToolUse hook): replacement goes to stdout as:
#   {"hookSpecificOutput": {"hookEventName": "PostToolUse", "updatedToolOutput": <native shape>}}
#
# Native shapes handled:
#   Bash:          {"stdout", "stderr", "interrupted", "isImage"}
#   Read:          {"type": "text", "file": {"filePath", "content", "numLines", ...}}
#   Grep/Glob:     plain string or {"output": ...}
#   WebFetch:      {"content": ...} or plain string
#   WebSearch:     list of result objects (serialised as JSON)
#   Agent:         any shape (subagent report) — serialised as JSON
#   mcp__*:        any shape — serialised as JSON
#   Unknown:       serialised as JSON (fallback)
#
# hook-savings.jsonl format: {"d":"YYYY-MM-DD","saved":N,"tool":"ToolName"}

set -euo pipefail

PIGGYBANK="${PIGGYBANK_BIN:-$HOME/.axe/bin/piggybank}"
[ -x "$PIGGYBANK" ] || PIGGYBANK=$(command -v piggybank 2>/dev/null || echo "$HOME/.piggybank/bin/piggybank")
STORE_DIR="${PIGGYBANK_STORE_DIR:-$HOME/.piggybank/store}"

# Default threshold: PIGGYBANK_MIN_BYTES, fallback to legacy PIGGYBANK_HOOK_THRESHOLD, then 2048
DEFAULT_THRESHOLD="${PIGGYBANK_MIN_BYTES:-${PIGGYBANK_HOOK_THRESHOLD:-2048}}"
FS_THRESHOLD="${PIGGYBANK_FIRST_SIGHT_THRESHOLD:-16384}"

payload=$(cat)

# Extract tool_name
tool_name=$(printf '%s' "$payload" | python3 -c \
  "import sys,json; print(json.load(sys.stdin).get('tool_name',''))" 2>/dev/null || echo "")

[ -n "$tool_name" ] || exit 0

# Allowlist check: PIGGYBANK_TOOLS is comma-separated list; if set, only listed tools run
if [ -n "${PIGGYBANK_TOOLS:-}" ]; then
    allowed=0
    IFS=',' read -ra tlist <<< "$PIGGYBANK_TOOLS"
    for t in "${tlist[@]}"; do
        if [ "$t" = "$tool_name" ]; then allowed=1; break; fi
    done
    [ "$allowed" -eq 1 ] || exit 0
fi

# Denylist check: PIGGYBANK_SKIP_TOOLS is comma-separated list; if tool is listed, skip
if [ -n "${PIGGYBANK_SKIP_TOOLS:-}" ]; then
    IFS=',' read -ra slist <<< "$PIGGYBANK_SKIP_TOOLS"
    for t in "${slist[@]}"; do
        if [ "$t" = "$tool_name" ]; then exit 0; fi
    done
fi

# Determine per-tool threshold via PIGGYBANK_TOOL_THRESHOLD_<NORMALIZED_TOOLNAME>
# Normalize: uppercase, non-alphanumeric -> underscore
tool_env_key=$(printf '%s' "$tool_name" | tr '[:lower:]' '[:upper:]' | tr -c 'A-Z0-9' '_')
threshold_var="PIGGYBANK_TOOL_THRESHOLD_${tool_env_key}"
threshold="${!threshold_var:-$DEFAULT_THRESHOLD}"

# Extract the text content from tool_response, serialising non-text shapes as JSON
output=$(printf '%s' "$payload" | python3 -c "
import sys, json

d = json.load(sys.stdin)
tool = d.get('tool_name', '')
r = d.get('tool_response', {})

def extract_text(tool, r):
    # Read: nested file.content
    if tool == 'Read' and isinstance(r, dict):
        return (r.get('file') or {}).get('content') or ''
    # Bash: stdout field
    if tool == 'Bash' and isinstance(r, dict):
        text = r.get('stdout') or r.get('output') or ''
        if isinstance(text, list):
            text = ' '.join(str(x.get('text', x)) if isinstance(x, dict) else str(x) for x in text)
        return text
    # WebFetch / generic dict with common text fields
    if isinstance(r, dict):
        for key in ('content', 'output', 'text', 'stdout', 'result'):
            val = r.get(key)
            if isinstance(val, str) and val:
                return val
            if isinstance(val, list):
                return json.dumps(val)
        # No known text field — serialise whole response as JSON
        return json.dumps(r)
    # Plain string
    if isinstance(r, str):
        return r
    # List (e.g. WebSearch results) or anything else
    return json.dumps(r)

print(extract_text(tool, r))
" 2>/dev/null || echo "")

byte_count=${#output}
if [ "$byte_count" -le "$threshold" ]; then
    exit 0
fi

# Build session-scoped dedup key
key=$(printf '%s' "$payload" | python3 -c "
import sys, json
d = json.load(sys.stdin)
inp = d.get('tool_input', {}) or {}
base = ''
if isinstance(inp, dict):
    base = (inp.get('command') or inp.get('file_path') or inp.get('url')
            or inp.get('pattern') or inp.get('query') or inp.get('prompt') or '')
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

# First-sight large Bash output: dedup+elision even without a session match
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

# Emit replacement output in the tool's native shape
printf '%s' "$payload" | python3 -c "
import json, sys

header = sys.argv[1]
body = sys.argv[2]
d = json.load(sys.stdin)
tool = d.get('tool_name', '')
r = d.get('tool_response', {})

def build_updated(tool, r, header, body):
    if tool == 'Read' and isinstance(r, dict):
        f = dict(r.get('file') or {})
        f['content'] = header + '\n' + body
        return {'type': 'text', 'file': f}
    if tool == 'Bash' and isinstance(r, dict):
        return {
            'stdout': header + '\n' + body,
            'stderr': (r.get('stderr') or '') if isinstance(r, dict) else '',
            'interrupted': bool(r.get('interrupted')) if isinstance(r, dict) else False,
            'isImage': bool(r.get('isImage')) if isinstance(r, dict) else False,
        }
    # For dict responses with a known text field, update that field
    if isinstance(r, dict):
        updated = dict(r)
        for key in ('content', 'output', 'text', 'stdout', 'result'):
            if key in updated and isinstance(updated[key], str):
                updated[key] = header + '\n' + body
                return updated
        # No known field found — return a generic wrapper
        return {'_compressed': True, 'content': header + '\n' + body}
    # String or list: return generic wrapper
    return {'_compressed': True, 'content': header + '\n' + body}

updated = build_updated(tool, r, header, body)
print(json.dumps({'hookSpecificOutput': {
    'hookEventName': 'PostToolUse',
    'updatedToolOutput': updated,
}}))
" "$header" "$compressed"
