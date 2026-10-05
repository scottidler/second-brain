//! One start fn per transport subsystem (telegram bot, discord, ntfy, signal).
//! Each host-gates on its config block, spawns its task into `serve_init`'s
//! JoinSet, and returns the `SubsystemStatus` the startup banner reports.

use eyre::{Context, Result};
use std::sync::Arc;
use tokio::task::JoinSet;

use super::SubsystemStatus;
use crate::config::{self, Config};
use crate::notify::{Desktop, Telegram};
use crate::{discord, ntfy, signal, telegram};

pub(super) fn start_telegram_bot(
    tasks: &mut JoinSet<Result<()>>,
    config: &Arc<Config>,
    resolved_tg_token: &Option<String>,
    telegram: &Option<Telegram>,
    desktop: &Option<Desktop>,
) -> SubsystemStatus {
    // Telegram bot (config-driven, host-gated)
    let mut telegram_bot_status = SubsystemStatus::Disabled;
    if let Some(tg_config) = &config.telegram {
        if !config::is_local_host(&tg_config.host) {
            log::info!(
                "Telegram configured but host {:?} does not match this machine, skipping",
                tg_config.host
            );
            telegram_bot_status = SubsystemStatus::SkippedHostMismatch;
        } else if let Some(token) = resolved_tg_token.clone() {
            log::info!(
                "Telegram bot enabled (allowed_chat_ids: {:?})",
                tg_config.allowed_chat_ids
            );
            let tg_cfg = tg_config.clone();
            let cfg = config.clone();
            let tg = telegram.clone();
            let desk = desktop.clone();
            tasks.spawn(async move { telegram::run(token, tg_cfg, cfg, tg, desk).await });
            telegram_bot_status = SubsystemStatus::Active;
        } else {
            telegram_bot_status = SubsystemStatus::SkippedNoToken;
        }
    }
    telegram_bot_status
}

pub(super) fn start_discord(
    tasks: &mut JoinSet<Result<()>>,
    config: &Arc<Config>,
    desktop: &Option<Desktop>,
) -> SubsystemStatus {
    // Discord bot (config-driven, host-gated)
    let mut discord_status = SubsystemStatus::Disabled;
    if let Some(dc_config) = &config.discord {
        if !config::is_local_host(&dc_config.host) {
            log::info!(
                "Discord configured but host {:?} does not match this machine, skipping",
                dc_config.host
            );
            discord_status = SubsystemStatus::SkippedHostMismatch;
        } else {
            match config::resolve_secret(&dc_config.bot_token) {
                Ok(token) => {
                    log::info!("Discord bot enabled (channel_id: {})", dc_config.channel_id);
                    let dc = dc_config.clone();
                    let cfg = config.clone();
                    let desk = desktop.clone();
                    tasks.spawn(async move { discord::run(token, dc, cfg, desk).await });
                    discord_status = SubsystemStatus::Active;
                }
                Err(e) => {
                    log::warn!("Discord configured but token not available: {e:#}");
                    discord_status = SubsystemStatus::SkippedNoToken;
                }
            }
        }
    }
    discord_status
}

pub(super) fn start_ntfy(
    tasks: &mut JoinSet<Result<()>>,
    config: &Arc<Config>,
    telegram: &Option<Telegram>,
    desktop: &Option<Desktop>,
) -> SubsystemStatus {
    // ntfy subscriber (config-driven, host-gated)
    let mut ntfy_status = SubsystemStatus::Disabled;
    if let Some(ntfy_config) = &config.ntfy {
        if !config::is_local_host(&ntfy_config.host) {
            log::info!(
                "ntfy configured but host {:?} does not match this machine, skipping",
                ntfy_config.host
            );
            ntfy_status = SubsystemStatus::SkippedHostMismatch;
        } else {
            let server = ntfy_config.server.clone();
            let topic = ntfy_config.topic.clone();
            let token = ntfy_config.token.as_ref().and_then(|t| config::resolve_secret(t).ok());
            let cfg = config.clone();
            let tg = telegram.clone();
            let desk = desktop.clone();
            let topic_for_status = ntfy_config.topic.clone();
            let read_timeout = ntfy_config.read_timeout;
            tasks.spawn(async move { ntfy::run(server, topic, token, read_timeout, cfg, tg, desk).await });
            ntfy_status = SubsystemStatus::ActiveWithDetail(format!("topic: {topic_for_status}"));
        }
    }
    ntfy_status
}

pub(super) fn start_signal(
    tasks: &mut JoinSet<Result<()>>,
    config: &Arc<Config>,
    desktop: &Option<Desktop>,
) -> Result<SubsystemStatus> {
    // Signal transport (config-driven, host-gated, single-machine pin).
    // libsignal-protocol's storage futures are !Send, so signal cannot run
    // on tokio's multi-thread runtime. We spawn a dedicated OS thread that
    // owns a current-thread tokio runtime and a LocalSet; signal::run drives
    // its receive loop and per-envelope `spawn_local` dispatches there. The
    // bridge back into the JoinSet is a `oneshot::channel<Result<()>>` so
    // an Err (NotLinked / Deauthorized) still propagates to
    // `ServerHandle::wait` exactly like every other transport.
    let mut signal_status = SubsystemStatus::Disabled;
    if let Some(signal_config) = config.signal.clone() {
        if !config::is_local_host(&Some(signal_config.host.clone())) {
            log::info!(
                "Signal configured but host {:?} does not match this machine, skipping",
                signal_config.host
            );
            signal_status = SubsystemStatus::SkippedHostMismatch;
        } else {
            // Resolve the canonical signal state path ONCE here. This is the
            // operator-visible breadcrumb -- if the path the daemon expects
            // diverges from the one the operator passed to `signal-rs link
            // --state-dir`, the absolute resolved path is in the journal.
            let state_dir = vault::paths::borg_signal_state_dir();
            log::info!(
                "Signal transport enabled (state_dir: {}, allowed_senders: {})",
                state_dir.display(),
                signal_config.allowed_senders.len()
            );
            let cfg = config.clone();
            let desk = desktop.clone();
            let (tx, rx) = tokio::sync::oneshot::channel::<Result<()>>();
            std::thread::Builder::new()
                .name("signal-runtime".to_string())
                .spawn(move || {
                    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                        Ok(r) => r,
                        Err(e) => {
                            let _ = tx.send(Err(eyre::eyre!("signal: failed to build runtime: {e}")));
                            return;
                        }
                    };
                    let local = tokio::task::LocalSet::new();
                    let result = rt.block_on(local.run_until(signal::run(signal_config, state_dir, cfg, desk)));
                    let _ = tx.send(result);
                })
                .context("failed to spawn signal-runtime thread")?;
            tasks.spawn(async move {
                match rx.await {
                    Ok(res) => res,
                    Err(_) => Err(eyre::eyre!("signal: runtime thread dropped without reporting")),
                }
            });
            signal_status = SubsystemStatus::Active;
        }
    }
    Ok(signal_status)
}
