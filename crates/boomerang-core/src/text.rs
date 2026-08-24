use crate::Store;

/// Private-use-area sentinel used to delimit Boomerang's own control lines
/// inside compressed text. Chosen because it essentially never appears in
/// real logs/text, so control lines can't collide with content without
/// needing an escaping scheme. This is a real limitation against
/// adversarial input, not a cryptographic guarantee — acceptable for text
/// this tool generates and re-parses itself.
const PUA: char = '\u{E000}';

#[derive(Clone, Copy)]
pub struct TextOptions {
    /// Collapse a run of >= this many *consecutive* identical lines into
    /// one copy plus a repeat marker. Must be >= 2.
    pub dedup_min_repeat: usize,
    /// Above this many lines, elide the middle and keep only the ends.
    pub elide_threshold_lines: usize,
    pub keep_head: usize,
    pub keep_tail: usize,
}

impl Default for TextOptions {
    fn default() -> Self {
        Self {
            dedup_min_repeat: 3,
            elide_threshold_lines: 60,
            keep_head: 15,
            keep_tail: 10,
        }
    }
}

/// Compress arbitrary text/log content via two independent, composable moves:
///
/// 1. **Consecutive-line dedup** — N identical lines in a row become one
///    copy plus a "(repeated N times)" marker. Lossless on its own.
/// 2. **Middle elision** — past `elide_threshold_lines`, keep the first
///    `keep_head` and last `keep_tail` lines and replace everything between
///    with a marker referencing the *exact original* middle span, stored in
///    `store`. Lossy in the view an LLM sees, but `decompress_text` (given
///    the same store) reconstructs it exactly — the guarantee is "nothing
///    is gone," not "nothing is hidden."
///
/// Non-UTF-8 input has no line structure to exploit, so it's stored whole
/// and handed back as a single elide marker rather than rejected.
pub fn compress_text(store: &Store, input: &[u8], opts: &TextOptions) -> std::io::Result<Vec<u8>> {
    debug_assert!(
        opts.dedup_min_repeat >= 2,
        "dedup_min_repeat < 2 collapses nothing"
    );

    let text = match std::str::from_utf8(input) {
        Ok(t) => t,
        Err(_) => {
            let id = store.put(input)?;
            return Ok(elide_marker(count_lines_bytes(input), &id).into_bytes());
        }
    };

    let lines: Vec<&str> = text.split('\n').collect();

    if lines.len() <= opts.elide_threshold_lines {
        return Ok(dedup_lines(&lines, opts.dedup_min_repeat)
            .join("\n")
            .into_bytes());
    }

    let keep_head = opts.keep_head.min(lines.len());
    let keep_tail = opts.keep_tail.min(lines.len() - keep_head);
    let head = &lines[..keep_head];
    let tail = &lines[lines.len() - keep_tail..];
    let middle = &lines[keep_head..lines.len() - keep_tail];

    let id = store.put(middle.join("\n").as_bytes())?;

    let mut out = dedup_lines(head, opts.dedup_min_repeat);
    out.push(elide_marker(middle.len(), &id));
    out.extend(dedup_lines(tail, opts.dedup_min_repeat));
    Ok(out.join("\n").into_bytes())
}

/// Reconstruct the original exactly, given the same `store` the content was
/// compressed against. This is the reversible half of the guarantee: the
/// LLM sees the smaller view from `compress_text`; anything that asks for
/// the rest gets exactly what was elided, byte for byte.
pub fn decompress_text(store: &Store, input: &[u8]) -> std::io::Result<Vec<u8>> {
    if let Some(id) = parse_whole_document_elide(input) {
        return store.get(&id);
    }

    let text = std::str::from_utf8(input)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let mut out: Vec<String> = Vec::new();

    for line in text.split('\n') {
        if let Some((_count, id)) = parse_elide(line) {
            let restored = store.get(&id)?;
            let restored = String::from_utf8(restored)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            out.extend(restored.split('\n').map(str::to_string));
        } else if let Some(count) = parse_repeat(line) {
            let last = out.last().cloned().unwrap_or_default();
            for _ in 1..count {
                out.push(last.clone());
            }
        } else {
            out.push(line.to_string());
        }
    }

    Ok(out.join("\n").into_bytes())
}

fn dedup_lines(lines: &[&str], min_repeat: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let mut run = 1;
        while i + run < lines.len() && lines[i + run] == line {
            run += 1;
        }
        let marker = repeat_marker(run);
        // Only collapse if it actually shrinks the output — marker overhead
        // (~20+ bytes) can exceed the savings for short lines with few
        // repeats, and applying a lossy rewrite that doesn't pay for itself
        // is pure downside.
        let collapsed_len = line.len() + 1 + marker.len();
        let literal_len = (line.len() + 1) * run;
        if run >= min_repeat && collapsed_len < literal_len {
            out.push(line.to_string());
            out.push(marker);
        } else {
            for _ in 0..run {
                out.push(line.to_string());
            }
        }
        i += run;
    }
    out
}

fn repeat_marker(count: usize) -> String {
    format!("{PUA}BOOMERANG:REPEAT:{count}{PUA}")
}

fn elide_marker(line_count: usize, id: &str) -> String {
    format!("{PUA}BOOMERANG:ELIDE:{line_count}:{id}{PUA}")
}

