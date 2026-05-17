//! GitHub REST-API Stage-0/1 fetcher.
//!
//! For `github.com/<owner>/<repo>` URLs (no path beyond the repo root) the
//! generic fetcher chain is bypassed; this module calls the GitHub REST API
//! directly (`GET /repos/{owner}/{repo}` + `GET /repos/{owner}/{repo}/readme`)
//! and returns a `FetchResult` whose bytes are the README markdown plus a
//! short metadata block. The structured fields (stars, primary language,
//! last commit, topics) come back on `RepoMetadata` so the Phase-4
//! `RepoDistiller` can attach them to `Distilled.kind_specific` without
//! re-fetching.
//!
//! Authentication via `GITHUB_TOKEN` env is optional; the API works
//! unauthenticated subject to a 60 req/h IP rate limit. Authenticated
//! callers get 5K req/h.

use async_trait::async_trait;
use eyre::{Context, Result, bail};
use serde::Deserialize;

use crate::stages::artifact::sha256_hex;
use crate::stages::fetcher::Fetcher;
use crate::types::{FetchMeta, FetchResult};

const API_BASE: &str = "https://api.github.com";
const USER_AGENT: &str = "obsidian-borg-github-fetcher/0.1";

/// Stage-0 metadata frozen at ingest. Mirrors `vault::distilled::RepoPayload`
/// in field names so the distiller can copy through verbatim. Not the same
/// type because we want to keep the distillers crate free of HTTP/JSON deps.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoMetadata {
    pub owner: String,
    pub repo: String,
    pub stars: Option<u32>,
    pub primary_language: Option<String>,
    /// ISO 8601 UTC, e.g. "2026-05-16T14:03:22Z".
    pub last_commit: Option<String>,
    pub topics: Vec<String>,
    pub default_branch: Option<String>,
    /// Description from the GitHub /repos response.
    pub description: Option<String>,
}

/// Result of a full github fetch: rendered markdown transcript (README +
/// metadata block) plus the structured metadata.
#[derive(Debug, Clone)]
pub struct RepoFetch {
    pub transcript: String,
    pub metadata: RepoMetadata,
    /// Raw GitHub-API JSON envelope captured before deserialization:
    /// `{"repo": <unparsed-/repos-response>, "readme": <unparsed-/readme-response>}`.
    /// Persisted as Stage-0 `fetched.html` (with `extractor: "github-api"` and
    /// `content_type: "application/json"`) by `distill_for_publish_repo` so a
    /// future `borg replay --from-stage 1` can reconstruct the distiller's
    /// input without re-hitting the GitHub API.
    pub raw_json: Vec<u8>,
}

/// Parse a github.com URL into (owner, repo) if it points at a repo root.
/// Returns None for non-github URLs or URLs that go deeper than `/owner/repo`
/// (issues, PRs, files inside the tree, etc. - those should fall through to
/// the generic fetcher chain).
pub fn parse_repo_url(url: &str) -> Option<(String, String)> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    if host != "github.com" && !host.ends_with(".github.com") {
        return None;
    }
    let mut segments = parsed.path_segments()?.filter(|s| !s.is_empty());
    let owner = segments.next()?.to_string();
    let repo_raw = segments.next()?;
    let repo = repo_raw.trim_end_matches(".git").to_string();
    if segments.next().is_some() {
        return None;
    }
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner, repo))
}

/// Render a markdown transcript from README markdown + the structured
/// metadata. The metadata block leads so Fabric sees the high-signal fields
/// before the README's marketing prose.
pub fn render_transcript(readme_md: &str, metadata: &RepoMetadata) -> String {
    let mut out = String::new();
    out.push_str("# Repository Metadata\n\n");
    out.push_str(&format!("- repo: {}/{}\n", metadata.owner, metadata.repo));
    if let Some(stars) = metadata.stars {
        out.push_str(&format!("- stars: {stars}\n"));
    }
    if let Some(lang) = &metadata.primary_language {
        out.push_str(&format!("- primary-language: {lang}\n"));
    }
    if let Some(commit) = &metadata.last_commit {
        out.push_str(&format!("- last-commit: {commit}\n"));
    }
    if !metadata.topics.is_empty() {
        out.push_str(&format!("- topics: {}\n", metadata.topics.join(", ")));
    }
    if let Some(branch) = &metadata.default_branch {
        out.push_str(&format!("- default-branch: {branch}\n"));
    }
    if let Some(desc) = &metadata.description {
        out.push_str(&format!("- description: {desc}\n"));
    }
    out.push_str("\n# README\n\n");
    out.push_str(readme_md.trim());
    out.push('\n');
    out
}

