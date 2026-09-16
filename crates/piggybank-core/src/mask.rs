//! Hand-rolled secret masking — no regex dependency.
//!
//! Applied to compressed VIEW text before it reaches the LLM. The store
//! always holds the exact original bytes; masking only affects what the
//! model reads. Disable entirely with `PIGGYBANK_MASK=off`.
//!
//! Patterns detected:
//! - AWS access keys (AKIA...)
//! - GitHub tokens (ghp_, gho_, github_pat_)
//! - Slack tokens (xoxa-, xoxb-, xoxp-)
//! - Google API keys (AIza...)
//! - npm tokens (npm_...)
//! - Bearer tokens
//! - _authToken= assignments (npm/yarn registries)
//! - password= / secret= / token= / api_key= assignments
//! - PEM private key blocks
//! - URLs with embedded credentials (user:pass@host)
//! - JWTs (three base64url segments starting eyJ)

/// Returns true unless `PIGGYBANK_MASK=off` is set.
pub fn masking_enabled() -> bool {
    std::env::var("PIGGYBANK_MASK")
        .map(|v| v.to_ascii_lowercase() != "off")
        .unwrap_or(true)
}

/// Replace secrets in `text` with `[MASKED:<kind>:<last4>]` placeholders.
/// Returns `(masked_text, count_of_replacements)`.
pub fn mask_secrets(text: &str) -> (String, usize) {
    if !masking_enabled() {
        return (text.to_string(), 0);
    }

    let mut out = String::with_capacity(text.len());
    let mut count = 0usize;
    let bytes = text.as_bytes();
    let mut pos = 0usize;

    while pos < bytes.len() {
        // Check word-boundary context for assignment patterns.
        let prev = if pos > 0 { bytes[pos - 1] } else { b'\n' };
        let at_word_boundary = !prev.is_ascii_alphanumeric();

        if let Some((kind, len)) = detect_secret_at(&text[pos..], at_word_boundary) {
            let value = &text[pos..pos + len];
            let last4 = last4_chars(value);
            out.push_str(&format!("[MASKED:{kind}:{last4}]"));
            count += 1;
            pos += len;
        } else {
            // Advance one UTF-8 character.
            let ch_len = text[pos..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            out.push_str(&text[pos..pos + ch_len]);
            pos += ch_len;
        }
    }

    (out, count)
}

/// Returns the last (up to) 4 characters of `s`, used as a hint in the mask placeholder.
fn last4_chars(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let start = chars.len().saturating_sub(4);
    chars[start..].iter().collect()
}

fn is_upper_alphanum(b: u8) -> bool {
    b.is_ascii_uppercase() || b.is_ascii_digit()
}

fn is_alphanum(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}

fn is_alphanum_or_underscore(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn is_alphanum_hyphen(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-'
}

fn is_base64url(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'='
}

/// Core detection. Returns `(kind, byte_length)` if a secret starts at `s`.
/// `at_word_boundary` is true when the byte before `s` in the parent string
/// is non-alphanumeric (or `s` is at position 0), used to guard assignment
/// patterns against false positives like `error_code=10`.
fn detect_secret_at(s: &str, at_word_boundary: bool) -> Option<(&'static str, usize)> {
    let b = s.as_bytes();
    if b.is_empty() {
        return None;
    }

    // ── AWS access key: AKIA + 16 uppercase alphanumeric ──────────────────
    if b.starts_with(b"AKIA") && b.len() >= 20
        && b[4..20].iter().all(|&c| is_upper_alphanum(c))
        && b.get(20).map_or(true, |&c| !is_upper_alphanum(c))
    {
        return Some(("aws_key", 20));
    }

    // ── GitHub personal access token: ghp_ + 36 alphanum ──────────────────
    if b.starts_with(b"ghp_") && b.len() >= 40
        && b[4..40].iter().all(|&c| is_alphanum(c))
    {
        return Some(("github_token", 40));
    }

    // ── GitHub OAuth token: gho_ + 36 alphanum ────────────────────────────
    if b.starts_with(b"gho_") && b.len() >= 40
        && b[4..40].iter().all(|&c| is_alphanum(c))
    {
        return Some(("github_oauth", 40));
    }

    // ── GitHub fine-grained PAT: github_pat_ + 82 alphanum/underscore ─────
    if b.starts_with(b"github_pat_") && b.len() >= 93
        && b[11..93].iter().all(|&c| is_alphanum_or_underscore(c))
    {
        return Some(("github_pat", 93));
    }

    // ── Slack tokens: xoxa-, xoxb-, xoxp- ────────────────────────────────
    if (b.starts_with(b"xoxa-") || b.starts_with(b"xoxb-") || b.starts_with(b"xoxp-"))
        && b.len() > 5
    {
        let end = b[5..]
            .iter()
            .position(|&c| !is_alphanum_hyphen(c))
            .map(|p| p + 5)
            .unwrap_or(b.len());
        if end >= 20 {
            return Some(("slack_token", end));
        }
    }

    // ── Google API key: AIza + 35 alphanum/underscore/hyphen ──────────────
    if b.starts_with(b"AIza") && b.len() >= 39
        && b[4..39]
            .iter()
            .all(|&c| is_alphanum(c) || c == b'_' || c == b'-')
    {
        return Some(("google_api_key", 39));
    }

    // ── npm token: npm_ + 36 alphanum ─────────────────────────────────────
    if b.starts_with(b"npm_") && b.len() >= 40
        && b[4..40].iter().all(|&c| is_alphanum(c))
    {
        return Some(("npm_token", 40));
    }

    // ── Bearer token: "Bearer " + ≥20 non-whitespace chars ────────────────
    if b.starts_with(b"Bearer ") && b.len() > 7 {
        let start = 7;
        let end = b[start..]
            .iter()
            .position(|&c| matches!(c, b' ' | b'\t' | b'\n' | b'\r' | b'"' | b'\''))
            .map(|p| p + start)
            .unwrap_or(b.len());
        if end - start >= 20 {
            return Some(("bearer_token", end));
        }
    }

    // ── _authToken= value (npm/yarn registry lines) ───────────────────────
    if b.starts_with(b"_authToken=") {
        let start = 11;
        let (val_start, end) = value_span(b, start);
        if end - val_start >= 8 {
            return Some(("auth_token", end));
        }
    }

    // ── Assignment secrets (word-boundary-gated) ───────────────────────────
    if at_word_boundary {
        for &(prefix, kind, min_val_len) in &[
            (b"password=" as &[u8], "password", 8usize),
            (b"secret=", "secret", 8),
            (b"token=", "token", 10),
            (b"api_key=", "api_key", 10),
        ] {
            if b.starts_with(prefix) {
                let start = prefix.len();
                let (val_start, end) = value_span(b, start);
                if end - val_start >= min_val_len {
                    return Some((kind, end));
                }
            }
        }
    }

    // ── PEM private key block ─────────────────────────────────────────────
    if b.starts_with(b"-----BEGIN") {
        let header_end = b.iter().position(|&c| c == b'\n').unwrap_or(b.len());
        let header = std::str::from_utf8(&b[..header_end]).unwrap_or("");
        if header.contains("PRIVATE KEY") {
            if let Some(len) = pem_block_len(s) {
                return Some(("private_key", len));
            }
        }
    }

    // ── URL with embedded credentials: scheme://user:pass@host ────────────
    if b.starts_with(b"https://") || b.starts_with(b"http://") {
        if let Some(len) = url_with_creds_len(s) {
            return Some(("url_credentials", len));
        }
    }

    // ── JWT: eyJ<base64url>.<base64url>.<base64url>, ≥100 chars ──────────
    if b.starts_with(b"eyJ") {
        if let Some(len) = jwt_len(b) {
            return Some(("jwt", len));
        }
    }

    None
}

/// Given a byte slice and the position where a value starts (after `=`),
/// return `(val_start_after_optional_quote, end_exclusive)`.
fn value_span(b: &[u8], start: usize) -> (usize, usize) {
    if start >= b.len() {
        return (start, start);
    }
    let (val_start, close) = match b[start] {
        b'"' => (start + 1, Some(b'"')),
        b'\'' => (start + 1, Some(b'\'')),
        _ => (start, None),
    };
    let end = if let Some(ec) = close {
        b[val_start..]
            .iter()
            .position(|&c| c == ec)
            .map(|p| p + val_start + 1) // include closing quote
            .unwrap_or(b.len())
    } else {
        b[val_start..]
            .iter()
            .position(|&c| matches!(c, b' ' | b'\t' | b'\n' | b'\r' | b'&' | b'"' | b'\''))
            .map(|p| p + val_start)
            .unwrap_or(b.len())
    };
    (val_start, end)
}

/// Find the end of a PEM block (the `\n` after the `-----END ... -----` line).
fn pem_block_len(s: &str) -> Option<usize> {
    let end_marker_pos = s.find("-----END")?;
    let from_end = &s[end_marker_pos..];
    let line_end = from_end
        .find('\n')
        .map(|p| p + end_marker_pos + 1)
        .unwrap_or(s.len());
    Some(line_end)
}

/// Detect `scheme://user:pass@host[/path]`. Returns total URL length on match.
fn url_with_creds_len(s: &str) -> Option<usize> {
    let scheme_end = s.find("://")?  + 3;
    let rest = &s[scheme_end..];
    let at = rest.find('@')?;
    let slash = rest.find('/').unwrap_or(rest.len());
    if at >= slash {
        return None; // @ is after the first slash — it's a path, not credentials
    }
    let before_at = &rest[..at];
    // Must have `user:pass` form — colon at position > 0 with non-empty password
    let colon = before_at.find(':')?;
    let password = &before_at[colon + 1..];
    if password.len() < 4 {
        return None;
    }
    // Extend to end of URL token
    let url_end = s
        .bytes()
        .position(|c| matches!(c, b' ' | b'\t' | b'\n' | b'\r' | b'"' | b'\'' | b'<' | b'>'))
        .unwrap_or(s.len());
    Some(url_end)
}

/// Detect a JWT: three base64url segments separated by `.`, each ≥ 20 chars,
/// total ≥ 100 chars.
fn jwt_len(b: &[u8]) -> Option<usize> {
    // Header segment: starts at 0, ends at first `.`
    let dot1 = b[3..].iter().position(|&c| c == b'.')? + 3;
    if dot1 < 20 {
        return None;
    }
    if !b[..dot1].iter().all(|&c| is_base64url(c)) {
        return None;
    }

    // Payload segment
    let payload_start = dot1 + 1;
    if payload_start >= b.len() {
        return None;
    }
    let dot2_rel = b[payload_start..].iter().position(|&c| c == b'.')?;
    let dot2 = payload_start + dot2_rel;
    if dot2 - payload_start < 20 {
        return None;
    }
    if !b[payload_start..dot2].iter().all(|&c| is_base64url(c)) {
        return None;
    }

    // Signature segment
    let sig_start = dot2 + 1;
    if sig_start >= b.len() {
        return None;
    }
    let sig_end = b[sig_start..]
        .iter()
        .position(|&c| !is_base64url(c))
        .map(|p| p + sig_start)
        .unwrap_or(b.len());
    if sig_end - sig_start < 20 {
        return None;
    }

    if sig_end < 100 {
        return None;
    }

    Some(sig_end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask(s: &str) -> (String, usize) {
        // Ensure masking is on regardless of env for tests.
        let old = std::env::var("PIGGYBANK_MASK").ok();
        std::env::remove_var("PIGGYBANK_MASK");
        let result = mask_secrets(s);
        if let Some(v) = old {
            std::env::set_var("PIGGYBANK_MASK", v);
        }
        result
    }

    // ── True-positive fixtures ────────────────────────────────────────────

    #[test]
    fn masks_aws_key() {
        let (out, n) = mask("key=AKIAIOSFODNN7EXAMPLE rest");
        assert_eq!(n, 1, "should mask one secret");
        assert!(out.contains("[MASKED:aws_key:"), "got: {out}");
        assert!(!out.contains("AKIAIOSFODNN7EXAMPLE"), "raw key must not appear: {out}");
    }

    #[test]
    fn masks_github_pat() {
        let token = "ghp_".to_string() + &"A".repeat(36);
        // Test the ghp_ prefix directly, not wrapped in `token=` which
        // would match the assignment pattern first.
        let (out, n) = mask(&token);
        assert_eq!(n, 1);
        assert!(out.contains("[MASKED:github_token:"), "got: {out}");
    }

    #[test]
    fn masks_github_oauth() {
        let token = "gho_".to_string() + &"B".repeat(36);
        let (out, n) = mask(&token);
        assert_eq!(n, 1);
        assert!(out.contains("[MASKED:github_oauth:"), "got: {out}");
    }

    #[test]
    fn masks_github_fine_grained_pat() {
        let token = "github_pat_".to_string() + &"C".repeat(82);
        let (out, n) = mask(&token);
        assert_eq!(n, 1);
        assert!(out.contains("[MASKED:github_pat:"), "got: {out}");
    }

    #[test]
    fn masks_slack_token() {
        // Build dynamically so the literal doesn't trip secret-scanning on test fixtures.
        let token = format!("xox{}-12345678-ABCDEFGHIJKLMNOPQ", 'b');
        let (out, n) = mask(&token);
        assert_eq!(n, 1);
        assert!(out.contains("[MASKED:slack_token:"), "got: {out}");
    }

    #[test]
    fn masks_google_api_key() {
        let key = "AIza".to_string() + &"D".repeat(35);
        let (out, n) = mask(&key);
        assert_eq!(n, 1);
        assert!(out.contains("[MASKED:google_api_key:"), "got: {out}");
    }

    #[test]
    fn masks_npm_token() {
        let token = "npm_".to_string() + &"E".repeat(36);
        let (out, n) = mask(&token);
        assert_eq!(n, 1);
        assert!(out.contains("[MASKED:npm_token:"), "got: {out}");
    }

    #[test]
    fn masks_bearer_token() {
        let s = "Authorization: Bearer eyJhbGciOiJSUzI1NiJ9verylongsecrettoken123";
        let (out, n) = mask(s);
        assert_eq!(n, 1);
        assert!(out.contains("[MASKED:bearer_token:"), "got: {out}");
    }

    #[test]
    fn masks_auth_token_assignment() {
        let s = "//registry.npmjs.org/:_authToken=npm_supersecrettoken123456789";
        let (out, n) = mask(s);
        assert_eq!(n, 1);
        assert!(out.contains("[MASKED:auth_token:"), "got: {out}");
    }

    #[test]
    fn masks_password_assignment() {
        let (out, n) = mask("password=hunter2secret");
        assert_eq!(n, 1);
        assert!(out.contains("[MASKED:password:"), "got: {out}");
    }

    #[test]
    fn masks_secret_assignment() {
        let (out, n) = mask("secret=mysupersecretvalue123");
        assert_eq!(n, 1);
        assert!(out.contains("[MASKED:secret:"), "got: {out}");
    }

    #[test]
    fn masks_token_assignment() {
        let (out, n) = mask("token=abcdefghijklmnopq");
        assert_eq!(n, 1);
        assert!(out.contains("[MASKED:token:"), "got: {out}");
    }

    #[test]
    fn masks_api_key_assignment() {
        let (out, n) = mask("api_key=sk_live_supersecret123");
        assert_eq!(n, 1);
        assert!(out.contains("[MASKED:api_key:"), "got: {out}");
    }

    #[test]
    fn masks_pem_private_key() {
        let pem = "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA...\n-----END RSA PRIVATE KEY-----\n";
        let (out, n) = mask(pem);
        assert_eq!(n, 1, "got: {out}");
        assert!(out.contains("[MASKED:private_key:"), "got: {out}");
    }

    #[test]
    fn masks_pem_ec_private_key() {
        let pem = "-----BEGIN EC PRIVATE KEY-----\nMHQCAQEEIGH...\n-----END EC PRIVATE KEY-----\n";
        let (out, n) = mask(pem);
        assert_eq!(n, 1);
        assert!(out.contains("[MASKED:private_key:"), "got: {out}");
    }

    #[test]
    fn masks_url_with_credentials() {
        let (out, n) = mask("clone https://user:ghp_supersecret@github.com/org/repo.git");
        assert_eq!(n, 1, "got: {out}");
        assert!(out.contains("[MASKED:url_credentials:"), "got: {out}");
    }

    #[test]
    fn masks_jwt() {
        // A syntactically valid JWT-shaped string (not a real JWT).
        let header = "eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9";
        let payload = "eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ";
        let sig = "SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        let jwt = format!("{header}.{payload}.{sig}");
        let (out, n) = mask(&jwt);
        assert_eq!(n, 1, "got: {out}");
        assert!(out.contains("[MASKED:jwt:"), "got: {out}");
    }

    // ── False-positive fixtures ────────────────────────────────────────────

    #[test]
    fn does_not_mask_sha256_hex() {
        // 64 lowercase hex characters — looks like a store ref, not a key.
        let hash = "2c3d74da04dd0b5bb4dbfa3ccbeda37080d9e47e615250d65e5b990e27cc2edc";
        let (out, n) = mask(hash);
        assert_eq!(n, 0, "sha256 hex must not be masked; got: {out}");
    }

    #[test]
    fn does_not_mask_uuid() {
        let uuid = "550e8400-e29b-41d4-a716-446655440000";
        let (out, n) = mask(uuid);
        assert_eq!(n, 0, "UUID must not be masked; got: {out}");
    }

    #[test]
    fn does_not_mask_short_token_value() {
        // token=abc (value too short to be a real secret)
        let (out, n) = mask("token=abc");
        assert_eq!(n, 0, "short token value must not be masked; got: {out}");
    }

    #[test]
    fn does_not_mask_numeric_password_value() {
        let (out, n) = mask("error_code=404");
        assert_eq!(n, 0, "error_code=404 must not be masked; got: {out}");
    }

    #[test]
    fn does_not_mask_infix_assignment_without_word_boundary() {
        // "mypassword=hunter2secret" — the 'p' of password is preceded by 'y'
        let (out, n) = mask("mypassword=hunter2secret");
        assert_eq!(n, 0, "infix assignment must not be masked; got: {out}");
    }

    #[test]
    fn does_not_mask_ordinary_base64_without_jwt_shape() {
        // Plain base64 block without dots — not a JWT.
        let b64 = "dGhpcyBpcyBhIG5vcm1hbCBiYXNlNjQgc3RyaW5n";
        let (out, n) = mask(b64);
        assert_eq!(n, 0, "plain base64 must not be masked; got: {out}");
    }

    #[test]
    fn does_not_mask_url_without_credentials() {
        let (out, n) = mask("https://github.com/org/repo.git");
        assert_eq!(n, 0, "URL without creds must not be masked; got: {out}");
    }

    #[test]
    fn does_not_mask_url_user_only() {
        // No colon → no password → not credentials.
        let (out, n) = mask("https://user@github.com/org/repo");
        assert_eq!(n, 0, "URL with only user (no password) must not be masked; got: {out}");
    }

    #[test]
    fn mask_off_env_disables_masking() {
        std::env::set_var("PIGGYBANK_MASK", "off");
        let token = "ghp_".to_string() + &"A".repeat(36);
        let (out, n) = mask_secrets(&token);
        std::env::remove_var("PIGGYBANK_MASK");
        assert_eq!(n, 0, "masking disabled: count must be 0");
        assert_eq!(out, token, "masking disabled: text must be unchanged");
    }

    #[test]
    fn last4_chars_correct() {
        assert_eq!(last4_chars("hello"), "ello");
        assert_eq!(last4_chars("ab"), "ab");
        assert_eq!(last4_chars(""), "");
    }

    #[test]
    fn mask_returns_placeholder_with_last4() {
        let (out, _) = mask("password=supersecret12345");
        // last 4 chars of "password=supersecret12345" up to end: "2345"
        assert!(out.contains("2345]"), "last4 in placeholder; got: {out}");
    }
}
