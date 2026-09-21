//! Open-vocabulary tag candidates: read them, reconcile note identity, and
//! aggregate the two arms into one proposal list.
//!
//! The note-derived arm (a tag hand-typed in Obsidian) is structurally empty
//! under tags-only: `filter_and_cap` drops every unresolvable candidate at
//! publish, so no note can carry a novel tag. The signal that is left lives in
//! borg's staged `distilled.yml` files, which `write_distilled_yml` writes
//! BEFORE the canonical filter runs - 12 of the 26 deployed fabric patterns
//! still instruct the model to propose freely.
//!
//! This is cortex's SECOND read-only edge into borg's staging root; the first
//! is `cortex::embed::read_staged_transcript`. borg remains the sole staging
//! writer and nothing here opens a borg writer path.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use eyre::{Context, Result};
use rayon::prelude::*;
use serde::Deserialize;

use vault::canonical::{self, CanonicalSet, TagMapping};
use vault::identity::NoteIndex;
use vault::note::Note;

use crate::sweep::{Proposal, ProposalSource};

/// Sample provenance cap, matching `cortex::entities::MAX_SAMPLE_NOTES`.
pub const MAX_SAMPLE_SOURCES: usize = 5;

/// One staged trace's raw, pre-filter candidate tags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedCandidates {
    pub trace: String,
    pub tags: Vec<String>,
}

/// What one walk of the staging root found.
///
/// Carries the counts as well as the candidates because the caller reports
/// them and because `scanned == false` (no staging root on this host) must be
/// distinguishable from "scanned and found nothing".
#[derive(Debug, Clone, Default)]
pub struct StagedScan {
    pub candidates: Vec<StagedCandidates>,
    /// False when the staging root does not exist: a client-only host that
    /// never had staging is not an error.
    pub scanned: bool,
    /// Trace directories carrying no `distilled.yml`. The ordinary case
    /// (45,637 of 46,296 on the daemon host), counted and never logged.
    pub without_distilled: usize,
    /// Files that exist but could not be read or parsed. One WARN each.
    pub unreadable: usize,
    /// Oldest and newest `meta.produced-at` seen, so a reader of the queue can
    /// tell what window the frequencies cover.
    pub window: Option<(String, String)>,
}

impl StagedScan {
    /// `"<oldest> .. <newest>"`, the form written to `staged-window`.
    pub fn window_label(&self) -> Option<String> {
        self.window.as_ref().map(|(lo, hi)| format!("{lo} .. {hi}"))
    }
}

/// The subset of `distilled.yml` this scan needs.
///
/// Deliberately NOT `vault::distilled::Distilled`: the full type pulls every
/// per-kind payload and the transcript, and 10.6 MB of YAML is parsed on each
/// sweep tick. Unknown keys are ignored by design - this is a narrow read of
/// someone else's artifact, not a schema the scanner owns.
#[derive(Debug, Deserialize)]
struct StagedDistilled {
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    meta: Option<StagedMeta>,
}

#[derive(Debug, Deserialize)]
struct StagedMeta {
    #[serde(rename = "produced-at", default)]
    produced_at: Option<String>,
}

