//! The daemon's stop signal (SIGTERM from systemd, SIGINT from a terminal).
//!
//! The listeners are created ONCE, by [`Shutdown::listen`], and live as long as
//! the value. Polling [`Shutdown::recv`] inside a looping `tokio::select!` is
//! safe: when another arm wins, only the `recv` future is dropped, never the
//! listener, so a signal that arrives while an arm's work runs is still pending
//! on the next iteration.
//!
//! Never build a fresh listener per iteration (`tokio::signal::ctrl_c()` or
//! `signal(SignalKind::terminate())` inside the loop). tokio delivers a signal
//! to the receivers alive at broadcast time; with none alive the event is
//! discarded, and a receiver created afterwards starts at the current version.
//! Because tokio has already replaced the default terminate action, the signal
//! is lost and the process ignores SIGTERM until systemd SIGKILLs it.
//!
//! [`StopHandle`] carries the signal INTO tick work. `select!` observes
//! `Shutdown` only between arms, and every arm runs its work inline under
//! `block_in_place`, so a long tick ignored SIGTERM until systemd SIGKILLed it.
//! [`StopHandle::listen`] moves the one `Shutdown` into a spawned task that sets
//! a flag; work reads it with [`StopHandle::is_stopped`] at its check points and
//! the loop's stop arm awaits [`StopHandle::stopped`].

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(unix)]
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::sync::Notify;

/// Long-lived SIGTERM + SIGINT listeners. Construct before the loop.
pub struct Shutdown {
    #[cfg(unix)]
    terminate: Option<Signal>,
    #[cfg(unix)]
    interrupt: Option<Signal>,
}

impl Shutdown {
    /// Install the listeners. Must run inside a tokio runtime. A listener that
    /// fails to install is logged and skipped; the other still stops the loop.
    pub fn listen() -> Self {
        log::debug!("Shutdown::listen: installing SIGTERM and SIGINT listeners");
        #[cfg(unix)]
        {
            let install = |name: &str, kind: SignalKind| match signal(kind) {
                Ok(listener) => Some(listener),
                Err(e) => {
                    log::warn!("daemon: failed to install {name} handler: {e}");
                    None
                }
            };
            let terminate = install("SIGTERM", SignalKind::terminate());
            let interrupt = install("SIGINT", SignalKind::interrupt());
            if terminate.is_none() && interrupt.is_none() {
                log::error!("daemon: no shutdown signal handler installed; only SIGKILL will stop it");
            }
            Self { terminate, interrupt }
        }
        #[cfg(not(unix))]
        {
            Self {}
        }
    }

    /// Resolve once a stop signal has been delivered to this listener set.
    /// Cancel-safe: dropping the future loses nothing.
    pub async fn recv(&mut self) {
        #[cfg(unix)]
        {
            match (self.terminate.as_mut(), self.interrupt.as_mut()) {
                (Some(term), Some(int)) => {
                    tokio::select! {
                        _ = term.recv() => {}
                        _ = int.recv() => {}
                    }
                }
                (Some(term), None) => {
                    term.recv().await;
                }
                (None, Some(int)) => {
                    int.recv().await;
                }
                (None, None) => std::future::pending::<()>().await,
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

/// The daemon's stop request, readable from blocking tick work.
///
/// Clones share one flag. Once set it never clears: a stop is final.
#[derive(Clone, Debug, Default)]
pub struct StopHandle {
    inner: Arc<StopState>,
}

#[derive(Debug, Default)]
struct StopState {
    stopped: AtomicBool,
    notify: Notify,
}

impl StopHandle {
    /// Install the SIGTERM/SIGINT listeners and spawn the task that owns them.
    /// The first signal sets the flag and wakes every [`StopHandle::stopped`]
    /// waiter. Must run inside a tokio runtime.
    pub fn listen() -> Self {
        log::debug!("StopHandle::listen: spawning the stop-signal listener task");
        let handle = Self::default();
        let mut shutdown = Shutdown::listen();
        let setter = handle.clone();
        tokio::spawn(async move {
            shutdown.recv().await;
            log::info!("stop signal received; stopping at the next check point");
            setter.stop();
        });
        handle
    }

    /// A handle no signal sets, for callers outside the daemon loop (one-shot
    /// `sb cortex ...` commands, tests). Their Ctrl-C still goes through
    /// `vault::process::install_interrupt_handler`.
    pub fn never() -> Self {
        Self::default()
    }

    /// True once a stop signal has arrived. One atomic load; call it freely.
    pub fn is_stopped(&self) -> bool {
        self.inner.stopped.load(Ordering::Acquire)
    }

    /// Resolve once a stop signal has arrived (at once if it already has).
    /// Cancel-safe: dropping the future loses nothing, the flag stays set.
    pub async fn stopped(&self) {
        let notified = self.inner.notify.notified();
        tokio::pin!(notified);
        // Register before reading the flag, so a stop landing between the read
        // and the await still wakes this waiter.
        notified.as_mut().enable();
        if self.is_stopped() {
            return;
        }
        notified.await;
    }

    fn stop(&self) {
        self.inner.stopped.store(true, Ordering::Release);
        self.inner.notify.notify_waiters();
    }
}

#[cfg(test)]
mod tests;
