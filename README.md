# Boomerang

Micro context-compression for AI agents. A much lighter, more robust take on
[headroom](https://github.com/chopratejas/headroom): same core idea (shrink
what an agent reads before it hits the LLM, never actually lose anything),
built as a single self-contained Rust binary instead of a Python package that
pulls in `torch`+`transformers`. Measured, not asserted: 426KB release binary
(size-tuned release profile: LTO, single codegen unit, stripped), sub-5ms
cold start, only linked against the OS's own `libSystem` on macOS —
on Linux with a musl target it's genuinely fully static. "Static" isn't quite
the right word on macOS specifically (Apple doesn't support fully static
linking there at all — even `libSystem` is always dynamic), so what actually
matters and is true everywhere: no interpreter, no package manager, no venv,
nothing that can rot out from under it the way `headroom-ai`'s did.

## Why this exists

`headroom-ai`'s pipx venv broke because Homebrew retired the Python version
it was pinned to — a rotted interpreter symlink took down an MCP server with
one line in a config file. That's not a headroom-specific bug, it's what
happens when a context-compression layer — something that sits between an
agent and its own perception of the world — depends on a large, versioned
runtime. Boomerang's answer: no interpreter, no downloaded model weights, no
venv. One binary.

## The name

Headroom compresses; Boomerang returns. The mechanism this project is built
around is content-addressed, reversible retrieval — nothing is ever deleted,
only set aside, and it comes back the moment something asks for it by
reference. A boomerang doesn't need to be fetched — it's already coming back.

## Beyond compression

Strip away the "compressor" framing and what's actually been built is a
content-addressed, atomically-written, crash-safe, *provably* reversible
blob store — compress/decompress round-trips are tested exhaustively
(proptest fuzzing, verified against the real binary, not just asserted),
not approximated the way headroom's ML-based CCR caching is. That primitive
is worth more than the compression it happens to enable. Four consequences
of it, proven not just designed:

- **Shared memory across agents that never coordinate.** `Store` is just
  content-addressed files in a directory with atomic writes — nothing
  requires one caller to know another exists. Two completely independent
  `boomerang mcp serve` processes pointed at the same store directory, with
  zero communication between them, compound each other's compression
  history automatically: the second agent's very first call already
  benefits from content the first agent introduced. headroom has no
  equivalent — its compression state doesn't cross process boundaries.
  Verified with two genuinely separate subprocesses, not two calls in one
  process (`two_independent_callers_sharing_a_store_benefit_from_each_others_history`
  in `crates/boomerang-core/src/json.rs`).
