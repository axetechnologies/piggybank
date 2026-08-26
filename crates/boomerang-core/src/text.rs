use crate::markers::{escape_lines, strip_pua, unescape_lines, PUA};
use crate::Store;

#[derive(Clone, Copy, Debug)]
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
/// and handed back as a single raw-passthrough marker rather than rejected.
///
/// Every line is escaped (`escape_lines`) before any dedup/elision logic
/// runs, and unescaped (`unescape_lines`) as the last step of decompression
/// — see `markers.rs`. Without this, genuine input containing a line that
/// happens to look like one of our own control markers (e.g. embedding a
/// real store ref id) would decompress into unrelated content substituted
/// in from wherever that id pointed — confirmed as a real bug, not a
/// theoretical one, before this existed.
pub fn compress_text(store: &Store, input: &[u8], opts: &TextOptions) -> std::io::Result<Vec<u8>> {
    debug_assert!(
        opts.dedup_min_repeat >= 2,
        "dedup_min_repeat < 2 collapses nothing"
    );

    let text = match std::str::from_utf8(input) {
        Ok(t) => t,
        Err(_) => {
            // Raw bytes, never escaped (escaping is a text/line concept) —
            // decompress_text's raw_marker check returns these untouched.
            let id = store.put(input)?;
            return Ok(raw_marker(&id).into_bytes());
        }
    };

    let escaped = escape_lines(text);
    let lines: Vec<&str> = escaped.split('\n').collect();

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

    let mut out = dedup_lines(head, opts.dedup_min_repeat);
    if !middle.is_empty() {
        // "".split('\n') yields one empty element, not zero - eliding an
        // empty middle (keep_head+keep_tail covering the whole input) would
        // otherwise store "" and reintroduce a line that was never there,
        // as well as being a marker that elides nothing.
        let id = store.put(middle.join("\n").as_bytes())?;
        out.push(elide_marker(middle.len(), &id));
    }
    out.extend(dedup_lines(tail, opts.dedup_min_repeat));
    Ok(out.join("\n").into_bytes())
}

