use axum::Json;
use serde::{Deserialize, Serialize};

/// Optional capabilities this daemon has beyond the base `/ingest` contract.
/// A client whose request depends on one (`sb borg eval --remediate --apply`
/// needs `replay-of`, `sb borg distill` needs `distill`) checks `/health`
/// first: an older daemon drops an unknown `/ingest` field silently, which
/// would post a remediation with no lineage, and has no `/distill` route.
/// `sb borg ingest` checks `bulk-batch` before a bulk.
pub const DAEMON_FEATURES: &[&str] = &[FEATURE_REPLAY_OF, FEATURE_DISTILL, FEATURE_BULK_BATCH];

/// The daemon records `replay-of` and `replay-origin` at intake.
pub const FEATURE_REPLAY_OF: &str = "replay-of";

/// The daemon serves `POST /distill`, `GET /distill/{trace}` and
/// `GET /distill/{trace}/file/{*path}`: a scratch run that writes nothing to
/// the vault.
pub const FEATURE_DISTILL: &str = "distill";

/// The daemon records `IngestRequest.batch` at intake, serves
/// `POST /batch/{id}/seal`, and sends one summary when a sealed batch drains.
pub const FEATURE_BULK_BATCH: &str = "bulk-batch";

#[derive(Debug, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub service: String,
    pub version: String,
    /// [`DAEMON_FEATURES`]. `default`: a daemon older than the field sends
    /// none, which reads as "supports nothing optional".
    #[serde(default)]
    pub features: Vec<String>,
}

impl HealthResponse {
    pub fn supports(&self, feature: &str) -> bool {
        self.features.iter().any(|f| f == feature)
    }
}

pub async fn health_handler(service: &'static str, version: &str) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        service: service.to_string(),
        version: version.to_string(),
        features: DAEMON_FEATURES.iter().map(|f| f.to_string()).collect(),
    })
}

#[cfg(test)]
mod tests;
