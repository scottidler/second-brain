#![allow(clippy::unwrap_used)]

use super::*;
use chrono::{Duration, Utc};
use tempfile::TempDir;

#[test]
fn add_and_check_block() {
    let mut bl = Blocklist::new();
    let now = Utc::now();
    bl.add_or_refresh("xda-developers.com", "test block", now + Duration::hours(24));
    assert!(bl.is_blocked("xda-developers.com", now));
    assert!(!bl.is_blocked("example.com", now));
}

#[test]
fn is_blocked_handles_case_and_www() {
    let mut bl = Blocklist::new();
    let now = Utc::now();
    bl.add_or_refresh("XDA-Developers.com", "test", now + Duration::hours(24));
    assert!(bl.is_blocked("xda-developers.com", now));
    // domain_for strips www.
    let domain = domain_for("https://www.xda-developers.com/article");
    assert_eq!(domain, "xda-developers.com");
    assert!(bl.is_blocked(&domain, now));
}

#[test]
fn expired_block_is_not_active() {
    let mut bl = Blocklist::new();
    let now = Utc::now();
    bl.add_or_refresh("example.com", "test", now - Duration::hours(1));
    assert!(!bl.is_blocked("example.com", now));
}

#[test]
fn gate_0_rejects_blocked_url() {
    let mut bl = Blocklist::new();
    let now = Utc::now();
    bl.add_or_refresh("xda-developers.com", "test", now + Duration::hours(24));
    let err = gate_0(&bl, "https://www.xda-developers.com/foo", now, ()).unwrap_err();
    assert!(format!("{err:#}").contains("blocklisted"));
}

#[test]
fn gate_0_passes_unblocked_url() {
    let bl = Blocklist::new();
    let now = Utc::now();
    gate_0(&bl, "https://example.com/", now, ()).unwrap();
}

#[test]
fn remove_clears_domain() {
    let mut bl = Blocklist::new();
    let now = Utc::now();
    bl.add_or_refresh("example.com", "test", now + Duration::hours(1));
    assert!(bl.is_blocked("example.com", now));
    bl.remove("example.com");
    assert!(!bl.is_blocked("example.com", now));
}

#[test]
fn roundtrip_to_yaml_file() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("blocked-domains.yml");
    let mut bl = Blocklist::new();
    let now = Utc::now();
    bl.add_or_refresh("example.com", "test block", now + Duration::hours(24));
    bl.save_to(&path).unwrap();
    let loaded = Blocklist::from_file(&path).unwrap();
    assert!(loaded.is_blocked("example.com", now));
}

#[test]
fn add_or_refresh_extends_but_does_not_shorten() {
    let mut bl = Blocklist::new();
    let now = Utc::now();
    bl.add_or_refresh("example.com", "r1", now + Duration::hours(24));
    let first = bl.get("example.com").unwrap().retriable_after.clone();
    bl.add_or_refresh("example.com", "r2", now + Duration::hours(1));
    // earlier timestamp should not shorten the window
    assert_eq!(bl.get("example.com").unwrap().retriable_after, first);
    // later one should extend it
    bl.add_or_refresh("example.com", "r3", now + Duration::hours(48));
    assert_ne!(bl.get("example.com").unwrap().retriable_after, first);
}

#[test]
fn hits_are_incremented_on_refresh() {
    let mut bl = Blocklist::new();
    let now = Utc::now();
    bl.add_or_refresh("a.com", "r", now + Duration::hours(1));
    bl.add_or_refresh("a.com", "r", now + Duration::hours(1));
    bl.add_or_refresh("a.com", "r", now + Duration::hours(1));
    assert_eq!(bl.get("a.com").unwrap().hits, 3);
}

#[test]
fn domain_for_handles_various_inputs() {
    assert_eq!(domain_for("https://www.example.com/foo"), "example.com");
    assert_eq!(domain_for("https://example.com"), "example.com");
    assert_eq!(domain_for("https://sub.example.com/"), "sub.example.com");
    assert_eq!(domain_for("not-a-url"), "not-a-url");
}

#[test]
fn parse_retry_after_falls_back_to_7_days() {
    let now = Utc::now();
    let got = parse_retry_after("some unrelated message", now);
    assert!(got > now + Duration::days(6));
    assert!(got < now + Duration::days(8));
}

#[test]
fn parse_retry_after_picks_up_rfc3339() {
    let now = Utc::now();
    let got = parse_retry_after("blocked until 2026-04-20T00:00:00Z", now);
    assert_eq!(got.format("%Y-%m-%d").to_string(), "2026-04-20");
}
