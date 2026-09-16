#!/usr/bin/env bash
# hooks/pre-compact-budget.sh
# PreCompact hook: inject piggybank marker-preservation guidance and a
# compaction ledger index into the compaction prompt.
#
# Runs before Claude Code's auto-compaction. It:
#   1. Counts PIGGYBANK markers in the transcript (for marker-preservation guidance)
#   2. Builds a compaction ledger: stores large tool results content-addressed and
#      emits a BOOMERANG:CREF index so the post-compaction model can retrieve exact bytes.
#   3. Appends the ledger index to the custom_instructions so it survives in the summary.
#
# Claude Code passes hook data via stdin as JSON:
#   {session_id, transcript_path, hook_event_name, custom_instructions}
# To inject guidance, print {"custom_instructions": "..."} to stdout.

set -euo pipefail

PIGGYBANK="${PIGGYBANK_BIN:-$HOME/.axe/bin/piggybank}"
[ -x "$PIGGYBANK" ] || PIGGYBANK=$(command -v piggybank 2>/dev/null || echo "$HOME/.piggybank/bin/piggybank")
STORE_DIR="${PIGGYBANK_STORE_DIR:-$HOME/.piggybank/store}"
LEDGER_MIN_BYTES="${PIGGYBANK_LEDGER_MIN_BYTES:-2048}"

payload=$(cat)

transcript_path=$(printf '%s' "$payload" | python3 -c \
  "import sys,json; print(json.load(sys.stdin).get('transcript_path',''))" 2>/dev/null || echo "")

marker_count=0
ledger_index=""

if [ -n "$transcript_path" ] && [ -f "$transcript_path" ]; then
    marker_count=$(grep -c "PIGGYBANK:" "$transcript_path" 2>/dev/null || echo "0")

    # Build compaction ledger: store large tool results and emit BOOMERANG:CREF index
    if [ -x "$PIGGYBANK" ]; then
        ledger_index=$("$PIGGYBANK" ledger "$transcript_path" \
            --store-dir "$STORE_DIR" \
            --min-bytes "$LEDGER_MIN_BYTES" 2>/dev/null || echo "")
    fi
fi

# Get lifetime savings from piggybank stats
savings_note=""
if [ -x "$PIGGYBANK" ]; then
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

if [ "$marker_count" -eq 0 ] && [ -z "$ledger_index" ] && [ -z "$savings_note" ]; then
    exit 0
fi

# Build the custom_instructions payload
python3 -c "
import json, sys

marker_count = int(sys.argv[1])
savings_note = sys.argv[2]
ledger_index = sys.argv[3]

parts = []

if marker_count > 0 or savings_note:
    parts.append(
        f'PIGGYBANK COMPRESSION ACTIVE: This conversation contains {marker_count} compressed '
        f'content references (PIGGYBANK: markers). These markers are the ONLY handle for '
        f'reconstructing large tool outputs. Rules: (1) Copy any PIGGYBANK: marker VERBATIM '
        f'into your summary — never paraphrase or omit them. (2) Record which tool call '
        f'produced each marker so it can be correlated after compaction. '
        f'(3) A marker like PIGGYBANK:ELIDE:<hash> means a section was elided; '
        f'\"piggybank retrieve <hash>\" restores it. {savings_note}'
    )

if ledger_index:
    parts.append(
        'COMPACTION LEDGER — large tool results stored for retrieval after compaction. '
        'Copy these BOOMERANG:CREF lines verbatim into your summary:\n' + ledger_index
    )

instructions = '\n\n'.join(parts)
print(json.dumps({'custom_instructions': instructions}))
" "$marker_count" "$savings_note" "$ledger_index"
