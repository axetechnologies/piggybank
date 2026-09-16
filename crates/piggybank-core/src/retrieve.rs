//! Partial-retrieval helpers for the `retrieve` MCP tool.
//!
//! After fetching the exact bytes from the content store, these functions
//! slice the result by lines, grep substring, head/tail, and optionally
//! apply a byte budget (using the same elision mechanics as compress_text_budget).

/// Parameters for a partial-retrieve operation.
#[derive(Debug, Default, Clone)]
pub struct RetrieveOpts {
    /// 1-based inclusive line range, e.g. "10-20" → lines 10..=20.
    pub lines: Option<(usize, usize)>,
    /// Substring (or simple `*`/`?` glob) to match. Case-sensitive.
    pub grep: Option<String>,
    /// Lines of context before and after each grep match.
    pub context: usize,
    /// Return only the first N lines.
    pub head: Option<usize>,
    /// Return only the last N lines.
    pub tail: Option<usize>,
    /// If the resulting slice still exceeds this byte count, apply the
    /// same anomaly-ranked elision as compress_text_budget.
    pub max_bytes: Option<usize>,
}

/// Result of a partial-retrieve operation.
pub struct RetrieveResult {
    /// The sliced content. ELIDE markers reference the same store as the
    /// original retrieve, and can be fetched by the caller with another
    /// `retrieve` call carrying the embedded ref id.
    pub content: Vec<u8>,
    /// Total line count of the original stored bytes.
    pub total_lines: usize,
    /// Total byte count of the original stored bytes.
    pub total_bytes: usize,
    /// Human-readable description of which slice was returned, e.g.
    /// "lines 10-20 of 500" or "head 20 of 500 lines".
    pub slice_description: String,
}

/// Apply `opts` to the `bytes` retrieved from the store.
///
/// If `opts` has no slice params set, returns the full content unchanged
/// (total_lines/total_bytes still populated).
///
/// When `max_bytes` is set and the selection exceeds it, calls back into
/// `compress_text_budget` to apply elision within the budget. The store is
/// therefore needed only when `max_bytes` is set.
pub fn apply_retrieve_opts(
    bytes: &[u8],
    opts: &RetrieveOpts,
    store: Option<&crate::Store>,
) -> std::io::Result<RetrieveResult> {
    let total_bytes = bytes.len();

    let text = match std::str::from_utf8(bytes) {
        Ok(t) => t,
        Err(_) => {
            // Binary content: can't slice by lines. Return as-is.
            return Ok(RetrieveResult {
                content: bytes.to_vec(),
                total_lines: 0,
                total_bytes,
                slice_description: "binary content (not sliceable)".into(),
            });
        }
    };

    let all_lines: Vec<&str> = text.lines().collect();
    let total_lines = all_lines.len();

    // Determine which lines to select (0-based indices).
    let selected: Vec<usize> = select_lines(&all_lines, opts);

    let slice_desc = describe_slice(&selected, total_lines, opts);

    // Build the content string from selected lines.
    let selected_lines: Vec<&str> = selected.iter().map(|&i| all_lines[i]).collect();
    let selected_text = selected_lines.join("\n");

    // Apply max_bytes budget if needed.
    let content_bytes: Vec<u8> = if let (Some(max), Some(store)) = (opts.max_bytes, store) {
        let raw = selected_text.as_bytes();
        if raw.len() <= max {
            raw.to_vec()
        } else {
            crate::compress_text_budget(store, raw, max)?
        }
    } else {
        selected_text.into_bytes()
    };

    Ok(RetrieveResult {
        content: content_bytes,
        total_lines,
        total_bytes,
        slice_description: slice_desc,
    })
}

fn select_lines(lines: &[&str], opts: &RetrieveOpts) -> Vec<usize> {
    let n = lines.len();

    // Lines filter (1-based inclusive).
    if let Some((start, end)) = opts.lines {
        let s = start.saturating_sub(1).min(n);
        let e = end.min(n);
        return (s..e).collect();
    }

    // Grep filter.
    if let Some(ref pattern) = opts.grep {
        let mut matched: Vec<usize> = (0..n).filter(|&i| glob_match(pattern, lines[i])).collect();

        if opts.context > 0 {
            let mut with_ctx: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
            for &m in &matched {
                let start = m.saturating_sub(opts.context);
                let end = (m + opts.context + 1).min(n);
                for i in start..end {
                    with_ctx.insert(i);
                }
            }
            matched = with_ctx.into_iter().collect();
        }
        return matched;
    }

    // Head.
    if let Some(h) = opts.head {
        return (0..h.min(n)).collect();
    }

    // Tail.
    if let Some(t) = opts.tail {
        let start = n.saturating_sub(t);
        return (start..n).collect();
    }

    // No filter: all lines.
    (0..n).collect()
}

fn describe_slice(selected: &[usize], total_lines: usize, opts: &RetrieveOpts) -> String {
    if selected.len() == total_lines {
        return format!("all {total_lines} lines");
    }
    if let Some((s, e)) = opts.lines {
        let actual_end = e.min(total_lines);
        return format!(
            "lines {s}-{actual_end} of {total_lines} ({} lines returned)",
            selected.len()
        );
    }
    if let Some(ref pat) = opts.grep {
        let ctx = if opts.context > 0 {
            format!(" ±{} context", opts.context)
        } else {
            String::new()
        };
        return format!(
            "grep {:?}{} → {} of {total_lines} lines",
            pat,
            ctx,
            selected.len()
        );
    }
    if let Some(h) = opts.head {
        return format!("head {} of {total_lines} lines", h.min(total_lines));
    }
    if let Some(t) = opts.tail {
        return format!("tail {} of {total_lines} lines", t.min(total_lines));
    }
    format!("{} of {total_lines} lines", selected.len())
}

