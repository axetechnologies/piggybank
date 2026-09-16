//! Content-class-aware token estimation and model-aware pricing.
//!
//! Replaces the naive bytes/4 heuristic with per-class empirically-fitted
//! ratios. See `docs/TOKEN_ACCOUNTING.md` for the derivation method.
//!
//! Content classes:
//!   json    — starts with `{` or `[`; JSON-heavy files tokenise densely
//!   code    — presence of common code keywords / syntax
//!   logs    — timestamp or log-level markers
//!   hex     — ≥ 70% of non-whitespace chars are hex or base64 chars
//!   prose   — fallback (natural language, mixed)
//!
//! Ratios (bytes per token, see TOKEN_ACCOUNTING.md for fit methodology):
//!   prose   3.3
//!   code    3.0
//!   json    2.8
//!   logs    3.0
//!   hex     2.0

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentClass {
    Json,
    Code,
    Logs,
    Hex,
    Prose,
}

impl ContentClass {
    pub fn as_str(self) -> &'static str {
        match self {
            ContentClass::Json => "json",
            ContentClass::Code => "code",
            ContentClass::Logs => "logs",
            ContentClass::Hex => "hex",
            ContentClass::Prose => "prose",
        }
    }

    /// Empirically-fitted bytes-per-token ratio for this class.
    /// See docs/TOKEN_ACCOUNTING.md for derivation details.
    pub fn bytes_per_token(self) -> f64 {
        match self {
            ContentClass::Json => 2.8,
            ContentClass::Code => 3.0,
            ContentClass::Logs => 3.0,
            ContentClass::Hex => 2.0,
            ContentClass::Prose => 3.3,
        }
    }
}

/// Classify `bytes` into a content class by cheap heuristics.
pub fn classify(bytes: &[u8]) -> ContentClass {
    if bytes.is_empty() {
        return ContentClass::Prose;
    }

    // Trim leading whitespace to check for JSON opener.
    let trimmed = bytes
        .iter()
        .position(|&b| !b.is_ascii_whitespace())
        .map(|p| &bytes[p..])
        .unwrap_or(bytes);
    let first = trimmed.first().copied().unwrap_or(b' ');
    if first == b'{' {
        return ContentClass::Json;
    }
    // `[` only signals JSON if what follows looks like a JSON value (string,
    // number, boolean, null, object, or array) — not `[ERROR]` or `[INFO]`.
    if first == b'[' {
        let second = trimmed.get(1).copied().unwrap_or(b' ');
        if matches!(second, b'"' | b'{' | b'[' | b'n' | b't' | b'f')
            || second.is_ascii_digit()
            || second == b'-'
        {
            return ContentClass::Json;
        }
    }

    // Sample up to 4 KB for heuristics (avoid scanning huge buffers).
    let sample_len = bytes.len().min(4096);
    let sample = &bytes[..sample_len];

    // Logs: look for common log-level markers or ISO-8601-ish timestamps.
    if has_log_markers(sample) {
        return ContentClass::Logs;
    }

    // Code: look for common programming keywords.
    if has_code_markers(sample) {
        return ContentClass::Code;
    }

    // Hex/base64: if ≥ 70% of non-whitespace bytes are hex/base64 chars.
    let non_ws: Vec<u8> = sample
        .iter()
        .filter(|&&b| !b.is_ascii_whitespace())
        .copied()
        .collect();
    if !non_ws.is_empty() {
        let hex_b64_count = non_ws
            .iter()
            .filter(|&&b| {
                b.is_ascii_hexdigit()
                    || b == b'+'
                    || b == b'/'
                    || b == b'='
                    || b == b'_'
                    || b == b'-'
            })
            .count();
        if hex_b64_count as f64 / non_ws.len() as f64 >= 0.70 {
            return ContentClass::Hex;
        }
    }

    ContentClass::Prose
}

fn has_log_markers(sample: &[u8]) -> bool {
    let s = std::str::from_utf8(sample).unwrap_or("");
    // Common log level markers
    let level_markers = [
        " INFO ", " WARN ", " ERROR ", " DEBUG ", " TRACE ", " FATAL ", "[INFO]", "[WARN]",
        "[ERROR]", "[DEBUG]", "[TRACE]", "INFO:", "WARN:", "ERROR:", "DEBUG:",
    ];
    for m in &level_markers {
        if s.contains(m) {
            return true;
        }
    }
    // Timestamp patterns: "2024-" or "2025-" or "2026-" (ISO-8601 year prefix)
    // Check for digit-digit-digit-digit-hyphen pattern at line start
    for line in s.lines().take(10) {
        let lb = line.trim_start().as_bytes();
        if lb.len() >= 5
            && lb[0].is_ascii_digit()
            && lb[1].is_ascii_digit()
            && lb[2].is_ascii_digit()
            && lb[3].is_ascii_digit()
            && lb[4] == b'-'
        {
            return true;
        }
        // Unix timestamp-like: large number at start (10+ digits)
        if lb.len() >= 10 && lb[..10].iter().all(|b| b.is_ascii_digit()) {
            return true;
        }
    }
    false
}

