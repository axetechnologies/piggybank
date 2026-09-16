#!/usr/bin/env node
const { execSync } = require("child_process");
const fs = require("fs");
const path = require("path");
const https = require("https");
const os = require("os");

const REPO = "axetechnologies/piggybank";
const BIN_DIR = path.join(__dirname, "..");
const BIN_PATH = path.join(BIN_DIR, "piggybank");
const PKG_VERSION = require(path.join(BIN_DIR, "package.json")).version;

function target() {
  const platform = os.platform();
  const arch = os.arch();
  if (platform === "darwin" && arch === "arm64") return "aarch64-apple-darwin";
  if (platform === "darwin" && arch === "x64") return "x86_64-apple-darwin";
  if (platform === "linux" && arch === "x64") return "x86_64-unknown-linux-musl";
  if (platform === "linux" && arch === "arm64") return "aarch64-unknown-linux-musl";
  throw new Error(`unsupported platform: ${platform}-${arch}`);
}

function latestTag() {
  const url = `https://api.github.com/repos/${REPO}/releases/latest`;
  const body = execSync(`curl -fsSL "${url}"`, { encoding: "utf8" });
  const match = body.match(/"tag_name"\s*:\s*"([^"]+)"/);
  if (!match) throw new Error("no releases found");
  return match[1];
}

function tagExists(tag) {
  try {
    const url = `https://api.github.com/repos/${REPO}/releases/tags/${tag}`;
    const body = execSync(`curl -fsSL "${url}"`, { encoding: "utf8" });
    return body.includes('"tag_name"');
  } catch {
    return false;
  }
}

function download(url, dest, isZip) {
  const ext = isZip ? ".zip" : ".tar.gz";
  const tmp = dest + ".tmp" + ext;
  execSync(`curl -fsSL "${url}" -o "${tmp}"`);
  if (isZip) {
    execSync(`unzip -o "${tmp}" -d "${path.dirname(dest)}"`);
  } else {
    execSync(`tar xzf "${tmp}" -C "${path.dirname(dest)}"`);
  }
  fs.unlinkSync(tmp);
  fs.chmodSync(dest, 0o755);
}

try {
  const t = target();
  const preferredTag = `v${PKG_VERSION}`;
  let tag;
  const isZip = t.includes("windows");
  const ext = isZip ? "zip" : "tar.gz";

  if (tagExists(preferredTag)) {
    tag = preferredTag;
  } else {
    console.warn(
      `piggybank-mcp: release ${preferredTag} not found, falling back to latest`
    );
    tag = latestTag();
  }

  const url = `https://github.com/${REPO}/releases/download/${tag}/piggybank-${t}.${ext}`;
  console.log(`piggybank-mcp: downloading ${tag} for ${t}...`);
  download(url, BIN_PATH, isZip);
  console.log("piggybank-mcp: installed successfully");
} catch (e) {
  console.error("piggybank-mcp: install failed:", e.message);
  process.exit(1);
}
