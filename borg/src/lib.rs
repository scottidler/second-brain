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
pub mod config;
pub mod dashboard;
pub mod description;
pub mod discord;
pub mod error;
pub mod extraction;
pub mod fabric;
pub mod github;
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
pub mod replay;
pub mod retention;
pub mod router;
pub mod routes;
pub mod slides;
pub mod stages;
pub mod startup;
pub mod telegram;
pub mod trace;
pub mod transcription;
pub mod triage;
pub mod types;
pub mod watchdog;
pub mod youtube;

use axum::Router;
use axum::routing::{get, post};
use eyre::{Context, Result};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;
use tower_http::cors::{Any, CorsLayer};

use config::Config;
use notify::Notifier;

/// Shared application state for the HTTP server and daemon tasks.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub notifier: Option<Notifier>,
}

pub fn build_router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([axum::http::Method::GET, axum::http::Method::POST])
        .allow_headers([axum::http::header::CONTENT_TYPE]);

    Router::new()
        .route("/health", get(routes::health))
        .route("/health/audit", get(routes::health_audit))
        .route("/ingest", post(routes::ingest))
        .route("/ingest/file", post(routes::ingest_multipart))
        .route("/note", post(routes::note))
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
    pub telegram_notifier: SubsystemStatus,
    pub telegram_bot: SubsystemStatus,
    pub discord: SubsystemStatus,
    pub ntfy: SubsystemStatus,
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
    pub async fn wait(mut self) -> Result<()> {
        while let Some(result) = self.tasks.join_next().await {
            match result {
                Ok(Ok(())) => log::info!("a daemon task exited cleanly"),
                Ok(Err(e)) => log::error!("a daemon task failed: {e:#}"),
                Err(e) => {
                    if e.is_panic() {
                        log::error!("a daemon task panicked: {e}");
                    } else {
                        log::error!("a daemon task was cancelled: {e}");
                    }
                }
            }
        }
        Ok(())
    }
}

/// Boot every borg subsystem (HTTP server, telegram, discord, ntfy, watchdog)
/// and return a startup snapshot plus an opaque handle the caller awaits.
pub async fn serve_init(config: Config) -> Result<(ServerStartup, ServerHandle)> {
    log::info!("Starting obsidian-borg daemon");

    let addr: SocketAddr = format!("{}:{}", config.server.host, config.server.port)
        .parse()
        .context("Invalid server address")?;

    log::info!("Server address: {addr}");
    log::debug!("Vault inbox: {}", config.vault.inbox_path);
    log::debug!("Transcriber URL: {}", config.transcriber.url);
    log::debug!("Groq model: {}", config.groq.model);
    log::debug!("LLM provider: {}, model: {}", config.llm.provider, config.llm.model);

    // Ensure vault system files exist on startup. vault_root must resolve here
    // - the daemon cannot start without one.
    let ledger_p = ledger::ledger_path(&config)?;
    if let Err(e) = ledger::ensure_ledger_exists(&ledger_p) {
        log::warn!("Failed to ensure Borg Ledger exists: {e:#}");
    }
    let dashboard_p = dashboard::dashboard_path(&config)?;
    if let Err(e) = dashboard::ensure_dashboard_exists(&dashboard_p) {
        log::warn!("Failed to ensure Borg Dashboard exists: {e:#}");
    }
    let intake_p = intake::intake_path(&config)?;
    if let Err(e) = vault::intake::ensure_intake_exists(&intake_p) {
        log::warn!("Failed to ensure Borg Intake exists: {e:#}");
    }
    let dlq_p = intake::dlq_path(&config)?;
    if let Err(e) = vault::dlq::ensure_dlq_exists(&dlq_p) {
        log::warn!("Failed to ensure Borg DLQ exists: {e:#}");
    }

    let config = Arc::new(config);
    let mut tasks = tokio::task::JoinSet::new();

    // Build the shared Telegram notifier (if configured)
    let mut notifier: Option<Notifier> = None;
    let mut resolved_tg_token: Option<String> = None;
    let mut telegram_notifier_status = SubsystemStatus::Disabled;

    if let Some(tg_config) = &config.telegram {
        match config::resolve_secret(&tg_config.bot_token) {
            Ok(token) => {
                notifier = Notifier::new(&token, tg_config);
                resolved_tg_token = Some(token);
                telegram_notifier_status = if notifier.is_some() {
                    SubsystemStatus::Active
                } else {
                    SubsystemStatus::Disabled
                };
            }
            Err(e) => {
                log::warn!("Telegram configured but token not available: {e:#}");
                telegram_notifier_status = SubsystemStatus::SkippedNoToken;
            }
        }
    }

    // HTTP server (always runs)
    let state = AppState {
        config: config.clone(),
        notifier: notifier.clone(),
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
            let tg = tg_config.clone();
            let cfg = config.clone();
            let tg_notifier = notifier.clone();
            tasks.spawn(async move { telegram::run(token, tg, cfg, tg_notifier).await });
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
                    tasks.spawn(async move { discord::run(token, dc, cfg).await });
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
            let ntfy_notifier = notifier.clone();
            let topic_for_status = ntfy_config.topic.clone();
            tasks.spawn(async move { ntfy::run(server, topic, token, cfg, ntfy_notifier).await });
            ntfy_status = SubsystemStatus::ActiveWithDetail(format!("topic: {topic_for_status}"));
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
            telegram_notifier: telegram_notifier_status,
            telegram_bot: telegram_bot_status,
            discord: discord_status,
            ntfy: ntfy_status,
            watchdog: SubsystemStatus::Active,
        },
        ServerHandle { tasks },
    ))
}

