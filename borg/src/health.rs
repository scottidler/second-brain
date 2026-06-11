use axum::Json;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub service: String,
    pub version: String,
}

pub async fn health_handler(service: &'static str, version: &str) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        service: service.to_string(),
        version: version.to_string(),
    })
}

#[cfg(test)]
mod tests;
