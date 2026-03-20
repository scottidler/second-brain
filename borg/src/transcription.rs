use crate::types::{AudioFormat, TranscriptionRequest, TranscriptionResponse};
use eyre::{Context, Result};
use std::time::Duration;

pub struct TranscriptionClient {
    transcriber_url: String,
    groq_api_key: Option<String>,
    groq_model: String,
    timeout: Duration,
    http: reqwest::Client,
}

impl TranscriptionClient {
    pub fn new(transcriber_url: &str, groq_api_key: Option<String>, groq_model: &str, timeout_secs: u64) -> Self {
        Self {
            transcriber_url: transcriber_url.to_string(),
            groq_api_key,
            groq_model: groq_model.to_string(),
            timeout: Duration::from_secs(timeout_secs),
            http: reqwest::Client::new(),
        }
    }

    pub async fn transcribe(
        &self,
        audio_bytes: Vec<u8>,
        format: AudioFormat,
        language: Option<String>,
    ) -> Result<TranscriptionResponse> {
        // Tier 2: Try remote transcriber first
        log::debug!(
            "Tier 2: Trying remote transcriber at {} ({} bytes audio)",
            self.transcriber_url,
            audio_bytes.len()
        );
        match self.try_transcriber(&audio_bytes, &format, &language).await {
            Ok(response) => {
                log::info!(
                    "Transcription via remote transcriber succeeded ({} chars)",
                    response.text.len()
                );
                return Ok(response);
            }
            Err(e) => {
                log::warn!("Remote transcriber failed: {e:#}");
            }
        }

        // Tier 3: Fall back to Groq API
        log::debug!(
            "Tier 3: Trying Groq API (model={}, key={})",
            self.groq_model,
            if self.groq_api_key.is_some() { "present" } else { "MISSING" }
        );
        match self.try_groq(&audio_bytes, &format, &language).await {
            Ok(response) => {
                log::info!("Transcription via Groq succeeded ({} chars)", response.text.len());
                Ok(response)
            }
            Err(e) => {
                log::error!("Groq transcription also failed: {e:#}");
                Err(e).context("Both transcriber and Groq fallback failed")
            }
        }
    }

    async fn try_transcriber(
        &self,
        audio_bytes: &[u8],
        format: &AudioFormat,
        language: &Option<String>,
    ) -> Result<TranscriptionResponse> {
        let url = format!("{}/transcribe", self.transcriber_url);
        let request = TranscriptionRequest {
            audio_bytes: audio_bytes.to_vec(),
            language: language.clone(),
            format: match format {
                AudioFormat::Mp3 => AudioFormat::Mp3,
                AudioFormat::Wav => AudioFormat::Wav,
                AudioFormat::Ogg => AudioFormat::Ogg,
            },
        };

        let response = self
            .http
            .post(&url)
            .timeout(self.timeout)
            .json(&request)
            .send()
            .await
            .context("Failed to reach remote transcriber")?;

        if !response.status().is_success() {
            eyre::bail!("Remote transcriber returned status {}", response.status());
        }

        response
            .json::<TranscriptionResponse>()
            .await
            .context("Failed to parse transcriber response")
    }

    async fn try_groq(
        &self,
        audio_bytes: &[u8],
        format: &AudioFormat,
        language: &Option<String>,
    ) -> Result<TranscriptionResponse> {
        let api_key = self
            .groq_api_key
            .as_ref()
            .ok_or_else(|| eyre::eyre!("GROQ_API_KEY not set, cannot fall back to Groq"))?;

        let extension = match format {
            AudioFormat::Mp3 => "mp3",
            AudioFormat::Wav => "wav",
            AudioFormat::Ogg => "ogg",
        };

        let file_part = reqwest::multipart::Part::bytes(audio_bytes.to_vec())
            .file_name(format!("audio.{extension}"))
            .mime_str(&format!("audio/{extension}"))
            .context("Invalid MIME type")?;

        let mut form = reqwest::multipart::Form::new()
            .text("model", self.groq_model.clone())
            .text("response_format", "json")
            .part("file", file_part);

        if let Some(lang) = language {
            form = form.text("language", lang.clone());
        }

        let response = self
            .http
            .post("https://api.groq.com/openai/v1/audio/transcriptions")
            .bearer_auth(api_key)
            .multipart(form)
            .timeout(self.timeout)
            .send()
            .await
            .context("Failed to reach Groq API")?;

        if !response.status().is_success() {
            let body = response.text().await.unwrap_or_default();
            eyre::bail!("Groq API error: {body}");
        }

        let json: serde_json::Value = response.json().await.context("Failed to parse Groq response")?;

        Ok(TranscriptionResponse {
            text: json["text"].as_str().unwrap_or("").to_string(),
            language: json["language"].as_str().unwrap_or("en").to_string(),
            duration_secs: json["duration"].as_f64().unwrap_or(0.0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_construction() {
        let client = TranscriptionClient::new(
            "http://localhost:8090",
            Some("test-key".to_string()),
            "whisper-large-v3",
            120,
        );
        assert_eq!(client.transcriber_url, "http://localhost:8090");
        assert_eq!(client.groq_model, "whisper-large-v3");
        assert_eq!(client.timeout, Duration::from_secs(120));
    }

    #[test]
    fn test_client_without_groq_key() {
        let client = TranscriptionClient::new("http://localhost:8090", None, "whisper-large-v3", 120);
        assert!(client.groq_api_key.is_none());
    }
}
