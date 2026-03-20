#![deny(clippy::unwrap_used)]
#![deny(dead_code)]
#![deny(unused_variables)]

pub use vault;

pub mod assets;
pub mod audit;
pub mod backoff;
pub mod cli;
pub mod config;
pub mod dashboard;
pub mod discord;
pub mod error;
pub mod extraction;
pub mod fabric;
pub mod health;
pub mod hygiene;
pub mod jina;
pub mod ledger;
pub mod logging;
pub mod markdown;
pub mod migrate;
pub mod notify;
pub mod ntfy;
pub mod ocr;
pub mod pipeline;
pub mod quality;
pub mod router;
pub mod routes;
pub mod telegram;
pub mod trace;
pub mod transcription;
pub mod types;
pub mod youtube;

use axum::Router;
use axum::routing::{get, post};
use colored::*;
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
        .route("/ingest", post(routes::ingest))
        .route("/ingest/file", post(routes::ingest_multipart))
        .route("/note", post(routes::note))
        .layer(cors)
        .with_state(state)
}

pub async fn run_server(config: Config, _verbose: bool) -> Result<()> {
    log::info!("Starting obsidian-borg daemon");

    let addr: SocketAddr = format!("{}:{}", config.server.host, config.server.port)
        .parse()
        .context("Invalid server address")?;

    log::info!("Server address: {addr}");
    log::debug!("Vault inbox: {}", config.vault.inbox_path);
    log::debug!("Transcriber URL: {}", config.transcriber.url);
    log::debug!("Groq model: {}", config.groq.model);
    log::debug!("LLM provider: {}, model: {}", config.llm.provider, config.llm.model);

    // Ensure vault system files exist on startup
    if let Err(e) = ledger::ensure_ledger_exists(&ledger::ledger_path(&config)) {
        log::warn!("Failed to ensure Borg Ledger exists: {e:#}");
    }
    if let Err(e) = dashboard::ensure_dashboard_exists(&dashboard::dashboard_path(&config)) {
        log::warn!("Failed to ensure Borg Dashboard exists: {e:#}");
    }

    let config = Arc::new(config);
    let mut tasks = tokio::task::JoinSet::new();

    // Build the shared Telegram notifier (if configured)
    let mut notifier: Option<Notifier> = None;
    let mut resolved_tg_token: Option<String> = None;

    if let Some(tg_config) = &config.telegram {
        match config::resolve_secret(&tg_config.bot_token) {
            Ok(token) => {
                notifier = Notifier::new(&token, tg_config);
                resolved_tg_token = Some(token);
                if notifier.is_some() {
                    println!("{} telegram notifier active", "-->".green());
                }
            }
            Err(e) => {
                log::warn!("Telegram configured but token not available: {e:#}");
                eprintln!("{} telegram notifier skipped (token not available)", "-->".yellow());
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
    println!("{} http server on {}", "-->".green(), addr.to_string().cyan());

    // Telegram bot (config-driven, host-gated)
    if let Some(tg_config) = &config.telegram {
        if !config::is_local_host(&tg_config.host) {
            log::info!(
                "Telegram configured but host {:?} does not match this machine, skipping",
                tg_config.host
            );
            eprintln!("{} telegram bot skipped (host mismatch)", "-->".yellow());
        } else if let Some(token) = resolved_tg_token.clone() {
            log::info!(
                "Telegram bot enabled (allowed_chat_ids: {:?})",
                tg_config.allowed_chat_ids
            );
            let tg = tg_config.clone();
            let cfg = config.clone();
            let tg_notifier = notifier.clone();
            tasks.spawn(async move { telegram::run(token, tg, cfg, tg_notifier).await });
            println!("{} telegram bot active", "-->".green());
        } else {
            eprintln!("{} telegram bot skipped (token not available)", "-->".yellow());
        }
    }

    // Discord bot (config-driven, host-gated)
    if let Some(dc_config) = &config.discord {
        if !config::is_local_host(&dc_config.host) {
            log::info!(
                "Discord configured but host {:?} does not match this machine, skipping",
                dc_config.host
            );
            eprintln!("{} discord bot skipped (host mismatch)", "-->".yellow());
        } else {
            match config::resolve_secret(&dc_config.bot_token) {
                Ok(token) => {
                    log::info!("Discord bot enabled (channel_id: {})", dc_config.channel_id);
                    let dc = dc_config.clone();
                    let cfg = config.clone();
                    tasks.spawn(async move { discord::run(token, dc, cfg).await });
                    println!("{} discord bot active", "-->".green());
                }
                Err(e) => {
                    log::warn!("Discord configured but token not available: {e:#}");
                    eprintln!("{} discord bot skipped (token not available)", "-->".yellow());
                }
            }
        }
    }

    // ntfy subscriber (config-driven, host-gated)
    if let Some(ntfy_config) = &config.ntfy {
        if !config::is_local_host(&ntfy_config.host) {
            log::info!(
                "ntfy configured but host {:?} does not match this machine, skipping",
                ntfy_config.host
            );
            eprintln!("{} ntfy subscriber skipped (host mismatch)", "-->".yellow());
        } else {
            let server = ntfy_config.server.clone();
            let topic = ntfy_config.topic.clone();
            let token = ntfy_config.token.as_ref().and_then(|t| config::resolve_secret(t).ok());
            let cfg = config.clone();
            let ntfy_notifier = notifier.clone();
            tasks.spawn(async move { ntfy::run(server, topic, token, cfg, ntfy_notifier).await });
            println!(
                "{} ntfy subscriber active (topic: {})",
                "-->".green(),
                ntfy_config.topic
            );
        }
    }

    // Monitor tasks: log failures but keep the daemon alive as long as HTTP is running
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok(Ok(())) => {
                log::info!("A daemon task exited cleanly");
            }
            Ok(Err(e)) => {
                log::error!("A daemon task failed: {e:#}");
            }
            Err(e) => {
                if e.is_panic() {
                    log::error!("A daemon task panicked: {e}");
                } else {
                    log::error!("A daemon task was cancelled: {e}");
                }
            }
        }
    }

    Ok(())
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

pub async fn run_note(config: Config, text: String, tags: Option<Vec<String>>) -> Result<()> {
    let content = types::ContentKind::Text(text);
    let result = pipeline::process_content(
        content,
        tags.unwrap_or_default(),
        types::IngestMethod::Cli,
        false,
        &config,
        None,
    )
    .await;

    match &result.status {
        types::IngestStatus::Completed => {
            let title = result.title.as_deref().unwrap_or("Untitled");
            let path = result.note_path.as_deref().unwrap_or("unknown");
            println!("Captured: \"{title}\" -> {path}");
        }
        types::IngestStatus::Failed { reason } => {
            eprintln!("Error: {reason}");
            std::process::exit(1);
        }
        _ => {}
    }

    Ok(())
}

pub async fn run_file_ingest(
    config: Config,
    file_path: std::path::PathBuf,
    tags: Option<Vec<String>>,
    force: bool,
) -> Result<()> {
    let filename = file_path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let data = std::fs::read(&file_path).context(format!("Failed to read file: {}", file_path.display()))?;

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
        eyre::bail!(
            "Unsupported file type: {}. Supported extensions: {}",
            filename,
            all_extensions.join(", ")
        );
    };

    let result = pipeline::process_content(
        content,
        tags.unwrap_or_default(),
        types::IngestMethod::Cli,
        force,
        &config,
        None,
    )
    .await;

    match &result.status {
        types::IngestStatus::Completed => {
            let title = result.title.as_deref().unwrap_or("Untitled");
            let path = result.note_path.as_deref().unwrap_or("unknown");
            println!("Captured: \"{title}\" -> {path}");
        }
        types::IngestStatus::Failed { reason } => {
            eprintln!("Error: {reason}");
            std::process::exit(1);
        }
        _ => {}
    }

    Ok(())
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

pub async fn run_reingest(
    config: Config,
    all: bool,
    content_type: Option<String>,
    domain: Option<String>,
    source: Option<String>,
    before: Option<String>,
    after: Option<String>,
    dry_run: bool,
) -> Result<()> {
    use ledger::{EntryFilter, QueriedEntry};

    if !all && source.is_none() && content_type.is_none() && domain.is_none() {
        eyre::bail!("Specify --all, --source <URL>, --type <TYPE>, or --domain <DOMAIN> to select entries");
    }

    let ledger_file = ledger::ledger_path(&config);

    // Content type filtering requires reading the vault note to check the type: field.
    // For --type, we read each note's frontmatter after the ledger query.
    let filter = EntryFilter {
        source: source.clone(),
        domain: domain.clone(),
        before,
        after,
    };

    let entries: Vec<QueriedEntry> = ledger::query_entries(&ledger_file, &filter)?;

    // If --type filter is specified, filter by reading each note's frontmatter
    let entries: Vec<QueriedEntry> = if let Some(ref type_filter) = content_type {
        let vault_root = pipeline::expand_vault_root(&config.vault.root_path);
        entries
            .into_iter()
            .filter(|e| {
                if e.path == "-" {
                    return false;
                }
                let note_path = vault_root.join(&e.path);
                match std::fs::read_to_string(&note_path) {
                    Ok(content) => {
                        // Quick frontmatter type: field check
                        content
                            .lines()
                            .any(|l| l.trim().starts_with("type:") && l.contains(type_filter))
                    }
                    Err(_) => false,
                }
            })
            .collect()
    } else {
        entries
    };

    if entries.is_empty() {
        println!("No matching entries found.");
        return Ok(());
    }

    println!(
        "{} {} entries{}",
        if dry_run { "Would reingest" } else { "Reingesting" },
        entries.len(),
        if dry_run { " (dry run)" } else { "" }
    );

    for (i, entry) in entries.iter().enumerate() {
        println!(
            "  [{}/{}] {} - {} ({})",
            i + 1,
            entries.len(),
            entry.date,
            entry.title,
            entry.source
        );

        if dry_run {
            continue;
        }

        // Reingest by calling the daemon's ingest endpoint (same as CLI ingest)
        let host = &config.hotkey.host;
        let port = config.hotkey.port;
        let endpoint = format!("http://{host}:{port}/ingest");

        let body = serde_json::json!({
            "url": entry.source,
            "tags": [],
            "force": false,
            "method": "cli",
        });

        let client = reqwest::Client::new();
        match client.post(&endpoint).json(&body).send().await {
            Ok(response) => {
                let result: types::IngestResult =
                    response.json().await.context("Failed to parse response from daemon")?;
                match &result.status {
                    types::IngestStatus::Completed => {
                        let title = result.title.as_deref().unwrap_or("Untitled");
                        println!("    -> Replaced: \"{title}\"");
                    }
                    types::IngestStatus::Failed { reason } => {
                        eprintln!("    -> Failed: {reason}");
                    }
                    other => {
                        println!("    -> {:?}", other);
                    }
                }
            }
            Err(e) => {
                if e.is_connect() {
                    eyre::bail!("Cannot reach obsidian-borg at http://{host}:{port} - is the daemon running?");
                }
                eprintln!("    -> Error: {e}");
            }
        }
    }

    if !dry_run {
        println!("Reingest complete.");
    }

    Ok(())
}

pub async fn run_ingest(
    config: Config,
    url: String,
    tags: Option<Vec<String>>,
    force: bool,
    notify: bool,
    method: types::IngestMethod,
) -> Result<()> {
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
            format!("cannot reach obsidian-borg at http://{host}:{port} — is the daemon running?")
        } else {
            format!("{e}")
        };
        if notify {
            send_notification("Error", &msg);
        }
        eyre::eyre!("{msg}")
    })?;

    let result: types::IngestResult = response.json().await.context("Failed to parse response from daemon")?;

    match &result.status {
        types::IngestStatus::Completed => {
            let title = result.title.as_deref().unwrap_or("Untitled");
            let path = result.note_path.as_deref().unwrap_or("unknown");
            println!("Captured: \"{title}\" -> {path}");
            if notify {
                send_notification("Saved", title);
            }
        }
        types::IngestStatus::Duplicate { original_date } => {
            println!("Duplicate: already ingested on {original_date}");
            if notify {
                send_notification("Duplicate", &format!("Already ingested on {original_date}"));
            }
        }
        types::IngestStatus::Failed { reason } => {
            if notify {
                send_notification("Failed", reason);
            }
            eprintln!("Error: {reason}");
            std::process::exit(1);
        }
        types::IngestStatus::Queued => {
            println!("Queued for processing.");
            if notify {
                send_notification("Queued", &url);
            }
        }
    }

    Ok(())
}