/// Walk `<staging_root>/*/distilled.yml` and return each one's raw `tags`.
///
/// A missing staging root is `Ok` with `scanned: false` - a client-only host
/// POSTs to the daemon and owns no staging tree, and that is not a failure.
/// A root that EXISTS but cannot be enumerated is an `Err`, because the caller
/// overwrites the proposal queue unconditionally: "could not look" must never
/// be written out as "nothing found".
pub fn read_staged_candidates(staging_root: &Path) -> Result<StagedScan> {
    log::debug!(
        "proposals::read_staged_candidates: staging_root={}",
        staging_root.display()
    );

    // NOT `Path::exists()`: it maps EVERY error to false, including EACCES on
    // a parent component. That turns "cannot look" into "not scanned", and
    // because the write is unconditional it replaces a populated queue with
    // `proposals: []` and exits 0. Only NotFound is a skip.
    match std::fs::metadata(staging_root) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            log::info!(
                "proposals::read_staged_candidates: no staging root at {}, skipping the staged arm",
                staging_root.display()
            );
            return Ok(StagedScan::default());
        }
        Err(e) => {
            return Err(eyre::eyre!(
                "failed to stat staging root {}: {e}",
                staging_root.display()
            ));
        }
    }

    let entries: Vec<std::path::PathBuf> = std::fs::read_dir(staging_root)
        .with_context(|| format!("failed to enumerate staging root {}", staging_root.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("failed to read staging root {}", staging_root.display()))?
        .into_iter()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.path())
        .collect();

    #[derive(Default)]
    struct Partial {
        candidates: Vec<StagedCandidates>,
        without_distilled: usize,
        unreadable: usize,
        window: Option<(String, String)>,
    }

    let partial = entries
        .par_iter()
        .filter_map(|dir| {
            let trace = dir.file_name()?.to_str()?.to_string();
            let path = dir.join("distilled.yml");
            if !path.exists() {
                return Some((None, 1usize, 0usize, None));
            }
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    log::warn!("proposals: unreadable staged distilled.yml at {}: {e}", path.display());
                    return Some((None, 0, 1, None));
                }
            };
            let parsed: StagedDistilled = match serde_yaml::from_str(&text) {
                Ok(p) => p,
                Err(e) => {
                    log::warn!("proposals: unparseable staged distilled.yml at {}: {e}", path.display());
                    return Some((None, 0, 1, None));
                }
            };

            // One trace votes AT MOST ONCE per tag: dedupe within the trace and
            // normalize the text the same way the vault does.
            let mut seen = HashSet::new();
            let mut tags = Vec::new();
            for raw in &parsed.tags {
                let tag = vault::hygiene::sanitize_tag(raw);
                if tag.is_empty() {
                    continue;
                }
                if seen.insert(tag.clone()) {
                    tags.push(tag);
                }
            }

            let produced = parsed.meta.and_then(|m| m.produced_at);
            Some((Some(StagedCandidates { trace, tags }), 0, 0, produced))
        })
        .fold(Partial::default, |mut acc, (cand, missing, bad, produced)| {
            if let Some(c) = cand {
                acc.candidates.push(c);
            }
            acc.without_distilled += missing;
            acc.unreadable += bad;
            if let Some(ts) = produced {
                acc.window = Some(match acc.window.take() {
                    Some((lo, hi)) => (lo.min(ts.clone()), hi.max(ts)),
                    None => (ts.clone(), ts),
                });
            }
            acc
        })
        .reduce(Partial::default, |mut a, b| {
            a.candidates.extend(b.candidates);
            a.without_distilled += b.without_distilled;
            a.unreadable += b.unreadable;
            a.window = match (a.window, b.window) {
                (Some((alo, ahi)), Some((blo, bhi))) => Some((alo.min(blo), ahi.max(bhi))),
                (Some(w), None) | (None, Some(w)) => Some(w),
                (None, None) => None,
            };
            a
        });

    log::info!(
        "proposals::read_staged_candidates: traces={} with_distilled={} without={} unreadable={}",
        entries.len(),
        partial.candidates.len(),
        partial.without_distilled,
        partial.unreadable
    );

    Ok(StagedScan {
        candidates: partial.candidates,
        scanned: true,
        without_distilled: partial.without_distilled,
        unreadable: partial.unreadable,
        window: partial.window,
    })
}

/// One receipts row, reduced to what identity reconciliation needs.
struct Receipt {
    trace_id: String,
    note_path: Option<String>,
    raw_input: String,
}