/// Thin wrapper preserved for internal callers (daemon::run with --start).
/// New sb code paths should use `serve_init` + `ServerHandle::wait` to get
/// the typed startup banner instead.
pub async fn serve(config: Config, _verbose: bool) -> Result<()> {
    let (_startup, handle) = serve_init(config).await?;
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
    intake::record_intake_with_sidecar(
        &config,
        types::IngestMethod::Cli,
        "cli",
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
    intake::record_intake(
        &config,
        types::IngestMethod::Cli,
        "cli",
        intake_kind,
        &preview,
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
        intake::record_dlq(
            &config,
            types::IngestMethod::Cli,
            intake::Stage::IntakeReject,
            &reason,
            &preview,
            &trace_id,
            None,
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

    let ledger_file = ledger::ledger_path(&config)?;

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
        let status = match client.post(&endpoint).json(&body).send().await {
            Ok(response) => {
                let result: types::IngestResult =
                    response.json().await.context("Failed to parse response from daemon")?;
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
    notify: bool,
    method: types::IngestMethod,
) -> Result<IngestOutcome> {
    let host = &config.hotkey.host;
    let port = config.hotkey.port;
    let endpoint = format!("http://{host}:{port}/ingest");

    if notify {
        send_notification("Ingesting...", &url);
    }

    let body = serde_json::json!({
        "url": url,
        "tags": tags.unwrap_or_default(),
        "force": force,
        "method": method,
    });

    let client = reqwest::Client::new();
    let response = client.post(&endpoint).json(&body).send().await.map_err(|e| {
        let msg = if e.is_connect() {
            format!("cannot reach obsidian-borg at http://{host}:{port} - is the daemon running?")
        } else {
            format!("{e}")
        };
        if notify {
            send_notification("Error", &msg);
        }
        eyre::eyre!("{msg}")
    })?;

    let result: types::IngestResult = response.json().await.context("Failed to parse response from daemon")?;

    Ok(match result.status {
        types::IngestStatus::Completed => {
            let title = result.title.unwrap_or_else(|| "Untitled".to_string());
            let path = result.note_path.unwrap_or_else(|| "unknown".to_string());
            if notify {
                send_notification("Saved", &title);
            }
            IngestOutcome::Captured { title, path }
        }
        types::IngestStatus::Duplicate { original_date } => {
            if notify {
                send_notification("Duplicate", &format!("Already ingested on {original_date}"));
            }
            IngestOutcome::Duplicate { original_date }
        }
        types::IngestStatus::Failed { reason } => {
            if notify {
                send_notification("Failed", &reason);
            }
            IngestOutcome::Failed { reason }
        }
        types::IngestStatus::Queued => {
            if notify {
                send_notification("Queued", &url);
            }
            IngestOutcome::Queued
        }
    })
}

fn send_notification(summary: &str, body: &str) {
    let _ = notify_rust::Notification::new()
        .appname("borg")
        .summary(&format!("obsidian-borg: {summary}"))
        .body(body)
        .timeout(notify_rust::Timeout::Milliseconds(5000))
        .show();
}

fn generate_manifest(config: &config::Config) -> serde_json::Value {
    let version = env!("CARGO_PKG_VERSION");

    // Build host_permissions and connect-src list from server config
    let mut host_permissions = vec![serde_json::json!("http://localhost/*")];
    let mut connect_sources = vec!["http://localhost:*".to_string()];
    let host = &config.server.host;
    if host != "localhost" && host != "127.0.0.1" && host != "0.0.0.0" {
        host_permissions.push(serde_json::json!(format!("http://{host}/*")));
        connect_sources.push(format!("http://{host}:*"));
        if host.ends_with(".lan") {
            host_permissions.push(serde_json::json!("http://*.lan/*"));
            connect_sources.push("http://*.lan:*".to_string());
        }
    }
    let hotkey_host = &config.hotkey.host;
    if hotkey_host != "localhost" && hotkey_host != "127.0.0.1" && hotkey_host != host {
        host_permissions.push(serde_json::json!(format!("http://{hotkey_host}/*")));
        connect_sources.push(format!("http://{hotkey_host}:*"));
        if hotkey_host.ends_with(".lan") && !host.ends_with(".lan") {
            host_permissions.push(serde_json::json!("http://*.lan/*"));
            connect_sources.push("http://*.lan:*".to_string());
        }
    }

    // CSP that explicitly allows HTTP connect to configured hosts (prevents HTTPS upgrade)
    let csp = format!("default-src 'self'; connect-src {}", connect_sources.join(" "));

    serde_json::json!({
        "manifest_version": 3,
        "name": "obsidian-borg Capture",
        "description": "Send the current tab URL to obsidian-borg for ingestion",
        "version": version,
        "icons": {
            "16": "icons/locutus-16.png",
            "48": "icons/locutus-48.png",
            "128": "icons/locutus-128.png"
        },
        "action": {
            "default_icon": {
                "16": "icons/locutus-16.png",
                "48": "icons/locutus-48.png",
                "128": "icons/locutus-128.png"
            }
        },
        "background": {
            "scripts": ["background.js"],
            "service_worker": "background.js"
        },
        "permissions": ["activeTab", "storage", "notifications"],
        "host_permissions": host_permissions,
        "content_security_policy": {
            "extension_pages": csp
        },
        "commands": {
            "capture-url": {
                "description": "Capture current tab URL",
                "suggested_key": {
                    "default": "Alt+Shift+B"
                }
            }
        },
        "options_ui": {
            "page": "options.html",
            "open_in_tab": false
        },
        "browser_specific_settings": {
            "gecko": {
                "id": "obsidian-borg@scottidler",
                "strict_min_version": "140.0",
                "data_collection_permissions": {
                    "required": ["none"],
                    "optional": []
                }
            }
        }
    })
}

/// Outcome of `borg::sign`. sb prints "Signing extension v{version} in {dir}"
/// and "Extension signed successfully. Check {dir}/web-ext-artifacts/" from
/// these fields.
#[derive(Debug)]
pub struct SignResult {
    pub extension_dir: PathBuf,
    pub version: String,
}

pub async fn sign(config: &config::Config) -> Result<SignResult> {
    let repo_root = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("Failed to run git")?;
    if !repo_root.status.success() {
        eyre::bail!("Not inside a git repository - cannot locate extension directory");
    }
    let root = PathBuf::from(String::from_utf8_lossy(&repo_root.stdout).trim().to_string());
    let extension_dir = root.join("clients/extension");
    if !extension_dir.exists() {
        eyre::bail!("Extension directory not found at {}", extension_dir.display());
    }

    // Generate manifest.json from config
    let manifest = generate_manifest(config);
    let manifest_path = extension_dir.join("manifest.json");
    std::fs::write(&manifest_path, serde_json::to_string_pretty(&manifest)? + "\n")
        .context("Failed to write manifest.json")?;

    let cargo_version = env!("CARGO_PKG_VERSION");
    let jwt_issuer =
        std::env::var("MOZILLA_JWT_ISSUER").context("MOZILLA_JWT_ISSUER env var must be set (AMO API key)")?;
    let jwt_secret =
        std::env::var("MOZILLA_JWT_SECRET").context("MOZILLA_JWT_SECRET env var must be set (AMO API secret)")?;

    log::info!("signing extension v{cargo_version} in {}", extension_dir.display());

    let status = std::process::Command::new("web-ext")
        .args([
            "sign",
            "--api-key",
            &jwt_issuer,
            "--api-secret",
            &jwt_secret,
            "--channel",
            "unlisted",
            "--ignore-files",
            "sign.sh",
        ])
        .current_dir(&extension_dir)
        .status()
        .context("Failed to run web-ext - is it installed?")?;

    if !status.success() {
        eyre::bail!("web-ext sign failed");
    }

    Ok(SignResult {
        extension_dir,
        version: cargo_version.to_string(),
    })
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
        let post_install = install_hotkey(&host, port, &key).await?;
        Ok(HotkeyOutcome::Installed {
            key,
            host,
            port,
            post_install,
        })
    } else if opts.uninstall {
        uninstall_hotkey().await?;
        Ok(HotkeyOutcome::Uninstalled)
    } else {
        Ok(HotkeyOutcome::NoAction)
    }
}

/// Outcome of a `sb borg daemon <flag>` invocation (everything except
/// `--start`, which sb routes to `serve_init`). Variants carry the typed
/// data sb needs to format the user-facing message; no pre-rendered text
/// crosses the lib boundary. `Status` carries the raw systemctl-status
/// blob because systemd's output is not contract-stable across versions;
/// parsing structured fields out of it would be brittle scope-creep
/// (per 2026-05-20 architect consensus).
#[derive(Debug)]
pub enum DaemonOutcome {
    Installed { unit_path: PathBuf },
    Uninstalled { unit_path: PathBuf },
    NotInstalled { unit_path: PathBuf },
    Reinstalled { unit_path: PathBuf },
    Stopped,
    Restarted,
    Status { raw_output: String },
    NoAction,
}

/// Internal: outcome of an uninstall attempt. `was_present = false` means
/// the unit file was already absent (no-op); `true` means a file was
/// removed.
struct UninstallOutcome {
    unit_path: PathBuf,
    was_present: bool,
}

/// Dispatch the non-start daemon flags (install/uninstall/reinstall/stop/restart/status).
/// `--start` is handled separately by sb via `serve_init` + `ServerHandle::wait` so the
/// startup banner can be formatted from typed data.
pub async fn daemon(_config: Config, _verbose: bool, opts: opts::DaemonOpts) -> Result<DaemonOutcome> {
    use crate::opts::DaemonOpts;

    match opts {
        DaemonOpts { install: true, .. } => Ok(DaemonOutcome::Installed {
            unit_path: install_service().await?,
        }),
        DaemonOpts { uninstall: true, .. } => {
            let outcome = uninstall_service().await?;
            if outcome.was_present {
                Ok(DaemonOutcome::Uninstalled {
                    unit_path: outcome.unit_path,
                })
            } else {
                Ok(DaemonOutcome::NotInstalled {
                    unit_path: outcome.unit_path,
                })
            }
        }
        DaemonOpts { reinstall: true, .. } => {
            let _ = uninstall_service().await;
            Ok(DaemonOutcome::Reinstalled {
                unit_path: install_service().await?,
            })
        }
        DaemonOpts { stop: true, .. } => {
            stop_service().await?;
            Ok(DaemonOutcome::Stopped)
        }
        DaemonOpts { restart: true, .. } => {
            restart_service().await?;
            Ok(DaemonOutcome::Restarted)
        }
        DaemonOpts { status: true, .. } => Ok(DaemonOutcome::Status {
            raw_output: show_status().await?,
        }),
        DaemonOpts { start: true, .. } => Err(eyre::eyre!(
            "borg::daemon: --start should be dispatched by sb via serve_init"
        )),
        _ => Ok(DaemonOutcome::NoAction),
    }
}

async fn install_service() -> Result<PathBuf> {
    let exe_path = std::env::current_exe().context("Failed to detect binary path")?;
    let exe = exe_path.display().to_string();

    if cfg!(target_os = "linux") {
        install_systemd(&exe).await
    } else if cfg!(target_os = "macos") {
        install_launchd(&exe).await
    } else {
        eyre::bail!("Unsupported platform for service install")
    }
}

async fn uninstall_service() -> Result<UninstallOutcome> {
    if cfg!(target_os = "linux") {
        uninstall_systemd().await
    } else if cfg!(target_os = "macos") {
        uninstall_launchd().await
    } else {
        eyre::bail!("Unsupported platform for service uninstall")
    }
}

async fn stop_service() -> Result<()> {
    if cfg!(target_os = "linux") {
        systemctl(&["stop", "borg"]).await?;
    } else if cfg!(target_os = "macos") {
        launchctl(&["stop", "com.borg"]).await?;
    } else {
        eyre::bail!("Unsupported platform for service stop")
    }
    Ok(())
}

async fn restart_service() -> Result<()> {
    if cfg!(target_os = "linux") {
        systemctl(&["restart", "borg"]).await?;
    } else if cfg!(target_os = "macos") {
        launchctl(&["stop", "com.borg"]).await.ok();
        launchctl(&["start", "com.borg"]).await?;
    } else {
        eyre::bail!("Unsupported platform for service restart")
    }
    Ok(())
}

async fn show_status() -> Result<String> {
    if cfg!(target_os = "linux") {
        let output = tokio::process::Command::new("systemctl")
            .args(["--user", "status", "borg"])
            .output()
            .await
            .context("Failed to run systemctl")?;
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else if cfg!(target_os = "macos") {
        let output = tokio::process::Command::new("launchctl")
            .args(["list", "com.borg"])
            .output()
            .await
            .context("Failed to run launchctl")?;
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        eyre::bail!("Unsupported platform for service status")
    }
}

/// Run `systemctl --user <args>` and return Ok if it succeeds.
async fn systemctl(args: &[&str]) -> Result<()> {
    let mut cmd_args = vec!["--user"];
    cmd_args.extend(args);
    let output = tokio::process::Command::new("systemctl")
        .args(&cmd_args)
        .output()
        .await
        .context("Failed to run systemctl")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eyre::bail!("systemctl --user {} failed: {stderr}", args.join(" "));
    }
    Ok(())
}

/// Run `launchctl <args>` and return Ok if it succeeds.
async fn launchctl(args: &[&str]) -> Result<()> {
    let output = tokio::process::Command::new("launchctl")
        .args(args)
        .output()
        .await
        .context("Failed to run launchctl")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eyre::bail!("launchctl {} failed: {stderr}", args.join(" "));
    }
    Ok(())
}

async fn install_systemd(exe_path: &str) -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| eyre::eyre!("Cannot determine home directory"))?;
    let unit_dir = home.join(".config/systemd/user");
    let unit_path = unit_dir.join("borg.service");

    let vault_path = home.join("repos/scottidler/obsidian");
    let secrets_path = home.join("repos/scottidler/secrets/.secrets");
    let manifest_bin = home.join(".cargo/bin/manifest");
    let uid = std::process::Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "1000".to_string());
    let env_file = format!("/run/user/{}/borg.env", uid);
    let unit_content = format!(
        r#"[Unit]
Description=borg - Obsidian ingestion daemon (second-brain)
After=network-online.target
Wants=network-online.target
StartLimitBurst=5
StartLimitIntervalSec=60

[Service]
Type=simple
ExecStartPre=/bin/sh -c '{manifest} age decrypt {secrets} -f env > {env_file}'
EnvironmentFile=-{env_file}
Environment="PATH={home}/.local/bin:{home}/.cargo/bin:{home}/go/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
ExecStart={exe_path} borg --log-level debug daemon --start
Restart=always
RestartSec=5
WorkingDirectory={home}

# Hardening
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=read-only
ReadWritePaths={vault}
PrivateTmp=true

[Install]
WantedBy=default.target
"#,
        home = home.display(),
        vault = vault_path.display(),
        manifest = manifest_bin.display(),
        secrets = secrets_path.display(),
        env_file = env_file,
    );

    // Stop the running service if active (ignore errors - may not be running)
    systemctl(&["stop", "borg"]).await.ok();

    // Write (or overwrite) the unit file
    std::fs::create_dir_all(&unit_dir).context("Failed to create systemd user unit directory")?;
    std::fs::write(&unit_path, &unit_content).context("Failed to write systemd unit file")?;

    // Reload so systemd picks up changes, then enable + start
    systemctl(&["daemon-reload"]).await?;
    systemctl(&["enable", "--now", "borg"]).await?;

    Ok(unit_path)
}

