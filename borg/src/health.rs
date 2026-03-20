use axum::Json;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub service: String,
    pub version: String,
}

pub async fn health_handler(service: &'static str, version: &'static str) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        service: service.to_string(),
        version: version.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_health_handler() {
        let Json(resp) = health_handler("test-service", "0.1.0").await;
        assert_eq!(resp.status, "ok");
        assert_eq!(resp.service, "test-service");
        assert_eq!(resp.version, "0.1.0");
    }
}
