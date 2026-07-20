#![allow(clippy::unwrap_used)]

use super::*;
use chrono::{Duration, Utc};
use std::sync::Mutex;

// `cooldown_map()` is a process-global map and `reset_cooldowns()` wipes ALL of
// it, so the cooldown tests below cannot run in parallel: one test's reset can
// fire between another's two `should_alert` calls and drop a just-recorded
// entry. Serialize every cooldown-touching test behind this lock (same pattern
// as the ENV_LOCK guard used for env-var tests). The `format_*` tests do not
// touch the map and are exempt.
static COOLDOWN_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn first_alert_fires_then_is_suppressed() {
    let _guard = COOLDOWN_LOCK.lock().unwrap();
    reset_cooldowns();
    let now = Utc::now();
    assert!(should_alert("xda-developers.com", GateId::BlockPage, now, 60));
    // Second call within window is suppressed
    assert!(!should_alert("xda-developers.com", GateId::BlockPage, now, 60));
}

#[test]
fn different_gates_share_domain_but_have_independent_cooldown() {
    let _guard = COOLDOWN_LOCK.lock().unwrap();
    reset_cooldowns();
    let now = Utc::now();
    assert!(should_alert("example.com", GateId::BlockPage, now, 60));
    // Different gate: independent cooldown
    assert!(should_alert("example.com", GateId::FailedFetchParaphrase, now, 60));
}

#[test]
fn different_domains_have_independent_cooldown() {
    let _guard = COOLDOWN_LOCK.lock().unwrap();
    reset_cooldowns();
    let now = Utc::now();
    assert!(should_alert("a.com", GateId::BlockPage, now, 60));
    assert!(should_alert("b.com", GateId::BlockPage, now, 60));
}

#[test]
fn cooldown_expires_after_window() {
    let _guard = COOLDOWN_LOCK.lock().unwrap();
    reset_cooldowns();
    let now = Utc::now();
    assert!(should_alert("c.com", GateId::BlockPage, now, 60));
    assert!(!should_alert("c.com", GateId::BlockPage, now, 60));
    // Jump past the window
    let later = now + Duration::minutes(61);
    assert!(should_alert("c.com", GateId::BlockPage, later, 60));
}

#[test]
fn format_includes_trace_gate_and_replay_hint() {
    let msg = format_gate_alert(
        "tg-abcdef",
        1,
        GateId::BlockPage,
        Some("xda-developers.com"),
        "anonymous access blocked",
        Some("2026-04-20T00:00:00Z"),
    );
    assert!(msg.contains("stage-1 reject"));
    assert!(msg.contains("tg-abcdef"));
    assert!(msg.contains("block-page"));
    assert!(msg.contains("xda-developers.com"));
    assert!(msg.contains("replay: borg replay tg-abcdef"));
    assert!(msg.contains("2026-04-20T00:00:00Z"));
}

#[test]
fn format_gate_2_uses_from_stage_2_replay_hint() {
    let msg = format_gate_alert(
        "tg-xyz",
        2,
        GateId::FailedFetchParaphrase,
        Some("some.com"),
        "paraphrased block",
        None,
    );
    assert!(msg.contains("--from-stage 2"));
}

#[test]
fn format_selection_gate_has_no_replay_hint() {
    let msg = format_gate_alert("hv-abc123", 0, GateId::Selection, None, "below selection bar", None);
    assert!(msg.contains("gate=selection"));
    assert!(msg.contains("no replay"));
    assert!(!msg.contains("--from-stage"));
}