fn parse_repeat(line: &str) -> Option<usize> {
    strip_pua(line, "BOOMERANG:REPEAT:")?.parse().ok()
}

fn parse_elide(line: &str) -> Option<(usize, String)> {
    let body = strip_pua(line, "BOOMERANG:ELIDE:")?;
    let (count, id) = body.split_once(':')?;
    Some((count.parse().ok()?, id.to_string()))
}

/// If `input` is, in its entirety, exactly one elide marker (the whole
/// document was replaced by a single reference — either because it wasn't
/// UTF-8, or because keep_head/keep_tail elided everything), return the
/// referenced id so the caller can hand back the raw stored bytes directly
/// instead of running them through UTF-8 line parsing.
fn parse_whole_document_elide(input: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(input).ok()?;
    parse_elide(text).map(|(_, id)| id)
}

fn strip_pua<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let mut chars = line.chars();
    if chars.next() != Some(PUA) {
        return None;
    }
    chars.as_str().strip_prefix(prefix)?.strip_suffix(PUA)
}

fn count_lines_bytes(input: &[u8]) -> usize {
    input.iter().filter(|&&b| b == b'\n').count() + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> Store {
        let dir = std::env::temp_dir().join(format!("boomerang-text-test-{}", uuid_ish()));
        Store::open(&dir).unwrap()
    }

    fn uuid_ish() -> String {
        format!("{:?}-{}", std::time::SystemTime::now(), std::process::id())
    }

    fn assert_round_trips(store: &Store, input: &[u8], opts: &TextOptions) -> Vec<u8> {
        let compressed = compress_text(store, input, opts).unwrap();
        let restored = decompress_text(store, &compressed).unwrap();
        assert_eq!(
            restored, input,
            "decompress_text must reconstruct the original exactly"
        );
        compressed
    }

    #[test]
    fn small_input_round_trips_with_dedup_only() {
        let store = temp_store();
        let repeated = "ERROR: connection refused by upstream, retrying in 500ms";
        let input =
            format!("line one\n{repeated}\n{repeated}\n{repeated}\n{repeated}\nline five\n");
        let compressed = assert_round_trips(&store, input.as_bytes(), &TextOptions::default());
        assert!(
            compressed.len() < input.len(),
            "4x repeated line should shrink"
        );
    }

    #[test]
    fn short_repeated_line_that_would_not_pay_for_itself_is_left_literal() {
        // Marker overhead exceeds the savings here (short line, few repeats) —
        // the compressor must not apply a transform that grows the output.
        let store = temp_store();
        let input = "a\nx\nx\nx\nb\n";
        let compressed = assert_round_trips(&store, input.as_bytes(), &TextOptions::default());
        assert_eq!(
            compressed,
            input.as_bytes(),
            "no-op when collapsing wouldn't shrink anything"
        );
    }

    #[test]
    fn large_block_elides_and_still_round_trips() {
        let store = temp_store();
        let mut lines: Vec<String> = Vec::new();
        for i in 0..500 {
            lines.push(format!(
                "log line {i} some payload that takes up real space"
            ));
        }
        let input = lines.join("\n");

        let compressed = assert_round_trips(&store, input.as_bytes(), &TextOptions::default());
        assert!(
            compressed.len() < input.len() / 5,
            "elided view should be dramatically smaller: {} vs {}",
            compressed.len(),
            input.len()
        );

        // Prove it's actually eliding, not just re-embedding: a distinctive
        // line from deep in the middle must not appear in the compressed view.
        let compressed_text = String::from_utf8(compressed).unwrap();
        assert!(!compressed_text.contains("log line 250"));
    }

    #[test]
    fn non_utf8_input_round_trips_via_whole_document_store() {
        let store = temp_store();
        let input: &[u8] = &[0xff, 0xfe, 0x00, 0x01, 0xff, 0xff, 0x10];
        assert_round_trips(&store, input, &TextOptions::default());
    }

    #[test]
    fn repeated_block_inside_an_elided_middle_still_round_trips() {
        let store = temp_store();
        let mut lines: Vec<String> = Vec::new();
        for _ in 0..300 {
            lines.push("panic: same failure over and over".to_string());
        }
        let input = lines.join("\n");
        assert_round_trips(&store, input.as_bytes(), &TextOptions::default());
    }

    #[test]
    fn elided_content_is_exactly_recoverable_straight_from_the_store() {
        let store = temp_store();
        let mut lines: Vec<String> = Vec::new();
        for i in 0..200 {
            lines.push(format!("row {i}"));
        }
        let input = lines.join("\n");
        let opts = TextOptions::default();
        let compressed = compress_text(&store, input.as_bytes(), &opts).unwrap();

        let compressed_text = String::from_utf8(compressed).unwrap();
        let (_, id) = compressed_text
            .split('\n')
            .find_map(parse_elide)
            .expect("expected an elide marker in a 200-line input");

        let middle_lines: Vec<&str> = input.split('\n').collect();
        let middle = &middle_lines[opts.keep_head..middle_lines.len() - opts.keep_tail];
        assert_eq!(store.get(&id).unwrap(), middle.join("\n").into_bytes());
    }

    #[test]
    fn empty_input_round_trips() {
        let store = temp_store();
        assert_round_trips(&store, b"", &TextOptions::default());
    }
}
