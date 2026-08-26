# Boomerang Roadmap

Novel features under consideration. Organized by impact tier.

---

## Tier 1 — High Impact, Novel

### 1. Budget-Constrained Compression
Invert the model. Instead of "compress as much as you can," the caller says: "I have N bytes of budget — give me the highest information density you can fit." The server prioritizes anomalous/novel content and progressively elides the rest.

- **New tool**: `compress_budget(content, max_bytes, key?)`
- **Why it matters**: Makes Boomerang a *context management layer*, not just a compression utility. Every other tool produces output without knowing the remaining context budget. Boomerang is uniquely positioned to be the gatekeeper.
- **Approach**: For text/logs, keep anomalous lines (non-modal templates) verbatim, collapse repetitive lines into counts, elide the middle. For JSON, keep the schema + outlier rows, elide homogeneous bulk. Falls back to normal compress if budget exceeds compressed size.

### 2. Streaming Append-Only Mode
Send only new bytes when tailing logs or polling a build.

- **New tool**: `compress_append(key, content)`
- **Why it matters**: Right now tailing logs requires resubmitting the entire content each call. Append mode makes polling nearly free after the first call.
- **Approach**: Server maintains a running buffer per key. New content is appended, dedup/elision applied to the combined buffer, but only the delta since last call is returned. Decompress reconstructs the full stream.

### 3. Anomaly-Aware Elision
Structural compression treats all lines equally. But `healthcheck OK × 500` is noise; the one `[ERROR] connection refused` is the whole point.

- **Why it matters**: No LLM needed — simple frequency analysis on line templates (strip timestamps/numbers, hash the skeleton) surfaces statistical outliers automatically.
- **Approach**: During text compression, build a frequency table of line templates. Lines matching the dominant template(s) are aggressively collapsed; outlier lines are always kept verbatim. Composable with existing dedup and elision.

### 4. Cross-Session Persistence (Warm Start)
Keys survive across conversations. When a new session compresses `fleet-status`, Boomerang diffs against the *last known state from a previous session*.

- **Why it matters**: The LLM gets "here's what changed since you last looked" without anyone remembering the old state. Turns Boomerang into lightweight agent memory for structured state.
- **Approach**: Session already persists `.session.json` on disk. Extend so session state isn't cleared on process restart — the existing `Session::open` already does this. The missing piece is exposing a `last_seen_summary(key)` tool that returns "unchanged since <timestamp>" or a concise diff summary without requiring the caller to resend content.

---

## Tier 2 — Strong Utility

### 5. Fingerprint-Only Change Detection
Check whether content has changed since last compression without sending the content at all.

- **New tool**: `changed(key, hash)`
- **Approach**: Client computes a cheap hash (sha256 of content), sends 64 bytes. Boomerang compares against the stored id for that key. Returns `{changed: bool, last_compressed_unix: u64}`. If unchanged, skip the compress call entirely — zero bytes transferred.

### 6. Content-Type Specializations
Beyond JSON arrays and text logs:

- **Stack traces**: Dedup common frames, highlight the divergent lines.
- **Diffs/patches**: Compress context lines, preserve change hunks verbatim.
- **HTML**: Strip tags/chrome, extract text, preserve structural markers.
- **CSV**: Columnar compression like JSON but without key overhead.

### 7. Server-Side Projections
Register a filter: "from `fleet-status`, only return nodes where `status != healthy`."

- **New tool**: `compress_filtered(content, key?, filter_expr)`
- **Approach**: Simple predicate expressions on JSON fields. Combined with session diffing: "since last check, jl3 went from healthy to degraded." Filtering happens before content hits the context window.

### 8. Multi-Agent Shared Store
When multiple agents run in parallel (buddy worktrees, fleet ghosts), they share a single Boomerang store.

- **Why it matters**: Agent A compresses fleet-status; Agent B references the same key and gets a diff against what A already stored. No duplicate storage, cross-agent awareness.
- **Status**: The store is already content-addressed on disk — two processes sharing a `--store-dir` already benefit from each other's history (tested and confirmed). The gap is session-level key tracking, which is per-process. Fix: move session state to a lockfile-guarded shared file, or accept that cross-agent sharing is store-level only (values) and session-level sharing (keys) requires explicit coordination.

---

## Tier 3 — Polish / Operational

### 9. Cross-Key Correlation
"These 3 keys changed together in the last cycle." Lightweight observability without understanding semantics.

- **Approach**: Track timestamps of key changes. Surface co-change groups on request.

### 10. TTL / Garbage Collection Policies
Refs expire based on configurable policies per key prefix.

- **Status**: `gc` CLI command already exists with age-based deletion. Gap: per-key TTL policies, and a way to mark certain keys as "ephemeral" (build logs: 1 hour) vs "persistent" (config snapshots: indefinite).

### 11. Compression Analytics
Which keys are hot, which waste the most context while rarely changing.

- **New tool**: `analytics()`
- **Approach**: Track per-key: compress call count, average size, change frequency, compression ratio. Surface recommendations: "fleet-status is 40% of your budget but changes <1% per call — poll less."

### 12. Schema Registration
For known shapes, register the schema once, then only store values.

- **Approach**: `register_schema(key, schema)` — subsequent compressions under that key can be even more aggressive than columnar because the server knows the structure and can store only the delta from the schema.

---

## Design Principles

- **Lossless round-trip is non-negotiable.** Every feature must preserve the guarantee that decompress(compress(x)) == x. Budget-constrained compression is the one exception, and even there the elided content must be recoverable via `retrieve`.
- **Pay-for-itself discipline.** No transform should ever grow the output. If a compression strategy doesn't save bytes, skip it.
- **Light over clever.** Prefer simple, obviously-correct implementations over clever algorithms. The existing codebase demonstrates this: textbook LCS over Myers, linear scans over indexes, proptest over assumptions.
- **No new dependencies without justification.** The current dependency set is minimal. Keep it that way.