async fn install_launchd(exe_path: &str) -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| eyre::eyre!("Cannot determine home directory"))?;
    let plist_dir = home.join("Library/LaunchAgents");
    let plist_path = plist_dir.join("com.obsidian-borg.plist");

    let plist_content = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.obsidian-borg</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe_path}</string>
        <string>daemon</string>
        <string>--start</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/tmp/obsidian-borg.stdout.log</string>
    <key>StandardErrorPath</key>
    <string>/tmp/obsidian-borg.stderr.log</string>
</dict>
</plist>
"#
    );

    // Unload if already loaded (ignore errors - may not be loaded)
    launchctl(&["unload", &plist_path.to_string_lossy()]).await.ok();

    std::fs::create_dir_all(&plist_dir).context("Failed to create LaunchAgents directory")?;
    std::fs::write(&plist_path, &plist_content).context("Failed to write plist file")?;

    launchctl(&["load", &plist_path.to_string_lossy()]).await?;
    Ok(plist_path)
}

async fn uninstall_systemd() -> Result<UninstallOutcome> {
    let home = dirs::home_dir().ok_or_else(|| eyre::eyre!("Cannot determine home directory"))?;
    let unit_path = home.join(".config/systemd/user/borg.service");

    if !unit_path.exists() {
        return Ok(UninstallOutcome {
            unit_path,
            was_present: false,
        });
    }

    let _ = std::process::Command::new("systemctl")
        .args(["--user", "disable", "--now", "borg"])
        .status();

    std::fs::remove_file(&unit_path).context("Failed to remove unit file")?;

    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();

    Ok(UninstallOutcome {
        unit_path,
        was_present: true,
    })
}

