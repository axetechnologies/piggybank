#!/usr/bin/env node
// bin/prepack.js — copy hooks from repo root into npm/hooks/ at pack time
// so the published package contains the hook scripts.

const fs = require("fs");
const path = require("path");

const pkgDir = path.resolve(__dirname, "..");
const repoRoot = path.resolve(pkgDir, "..");
const srcHooks = path.join(repoRoot, "hooks");
const destHooks = path.join(pkgDir, "hooks");

if (!fs.existsSync(srcHooks)) {
  console.error("prepack: hooks/ directory not found at", srcHooks);
  process.exit(1);
}

fs.mkdirSync(destHooks, { recursive: true });

for (const name of fs.readdirSync(srcHooks)) {
  const src = path.join(srcHooks, name);
  const dest = path.join(destHooks, name);
  fs.copyFileSync(src, dest);
  // Make shell scripts executable
  if (name.endsWith(".sh")) {
    fs.chmodSync(dest, 0o755);
  }
  console.log("prepack: copied", name);
}
console.log("prepack: hooks copied to", destHooks);
