//! The borg daemon: HTTP router, shared state, startup snapshot types, and
//! `serve_init`, which boots every subsystem. Per-transport start fns live in
//! `server/transports.rs`.

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};

/// Maximum accepted multipart upload size. Sized to the largest supported
/// attachment (audio/video documents); larger bodies are rejected with 413
/// at the layer instead of axum's undocumented default 2 MB limit silently
/// 413-ing legitimate uploads.
const MAX_UPLOAD_BYTES: usize = 64 * 1024 * 1024;
use eyre::{Context, Result};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tower_http::cors::{Any, CorsLayer};

use crate::config::{self, Config};
use crate::notify::{Desktop, Telegram};
use crate::{ledger, retention, routes, startup, watchdog};

mod transports;

/// Shared application state for the HTTP server and daemon tasks.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub telegram: Option<Telegram>,
    pub desktop: Option<Desktop>,
    pub version: String,
    /// The resolved auth token for the HTTP write routes (env var / file
    /// already read via `vault::config::resolve_secret` at startup), or
    /// `None` when no token is configured. Holds the literal secret, not the
    /// reference. See `routes::require_auth`.
    pub auth_token: Option<String>,
}

pub fn build_router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([axum::http::Method::GET, axum::http::Method::POST])
        .allow_headers([axum::http::header::CONTENT_TYPE, axum::http::header::AUTHORIZATION]);

    // Write routes sit behind the auth gate; `/health*` stays open so probes
    // and the dashboard never need a token. The gate runs before the handler,
    // so a 401 never reaches intake (no receipt, no sidecar).
    let protected = Router::new()
        .route("/ingest", post(routes::ingest))
        .route(
            "/ingest/file",
            post(routes::ingest_multipart).layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES)),
        )
        .route("/note", post(routes::note))
        // Replay/reingest poll this for a trace's terminal state (the receipts
        // DB is per-host on the daemon; client hosts can't read it directly).
        // Auth-gated alongside the write routes.
        .route("/trace/{trace_id}", get(routes::trace_state))
        // Ingest-queue snapshot (batch progress, wedged items). Auth-gated:
        // it carries source URLs and failure reasons.
        .route("/queue", get(routes::queue))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            routes::require_auth,
        ));

    Router::new()
        .route("/health", get(routes::health))
        .route("/health/audit", get(routes::health_audit))
        .merge(protected)
        .layer(cors)
        .with_state(state)
}

/// Status of one of borg's startup subsystems (telegram / discord / ntfy /
/// watchdog). Populated by `serve_init` so sb can render the startup banner
/// without the lib touching stdout.
#[derive(Debug, Clone)]
pub enum SubsystemStatus {
    Active,
    ActiveWithDetail(String),
    SkippedNoToken,
    SkippedHostMismatch,
    Disabled,
}

/// Snapshot returned by `serve_init` capturing the per-subsystem startup
/// outcome. sb prints the banner from these fields.
#[derive(Debug, Clone)]
pub struct ServerStartup {
    pub addr: SocketAddr,
    pub telegram: SubsystemStatus,
    pub telegram_bot: SubsystemStatus,
    pub discord: SubsystemStatus,
    pub ntfy: SubsystemStatus,
    pub signal: SubsystemStatus,
    pub desktop: SubsystemStatus,
    pub watchdog: SubsystemStatus,
}

/// Opaque wrapper around the internal tokio::task::JoinSet. Keeping the
/// concurrency primitive private means sb has no compile-time dependency on
/// tokio's JoinSet type. The only operation sb performs is `wait().await`.
pub struct ServerHandle {
    tasks: tokio::task::JoinSet<Result<()>>,
}

impl ServerHandle {
    /// Await any of the spawned tasks to exit. Under normal operation this
    /// blocks until ctrl-C / SIGTERM kills the daemon.
    ///
    /// Fail-fast: the first task that resolves to `Err` (or panics) aborts the
    /// remaining tasks and propagates the error out, so `Restart=always` and
    /// `sb doctor` actually observe the failure. Previously such errors were
    /// logged and the supervisor kept waiting on the survivors - a transport
    /// or watcher task could die while the process stayed "up", the
    /// "worked-for-weeks-then-broke" silent-degradation class.
    pub async fn wait(mut self) -> Result<()> {
        while let Some(result) = self.tasks.join_next().await {
            match result {
                Ok(Ok(())) => log::info!("a daemon task exited cleanly"),
                Ok(Err(e)) => {
                    log::error!("a daemon task failed: {e:#}");
                    self.tasks.abort_all();
                    return Err(e);
                }
                Err(e) => {
                    if e.is_panic() {
                        log::error!("a daemon task panicked: {e}");
                    } else {
                        log::error!("a daemon task was cancelled: {e}");
                    }
                    self.tasks.abort_all();
                    return Err(eyre::eyre!("daemon task did not complete: {e}"));
                }
            }
        }
        Ok(())
    }
}