/// trace id -> VAULT-RELATIVE note path, reconciled through four tiers.
///
/// Every key it emits is in the same path space as `Note.path`, so the
/// note-derived and staged arms cannot double-count one note.
///
/// | Tier | Rule |
/// |---|---|
/// | 1 | a vault note whose frontmatter `trace:` is this trace, converged through `superseded-by` |
/// | 2 | the recorded `note_path`, relativized, still exists in the vault |
/// | 3 | exactly one vault note whose filename stem equals the recorded basename AND whose `source` matches the receipt's |
/// | 4 | none of the above: the trace keys on itself, `trace:<id>` (the caller's fallback, not emitted here) |
///
/// The vault's own back-edge OUTRANKS the receipt: an `inbox/` to `notes/`
/// move vacates a path and a later ingest can land a different note on it, so
/// a receipts-first order would name the wrong note. Measured on the live
/// corpus: across the 21 traces both tiers resolve, 14 agree and zero
/// disagree, so the ordering is free today and removes the class outright.
///
/// An unreadable DB is an `Err`, not an empty map: silently resolving nothing
/// restores exactly the double-counting this removes, and the operator sees a
/// clean scan.
pub fn resolve_trace_notes(receipts_db: &Path, vault_root: &Path, notes: &[Note]) -> Result<HashMap<String, String>> {
    log::debug!(
        "proposals::resolve_trace_notes: db={} vault_root={} notes={}",
        receipts_db.display(),
        vault_root.display(),
        notes.len()
    );

    let index = NoteIndex::build(notes);
    let receipts = read_receipts(receipts_db)?;
    let mut resolved: HashMap<String, String> = HashMap::new();
    let (mut t1, mut t2, mut t3) = (0usize, 0usize, 0usize);

    for receipt in &receipts {
        // Tier 1: the vault's own back-edge, converged through `superseded-by`.
        if let Some(note) = index.resolve_trace(&receipt.trace_id) {
            resolved.insert(receipt.trace_id.clone(), note.path.to_string_lossy().to_string());
            t1 += 1;
            continue;
        }

        let Some(recorded) = receipt.note_path.as_deref() else {
            continue;
        };
        let recorded = Path::new(recorded);
        let relative = recorded.strip_prefix(vault_root).unwrap_or(recorded);

        // Tier 2: the recorded path, relativized, still exists in the vault.
        if index.contains_path(relative) {
            resolved.insert(receipt.trace_id.clone(), relative.to_string_lossy().to_string());
            t2 += 1;
            continue;
        }

        // Tier 3: a UNIQUE basename match whose `source` agrees with the
        // receipt's. The source cross-check is not optional: a recorded
        // basename can now belong to a different note entirely, and "exactly
        // one basename match" alone would name it.
        let Some(stem) = relative.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if let Some(note) = index.unique_by_stem(stem)
            && note.frontmatter.source.as_deref() == Some(receipt.raw_input.as_str())
        {
            resolved.insert(receipt.trace_id.clone(), note.path.to_string_lossy().to_string());
            t3 += 1;
        }
    }

    log::info!(
        "proposals::resolve_trace_notes: receipts={} resolved={} (tier1={t1} tier2={t2} tier3={t3})",
        receipts.len(),
        resolved.len()
    );
    Ok(resolved)
}

/// Read borg's receipts DB READ-ONLY, the way `oracle`'s `failure_history`
/// does. cortex never opens a borg writer path.
fn read_receipts(receipts_db: &Path) -> Result<Vec<Receipt>> {
    let conn = rusqlite::Connection::open_with_flags(
        receipts_db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|e| eyre::eyre!("failed to open receipts DB {}: {e}", receipts_db.display()))?;

    let mut stmt = conn
        .prepare("SELECT trace_id, note_path, raw_input FROM receipts")
        .map_err(|e| eyre::eyre!("failed to query receipts DB {}: {e}", receipts_db.display()))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(Receipt {
                trace_id: row.get(0)?,
                note_path: row.get(1)?,
                raw_input: row.get(2)?,
            })
        })
        .map_err(|e| eyre::eyre!("failed to read receipts DB {}: {e}", receipts_db.display()))?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| eyre::eyre!("failed to read a receipts row: {e}"))?);
    }
    Ok(out)
}

