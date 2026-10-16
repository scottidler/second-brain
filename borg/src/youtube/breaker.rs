//! Pauses every YouTube fetch when YouTube is rate-limiting the session.
//!
//! YouTube's session rate limit answers every video "Video unavailable"
//! ("This content isn't available, try again later" in the browser). On
//! 2026-10-15 a 16-job reingest kept going through it: about 850 failing
//! requests on the operator's cookies in four minutes, which locked the
//! account's YouTube in the browser too.
//!
//! A bare "Video unavailable" is also what some deleted videos return, and a
//! re-run puts every known-dead video at the head of its list, so a streak of
//! failures alone is not the limit (the first version of this breaker paused on
//! five dead videos in a row). Two rules tell them apart:
//!
//! - Only an ambiguous or rate-limit error counts toward the streak
//!   ([`counts_toward_pause`]); an error that names a permanent cause does not.
//! - When the streak is reached, the last video that fetched cleanly is fetched
//!   again (the canary). If it still works the streak was dead videos and
//!   resets; if it fails too, the session is limited and every fetch pauses.
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
/// The canary before any video has fetched cleanly in this process: YouTube's
/// first upload, which is not going away.
pub const DEFAULT_CANARY_URL: &str = "https://www.youtube.com/watch?v=jNQXAC9IVRw";

/// Errors that name a cause particular to one video. They never count toward
/// a pause, whatever the streak.
const PERMANENT: [&str; 9] = [
    "This video is unavailable",
    "has been removed",
    "Requested format is not available",
    "Private video",
    "has been terminated",
    "members-only",
    "Sign in to confirm your age",
    "not made this video available in your country",
    "This live event will begin",
];

/// Whether a yt-dlp failure looks like the session limit rather than a dead
/// video: a rate-limit or bot-check message, or a bare "Video unavailable".
pub fn counts_toward_pause(stderr: &str) -> bool {
    if PERMANENT.iter().any(|p| stderr.contains(p)) {
        return false;
    }
    stderr.contains("Video unavailable")
        || stderr.contains("try again later")
        || stderr.contains("not a bot")
        || stderr.contains("HTTP Error 429")
}

/// What the caller does after a failure is recorded.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing more: a permanent error, or the streak is still short.
    Continue,
    /// The streak is reached: fetch this known-good video and report it with
    /// [`Breaker::canary_result`].
    Canary(String),
    /// The streak is reached and there is no known-good video to try: pause.
    Paused,
}

#[derive(Debug)]
pub struct Breaker {
    pause_after_failures: u32,
    pause_for: Duration,
    streak: u32,
    last_success: Option<String>,
    probing: bool,
    paused_until: Option<Instant>,
}

impl Breaker {
    pub fn new(pause_after_failures: u32, pause_for: Duration) -> Self {
        Self {
            pause_after_failures: pause_after_failures.max(1),
            pause_for,
            streak: 0,
            last_success: None,
            probing: false,
            paused_until: None,
        }
    }

    /// `Some(remaining)` while paused.
    pub fn paused_for(&self, now: Instant) -> Option<Duration> {
        self.paused_until.filter(|until| *until > now).map(|until| until - now)
    }

    pub fn record_success(&mut self, url: &str) {
        self.streak = 0;
        self.last_success = Some(url.to_string());
    }

    pub fn record_failure(&mut self, stderr: &str, now: Instant) -> Verdict {
        if self.paused_for(now).is_some() || self.probing || !counts_toward_pause(stderr) {
            return Verdict::Continue;
        }
        self.streak += 1;
        if self.streak < self.pause_after_failures {
            return Verdict::Continue;
        }
        match self.last_success.clone() {
            Some(canary) => {
                self.probing = true;
                Verdict::Canary(canary)
            }
            None => {
                self.pause(now);
                Verdict::Paused
            }
        }
    }

    /// The canary fetched (`true`): the streak was dead videos. It failed: pause.
    pub fn canary_result(&mut self, ok: bool, now: Instant) {
        self.probing = false;
        if ok {
            self.streak = 0;
        } else {
            self.pause(now);
        }
    }

    fn pause(&mut self, now: Instant) {
        self.streak = 0;
        self.paused_until = Some(now + self.pause_for);
    }
}

static SETTINGS: OnceLock<(u32, Duration, String)> = OnceLock::new();
static BREAKER: Mutex<Option<Breaker>> = Mutex::new(None);

/// Size the process-wide breaker from config. The first call wins.
pub fn init(pause_after_failures: u32, pause_for: Duration, canary_url: &str) {
    log::debug!(
        "youtube::breaker::init: pause_after_failures={pause_after_failures} pause_for={pause_for:?} canary_url={canary_url}"
    );
    let _ = SETTINGS.set((pause_after_failures, pause_for, canary_url.to_string()));
}

fn with_breaker<T>(f: impl FnOnce(&mut Breaker) -> T) -> T {
    let mut guard = BREAKER.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let breaker = guard.get_or_insert_with(|| {
        let (failures, pause_for, canary) = SETTINGS.get().cloned().unwrap_or((
            DEFAULT_PAUSE_AFTER_FAILURES,
            DEFAULT_PAUSE_FOR,
            DEFAULT_CANARY_URL.to_string(),
        ));
        let mut breaker = Breaker::new(failures, pause_for);
        breaker.record_success(&canary);
        breaker
    });
    f(breaker)
}

/// Refuse to call YouTube while paused.
pub fn check(url: &str) -> Result<()> {
    if let Some(remaining) = with_breaker(|b| b.paused_for(Instant::now())) {
        bail!(
            "{PAUSED} for another {}s: YouTube is rate-limiting the session (a known-good video failed too); not fetching {url}",
            remaining.as_secs()
        );
    }
    Ok(())
}

pub fn record_success(url: &str) {
    with_breaker(|b| b.record_success(url));
}

pub fn record_failure(url: &str, stderr: &str) -> Verdict {
    let verdict = with_breaker(|b| b.record_failure(stderr, Instant::now()));
    match &verdict {
        Verdict::Continue => {}
        Verdict::Canary(canary) => {
            log::warn!(
                "youtube: failure streak ending at {url}; fetching known-good {canary} to tell dead videos from a rate limit"
            )
        }
        Verdict::Paused => log_paused(url),
    }
    verdict
}

/// Report the canary fetch started by a [`Verdict::Canary`].
pub fn canary_result(canary: &str, ok: bool) {
    with_breaker(|b| b.canary_result(ok, Instant::now()));
    if ok {
        log::info!("youtube: canary {canary} fetched; the failure streak was dead videos");
    } else {
        log_paused(canary);
    }
}

fn log_paused(url: &str) {
    let pause_for = with_breaker(|b| b.pause_for);
    log::error!(
        "youtube: {PAUSED} for {}s: failures included a known-good video ({url}); every YouTube fetch fails fast until then",
        pause_for.as_secs()
    );
}

#[cfg(test)]
mod tests;