async fn uninstall_launchd() -> Result<UninstallOutcome> {
    let home = dirs::home_dir().ok_or_else(|| eyre::eyre!("Cannot determine home directory"))?;
    let plist_path = home.join("Library/LaunchAgents/com.obsidian-borg.plist");

    if !plist_path.exists() {
        return Ok(UninstallOutcome {
            unit_path: plist_path,
            was_present: false,
        });
    }

    let _ = std::process::Command::new("launchctl")
        .args(["unload", &plist_path.to_string_lossy()])
        .status();

    std::fs::remove_file(&plist_path).context("Failed to remove plist file")?;
    Ok(UninstallOutcome {
        unit_path: plist_path,
        was_present: true,
    })
}

const GNOME_KEYBINDINGS_SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys";
const GNOME_KEYBINDING_PATH: &str = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/obsidian-borg/";

/// Install (best-effort) the keyboard shortcut and return a non-Linux
/// fallback message when applicable. On Linux, returns None - sb prints
/// the standard installed banner.
async fn install_hotkey(host: &str, port: u16, key: &str) -> Result<Option<String>> {
    let _ = (host, port);
    let exe_path = std::env::current_exe().context("Failed to detect binary path")?;
    let command = format!("{} ingest --clipboard", exe_path.display());

    if cfg!(target_os = "linux") {
        install_gnome_keybinding(&command, key)?;
        Ok(None)
    } else {
        Ok(Some(format!(
            "Bind this command to {key} in your OS settings:\n  {command}"
        )))
    }
}

