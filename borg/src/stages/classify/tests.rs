#![allow(clippy::unwrap_used)]

use super::*;
use chrono::{Duration, Utc};

#[test]
fn detects_anonymous_access_pattern() {
    let now = Utc::now();
    let body = b"Error: anonymous access to domain blocked until Mon Apr 20 2026";
    let got = detect_block_page(body, 200, now).unwrap();
    assert!(got.reason.contains("anonymous access"));
}

#[test]
fn detects_security_compromise_error() {
    let now = Utc::now();
    let body = b"SecurityCompromiseError: request rejected";
    let got = detect_block_page(body, 200, now).unwrap();
    assert!(got.reason.to_ascii_lowercase().contains("securitycompromise"));
}

#[test]
fn detects_suspected_ddos_message() {
    let now = Utc::now();
    let body = b"Your IP has been flagged for suspected DDoS attacks.";
    let got = detect_block_page(body, 200, now).unwrap();
    assert!(got.reason.contains("suspected ddos"));
}

#[test]
fn http_451_alone_triggers_gate() {
    let now = Utc::now();
    let body = b"";
    let got = detect_block_page(body, 451, now).unwrap();
    assert!(got.reason.contains("451"));
    // Falls back to now+7d because no body
    assert!(got.retriable_after > now + Duration::days(6));
}

#[test]
fn clean_body_returns_none() {
    let now = Utc::now();
    let body = b"<html><body><h1>Article title</h1><p>Real content here.</p></body></html>";
    assert!(detect_block_page(body, 200, now).is_none());
}

#[test]
fn case_insensitive_match() {
    let now = Utc::now();
    let body = b"ANONYMOUS ACCESS TO DOMAIN is not permitted";
    let got = detect_block_page(body, 200, now).unwrap();
    assert!(got.reason.contains("anonymous access"));
}

#[test]
fn retriable_after_parses_iso_timestamp() {
    let now = Utc::now();
    let body = b"anonymous access to domain blocked until 2026-04-20T00:00:00Z";
    let got = detect_block_page(body, 200, now).unwrap();
    assert_eq!(got.retriable_after.format("%Y-%m-%d").to_string(), "2026-04-20");
}
