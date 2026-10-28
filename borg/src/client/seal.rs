//! The client half of a `sb borg ingest` bulk: refuse a daemon that cannot
//! batch, and seal the batch once its URL items are submitted (design:
//! docs/design/2026-10-27-multi-target-ingest.md).

use eyre::{Context, Result};
use vault::daemon::client::DaemonClient;

use crate::batch::SealRequest;
use crate::config::Config;
use crate::health::{FEATURE_BULK_BATCH, HealthResponse};

/// Refuse a daemon whose `/health` lacks `bulk-batch`: an older one drops the
/// `batch` field silently, and the bulk would never summarize.
pub async fn require_bulk_feature(config: &Config) -> Result<()> {
    let daemon = DaemonClient::new(&config.hotkey, config.server.auth_token.as_deref())?;
    let resp = daemon
        .get("/health", &[], None)
        .await
        .with_context(|| format!("checking the daemon at {} before a bulk ingest", daemon.addr()))?;
    let health: HealthResponse = daemon.json("/health", resp).await?;
    log::debug!(
        "seal::require_bulk_feature: version={} features={:?}",
        health.version,
        health.features
    );
    if !health.supports(FEATURE_BULK_BATCH) {
        eyre::bail!(
            "the daemon at {} (version {}) has no `{FEATURE_BULK_BATCH}` feature; deploy it (`otto deploy`) \
             before a bulk ingest, or pass at most ingest.bulk-threshold targets",
            daemon.addr(),
            health.version
        );
    }
    Ok(())
}

/// `POST /batch/{id}/seal` with the trace ids the client saw `Queued`. Any
/// non-2xx answer (404 unknown batch, 409 a different set) is an error.
pub async fn seal_batch(config: &Config, batch_id: &str, trace_ids: &[String]) -> Result<()> {
    log::debug!("seal::seal_batch: batch={batch_id} trace_ids={}", trace_ids.len());
    let daemon = DaemonClient::new(&config.hotkey, config.server.auth_token.as_deref())?;
    let path = format!("/batch/{batch_id}/seal");
    let body = serde_json::to_value(SealRequest {
        trace_ids: trace_ids.to_vec(),
    })?;
    let resp = daemon.post_json(&path, &body).await?;
    daemon.ensure_success(&path, resp).await?;
    Ok(())
}