fn has_code_markers(sample: &[u8]) -> bool {
    let s = std::str::from_utf8(sample).unwrap_or("");
    let markers = [
        "fn ",
        "pub fn",
        "async fn",
        "impl ",
        "struct ",
        "enum ",
        "trait ", // Rust
        "def ",
        "class ",
        "import ",
        "from ",
        "return ", // Python/general
        "function ",
        "const ",
        "let ",
        "var ",
        "=>", // JS/TS
        "func ",
        "package ",
        "type ", // Go
        "public class",
        "private ",
        "protected ", // Java/C#
        "#include",
        "namespace ",
        "template<", // C/C++
        "SELECT ",
        "FROM ",
        "WHERE ",
        "INSERT INTO", // SQL
    ];
    for m in &markers {
        if s.contains(m) {
            return true;
        }
    }
    false
}

/// Estimate the number of tokens in `bytes` using content-class-aware ratios.
pub fn estimate_tokens(bytes: &[u8]) -> (f64, ContentClass) {
    let class = classify(bytes);
    let tokens = bytes.len() as f64 / class.bytes_per_token();
    (tokens, class)
}

/// Estimate tokens from a byte count with class pre-determined.
pub fn tokens_from_bytes(bytes: u64, class: ContentClass) -> f64 {
    bytes as f64 / class.bytes_per_token()
}

// ── Pricing ───────────────────────────────────────────────────────────────────

/// Per-model pricing in USD per million tokens.
#[derive(Debug, Clone, Copy)]
pub struct ModelPricing {
    pub input_per_mtok: f64,
    pub cache_read_per_mtok: f64,
}

/// Try loading a pricing entry from the file pointed to by
/// `PIGGYBANK_PRICING_FILE`. The file must be a JSON object keyed by model id
/// (or `"default"`), each value an object with `input_per_mtok` and
/// `cache_read_per_mtok` (both `f64`). Example:
///
/// ```json
/// {
///   "claude-opus-5": {"input_per_mtok": 15.0, "cache_read_per_mtok": 1.5},
///   "default":       {"input_per_mtok": 3.0,  "cache_read_per_mtok": 0.3}
/// }
/// ```
///
/// Returns `None` if the env var is unset, the file is unreadable, the
/// requested model id is absent, AND there is no `"default"` key in the file.
fn pricing_from_file(model_id: &str) -> Option<ModelPricing> {
    let path = std::env::var("PIGGYBANK_PRICING_FILE").ok()?;
    let raw = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let entry = v.get(model_id).or_else(|| v.get("default"))?;
    let input = entry.get("input_per_mtok")?.as_f64()?;
    let cache = entry.get("cache_read_per_mtok")?.as_f64()?;
    Some(ModelPricing {
        input_per_mtok: input,
        cache_read_per_mtok: cache,
    })
}

/// Look up pricing for a model id. Checks `PIGGYBANK_PRICING_FILE` first,
/// then the built-in table, then falls back to the `default` entry.
pub fn model_pricing(model_id: &str) -> (&'static str, ModelPricing) {
    // External override: operators can point PIGGYBANK_PRICING_FILE at a JSON
    // file with custom rates without rebuilding the binary.
    if let Some(p) = pricing_from_file(model_id) {
        return ("file", p);
    }

    // Prices are estimates based on Anthropic's public pricing page patterns.
    // Verify current rates at https://www.anthropic.com/pricing before using
    // for billing decisions.
    let table: &[(&str, ModelPricing)] = &[
        (
            "claude-fable-5-1",
            ModelPricing {
                input_per_mtok: 3.00,
                cache_read_per_mtok: 0.30,
            },
        ),
        (
            "claude-opus-5",
            ModelPricing {
                input_per_mtok: 15.00,
                cache_read_per_mtok: 1.50,
            },
        ),
        (
            "claude-sonnet-5",
            ModelPricing {
                input_per_mtok: 3.00,
                cache_read_per_mtok: 0.30,
            },
        ),
        (
            "claude-haiku-4-5-20251001",
            ModelPricing {
                input_per_mtok: 0.80,
                cache_read_per_mtok: 0.08,
            },
        ),
    ];

    for &(id, pricing) in table {
        if id == model_id {
            return (id, pricing);
        }
    }

    // Default: claude-sonnet-5 pricing
    (
        "default",
        ModelPricing {
            input_per_mtok: 3.00,
            cache_read_per_mtok: 0.30,
        },
    )
}

/// Resolve the model id from the `PIGGYBANK_MODEL` environment variable,
/// falling back to "default".
pub fn model_from_env() -> String {
    std::env::var("PIGGYBANK_MODEL").unwrap_or_else(|_| "default".to_string())
}

/// Compute USD saved given bytes saved and a model id string.
/// Uses prose class as a conservative default when actual content is unavailable.
pub fn usd_saved(bytes_saved: u64, model_id: &str) -> f64 {
    let tokens = tokens_from_bytes(bytes_saved, ContentClass::Prose);
    let (_, pricing) = model_pricing(model_id);
    tokens / 1_000_000.0 * pricing.input_per_mtok
}

