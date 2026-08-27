#!/usr/bin/env bash
set -euo pipefail

REPO="memjar/piggybank"
INSTALL_DIR="${PIGGYBANK_DIR:-$HOME/.piggybank}"
BIN_DIR="$INSTALL_DIR/bin"
STORE_DIR="$INSTALL_DIR/store"
CLAUDE_JSON="$HOME/.claude.json"

info()  { printf '\033[1;34m→\033[0m %s\n' "$*"; }
ok()    { printf '\033[1;32m✓\033[0m %s\n' "$*"; }
fail()  { printf '\033[1;31m✗\033[0m %s\n' "$*" >&2; exit 1; }

detect_target() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$os" in
    Darwin)
      case "$arch" in
        arm64|aarch64) echo "aarch64-apple-darwin" ;;
        x86_64)        echo "x86_64-apple-darwin" ;;
        *) fail "unsupported macOS arch: $arch" ;;
      esac ;;
    Linux)
      case "$arch" in
        x86_64) echo "x86_64-unknown-linux-musl" ;;
        *) fail "unsupported Linux arch: $arch" ;;
      esac ;;
    *) fail "unsupported OS: $os" ;;
  esac
}

latest_tag() {
  curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
    | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"//;s/".*//'
}

install_binary() {
  local target tag url tmp
  target="$(detect_target)"
  info "detected target: $target"

  tag="$(latest_tag 2>/dev/null || true)"
  if [ -z "$tag" ]; then
    fail "no releases found — ask the repo maintainer to tag a release (git tag v0.1.0 && git push --tags)"
  fi
  info "latest release: $tag"

  url="https://github.com/$REPO/releases/download/$tag/piggybank-${target}.tar.gz"
  info "downloading $url"

  mkdir -p "$BIN_DIR" "$STORE_DIR"
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT

  curl -fsSL "$url" | tar xz -C "$tmp"
  mv "$tmp/piggybank" "$BIN_DIR/piggybank"
  chmod +x "$BIN_DIR/piggybank"
  ok "installed $BIN_DIR/piggybank"
}

configure_claude() {
  local bin_path="$BIN_DIR/piggybank"
  local mcp_entry

  mcp_entry=$(cat <<EOF
{
  "command": "$bin_path",
  "args": ["mcp", "serve", "--store-dir", "$STORE_DIR"],
  "type": "stdio",
  "env": {}
}
EOF
)

  if [ ! -f "$CLAUDE_JSON" ]; then
    printf '{"mcpServers":{"piggybank":%s}}\n' "$mcp_entry" > "$CLAUDE_JSON"
    ok "created $CLAUDE_JSON with piggybank MCP"
    return
  fi

  if command -v python3 >/dev/null 2>&1; then
    python3 -c "
import json, sys
with open('$CLAUDE_JSON') as f:
    cfg = json.load(f)
cfg.setdefault('mcpServers', {})
cfg['mcpServers']['piggybank'] = json.loads('''$mcp_entry''')
with open('$CLAUDE_JSON', 'w') as f:
    json.dump(cfg, f, indent=2)
    f.write('\n')
print('updated $CLAUDE_JSON')
"
    ok "added piggybank MCP to $CLAUDE_JSON"
  else
    info "python3 not found — add this to $CLAUDE_JSON manually under mcpServers:"
    echo "$mcp_entry"
  fi
}

info "installing piggybank — context compression for AI agents"
install_binary
configure_claude
echo ""
ok "done! restart Claude Code to activate piggybank MCP"
echo "   tools: compress, decompress, retrieve, stats, verify, changed, compress_append, compress_budget"
echo "   store: $STORE_DIR"
