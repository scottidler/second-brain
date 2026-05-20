#![allow(clippy::unwrap_used)]

use super::*;

#[cfg(target_os = "linux")]
#[test]
fn rss_returns_some_positive_value_on_linux() {
    let rss = read_self_rss().expect("VmRSS must be readable in a test process");
    assert!(rss > 0, "VmRSS={rss}");
}

#[cfg(not(target_os = "linux"))]
#[test]
fn rss_returns_none_off_linux() {
    assert!(read_self_rss().is_none());
}

#[test]
fn human_bytes_picks_appropriate_unit() {
    assert_eq!(human_bytes(0), "0 B");
    assert_eq!(human_bytes(1023), "1023 B");
    assert_eq!(human_bytes(1024), "1.00 KB");
    assert_eq!(human_bytes(1024 * 1024), "1.00 MB");
    assert_eq!(human_bytes(1024u64.pow(3)), "1.00 GB");
}
