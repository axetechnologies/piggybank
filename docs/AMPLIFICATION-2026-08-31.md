# Piggybank Amplification Plan — 2026-08-31

Goal (James): make piggybank's effect *felt* — users get 3-4x more usage out of a session
via less token flow and fewer external API calls. This doc records what was verified by
running, what was measured, and the ranked levers. Doctrine: verified "no" beats
unverified "yes"; every number below was produced, not assumed.

## What was broken (verified, now fixed on branch)

The flagship auto-compression path — the PostToolUse hook — had never worked, for three
independent reasons, each failing silently:

1. **Wrong binary path.** `hooks/post-tool-use-compress.sh` defaulted to
   `$HOME/.piggybank/bin/piggybank`; real installs put the binary at `~/.axe/bin/piggybank`
   (and `install.sh` doesn't wire the hook into Claude Code settings at all, so most
   installs never even registered it). `2>/dev/null || echo ""` swallowed the failure.
2. **Wrong response schema.** The hook emitted `{"output": ...}`. Claude Code's actual
   PostToolUse contract (code.claude.com/docs/en/hooks) requires
   `{"hookSpecificOutput": {"hookEventName": "PostToolUse", "updatedToolOutput": <tool-native shape>}}`
   — for Bash that's `{"stdout", "stderr", "interrupted", "isImage"}`. The harness
   silently ignored the wrong shape (verified live: hook fired, output arrived raw).
3. **Unreachable threshold.** Default 100,000 bytes; Claude Code truncates Bash output
   around 30KB, so the hook would ~never trigger even if 1–2 were fixed.

**Fix shipped on this branch** (`hooks/post-tool-use-compress.sh`): binary fallback chain,
`updatedToolOutput` contract (Bash only until other tools' native shapes are verified),
threshold default 4096. **Live end-to-end proof in a real Claude Code session:** identical
command run twice; second result was replaced in-context — `6,599 -> 86 bytes` with the
piggybank header. First time the feature has ever functioned.

## What was measured (honest ceilings)

- Across 3 real session transcripts, tool outputs >1KB totaled 3.7 / 0.3 / 0.2 MB;
  **line-level dedup ceiling = 12% / 19% / 7%** of tool-output bytes. (The tempting
  "compress-log the whole transcript → 100% saved" number is an artifact: the view is all
  elision markers; first-sight content must still be paid for.)
- **Verified no: single-session lossless dedup cannot deliver 3-4x by itself.** The 3-4x
  target requires the levers below that change *what gets re-sent and re-fetched*, not
  just how repeats are encoded.
- Live MCP stats reading `0.0%` on unique payloads is correct behavior
  (`record_and_savings`, mcp.rs:450 — lifetime aggregate), but it *feels* like the product
  is doing nothing. Perception fix is P1.

## Ranked levers

**P0 — shipped here: make the existing feature exist.** Hook contract + path + threshold
(above). Also: `install.sh` must wire both hooks into `~/.claude/settings.json`
(PostToolUse matcher `Bash|Read|Grep|WebFetch`, PreCompact) — today every fresh install
ships the dead feature. One-file change, do next.

**P1 — make savings felt (perception is half the ask).**
- Real tokenizer in the live stats path. `benchmarks/compare.py` already uses tiktoken;
  the server reports bytes/4 (`BYTES_PER_TOKEN=4.0`, mcp.rs:67). Count real tokens saved,
  report `tokens saved / $ saved / % of context freed`.
- Statusline surface: a `piggybank statusline` subcommand emitting one line
  ("🐷 saved 41k tok / $0.12 today") for Claude Code statusline config — same trick as the
  fleet's "$137 frontier cost avoided" line, which James already responds to.
- Stop-hook session receipt: on session end, one summary line into the transcript/log.

**P2 — coverage: rewrite more of what enters context.**
- Extend `updatedToolOutput` rewriting to Read/Grep/WebFetch (verify each native shape).
- Add a true single-shot stdin mode (`piggybank compress-stdin <key> [--store-dir]`) so
  hooks don't need the mktemp workaround — today every CLI subcommand requires a file path.
- MCP proxy (`proxy.rs`, already built): wrap chatty MCP servers (axe-remote's ~900 tools
  return verbose JSON) so their results are auto-columnarized. Wire the harvest logger into
  the proxy path (currently the one half-finished piece).

**P3 — the actual 3-4x: cross-session + cross-agent reuse.**
The store is already content-addressed and cross-call; the multiplier is fleets of
subagents re-reading the same files. N agents × same repo = N× first-sight cost today;
a shared store + warm-start summaries (ROADMAP #4) turns that into 1× + refs. This is
where multi-agent workflows (the heaviest token burners) get order-of-magnitude relief.
Needs: stable cross-session store keys (file path + content hash), a session-start hook
that primes hot keys, and eviction discipline (gc exists).

**P4 — fewer external API calls: read-through fetch cache.**
Confirmed absent today (no HTTP client in core/cli; only the brain.axe.onl stats POST).
Add `piggybank fetch <url>` / proxy-level GET cache: content-addressed body + TTL +
conditional requests (ETag/If-Modified-Since). Repeat fetches of docs/APIs across sessions
and agents become store hits — this is the literal "fewer external API calls made" ask.

**P5 — compression power (incremental, cheap).**
- Anomaly-aware elision in the *normal* compress path (ROADMAP #3 — today only
  `compress_budget` ranks by anomaly score).
- Loosen cross-key match: >33% line-overlap trigger (session.rs:364) only fires on
  first-sight keys, text-only; extend to JSON and tune the threshold with the existing
  "must pay for itself" size check as the guard.
- Content-type specializations (stack traces, diffs, CSV) — ROADMAP #6.
- Conversation-turn layer: RESULTS.md's own Limitations section names it — today
  compression sees one tool result at a time, never the accumulated conversation. Biggest
  structural unlock, biggest effort.

## Notes
- **AI-agnostic rule enforcement (fixed on this branch, verified by grep):** the Rust
  source hardcoded `https://brain.axe.onl/api/save`, an `X-AXE-Key` header, `AXE_FLEET_KEY`
  env fallback, and a `~/.axe/boomerang-store` default path. Stats reporting is now fully
  env-driven and opt-in: `PIGGYBANK_STATS_URL` + `PIGGYBANK_STATS_KEY` (both required to
  report at all) + `PIGGYBANK_STATS_KEY_HEADER` (default `X-Api-Key`); default store path
  is `~/.piggybank/store`. **Migration for fleet installs:** set
  `PIGGYBANK_STATS_URL=https://brain.axe.onl/api/save`, `PIGGYBANK_STATS_KEY=$BRAIN_KEY`,
  `PIGGYBANK_STATS_KEY_HEADER=X-AXE-Key` in the MCP server env at deploy time — do NOT
  hot-swap the running binary without these or fleet stats silently stop.
- "Provable Context" reframe: zero hits in this repo (README still leads with the
  piggybank/money framing). Either land the reframe or stop referencing it.
- The 100KB threshold comment claims it "matches BASH_MAX_OUTPUT_LENGTH" — unverified;
  threshold is now 4096 by default and env-tunable regardless.
- Async hooks can't rewrite output (response arrives too late) — the piggybank hook entry
  must stay synchronous; keep it fast (it is: no warmup, pure Rust).
