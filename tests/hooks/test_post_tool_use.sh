#!/usr/bin/env bash
# tests/hooks/test_post_tool_use.sh
# Integration tests for hooks/post-tool-use-compress.sh
#
# Requires: piggybank binary on PATH or PIGGYBANK_BIN set
# Runs standalone; exit 0 = pass, non-zero = fail

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
HOOK="$REPO_ROOT/hooks/post-tool-use-compress.sh"
PIGGYBANK="${PIGGYBANK_BIN:-$HOME/.axe/bin/piggybank}"
[ -x "$PIGGYBANK" ] || PIGGYBANK=$(command -v piggybank 2>/dev/null || echo "")

STORE_DIR=$(mktemp -d)
trap 'rm -rf "$STORE_DIR"' EXIT

PASS=0
FAIL=0

ok() { echo "PASS: $1"; PASS=$((PASS + 1)); }
fail() { echo "FAIL: $1"; FAIL=$((FAIL + 1)); }

# Run the hook with controlled env vars
run_hook() {
    local min_bytes="${1:-2048}"
    shift
    PIGGYBANK_BIN="$PIGGYBANK" \
    PIGGYBANK_STORE_DIR="$STORE_DIR" \
    PIGGYBANK_MIN_BYTES="$min_bytes" \
    PIGGYBANK_FIRST_SIGHT_THRESHOLD="$min_bytes" \
    "$@" \
    bash "$HOOK"
}

# Build a PostToolUse JSON payload
make_bash_payload() {
    local stdout="$1" session="${2:-test-session}"
    python3 -c "
import json, sys
print(json.dumps({
    'tool_name': 'Bash',
    'tool_input': {'command': 'test-cmd'},
    'tool_response': {'stdout': sys.argv[1], 'stderr': '', 'interrupted': False, 'isImage': False},
    'session_id': sys.argv[2],
}))
" "$stdout" "$session"
}

make_payload() {
    local tool="$1" response_json="$2" session="${3:-test-session}" input_key="${4:-test-cmd}"
    python3 -c "
import json, sys
print(json.dumps({
    'tool_name': sys.argv[1],
    'tool_input': {'command': sys.argv[4]},
    'tool_response': json.loads(sys.argv[2]),
    'session_id': sys.argv[3],
}))
" "$tool" "$response_json" "$session" "$input_key"
}

BIG=$(python3 -c "print('data line content here\n' * 400, end='')")  # ~9600 bytes

# ─── Test 1: Bash output below threshold passes through (exits 0, no stdout) ───
small=$(python3 -c "print('x' * 50, end='')")
result=$(make_bash_payload "$small" | run_hook 2048 || true)
if [ -z "$result" ]; then
    ok "Bash below threshold: passthrough"
else
    fail "Bash below threshold: unexpected output: ${result:0:100}"
fi

# ─── Test 2: Bash output above threshold (first sight with low FS_THRESHOLD) ───
if [ -x "$PIGGYBANK" ]; then
    result=$(make_bash_payload "$BIG" "bash-test-2" | run_hook 100 || true)
    if printf '%s' "$result" | python3 -c "import sys,json; d=json.load(sys.stdin); assert 'hookSpecificOutput' in d" 2>/dev/null; then
        ok "Bash above threshold: returns hookSpecificOutput"
    else
        # Try second call (session dedup path)
        make_bash_payload "$BIG" "bash-test-2-sess" | run_hook 100 > /dev/null 2>&1 || true
        result2=$(make_bash_payload "$BIG" "bash-test-2-sess" | run_hook 100 || true)
        if printf '%s' "$result2" | python3 -c "import sys,json; d=json.load(sys.stdin); assert 'hookSpecificOutput' in d" 2>/dev/null; then
            ok "Bash above threshold (session dedup): returns hookSpecificOutput"
        else
            fail "Bash above threshold: got: ${result:0:200}"
        fi
    fi
else
    echo "SKIP: Bash compression (no piggybank binary)"
fi

