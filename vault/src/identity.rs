//! Note identity across staged traces, owned here because the mechanics are
//! shared and independent copies drift.
//!
//! A staged trace must resolve to the ONE vault note it produced, or the
//! proposal scanner counts one note twice. Three facts make that non-trivial:
//!
//! - The note `trace:` frontmatter is a ONE-WAY map, note to trace. A trace
//!   that was superseded by a reingest has no note pointing back at it.
//! - Reingest leaves the OLD note carrying the same `trace:`, so one trace can
//!   match several notes. `superseded-by` (see [`crate::tombstone`]) is what
//!   picks the survivor.
//! - Receipt `note_path` values are a location snapshot taken at ingest, not
//!   an identity: `cortex classify` moves a note from `inbox/` to `notes/`
//!   without updating the receipt.
//!
//! This module owns the vault-side half: the index, the `trace:` back-edge and
//! the `superseded-by` convergence. The receipts-side tiers live with their
//! caller, because `vault` must not learn borg's schema.
//!
//! Deliberately NOT reusing `borg::harvest::identity`: that is a
//! publish-replacement policy taking a `ResolveIntent`, a source and a body
//! hash, with a process-lifetime cache, and its strict trace guard rejects the
//! historical traces a proposal scan must reconcile. cortex also has no borg
//! dependency and must not gain one - that inverts the one-way data flow.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::note::Note;
use crate::tombstone::SUPERSEDED_BY_KEY;

/// A read-only index over a scanned vault, built once per scan.
///
/// Borrows the notes rather than cloning them: a vault scan is thousands of
/// notes and this is built on every sweep tick.
pub struct NoteIndex<'a> {
    by_trace: HashMap<&'a str, Vec<&'a Note>>,
    by_stem: HashMap<&'a str, Vec<&'a Note>>,
    paths: HashSet<&'a Path>,
    /// Filename stems present in the vault, for testing whether a
    /// `superseded-by` target actually resolves.
    stems: HashSet<&'a str>,
}

impl<'a> NoteIndex<'a> {
    pub fn build(notes: &'a [Note]) -> Self {
        let mut by_trace: HashMap<&'a str, Vec<&'a Note>> = HashMap::new();
        let mut by_stem: HashMap<&'a str, Vec<&'a Note>> = HashMap::new();
        let mut paths: HashSet<&'a Path> = HashSet::new();
        let mut stems: HashSet<&'a str> = HashSet::new();

        for note in notes {
            if let Some(trace) = note.frontmatter.trace.as_deref() {
                by_trace.entry(trace).or_default().push(note);
            }
            if let Some(stem) = note.path.file_stem().and_then(|s| s.to_str()) {
                by_stem.entry(stem).or_default().push(note);
                stems.insert(stem);
            }
            paths.insert(note.path.as_path());
        }

        log::debug!(
            "identity::NoteIndex::build: notes={} traces={} stems={}",
            notes.len(),
            by_trace.len(),
            by_stem.len()
        );

        Self {
            by_trace,
            by_stem,
            paths,
            stems,
        }
    }

    /// The single non-superseded note carrying this `trace:`, or `None`.
    ///
    /// `None` covers three distinct situations on purpose, because the caller
    /// treats them identically (fall through to the next tier): no note claims
    /// the trace, more than one survivor claims it, or the `superseded-by`
    /// chain is a cycle so nothing survives.
    ///
    /// A `superseded-by` naming a stem that is NOT in the vault is ignored and
    /// that candidate stays in the running - a dangling tombstone marker must
    /// not silently retire a note that is the only thing left.
    pub fn resolve_trace(&self, trace: &str) -> Option<&'a Note> {
        let candidates = self.by_trace.get(trace)?;
        if candidates.len() == 1 {
            return Some(candidates[0]);
        }

        let survivors: Vec<&&'a Note> = candidates
            .iter()
            .filter(|note| match superseded_target(note) {
                Some(target) => !self.stems.contains(target),
                None => true,
            })
            .collect();

        match survivors.as_slice() {
            [only] => Some(**only),
            _ => {
                log::debug!(
                    "identity::resolve_trace: trace={trace} candidates={} survivors={} -> unresolved",
                    candidates.len(),
                    survivors.len()
                );
                None
            }
        }
    }

    /// True when this vault-relative path is a note in the scan.
    pub fn contains_path(&self, relative: &Path) -> bool {
        self.paths.contains(relative)
    }

    /// The single note with this filename stem, or `None` when zero or several
    /// share it. 38 stems collide vault-wide, so "exactly one" is the contract.
    pub fn unique_by_stem(&self, stem: &str) -> Option<&'a Note> {
        match self.by_stem.get(stem)?.as_slice() {
            [only] => Some(only),
            _ => None,
        }
    }
}

/// The stem a note's `superseded-by` points at, when it carries one.
///
/// Tombstones store a filename STEM, not a path (see [`crate::tombstone`]).
fn superseded_target(note: &Note) -> Option<&str> {
    note.frontmatter.extra.get(SUPERSEDED_BY_KEY).and_then(|v| v.as_str())
}

#[cfg(test)]
mod tests;
