//! Gem: multi-turn dialog-slice unit of capture for facet v2.
//!
//! A gem is the four-part anatomy from the Shopify-CEO talk (task,
//! context, interaction, review) carrying VERBATIM AI output alongside
//! Scott's. See design doc
//! `docs/design/2026-05-26-facet-v2-gems-and-narrative-spectra.md`.
//!
//! This module owns only the struct definitions and the `content_hash`
//! identity calculation. Persistence (upsert against the `gems` and
//! `interaction_turns` tables created by `bin/migrate-facet-v2.sh`)
//! lands in Phase 3.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[cfg(test)]
mod tests;

/// One gem: a multi-turn dialog slice covering one apprenticeship
/// recipe. `id` is 0 for not-yet-persisted gems; the ledger fills it
/// in on upsert.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gem {
    #[serde(default)]
    pub id: i64,
    pub workitem_id: i64,
    pub session_uuid: String,
    pub task: String,
    #[serde(default)]
    pub context_loaded: Vec<String>,
    #[serde(default)]
    pub context_missing: Vec<String>,
    pub interaction: Vec<InteractionTurn>,
    #[serde(default)]
    pub review: Review,
    #[serde(default)]
    pub tags: Vec<String>,
    pub why_it_matters: String,
    pub extractor_model: String,
    pub extracted_at: DateTime<Utc>,
}

/// One turn inside a gem's interaction. `ai_says` and `user_says` are
/// verbatim; tool-result `user_says` values over 800 chars are replaced
/// with a `<tool-result: N lines, $tool>` placeholder at extract time
/// per the v2 pattern (see `facet/patterns/facet-extract-v2.md` and the
/// risk-table entry "Tool-result blowout").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractionTurn {
    pub ai_says: String,
    pub ai_turn_uuid: String,
    pub user_says: String,
    pub user_turn_uuid: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Review fields are all optional; only those evidenced in the slice
/// are populated.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Review {
    #[serde(default)]
    pub accepted: Option<String>,
    #[serde(default)]
    pub rejected: Option<String>,
    #[serde(default)]
    pub verified_manually: Option<String>,
    #[serde(default)]
    pub rewrote_by_hand: Option<String>,
}

impl Gem {
    /// Compute the identity hash for this gem.
    ///
    /// Sorts the AI and user turn UUIDs across every interaction turn,
    /// joins with `|`, sha256s, and hex-encodes. The result is the
    /// `content_hash` column in the `gems` table and participates in
    /// the `UNIQUE (workitem_id, content_hash)` idempotency key.
    ///
    /// Sorting the UUIDs makes the hash stable against chunker-boundary
    /// shifts that don't change the set of turns covered.
    pub fn content_hash(&self) -> String {
        let mut uuids: Vec<&str> = Vec::with_capacity(self.interaction.len() * 2);
        for turn in &self.interaction {
            uuids.push(turn.ai_turn_uuid.as_str());
            uuids.push(turn.user_turn_uuid.as_str());
        }
        uuids.sort_unstable();
        let mut hasher = Sha256::new();
        for (idx, uuid) in uuids.iter().enumerate() {
            if idx > 0 {
                hasher.update(b"|");
            }
            hasher.update(uuid.as_bytes());
        }
        hex::encode(hasher.finalize())
    }

    /// Boundary user-turn UUIDs for inspection. Stored as `gems`
    /// columns but does not participate in the idempotency key.
    /// Returns `None` if the gem has no interaction turns (which is
    /// invalid; a gem must have at least 2 turns per the v2 pattern).
    pub fn boundary_user_turn_uuids(&self) -> Option<(&str, &str)> {
        let first = self.interaction.first()?;
        let last = self.interaction.last()?;
        Some((first.user_turn_uuid.as_str(), last.user_turn_uuid.as_str()))
    }
}