/// Boot every borg subsystem (HTTP server, telegram, discord, ntfy, watchdog)
/// and return a startup snapshot plus an opaque handle the caller awaits.
pub async fn serve_init(config: Config, version: String) -> Result<(ServerStartup, ServerHandle)> {
    log::info!("Starting borg daemon");

    // Refuse to start without canonical assets present and parseable. The
    // alternative (silent-degrade ingest) lets junk tags accumulate in the
    // vault and breaks the canonical contract every other subsystem
    // depends on. Operator gets an actionable `sb bootstrap` pointer.
    startup::validate_canonical_assets(&config.tags.canonical_path).context("borg::serve_init")?;

    let addr: SocketAddr = format!("{}:{}", config.server.host, config.server.port)
        .parse()
        .context("Invalid server address")?;

    // Resolve the optional write-route auth token. Like telegram.bot-token,
    // `server.auth-token` holds a secret *reference* (env-var name or file
    // path), resolved here via the same mechanism. If a token is configured
    // but unresolvable, fail closed: the operator opted into auth, so silently
    // running the write routes unauthenticated would be a security downgrade.
    let auth_token: Option<String> = match &config.server.auth_token {
        Some(reference) => {
            Some(config::resolve_secret(reference).context("resolving server.auth-token (write-route auth)")?)
        }
        None => None,
    };
    let host_is_loopback = matches!(config.server.host.as_str(), "127.0.0.1" | "::1" | "localhost");
    if !host_is_loopback && auth_token.is_none() {
        log::warn!(
            "borg HTTP server bound to non-loopback address {} with no auth-token: the /ingest, \
             /ingest/file, and /note write routes are reachable unauthenticated. Set \
             server.auth-token (an env-var name or file path) to require a Bearer token.",
            config.server.host
        );
    }

    log::info!("Server address: {addr}");
    log::debug!("Vault inbox: {}", config.inbox_dir()?.display());
    log::debug!("Transcriber URL: {}", config.transcriber.url);
    log::debug!("Groq model: {}", config.groq.model);
    log::debug!("LLM provider: {}, model: {}", config.llm.provider, config.llm.model);

    // Ensure vault system files exist on startup. vault_root must resolve here
    // - the daemon cannot start without one.
    let ledger_p = ledger::ledger_path()?;
    if let Err(e) = ledger::ensure_ledger_exists(&ledger_p) {
        log::warn!("Failed to ensure Borg Ledger exists: {e:#}");
    }
    // borg-dashboard.md (Dataview) was retired in favour of the live-updating
    // borg-ledger.base view; its `WHERE ingested = date(today)` queries broke
    // once `ingested:` became a datetime. The ledger stays as the dedup datastore.

    let config = Arc::new(config);
    let mut tasks = tokio::task::JoinSet::new();

    // Build the shared Telegram notifier (if configured)
    let mut telegram: Option<Telegram> = None;
    let mut resolved_tg_token: Option<String> = None;
    let mut telegram_status = SubsystemStatus::Disabled;

    if let Some(tg_config) = &config.telegram {
        match config::resolve_secret(&tg_config.bot_token) {
            Ok(token) => {
                telegram = Telegram::new(&token, tg_config);
                resolved_tg_token = Some(token);
                telegram_status = if telegram.is_some() {
                    SubsystemStatus::Active
                } else {
                    SubsystemStatus::Disabled
                };
            }
            Err(e) => {
                log::warn!("Telegram configured but token not available: {e:#}");
                telegram_status = SubsystemStatus::SkippedNoToken;
            }
        }
    }

    // Build the desktop notifier (host-gated; mirrors telegram/discord/ntfy)
    let mut desktop: Option<Desktop> = None;
    let mut desktop_status = SubsystemStatus::Disabled;
    if let Some(dn_config) = &config.desktop {
        if !config::is_local_host(&dn_config.host) {
            log::info!(
                "Desktop notifier configured but host {:?} does not match this machine, skipping",
                dn_config.host
            );
            desktop_status = SubsystemStatus::SkippedHostMismatch;
        } else {
            desktop = Desktop::new(dn_config);
            desktop_status = if desktop.is_some() {
                SubsystemStatus::Active
            } else {
                SubsystemStatus::Disabled
            };
        }
    }

    // HTTP server (always runs)
    let state = AppState {
        config: config.clone(),
        telegram: telegram.clone(),
        desktop: desktop.clone(),
        version: version.clone(),
        auth_token,
    };
    let app = build_router(state);
    let listener = TcpListener::bind(addr).await.context("Failed to bind to address")?;
    tasks.spawn(async move { axum::serve(listener, app).await.map_err(|e| eyre::eyre!(e)) });
    log::info!("HTTP server listening on {addr}");

    let telegram_bot_status =
        transports::start_telegram_bot(&mut tasks, &config, &resolved_tg_token, &telegram, &desktop);
    let discord_status = transports::start_discord(&mut tasks, &config, &desktop);
    let ntfy_status = transports::start_ntfy(&mut tasks, &config, &telegram, &desktop);
    let signal_status = transports::start_signal(&mut tasks, &config, &desktop)?;

    // Watchdog
    {
        let cfg = config.clone();
        tasks.spawn(async move {
            watchdog::run(cfg).await;
            Err::<(), eyre::Report>(eyre::eyre!("watchdog exited unexpectedly"))
        });
    }

    // Raw-input sidecar retention. Returns immediately (Ok) when the window is
    // disabled, so a `intake.retention-days: 0` config is not an error state.
    {
        let cfg = config.clone();
        tasks.spawn(async move {
            retention::run_sidecar_sweep(cfg).await;
            Ok::<(), eyre::Report>(())
        });
    }

    Ok((
        ServerStartup {
            addr,
            telegram: telegram_status,
            telegram_bot: telegram_bot_status,
            discord: discord_status,
            ntfy: ntfy_status,
            signal: signal_status,
            desktop: desktop_status,
            watchdog: SubsystemStatus::Active,
        },
        ServerHandle { tasks },
    ))
}

#[cfg(test)]
mod tests;
