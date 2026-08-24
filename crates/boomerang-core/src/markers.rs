/// Private-use-area sentinel used to delimit Boomerang's own control lines
/// inside compressed text. Chosen because it essentially never appears in
/// real logs/text/code, so control lines can't collide with content without
/// needing an escaping scheme. A real limitation against adversarial input,
/// not a cryptographic guarantee — acceptable for text this tool generates
/// and re-parses itself.
pub(crate) const PUA: char = '\u{E000}';

pub(crate) fn strip_pua<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let mut chars = line.chars();
    if chars.next() != Some(PUA) {
        return None;
    }
    chars.as_str().strip_prefix(prefix)?.strip_suffix(PUA)
}
