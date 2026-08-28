#!/usr/bin/env bash
# hooks/pre-compact-budget.sh
# PreCompact hook: inject piggybank marker-preservation guidance into the compaction prompt.
#
# Runs before Claude Code's auto-compaction. If PostToolUse compression was active during
# this session, BOOMERANG markers in the transcript encode compressed tool outputs. This
# hook tells Claude to preserve those markers verbatim in its summary so content remains
# retrievable after compaction.
#
# Claude Code passes hook data via stdin as JSON:
#   {session_id, transcript_path, hook_event_name, custom_instructions}
# To inject guidance, print {"custom_instructions": "..."} to stdout.

set -euo pipefail

PIGGYBANK="${PIGGYBANK_BIN:-$HOME/.piggybank/bin/piggybank}"

# Read the hook payload
payload=$(cat)

# Count BOOMERANG markers in the transcript to decide if guidance is needed
transcript_path=$(printf '%s' "$payload" | python3 -c \
  "import sys,json; print(json.load(sys.stdin).get('transcript_path',''))" 2>/dev/null || echo "")

marker_count=0
if [ -n "$transcript_path" ] && [ -f "$transcript_path" ]; then
    marker_count=$(grep -c "BOOMERANG:" "$transcript_path" 2>/dev/null || echo "0")
fi

# Get lifetime savings from piggybank stats
savings_note=""
if command -v "$PIGGYBANK" &>/dev/null; then
    stats_json=$("$PIGGYBANK" stats 2>/dev/null || echo "")
    if [ -n "$stats_json" ]; then
        savings_note=$(printf '%s' "$stats_json" | python3 -c "
import sys, json
try:
    d = json.load(sys.stdin)
    saved = d.get('total_saved_bytes', 0)
    calls = d.get('compress_calls', 0)
    pct = d.get('saved_pct', 0.0)
    if calls > 0:
        print(f'Piggybank compressed {calls} tool outputs, saving {saved:,} bytes ({pct:.1f}%) this session.')
except Exception:
    pass
" 2>/dev/null || echo "")
    fi
fi

# Build the custom_instructions payload
if [ "$marker_count" -gt 0 ] || [ -n "$savings_note" ]; then
    instructions="PIGGYBANK COMPRESSION ACTIVE: This conversation contains $marker_count compressed content references (BOOMERANG: markers). These markers are the ONLY handle for reconstructing large tool outputs. Rules: (1) Copy any BOOMERANG: marker VERBATIM into your summary — never paraphrase or omit them. (2) Record which tool call produced each marker so it can be correlated after compaction. (3) A marker like BOOMERANG:ELIDE:<hash> means a section was elided; 'piggybank retrieve <hash>' restores it. $savings_note"
else
    # No compression activity — pass through with no modification
    exit 0
fi

python3 -c "
import json, sys
print(json.dumps({'custom_instructions': sys.argv[1]}))
" "$instructions"
