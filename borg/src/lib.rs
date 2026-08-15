#![deny(clippy::unwrap_used)]
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
pub mod telegram;
pub mod thread;
pub mod trace;
pub mod transcription;
pub mod triage;
pub mod types;
pub mod watchdog;
pub mod youtube;

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

use config::Config;
use notify::{Desktop, Telegram};

// Daemon lifecycle + OS service management live in `service` (Phase 8 split);
// re-exported so the public API (`borg::daemon`, `borg::DaemonOutcome`) is
// unchanged for sb's CLI dispatch.
pub use service::{DaemonOutcome, daemon};
pub use signal::{SignalProbe, probe_signal};
pub use telegram::probe_telegram;

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
    log::info!("Starting obsidian-borg daemon");

    // Refuse to start without canonical assets present and parseable. The
    // alternative (silent-degrade ingest) lets junk tags accumulate in the
    // vault and breaks the canonical contract every other subsystem
    // depends on. Operator gets an actionable `sb bootstrap` pointer.
    startup::validate_canonical_assets().context("borg::serve_init")?;

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
            tasks.spawn(async move { ntfy::run(server, topic, token, cfg, tg, desk).await });
            ntfy_status = SubsystemStatus::ActiveWithDetail(format!("topic: {topic_for_status}"));
        }
    }

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

    // Watchdog
    {
        let cfg = config.clone();
        tasks.spawn(async move {
            watchdog::run(cfg).await;
            Err::<(), eyre::Report>(eyre::eyre!("watchdog exited unexpectedly"))
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

/// Thin wrapper preserved for internal callers (daemon::run with --start).
/// New sb code paths should use `serve_init` + `ServerHandle::wait` to get
/// the typed startup banner instead.
pub async fn serve(config: Config, version: String) -> Result<()> {
    let (_startup, handle) = serve_init(config, version).await?;
    handle.wait().await
}

pub fn resolve_note_text(text: Option<String>, clipboard: bool) -> Result<String> {
    if let Some(text) = text {
        return Ok(text);
    }
    if clipboard {
        let mut board = arboard::Clipboard::new().context("Failed to access clipboard")?;
        let text = board.get_text().context("Clipboard is empty or not text")?;
        let text = text.trim().to_string();
        if text.is_empty() {
            eyre::bail!("Clipboard is empty");
        }
        return Ok(text);
    }
    eyre::bail!("No text provided. Use a text argument or --clipboard")
}

/// Outcome of a borg ingest / note / ingest-file invocation. sb maps each
/// variant to the corresponding stdout/stderr/exit-code combo.
#[derive(Debug)]
pub enum IngestOutcome {
    Captured { title: String, path: String },
    Duplicate { original_date: String },
    Failed { reason: String },
    Queued,
}

pub async fn note(config: Config, text: String, tags: Option<Vec<String>>) -> Result<IngestOutcome> {
    let trace_id = trace::generate(types::IngestMethod::Cli);
    intake::record_received_with_sidecar(
        &config,
        types::IngestMethod::Cli,
        intake::Kind::Text,
        &intake::preview_text(&text),
        text.as_bytes(),
        &trace_id,
    )
    .context("Failed to record cli intake")?;

    let content = types::ContentKind::Text(text);
    let result = pipeline::process_content(
        content,
        tags.unwrap_or_default(),
        types::IngestMethod::Cli,
        false,
        &config,
        Some(trace_id),
        None,
    )
    .await;

    Ok(match result.status {
        types::IngestStatus::Completed => IngestOutcome::Captured {
            title: result.title.unwrap_or_else(|| "Untitled".to_string()),
            path: result.note_path.unwrap_or_else(|| "unknown".to_string()),
        },
        types::IngestStatus::Failed { reason } => IngestOutcome::Failed { reason },
        types::IngestStatus::Duplicate { original_date } => IngestOutcome::Duplicate { original_date },
        types::IngestStatus::Queued => IngestOutcome::Queued,
    })
}

pub async fn ingest_file(
    config: Config,
    file_path: std::path::PathBuf,
    tags: Option<Vec<String>>,
    force: bool,
) -> Result<IngestOutcome> {
    let filename = file_path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let data = std::fs::read(&file_path).context(format!("Failed to read file: {}", file_path.display()))?;

    let trace_id = trace::generate(types::IngestMethod::Cli);
    let intake_kind = if assets::is_image_extension(&filename) {
        intake::Kind::Photo
    } else if assets::is_audio_extension(&filename) {
        intake::Kind::Audio
    } else if assets::is_pdf_extension(&filename) || assets::is_document_extension(&filename) {
        intake::Kind::Document
    } else {
        intake::Kind::Unknown
    };
    let preview = intake::binary_descriptor(intake_kind, &filename, data.len(), None);
    intake::record_received_with_sidecar(
        &config,
        types::IngestMethod::Cli,
        intake_kind,
        &preview,
        preview.as_bytes(),
        &trace_id,
    )
    .context("Failed to record cli intake")?;

    let content = if assets::is_image_extension(&filename) {
        types::ContentKind::Image { data, filename }
    } else if assets::is_pdf_extension(&filename) {
        types::ContentKind::Pdf { data, filename }
    } else if assets::is_document_extension(&filename) {
        types::ContentKind::Document { data, filename }
    } else if assets::is_audio_extension(&filename) {
        types::ContentKind::Audio { data, filename }
    } else {
        let all_extensions: Vec<&str> = assets::IMAGE_EXTENSIONS
            .iter()
            .chain(assets::PDF_EXTENSIONS.iter())
            .chain(assets::DOCUMENT_EXTENSIONS.iter())
            .chain(assets::AUDIO_EXTENSIONS.iter())
            .copied()
            .collect();
        let reason = format!(
            "Unsupported file type: {}. Supported extensions: {}",
            filename,
            all_extensions.join(", ")
        );
        intake::record_failure_at_door(
            types::IngestMethod::Cli,
            &trace_id,
            vault::receipts::FailureStage::IntakeRejected,
            &reason,
        );
        eyre::bail!("{reason}");
    };

    let result = pipeline::process_content(
        content,
        tags.unwrap_or_default(),
        types::IngestMethod::Cli,
        force,
        &config,
        Some(trace_id),
        None,
    )
    .await;

    Ok(match result.status {
        types::IngestStatus::Completed => IngestOutcome::Captured {
            title: result.title.unwrap_or_else(|| "Untitled".to_string()),
            path: result.note_path.unwrap_or_else(|| "unknown".to_string()),
        },
        types::IngestStatus::Failed { reason } => IngestOutcome::Failed { reason },
        types::IngestStatus::Duplicate { original_date } => IngestOutcome::Duplicate { original_date },
        types::IngestStatus::Queued => IngestOutcome::Queued,
    })
}

pub fn resolve_ingest_url(url: Option<String>, clipboard: bool) -> Result<String> {
    if let Some(url) = url {
        return Ok(url);
    }
    if clipboard {
        let mut board = arboard::Clipboard::new().context("Failed to access clipboard")?;
        let text = board.get_text().context("Clipboard is empty or not text")?;
        let text = text.trim().to_string();
        if text.is_empty() {
            eyre::bail!("Clipboard is empty");
        }
        if !text.starts_with("http://") && !text.starts_with("https://") {
            eyre::bail!("Clipboard content is not a URL: {text}");
        }
        return Ok(text);
    }
    eyre::bail!("No URL provided. Use a URL argument or --clipboard")
}

/// Streamed progress event from `borg::reingest`. Emitted via the caller-
/// supplied callback so sb can print as each ledger entry is visited - the
/// architect-flagged case (sequential HTTP per entry; buffering would silence
/// the CLI for 10+ minutes).
#[derive(Debug)]
pub enum ReingestEvent {
    Matched {
        count: usize,
        dry_run: bool,
    },
    ItemStart {
        index: usize,
        total: usize,
        date: String,
        slug: String,
        source: String,
    },
    ItemReplaced {
        title: String,
    },
    ItemFailed {
        reason: String,
    },
    ItemOther(String),
    ItemError(String),
    Complete {
        dry_run: bool,
    },
    NoMatches,
}

/// Aggregate summary of a reingest run. The streaming `ReingestEvent` callback
/// drives live UX (so a 10-minute 800-entry run never goes silent); this struct
/// is the final typed contract the design doc specified: callers that don't
/// want to handle events still get a structural summary.
///
/// Mirrors the dry-run/apply disambiguation rule used elsewhere in the workspace:
/// `would_process` is populated only in dry-run mode; `processed` only in apply mode.
#[derive(Debug, Default, Clone)]
pub struct ReingestReport {
    pub matched: usize,
    pub would_process: Vec<ReingestCandidate>,
    pub processed: Vec<ReingestEntry>,
}

#[derive(Debug, Clone)]
pub struct ReingestCandidate {
    pub date: String,
    pub slug: String,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct ReingestEntry {
    pub source: String,
    pub status: ReingestEntryStatus,
}

#[derive(Debug, Clone)]
pub enum ReingestEntryStatus {
    Replaced { title: String },
    Failed { reason: String },
    Other(String),
    Error(String),
}

/// Reingest existing ledger entries via the daemon's ingest endpoint.
///
/// Emits a streaming `ReingestEvent` per matched entry through the caller-
/// supplied progress callback; sb prints as they arrive so the user sees
/// progress on 800-row ledgers (~10+ minutes of sequential HTTP work). The
/// returned `ReingestReport` is the design-spec'd aggregate summary; the
/// callback gives live UX, the report gives a structural contract for
/// programmatic callers. `ReingestEvent::Complete` exists as the streaming
/// signal of run completion; the typed return is the post-hoc summary.
pub async fn reingest(
    config: Config,
    all: bool,
    content_type: Option<String>,
    domain: Option<String>,
    source: Option<String>,
    before: Option<String>,
    after: Option<String>,
    dry_run: bool,
    mut progress: impl FnMut(&ReingestEvent) + Send,
) -> Result<ReingestReport> {
    use ledger::{EntryFilter, QueriedEntry};

    if !all && source.is_none() && content_type.is_none() && domain.is_none() {
        eyre::bail!("Specify --all, --source <URL>, --type <TYPE>, or --domain <DOMAIN> to select entries");
    }

    let ledger_file = ledger::ledger_path()?;

    let filter = EntryFilter {
        source: source.clone(),
        domain: domain.clone(),
        before,
        after,
    };

    let entries: Vec<QueriedEntry> = ledger::query_entries(&ledger_file, &filter)?;

    let entries: Vec<QueriedEntry> = if let Some(ref type_filter) = content_type {
        let vault_root = config.vault_root()?;
        entries
            .into_iter()
            .filter(|e| {
                if e.filename == "-" {
                    return false;
                }
                let note_path = [
                    vault_root.join("notes").join(&e.filename),
                    vault_root.join("inbox").join(&e.filename),
                ]
                .into_iter()
                .find(|p| p.exists());
                let Some(note_path) = note_path else {
                    return false;
                };
                match std::fs::read_to_string(&note_path) {
                    Ok(content) => content
                        .lines()
                        .any(|l| l.trim().starts_with("type:") && l.contains(type_filter)),
                    Err(_) => false,
                }
            })
            .collect()
    } else {
        entries
    };

    let mut report = ReingestReport::default();

    if entries.is_empty() {
        progress(&ReingestEvent::NoMatches);
        return Ok(report);
    }

    report.matched = entries.len();
    progress(&ReingestEvent::Matched {
        count: entries.len(),
        dry_run,
    });

    for (i, entry) in entries.iter().enumerate() {
        progress(&ReingestEvent::ItemStart {
            index: i,
            total: entries.len(),
            date: entry.date.clone(),
            slug: entry.slug.clone(),
            source: entry.source.clone(),
        });

        if dry_run {
            report.would_process.push(ReingestCandidate {
                date: entry.date.clone(),
                slug: entry.slug.clone(),
                source: entry.source.clone(),
            });
            continue;
        }

        let host = &config.hotkey.host;
        let port = config.hotkey.port;
        let endpoint = format!("http://{host}:{port}/ingest");

        let body = serde_json::json!({
            "url": entry.source,
            "tags": [],
            "force": true,
            "method": "cli",
        });

        let client = reqwest::Client::new();
        let mut req = client.post(&endpoint).json(&body);
        if let Some(token) = config::resolve_client_auth_token(&config.server) {
            req = req.bearer_auth(token);
        }
        let status = match req.send().await {
            Ok(response) => {
                let mut result: types::IngestResult =
                    response.json().await.context("Failed to parse response from daemon")?;
                // The daemon answers `Queued`; poll `/trace/{id}` for the real
                // terminal state so reingest reports accurate counts and paces
                // one entry at a time.
                if matches!(result.status, types::IngestStatus::Queued)
                    && let Some(tid) = result.trace_id.clone()
                {
                    result = replay::poll_trace_terminal(&config, host, port, &tid)
                        .await
                        .unwrap_or(result);
                }
                match result.status {
                    types::IngestStatus::Completed => {
                        let title = result.title.unwrap_or_else(|| "Untitled".to_string());
                        progress(&ReingestEvent::ItemReplaced { title: title.clone() });
                        ReingestEntryStatus::Replaced { title }
                    }
                    types::IngestStatus::Failed { reason } => {
                        progress(&ReingestEvent::ItemFailed { reason: reason.clone() });
                        ReingestEntryStatus::Failed { reason }
                    }
                    other => {
                        let s = format!("{other:?}");
                        progress(&ReingestEvent::ItemOther(s.clone()));
                        ReingestEntryStatus::Other(s)
                    }
                }
            }
            Err(e) => {
                if e.is_connect() {
                    eyre::bail!("Cannot reach obsidian-borg at http://{host}:{port} - is the daemon running?");
                }
                let s = e.to_string();
                progress(&ReingestEvent::ItemError(s.clone()));
                ReingestEntryStatus::Error(s)
            }
        };
        report.processed.push(ReingestEntry {
            source: entry.source.clone(),
            status,
        });
    }

    progress(&ReingestEvent::Complete { dry_run });

    Ok(report)
}

pub async fn ingest(
    config: Config,
    url: String,
    tags: Option<Vec<String>>,
    force: bool,
    method: types::IngestMethod,
) -> Result<IngestOutcome> {
    let host = &config.hotkey.host;
    let port = config.hotkey.port;
    let endpoint = format!("http://{host}:{port}/ingest");

    let body = serde_json::json!({
        "url": url,
        "tags": tags.unwrap_or_default(),
        "force": force,
        "method": method,
    });

    let client = reqwest::Client::new();
    // Send the write-route Bearer token when one is configured, so enabling
    // server.auth-token doesn't 401 this first-party CLI path.
    let mut req = client.post(&endpoint).json(&body);
    if let Some(token) = config::resolve_client_auth_token(&config.server) {
        req = req.bearer_auth(token);
    }
    // The Error toast here is unconditional and load-bearing: when the HTTP
    // POST itself fails (daemon not running), the daemon by definition
    // cannot deliver the failure notification. The CLI may be wired to a
    // desktop hotkey where stderr is not visible. This is the symmetric
    // counterpart to the `fail()` / `catch (err)` path in popup.js.
    let response = req.send().await.map_err(|e| {
        let msg = if e.is_connect() {
            format!("cannot reach obsidian-borg at http://{host}:{port} - is the daemon running?")
        } else {
            format!("{e}")
        };
        send_notification("Error", &msg);
        eyre::eyre!("{msg}")
    })?;

    let result: types::IngestResult = response.json().await.context("Failed to parse response from daemon")?;

    Ok(match result.status {
        types::IngestStatus::Completed => {
            let title = result.title.unwrap_or_else(|| "Untitled".to_string());
            let path = result.note_path.unwrap_or_else(|| "unknown".to_string());
            IngestOutcome::Captured { title, path }
        }
        types::IngestStatus::Duplicate { original_date } => IngestOutcome::Duplicate { original_date },
        types::IngestStatus::Failed { reason } => IngestOutcome::Failed { reason },
        types::IngestStatus::Queued => IngestOutcome::Queued,
    })
}

/// Display duration for the synchronous hotkey-path toast. Distinct from
/// `notify::Desktop`'s D-Bus call-timeout (a wedge-detection bound) — this is
/// how long the toast stays on screen, so it is intentionally longer.
const HOTKEY_NOTIFY_DISPLAY_MS: u32 = 5000;

fn send_notification(summary: &str, body: &str) {
    if notify::real_notifications_disabled() {
        log::debug!("send_notification: suppressed under test (summary={summary:?})");
        return;
    }
    let _ = notify_rust::Notification::new()
        .appname(config::APP_NAME)
        .summary(&format!("obsidian-borg: {summary}"))
        .body(body)
        .timeout(notify_rust::Timeout::Milliseconds(HOTKEY_NOTIFY_DISPLAY_MS))
        .show();
}

/// Outcome of `borg::hotkey`. sb prints the user-facing summary for each
/// variant; `NoAction` is a usage-help case that sb maps to an exit-1.
#[derive(Debug)]
pub enum HotkeyOutcome {
    Installed {
        key: String,
        host: String,
        port: u16,
        post_install: Option<String>,
    },
    Uninstalled,
    NoAction,
}

pub async fn hotkey(opts: opts::HotkeyOpts, config: &Config) -> Result<HotkeyOutcome> {
    // CLI args override config; if CLI has default values, fall back to config
    let host = if opts.host == "localhost" { config.hotkey.host.clone() } else { opts.host };
    let port = if opts.port == 8181 { config.hotkey.port } else { opts.port };
    let key = if opts.key == "<Ctrl><Shift>b" { config.hotkey.key.clone() } else { opts.key };

    if opts.install {
        let post_install = service::install_hotkey(&host, port, &key).await?;
        Ok(HotkeyOutcome::Installed {
            key,
            host,
            port,
            post_install,
        })
    } else if opts.uninstall {
        service::uninstall_hotkey().await?;
        Ok(HotkeyOutcome::Uninstalled)
    } else {
        Ok(HotkeyOutcome::NoAction)
    }
}

#[cfg(test)]
mod tests;
