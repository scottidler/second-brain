//! The transcript chunker under the REAL BGE tokenizer
//! (`docs/design/2026-10-29-retrieval-quality-from-rag-scour.md`, Chunker).
//!
//! Recipe 1 windowed 400 whitespace words and let the encoder truncate at 512
//! tokens: 90.7% of live chunk rows overran, and a no-whitespace run (a URL,
//! code) was one "word" of thousands of tokens. These tests drive
//! `vault::embedding::chunk` with the committed `tokenizer.json` fixture and
//! check every window with an independent tokenizer instance, truncation off.

use std::path::PathBuf;

use tokenizers::Tokenizer;
use vault::embedding::chunk::{Segmenter, TokenChunker, chunk, windows};
use vault::embedding::{CHUNK_OVERLAP_TOKENS, PREFIX_MAX_TOKENS};

/// bge-small-en-v1.5's window, special tokens included.
const MAX_SEQ_LEN: usize = 512;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bge-small-en-v1.5-tokenizer.json")
}

/// The fixture with truncation and padding off: the true encoded length.
fn reference() -> Tokenizer {
    let mut tokenizer = Tokenizer::from_file(fixture()).expect("load the committed BGE tokenizer fixture");
    tokenizer.with_truncation(None).expect("truncation off");
    tokenizer.with_padding(None);
    tokenizer
}

fn encoded_len(tokenizer: &Tokenizer, text: &str) -> usize {
    tokenizer.encode(text, true).expect("encode").get_ids().len()
}

/// At least 20k tokens of transcript-shaped text with the shapes that break a
/// word count: a long no-whitespace URL run, long compound words, accented
/// and CJK text, code-ish punctuation.
fn hostile_transcript(chunker: &TokenChunker) -> String {
    let url = format!(
        "https://example.com/{}?q={}#frag",
        (0..400)
            .map(|i| format!("segment{i}-pneumonoultramicroscopic_{i}"))
            .collect::<Vec<_>>()
            .join("/"),
        "x".repeat(300)
    );
    let compounds = "Donaudampfschifffahrtsgesellschaftskapitänsmütze antidisestablishmentarianism \
                     supercalifragilisticexpialidocious Rindfleischetikettierungsüberwachungsaufgabenübertragungsgesetz \
                     floccinaucinihilipilification hippopotomonstrosesquippedaliophobia";
    let prose = "So the speaker says the second approach works better, because the retriever \
                 sees a café-style naïve résumé of the corpus; 東京 and 北京 are mentioned too. \
                 Then fn main() { let x = vec![1, 2, 3]; } gets pasted into the chat.";
    let mut text = String::new();
    let mut round = 0;
    while chunker.token_spans(&text).expect("spans").len() < 20_000 {
        text.push_str(prose);
        text.push(' ');
        text.push_str(compounds);
        text.push(' ');
        if round % 4 == 0 {
            text.push_str(&url);
            text.push(' ');
        }
        round += 1;
    }
    text
}

#[test]
fn chunk_fits_model_window() {
    let chunker = TokenChunker::from_file(&fixture()).expect("chunker");
    let reference = reference();
    let text = hostile_transcript(&chunker);
    let total = encoded_len(&reference, &text);
    assert!(total >= 20_000, "fixture is {total} tokens");

    let max_prefix = (0..PREFIX_MAX_TOKENS * 2)
        .map(|i| format!("Title{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    for prefix in [
        "",
        "A talk about retrieval. The speaker compares chunkers.",
        max_prefix.as_str(),
    ] {
        let chunks = chunk(&chunker, MAX_SEQ_LEN, prefix, &text).expect("chunk");
        for (i, c) in chunks.iter().enumerate() {
            let len = encoded_len(&reference, c);
            assert!(
                len <= MAX_SEQ_LEN,
                "chunk {i} (prefix {} bytes) encodes to {len} tokens with [CLS]/[SEP], over {MAX_SEQ_LEN}",
                prefix.len()
            );
        }
        assert!(
            chunks.len() > total / MAX_SEQ_LEN,
            "{} chunks for {total} tokens",
            chunks.len()
        );
        let longest = chunks.iter().map(|c| encoded_len(&reference, c)).max().unwrap_or(0);
        assert!(
            longest > MAX_SEQ_LEN - 16,
            "windows are filled, not shrunk: longest {longest}"
        );
    }
}

#[test]
fn chunk_overlap_has_no_gaps() {
    let chunker = TokenChunker::from_file(&fixture()).expect("chunker");
    let text = hostile_transcript(&chunker);
    let spans = chunker.token_spans(&text).expect("spans");
    let ws = windows(&chunker, MAX_SEQ_LEN, "", &text).expect("windows");
    assert!(ws.len() > 1);

    assert_eq!(
        ws.first().expect("first").tokens.start,
        0,
        "the first window starts at token 0"
    );
    assert_eq!(
        ws.last().expect("last").tokens.end,
        spans.len(),
        "the last window ends at the last token"
    );
    for pair in ws.windows(2) {
        let (prev, next) = (&pair[0].tokens, &pair[1].tokens);
        assert!(next.start > prev.start, "windows advance: {prev:?} then {next:?}");
        assert!(
            next.start < prev.end,
            "consecutive windows share at least one source token: {prev:?} then {next:?}"
        );
        assert!(prev.end - next.start <= CHUNK_OVERLAP_TOKENS);
    }
    for w in &ws {
        let verbatim = &text[spans[w.tokens.start].start..spans[w.tokens.end - 1].end];
        assert_eq!(w.text, verbatim, "a window is a verbatim slice of its source tokens");
    }
}
