#![deny(dead_code)]
#![deny(unused_variables)]
// Lib invariant: borg pub fns return typed data; sb owns stdout/stderr.
// Production code emits nothing via println!/eprintln! - log::* / tracing::*
// route through the logger initializer instead. Test modules that print
// captured stdout opt in via #[cfg_attr(test, allow(...))] on the test
// declaration.
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

pub use vault;

pub mod assets;
pub mod audit;
pub mod backfill;
pub mod backoff;
pub mod blocklist;
pub mod byline;
pub mod config;
pub mod dedupe;
pub mod description;
pub mod discord;
pub mod dispatch;
pub mod error;
pub mod eval;
pub mod extension;
pub mod extraction;
pub mod fabric;
pub mod github;
pub mod harvest;
pub mod health;
pub mod hygiene;
pub mod intake;
pub mod jina;
pub mod ledger;
pub mod markdown;
pub mod migrate;
pub mod notify;
pub mod ntfy;
pub mod ocr;
pub mod opts;
pub mod pipeline;
pub mod quality;
pub mod queue;
pub mod readability;
pub mod receipts;
pub mod replay;
pub mod retention;
pub mod rkvr;
pub mod router;
pub mod routes;
pub mod service;
pub mod signal;
pub mod slides;
pub mod stages;
pub mod startup;
#[cfg(test)]
pub(crate) mod stub;
pub mod telegram;
pub mod thread;
pub mod trace;
pub mod transcription;
pub mod triage;
pub mod types;
pub mod watchdog;
pub mod youtube;

mod client;
mod server;

use eyre::Result;

use config::Config;

// Daemon lifecycle + OS service management live in `service` (Phase 8 split);
// re-exported so the public API (`borg::daemon`, `borg::DaemonOutcome`) is
// unchanged for sb's CLI dispatch.
pub use client::{
    IngestOutcome, ReingestCandidate, ReingestEntry, ReingestEntryStatus, ReingestEvent, ReingestReport, ingest,
    ingest_file, note, reingest, resolve_ingest_url, resolve_note_text,
};
pub use server::{AppState, ServerHandle, ServerStartup, SubsystemStatus, build_router, serve_init};
pub use service::{DaemonOutcome, daemon};
pub use signal::{SignalProbe, probe_signal};
pub use telegram::probe_telegram;

/// Outcome of `borg::hotkey`. sb prints the user-facing summary for each
/// variant; `NoAction` is a usage-help case that sb maps to an exit-1.
#[derive(Debug)]
pub enum HotkeyOutcome {
    Installed {
        key: String,
        command: String,
        host: String,
        port: u16,
        post_install: Option<String>,
    },
    Uninstalled,
    NoAction,
}

pub async fn hotkey(opts: opts::HotkeyOpts, config: &Config) -> Result<HotkeyOutcome> {
    // CLI key overrides config; the default value falls back to config
    let key = if opts.key == "<Ctrl><Shift>b" { config.hotkey.key.clone() } else { opts.key };

    if opts.install {
        let (command, post_install) = service::install_hotkey(&key).await?;
        Ok(HotkeyOutcome::Installed {
            key,
            command,
            host: config.hotkey.host.clone(),
            port: config.hotkey.port,
            post_install,
        })
    } else if opts.uninstall {
        service::uninstall_hotkey().await?;
        Ok(HotkeyOutcome::Uninstalled)
    } else {
        Ok(HotkeyOutcome::NoAction)
    }
}
