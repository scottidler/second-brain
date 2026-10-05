//! Client-side entry points sb's CLI calls: `note` / `ingest_file` run the
//! pipeline in-process; `ingest` / `reingest` go through the daemon's
//! `/ingest` route. Also the clipboard/argument resolvers for their inputs.

use eyre::{Context, Result};

use crate::config::{self, Config};
use crate::{assets, intake, ledger, notify, pipeline, replay, trace, types};

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

/// Whether the ledger entry's note (in `notes/` or `inbox/`) carries a `type:` line
/// containing `type_filter`. An unreadable note is excluded with a WARN naming it.
fn note_has_type(vault_root: &std::path::Path, entry: &ledger::QueriedEntry, type_filter: &str) -> bool {
    if entry.filename == "-" {
        return false;
    }
    let note_path = [
        vault_root.join("notes").join(&entry.filename),
        vault_root.join("inbox").join(&entry.filename),
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
        Err(e) => {
            log::warn!(
                "reingest: excluding {} from --type {type_filter}, note unreadable: {e}",
                note_path.display()
            );
            false
        }
    }
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
    source: Option<String>,
    before: Option<String>,
    after: Option<String>,
    dry_run: bool,
    mut progress: impl FnMut(&ReingestEvent) + Send,
) -> Result<ReingestReport> {
    use ledger::{EntryFilter, QueriedEntry};

    if !all && source.is_none() && content_type.is_none() {
        eyre::bail!("Specify --all, --source <URL>, or --type <TYPE> to select entries");
    }

    let ledger_file = ledger::ledger_path()?;

    let filter = EntryFilter {
        source: source.clone(),
        before,
        after,
    };

    let entries: Vec<QueriedEntry> = ledger::query_entries(&ledger_file, &filter)?;

    let entries: Vec<QueriedEntry> = if let Some(ref type_filter) = content_type {
        let vault_root = config.vault_root()?;
        entries
            .into_iter()
            .filter(|e| note_has_type(&vault_root, e, type_filter))
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

    let daemon = vault::daemon::client::DaemonClient::new(&config.hotkey, config.server.auth_token.as_deref())?;
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

        let body = serde_json::json!({
            "url": entry.source,
            "tags": [],
            "force": true,
            "method": "cli",
        });

        let sent = match daemon.post_json("/ingest", &body).await {
            Ok(response) => daemon.json::<types::IngestResult>("/ingest", response).await,
            Err(e) => Err(e),
        };
        let status = match sent {
            Ok(mut result) => {
                // The daemon answers `Queued`; poll `/trace/{id}` for the real
                // terminal state so reingest reports accurate counts and paces
                // one entry at a time.
                if matches!(result.status, types::IngestStatus::Queued)
                    && let Some(tid) = result.trace_id.clone()
                {
                    result = replay::poll_trace_terminal(&config, &daemon, &tid)
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
                    eyre::bail!("cannot reach the borg daemon at http://{host}:{port} - is the daemon running?");
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

    let body = serde_json::json!({
        "url": url,
        "tags": tags.unwrap_or_default(),
        "force": force,
        "method": method,
    });

    // The client sends the write-route Bearer token when one is configured, so
    // enabling server.auth-token doesn't 401 this first-party CLI path.
    let daemon = vault::daemon::client::DaemonClient::new(&config.hotkey, config.server.auth_token.as_deref())?;
    // The Error toast here is unconditional and load-bearing: when the HTTP
    // POST itself fails (daemon not running), the daemon by definition
    // cannot deliver the failure notification. The CLI may be wired to a
    // desktop hotkey where stderr is not visible. This is the symmetric
    // counterpart to the `fail()` / `catch (err)` path in popup.js.
    let response = daemon.post_json("/ingest", &body).await.map_err(|e| {
        let msg = if e.is_connect() {
            format!("cannot reach the borg daemon at http://{host}:{port} - is the daemon running?")
        } else {
            format!("{e}")
        };
        send_notification("Error", &msg);
        eyre::eyre!("{msg}")
    })?;

    let result: types::IngestResult = daemon.json("/ingest", response).await?;

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
        .summary(&format!("borg: {summary}"))
        .body(body)
        .timeout(notify_rust::Timeout::Milliseconds(HOTKEY_NOTIFY_DISPLAY_MS))
        .show();
}

#[cfg(test)]
mod tests;