/// Union both candidate arms into one proposal list. Pure: no I/O.
///
/// Counts DISTINCT SOURCE KEYS, where a key is the vault-relative note path
/// when one is known and `trace:<id>` when it is not. A staged trace that
/// resolves to a note collapses with the note-derived entry for that same
/// note, so the two arms cannot double-count one note - and two staged traces
/// for one note count once, which is what removes `system-prompt` (it reached
/// 3 only by counting `notes/pi-coding-agent-free-course.md` twice).
///
/// Drops any candidate that resolves through a canonical tier, and any the
/// human already rejected. Ordering is deterministic: frequency descending,
/// then tag ascending.
pub fn aggregate(
    note_candidates: &[(String, Vec<String>)],
    staged: &[StagedCandidates],
    trace_to_note: &HashMap<String, String>,
    canon: &CanonicalSet,
    mapping: &TagMapping,
    threshold: usize,
) -> Vec<Proposal> {
    // tag -> (source keys, which arms contributed)
    let mut acc: HashMap<String, (HashSet<String>, bool, bool)> = HashMap::new();

    let mut record = |tag: &str, key: String, from_note: bool| {
        if canonical::is_rejected(tag, mapping) {
            return;
        }
        if !canonical::match_to_canonical(tag, canon, mapping).is_empty() {
            return;
        }
        let entry = acc
            .entry(tag.to_string())
            .or_insert_with(|| (HashSet::new(), false, false));
        entry.0.insert(key);
        if from_note {
            entry.1 = true;
        } else {
            entry.2 = true;
        }
    };

    for (note_path, tags) in note_candidates {
        for tag in tags {
            record(tag, note_path.clone(), true);
        }
    }

    for staged in staged {
        // The resolved note path where identity reconciliation found one, else
        // the trace keys on itself. This is the whole dedupe mechanism.
        let key = trace_to_note
            .get(&staged.trace)
            .cloned()
            .unwrap_or_else(|| format!("trace:{}", staged.trace));
        for tag in &staged.tags {
            record(tag, key.clone(), false);
        }
    }

    let mut proposals: Vec<Proposal> = acc
        .into_iter()
        .filter(|(_, (keys, _, _))| keys.len() >= threshold)
        .map(|(tag, (keys, from_note, from_staged))| {
            let mut sources: Vec<String> = keys.into_iter().collect();
            sources.sort();
            let frequency = sources.len();
            sources.truncate(MAX_SAMPLE_SOURCES);
            Proposal {
                tag,
                frequency,
                sources,
                source: match (from_note, from_staged) {
                    (true, true) => ProposalSource::Both,
                    (true, false) => ProposalSource::Note,
                    (false, _) => ProposalSource::Staged,
                },
            }
        })
        .collect();

    proposals.sort_by(|a, b| b.frequency.cmp(&a.frequency).then_with(|| a.tag.cmp(&b.tag)));
    log::info!(
        "proposals::aggregate: {} candidate(s) at threshold {threshold}",
        proposals.len()
    );
    proposals
}

#[cfg(test)]
mod tests;

// ---------------------------------------------------------------------------
// Promotion: a pending proposal becomes a canonical tag, as a reviewable diff.
// ---------------------------------------------------------------------------

/// Outcome of one `tag-promote` invocation. Three branches, mirroring
/// `cortex::entities::PromoteReport`: already-present, applied, dry-run.
#[derive(Debug, Clone)]
pub struct TagPromoteReport {
    pub tags: Vec<String>,
    pub group: String,
    pub applied: bool,
    /// Tags that were already canonical, so nothing was done for them.
    pub already_present: Vec<String>,
    pub diff: String,
}

