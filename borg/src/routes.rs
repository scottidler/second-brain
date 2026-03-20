use axum::Json;
use axum::extract::{Multipart, State};

use serde::Deserialize;

use crate::AppState;
use crate::assets;
use crate::health::HealthResponse;
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

pub async fn ingest(State(state): State<AppState>, Json(request): Json<IngestRequest>) -> Json<IngestResult> {
    log::info!("Received ingest request for URL: {}", request.url);

    let tags = request.tags.unwrap_or_default();
    let method = request.method.unwrap_or(IngestMethod::Http);
    let trace_id = trace::generate(method);

    if let Some(ref n) = state.notifier {
        let _ = n.processing(&trace_id, "Processing...", None).await;
    }

    let content = ContentKind::Url(request.url.clone());
    let result = pipeline::process_content(content, tags, method, request.force, &state.config, Some(trace_id)).await;

    if let Some(ref n) = state.notifier {
        n.result(&result, &request.url, None).await;
    }

    match &result.status {
        IngestStatus::Failed { reason } => {
            log::warn!("Ingest failed for {}: {reason}", request.url);
        }
        IngestStatus::Completed => {
            log::info!("Ingest completed for {}", request.url);
        }
        IngestStatus::Duplicate { .. } => {
            log::info!("Duplicate URL skipped for {}", request.url);
        }
        IngestStatus::Queued => {}
    }

    Json(result)
}

pub async fn note(State(state): State<AppState>, Json(request): Json<NoteRequest>) -> Json<IngestResult> {
    log::info!("Received note request: {} chars", request.text.len());

    let trace_id = trace::generate(IngestMethod::Http);
    let display = if request.text.len() > 50 {
        format!("{}...", &request.text[..50])
    } else {
        request.text.clone()
    };

    if let Some(ref n) = state.notifier {
        let _ = n.processing(&trace_id, "Processing note...", None).await;
    }

    let tags = request.tags.unwrap_or_default();
    let content = ContentKind::Text(request.text);
    let result =
        pipeline::process_content(content, tags, IngestMethod::Http, false, &state.config, Some(trace_id)).await;

    if let Some(ref n) = state.notifier {
        n.result(&result, &display, None).await;
    }

    match &result.status {
        IngestStatus::Failed { reason } => {
            log::warn!("Note capture failed: {reason}");
        }
        IngestStatus::Completed => {
            log::info!("Note captured: {:?}", result.title);
        }
        _ => {}
    }

    Json(result)
}

pub async fn ingest_multipart(State(state): State<AppState>, mut multipart: Multipart) -> Json<IngestResult> {
    let mut file_data: Option<(Vec<u8>, String)> = None;
    let mut tags: Vec<String> = vec![];
    let mut force = false;

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
                        return Json(IngestResult {
                            status: IngestStatus::Failed {
                                reason: format!("Failed to read uploaded file: {e}"),
                            },
                            ..Default::default()
                        });
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

    let Some((data, filename)) = file_data else {
        return Json(IngestResult {
            status: IngestStatus::Failed {
                reason: "No 'file' field in multipart upload".to_string(),
            },
            ..Default::default()
        });
    };

    log::info!("Received multipart file upload: {filename} ({} bytes)", data.len());

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
        return Json(IngestResult {
            status: IngestStatus::Failed {
                reason: format!(
                    "Unsupported file type: {}. Supported extensions: {}",
                    filename,
                    all_extensions.join(", ")
                ),
            },
            ..Default::default()
        });
    };

    let trace_id = trace::generate(IngestMethod::Http);
    let display_filename = match &content {
        ContentKind::Image { filename, .. }
        | ContentKind::Pdf { filename, .. }
        | ContentKind::Audio { filename, .. }
        | ContentKind::Document { filename, .. } => filename.clone(),
        _ => "file".to_string(),
    };

    if let Some(ref n) = state.notifier {
        let _ = n
            .processing(&trace_id, &format!("Processing file: {display_filename}..."), None)
            .await;
    }

    let result =
        pipeline::process_content(content, tags, IngestMethod::Http, force, &state.config, Some(trace_id)).await;

    if let Some(ref n) = state.notifier {
        n.result(&result, &format!("[file: {display_filename}]"), None).await;
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

    Json(result)
}
