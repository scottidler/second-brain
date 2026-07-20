//! Gate-rejection alerting. Shared between Gate-0/1/2 to produce a consistent
//! alert format (suitable for Telegram / ntfy) with per-domain cooldown so a
//! domain-wide outage does not flood the alert channel.
//!
//! Phase 5 scope: format + per-domain cooldown + structured logging. Delivery
//! to the actual Telegram notifier is done by callers that have an AppState
//! handle (routes.rs / telegram.rs / daemon tasks); pipeline.rs emits the
//! alert message at WARN so the operator can tail journalctl or a future
//! daemon task can sweep the log and forward.

use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::OnceLock;

use crate::types::GateId;

type CooldownKey = (String, GateId);
type CooldownMap = HashMap<CooldownKey, DateTime<Utc>>;

fn cooldown_map() -> &'static Mutex<CooldownMap> {
    static MAP: OnceLock<Mutex<CooldownMap>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Default per-domain cooldown window. First rejection fires immediately; a
/// second rejection from the same (domain, gate) within this window collapses
/// into the previous alert.
pub const DEFAULT_COOLDOWN_MINUTES: i64 = 60;

/// Decide whether an alert for `(domain, gate)` should fire right now, given
/// the previous alert time and a cooldown window. Updates the map on `true`.
pub fn should_alert(domain: &str, gate: GateId, now: DateTime<Utc>, cooldown_minutes: i64) -> bool {
    let mut map = cooldown_map().lock().expect("alert cooldown map poisoned");
    let key: CooldownKey = (domain.to_ascii_lowercase(), gate);
    if let Some(last) = map.get(&key)
        && now.signed_duration_since(*last) < Duration::minutes(cooldown_minutes.max(0))
    {
        return false;
    }
    map.insert(key, now);
    true
}

/// Format a gate rejection as a human-readable alert message suitable for
/// Telegram / ntfy. Kept one-line-per-section so it renders cleanly in either.
pub fn format_gate_alert(
    trace_id: &str,
    stage: u8,
    gate: GateId,
    domain: Option<&str>,
    reason: &str,
    retriable_after: Option<&str>,
) -> String {
    let mut out = String::new();
    let domain_str = domain.unwrap_or("-");
    out.push_str(&format!("[borg] stage-{stage} reject: {domain_str}\n"));
    out.push_str(&format!("trace {trace_id} gate={gate} - {reason}\n"));
    match gate {
        GateId::DomainBlocklist | GateId::BlockPage => {
            if let Some(ts) = retriable_after {
                out.push_str(&format!("replay: borg replay {trace_id} --from-stage 0 (after {ts})"));
            } else {
                out.push_str(&format!("replay: borg replay {trace_id} --from-stage 0"));
            }
        }
        GateId::FailedFetchParaphrase => {
            out.push_str(&format!("replay: borg replay {trace_id} --from-stage 2"));
        }
        GateId::StructuralQuality => {
            out.push_str(&format!("replay: borg replay {trace_id} --from-stage 2"));
        }
        GateId::Selection => {
            out.push_str("no replay: below the harvest selection bar (sb borg harvest --force to reconsider)");
        }
    }
    out
}

/// Emit a structured WARN log for a gate rejection, respecting per-domain
/// cooldown. Returns true if the alert was "fired" (cooldown respected / not
/// yet in window), false if suppressed.
pub fn emit_gate_alert(
    trace_id: &str,
    stage: u8,
    gate: GateId,
    domain: Option<&str>,
    reason: &str,
    retriable_after: Option<&str>,
    cooldown_minutes: i64,
) -> bool {
    let now = Utc::now();
    let domain_for_cooldown = domain.unwrap_or("-");
    if !should_alert(domain_for_cooldown, gate, now, cooldown_minutes) {
        log::debug!("[{trace_id}] alert suppressed by cooldown: gate={gate} domain={domain_for_cooldown}");
        return false;
    }
    let message = format_gate_alert(trace_id, stage, gate, domain, reason, retriable_after);
    log::warn!("{message}");
    true
}

/// Reset the cooldown map. Exposed for tests only.
#[cfg(test)]
pub fn reset_cooldowns() {
    let mut map = cooldown_map().lock().expect("alert cooldown map poisoned");
    map.clear();
}

#[cfg(test)]
mod tests;
