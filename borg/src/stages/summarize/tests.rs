#![allow(clippy::unwrap_used)]

use super::*;

#[test]
fn detects_error_message_paraphrase() {
    let summary = "The provided input contains an error message indicating \
                   that access to the XDA Developers website is blocked due \
                   to suspected DDoS attacks.";
    let got = detect_paraphrased_block(summary).unwrap();
    assert!(got.contains("error message indicating") || got.contains("access to the website is blocked"));
}

#[test]
fn detects_only_an_error_message() {
    let summary = "This content contains only an error message.";
    let got = detect_paraphrased_block(summary).unwrap();
    assert!(got.contains("only an error message"));
}

#[test]
fn detects_no_actual_content() {
    let summary = "The page contains no actual content, just a block page.";
    let got = detect_paraphrased_block(summary).unwrap();
    assert!(got.contains("no actual content"));
}

#[test]
fn clean_summary_returns_none() {
    let summary = "Docker containers provide lightweight virtualisation. \
                   The article lists seven useful containers for self-hosters.";
    assert!(detect_paraphrased_block(summary).is_none());
}

#[test]
fn is_case_insensitive() {
    let summary = "ACCESS TO THE WEBSITE IS BLOCKED right now.";
    assert!(detect_paraphrased_block(summary).is_some());
}
