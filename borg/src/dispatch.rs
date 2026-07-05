use crate::config::Config;
use crate::notify::{Desktop, Telegram};
use crate::pipeline;
use crate::types::{ContentKind, IngestMethod, IngestResult};

/// Run one captured input through the pipeline with operator-facing progress
/// and result notifications.
///
/// Fires the desktop and telegram sinks side-by-side (trait-free per policy)
/// around `pipeline::process_content`: a "processing" toast/reply before the
/// work, the terminal `IngestResult` after. This is the dispatch boilerplate
/// every transport spawned per input (telegram/ntfy/routes); the per-site
/// parts are the `content`, `tags`, `method`, `force`, `display_source`,
/// `processing_msg`, and (for telegram replies) `chat_id_override`.
///
/// Called from inside the per-input `tokio::spawn` in each transport. The
/// caller keeps its own pre/post logging and returns the `Queued` placeholder;
/// the resolved `IngestResult` is returned here for that logging.
pub(crate) async fn dispatch_ingest(
    content: ContentKind,
    tags: Vec<String>,
    method: IngestMethod,
    force: bool,
    config: &Config,
    trace_id: String,
    display_source: &str,
    processing_msg: &str,
    desktop: Option<Desktop>,
    telegram: Option<Telegram>,
    chat_id_override: Option<i64>,
) -> IngestResult {
    let prior = if let Some(d) = &desktop {
        d.processing(&trace_id, processing_msg).await
    } else {
        None
    };
    if let Some(t) = &telegram {
        let _ = t.processing(&trace_id, processing_msg, chat_id_override).await;
    }

    // dispatch_ingest serves telegram/ntfy/http, where the URL capture note
    // rides inside `ContentKind::Url { note }` and attachments carry no caption
    // migration (that is Signal-only, which calls `process_content` directly).
    let result = pipeline::process_content(content, tags, method, force, config, Some(trace_id), None).await;

    if let Some(t) = telegram {
        t.result(&result, display_source, chat_id_override).await;
    }
    if let Some(d) = desktop {
        d.result(&result, display_source, prior).await;
    }

    result
}