/// Promote pending tag proposals into a group of `canonical-tags.yml`.
///
/// Dry-run by default: prints nothing (this is a library), writes nothing, and
/// returns the diff. `apply` writes the vocabulary and drops the promoted
/// entries from the queue.
///
/// **Membership is checked BEFORE traceability.** `promote_concept` bails on
/// "no pending proposal" before its already-present no-op, so a second
/// `--apply` of the same slug exits non-zero: the first apply removed the very
/// proposal it then demands. Checking membership first makes a repeat apply an
/// idempotent no-op, which is what an operator re-running a batch expects.
pub fn promote_tags(
    proposals_path: &Path,
    canonical_path: &Path,
    tags: &[String],
    group: &str,
    apply: bool,
) -> Result<TagPromoteReport> {
    log::debug!(
        "proposals::promote_tags: tags={tags:?} group={group} apply={apply} canonical={} proposals={}",
        canonical_path.display(),
        proposals_path.display()
    );

    let text = std::fs::read_to_string(canonical_path)
        .with_context(|| format!("failed to read {}", canonical_path.display()))?;
    let file: vault::canonical::CanonicalTagsFile =
        serde_yaml::from_str(&text).with_context(|| format!("failed to parse {}", canonical_path.display()))?;

    if !file.tags.contains_key(group) {
        let mut groups: Vec<&String> = file.tags.keys().collect();
        groups.sort();
        eyre::bail!(
            "group {group:?} is not in {}; valid groups: {groups:?}",
            canonical_path.display()
        );
    }

    let existing = file.all_tags();
    let queue = load_queue(proposals_path)?;

    let mut already_present = Vec::new();
    let mut to_add = Vec::new();
    for tag in tags {
        if !vault::canonical::is_kebab_tag(tag) {
            eyre::bail!("tag {tag:?} is not ^[a-z0-9]+(-[a-z0-9]+)*$");
        }
        // Membership first: a repeat --apply of an already-promoted tag is a
        // no-op, not a failure.
        if existing.contains(tag) {
            already_present.push(tag.clone());
            continue;
        }
        if !queue.iter().any(|p| &p.tag == tag) {
            eyre::bail!(
                "no proposal with tag {tag:?} in {} - promotion must trace to a pending proposal",
                proposals_path.display()
            );
        }
        // Dedupe: `tag-promote ci ci --apply` would otherwise write two
        // identical `    - ci` lines and break the uniqueness invariant Phase
        // 7 itself introduced into the shipped-file test.
        if !to_add.contains(tag) {
            to_add.push(tag.clone());
        }
    }

    let projected = existing.len() + to_add.len();
    if projected > file.max_canonical {
        eyre::bail!(
            "promoting {} tag(s) would take the vocabulary to {projected}, over max-canonical {} (currently {})",
            to_add.len(),
            file.max_canonical,
            existing.len()
        );
    }

    let mut diff = String::new();
    for tag in &to_add {
        diff.push_str(&format!("canonical-tags.yml:  + `{tag}` under group `{group}`\n"));
        diff.push_str(&format!("tag-proposals.yml:   - proposal `{tag}` (promoted)\n"));
    }
    for tag in &already_present {
        diff.push_str(&format!("canonical-tags.yml:    `{tag}` already canonical, no-op\n"));
    }

    if apply && !to_add.is_empty() {
        let updated = insert_tags_into_group(&text, group, &to_add)?;
        // Validate the CANDIDATE BUFFER, not the file on disk:
        // `CanonicalTagsFile::load` takes a path and would re-read the old
        // bytes, proving nothing about what is about to be written.
        serde_yaml::from_str::<vault::canonical::CanonicalTagsFile>(&updated).with_context(|| {
            format!(
                "the edited {} would not parse; refusing to write",
                canonical_path.display()
            )
        })?;
        vault::note::write_atomic(canonical_path, updated.as_bytes())
            .with_context(|| format!("failed to write {}", canonical_path.display()))?;

        let remaining: Vec<crate::sweep::Proposal> = queue.into_iter().filter(|p| !to_add.contains(&p.tag)).collect();
        overwrite_queue(proposals_path, remaining)?;
        log::info!("proposals::promote_tags: promoted {to_add:?} into group {group}");
    }

    Ok(TagPromoteReport {
        tags: to_add,
        group: group.to_string(),
        applied: apply,
        already_present,
        diff,
    })
}

