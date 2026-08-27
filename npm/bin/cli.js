#!/usr/bin/env node
const { spawn } = require("child_process");
const path = require("path");

const bin = path.join(__dirname, "..", "piggybank");
const args = process.argv.slice(2);

if (args.length === 0) {
  args.push("mcp", "serve");
}

const child = spawn(bin, args, { stdio: "inherit" });
child.on("exit", (code) => process.exit(code ?? 1));
