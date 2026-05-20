//! Domain blocklist. Persisted as YAML at `~/.local/share/borg/blocked-domains.yml`.
//! Populated by Gate-1 rejections (Phase 3) and consulted by Gate-0 pre-fetch.

use chrono::{DateTime, Utc};
use eyre::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct BlockedDomain {
    /// When the domain was added to the blocklist.
    pub blocked_at: String,
    /// When Gate-0 may unblock the domain (RFC3339 timestamp). After this
    /// time the domain is auto-retriable.
    pub retriable_after: String,
    /// Free-form reason the domain was blocked.
    pub reason: String,
    /// Optional count of consecutive rejections against the domain.
    #[serde(default)]
    pub hits: u32,
}

impl BlockedDomain {
    pub fn new(reason: impl Into<String>, retriable_after: DateTime<Utc>) -> Self {
        Self {
            blocked_at: Utc::now().to_rfc3339(),
            retriable_after: retriable_after.to_rfc3339(),
            reason: reason.into(),
            hits: 1,
        }
    }

    /// Whether the block is currently in effect at `now`.
    pub fn is_active_at(&self, now: DateTime<Utc>) -> bool {
        match DateTime::parse_from_rfc3339(&self.retriable_after) {
            Ok(dt) => now < dt.with_timezone(&Utc),
            // Unparseable timestamp is treated as permanently blocked - caller
            // must `blocklist remove` explicitly. This is safer than auto-unblocking.
            Err(_) => true,
        }
    }
}

/// In-memory view of the blocklist. Methods are append/remove-style; persist
/// with `save_to`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Blocklist {
    pub domains: HashMap<String, BlockedDomain>,
}

impl Blocklist {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_file(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path).with_context(|| format!("read blocklist {}", path.display()))?;
        let bl: Self = serde_yaml::from_str(&text).context("parse blocklist yaml")?;
        Ok(bl)
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
        }
        let text = serde_yaml::to_string(self).context("serialize blocklist")?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, text.as_bytes()).with_context(|| format!("write {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
        Ok(())
    }

    pub fn is_blocked(&self, domain: &str, now: DateTime<Utc>) -> bool {
        let domain_lc = domain.to_ascii_lowercase();
        self.domains
            .get(&domain_lc)
            .map(|b| b.is_active_at(now))
            .unwrap_or(false)
    }

    pub fn get(&self, domain: &str) -> Option<&BlockedDomain> {
        let domain_lc = domain.to_ascii_lowercase();
        self.domains.get(&domain_lc)
    }

    /// Insert or refresh a domain. Increments `hits` if the domain already
    /// exists and replaces its `retriable_after` if the new one is later.
    pub fn add_or_refresh(&mut self, domain: &str, reason: &str, retriable_after: DateTime<Utc>) -> &BlockedDomain {
        let domain_lc = domain.to_ascii_lowercase();
        let existed = self.domains.contains_key(&domain_lc);
        let entry = self
            .domains
            .entry(domain_lc.clone())
            .or_insert_with(|| BlockedDomain::new(reason.to_string(), retriable_after));
        if existed {
            entry.hits = entry.hits.saturating_add(1);
        }
        entry.reason = reason.to_string();
        if let Ok(existing) = DateTime::parse_from_rfc3339(&entry.retriable_after) {
            if retriable_after > existing.with_timezone(&Utc) {
                entry.retriable_after = retriable_after.to_rfc3339();
            }
        } else {
            entry.retriable_after = retriable_after.to_rfc3339();
        }
        entry
    }

    pub fn remove(&mut self, domain: &str) -> Option<BlockedDomain> {
        let domain_lc = domain.to_ascii_lowercase();
        self.domains.remove(&domain_lc)
    }

    pub fn list(&self) -> Vec<(String, &BlockedDomain)> {
        let mut rows: Vec<_> = self.domains.iter().map(|(k, v)| (k.clone(), v)).collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        rows
    }
}

