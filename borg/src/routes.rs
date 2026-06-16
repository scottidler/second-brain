use axum::Json;
use axum::extract::{Multipart, Path, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::assets;
use crate::health::HealthResponse;
use crate::intake::{self as intake_log, Kind as IntakeKind};
use crate::trace;
use crate::types::{ContentKind, IngestMethod, IngestRequest, IngestResult, IngestStatus};
use vault::receipts::FailureStage;

#[derive(Debug, Deserialize)]
pub struct NoteRequest {
    pub text: String,
    pub tags: Option<Vec<String>>,
}

/// Constant-time byte comparison for the auth token. A length-check short
/// circuit plus a byte-fold avoids leaking the matching prefix length through
/// early-return timing the way `==` does.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Auth gate for the HTTP write routes (`/ingest`, `/ingest/file`, `/note`).
///
/// When `state.auth_token` is `Some` (a token was configured and resolved at
/// startup), the request must carry a matching `Authorization: Bearer <token>`
/// header; otherwise it is rejected with `401`. When `None` (the default), the
/// request passes through unchanged - backward-compatible, unauthenticated.
///
/// This runs as a layer in FRONT of the write handlers, so the check executes
/// before any intake write (`record_received_with_sidecar`). A rejected
/// request therefore never creates a receipts row or a raw-input sidecar - a
/// `401` is a *refused* request, not a *dropped* input, so borg's
/// durable-capture-at-the-door invariant does not apply to it.
pub async fn require_auth(State(state): State<AppState>, request: Request, next: Next) -> Response {
    log::debug!(
        "require_auth: path={} auth_configured={}",
        request.uri().path(),
        state.auth_token.is_some()
    );
    let Some(expected) = state.auth_token.as_deref() else {
        return next.run(request).await;
    };
    let authorized = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|token| constant_time_eq(token.as_bytes(), expected.as_bytes()))
        .unwrap_or(false);
    if authorized {
        next.run(request).await
    } else {
        log::warn!(
            "require_auth: rejecting unauthenticated write request to {}",
            request.uri().path()
        );
        (StatusCode::UNAUTHORIZED, "unauthorized\n").into_response()
    }
}

pub async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    crate::health::health_handler("obsidian-borg", &state.version).await
}

#[derive(serde::Serialize, Default)]
pub struct AuditHealth {
    pub received: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub crashed: usize,
    pub failed_24h: usize,
    pub crashed_24h: usize,
    pub degraded_24h: usize,
}

