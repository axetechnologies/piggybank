#!/usr/bin/env node
// piggybank-mcp CLI entry point.
// Forwards all subcommands to the native piggybank binary.
// For `init`, also passes --hooks-dir pointing to this package's bundled hooks.

const { spawn } = require("child_process");
const path = require("path");
const fs = require("fs");

const pkgDir = path.join(__dirname, "..");
const bin = path.join(pkgDir, "piggybank");

const args = process.argv.slice(2);

if (args.length === 0) {
  args.push("mcp", "serve");
}

// For `init`: inject --hooks-src so the binary can copy hooks from the npm package.
// This is only effective if the bundled hooks/ directory exists (i.e. after `npm pack`).
if (args[0] === "init") {
  const hooksDir = path.join(pkgDir, "hooks");
  if (fs.existsSync(hooksDir) && !args.includes("--hooks-src")) {
    args.push("--hooks-src", hooksDir);
  }
}

if (!fs.existsSync(bin)) {
  console.error(
    "piggybank-mcp: binary not found at",
    bin,
    "\nRun: npm install piggybank-mcp  (postinstall downloads the binary)"
  );
  process.exit(1);
}

const child = spawn(bin, args, { stdio: "inherit" });
child.on("exit", (code) => process.exit(code ?? 1));
