//! `sb cortex hub --asymmetry` (Phase 3 of
//! `docs/design/2026-08-15-entity-hub-two-vector-synthesis.md`).
//!
//! Answers the design's original question - "what have I read about X but
//! never applied" - at zero LLM cost: per hub, split its DELIBERATE inbound
//! membership into source-vector and session-vector counts, then classify the
//! hub into one of four buckets. The membership query is
//! `SearchIndex::hub_members_deliberate` - the EXACT filter Phase 2's body
//! builder reads from (deliberate kinds only, no `entities/%` src) - reused
//! verbatim, not re-derived: a second membership query could silently drift
//! from the builder's and report a different reality than the one the hub
//! bodies were built from.
//!
//! Read-only by construction: every call below is a `SELECT` or a filesystem
//! read (`hub_members_deliberate`, `load_hub_member`). Nothing here writes a
//! note, an edge, or an `entities` row.

use std::path::Path;

use eyre::Result;
use vault::search::SearchIndex;

use super::{HubStub, Vector, load_hub_member};

/// Which of the four asymmetry buckets a hub falls into, classified purely
/// from its deliberate source/session membership counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsymmetryBucket {
    /// At least one deliberate source member AND one deliberate session
    /// member: read about it, AND applied it in a session.
    Both,
    /// At least one deliberate source member, zero session members: read
    /// about it, never applied it in a session.
    LearnedNotApplied,
    /// At least one deliberate session member, zero source members: applied
    /// it in a session, never read external content about it.
    AppliedNotRead,
    /// Zero deliberate source AND zero deliberate session members. The hub
    /// may still carry deliberate members of neither vector (an `image` note,
    /// say), or none at all.
    Unlinked,
}

impl AsymmetryBucket {
    fn classify(sources: usize, sessions: usize) -> Self {
        match (sources > 0, sessions > 0) {
            (true, true) => Self::Both,
            (true, false) => Self::LearnedNotApplied,
            (false, true) => Self::AppliedNotRead,
            (false, false) => Self::Unlinked,
        }
    }

    /// The bucket name as printed by `sb cortex hub --asymmetry` (exactly the
    /// four labels the design doc names).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Both => "both",
            Self::LearnedNotApplied => "learned-not-applied",
            Self::AppliedNotRead => "applied-not-read",
            Self::Unlinked => "unlinked",
        }
    }
}

/// One hub's row in the asymmetry report.
#[derive(Debug, Clone, PartialEq)]
pub struct AsymmetryRow {
    pub hub_path: String,
    pub title: String,
    /// Deliberate source-vector (youtube/article/github/social/research)
    /// member count.
    pub sources: usize,
    /// Deliberate session-vector member count.
    pub sessions: usize,
    pub bucket: AsymmetryBucket,
}

/// Per-bucket totals across a report. `total()` is asserted (by test) to
/// equal the number of hubs the report covers - the "four buckets sum to the
/// hub count" success criterion.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AsymmetryTotals {
    pub both: usize,
    pub learned_not_applied: usize,
    pub applied_not_read: usize,
    pub unlinked: usize,
}

impl AsymmetryTotals {
    pub fn total(&self) -> usize {
        self.both + self.learned_not_applied + self.applied_not_read + self.unlinked
    }
}

/// The full report: one row per materialized hub, sorted by path for
/// determinism (a stubbed-but-not-yet-materialized hub has no file to read
/// membership from and is excluded, same as the body builder's `abs.exists()`
/// gate).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AsymmetryReport {
    pub rows: Vec<AsymmetryRow>,
}

impl AsymmetryReport {
    pub fn totals(&self) -> AsymmetryTotals {
        let mut t = AsymmetryTotals::default();
        for row in &self.rows {
            match row.bucket {
                AsymmetryBucket::Both => t.both += 1,
                AsymmetryBucket::LearnedNotApplied => t.learned_not_applied += 1,
                AsymmetryBucket::AppliedNotRead => t.applied_not_read += 1,
                AsymmetryBucket::Unlinked => t.unlinked += 1,
            }
        }
        t
    }

    /// Deterministic text rendering for `sb cortex hub --asymmetry`. `sb`
    /// prints this; the library never touches stdout (`sb/AGENTS.md`: stdio
    /// belongs to `sb`). Pure function of the report's own data, so calling it
    /// twice against unchanged state produces byte-identical text (Phase 3
    /// success criterion: two runs produce byte-identical output).
    pub fn render(&self) -> String {
        let t = self.totals();
        let mut out = format!(
            "asymmetry: both={} learned-not-applied={} applied-not-read={} unlinked={} (hubs={})\n",
            t.both,
            t.learned_not_applied,
            t.applied_not_read,
            t.unlinked,
            self.rows.len(),
        );
        for row in &self.rows {
            out.push_str(&format!(
                "  {:<20} sources={:<4} sessions={:<4} {}\n",
                row.bucket.as_str(),
                row.sources,
                row.sessions,
                row.hub_path,
            ));
        }
        out
    }
}

/// Build the report: one row per hub that actually exists on disk, classified
/// from its DELIBERATE membership only via `hub_members_deliberate` - the
/// same query Phase 2's body builder reads from, never a second one.
///
/// Read-only: only issues `SELECT`s against `index` and reads member note
/// files off disk; writes nothing to the vault or the index.
pub fn build_asymmetry_report(vault_root: &Path, stubs: &[HubStub], index: &SearchIndex) -> Result<AsymmetryReport> {
    let mut rows = Vec::new();
    for stub in stubs {
        let hub_path = stub.hub_path();
        if !vault_root.join(&hub_path).exists() {
            continue;
        }
        let (sources, sessions) = classify_membership(vault_root, index, &hub_path)?;
        rows.push(AsymmetryRow {
            bucket: AsymmetryBucket::classify(sources, sessions),
            hub_path,
            title: stub.title.clone(),
            sources,
            sessions,
        });
    }
    rows.sort_by(|a, b| a.hub_path.cmp(&b.hub_path));
    Ok(AsymmetryReport { rows })
}

/// Split one hub's deliberate membership into `(source_count, session_count)`.
/// A member that fails to load is skipped and logged, never aborts the
/// report - every hub still lands in exactly one of the four buckets.
fn classify_membership(vault_root: &Path, index: &SearchIndex, hub_path: &str) -> Result<(usize, usize)> {
    let mut sources = 0usize;
    let mut sessions = 0usize;
    for member_rel in index.hub_members_deliberate(hub_path)? {
        match load_hub_member(vault_root, &member_rel) {
            Ok(member) => match Vector::of(&member.note_type) {
                Vector::Source => sources += 1,
                Vector::Session => sessions += 1,
                Vector::Other => {}
            },
            Err(e) => {
                log::warn!("cortex::hub::asymmetry: skipping unreadable member {member_rel} of {hub_path} ({e:#})");
            }
        }
    }
    Ok((sources, sessions))
}

#[cfg(test)]
mod tests;
