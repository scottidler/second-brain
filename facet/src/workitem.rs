//! Work-item identity, slug derivation, cross-session clustering.
//!
//! The cluster stage feeds the cluster LLM a YAML digest of one session's
//! NEW turns plus a list of known work-items the same repo touches. The
//! LLM returns one or more [`Assignment`] ranges that partition the new
//! turns by underlying problem. Each assignment becomes one
//! `cluster_assignments` row in the ledger, and the extract stage later
//! picks those rows up.

pub mod cluster;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkItemStatus {
    Active,
    Dormant,
    Archived,
}

impl WorkItemStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Dormant => "dormant",
            Self::Archived => "archived",
        }
    }
}

impl std::str::FromStr for WorkItemStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "active" => Ok(Self::Active),
            "dormant" => Ok(Self::Dormant),
            "archived" => Ok(Self::Archived),
            _ => Err(format!("unknown WorkItemStatus: {s}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkItem {
    pub id: i64,
    pub slug: String,
    pub title: String,
    pub repos: Vec<String>,
    pub status: WorkItemStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub dormant_since: Option<DateTime<Utc>>,
    pub sessions_count: u32,
    pub modes_present: Vec<String>,
}

/// One cluster assignment output by the cluster LLM. A row in
/// `cluster_assignments` corresponds to one [`Assignment`]. The
/// `AssignmentKind` is flattened so the wire YAML uses
/// `kind: existing|new` plus the matching `slug` / `title` field at the
/// same indent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Assignment {
    pub first_turn_uuid: String,
    pub last_turn_uuid: String,
    #[serde(flatten)]
    pub kind: AssignmentKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum AssignmentKind {
    Existing { slug: String },
    New { title: String },
}

/// Convert a human-friendly title into a kebab-case slug. Lowercases,
/// strips punctuation, collapses runs of non-alphanumeric chars into a
/// single `-`, trims leading/trailing dashes, and caps length at 80.
pub fn derive_slug(title: &str) -> String {
    log::debug!("derive_slug: title_len={}", title.len());
    let mut out = String::with_capacity(title.len());
    let mut prev_dash = true; // suppress leading dashes
    for c in title.chars() {
        let mapped = if c.is_ascii_alphanumeric() {
            Some(c.to_ascii_lowercase())
        } else if c.is_whitespace() || c == '-' || c == '_' || c == '.' || c == '/' {
            Some('-')
        } else {
            None
        };
        match mapped {
            Some('-') => {
                if !prev_dash {
                    out.push('-');
                    prev_dash = true;
                }
            }
            Some(c) => {
                out.push(c);
                prev_dash = false;
            }
            None => {}
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.len() > 80 {
        out.truncate(80);
        while out.ends_with('-') {
            out.pop();
        }
    }
    if out.is_empty() {
        return "untitled".to_string();
    }
    out
}

#[cfg(test)]
mod tests;