/// Simple glob matcher supporting `*` (any substring) and `?` (any char).
/// Falls back to plain substring match when there are no glob chars.
fn glob_match(pattern: &str, text: &str) -> bool {
    if !pattern.contains('*') && !pattern.contains('?') {
        return text.contains(pattern);
    }
    glob_match_inner(pattern.as_bytes(), text.as_bytes())
}

fn glob_match_inner(pat: &[u8], text: &[u8]) -> bool {
    match (pat.first(), text.first()) {
        (None, None) => true,
        (Some(&b'*'), _) => {
            // '*' matches zero or more chars
            glob_match_inner(&pat[1..], text)
                || (!text.is_empty() && glob_match_inner(pat, &text[1..]))
        }
        (Some(&b'?'), Some(_)) => glob_match_inner(&pat[1..], &text[1..]),
        (Some(p), Some(t)) if p == t => glob_match_inner(&pat[1..], &text[1..]),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_text(n: usize) -> String {
        (0..n)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn no_opts_returns_all() {
        let text = make_text(10);
        let result = apply_retrieve_opts(text.as_bytes(), &RetrieveOpts::default(), None).unwrap();
        assert_eq!(result.total_lines, 10);
        assert_eq!(result.content, text.as_bytes());
        assert!(result.slice_description.contains("all 10 lines"));
    }

    #[test]
    fn lines_range_slices_correctly() {
        let text = make_text(20);
        let opts = RetrieveOpts {
            lines: Some((5, 10)),
            ..Default::default()
        };
        let result = apply_retrieve_opts(text.as_bytes(), &opts, None).unwrap();
        assert_eq!(result.total_lines, 20);
        let content = String::from_utf8(result.content).unwrap();
        assert!(content.contains("line 4"), "line 5 (0-based 4)");
        assert!(content.contains("line 9"), "line 10 (0-based 9)");
        assert!(!content.contains("line 10"), "line 11 excluded");
        assert!(result.slice_description.contains("lines 5-10 of 20"));
    }

    #[test]
    fn head_returns_first_n() {
        let text = make_text(50);
        let opts = RetrieveOpts {
            head: Some(5),
            ..Default::default()
        };
        let result = apply_retrieve_opts(text.as_bytes(), &opts, None).unwrap();
        let content = String::from_utf8(result.content).unwrap();
        assert_eq!(content.lines().count(), 5, "head 5 should give 5 lines");
        assert!(content.contains("line 0"));
        assert!(!content.contains("line 5"));
        assert!(result.slice_description.contains("head 5"));
    }

    #[test]
    fn tail_returns_last_n() {
        let text = make_text(50);
        let opts = RetrieveOpts {
            tail: Some(3),
            ..Default::default()
        };
        let result = apply_retrieve_opts(text.as_bytes(), &opts, None).unwrap();
        let content = String::from_utf8(result.content).unwrap();
        let line_count = content.lines().count();
        assert_eq!(line_count, 3, "tail 3 should give 3 lines");
        assert!(content.contains("line 47"));
        assert!(content.contains("line 49"));
    }

    #[test]
    fn grep_finds_matching_lines() {
        let text = "alpha\nbeta\nalpha again\ngamma\nalpha third";
        let opts = RetrieveOpts {
            grep: Some("alpha".into()),
            ..Default::default()
        };
        let result = apply_retrieve_opts(text.as_bytes(), &opts, None).unwrap();
        let content = String::from_utf8(result.content).unwrap();
        let lcount = content.lines().count();
        assert_eq!(lcount, 3, "grep should find 3 alpha lines");
        assert!(!content.contains("beta"));
        assert!(!content.contains("gamma"));
    }

    #[test]
    fn grep_with_context() {
        let text = "line 0\nline 1\nERROR here\nline 3\nline 4";
        let opts = RetrieveOpts {
            grep: Some("ERROR".into()),
            context: 1,
            ..Default::default()
        };
        let result = apply_retrieve_opts(text.as_bytes(), &opts, None).unwrap();
        let content = String::from_utf8(result.content).unwrap();
        assert!(content.contains("line 1"), "context before match");
        assert!(content.contains("ERROR here"));
        assert!(content.contains("line 3"), "context after match");
        assert!(!content.contains("line 0"), "too far before");
        assert!(!content.contains("line 4"), "too far after");
    }

    #[test]
    fn glob_star_matches() {
        assert!(glob_match("*.rs", "src/main.rs"));
        assert!(glob_match("error*", "error[E0308]: mismatched types"));
        assert!(!glob_match("*.rs", "src/main.py"));
    }

    #[test]
    fn glob_question_matches_single_char() {
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "ac"));
    }

    #[test]
    fn binary_content_returned_as_is() {
        let bytes: Vec<u8> = vec![0xff, 0xfe, 0x00, 0x01];
        let result = apply_retrieve_opts(&bytes, &RetrieveOpts::default(), None).unwrap();
        assert_eq!(result.content, bytes);
        assert!(result.slice_description.contains("binary"));
    }

    #[test]
    fn lines_range_clamped_to_total() {
        let text = make_text(5);
        let opts = RetrieveOpts {
            lines: Some((1, 100)),
            ..Default::default()
        };
        let result = apply_retrieve_opts(text.as_bytes(), &opts, None).unwrap();
        assert_eq!(result.total_lines, 5);
        let content_str = String::from_utf8(result.content).unwrap();
        assert_eq!(content_str.lines().count(), 5);
    }
}
