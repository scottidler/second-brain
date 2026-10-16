//! Pauses every YouTube fetch after a streak of metadata failures.
//!
//! YouTube rate-limits a signed-in session by answering every video with
//! "Video unavailable" ("This content isn't available, try again later" in the
//! browser). yt-dlp reports only the first sentence, which a deleted video also
//! returns, so the message cannot tell them apart; a streak can. On 2026-10-15 a
//! 16-job reingest kept going through the limit and sent about 850 failing
//! requests on the operator's cookies in four minutes, which locked the
//! account's YouTube in the browser too. A deleted video is a one-off; a run of
//! them is the limit.
//!
//! [`check`] runs before every yt-dlp spawn; [`record_success`] and
//! [`record_failure`] run on each metadata fetch, the first YouTube request of
//! every ingest. Until [`init`] runs (tests, one-shot CLI paths) the defaults
//! apply.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use eyre::{Result, bail};

/// Failed ingests read this prefix to see the fetch never reached YouTube.
pub const PAUSED: &str = "youtube paused";

pub const DEFAULT_PAUSE_AFTER_FAILURES: u32 = 5;
pub const DEFAULT_PAUSE_FOR: Duration = Duration::from_secs(30 * 60);

#[derive(Debug)]
pub struct Breaker {
    pause_after_failures: u32,
    pause_for: Duration,
    streak: u32,
    paused_until: Option<Instant>,
}

impl Breaker {
    pub fn new(pause_after_failures: u32, pause_for: Duration) -> Self {
        Self {
            pause_after_failures: pause_after_failures.max(1),
            pause_for,
            streak: 0,
            paused_until: None,
        }
    }

    /// `Some(remaining)` while paused.
    pub fn paused_for(&self, now: Instant) -> Option<Duration> {
        self.paused_until.filter(|until| *until > now).map(|until| until - now)
    }

    pub fn record_success(&mut self) {
        self.streak = 0;
    }

    /// Returns true when this failure opens the pause.
    pub fn record_failure(&mut self, now: Instant) -> bool {
        if self.paused_for(now).is_some() {
            return false;
        }
        self.streak += 1;
        if self.streak >= self.pause_after_failures {
            self.streak = 0;
            self.paused_until = Some(now + self.pause_for);
            return true;
        }
        false
    }
}

static SETTINGS: OnceLock<(u32, Duration)> = OnceLock::new();
static BREAKER: Mutex<Option<Breaker>> = Mutex::new(None);

/// Size the process-wide breaker from config. The first call wins.
pub fn init(pause_after_failures: u32, pause_for: Duration) {
    log::debug!("youtube::breaker::init: pause_after_failures={pause_after_failures} pause_for={pause_for:?}");
    let _ = SETTINGS.set((pause_after_failures, pause_for));
}

fn with_breaker<T>(f: impl FnOnce(&mut Breaker) -> T) -> T {
    let mut guard = BREAKER.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let breaker = guard.get_or_insert_with(|| {
        let (failures, pause_for) = SETTINGS
            .get()
            .copied()
            .unwrap_or((DEFAULT_PAUSE_AFTER_FAILURES, DEFAULT_PAUSE_FOR));
        Breaker::new(failures, pause_for)
    });
    f(breaker)
}

/// Refuse to call YouTube while paused.
pub fn check(url: &str) -> Result<()> {
    if let Some(remaining) = with_breaker(|b| b.paused_for(Instant::now())) {
        bail!(
            "{PAUSED} for another {}s after a streak of failed fetches (likely YouTube's session rate limit); not fetching {url}",
            remaining.as_secs()
        );
    }
    Ok(())
}

pub fn record_success() {
    with_breaker(Breaker::record_success);
}

pub fn record_failure(url: &str) {
    if with_breaker(|b| b.record_failure(Instant::now())) {
        let pause_for = with_breaker(|b| b.pause_for);
        log::error!(
            "youtube: {PAUSED} for {}s: metadata fetches failed repeatedly, last {url}; every YouTube fetch fails fast until then",
            pause_for.as_secs()
        );
    }
}

#[cfg(test)]
mod tests;
