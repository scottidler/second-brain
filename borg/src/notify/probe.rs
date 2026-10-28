//! Test-only record of the per-item pings the live [`super::Telegram`] and
//! [`super::Desktop`] sinks were asked to send. Under `cfg(test)` the disable
//! gate stops the send, so this is the only way a test driving the real
//! `/ingest` route or `dispatch_ingest` can tell whether a ping was attempted.
//! Keys are the trace id (processing) or the display source (result); tests
//! run in parallel, so each asserts only on its own keys.

use std::sync::Mutex;

static ATTEMPTED: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub(crate) fn record(key: &str) {
    ATTEMPTED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(key.to_string());
}

/// How many pings were attempted under `key`.
pub(crate) fn attempts(key: &str) -> usize {
    ATTEMPTED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .filter(|k| k.as_str() == key)
        .count()
}
