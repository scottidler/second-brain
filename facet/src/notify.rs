//! Optional notification sinks for facet events.
//!
//! Phase-1 implementation: log-based only. Both event types route to
//! `log::warn!` / `log::info!` when the corresponding config flag is
//! true; both are no-ops when the flag is false. This keeps the
//! config surface honest (the keys actually do something) without
//! dragging in Telegram/desktop transports — those are an additive
//! upgrade. Borg-style multi-sink notifications can be wired here
//! later by reading `borg::notify::Telegram` / `borg::notify::Desktop`
//! and dispatching in parallel.
//!
//! Routing summary:
//!
//! - `on_new_workitem(slug)` — INFO when a new work-item is first
//!   persisted by the cluster stage.
//! - `on_budget_exhausted(reason)` — WARN when a tick stops because
//!   the per-tick or per-day budget cap was reached. The log emit
//!   uses WARN so the journald/systemd alerting paths surface it.

use crate::config::NotifyConfig;

/// Fire a "new work-item" notification if the config opt-in is set.
pub fn on_new_workitem(notify: &NotifyConfig, slug: &str, title: &str) {
    if !notify.on_new_workitem {
        return;
    }
    log::info!("facet::notify::on_new_workitem: slug={slug} title={title}");
}

/// Fire a "budget exhausted" notification if the config opt-in is set.
pub fn on_budget_exhausted(notify: &NotifyConfig, reason: &str) {
    if !notify.on_budget_exhausted {
        return;
    }
    log::warn!("facet::notify::on_budget_exhausted: {reason}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_panic_when_disabled() {
        let cfg = NotifyConfig::default();
        on_new_workitem(&cfg, "x", "Some title");
        on_budget_exhausted(&cfg, "test");
    }

    #[test]
    fn no_panic_when_enabled() {
        let cfg = NotifyConfig {
            on_new_workitem: true,
            on_budget_exhausted: true,
        };
        on_new_workitem(&cfg, "x", "Some title");
        on_budget_exhausted(&cfg, "test");
    }
}