/// Compute USD saved from a byte count using content-class-aware estimation.
pub fn usd_saved_classified(bytes_saved: u64, class: ContentClass, model_id: &str) -> f64 {
    let tokens = tokens_from_bytes(bytes_saved, class);
    let (_, pricing) = model_pricing(model_id);
    tokens / 1_000_000.0 * pricing.input_per_mtok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_json_by_opener() {
        assert_eq!(classify(b"{\"key\": \"value\"}"), ContentClass::Json);
        assert_eq!(classify(b"[1, 2, 3]"), ContentClass::Json);
        assert_eq!(classify(b"   {\"x\": 1}"), ContentClass::Json); // leading whitespace
    }

    #[test]
    fn classify_logs_by_level_marker() {
        assert_eq!(
            classify(b"2024-01-15 12:00:00 INFO some message"),
            ContentClass::Logs
        );
        assert_eq!(classify(b"[ERROR] failed to connect"), ContentClass::Logs);
        assert_eq!(
            classify(b"2024-01-15T12:00:00Z WARNING timeout"),
            ContentClass::Logs
        );
    }

    #[test]
    fn classify_code_by_keywords() {
        assert_eq!(
            classify(b"fn main() { println!(\"hello\"); }"),
            ContentClass::Code
        );
        assert_eq!(classify(b"def hello():\n    return 42"), ContentClass::Code);
        assert_eq!(
            classify(b"public class Foo { private int x; }"),
            ContentClass::Code
        );
    }

    #[test]
    fn classify_hex_by_density() {
        // 64-char sha256 hex string — >70% hex chars
        let hash = b"2c3d74da04dd0b5bb4dbfa3ccbeda37080d9e47e615250d65e5b990e27cc2edc";
        assert_eq!(classify(hash), ContentClass::Hex);
    }

    #[test]
    fn classify_prose_as_fallback() {
        assert_eq!(
            classify(b"The quick brown fox jumps over the lazy dog."),
            ContentClass::Prose
        );
    }

    #[test]
    fn estimate_tokens_non_zero() {
        let (tokens, class) = estimate_tokens(b"hello world");
        assert!(tokens > 0.0);
        assert_eq!(class, ContentClass::Prose);
    }

    #[test]
    fn model_pricing_known_models() {
        let (id, p) = model_pricing("claude-opus-5");
        assert_eq!(id, "claude-opus-5");
        assert!(p.input_per_mtok > 3.0, "opus should cost more than sonnet");

        let (id, p) = model_pricing("claude-haiku-4-5-20251001");
        assert_eq!(id, "claude-haiku-4-5-20251001");
        assert!(p.input_per_mtok < 1.0, "haiku should be cheapest");
    }

    #[test]
    fn model_pricing_unknown_falls_back_to_default() {
        let (id, _) = model_pricing("claude-unknown-99");
        assert_eq!(id, "default");
    }

    #[test]
    fn usd_saved_positive_for_nonzero_bytes() {
        let usd = usd_saved(1_000_000, "claude-sonnet-5");
        assert!(usd > 0.0);
    }

    #[test]
    fn bytes_per_token_ratios_ordered() {
        // hex should have the lowest (smallest) ratio = most tokens per byte
        assert!(ContentClass::Hex.bytes_per_token() < ContentClass::Json.bytes_per_token());
        assert!(ContentClass::Json.bytes_per_token() < ContentClass::Prose.bytes_per_token());
    }

    #[test]
    fn pricing_file_overrides_builtin() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(
            f,
            r#"{{"claude-opus-5":{{"input_per_mtok":99.0,"cache_read_per_mtok":9.9}}}}"#
        )
        .unwrap();
        let path = f.path().to_str().unwrap().to_string();
        std::env::set_var("PIGGYBANK_PRICING_FILE", &path);
        let (src, p) = model_pricing("claude-opus-5");
        std::env::remove_var("PIGGYBANK_PRICING_FILE");
        assert_eq!(src, "file");
        assert!((p.input_per_mtok - 99.0).abs() < 0.001);
        assert!((p.cache_read_per_mtok - 9.9).abs() < 0.001);
    }

    #[test]
    fn pricing_file_default_key_used_for_unknown_model() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(
            f,
            r#"{{"default":{{"input_per_mtok":5.0,"cache_read_per_mtok":0.5}}}}"#
        )
        .unwrap();
        let path = f.path().to_str().unwrap().to_string();
        std::env::set_var("PIGGYBANK_PRICING_FILE", &path);
        let (src, p) = model_pricing("some-future-model");
        std::env::remove_var("PIGGYBANK_PRICING_FILE");
        assert_eq!(src, "file");
        assert!((p.input_per_mtok - 5.0).abs() < 0.001);
    }

    #[test]
    fn pricing_file_missing_falls_through_to_builtin() {
        std::env::set_var("PIGGYBANK_PRICING_FILE", "/nonexistent/path/pricing.json");
        let (src, p) = model_pricing("claude-haiku-4-5-20251001");
        std::env::remove_var("PIGGYBANK_PRICING_FILE");
        assert_eq!(src, "claude-haiku-4-5-20251001");
        assert!((p.input_per_mtok - 0.80).abs() < 0.001);
    }
}