/// Default filesystem path for the blocklist yaml.
pub fn default_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from(".local/share"))
        .join("borg")
        .join("blocked-domains.yml")
}

/// Extract the registrable domain (host minus leading `www.`) from a URL. For
/// non-URLs, returns the input lowercased.
pub fn domain_for(url: &str) -> String {
    match url::Url::parse(url).ok().and_then(|u| u.host_str().map(String::from)) {
        Some(host) => {
            let host = host.to_ascii_lowercase();
            host.strip_prefix("www.").map(String::from).unwrap_or(host)
        }
        None => url.to_ascii_lowercase(),
    }
}

/// Gate-0 check: reject if `url` is on the active blocklist. Returns
/// `Err` with a clear reason for the caller to record as a rejection.
pub fn gate_0<T>(blocklist: &Blocklist, url: &str, now: DateTime<Utc>, _: T) -> Result<()>
where
    T: Sized,
{
    let domain = domain_for(url);
    if blocklist.is_blocked(&domain, now) {
        let entry = blocklist.get(&domain).expect("entry present by is_blocked");
        bail!(
            "gate-0: domain {domain} is blocklisted until {} ({})",
            entry.retriable_after,
            entry.reason
        );
    }
    Ok(())
}

/// Parse "anonymous access to domain blocked until Mon Apr 20 2026" style
/// messages to extract a `retriable_after` timestamp. Falls back to `now + 7d`.
pub fn parse_retry_after(message: &str, now: DateTime<Utc>) -> DateTime<Utc> {
    if let Some(idx) = message.to_ascii_lowercase().find("blocked until ") {
        let tail = &message[idx + "blocked until ".len()..];
        // Try RFC3339 first.
        if let Ok(dt) = DateTime::parse_from_rfc3339(tail.trim()) {
            return dt.with_timezone(&Utc);
        }
        // Try "Mon Apr 20 2026" (ctime-ish).
        if let Ok(dt) = chrono::NaiveDate::parse_from_str(
            tail.trim()
                .split(|c: char| c.is_ascii_digit())
                .collect::<String>()
                .trim(),
            "%a %b %Y",
        ) {
            let _ = dt;
        }
        if let Ok(date) = try_parse_date(tail.trim()) {
            return date;
        }
    }
    now + chrono::Duration::days(7)
}

fn try_parse_date(s: &str) -> Result<DateTime<Utc>> {
    // Accept a few common shapes; strip trailing punctuation.
    let s = s.trim_end_matches('.').trim_end_matches(',').trim();
    if let Ok(dt) = chrono::NaiveDate::parse_from_str(s, "%a %b %d %Y") {
        return Ok(dt
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| eyre::eyre!("invalid time"))?
            .and_utc());
    }
    if let Ok(dt) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(dt
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| eyre::eyre!("invalid time"))?
            .and_utc());
    }
    bail!("no recognized date format in {s}");
}

pub fn run_list() -> Result<()> {
    let path = default_path();
    let bl = Blocklist::from_file(&path)?;
    if bl.domains.is_empty() {
        println!("(blocklist empty)");
    } else {
        for (domain, entry) in bl.list() {
            println!(
                "{domain:30} retriable-after={} hits={} reason={}",
                entry.retriable_after, entry.hits, entry.reason
            );
        }
    }
    Ok(())
}

pub fn run_remove(domain: &str) -> Result<()> {
    let path = default_path();
    let mut bl = Blocklist::from_file(&path)?;
    let removed = bl.remove(domain).is_some();
    bl.save_to(&path)?;
    if removed {
        println!("removed: {domain}");
    } else {
        println!("not blocklisted: {domain}");
    }
    Ok(())
}

pub fn run_clear() -> Result<()> {
    let path = default_path();
    let bl = Blocklist::default();
    bl.save_to(&path)?;
    println!("blocklist cleared");
    Ok(())
}

#[cfg(test)]
mod tests;
