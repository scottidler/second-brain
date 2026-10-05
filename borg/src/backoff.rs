use std::time::{Duration, Instant, SystemTime};

/// Minimum uptime before a transport connection counts as "healthy" enough to
/// reset its restart backoff. A drop sooner than this is treated as a flap, so
/// the backoff keeps growing instead of resetting on every handshake - the
/// previous reset-on-connect hot-looped at the ~1s base delay whenever a
/// failure fired immediately after the handshake.
pub const HEALTHY_RUN_SECS: u64 = 60;

/// First delay of every transport reconnect loop.
pub const RECONNECT_BASE: Duration = Duration::from_secs(1);
/// Longest delay of every transport reconnect loop.
pub const RECONNECT_CAP: Duration = Duration::from_secs(30);

pub struct ExponentialBackoff {
    attempt: u32,
    base: Duration,
    cap: Duration,
}

impl ExponentialBackoff {
    pub fn new(base: Duration, cap: Duration) -> Self {
        Self { attempt: 0, base, cap }
    }

    /// The reconnect schedule shared by the transport loops.
    pub fn reconnect() -> Self {
        Self::new(RECONNECT_BASE, RECONNECT_CAP)
    }

    pub fn attempts(&self) -> u32 {
        self.attempt
    }

    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    /// Reset the backoff only if the connection stayed up at least
    /// `HEALTHY_RUN_SECS`. Call with the instant the connection became live;
    /// a fast drop leaves the backoff growing rather than resetting.
    pub fn reset_if_healthy(&mut self, connected_at: Instant) {
        if connected_at.elapsed() >= Duration::from_secs(HEALTHY_RUN_SECS) {
            self.reset();
        }
    }

    /// The next delay: a server `hint` (capped) when given, else
    /// `base * 2^attempt` (capped). The attempt count advances either way.
    pub fn next_delay(&mut self, hint: Option<Duration>) -> Duration {
        let delay = match hint {
            Some(hint) => hint.min(self.cap),
            None => (self.base * 2u32.saturating_pow(self.attempt)).min(self.cap),
        };
        self.attempt = self.attempt.saturating_add(1);
        delay
    }

    pub async fn wait(&mut self, label: &str) {
        let delay = self.next_delay(None);
        log::info!("{label} in {delay:?} (attempt {})", self.attempt);
        tokio::time::sleep(delay).await;
    }
}

/// Parse a `Retry-After` header value: delta-seconds (`120`) or an HTTP-date
/// (`Wed, 21 Oct 2026 07:28:00 GMT`, relative to `now`; a date already past
/// is a zero wait). Anything else is `None`.
pub fn retry_after_header(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    if let Ok(secs) = value.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let at = httpdate::parse_http_date(value).ok()?;
    Some(at.duration_since(now).unwrap_or(Duration::ZERO))
}

#[cfg(test)]
mod tests;