fn install_gnome_keybinding(command: &str, key: &str) -> Result<()> {
    let _ = command;
    // Get current custom keybinding paths
    let output = std::process::Command::new("gsettings")
        .args(["get", GNOME_KEYBINDINGS_SCHEMA, "custom-keybindings"])
        .output()
        .context("Failed to run gsettings — is GNOME available?")?;

    let current = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // Parse current list and add our path if not present
    let new_list = if current == "@as []" || current.is_empty() {
        format!("['{}']", GNOME_KEYBINDING_PATH)
    } else if current.contains(GNOME_KEYBINDING_PATH) {
        current.clone()
    } else {
        // Insert before closing bracket
        let trimmed = current.trim_end_matches(']').trim_end_matches(", ");
        format!("{}, '{}']", trimmed, GNOME_KEYBINDING_PATH)
    };

    // Update the list
    std::process::Command::new("gsettings")
        .args(["set", GNOME_KEYBINDINGS_SCHEMA, "custom-keybindings", &new_list])
        .status()
        .context("Failed to update custom-keybindings list")?;

    // Set the keybinding properties
    let schema = format!(
        "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding:{}",
        GNOME_KEYBINDING_PATH
    );

    for (prop, val) in [("name", "borg"), ("command", command), ("binding", key)] {
        let status = std::process::Command::new("gsettings")
            .args(["set", &schema, prop, val])
            .status()
            .context(format!("Failed to set keybinding {prop}"))?;
        if !status.success() {
            eyre::bail!("gsettings set {prop} failed");
        }
    }

    log::info!("registered GNOME keybinding: {key} -> {command}");
    Ok(())
}

