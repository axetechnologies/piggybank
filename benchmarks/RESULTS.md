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
| `api_response.json` | 6,300 | 5,683 | **9.8%** | 6,240 | 1.0% |
| `build_log.txt` | 11,156 | 9,032 | 19.0% | 3,570 | **68.0%** |
| `git_diff.txt` | 22,780 | 22,471 | 1.4% | 376 | **98.3%** |

| file | headroom latency | boomerang latency | headroom warmup (one-time, cached) |
|---|---:|---:|---:|
| `api_response.json` | 1,434 ms | 5.6 ms | 4.2 s |
| `build_log.txt` | 2,279 ms | 5.4 ms | (shared, above) |
| `git_diff.txt` | 74 ms | 8.1 ms | (shared, above) |

Fidelity: boomerang's output was independently decompressed and compared against
the original for all three files — byte-identical for the two text files, and
value-equal (parsed JSON) for the JSON file, all `true`. headroom's fidelity
mechanism (CCR retrieval) wasn't exercised here — this benchmark calls its
library-level `compress()` function directly rather than going through its full
proxy/retrieval pipeline, so that guarantee isn't independently checked in this
harness.

## Analysis — this is a mixed result, not a sweep

**Boomerang wins decisively on structural/repetitive content.** The build log and
git diff are exactly its design target: long runs of near-identical or
mechanically-repeated lines. 68% and 98.3% reduction, at 5-8ms with zero model
dependency, because elision + dedup doesn't need to *understand* the content to
compress it — just recognize the repetition.

**headroom wins on the JSON case**, 9.8% vs 1.0%. Worth being honest about why:
these commit objects are mostly *unique* per-row data (different SHAs, messages,
timestamps) with little repeated key/value structure for a columnar transform to
exploit — boomerang's JSON compressor amortizes repeated keys and values, and
there isn't much repetition here to amortize. Kompress's semantic/prose-aware
compression found more to trim in the free-text fields (commit messages, author
names) than structural dedup could.

**headroom's near-zero result on the diff (1.4%) is plausibly a deliberate
safety choice, not a failure** — `protect_analysis_context` defaults to `True`,
and lossy ML summarization of exact code changes is a real correctness risk
worth protecting against. This is the strongest argument *for* boomerang's
specific design: elision-with-exact-retrieval gets the size win (98.3%) without
that risk, because nothing is ever paraphrased — the elided middle is byte-exact
on retrieval, not reconstructed from a model's compressed understanding of it.

**Operationally**, boomerang has no warmup, no model download, no network
dependency, and runs 100-400x faster once headroom's model is warm — the entire
reason this project exists is that the model dependency headroom's edge case
relies on (Kompress) is also what broke its pipx install in the first place
(see the main README).

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
