//! Regression tests for the chunking primitives. Both `split_with_overlap`
//! and `find_break_point` previously panicked on multi-byte input (a raw byte
//! cut inside a codepoint) and could stall / slice backwards for small chunk
//! sizes; the fixed-panic comments in `fabric.rs` cite the exact cases pinned
//! here.

use super::*;

// --- split_with_overlap ---

#[test]
fn split_short_text_returns_single_chunk() {
    let text = "short text under the limit";
    let chunks = split_with_overlap(text, 1000, 100);
    assert_eq!(chunks, vec![text.to_string()]);
}

#[test]
fn split_zero_chunk_size_returns_single_chunk() {
    // chunk_size == 0 would never advance; treated as "no chunking".
    let text = "some content here";
    let chunks = split_with_overlap(text, 0, 0);
    assert_eq!(chunks, vec![text.to_string()]);
}

#[test]
fn split_zero_overlap_reconstructs_original() {
    // With no overlap the concatenation of chunks is exactly the input - the
    // splitter must not drop or duplicate any bytes.
    let text = "Para one.\n\nPara two is a bit longer.\n\nPara three ends it here.";
    let chunks = split_with_overlap(text, 20, 0);
    assert!(chunks.len() > 1, "expected multiple chunks, got {}", chunks.len());
    assert_eq!(chunks.concat(), text);
}

#[test]
fn split_covers_text_edges_with_overlap() {
    let text = "a".repeat(1000);
    let chunks = split_with_overlap(&text, 100, 10);
    assert!(chunks.len() > 1);
    assert!(text.starts_with(chunks.first().expect("at least one chunk").as_str()));
    assert!(text.ends_with(chunks.last().expect("at least one chunk").as_str()));
}

#[test]
fn split_multibyte_does_not_panic_and_aligns_edges() {
    // Regression: a raw byte cut inside a multi-byte codepoint panicked. Each
    // char here is 4 bytes, so any byte-arithmetic cut lands mid-codepoint.
    let text = "😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀";
    let chunks = split_with_overlap(text, 10, 3);
    assert!(!chunks.is_empty());
    // Reaching here without panicking is the core assertion; the edges must
    // still line up with the input.
    assert!(text.starts_with(chunks.first().expect("at least one chunk").as_str()));
    assert!(text.ends_with(chunks.last().expect("at least one chunk").as_str()));
}

#[test]
fn split_tiny_chunk_size_terminates_without_stall() {
    // Regression: find_break_point's 200-byte lookback can return an offset
    // <= start for small chunks; the loop must still advance and terminate.
    let text = "one two three four five six seven eight nine ten";
    let chunks = split_with_overlap(text, 5, 2);
    assert!(!chunks.is_empty());
    // Reaching here proves the loop advanced to completion (no infinite stall).
    assert!(text.starts_with(chunks.first().expect("at least one chunk").as_str()));
}

#[test]
fn split_tiny_chunk_multibyte_does_not_panic() {
    // Combined regression: small chunk + multibyte content exercises both the
    // mid-codepoint cut and the sub-start lookback in the same call.
    let text = "café ☕ résumé ñoño 日本語 emoji 😀 done now";
    let chunks = split_with_overlap(text, 4, 2);
    assert!(!chunks.is_empty());
}

// --- find_break_point ---

#[test]
fn break_point_prefers_paragraph() {
    let text = "first para.\n\nsecond para continues here";
    let bp = find_break_point(text, 0, text.len());
    assert_eq!(&text[..bp], "first para.\n\n");
}

#[test]
fn break_point_falls_back_to_sentence() {
    let text = "one sentence. another sentence without breaks";
    let bp = find_break_point(text, 0, text.len());
    assert_eq!(&text[..bp], "one sentence. ");
}

#[test]
fn break_point_falls_back_to_line() {
    let text = "line one\nline two with no sentence end";
    let bp = find_break_point(text, 0, text.len());
    assert_eq!(&text[..bp], "line one\n");
}

#[test]
fn break_point_falls_back_to_end_when_no_boundary() {
    let text = "nobreakshereatall";
    let bp = find_break_point(text, 0, text.len());
    assert_eq!(bp, text.len());
}

#[test]
fn break_point_snaps_midcodepoint_bounds_without_panic() {
    // Regression: callers pass byte-arithmetic offsets that can land inside a
    // multi-byte codepoint; both bounds must be floored to char boundaries
    // before slicing the search region.
    let text = "😀😀😀😀😀"; // 5 × 4 bytes = 20 bytes
    // search_start=1 and end=15 both land mid-codepoint.
    let bp = find_break_point(text, 1, 15);
    assert!(
        text.is_char_boundary(bp),
        "break point {bp} must sit on a char boundary"
    );
}

