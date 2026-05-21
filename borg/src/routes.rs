use axum::Json;
use axum::extract::{Multipart, State};

use serde::Deserialize;

use crate::AppState;
use crate::assets;
use crate::health::HealthResponse;
use crate::intake::{self as intake_log, Kind as IntakeKind, Stage as DlqStage};
use crate::pipeline;
use crate::trace;
use crate::types::{ContentKind, IngestMethod, IngestRequest, IngestResult, IngestStatus};

#[derive(Debug, Deserialize)]
pub struct NoteRequest {
    pub text: String,
    pub tags: Option<Vec<String>>,
}

pub async fn health() -> Json<HealthResponse> {
    crate::health::health_handler("obsidian-borg", env!("GIT_DESCRIBE")).await
}

#[derive(serde::Serialize, Default)]
pub struct AuditHealth {
    pub orphan_count: usize,
    pub oldest_orphan_secs: Option<i64>,
    pub intake_rows: usize,
    pub ledger_rows: usize,
    pub dlq_rows: usize,
    pub dlq_pending: usize,
}

/// Live invariant status: how many intake rows currently have no ledger /
/// DLQ resolution, and how old the oldest such row is. Operators can poll
/// this to detect a silent-drop regression without reading the markdown
/// tables.
pub async fn health_audit(State(state): State<AppState>) -> Json<AuditHealth> {
    let stats = crate::triage::audit_health_stats(&state.config).unwrap_or_default();
    Json(stats)
}

pub async fn ingest(State(state): State<AppState>, Json(request): Json<IngestRequest>) -> Json<IngestResult> {
    log::info!("Received ingest request for URL: {}", request.url);

    let tags = request.tags.unwrap_or_default();
    let method = request.method.unwrap_or(IngestMethod::Http);
    let trace_id = trace::generate(method);

    // Durable intake BEFORE any pipeline dispatch. Synchronous so a write
    // failure returns Failed to the caller; everything beyond this point
    // runs on a detached task and is invisible to the client.
    if let Err(e) = intake_log::record_intake(&state.config, method, "http", IntakeKind::Url, &request.url, &trace_id) {
        log::error!("http/ingest: failed to record intake trace={trace_id}: {e:#}");
        return Json(IngestResult {
            status: IngestStatus::Failed {
                reason: format!("borg failed to record intake: {e}"),
            },
            trace_id: Some(trace_id),
            ..Default::default()
        });
    }

    // Spawn the pipeline on a detached task. The HTTP response returns
    // within milliseconds so the client's connection slot frees up. This
    // also protects the pipeline from being cancelled if the client gives
    // up - the previous synchronous-await pattern dropped the pipeline
    // future the moment Firefox recycled its service worker, leaving an
    // intake row with no ledger / DLQ resolution until the watchdog
    // caught it 31 minutes later.
    //
    // Per Design Invariant 1 the processing notifications also run inside
    // the spawn so notification-channel latency cannot couple to the HTTP
    // response time.
    let url = request.url.clone();
    let config = state.config.clone();
    let telegram = state.telegram.clone();
    let desktop = state.desktop.clone();
    let force = request.force;
    let task_trace = trace_id.clone();
    let task_url = url.clone();
    tokio::spawn(async move {
        let prior = if let Some(d) = &desktop {
            d.processing(&task_trace, "Processing...").await
        } else {
            None
        };
        if let Some(t) = &telegram {
            let _ = t.processing(&task_trace, "Processing...", None).await;
        }
        let content = ContentKind::Url(task_url.clone());
        let result = pipeline::process_content(content, tags, method, force, &config, Some(task_trace.clone())).await;
        if let Some(t) = telegram {
            t.result(&result, &task_url, None).await;
        }
        if let Some(d) = desktop {
            d.result(&result, &task_url, prior).await;
        }
        match &result.status {
            IngestStatus::Failed { reason } => {
                log::warn!("Ingest failed for {task_url}: {reason}");
            }
            IngestStatus::Completed => {
                log::info!("Ingest completed for {task_url}");
            }
            IngestStatus::Duplicate { .. } => {
                log::info!("Duplicate URL skipped for {task_url}");
            }
            IngestStatus::Queued => {}
        }
    });

    Json(IngestResult {
        status: IngestStatus::Queued,
        trace_id: Some(trace_id),
        canonical_url: Some(url),
        ..Default::default()
    })
}

