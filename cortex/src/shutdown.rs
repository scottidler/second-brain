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

#[cfg(unix)]
use tokio::signal::unix::{Signal, SignalKind, signal};

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