// --- subprocess sites (F2): each is driven through `sh -c` substituted for the
// real binary, with output past the 64 KiB pipe buffer. The `head | tr` form is
// load-bearing: bare `head -c`/`cat` can splice into the pipe and pass against
// a loop that never drains.

fn sh_command(script: &str) -> Command {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", script]);
    cmd
}

const ONE_MIB_OF_A: &str = "head -c 1048576 /dev/zero | tr '\\0' a";
const WELL_UNDER: Duration = Duration::from_secs(5);

#[test]
fn transcript_one_mib_of_stdout_is_returned_whole() {
    let started = std::time::Instant::now();
    let text = run_transcript(
        sh_command(ONE_MIB_OF_A),
        Duration::from_secs(30),
        "fabric -y --transcript",
    )
    .expect("run");
    assert_eq!(text.len(), 1_048_576);
    assert!(started.elapsed() < WELL_UNDER, "took {:?}", started.elapsed());
}

#[test]
fn transcript_timeout_is_err_naming_the_label() {
    let err = run_transcript(
        sh_command("sleep 30"),
        Duration::from_millis(300),
        "fabric -y --transcript",
    )
    .expect_err("timeout must be Err");
    let msg = format!("{err:#}");
    assert!(msg.contains("fabric -y --transcript timed out"), "{msg}");
}

#[test]
fn transcript_non_zero_exit_is_an_empty_transcript() {
    let text = run_transcript(
        sh_command("echo boom >&2; exit 3"),
        Duration::from_secs(10),
        "fabric -y --transcript",
    )
    .expect("non-zero exit degrades to empty");
    assert!(text.is_empty());
}

#[test]
fn transcript_command_carries_the_en_orig_track() {
    let cmd = transcript_command("fabric", "https://youtu.be/x");
    let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
    assert_eq!(
        args,
        [
            "-y",
            "https://youtu.be/x",
            "--transcript",
            "--yt-dlp-args=--sub-lang en-orig"
        ]
    );
}

#[test]
fn article_fabric_one_mib_of_stdout_is_returned_whole() {
    let started = std::time::Instant::now();
    let text = run_article_command(
        sh_command(ONE_MIB_OF_A),
        Duration::from_secs(30),
        "fabric -u",
        "https://x",
    )
    .expect("run")
    .expect("text");
    assert_eq!(text.len(), 1_048_576);
    assert!(started.elapsed() < WELL_UNDER, "took {:?}", started.elapsed());
}

#[test]
fn article_markitdown_fallback_one_mib_of_stdout_is_returned_whole() {
    let started = std::time::Instant::now();
    let text = run_article_command(
        sh_command(ONE_MIB_OF_A),
        Duration::from_secs(30),
        "markitdown",
        "https://x",
    )
    .expect("run")
    .expect("text");
    assert_eq!(text.len(), 1_048_576);
    assert!(started.elapsed() < WELL_UNDER, "took {:?}", started.elapsed());
}

#[test]
fn article_timeout_is_err_naming_the_label() {
    let err = run_article_command(
        sh_command("sleep 30"),
        Duration::from_millis(300),
        "fabric -u",
        "https://x",
    )
    .expect_err("timeout must be Err");
    assert!(format!("{err:#}").contains("fabric -u timed out"), "{err:#}");
}

#[test]
fn article_empty_output_and_failure_are_none_not_err() {
    let empty = run_article_command(sh_command("true"), Duration::from_secs(10), "markitdown", "u").expect("run");
    assert!(empty.is_none());
    let failed = run_article_command(
        sh_command("echo no >&2; exit 1"),
        Duration::from_secs(10),
        "markitdown",
        "u",
    )
    .expect("run");
    assert!(failed.is_none());
}

#[test]
fn article_missing_binary_is_err() {
    let err = run_article_command(
        article_command("/nonexistent/fabric-binary", "https://x"),
        Duration::from_secs(5),
        "fabric -u",
        "https://x",
    )
    .expect_err("spawn failure is Err");
    assert!(format!("{err:#}").contains("fabric -u"), "{err:#}");
}

#[test]
fn article_commands_build_the_documented_argv() {
    let fabric = article_command("fabric", "https://x");
    assert_eq!(fabric.get_program(), "fabric");
    assert_eq!(fabric.get_args().collect::<Vec<_>>(), ["-u", "https://x"]);
    let md = markitdown_command("https://x");
    assert_eq!(md.get_program(), "markitdown");
    assert_eq!(md.get_args().collect::<Vec<_>>(), ["https://x"]);
}