/// Live receipts health: lifetime status counts plus the last-24h failed /
/// crashed counts. Operators poll this to detect a silent-drop regression
/// (`crashed_24h > 0` = the watchdog had to declare an input lost) without
/// shelling into `sb borg log`.
pub async fn health_audit(State(_state): State<AppState>) -> Json<AuditHealth> {
    let stats = crate::triage::audit_health_stats().unwrap_or_default();
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
    if let Err(e) = intake_log::record_received_with_sidecar(
        &state.config,
        method,
        IntakeKind::Url,
        &request.url,
        request.url.as_bytes(),
        &trace_id,
    ) {
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
    // future the moment Firefox recycled its service worker, leaving the
    // receipts row stuck in `received` with no terminal resolution until the
    // watchdog promoted it to `crashed` 31 minutes later.
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
        let result = crate::dispatch::dispatch_ingest(
            ContentKind::Url(task_url.clone()),
            tags,
            method,
            force,
            &config,
            task_trace,
            &task_url,
            "Processing...",
            desktop,
            telegram,
            None,
        )
        .await;
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

/// Terminal-state view of one receipts row, returned by `GET /trace/{id}`.
/// Replay/reingest poll this endpoint for a trace's terminal state because
/// the receipts DB is per-host on the daemon - a client host (laptop) POSTs
/// to the daemon and owns no receipts DB, so it cannot read the row directly.
#[derive(Debug, Serialize, Deserialize)]
pub struct TraceStateResponse {
    pub found: bool,
    pub trace_id: String,
    /// `received` / `succeeded` / `failed` (a crashed row is `failed` with
    /// `failure_stage = "crashed"`).
    pub status: Option<String>,
    pub failure_stage: Option<String>,
    pub note_path: Option<String>,
}

/// `GET /trace/{trace_id}` - read a single receipts row's terminal state.
/// Auth-gated like the write routes.
pub async fn trace_state(State(_state): State<AppState>, Path(trace_id): Path<String>) -> Response {
    log::debug!("trace_state: trace_id={trace_id}");
    let lookup = crate::receipts::open_default().and_then(|conn| crate::receipts::get(&conn, &trace_id));
    match lookup {
        Ok(Some(r)) => Json(TraceStateResponse {
            found: true,
            trace_id: r.trace_id,
            status: Some(r.status),
            failure_stage: r.failure_stage,
            note_path: r.note_path,
        })
        .into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(TraceStateResponse {
                found: false,
                trace_id,
                status: None,
                failure_stage: None,
                note_path: None,
            }),
        )
            .into_response(),
        Err(e) => {
            log::error!("trace_state: receipts lookup failed for {trace_id}: {e:#}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("receipts lookup failed: {e}"),
            )
                .into_response()
        }
    }
}

pub async fn note(State(state): State<AppState>, Json(request): Json<NoteRequest>) -> Json<IngestResult> {
    log::info!("Received note request: {} chars", request.text.len());

    let trace_id = trace::generate(IngestMethod::Http);
    let display = vault::text::truncate_with_ellipsis(&request.text, 50);

    if let Err(e) = intake_log::record_received_with_sidecar(
        &state.config,
        IngestMethod::Http,
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
        let result = crate::dispatch::dispatch_ingest(
            ContentKind::Text(task_text),
            tags,
            IngestMethod::Http,
            false,
            &config,
            task_trace,
            &task_display,
            "Processing note...",
            desktop,
            telegram,
            None,
        )
        .await;
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
    // produces a failed receipts row tied to this trace.
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
                // Repeated `tags` fields (one value each), NOT one
                // comma-split field - matches the no-comma CLI list rule.
                if let Ok(text) = field.text().await {
                    let t = text.trim();
                    if !t.is_empty() {
                        tags.push(t.to_string());
                    }
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

    // The descriptor needs only the byte LENGTH, so this never clones the
    // full upload (the previous `Some(bytes.clone())` cloned the entire
    // payload only to discard it via `let _ =`).
    let (intake_kind, intake_preview) = match (&file_data, &decode_error) {
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
            (kind, preview)
        }
        (None, Some(err)) => (IntakeKind::Unknown, format!("[multipart decode failed: {err}]")),
        (None, None) => (IntakeKind::Empty, "[multipart upload missing file field]".to_string()),
    };

    // For multipart, the sidecar carries the descriptor (not the raw binary)
    // so `system/intake/` stays small regardless of upload size.
    if let Err(e) = intake_log::record_received_with_sidecar(
        &state.config,
        IngestMethod::Http,
        intake_kind,
        &intake_preview,
        intake_preview.as_bytes(),
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
        intake_log::record_failure_at_door(
            IngestMethod::Http,
            &trace_id,
            FailureStage::IntakeRejected,
            &format!("bad-payload: {err}"),
        );
        return Json(IngestResult {
            status: IngestStatus::Failed { reason: err },
            trace_id: Some(trace_id),
            ..Default::default()
        });
    }

    let Some((data, filename)) = file_data else {
        let reason = "No 'file' field in multipart upload".to_string();
        intake_log::record_failure_at_door(
            IngestMethod::Http,
            &trace_id,
            FailureStage::IntakeRejected,
            &format!("bad-payload: {reason}"),
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
        intake_log::record_failure_at_door(IngestMethod::Http, &trace_id, FailureStage::IntakeRejected, &reason);
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
        let result = crate::dispatch::dispatch_ingest(
            content,
            tags,
            IngestMethod::Http,
            force,
            &config,
            task_trace,
            &display_source,
            &processing_text,
            desktop,
            telegram,
            None,
        )
        .await;
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