pub async fn note(State(state): State<AppState>, Json(request): Json<NoteRequest>) -> Json<IngestResult> {
    log::info!("Received note request: {} chars", request.text.len());

    let trace_id = trace::generate(IngestMethod::Http);
    let display = if request.text.len() > 50 {
        format!("{}...", &request.text[..50])
    } else {
        request.text.clone()
    };

    if let Err(e) = intake_log::record_intake_with_sidecar(
        &state.config,
        IngestMethod::Http,
        "http",
        IntakeKind::Text,
        &intake_log::preview_text(&request.text),
        request.text.as_bytes(),
        &trace_id,
    ) {
        log::error!("http/note: failed to record intake trace={trace_id}: {e:#}");
        return Json(IngestResult {
            status: IngestStatus::Failed {
                reason: format!("borg failed to record intake: {e}"),
            },
            trace_id: Some(trace_id),
            ..Default::default()
        });
    }

    // Detach the pipeline from the HTTP handler - same reason as /ingest.
    // Per Design Invariant 1, notification calls run inside the spawn too.
    let tags = request.tags.unwrap_or_default();
    let config = state.config.clone();
    let telegram = state.telegram.clone();
    let desktop = state.desktop.clone();
    let task_trace = trace_id.clone();
    let task_display = display.clone();
    let task_text = request.text;
    tokio::spawn(async move {
        let prior = if let Some(d) = &desktop {
            d.processing(&task_trace, "Processing note...").await
        } else {
            None
        };
        if let Some(t) = &telegram {
            let _ = t.processing(&task_trace, "Processing note...", None).await;
        }
        let content = ContentKind::Text(task_text);
        let result = pipeline::process_content(
            content,
            tags,
            IngestMethod::Http,
            false,
            &config,
            Some(task_trace.clone()),
        )
        .await;
        if let Some(t) = telegram {
            t.result(&result, &task_display, None).await;
        }
        if let Some(d) = desktop {
            d.result(&result, &task_display, prior).await;
        }
        match &result.status {
            IngestStatus::Failed { reason } => log::warn!("Note capture failed: {reason}"),
            IngestStatus::Completed => log::info!("Note captured: {:?}", result.title),
            _ => {}
        }
    });

    Json(IngestResult {
        status: IngestStatus::Queued,
        trace_id: Some(trace_id),
        ..Default::default()
    })
}