fn load_queue(path: &Path) -> Result<Vec<crate::sweep::Proposal>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let file: crate::sweep::ProposalsFile =
        serde_yaml::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(file.proposals)
}

/// Rewrite the queue in place, preserving its scan header.
fn overwrite_queue(path: &Path, proposals: Vec<crate::sweep::Proposal>) -> Result<()> {
    let existing: crate::sweep::ProposalsFile = if path.exists() {
        serde_yaml::from_str(&std::fs::read_to_string(path)?)
            .with_context(|| format!("failed to parse {}", path.display()))?
    } else {
        crate::sweep::ProposalsFile::default()
    };
    let out = crate::sweep::ProposalsFile {
        scanned_at: existing.scanned_at,
        staged_window: existing.staged_window,
        proposals,
    };
    let yaml = serde_yaml::to_string(&out).wrap_err("failed to serialize proposals")?;
    vault::note::write_atomic(path, yaml.as_bytes()).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

/// Append `tags` to the end of `group`'s sequence, changing NOTHING else.
///
/// A targeted textual insert, not a serde round-trip: no round-trip preserves
/// sequence indentation, the empty-flow `  system: []` form, or comments, and
/// the phase's contract is that every line outside the touched group is
/// byte-identical.
///
/// Appends; never re-sorts. Neither the group keys nor the tags within a group
/// are ordered in the shipped file (the `ai` group opens `ai, agents, claude,
/// anthropic`), so "sorted position" has no defined meaning here and
/// re-sorting would produce a whole-file diff.
///
/// Two layouts are recognized, and anything else is a hard bail rather than a
/// guess: a block sequence (`  group:` followed by `    - tag` lines) and the
/// empty-flow form (`  group: []`), which is rewritten to block form.
fn insert_tags_into_group(text: &str, group: &str, tags: &[String]) -> Result<String> {
    let lines: Vec<&str> = text.lines().collect();
    let key_block = format!("  {group}:");
    let key_flow = format!("  {group}: []");

    let idx = lines
        .iter()
        .position(|l| *l == key_block || *l == key_flow)
        .ok_or_else(|| {
            eyre::eyre!("group {group:?} has no recognized `  {group}:` or `  {group}: []` line; refusing to guess")
        })?;

    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    let trailing_newline = text.ends_with('\n');

    if lines[idx] == key_flow {
        // Empty-flow group: rewrite the one line to block form, then append.
        out[idx] = key_block;
        for (offset, tag) in tags.iter().enumerate() {
            out.insert(idx + 1 + offset, format!("    - {tag}"));
        }
    } else {
        // Block sequence: find its LAST `    - ` entry and append after it.
        let mut last = None;
        for (i, line) in lines.iter().enumerate().skip(idx + 1) {
            if line.starts_with("    - ") {
                last = Some(i);
            } else if !line.trim().is_empty() {
                // Any non-entry, non-blank line ends this group.
                break;
            }
        }
        let after = last.ok_or_else(|| {
            eyre::eyre!("group {group:?} has no `    - ` entries and is not `[]`; refusing to guess its layout")
        })?;
        for (offset, tag) in tags.iter().enumerate() {
            out.insert(after + 1 + offset, format!("    - {tag}"));
        }
    }

    let mut joined = out.join("\n");
    if trailing_newline {
        joined.push('\n');
    }
    Ok(joined)
}
