use std::time::Duration;

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

    pub async fn wait(&mut self) {
        let delay = self.base * 2u32.saturating_pow(self.attempt);
        let delay = delay.min(self.cap);
        self.attempt = self.attempt.saturating_add(1);
        log::info!("reconnecting in {delay:?} (attempt {})", self.attempt);
        tokio::time::sleep(delay).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cap() {
        let backoff = ExponentialBackoff::new();
        assert_eq!(backoff.cap, Duration::from_secs(30));
        assert_eq!(backoff.base, Duration::from_secs(1));
    }

    #[test]
    fn test_reset() {
        let mut backoff = ExponentialBackoff::new();
        backoff.attempt = 5;
        backoff.reset();
        assert_eq!(backoff.attempt, 0);
    }
}