pub async fn ingest_multipart(State(state): State<AppState>, mut multipart: Multipart) -> Json<IngestResult> {
    // Generate trace at the door. Any decode/validation failure below
    // produces a DLQ row tied to this trace.
    let trace_id = trace::generate(IngestMethod::Http);
    let mut file_data: Option<(Vec<u8>, String)> = None;
    let mut tags: Vec<String> = vec![];
    let mut force = false;
    let mut decode_error: Option<String> = None;

    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "file" => {
                let filename = field.file_name().unwrap_or("upload").to_string();
                match field.bytes().await {
                    Ok(bytes) => {
                        file_data = Some((bytes.to_vec(), filename));
                    }
                    Err(e) => {
                        log::warn!("Failed to read file field: {e}");
                        decode_error = Some(format!("Failed to read uploaded file: {e}"));
                    }
                }
            }
            "tags" => {
                if let Ok(text) = field.text().await {
                    tags = text
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
            }
            "force" => {
                if let Ok(text) = field.text().await {
                    force = text.trim() == "true";
                }
            }
            _ => {
                log::debug!("Ignoring unknown multipart field: {name}");
            }
        }
    }

    let (intake_kind, intake_preview, intake_filename, intake_bytes) = match (&file_data, &decode_error) {
        (Some((bytes, filename)), _) => {
            let kind = if assets::is_image_extension(filename) {
                IntakeKind::Photo
            } else if assets::is_pdf_extension(filename) {
                IntakeKind::Document
            } else if assets::is_audio_extension(filename) {
                IntakeKind::Audio
            } else if assets::is_document_extension(filename) {
                IntakeKind::Document
            } else {
                IntakeKind::Unknown
            };
            let preview = intake_log::binary_descriptor(kind, filename, bytes.len(), None);
            (kind, preview, Some(filename.clone()), Some(bytes.clone()))
        }
        (None, Some(err)) => (
            IntakeKind::Unknown,
            format!("[multipart decode failed: {err}]"),
            None,
            None,
        ),
        (None, None) => (
            IntakeKind::Empty,
            "[multipart upload missing file field]".to_string(),
            None,
            None,
        ),
    };

    // For multipart, the sidecar carries the descriptor (not the raw binary)
    // so `system/intake/` stays small regardless of upload size.
    let _ = intake_bytes; // we only needed the bytes for descriptor sizing above
    if let Err(e) = intake_log::record_intake(
        &state.config,
        IngestMethod::Http,
        "http",
        intake_kind,
        &intake_preview,
        &trace_id,
    ) {
        log::error!("http/ingest_multipart: failed to record intake trace={trace_id}: {e:#}");
        return Json(IngestResult {
            status: IngestStatus::Failed {
                reason: format!("borg failed to record intake: {e}"),
            },
            trace_id: Some(trace_id),
            ..Default::default()
        });
    }

    if let Some(err) = decode_error {
        intake_log::record_dlq(
            &state.config,
            IngestMethod::Http,
            DlqStage::IntakeReject,
            &format!("bad-payload: {err}"),
            &intake_preview,
            &trace_id,
            None,
        );
        return Json(IngestResult {
            status: IngestStatus::Failed { reason: err },
            trace_id: Some(trace_id),
            ..Default::default()
        });
    }

    let Some((data, filename)) = file_data else {
        let reason = "No 'file' field in multipart upload".to_string();
        intake_log::record_dlq(
            &state.config,
            IngestMethod::Http,
            DlqStage::IntakeReject,
            &format!("bad-payload: {reason}"),
            &intake_preview,
            &trace_id,
            None,
        );
        return Json(IngestResult {
            status: IngestStatus::Failed { reason },
            trace_id: Some(trace_id),
            ..Default::default()
        });
    };

    log::info!(
        "Received multipart file upload: {filename} ({} bytes) (trace={trace_id})",
        data.len()
    );

    let _ = intake_filename;

    let content = if assets::is_image_extension(&filename) {
        ContentKind::Image { data, filename }
    } else if assets::is_pdf_extension(&filename) {
        ContentKind::Pdf { data, filename }
    } else if assets::is_document_extension(&filename) {
        ContentKind::Document { data, filename }
    } else if assets::is_audio_extension(&filename) {
        ContentKind::Audio { data, filename }
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
        intake_log::record_dlq(
            &state.config,
            IngestMethod::Http,
            DlqStage::IntakeReject,
            &reason,
            &intake_preview,
            &trace_id,
            None,
        );
        return Json(IngestResult {
            status: IngestStatus::Failed { reason },
            trace_id: Some(trace_id),
            ..Default::default()
        });
    };

    let display_filename = match &content {
        ContentKind::Image { filename, .. }
        | ContentKind::Pdf { filename, .. }
        | ContentKind::Audio { filename, .. }
        | ContentKind::Document { filename, .. } => filename.clone(),
        _ => "file".to_string(),
    };

    // Detach the pipeline from the HTTP handler - same reason as /ingest.
    // Per Design Invariant 1, notification calls run inside the spawn too.
    let config = state.config.clone();
    let telegram = state.telegram.clone();
    let desktop = state.desktop.clone();
    let task_trace = trace_id.clone();
    let task_display = display_filename.clone();
    let processing_text = format!("Processing file: {task_display}...");
    let display_source = format!("[file: {task_display}]");
    tokio::spawn(async move {
        let prior = if let Some(d) = &desktop {
            d.processing(&task_trace, &processing_text).await
        } else {
            None
        };
        if let Some(t) = &telegram {
            let _ = t.processing(&task_trace, &processing_text, None).await;
        }
        let result = pipeline::process_content(
            content,
            tags,
            IngestMethod::Http,
            force,
            &config,
            Some(task_trace.clone()),
        )
        .await;
        if let Some(t) = telegram {
            t.result(&result, &display_source, None).await;
        }
        if let Some(d) = desktop {
            d.result(&result, &display_source, prior).await;
        }
        match &result.status {
            IngestStatus::Failed { reason } => {
                log::warn!(
                    "File ingest failed for {}: {reason}",
                    result.title.as_deref().unwrap_or("unknown")
                );
            }
            IngestStatus::Completed => {
                log::info!("File ingest completed: {:?}", result.title);
            }
            _ => {}
        }
    });

    Json(IngestResult {
        status: IngestStatus::Queued,
        trace_id: Some(trace_id),
        ..Default::default()
    })
}
