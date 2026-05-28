//! Dream proposal renderer. Writes one markdown file per proposal at
//! `notes/glean-dreams/<kind>-<sha256-12>.md`. Content-addressed:
//! re-running with identical input is a no-op.

use chrono::Utc;
use eyre::{Context, Result};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DreamKind {
    Dedup,
    Xref,
    Stale,
}

impl DreamKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Dedup => "dedup",
            Self::Xref => "xref",
            Self::Stale => "stale",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DreamProposal {
    pub kind: DreamKind,
    pub confidence: f32,
    pub reason: String,
    /// content_hash strings of the source work-items.
    pub source_chunks: Vec<String>,
    pub suggested_title: Option<String>,
    pub direction: Option<String>,
}

pub fn write_proposal(dreams_dir: &Path, proposal: &DreamProposal) -> Result<()> {
    log::debug!(
        "dream::render::write_proposal: kind={} sources={}",
        proposal.kind.as_str(),
        proposal.source_chunks.len()
    );
    std::fs::create_dir_all(dreams_dir).context("mkdir dreams_dir")?;
    let hash = content_hash(proposal);
    let name = format!("{}-{}.md", proposal.kind.as_str(), &hash[..12]);
    let path = dreams_dir.join(&name);
    let body = compose_body(proposal, &hash);
    if path.exists() {
        let existing = std::fs::read_to_string(&path).context("read existing dream")?;
        if existing == body {
            log::debug!("dream::render::write_proposal: noop (identical)");
            return Ok(());
        }
    }
    std::fs::write(&path, body).context("write dream proposal")?;
    Ok(())
}

fn content_hash(proposal: &DreamProposal) -> String {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(proposal.kind.as_str().as_bytes());
    h.update(proposal.reason.as_bytes());
    let mut sources = proposal.source_chunks.clone();
    sources.sort();
    for s in sources {
        h.update(s.as_bytes());
    }
    hex::encode(h.finalize())
}

fn compose_body(proposal: &DreamProposal, hash: &str) -> String {
    let now = Utc::now().to_rfc3339();
    let mut s = String::new();
    s.push_str("---\n");
    s.push_str("type: glean-dream\n");
    s.push_str(&format!("kind: {}\n", proposal.kind.as_str()));
    s.push_str("status: proposed\n");
    s.push_str(&format!("confidence: {:.2}\n", proposal.confidence));
    s.push_str(&format!("hash: {hash}\n"));
    s.push_str("source-chunks:\n");
    for c in &proposal.source_chunks {
        s.push_str(&format!("  - {c}\n"));
    }
    if let Some(d) = &proposal.direction {
        s.push_str(&format!("direction: {d}\n"));
    }
    if let Some(t) = &proposal.suggested_title {
        s.push_str(&format!("suggested-title: \"{}\"\n", t.replace('"', "\\\"")));
    }
    s.push_str(&format!("proposed-at: {now}\n"));
    s.push_str("---\n\n");
    s.push_str(&format!("# {} proposal\n\n", proposal.kind.as_str()));
    s.push_str(&proposal.reason);
    s.push('\n');
    s
}
