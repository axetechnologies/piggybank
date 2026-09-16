# Format-aware vs generic compression ratios

Measured on the six fixtures in `crates/piggybank-core/tests/fixtures/`.
All sizes are compressed bytes relative to the original. Both compressors
store elided content in the same content-addressed store so the round-trip
invariant holds in both cases.

## Results

```
fixture                    orig  generic   format    gen%    fmt%
pytest_output.txt          4188     1617     2296   38.6%   54.8%
cargo_output.txt           1523     1523     1010  100.0%   66.3%
git_diff.txt               1215     1215     1215  100.0%  100.0%
git_status.txt              593      593      593  100.0%  100.0%
npm_install.txt             776      776      784  100.0%  101.0%
jsonl_logs.txt             2154     2154      224  100.0%   10.4%
```

`gen%` = generic compressor output / original.
`fmt%` = format-aware compressor output / original.
Values above 100% mean the compressed form is slightly larger than the
original due to marker overhead.

## Notes by fixture

**pytest** — Generic scores better on raw byte count (38.6% vs 54.8%) because
it does a blind head/tail cut that discards everything in the middle,
including failure tracebacks. Format-aware keeps tracebacks verbatim and only
elides PASSED lines, so it is semantically superior even though its output is
larger. The LLM sees all failures; the generic view may not.

**cargo** — Generic compressor does not compress (100%) because the file is
under the `elide_threshold_lines` default and triggers only dedup. Format-aware
achieves 66.3% by eliding the 18 `Compiling` lines into a single ELIDE marker
while keeping the two `error[E...]` spans verbatim.

**git_diff / git_status** — Both are small enough that no elision triggers.
Format-aware passes `git_status` through unchanged (already compact) and would
only collapse large context hunks in a real `git diff`. The test fixture is
too small to show a gain.

**npm_install** — 776-byte fixture with only 3 `npm notice` lines collapsible.
The ELIDE marker itself adds about 8 bytes of overhead, causing a 1% size
increase. On a real npm install with hundreds of notice lines, format-aware
would score significantly better.

**jsonl_logs** — Best case: 16 records with the same schema collapse to 1
verbatim record + 1 ELIDE marker = 10.4% of original. Generic compressor
does not recognise the repetition pattern and keeps all records (100%).

## Methodology

Ratios were measured via `Format::*` + `compress_text` called from a
unit test (`format::tests::bench_compression_ratios`) with the default
`TextOptions`. Sizes include ELIDE marker bytes but not the stored elided
content (which is in the store, not in the view the LLM receives).

To reproduce:

```
cargo test --package piggybank-core bench_compression_ratios -- --nocapture
```