async fn uninstall_hotkey() -> Result<()> {
    if cfg!(target_os = "linux") {
        uninstall_gnome_keybinding()?;
    }
    Ok(())
}

fn uninstall_gnome_keybinding() -> Result<()> {
    // Remove our path from the custom keybindings list
    let output = std::process::Command::new("gsettings")
        .args(["get", GNOME_KEYBINDINGS_SCHEMA, "custom-keybindings"])
        .output()
        .context("Failed to run gsettings")?;

    let current = String::from_utf8_lossy(&output.stdout).trim().to_string();

    if current.contains(GNOME_KEYBINDING_PATH) {
        // Remove our entry from the list
        let new_list = current
            .replace(&format!("'{}'", GNOME_KEYBINDING_PATH), "")
            .replace(", ,", ",")
            .replace("[,", "[")
            .replace(",]", "]")
            .replace("[, ", "[")
            .replace(", ]", "]");

        // Normalize empty list
        let new_list = if new_list.trim() == "[]" || new_list.trim() == "[' ']" {
            "@as []".to_string()
        } else {
            new_list
        };

        std::process::Command::new("gsettings")
            .args(["set", GNOME_KEYBINDINGS_SCHEMA, "custom-keybindings", &new_list])
            .status()
            .context("Failed to update custom-keybindings list")?;

        // Reset the keybinding properties
        let schema = format!(
            "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding:{}",
            GNOME_KEYBINDING_PATH
        );

        for prop in &["name", "command", "binding"] {
            let _ = std::process::Command::new("gsettings")
                .args(["reset", &schema, prop])
                .status();
        }

        log::info!("removed GNOME keybinding");
    } else {
        log::info!("no GNOME keybinding found to remove");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn test_router() -> Router {
        build_router(AppState {
            config: Arc::new(Config::default()),
            notifier: None,
        })
    }

    #[tokio::test]
    async fn test_health_endpoint() {
        let app = test_router();
        let req = Request::builder().uri("/health").body(Body::empty()).expect("request");
        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_ingest_endpoint() {
        let app = test_router();
        let body = serde_json::json!({"url": "https://youtube.com/watch?v=test"});
        let req = Request::builder()
            .method("POST")
            .uri("/ingest")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).expect("json")))
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_cors_preflight() {
        let app = test_router();
        let req = Request::builder()
            .method("OPTIONS")
            .uri("/ingest")
            .header("origin", "https://example.com")
            .header("access-control-request-method", "POST")
            .header("access-control-request-headers", "content-type")
            .body(Body::empty())
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers().contains_key("access-control-allow-origin"));
    }

    #[tokio::test]
    async fn test_cors_on_response() {
        let app = test_router();
        let req = Request::builder()
            .uri("/health")
            .header("origin", "https://example.com")
            .body(Body::empty())
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let origin = resp.headers().get("access-control-allow-origin").expect("cors header");
        assert_eq!(origin, "*");
    }

    #[tokio::test]
    async fn test_ingest_connection_refused() {
        // Use a port that's almost certainly not listening
        let config = Config {
            hotkey: config::HotkeyConfig {
                host: "127.0.0.1".to_string(),
                port: 19999,
                ..config::HotkeyConfig::default()
            },
            ..Config::default()
        };
        let result = ingest(
            config,
            "https://example.com".to_string(),
            None,
            false,
            false,
            types::IngestMethod::Cli,
        )
        .await;
        assert!(result.is_err());
        let err = format!("{}", result.expect_err("expected error"));
        assert!(
            err.contains("cannot reach obsidian-borg"),
            "expected connection error message, got: {err}"
        );
    }
}
