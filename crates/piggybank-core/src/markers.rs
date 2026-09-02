/// Private-use-area sentinel used to delimit Piggybank's own control lines
/// inside compressed text. Chosen because it essentially never appears in
/// real logs/text/code, so control lines usually can't collide with content.
/// "Usually" is not good enough on its own — see `escape_lines` below, which
/// closes the gap for the case where genuine input *does* contain a line
/// that happens to start with this character.
pub(crate) const PUA: char = '\u{E000}';

/// Marks a line as "escaped": the real content starts with `PUA` but must
/// not be interpreted as one of our control markers. See `escape_lines`.
const ESCAPE_MARK: char = '\u{E001}';

pub(crate) fn strip_pua<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let mut chars = line.chars();
    if chars.next() != Some(PUA) {
        return None;
    }
    chars.as_str().strip_prefix(prefix)?.strip_suffix(PUA)
}

/// Escape every line that starts with `PUA` — exactly the set of lines that
/// could otherwise be mistaken for one of our own control markers — by
/// prepending `PUA` + `ESCAPE_MARK`. Confirmed as a real, not theoretical,
/// gap before this existed: genuine text containing a coincidental
/// marker-shaped line (e.g. one embedding a real store ref id) would
/// silently decompress into *unrelated stored content substituted in from
/// wherever that id pointed* — a cross-content leak, not just wrong output.
///
/// Together with `unescape_lines`, this is a bijection: encoding always adds
/// exactly one escape layer to a line starting with `PUA` (including a line
/// that already looks escaped, since `PUA ESCAPE_MARK ...` itself starts
/// with `PUA`), and decoding always removes exactly one. Same pattern as
/// `escape_reserved_keys` in `json.rs`, applied to lines instead of object
/// keys — see that function's doc comment for why the "escape the escaper"
/// case needs this specific shape to stay correct.
pub(crate) fn escape_lines(text: &str) -> String {
    text.split('\n')
        .map(|line| {
            if line.starts_with(PUA) {
                format!("{PUA}{ESCAPE_MARK}{line}")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Reverses `escape_lines` exactly. Must run only after all real control
/// markers have already been resolved — any line still carrying the escape
/// prefix at that point was added by `escape_lines` and nothing else, so
/// stripping it unconditionally is safe.
pub(crate) fn unescape_lines(text: &str) -> String {
    text.split('\n')
        .map(|line| {
            let mut chars = line.chars();
            if chars.next() == Some(PUA) && chars.next() == Some(ESCAPE_MARK) {
                chars.as_str()
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_round_trips(text: &str) {
        let escaped = escape_lines(text);
        let restored = unescape_lines(&escaped);
        assert_eq!(
            restored, text,
            "escape_lines/unescape_lines must be a bijection"
        );
    }

    #[test]
    fn ordinary_text_is_untouched() {
        let text = "hello\nworld\nno special chars here";
        assert_eq!(escape_lines(text), text);
        assert_round_trips(text);
    }

    #[test]
    fn a_line_that_looks_like_a_real_marker_round_trips() {
        assert_round_trips("before\n\u{E000}PIGGYBANK:ELIDE:5:deadbeef\u{E000}\nafter");
    }

    #[test]
    fn an_already_escaped_looking_line_round_trips() {
        assert_round_trips("before\n\u{E000}\u{E001}already escaped looking\nafter");
    }
}
