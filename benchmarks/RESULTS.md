# Boomerang vs headroom

Run with `benchmarks/compare.py`, on real (not synthetic) content, using the same
neutral yardstick (tiktoken `cl100k_base`) for every measurement. Raw numbers in
`benchmarks/results.json`.

## Method

Corpus (`benchmarks/corpus/`), none of it curated to flatter either tool:

| file | source |
|---|---|
| `api_response.json` | real GitHub API response — 30 commits on this repo |
| `build_log.txt` | real `cargo build --workspace -v` output (verbose, real crate list) |
| `git_diff.txt` | real `git log -p --all` on this repo's own history |

headroom's compressor is history-aware — by default it never touches the last
`protect_recent=4` messages, and skips anything under `min_tokens_to_compress=250`
tokens. Each corpus file is wrapped as an older tool_result in a 9-message
transcript, using Anthropic's real `tool_result` wire shape (a user-role message
with a list of `tool_result` content blocks — the specific shape its router
detects to tell genuine tool output from human-typed text, which it protects by
default). Boomerang operates on the raw file directly via its actual CLI, the way
someone would really invoke it — it has no message-history layer yet (see the
main README's "not built yet" list).

headroom's semantic compressor (Kompress-v2-base, a 274MB HF model) loads in the
background on first use; a request inside roughly the first 20s "fails open" and
returns content unchanged rather than blocking. That's a real operational
characteristic, not a bug to route around, so it's reported here explicitly. The
warmup number below (4.2s) reflects an *already-downloaded, locally cached*
model — this session had already triggered the download once while debugging.
A genuinely first-ever run pays for the 274MB HuggingFace download before that
4.2s, which this harness did not separately time.

## Results

| file | raw tokens | headroom tokens | headroom reduction | boomerang tokens | boomerang reduction |
|---|---:|---:|---:|---:|---:|
| `api_response.json` | 6,300 | 5,683 | 9.8% | 3,995 | **36.6%** |
| `build_log.txt` | 11,156 | 9,032 | 19.0% | 3,570 | **68.0%** |
| `git_diff.txt` | 22,780 | 22,471 | 1.4% | 376 | **98.3%** |

Boomerang wins on all three now — see "The JSON fix" below for what changed and why.

| file | headroom latency | boomerang latency | headroom warmup (one-time, cached) |
|---|---:|---:|---:|
| `api_response.json` | 1,967 ms | 353.8 ms | 12.7 s |
| `build_log.txt` | 2,780 ms | 6.5 ms | (shared, above) |
| `git_diff.txt` | 86 ms | 9.9 ms | (shared, above) |

boomerang is slower on the JSON case specifically (354ms vs its usual single-digit ms) —
that's the cost of the new interning pass's dedup-key computation, measured (not assumed)
to scale roughly linearly to mildly superlinearly with document size rather than
catastrophically; see the doc comment on `MIN_INTERN_LEN` in `json.rs` for the actual
numbers checked. Still ~5.6x faster than headroom on this file, with zero warmup cost.

Fidelity: boomerang's output was independently decompressed and compared against
the original for all three files — byte-identical for the two text files, and
value-equal (parsed JSON) for the JSON file, all `true`. headroom's fidelity
mechanism (CCR retrieval) wasn't exercised here — this benchmark calls its
library-level `compress()` function directly rather than going through its full
proxy/retrieval pipeline, so that guarantee isn't independently checked in this
harness.

## Analysis

**Boomerang wins decisively on structural/repetitive content.** The build log and
git diff are exactly its design target: long runs of near-identical or
mechanically-repeated lines. 68% and 98.3% reduction, at 6-10ms with zero model
dependency, because elision + dedup doesn't need to *understand* the content to
compress it — just recognize the repetition.

**Boomerang now wins on the JSON case too** (36.6% vs 9.8%), but this took a real
fix, not a knob turn — see "The JSON fix" below. The original 1.0% result exposed
a genuine gap: columnarization only dedupes key *names* across rows, not repeated
*values*. These commit objects looked "mostly unique" at a glance (different SHAs,
messages, timestamps), but the *author*/*committer* sub-objects — GitHub user
records, ~940 bytes each — were the same person's data repeated identically 10
times (author + committer, across 5 commits), over 9KB of pure duplication in a
20.5KB file. That's exactly the kind of redundancy real API responses have all
the time (the same account/metadata object attached to many records), and the
original compressor had no mechanism to catch it.

### The JSON fix

Added a second, independent, composable pass — value interning — that runs after
columnarization: walk the whole compressed value, find any subtree (object,
array, or string) that appears more than once anywhere in it, and replace every
occurrence but the first with a `{"__boomerang_ref__": i}` marker into a
dictionary. Still fully lossless, still no `Store` involved (this is removable
redundancy, not information loss), still guarded by the same "pay for itself"
discipline as the log compressor's line-dedup — a candidate is only committed to
if it measurably shrinks the output, checked directly rather than assumed from a
threshold. Full detail and the round-trip/no-regression tests are in
`crates/boomerang-core/src/json.rs`.

One real cost: boomerang went from ~6ms to ~354ms on this file. The dedup key is
`value.to_string()`, computed once per node at a cost proportional to that node's
own subtree size — a naive analysis suggests quadratic blowup on deeply nested
documents. Measured instead of assumed (see the `MIN_INTERN_LEN` doc comment):
scaling the same API-response shape from 20KB to 4.2MB (200x) took wall time from
15ms to 2.0s (~134x) — roughly linear to mildly superlinear in practice, not
quadratic, because most subtrees in real JSON are small. Still ~5.6x faster than
headroom on this file, with zero warmup. Left as-is rather than rewritten into an
incrementally-computed structural hash, per the project's own "don't build for
hypothetical requirements" discipline — the measured numbers don't currently
justify the added complexity.

**headroom's near-zero result on the diff (1.4%) is plausibly a deliberate
safety choice, not a failure** — `protect_analysis_context` defaults to `True`,
and lossy ML summarization of exact code changes is a real correctness risk
worth protecting against. This is the strongest argument *for* boomerang's
specific design: elision-with-exact-retrieval gets the size win (98.3%) without
that risk, because nothing is ever paraphrased — the elided middle is byte-exact
on retrieval, not reconstructed from a model's compressed understanding of it.

**headroom's near-zero result on the diff (1.4%) is plausibly a deliberate
safety choice, not a failure** — `protect_analysis_context` defaults to `True`,
and lossy ML summarization of exact code changes is a real correctness risk
worth protecting against. This is the strongest argument *for* boomerang's
specific design: elision-with-exact-retrieval gets the size win (98.3%) without
that risk, because nothing is ever paraphrased — the elided middle is byte-exact
on retrieval, not reconstructed from a model's compressed understanding of it.

**Operationally**, boomerang has no warmup, no model download, and no network
dependency at all — it beats headroom's per-file latency by 5.6x to over 400x
across all three files (worst case still winning, on the JSON file, where the
new interning pass costs the most). The entire reason this project exists is
that the model dependency headroom's edge case relies on (Kompress) is also
what broke its pipx install in the first place (see the main README).

## Limitations of this benchmark

- Single run, three files — not a statistically powered comparison.
- Boomerang has no session/message-history awareness yet, so it can't currently
  do what headroom's diff-across-conversation-turns can (though it does have its
  own from-scratch diff-against-last-seen mechanism at the `Session` layer,
  not exercised by this harness since it operates on raw content, not chat
  history).
- headroom's CCR retrieval fidelity wasn't independently checked here.
- No downstream task-quality check (same LLM, same question, compressed vs raw
  context, diff the answers) — the harder and more important test, not yet
  built.
