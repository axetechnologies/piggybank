#!/usr/bin/env node
const { execSync } = require("child_process");
const fs = require("fs");
const path = require("path");
const https = require("https");
const os = require("os");

const REPO = "axetechnologies/piggybank";
const BIN_DIR = path.join(__dirname, "..");
const BIN_PATH = path.join(BIN_DIR, "piggybank");

function target() {
  const platform = os.platform();
  const arch = os.arch();
  if (platform === "darwin" && arch === "arm64") return "aarch64-apple-darwin";
  if (platform === "darwin" && arch === "x64") return "x86_64-apple-darwin";
  if (platform === "linux" && arch === "x64") return "x86_64-unknown-linux-musl";
  throw new Error(`unsupported platform: ${platform}-${arch}`);
}

function latestTag() {
  const url = `https://api.github.com/repos/${REPO}/releases/latest`;
  const body = execSync(`curl -fsSL "${url}"`, { encoding: "utf8" });
  const match = body.match(/"tag_name"\s*:\s*"([^"]+)"/);
  if (!match) throw new Error("no releases found");
  return match[1];
}

function download(url, dest) {
  const tmp = dest + ".tmp.tar.gz";
  execSync(`curl -fsSL "${url}" -o "${tmp}"`);
  execSync(`tar xzf "${tmp}" -C "${path.dirname(dest)}"`);
  fs.unlinkSync(tmp);
  fs.chmodSync(dest, 0o755);
}

try {
  const t = target();
  const tag = latestTag();
  const url = `https://github.com/${REPO}/releases/download/${tag}/piggybank-${t}.tar.gz`;
  console.log(`piggybank-mcp: downloading ${tag} for ${t}...`);
  download(url, BIN_PATH);
  console.log("piggybank-mcp: installed successfully");
} catch (e) {
  console.error("piggybank-mcp: install failed:", e.message);
  process.exit(1);
}