/// Reconstruct the original exactly, given the same `store` the content was
/// compressed against. This is the reversible half of the guarantee: the
/// LLM sees the smaller view from `compress_text`; anything that asks for
/// the rest gets exactly what was elided, byte for byte.
pub fn decompress_text(store: &Store, input: &[u8]) -> std::io::Result<Vec<u8>> {
    // The raw (non-UTF-8) fallback path is the only thing that ever
    // produces this marker, and its content was never escaped — return it
    // untouched, deliberately *not* running unescape_lines on it.
    if let Some(id) = parse_raw_marker(input) {
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

    Ok(unescape_lines(&out.join("\n")).into_bytes())
}

/// Confirm every elided/raw span referenced by a compressed view still
/// resolves in `store`, without reconstructing the content -
/// `Store::exists` per reference, not `Store::get`. Cheaper than
/// `decompress_text` when a caller only needs to know reconstruction is
/// still possible right now.
pub fn verify_text_with_store(store: &Store, input: &[u8]) -> std::io::Result<crate::VerifyResult> {
    let mut result = crate::VerifyResult::default();
    if let Some(id) = parse_raw_marker(input) {
        result.check(store, &id)?;
        return Ok(result.finish());
    }
    let Ok(text) = std::str::from_utf8(input) else {
        return Ok(result.finish()); // shouldn't happen alongside a failed raw-marker parse, but not this function's job to report that
    };
    for line in text.split('\n') {
        if let Some((_count, id)) = parse_elide(line) {
            result.check(store, &id)?;
        }
    }
    Ok(result.finish())
}

/// Budget-constrained compression: fit text into at most `max_bytes`
/// while maximizing information density. Tries normal compression first;
/// if the result exceeds the budget, forces elision and progressively
/// shrinks the kept head/tail until the view fits. Elided content is
/// always stored for recovery via `retrieve`.
///
/// Returns the same format as `compress_text` — `decompress_text` works
/// unchanged. The only difference is the *amount* elided, which is
/// driven by the budget rather than fixed thresholds.
///
/// Non-UTF-8 input stores the whole blob and returns a raw marker;
/// no further shrinking is possible without understanding the format.
pub fn compress_text_budget(
    store: &Store,
    input: &[u8],
    max_bytes: usize,
) -> std::io::Result<Vec<u8>> {
    let normal = compress_text(store, input, &TextOptions::default())?;
    if normal.len() <= max_bytes {
        return Ok(normal);
    }

    if std::str::from_utf8(input).is_err() {
        return Ok(normal);
    }

    let mut head = 15usize;
    let mut tail = 10usize;
    loop {
        let opts = TextOptions {
            dedup_min_repeat: 3,
            elide_threshold_lines: 1,
            keep_head: head,
            keep_tail: tail,
        };
        let compressed = compress_text(store, input, &opts)?;
        if compressed.len() <= max_bytes || (head == 0 && tail == 0) {
            return Ok(compressed);
        }
        head /= 2;
        tail /= 2;
    }
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

/// Distinct from `elide_marker` specifically so decompression can tell the
/// two apart unambiguously. Both can end up as "the entire compressed
/// document is one marker line" (raw: always; elide: only when custom
/// keep_head=keep_tail=0), but only content behind an elide marker was ever
/// escaped — content behind a raw marker (non-UTF-8 input) never was, and
/// must never be run through unescape_lines. Reusing one marker for both
/// and trying to infer which case applies from context is exactly the kind
/// of ambiguity this whole file exists to avoid.
fn raw_marker(id: &str) -> String {
    format!("{PUA}BOOMERANG:RAW:{id}{PUA}")
}

fn parse_repeat(line: &str) -> Option<usize> {
    strip_pua(line, "BOOMERANG:REPEAT:")?.parse().ok()
}

fn parse_elide(line: &str) -> Option<(usize, String)> {
    let body = strip_pua(line, "BOOMERANG:ELIDE:")?;
    let (count, id) = body.split_once(':')?;
    Some((count.parse().ok()?, id.to_string()))
}

fn parse_raw_marker(input: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(input).ok()?;
    strip_pua(text, "BOOMERANG:RAW:").map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> Store {
        temp_store_with_dir().0
    }

    fn temp_store_with_dir() -> (Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("boomerang-text-test-{}", uuid_ish()));
        (Store::open(&dir).unwrap(), dir)
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

    #[test]
    fn input_containing_a_literal_marker_line_round_trips_and_does_not_leak() {
        // Confirmed real bug before escape_lines/unescape_lines existed:
        // this exact shape of input - a line that happens to look like a
        // real ELIDE marker referencing a store id that genuinely exists
        // (from unrelated prior content) - decompressed into that
        // unrelated stored content being substituted in. Not just wrong
        // output: a cross-content leak.
        let store = temp_store();

        let secret = "TOP SECRET content from a completely unrelated compression";
        let mut filler: Vec<String> = (0..80).map(|i| format!("line {i}")).collect();
        filler.insert(40, secret.to_string());
        filler.extend((80..160).map(|i| format!("line {i}")));
        let with_secret = filler.join("\n");
        let compressed_with_secret =
            compress_text(&store, with_secret.as_bytes(), &TextOptions::default()).unwrap();
        let real_ref = String::from_utf8(compressed_with_secret)
            .unwrap()
            .lines()
            .find_map(|l| parse_elide(l).map(|(_, id)| id))
            .expect("expected an elide marker referencing the secret's storage");

        let forged = format!(
            "innocent line one\n{PUA}BOOMERANG:ELIDE:1:{real_ref}{PUA}\ninnocent line three"
        );
        let compressed = compress_text(&store, forged.as_bytes(), &TextOptions::default()).unwrap();
        let restored = decompress_text(&store, &compressed).unwrap();
        assert_eq!(
            restored,
            forged.into_bytes(),
            "must reconstruct the literal forged-looking input, not substitute in the referenced secret"
        );
        assert!(
            !String::from_utf8_lossy(&restored).contains("TOP SECRET"),
            "must not leak unrelated stored content"
        );
    }

    #[test]
    fn already_escaped_looking_input_round_trips() {
        let store = temp_store();
        let input = format!("before\n{PUA}\u{E001}already escaped looking\nafter");
        assert_round_trips(&store, input.as_bytes(), &TextOptions::default());
    }

    #[test]
    fn keep_head_and_tail_covering_the_whole_input_elides_nothing() {
        // Found by proptest, minimized from a 3-line input: when
        // keep_head+keep_tail >= total lines, the middle span is empty.
        // "".split('\n') yields one empty element, not zero, so eliding an
        // empty middle used to silently reintroduce a line that was never
        // there. Must not push an elide marker at all in this case.
        let store = temp_store();
        let opts = TextOptions {
            dedup_min_repeat: 2,
            elide_threshold_lines: 1,
            keep_head: 1,
            keep_tail: 2,
        };
        assert_round_trips(&store, b"\n\n", &opts);
    }

    #[test]
    fn verify_passes_for_a_real_elided_reference() {
        let store = temp_store();
        let big = (0..200)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let compressed = compress_text(&store, big.as_bytes(), &TextOptions::default()).unwrap();
        let result = verify_text_with_store(&store, &compressed).unwrap();
        assert!(result.ok);
        assert_eq!(result.checked_refs, 1);
    }

    #[test]
    fn verify_reports_missing_when_elided_content_is_deleted() {
        let (store, dir) = temp_store_with_dir();
        let big = (0..200)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let compressed = compress_text(&store, big.as_bytes(), &TextOptions::default()).unwrap();
        let compressed_text = String::from_utf8(compressed.clone()).unwrap();
        let (_, id) = compressed_text.lines().find_map(parse_elide).unwrap();
        std::fs::remove_file(dir.join(&id)).unwrap();

        let result = verify_text_with_store(&store, &compressed).unwrap();
        assert!(!result.ok);
        assert_eq!(result.missing_refs, vec![id]);
    }

    #[test]
    fn budget_returns_normal_when_it_fits() {
        let store = temp_store();
        let input = "short\nlog\noutput";
        let normal = compress_text(&store, input.as_bytes(), &TextOptions::default()).unwrap();
        let budget = compress_text_budget(&store, input.as_bytes(), 10_000).unwrap();
        assert_eq!(budget, normal);
    }

    #[test]
    fn budget_shrinks_to_fit() {
        let store = temp_store();
        let input: String = (0..500)
            .map(|i| format!("log line {i} with some payload data"))
            .collect::<Vec<_>>()
            .join("\n");
        let budget = 500;
        let compressed = compress_text_budget(&store, input.as_bytes(), budget).unwrap();
        assert!(
            compressed.len() <= budget,
            "must fit within budget: {} vs {budget}",
            compressed.len()
        );
        let restored = decompress_text(&store, &compressed).unwrap();
        assert_eq!(restored, input.as_bytes(), "must still round-trip exactly");
    }

    #[test]
    fn budget_extreme_shrink_still_round_trips() {
        let store = temp_store();
        let input: String = (0..1000)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let compressed = compress_text_budget(&store, input.as_bytes(), 200).unwrap();
        let restored = decompress_text(&store, &compressed).unwrap();
        assert_eq!(restored, input.as_bytes());
    }

    #[test]
    fn budget_non_utf8_returns_raw_marker() {
        let store = temp_store();
        let input: &[u8] = &[0xff, 0xfe, 0x00, 0x01];
        let compressed = compress_text_budget(&store, input, 2).unwrap();
        let restored = decompress_text(&store, &compressed).unwrap();
        assert_eq!(restored, input);
    }

    #[test]
    fn verify_on_small_input_with_no_elision_is_trivially_ok() {
        let store = temp_store();
        let compressed =
            compress_text(&store, b"just a few lines\nhere", &TextOptions::default()).unwrap();
        let result = verify_text_with_store(&store, &compressed).unwrap();
        assert!(result.ok);
        assert_eq!(result.checked_refs, 0);
    }

    mod proptests {
        use super::*;
        use proptest::prelude::*;

        /// Lines, deliberately biased toward marker-shaped and
        /// PUA-containing content rather than uniformly random text - the
        /// collision-prone area this module cares most about, and exactly
        /// the shape that caught the cross-content leak bug.
        fn arb_line() -> impl Strategy<Value = String> {
            prop_oneof![
                4 => "[a-zA-Z0-9 ]{0,20}",
                1 => Just(format!("{PUA}BOOMERANG:ELIDE:5:deadbeefdeadbeefdeadbeefdeadbeefdeadbeef{PUA}")),
                1 => Just(format!("{PUA}BOOMERANG:REPEAT:3{PUA}")),
                1 => Just(format!("{PUA}BOOMERANG:RAW:deadbeefdeadbeefdeadbeefdeadbeefdeadbeef{PUA}")),
                1 => "[a-zA-Z]{0,8}".prop_map(|s| format!("{PUA}{s}")),
                1 => "[a-zA-Z]{0,8}".prop_map(|s| format!("{PUA}\u{E001}{s}")),
            ]
        }

        fn arb_text() -> impl Strategy<Value = String> {
            prop::collection::vec(arb_line(), 0..40).prop_map(|lines| lines.join("\n"))
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(512))]

            #[test]
            fn arbitrary_text_round_trips(text in arb_text(), opts in arb_opts()) {
                let store = temp_store();
                let compressed = compress_text(&store, text.as_bytes(), &opts).unwrap();
                let restored = decompress_text(&store, &compressed).unwrap();
                prop_assert_eq!(restored, text.into_bytes());
            }

            /// Same property as json.rs's: verify must never false-flag a
            /// view produced against a store that's still fully intact.
            #[test]
            fn verify_never_false_flags_an_intact_store(text in arb_text(), opts in arb_opts()) {
                let store = temp_store();
                let compressed = compress_text(&store, text.as_bytes(), &opts).unwrap();
                let result = verify_text_with_store(&store, &compressed).unwrap();
                prop_assert!(result.ok);
                prop_assert!(result.missing_refs.is_empty());
            }
        }

        /// Vary the thresholds too, not just content - default options
        /// (60-line threshold) rarely trigger elision on the short
        /// generated inputs above, so most runs would only exercise dedup.
        fn arb_opts() -> impl Strategy<Value = TextOptions> {
            (2..5usize, 1..20usize, 0..5usize, 0..5usize).prop_map(
                |(dedup_min_repeat, elide_threshold_lines, keep_head, keep_tail)| TextOptions {
                    dedup_min_repeat,
                    elide_threshold_lines,
                    keep_head,
                    keep_tail,
                },
            )
        }
    }
}
