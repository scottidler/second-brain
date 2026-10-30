use super::*;

fn words(n: usize) -> String {
    (0..n).map(|i| format!("w{i}")).collect::<Vec<_>>().join(" ")
}

#[test]
fn blank_text_gives_no_windows() {
    assert!(chunk(&WhitespaceSegmenter, 512, "", "").expect("chunk").is_empty());
    assert!(
        chunk(&WhitespaceSegmenter, 512, "", "  \n\t ")
            .expect("chunk")
            .is_empty()
    );
}

#[test]
fn short_text_is_one_verbatim_window() {
    let text = "alpha  beta\ngamma";
    assert_eq!(chunk(&WhitespaceSegmenter, 512, "", text).expect("chunk"), vec![text]);
}

#[test]
fn text_exactly_at_capacity_is_one_window() {
    // 510 words + 2 specials = 512.
    let text = words(510);
    assert_eq!(chunk(&WhitespaceSegmenter, 512, "", &text).expect("chunk").len(), 1);
    assert_eq!(
        chunk(&WhitespaceSegmenter, 512, "", &words(511)).expect("chunk").len(),
        2
    );
}

#[test]
fn windows_stride_by_capacity_minus_overlap() {
    // capacity 510, stride 446: windows start at 0, 446, 892.
    let text = words(1000);
    let ws = windows(&WhitespaceSegmenter, 512, "", &text).expect("windows");
    let ranges: Vec<_> = ws.iter().map(|w| w.tokens.clone()).collect();
    assert_eq!(ranges, vec![0..510, 446..956, 892..1000]);
    for w in &ws {
        assert!(WhitespaceSegmenter.count_tokens(&w.text).expect("count") <= 512);
        assert!(text.contains(&w.text), "every window is a verbatim slice");
    }
}

#[test]
fn prefix_is_prepended_and_shrinks_capacity() {
    let text = words(1000);
    let ws = windows(&WhitespaceSegmenter, 512, "Title. First sentence.", &text).expect("windows");
    // prefix = 3 tokens, capacity = 507.
    assert_eq!(ws[0].tokens, 0..507);
    for w in &ws {
        assert!(w.text.starts_with("Title. First sentence.\n\n"));
        assert!(WhitespaceSegmenter.count_tokens(&w.text).expect("count") <= 512);
    }
}

#[test]
fn long_prefix_is_cut_to_prefix_max_tokens() {
    let prefix = (0..200).map(|i| format!("p{i}")).collect::<Vec<_>>().join(" ");
    let ws = windows(&WhitespaceSegmenter, 512, &prefix, &words(1000)).expect("windows");
    let kept = ws[0].text.split("\n\n").next().expect("prefix");
    assert_eq!(kept.split_whitespace().count(), PREFIX_MAX_TOKENS);
    assert_eq!(ws[0].tokens, 0..(512 - SPECIAL_TOKENS - PREFIX_MAX_TOKENS));
}

#[test]
fn window_too_small_for_the_overlap_is_an_error() {
    let err = chunk(
        &WhitespaceSegmenter,
        CHUNK_OVERLAP_TOKENS + SPECIAL_TOKENS,
        "",
        &words(10),
    )
    .expect_err("too small");
    assert!(err.to_string().contains("overlap"), "got: {err}");
}

#[test]
fn multibyte_text_slices_on_char_boundaries() {
    let text = (0..700).map(|i| format!("café{i} naïve")).collect::<Vec<_>>().join(" ");
    let ws = windows(&WhitespaceSegmenter, 512, "", &text).expect("windows");
    assert!(ws.len() > 1);
    for w in &ws {
        assert!(text.contains(&w.text));
    }
}

/// A segmenter whose re-encode of a window is one token longer than its span
/// count, as WordPiece does when a slice cuts a word: the chunker must shrink.
struct Inflating;

impl Segmenter for Inflating {
    fn token_spans(&self, text: &str) -> Result<Vec<Range<usize>>> {
        WhitespaceSegmenter.token_spans(text)
    }
    fn count_tokens(&self, text: &str) -> Result<usize> {
        Ok(WhitespaceSegmenter.count_tokens(text)? + 1)
    }
}

#[test]
fn window_that_re_encodes_longer_shrinks_until_it_fits() {
    let ws = windows(&Inflating, 512, "", &words(1000)).expect("windows");
    assert_eq!(ws[0].tokens, 0..509, "shrunk one token to fit");
    for w in &ws {
        assert!(Inflating.count_tokens(&w.text).expect("count") <= 512);
    }
}

struct Failing;

impl Segmenter for Failing {
    fn token_spans(&self, _text: &str) -> Result<Vec<Range<usize>>> {
        Err(eyre!("boom"))
    }
    fn count_tokens(&self, _text: &str) -> Result<usize> {
        Err(eyre!("boom"))
    }
}

#[test]
fn tokenizer_error_is_returned() {
    assert!(chunk(&Failing, 512, "", "alpha beta").is_err());
}
