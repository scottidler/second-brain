//! Cold-start bootstrap latch for the Signal transport.
//!
//! A freshly-linked borg device receives no Note-to-Self until it has SENT
//! once: the phone builds its outbound sync session to a linked device lazily,
//! on first receipt of a message from that device. borg therefore self-pings
//! at first start. This module persists a marker recording that the self-ping
//! succeeded, so the ping fires exactly once per identity and re-fires only if
//! it never actually landed.
//!
//! The latch keys off the *successful send* (not session presence) on purpose:
//! signal-rs commits the local session before the network `send_sync_message`,
//! so a failed send would leave a session present while the phone got nothing.
//! A `send` returning `Ok` means the `PreKeyMessage` was accepted for delivery,
//! which is exactly the condition that bootstraps the phone->device session.
//! See `docs/design/2026-05-28-signal-cold-start-bootstrap.md`.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Recognizable body for the cold-start self-ping. The user sees this once in
/// their Note-to-Self conversation when a fresh device bootstraps.
pub(crate) const COLD_START_BOOTSTRAP_BODY: &str = "borg: establishing Signal sync session";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Marker {
    account: String,
    device_id: u32,
    sent_at_ms: u64,
}

/// True only if a marker exists AND matches the live `{account, device_id}`, so
/// a re-link to a different identity reads as "not bootstrapped" even when a
/// stale marker file survives. Unreadable or corrupt markers are treated as
/// absent (fail toward re-bootstrapping, never toward a false "healthy").
pub(crate) fn bootstrap_done(marker_path: &Path, account: &str, device_id: u32) -> bool {
    let Ok(text) = std::fs::read_to_string(marker_path) else {
        return false;
    };
    match serde_json::from_str::<Marker>(&text) {
        Ok(marker) => marker.account == account && marker.device_id == device_id,
        Err(_) => false,
    }
}

/// Persist the latch after a successful self-send. Best-effort: a write failure
/// is logged and swallowed - the worst case is one redundant self-ping on the
/// next borg start, never a blocked ingest.
pub(crate) fn record_bootstrap(marker_path: &Path, account: &str, device_id: u32, sent_at_ms: u64) {
    let marker = Marker {
        account: account.to_string(),
        device_id,
        sent_at_ms,
    };
    let json = match serde_json::to_string(&marker) {
        Ok(json) => json,
        Err(e) => {
            log::warn!("signal: failed to serialize bootstrap marker: {e}");
            return;
        }
    };
    if let Some(parent) = marker_path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        log::warn!(
            "signal: failed to create bootstrap marker dir {}: {e}",
            parent.display()
        );
        return;
    }
    if let Err(e) = std::fs::write(marker_path, json) {
        log::warn!(
            "signal: failed to write bootstrap marker {}: {e}",
            marker_path.display()
        );
    }
}

#[cfg(test)]
mod tests;
