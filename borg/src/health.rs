use axum::Json;
use serde::{Deserialize, Serialize};

/// `/ingest` body fields this daemon writes into receipts, beyond the base
/// contract. A client whose request depends on one (`sb borg eval --remediate
/// --apply` needs `replay-of`) checks `/health` first: an older daemon drops
/// an unknown field silently, which would post a remediation with no lineage.
pub const DAEMON_FEATURES: &[&str] = &[FEATURE_REPLAY_OF];

/// The daemon records `replay-of` and `replay-origin` at intake.
pub const FEATURE_REPLAY_OF: &str = "replay-of";

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
