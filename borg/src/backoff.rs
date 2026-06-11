use std::time::{Duration, Instant};

/// Minimum uptime before a transport connection counts as "healthy" enough to
/// reset its restart backoff. A drop sooner than this is treated as a flap, so
/// the backoff keeps growing instead of resetting on every handshake - the
/// previous reset-on-connect hot-looped at the ~1s base delay whenever a
/// failure fired immediately after the handshake.
pub const HEALTHY_RUN_SECS: u64 = 60;

pub struct ExponentialBackoff {
    attempt: u32,
    base: Duration,
    cap: Duration,
}

impl Default for ExponentialBackoff {
    fn default() -> Self {
        Self::new()
    }
}

impl ExponentialBackoff {
    pub fn new() -> Self {
        Self {
            attempt: 0,
            base: Duration::from_secs(1),
            cap: Duration::from_secs(30),
        }
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

    pub async fn wait(&mut self) {
        let delay = self.base * 2u32.saturating_pow(self.attempt);
        let delay = delay.min(self.cap);
        self.attempt = self.attempt.saturating_add(1);
        log::info!("reconnecting in {delay:?} (attempt {})", self.attempt);
        tokio::time::sleep(delay).await;
    }
}

#[cfg(test)]
mod tests;