fn send_notification(summary: &str, body: &str) {
    let _ = notify_rust::Notification::new()
        .appname("obsidian-borg")
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

pub async fn run_sign(config: &config::Config) -> Result<()> {
    let repo_root = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("Failed to run git")?;
    if !repo_root.status.success() {
        eyre::bail!("Not inside a git repository — cannot locate extension directory");
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

    println!("Signing extension v{cargo_version} in {}", extension_dir.display());

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
        .context("Failed to run web-ext — is it installed?")?;

    if !status.success() {
        eyre::bail!("web-ext sign failed");
    }

    println!(
        "Extension signed successfully. Check {}/web-ext-artifacts/",
        extension_dir.display()
    );
    Ok(())
}

pub async fn run_hotkey(opts: cli::HotkeyOpts, config: &Config) -> Result<()> {
    // CLI args override config; if CLI has default values, fall back to config
    let host = if opts.host == "localhost" { config.hotkey.host.clone() } else { opts.host };
    let port = if opts.port == 8181 { config.hotkey.port } else { opts.port };
    let key = if opts.key == "<Ctrl><Shift>b" { config.hotkey.key.clone() } else { opts.key };

    if opts.install {
        install_hotkey(&host, port, &key).await
    } else if opts.uninstall {
        uninstall_hotkey().await
    } else {
        eprintln!("No hotkey action specified. See: obsidian-borg hotkey --help");
        Ok(())
    }
}

pub async fn run_daemon(config: Config, verbose: bool, opts: cli::DaemonOpts) -> Result<()> {
    use cli::DaemonOpts;

    match opts {
        DaemonOpts { install: true, .. } => install_service().await,
        DaemonOpts { uninstall: true, .. } => uninstall_service().await,
        DaemonOpts { reinstall: true, .. } => {
            uninstall_service().await.ok();
            install_service().await
        }
        DaemonOpts { start: true, .. } => run_server(config, verbose).await,
        DaemonOpts { stop: true, .. } => stop_service().await,
        DaemonOpts { restart: true, .. } => restart_service().await,
        DaemonOpts { status: true, .. } => show_status().await,
        _ => {
            eprintln!("No daemon action specified. See: obsidian-borg daemon --help");
            Ok(())
        }
    }
}

async fn install_service() -> Result<()> {
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

async fn uninstall_service() -> Result<()> {
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
        systemctl(&["stop", "obsidian-borg"]).await?;
        println!("Stopped obsidian-borg service");
    } else if cfg!(target_os = "macos") {
        launchctl(&["stop", "com.obsidian-borg"]).await?;
        println!("Stopped obsidian-borg service");
    } else {
        eyre::bail!("Unsupported platform for service stop")
    }
    Ok(())
}

async fn restart_service() -> Result<()> {
    if cfg!(target_os = "linux") {
        systemctl(&["restart", "obsidian-borg"]).await?;
        println!("Restarted obsidian-borg service");
    } else if cfg!(target_os = "macos") {
        launchctl(&["stop", "com.obsidian-borg"]).await.ok();
        launchctl(&["start", "com.obsidian-borg"]).await?;
        println!("Restarted obsidian-borg service");
    } else {
        eyre::bail!("Unsupported platform for service restart")
    }
    Ok(())
}

async fn show_status() -> Result<()> {
    if cfg!(target_os = "linux") {
        let output = tokio::process::Command::new("systemctl")
            .args(["--user", "status", "obsidian-borg"])
            .output()
            .await
            .context("Failed to run systemctl")?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        println!("{stdout}");
    } else if cfg!(target_os = "macos") {
        let output = tokio::process::Command::new("launchctl")
            .args(["list", "com.obsidian-borg"])
            .output()
            .await
            .context("Failed to run launchctl")?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        println!("{stdout}");
    } else {
        eyre::bail!("Unsupported platform for service status")
    }
    Ok(())
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

async fn install_systemd(exe_path: &str) -> Result<()> {
    let home = dirs::home_dir().ok_or_else(|| eyre::eyre!("Cannot determine home directory"))?;
    let unit_dir = home.join(".config/systemd/user");
    let unit_path = unit_dir.join("obsidian-borg.service");

    let vault_path = home.join("repos/scottidler/obsidian");
    let secrets_path = home.join(".../.secrets");
    let manifest_bin = home.join(".cargo/bin/manifest");
    let uid = std::process::Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "1000".to_string());
    let env_file = format!("/run/user/{}/obsidian-borg.env", uid);
    let unit_content = format!(
        r#"[Unit]
Description=obsidian-borg - Obsidian ingestion daemon
After=network-online.target
Wants=network-online.target
StartLimitBurst=5
StartLimitIntervalSec=60

[Service]
Type=simple
ExecStartPre=/bin/sh -c '{manifest} age decrypt {secrets} -f env > {env_file}'
EnvironmentFile=-{env_file}
Environment="PATH={home}/.local/bin:{home}/.cargo/bin:{home}/go/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
ExecStart={exe_path} daemon --start
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
    systemctl(&["stop", "obsidian-borg"]).await.ok();

    // Write (or overwrite) the unit file
    std::fs::create_dir_all(&unit_dir).context("Failed to create systemd user unit directory")?;
    std::fs::write(&unit_path, &unit_content).context("Failed to write systemd unit file")?;
    println!("Wrote {}", unit_path.display());

    // Reload so systemd picks up changes, then enable + start
    systemctl(&["daemon-reload"]).await?;
    systemctl(&["enable", "--now", "obsidian-borg"]).await?;

    println!("Service installed and started.");
    show_status().await.ok();
    Ok(())
}

async fn install_launchd(exe_path: &str) -> Result<()> {
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
    println!("Wrote {}", plist_path.display());

    launchctl(&["load", &plist_path.to_string_lossy()]).await?;

    println!("Service installed and started.");
    Ok(())
}

async fn uninstall_systemd() -> Result<()> {
    let home = dirs::home_dir().ok_or_else(|| eyre::eyre!("Cannot determine home directory"))?;
    let unit_path = home.join(".config/systemd/user/obsidian-borg.service");

    if !unit_path.exists() {
        println!("No service file found at {}", unit_path.display());
        return Ok(());
    }

    let _ = std::process::Command::new("systemctl")
        .args(["--user", "disable", "--now", "obsidian-borg"])
        .status();

    std::fs::remove_file(&unit_path).context("Failed to remove unit file")?;
    println!("Removed {}", unit_path.display());

    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();

    println!("Service uninstalled.");
    Ok(())
}

async fn uninstall_launchd() -> Result<()> {
    let home = dirs::home_dir().ok_or_else(|| eyre::eyre!("Cannot determine home directory"))?;
    let plist_path = home.join("Library/LaunchAgents/com.obsidian-borg.plist");

    if !plist_path.exists() {
        println!("No plist found at {}", plist_path.display());
        return Ok(());
    }

    let _ = std::process::Command::new("launchctl")
        .args(["unload", &plist_path.to_string_lossy()])
        .status();

    std::fs::remove_file(&plist_path).context("Failed to remove plist file")?;
    println!("Removed {}", plist_path.display());
    println!("Service uninstalled.");
    Ok(())
}

const GNOME_KEYBINDINGS_SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys";
const GNOME_KEYBINDING_PATH: &str = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/obsidian-borg/";

async fn install_hotkey(host: &str, port: u16, key: &str) -> Result<()> {
    let exe_path = std::env::current_exe().context("Failed to detect binary path")?;
    let command = format!("{} ingest --clipboard", exe_path.display());

    if cfg!(target_os = "linux") {
        install_gnome_keybinding(&command, key)?;
    } else {
        println!("Bind this command to {} in your OS settings:\n  {}", key, command);
        return Ok(());
    }

    println!("Hotkey installed: {key} -> obsidian-borg ingest --clipboard");
    println!("Daemon target: http://{host}:{port}/ingest (from config)");
    Ok(())
}

fn install_gnome_keybinding(command: &str, key: &str) -> Result<()> {
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

    for (prop, val) in [("name", "obsidian-borg"), ("command", command), ("binding", key)] {
        let status = std::process::Command::new("gsettings")
            .args(["set", &schema, prop, val])
            .status()
            .context(format!("Failed to set keybinding {prop}"))?;
        if !status.success() {
            eyre::bail!("gsettings set {prop} failed");
        }
    }

    println!("Registered GNOME keybinding: {key} -> {command}");
    Ok(())
}

async fn uninstall_hotkey() -> Result<()> {
    if cfg!(target_os = "linux") {
        uninstall_gnome_keybinding()?;
    }

    println!("Hotkey uninstalled.");
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

        println!("Removed GNOME keybinding");
    } else {
        println!("No GNOME keybinding found to remove");
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
    async fn test_run_ingest_connection_refused() {
        // Use a port that's almost certainly not listening
        let config = Config {
            hotkey: config::HotkeyConfig {
                host: "127.0.0.1".to_string(),
                port: 19999,
                ..config::HotkeyConfig::default()
            },
            ..Config::default()
        };
        let result = run_ingest(
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