# ─── Test 3: Read tool ───
if [ -x "$PIGGYBANK" ]; then
    read_payload=$(python3 -c "
import json
content = 'file line content\n' * 400
print(json.dumps({
    'tool_name': 'Read',
    'tool_input': {'file_path': '/tmp/test.txt'},
    'tool_response': {'type': 'text', 'file': {
        'filePath': '/tmp/test.txt',
        'content': content,
        'numLines': 400,
        'startLine': 1,
        'totalLines': 400,
    }},
    'session_id': 'read-session-3a',
}))
")
    # First call stores; second should dedup
    printf '%s' "$read_payload" | run_hook 100 > /dev/null 2>&1 || true
    result=$(printf '%s' "$read_payload" | run_hook 100 || true)
    if printf '%s' "$result" | python3 -c "
import sys,json
d=json.load(sys.stdin)
out=d['hookSpecificOutput']['updatedToolOutput']
assert out.get('type')=='text', f'type missing: {out}'
assert 'file' in out
" 2>/dev/null; then
        ok "Read tool: compressed with file shape preserved"
    else
        fail "Read tool: ${result:0:300}"
    fi
fi

# ─── Test 4: Grep tool ───
if [ -x "$PIGGYBANK" ]; then
    grep_content=$(python3 -c "print(('match: file.rs:1:  ' + 'x'*40 + '\n') * 100, end='')")
    grep_payload=$(python3 -c "
import json, sys
print(json.dumps({
    'tool_name': 'Grep',
    'tool_input': {'pattern': 'x+', 'path': '.'},
    'tool_response': sys.argv[1],
    'session_id': 'grep-session-4',
}))
" "$grep_content")
    # First call: store; second call: dedup ref
    printf '%s' "$grep_payload" | run_hook 100 > /dev/null 2>&1 || true
    result=$(printf '%s' "$grep_payload" | run_hook 100 || true)
    if printf '%s' "$result" | python3 -c "import sys,json; d=json.load(sys.stdin); assert 'hookSpecificOutput' in d" 2>/dev/null; then
        ok "Grep tool: returns hookSpecificOutput"
    else
        fail "Grep tool: ${result:0:200}"
    fi
fi

# ─── Test 5: mcp__ tool (JSON object response) ───
if [ -x "$PIGGYBANK" ]; then
    mcp_content=$(python3 -c "
import json
data = {'results': [{'id': i, 'text': 'x'*100} for i in range(30)]}
print(json.dumps(data))
")
    mcp_payload=$(python3 -c "
import json, sys
print(json.dumps({
    'tool_name': 'mcp__some_server__some_tool',
    'tool_input': {'query': 'test'},
    'tool_response': json.loads(sys.argv[1]),
    'session_id': 'mcp-session-5',
}))
" "$mcp_content")
    # First call: store; second call: dedup ref
    printf '%s' "$mcp_payload" | run_hook 100 > /dev/null 2>&1 || true
    result=$(printf '%s' "$mcp_payload" | run_hook 100 || true)
    if printf '%s' "$result" | python3 -c "import sys,json; d=json.load(sys.stdin); assert 'hookSpecificOutput' in d" 2>/dev/null; then
        ok "mcp__ tool: returns hookSpecificOutput with JSON serialisation"
    else
        fail "mcp__ tool: ${result:0:200}"
    fi
fi

# ─── Test 6: Agent tool (subagent report as string) ───
if [ -x "$PIGGYBANK" ]; then
    agent_content=$(python3 -c "print('Agent completed task. Details: ' + 'x'*100 + '\n', end='') ; print('Result: ' + 'y'*100, end='')")
    agent_content2=$(python3 - << 'PYEOF'
print('Agent completed task. Details: ' + 'x'*100 + '\n', end='')
print('Result: ' + 'y'*100, end='')
PYEOF
)
    agent_payload=$(python3 -c "
import json, sys
print(json.dumps({
    'tool_name': 'Agent',
    'tool_input': {'description': 'do something', 'prompt': 'test'},
    'tool_response': sys.argv[1],
    'session_id': 'agent-session-6',
}))
" "$agent_content2")
    printf '%s' "$agent_payload" | run_hook 100 > /dev/null 2>&1 || true
    result=$(printf '%s' "$agent_payload" | run_hook 100 || true)
    if printf '%s' "$result" | python3 -c "import sys,json; d=json.load(sys.stdin); assert 'hookSpecificOutput' in d" 2>/dev/null; then
        ok "Agent tool: returns hookSpecificOutput"
    else
        fail "Agent tool: ${result:0:200}"
    fi
fi

# ─── Test 7: PIGGYBANK_SKIP_TOOLS denylist ───
bash_large_payload=$(make_bash_payload "$BIG" "deny-session-7")
result=$(printf '%s' "$bash_large_payload" | \
    PIGGYBANK_BIN="$PIGGYBANK" PIGGYBANK_STORE_DIR="$STORE_DIR" \
    PIGGYBANK_MIN_BYTES=100 PIGGYBANK_FIRST_SIGHT_THRESHOLD=100 \
    PIGGYBANK_SKIP_TOOLS="Bash,Read" bash "$HOOK" || true)
if [ -z "$result" ]; then
    ok "PIGGYBANK_SKIP_TOOLS: Bash skipped"
else
    fail "PIGGYBANK_SKIP_TOOLS: Bash should be skipped: ${result:0:100}"
fi

# ─── Test 8: PIGGYBANK_TOOLS allowlist ───
grep_large=$(python3 -c "print('x'*5000, end='')")
grep_large_payload=$(make_payload "Grep" "$(python3 -c "import json; print(json.dumps('x'*5000))")" "allow-session-8")
result=$(printf '%s' "$grep_large_payload" | \
    PIGGYBANK_BIN="$PIGGYBANK" PIGGYBANK_STORE_DIR="$STORE_DIR" \
    PIGGYBANK_MIN_BYTES=100 PIGGYBANK_TOOLS="Bash,Read" bash "$HOOK" || true)
if [ -z "$result" ]; then
    ok "PIGGYBANK_TOOLS allowlist: Grep excluded when not in list"
else
    fail "PIGGYBANK_TOOLS allowlist: Grep should be excluded: ${result:0:100}"
fi

# ─── Test 9: Per-tool threshold override ───
grep_small=$(python3 -c "print('x'*500, end='')")
grep_small_payload=$(make_payload "Grep" "$(python3 -c "import json; print(json.dumps('x'*500))")" "thresh-session-9a")
# Prime the session
printf '%s' "$grep_small_payload" | \
    PIGGYBANK_BIN="$PIGGYBANK" PIGGYBANK_STORE_DIR="$STORE_DIR" \
    PIGGYBANK_MIN_BYTES=2048 PIGGYBANK_TOOL_THRESHOLD_GREP=100 \
    bash "$HOOK" > /dev/null 2>&1 || true
result=$(printf '%s' "$grep_small_payload" | \
    PIGGYBANK_BIN="$PIGGYBANK" PIGGYBANK_STORE_DIR="$STORE_DIR" \
    PIGGYBANK_MIN_BYTES=2048 PIGGYBANK_TOOL_THRESHOLD_GREP=100 \
    bash "$HOOK" || true)
if [ -x "$PIGGYBANK" ]; then
    if printf '%s' "$result" | python3 -c "import sys,json; d=json.load(sys.stdin); assert 'hookSpecificOutput' in d" 2>/dev/null; then
        ok "Per-tool threshold: Grep compressed at 100B override"
    else
        fail "Per-tool threshold: expected compression for 500B Grep: ${result:0:200}"
    fi
fi

# ─── Test 10: savings.jsonl appended ───
if [ -x "$PIGGYBANK" ]; then
    big_bash=$(python3 -c "print('data line content here\n' * 500, end='')")
    savings_payload=$(python3 -c "
import json, sys
print(json.dumps({
    'tool_name': 'Bash',
    'tool_input': {'command': 'savings-test'},
    'tool_response': {'stdout': sys.argv[1], 'stderr': '', 'interrupted': False, 'isImage': False},
    'session_id': 'savings-test-session-10a',
}))
" "$big_bash")
    # Prime session
    printf '%s' "$savings_payload" | run_hook 100 > /dev/null 2>&1 || true
    printf '%s' "$savings_payload" | run_hook 100 > /dev/null 2>&1 || true
    if [ -f "$STORE_DIR/hook-savings.jsonl" ] && grep -q '"tool":"Bash"' "$STORE_DIR/hook-savings.jsonl"; then
        ok "hook-savings.jsonl: entry written with tool field"
    else
        fail "hook-savings.jsonl: missing or wrong entry"
    fi
fi

# ─── Summary ───
echo ""
echo "Results: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
