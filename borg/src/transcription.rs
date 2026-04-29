use crate::types::{AudioFormat, TranscriptionRequest, TranscriptionResponse};
use eyre::{Context, Result};
use std::time::Duration;

/// Browser-shaped User-Agent applied to Groq requests. The default reqwest
/// UA is plain `reqwest/0.13` which Cloudflare's bot heuristic flags on
/// some egress IPs; sending a Firefox UA makes the WAF treat us as
/// browser traffic.
const BROWSER_UA: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:128.0) Gecko/20100101 Firefox/128.0";

/// Retry budget for transient Groq failures (429 / 503 / network blips).
/// One initial attempt + this many retries.
const GROQ_MAX_RETRIES: u32 = 2;

/// Cap on the back-off between attempts. Used when Retry-After is missing
/// or the server reports a value larger than we're willing to wait.
const GROQ_BACKOFF_CAP: Duration = Duration::from_secs(20);

pub struct TranscriptionClient {
    transcriber_url: String,
    groq_url: String,
    groq_api_key: Option<String>,
    groq_model: String,
    timeout: Duration,
    http: reqwest::Client,
}

impl TranscriptionClient {
    pub fn new(transcriber_url: &str, groq_api_key: Option<String>, groq_model: &str, timeout_secs: u64) -> Self {
        Self {
            transcriber_url: transcriber_url.to_string(),
            groq_url: "https://api.groq.com/openai/v1/audio/transcriptions".to_string(),
            groq_api_key,
            groq_model: groq_model.to_string(),
            timeout: Duration::from_secs(timeout_secs),
            http: reqwest::Client::builder()
                .user_agent(BROWSER_UA)
                .build()
                .expect("reqwest client should build"),
        }
    }

    /// Construct a client pointing at a custom Groq URL (used by tests to
    /// route to a local mock HTTP server). All other behavior matches `new`.
    #[cfg(test)]
    pub fn with_groq_url(
        transcriber_url: &str,
        groq_url: &str,
        groq_api_key: Option<String>,
        groq_model: &str,
        timeout_secs: u64,
    ) -> Self {
        let mut client = Self::new(transcriber_url, groq_api_key, groq_model, timeout_secs);
        client.groq_url = groq_url.to_string();
        client
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

        // Construct the request from scratch each retry: reqwest multipart bodies
        // can't be re-sent after they've been consumed.
        let build_form = || -> Result<reqwest::multipart::Form> {
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
            Ok(form)
        };

        let mut last_err: Option<eyre::Report> = None;
        for attempt in 0..=GROQ_MAX_RETRIES {
            let form = build_form()?;
            let result = self
                .http
                .post(&self.groq_url)
                .bearer_auth(api_key)
                .multipart(form)
                .timeout(self.timeout)
                .send()
                .await;
            let response = match result {
                Ok(r) => r,
                Err(e) => {
                    log::warn!("Groq attempt {attempt}: network error: {e:#}");
                    last_err = Some(e.into());
                    if attempt < GROQ_MAX_RETRIES {
                        tokio::time::sleep(retry_backoff(attempt, None)).await;
                        continue;
                    }
                    break;
                }
            };

            let status = response.status();
            if status.is_success() {
                let json: serde_json::Value = response.json().await.context("Failed to parse Groq response")?;
                return Ok(TranscriptionResponse {
                    text: json["text"].as_str().unwrap_or("").to_string(),
                    language: json["language"].as_str().unwrap_or("en").to_string(),
                    duration_secs: json["duration"].as_f64().unwrap_or(0.0),
                });
            }

            // Capture Retry-After before consuming the body.
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .map(Duration::from_secs);

            let body = response.text().await.unwrap_or_default();
            let retriable = matches!(status.as_u16(), 429 | 502 | 503 | 504);
            log::warn!("Groq attempt {attempt}: status={status} retriable={retriable} body={body}");
            last_err = Some(eyre::eyre!("Groq API status {status}: {body}"));

            if retriable && attempt < GROQ_MAX_RETRIES {
                tokio::time::sleep(retry_backoff(attempt, retry_after)).await;
                continue;
            }
            break;
        }

        Err(last_err.unwrap_or_else(|| eyre::eyre!("Groq failed without recording an error")))
    }
}

/// Compute a backoff delay: prefer the server-provided Retry-After, capped
/// at `GROQ_BACKOFF_CAP`; fall back to exponential 1s * 2^attempt.
fn retry_backoff(attempt: u32, retry_after: Option<Duration>) -> Duration {
    if let Some(server) = retry_after {
        return server.min(GROQ_BACKOFF_CAP);
    }
    let secs = 1u64
        .checked_shl(attempt)
        .unwrap_or(u64::MAX)
        .min(GROQ_BACKOFF_CAP.as_secs());
    Duration::from_secs(secs)
}

#[cfg(test)]
mod tests;
