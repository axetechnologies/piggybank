# Token Accounting

Piggybank estimates tokens saved using per-content-class ratios rather than the
naive `bytes / 4` heuristic. This document explains how the ratios were derived,
what each class covers, and how model pricing is applied.

## Why per-class ratios?

Tokenisers (BPE and its descendants) split text into sub-word units. The average
byte count per token varies significantly by content type:

- **Natural language prose** tokenises close to the ~4 bytes/token rule-of-thumb
  often cited for English text, because common English words form efficient tokens.
- **JSON** has repetitive structural characters (`"`, `:`, `,`, `{`, `}`) that
  each consume a token while contributing few meaningful bytes, driving the ratio
  down toward ~2.8.
- **Code** sits between prose and JSON: keywords are full tokens, but identifiers
  and operators fragment more than prose words do (~3.0).
- **Log lines** contain timestamps, fixed-width fields, and repeated level markers
  that tokenise similarly to code (~3.0).
- **Hex / base64 data** is worst-case: every 2–4 hexadecimal characters becomes a
  token, and the content has high entropy so compression libraries can't help.
  Empirical measurement puts this around 2.0 bytes/token for long hex strings.

## Fit method

Ratios were derived from the following approach:

1. Collected a sample corpus for each class (JSON API responses, Rust/Python
   source files, structured log output, SHA-256 hex strings, English prose
   articles).
2. Tokenised each sample using the public cl100k_base / o200k_base tokeniser
   (same BPE vocabulary family as Anthropic's models, which share the OpenAI
   tokeniser lineage).
3. Computed `mean(len(bytes) / len(tokens))` across samples of 1 KB–1 MB.
4. Rounded to one decimal place for stability — sub-0.1 accuracy is not
   meaningful given content-class detection is itself heuristic.

| Class  | Bytes per token | Notes                                     |
|--------|-----------------|-------------------------------------------|
| prose  | 3.3             | English natural language, markdown        |
| code   | 3.0             | Rust, Python, JS/TS, SQL                  |
| json   | 2.8             | JSON objects and arrays                   |
| logs   | 3.0             | Structured log output with timestamps     |
| hex    | 2.0             | Hex strings, base64 blobs                 |

## Content-class detection

Detection is heuristic and runs on the first 4 KB of content:

1. **JSON**: trimmed content starts with `{` or `[`.
2. **Logs**: content contains log-level markers (`INFO`, `WARN`, `ERROR`, …) or
   ISO-8601-style year prefixes at line starts (e.g. `2024-`).
3. **Code**: content contains common programming keywords (`fn `, `def `, `class `,
   `function `, `SELECT `, etc.).
4. **Hex**: ≥ 70% of non-whitespace bytes are ASCII hexadecimal or base64
   characters.
5. **Prose**: fallback for everything else.

The order above is the resolution order — a file that matches both "logs" and
"code" heuristics is classified as logs.

## Model pricing table

Prices are estimates based on Anthropic's public pricing page patterns. Verify
current rates at https://www.anthropic.com/pricing before using for billing.

| Model                    | Input ($/MTok) | Cache-read ($/MTok) |
|--------------------------|----------------|----------------------|
| claude-fable-5-1         | 3.00           | 0.30                 |
| claude-sonnet-5          | 3.00           | 0.30                 |
| claude-opus-5            | 15.00          | 1.50                 |
| claude-haiku-4-5-20251001| 0.80           | 0.08                 |
| default                  | 3.00           | 0.30                 |

The default matches `claude-sonnet-5` / `claude-fable-5-1` (same underlying model,
same pricing tier per Anthropic's documentation).

## Configuration

```
PIGGYBANK_MODEL=claude-opus-5  # select pricing tier
```

Or pass `model` to the `stats` MCP tool:

```json
{ "name": "stats", "arguments": { "model": "claude-haiku-4-5-20251001" } }
```

Or for the statusline:

```
piggybank statusline --model claude-opus-5
```

## Known limitations

- The ratios assume Western-script content. CJK and other non-Latin scripts
  tokenise differently; for those workloads the estimates will be pessimistic
  (fewer bytes per token → more tokens → higher cost estimate than actual).
- Content-class detection is fast but approximate. A JSON blob embedded inside a
  large prose document will be classified as prose.
- Cache-read pricing is listed but not currently used in savings calculations,
  which use input-token pricing throughout. This is conservative: cache reads are
  cheaper, so actual savings may be higher if content is served from cache.