#[derive(Debug, Deserialize)]
struct RepoResponse {
    #[serde(default)]
    stargazers_count: Option<u32>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    pushed_at: Option<String>,
    #[serde(default)]
    topics: Vec<String>,
    #[serde(default)]
    default_branch: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReadmeResponse {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    encoding: Option<String>,
}

/// Production GitHub fetcher. Uses `reqwest` against `api.github.com`.
#[derive(Debug, Clone)]
pub struct GitHubFetcher {
    client: reqwest::Client,
    token: Option<String>,
}

impl GitHubFetcher {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_else(|e| {
                    log::warn!("GitHubFetcher: falling back to default client: {e:#}");
                    reqwest::Client::new()
                }),
            token: std::env::var("GITHUB_TOKEN").ok().filter(|s| !s.is_empty()),
        }
    }

    /// Build with an explicit token (for tests).
    pub fn with_token(token: Option<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_else(|e| {
                    log::warn!("GitHubFetcher: falling back to default client: {e:#}");
                    reqwest::Client::new()
                }),
            token,
        }
    }

    /// Fetch repo metadata + README and render a transcript. Returns
    /// `RepoFetch` (structured + transcript + raw envelope) rather than
    /// `FetchResult` so the distiller can keep the metadata and the staging
    /// layer can persist the raw bytes for replay.
    pub async fn fetch_repo(&self, owner: &str, repo: &str) -> Result<RepoFetch> {
        log::debug!("GitHubFetcher::fetch_repo: owner={owner} repo={repo}");
        let repo_bytes = self.fetch_repo_meta_bytes(owner, repo).await?;
        let repo_meta: RepoResponse = serde_json::from_slice(&repo_bytes).context("github: /repos parse failed")?;

        let readme_bytes = self.fetch_readme_bytes(owner, repo).await.ok();
        let readme_md = match &readme_bytes {
            Some(bytes) => decode_readme_from_bytes(bytes).unwrap_or_else(|e| {
                log::warn!("GitHubFetcher: readme decode failed for {owner}/{repo}: {e:#}; using empty README");
                String::new()
            }),
            None => {
                log::warn!("GitHubFetcher: readme fetch failed for {owner}/{repo}; using empty README");
                String::new()
            }
        };

        let metadata = RepoMetadata {
            owner: owner.to_string(),
            repo: repo.to_string(),
            stars: repo_meta.stargazers_count,
            primary_language: repo_meta.language,
            last_commit: repo_meta.pushed_at,
            topics: repo_meta.topics,
            default_branch: repo_meta.default_branch,
            description: repo_meta.description,
        };
        let transcript = render_transcript(&readme_md, &metadata);

        // Build the Stage-0 JSON envelope from the raw bytes the API returned.
        // Inlining as serde_json::Value preserves the exact response shape
        // (whitespace not preserved, but field set is whole) without forcing
        // Serialize derives on RepoResponse/ReadmeResponse.
        let envelope = serde_json::json!({
            "repo": serde_json::from_slice::<serde_json::Value>(&repo_bytes).unwrap_or(serde_json::Value::Null),
            "readme": readme_bytes
                .as_deref()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
                .unwrap_or(serde_json::Value::Null),
        });
        let raw_json = serde_json::to_vec(&envelope).context("github: envelope serialize failed")?;

        Ok(RepoFetch {
            transcript,
            metadata,
            raw_json,
        })
    }

    async fn fetch_repo_meta_bytes(&self, owner: &str, repo: &str) -> Result<Vec<u8>> {
        let url = format!("{API_BASE}/repos/{owner}/{repo}");
        let mut req = self.client.get(&url).header("Accept", "application/vnd.github+json");
        if let Some(token) = &self.token {
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        let response = req.send().await.context("github: /repos request failed")?;
        let status = response.status();
        if !status.is_success() {
            bail!("github /repos/{owner}/{repo} returned HTTP {}", status.as_u16());
        }
        let bytes = response.bytes().await.context("github: /repos bytes failed")?;
        Ok(bytes.to_vec())
    }

    async fn fetch_readme_bytes(&self, owner: &str, repo: &str) -> Result<Vec<u8>> {
        let url = format!("{API_BASE}/repos/{owner}/{repo}/readme");
        let mut req = self.client.get(&url).header("Accept", "application/vnd.github+json");
        if let Some(token) = &self.token {
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        let response = req.send().await.context("github: /readme request failed")?;
        let status = response.status();
        if !status.is_success() {
            bail!("github /repos/{owner}/{repo}/readme returned HTTP {}", status.as_u16());
        }
        let bytes = response.bytes().await.context("github: /readme bytes failed")?;
        Ok(bytes.to_vec())
    }
}

/// Decode a `/readme` JSON response body's `content` field into raw README
/// markdown. Returns an empty string if `content` is missing or empty.
fn decode_readme_from_bytes(bytes: &[u8]) -> Result<String> {
    let body: ReadmeResponse = serde_json::from_slice(bytes).context("github: /readme parse failed")?;
    let content = body.content.unwrap_or_default();
    match body.encoding.as_deref() {
        Some("base64") => decode_base64_readme(&content),
        None | Some("") => Ok(content),
        Some(other) => bail!("github: unknown readme encoding: {other}"),
    }
}

fn decode_base64_readme(content: &str) -> Result<String> {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    // GitHub returns the body wrapped at 60 columns with literal newlines.
    let cleaned: String = content.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = STANDARD.decode(&cleaned).context("github: base64 decode failed")?;
    String::from_utf8(bytes).context("github: readme not utf-8")
}

impl Default for GitHubFetcher {
    fn default() -> Self {
        Self::new()
    }
}

/// `Fetcher` adapter for `MultiFetcher`. Returns a `FetchResult` whose bytes
/// are the rendered markdown transcript; structured metadata is discarded
/// here. Callers that need the structured fields must call `fetch_repo`
/// directly. As of Phase 4 this trait impl is wired into `MultiFetcher` but
/// not yet on the legacy hot path (process_article_fabric calls
/// `fabric::fetch_article` directly).
#[async_trait]
impl Fetcher for GitHubFetcher {
    async fn fetch(&self, url: &str) -> Result<FetchResult> {
        let Some((owner, repo)) = parse_repo_url(url) else {
            bail!("github fetcher: url is not a github repo root: {url}");
        };
        let fetched = self.fetch_repo(&owner, &repo).await?;
        let bytes = fetched.transcript.into_bytes();
        let sha256 = sha256_hex(&bytes);
        let meta = FetchMeta {
            source: url.to_string(),
            extractor: "github-api".to_string(),
            status: 200,
            content_type: Some("text/markdown".to_string()),
            bytes: bytes.len() as u64,
            sha256,
            fallbacks_attempted: Vec::new(),
        };
        Ok(FetchResult { bytes, meta })
    }
}

#[cfg(test)]
mod tests;