- **Provenance.** Every piece of content ever written to the store records
  when it was first seen (`.provenance.jsonl`, best-effort, never blocks
  the write it's attached to), retrievable via `boomerang_retrieve`'s
  `first_seen_unix` field — regardless of which caller originally wrote it.
  "What did any agent see, and when" becomes an answerable question instead
  of something lost the moment content gets compressed away.
- **Integrity verification without full reconstruction.** `boomerang_verify`
  walks a compressed view's reference chain and confirms every id still
  resolves in the store — `Store::exists` (a stat), not `Store::get` (a
  read) — reporting exactly which references are missing rather than
  failing outright. Cheaper than a full `boomerang_decompress` when a
  caller only needs "is this still fully reconstructable right now," not
  the content itself: a lightweight audit/integrity primitive, not just a
  compression optimization.
- **Retention, honestly scoped.** "Nothing is ever discarded" is the
  compression invariant, not a promise that a shared store grows forever
  for free. `Store::gc(older_than_unix, dry_run)` / `boomerang gc
  <store-dir> --older-than-days N [--dry-run]` deletes content by age -
  the only policy a content-addressed store can apply *safely* without a
  full reachability graph of every compressed view sitting in some agent's
  conversation history far outside this store's view. Content with no
  recorded first-seen time is never touched (no basis to judge it
  eligible), and this is deliberately CLI-only, human-invoked, dry-run
  capable, and never exposed over MCP - an agent should not be able to
  delete shared, possibly-multi-tenant content on its own initiative.

## Design

Two primitives, nothing else load-bearing:

1. **A view** — a smaller representation of a document.
2. **Recovery** — the original, byte-for-byte, on demand, forever.

Everything else (proxy, MCP server, per-agent wrappers) is product surface
around those two. See [`crates/boomerang-core`](crates/boomerang-core) for
the current state:

- `Store` — a content-addressed blob store (`sha256(bytes) -> bytes` on
  disk). Writing the same content twice is a no-op. This is what makes
  compression reversible: a compressor never decides what's safe to discard,
  because nothing it stores here is ever discarded. Every genuinely new
  write also records a first-seen timestamp (`first_seen()`,
  best-effort, never blocks the write it's attached to) — see "Beyond
  compression" below for what that and shared-directory writes make possible.
- `compress_json` / `decompress_json` — **lossless** structural compression,
  two composable passes. First, homogeneous arrays of objects (the shape of
  almost every API response or tool-result list) become a columnar table:
  keys written once, then rows of values, instead of repeating every key
  name per element. Second, value interning: any subtree (object, array, or
  string) that repeats anywhere in the result — e.g. the same GitHub-user
  object attached to ten different commits — gets replaced, after its first
  occurrence, with a short reference into a dictionary. Columnarization
  alone only dedupes key *names* across rows; interning is what catches
  repeated *values*, which is where most of the size actually lives in real
  API responses. Neither pass needs `Store` — there's nothing to hold back
  for later, because no information is lost.
- `compress_json_with_store` / `decompress_json_with_store` — beyond
  parity with headroom, not just matching it: cross-*call* structural
  memory for JSON, the thing `Session` already gives text but JSON never
  had. An agent polling the same status endpoint, or re-fetching a metadata
  object that hasn't changed, has each repeat cost next to nothing starting
  from the *second* time it's seen — content-addressed, so it works across
  entirely different documents and calls, not just one tracked key. Same
  "only commit if it actually shrinks" guarantee as everything else here
  (the marker itself costs 89 bytes; anything smaller than that never gets
  promoted, checked exactly, not just filtered by a threshold). This is
  what the MCP server's `boomerang_compress`/`boomerang_decompress` use for
  JSON; the plain `compress_json`/`decompress_json` above stay pure and
  store-free for callers (like the CLI's `compress`/`decompress`) that want
  a single, stateless, self-contained transform.
- `compress_text` / `decompress_text` — dedup of consecutive repeated lines
  (only when the collapse actually pays for itself) plus middle-elision past
  a line-count threshold, with the elided span held in `Store` and recovered
  exactly on retrieval.
- `Session` — diffs against whatever was last compressed under a given key
  (e.g. a file path): first sight passes through, an identical re-read
  collapses to a short marker, a changed re-read gets a compact line diff
  (a hand-rolled LCS differ) replayable against the stored previous version.
  `Session::open` persists state to disk so it survives process restarts.

`boomerang mcp serve` ([`crates/boomerang-cli/src/mcp.rs`](crates/boomerang-cli/src/mcp.rs))
exposes all of it over stdio as a hand-rolled MCP server — no SDK, no async
runtime, just newline-delimited JSON-RPC. Four tools: three named to mirror
headroom's own surface (`boomerang_compress`, `boomerang_retrieve`,
`boomerang_stats`), plus `boomerang_decompress` — full reconstruction of a
compressed view given the `kind` `compress` returned, which headroom's
surface doesn't expose and which session-mode compression genuinely needs
(there's no other way to get the diffed-and-reassembled new content back).

Not built yet:

- An HTTP proxy binary, sharing the same core crate.
- Message-history awareness at the proxy/MCP layer (right now `Session`
  diffs raw content by caller-supplied key; it doesn't yet auto-detect
  conversation structure the way headroom's router does).

Explicitly *not* planned for v0: output-token compression (touching what the
model writes, not just what it reads) and any ML-based prose compressor —
both are correctness/complexity risks that the deterministic core doesn't
need in order to capture most of the win.

## The one invariant

```
retrieve(store.put(x)) == x
```

byte-for-byte, always. Tested as a hard property in
`crates/boomerang-core/src/lib.rs`, not eyeballed.

## Status

All three compressors (`Store`, JSON, text/log, `Session` diff) are built,
tested (17 unit tests, all round-trip-verified), and exposed over a real MCP
server (`boomerang mcp serve`), CI-checked on every push
(fmt/clippy/test/release build). No HTTP proxy yet, no conversation-structure
awareness at the MCP layer yet.

See [`benchmarks/RESULTS.md`](benchmarks/RESULTS.md) for a real (not
synthetic) comparison against headroom: boomerang wins on all three corpus
files — 68–98% reduction on a build log and a git diff (5.6–400x faster,
zero warmup), and 36.6% vs headroom's 9.8% on a GitHub API JSON response,
after a real fix (value interning, not a knob turn) closed a genuine gap:
columnarization alone missed ~9KB of repeated author/committer objects in a
20.5KB file. See that file for the full analysis, what the fix cost
(latency on large JSON, measured not assumed), and its limitations.
