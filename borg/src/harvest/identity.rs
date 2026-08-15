//! Note-identity resolution (design doc
//! `docs/design/2026-08-15-harvest-note-identity-trace-keyed-replace.md`).
//!
//! Every `sb borg replay` of a harvest session trace used to write a NEW vault
//! note, because the filename stem is the model-generated distill slug and the
//! model never reproduces the same slug twice. [`resolve_prior_note`] answers
//! "does this trace already have a landed note, and if so where does it live
//! right now" so the publish path (Phase 3) can write to that exact path
//! instead of minting a sibling.
//!
//! Resolution order (all four branches live here; Phase 3 wires the caller):
//! 1. **Receipts fast path** - the trace's recorded `note_path`.
//! 2. **Vault index** - a `trace -> Vec<PathBuf>` scan of the live vault (for
//!    when cortex moved the note and the receipts row is stale).
//! 3. **Crash-recovery fallback** (`ResolveIntent::NewNote` only) - no trace
//!    match, but a note exists with the same `source:` AND the same
//!    `harvest-body-hash:` (a lost watermark entry, not a lost note).
//! 4. **None** - publish new.
//!
//! Every candidate must pass the three-term confirmation guard
//! ([`guard_passes`]): `trace:` == trace_id AND `source:` == primary_source AND
//! `harvest-body-hash:` (equal OR absent - legacy notes predate the key). A
//! resolved note carrying `superseded-by:` (a cortex tombstone) is never
//! itself returned - [`follow_tombstone_chain`] walks to the live survivor,
//! refusing on ambiguity, a missing stem, a cycle, or an exceeded depth bound.
//!
//! **Index freshness: memoized for the process lifetime, self-insert on
//! write, NO TTL.** Within one process borg is the sole creator of harvest
//! notes, so the in-memory view is exact by construction. A prior draft of
//! this design specced a timestamped/TTL rebuild; it is WITHDRAWN (see the
//! Architecture section's "Index freshness" paragraph) because the vault is
//! 3,141 files and EVERY nightly `NewNote` publish is a step-1 miss by
//! construction, making any rebuild-on-miss policy ~140 full vault scans a
//! night. [`note_published`] is the Phase 3 self-insert hook.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use eyre::{Context, Result};
use rusqlite::Connection;

use vault::config::ScanConfig;
use vault::frontmatter::Frontmatter;
use vault::note::{self, Note};

use crate::receipts;

/// Frontmatter key for the new input-transcript hash (Data Model). Read here;
/// written by Phase 3.
const HARVEST_BODY_HASH_KEY: &str = "harvest-body-hash";

/// Frontmatter key cortex's merge executor writes on a soft-retire tombstone
/// (`cortex::association::tombstone_content`). A bare filename stem, no
/// extension, no directory.
const SUPERSEDED_BY_KEY: &str = "superseded-by";

/// Depth bound on a `superseded-by` chain (Architecture: "a depth bound of
/// 8"). Guards against a pathological long chain burning cycles even when no
/// literal cycle exists.
const MAX_TOMBSTONE_DEPTH: usize = 8;

/// Why we are publishing. Decides which resolution branches are legal --
/// `FollowUp` is never a replace, because notes are immutable once published
/// (`harvest/publish.rs` dispatches a follow-up as a brand new note).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveIntent {
    /// `sb borg replay <trace>`: the trace is authoritative, steps 1-2 only.
    Replay,
    /// Harvest planner said `NewNote`: steps 1-2, plus the crash-recovery
    /// fallback (step 3).
    NewNote,
    /// Harvest planner said `FollowUp`, or `--force`: never resolves. Returned
    /// unconditionally at the top of [`resolve_prior_note`] - load-bearing
    /// because `classify_reappearance` returns `FollowUp` on `--force` before
    /// consulting the body hash, so an unchanged `--force` re-harvest would
    /// otherwise match the confirmation guard exactly.
    FollowUp,
}

/// One vault-wide scan's worth of path indices, memoized per vault root for
/// the process lifetime. Built from [`vault::note::scan_vault`] once, then
/// kept current by [`note_published`]'s self-insert - never rebuilt on a
/// timer or a TTL.
#[derive(Debug, Default, Clone)]
struct VaultIndex {
    /// `trace:` -> every absolute path carrying it (normally 0 or 1; a 32-bit
    /// trace collision is what makes >1 possible, and the confirmation guard
    /// is what makes that non-destructive).
    trace_index: HashMap<String, Vec<PathBuf>>,
    /// Filename stem (no extension) -> every absolute path with that stem.
    /// Feeds the tombstone follower.
    stem_index: HashMap<String, Vec<PathBuf>>,
    /// `(source, harvest-body-hash)` -> every absolute path carrying BOTH keys
    /// with those exact values. Built in the same scan so the crash-recovery
    /// fallback (step 3) never pays a second full-vault walk. A note missing
    /// `harvest-body-hash:` is never a member of any bucket here (Data Model:
    /// "both keys are required; a note lacking the hash is not eligible").
    source_hash_index: HashMap<(String, String), Vec<PathBuf>>,
}

/// Process-lifetime cache, keyed by vault root so distinct roots (production
/// vs. per-test tempdirs) never share state.
static INDEX_CACHE: LazyLock<Mutex<HashMap<PathBuf, VaultIndex>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Resolve the note this publish should REPLACE, if any. Absolute path or
/// `None`. See the module doc for the four-branch resolution order.
///
/// Fails CLOSED on a receipts DB error (Concurrency and failure modes): the
/// caller must not publish a possibly-duplicate note just because the guard
/// against duplication itself errored.
pub fn resolve_prior_note(
    conn: &Connection,
    vault_root: &Path,
    trace_id: &str,
    primary_source: &str,
    body_hash: &str,
    intent: ResolveIntent,
) -> Result<Option<PathBuf>> {
    log::debug!("harvest::identity::resolve_prior_note: trace={trace_id} source={primary_source} intent={intent:?}");
    if matches!(intent, ResolveIntent::FollowUp) {
        log::debug!("harvest::identity::resolve_prior_note: FollowUp intent never resolves");
        return Ok(None);
    }

    let index = get_or_build_index(vault_root)?;

    // Step 1: receipts fast path.
    if let Some(receipt) = receipts::get(conn, trace_id)
        .with_context(|| format!("harvest::identity: receipts lookup for trace {trace_id}"))?
        && let Some(note_path) = receipt.note_path.as_deref()
        && !note_path.is_empty()
    {
        let candidate = PathBuf::from(note_path);
        if let Some(resolved) =
            try_resolve_candidate(vault_root, &candidate, trace_id, primary_source, body_hash, &index)
        {
            log::debug!(
                "harvest::identity::resolve_prior_note: trace={trace_id} resolved via receipts fast path -> {}",
                resolved.display()
            );
            return Ok(Some(resolved));
        }
    }

    // Step 2: vault index (trace -> paths), covers a stale/absent receipts row
    // (e.g. cortex moved the note between directories).
    if let Some(candidates) = index.trace_index.get(trace_id) {
        for candidate in candidates {
            if let Some(resolved) =
                try_resolve_candidate(vault_root, candidate, trace_id, primary_source, body_hash, &index)
            {
                log::debug!(
                    "harvest::identity::resolve_prior_note: trace={trace_id} resolved via vault index -> {}",
                    resolved.display()
                );
                return Ok(Some(resolved));
            }
        }
    }

    // Step 3: crash-recovery fallback. NewNote only (Architecture: the intent
    // gate encodes "a brand new note, never an overwrite of the prior one" for
    // FollowUp/--force; Replay never needs it because the trace itself is
    // authoritative there).
    if matches!(intent, ResolveIntent::NewNote)
        && let Some(candidates) = index
            .source_hash_index
            .get(&(primary_source.to_string(), body_hash.to_string()))
    {
        for candidate in candidates {
            if let Some(resolved) =
                try_resolve_crash_candidate(vault_root, candidate, primary_source, body_hash, &index)
            {
                log::info!(
                    "harvest::identity::resolve_prior_note: trace={trace_id} resolved via crash-recovery fallback (source+hash match, no trace) -> {}",
                    resolved.display()
                );
                return Ok(Some(resolved));
            }
        }
    }

    log::debug!("harvest::identity::resolve_prior_note: trace={trace_id} no prior note - publish new");
    Ok(None)
}

/// Self-insert hook for the no-TTL index (Phase 3 calls this on every
/// successful publish, replay included). Inserts `(trace_id, absolute_path)`
/// into the CURRENT process's memoized index for `vault_root`, if that index
/// has already been built; a no-op if it has not (the next
/// [`resolve_prior_note`] call will build it fresh from disk, which already
/// includes this publish).
pub fn note_published(vault_root: &Path, trace_id: &str, absolute_path: &Path) {
    let canon_root = canonical_or(vault_root);
    let mut cache = INDEX_CACHE.lock().expect("harvest identity index cache poisoned");
    if let Some(index) = cache.get_mut(&canon_root) {
        log::debug!(
            "harvest::identity::note_published: trace={trace_id} path={} (self-insert into live index)",
            absolute_path.display()
        );
        index
            .trace_index
            .entry(trace_id.to_string())
            .or_default()
            .push(absolute_path.to_path_buf());
    }
}

/// Test-only: drop the memoized index for `vault_root` so the next
/// [`resolve_prior_note`] rebuilds it from disk. Stands in for the NEXT
/// PROCESS, which is the only place an external mover (cortex promoting a note
/// out of `inbox/`) becomes visible to the index - within one process borg is
/// the sole creator of harvest notes and the self-insert keeps the view exact
/// (see the module doc's freshness contract).
#[cfg(test)]
pub(crate) fn reset_index_cache_for_tests(vault_root: &Path) {
    let canon_root = canonical_or(vault_root);
    let mut cache = INDEX_CACHE.lock().expect("harvest identity index cache poisoned");
    cache.remove(&canon_root);
}

fn canonical_or(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Build (or fetch the memoized) [`VaultIndex`] for `vault_root`. One
/// `scan_vault` per process per root - see the module doc's freshness
/// contract.
fn get_or_build_index(vault_root: &Path) -> Result<VaultIndex> {
    let canon_root = canonical_or(vault_root);
    {
        let cache = INDEX_CACHE.lock().expect("harvest identity index cache poisoned");
        if let Some(index) = cache.get(&canon_root) {
            return Ok(index.clone());
        }
    }
    log::debug!(
        "harvest::identity::get_or_build_index: building fresh index for {}",
        vault_root.display()
    );
    let notes = note::scan_vault(vault_root, &ScanConfig::default())
        .with_context(|| format!("harvest::identity: scan_vault {}", vault_root.display()))?;
    let index = build_index(vault_root, &notes);
    let mut cache = INDEX_CACHE.lock().expect("harvest identity index cache poisoned");
    cache.insert(canon_root, index.clone());
    Ok(index)
}

fn build_index(vault_root: &Path, notes: &[Note]) -> VaultIndex {
    let mut index = VaultIndex::default();
    for n in notes {
        let absolute = vault_root.join(&n.path);
        if let Some(trace) = &n.frontmatter.trace {
            index
                .trace_index
                .entry(trace.clone())
                .or_default()
                .push(absolute.clone());
        }
        if let Some(stem) = absolute.file_stem().and_then(|s| s.to_str()) {
            index
                .stem_index
                .entry(stem.to_string())
                .or_default()
                .push(absolute.clone());
        }
        if let (Some(source), Some(hash)) = (&n.frontmatter.source, body_hash_of(&n.frontmatter)) {
            index
                .source_hash_index
                .entry((source.clone(), hash))
                .or_default()
                .push(absolute.clone());
        }
    }
    index
}

/// The note's `harvest-body-hash:` value, if the key is present with a string
/// value. `None` for an absent key OR a non-string value - both mean "not
/// eligible" for the guard's/step 3's purposes.
fn body_hash_of(fm: &Frontmatter) -> Option<String> {
    fm.extra
        .get(HARVEST_BODY_HASH_KEY)
        .and_then(|v| v.as_str().map(str::to_string))
}

/// The note's `superseded-by:` value (a bare filename stem), if present.
fn superseded_by_of(fm: &Frontmatter) -> Option<String> {
    fm.extra
        .get(SUPERSEDED_BY_KEY)
        .and_then(|v| v.as_str().map(str::to_string))
}

/// The three-term confirmation guard (steps 1-2): `trace:` equals `trace_id`,
/// AND `source:` equals `primary_source`, AND `harvest-body-hash:` either
/// equals `body_hash` or is ABSENT (legacy notes predate the key).
fn guard_passes(fm: &Frontmatter, trace_id: &str, primary_source: &str, body_hash: &str) -> bool {
    let trace_ok = fm.trace.as_deref() == Some(trace_id);
    let source_ok = fm.source.as_deref() == Some(primary_source);
    let hash_ok = match body_hash_of(fm) {
        None => true,
        Some(existing) => existing == body_hash,
    };
    trace_ok && source_ok && hash_ok
}

/// Parse `candidate`, apply the confirmation guard, follow a tombstone chain
/// if the guard-passing note is itself a tombstone, then re-stat the final
/// path. Returns `None` on a missing file, a parse failure, a failed guard, or
/// a refused tombstone follow (ambiguous/missing/cycle/depth - all WARNed by
/// [`follow_tombstone_chain`]).
fn try_resolve_candidate(
    vault_root: &Path,
    candidate: &Path,
    trace_id: &str,
    primary_source: &str,
    body_hash: &str,
    index: &VaultIndex,
) -> Option<PathBuf> {
    let note = parse_existing(vault_root, candidate)?;
    if !guard_passes(&note.frontmatter, trace_id, primary_source, body_hash) {
        return None;
    }
    resolve_target(vault_root, candidate, &note, index)
}

/// Step-3 variant: no `trace:` term (there was no trace match by definition),
/// but `source:` and `harvest-body-hash:` must BOTH equal exactly (Data
/// Model: "both keys are required; a note lacking the hash is not eligible" -
/// enforced upstream by [`build_index`] never populating `source_hash_index`
/// for a hash-less note, so `candidate` reaching this function already
/// guarantees the key was present).
fn try_resolve_crash_candidate(
    vault_root: &Path,
    candidate: &Path,
    primary_source: &str,
    body_hash: &str,
    index: &VaultIndex,
) -> Option<PathBuf> {
    let note = parse_existing(vault_root, candidate)?;
    let source_ok = note.frontmatter.source.as_deref() == Some(primary_source);
    let hash_ok = body_hash_of(&note.frontmatter).as_deref() == Some(body_hash);
    if !(source_ok && hash_ok) {
        return None;
    }
    resolve_target(vault_root, candidate, &note, index)
}

/// Shared tail of both resolve paths: if `note` carries `superseded-by:`,
/// follow the chain to its live survivor; otherwise the candidate itself is
/// the target. Re-stats the FINAL path before returning (a stale index/receipt
/// entry, or a tombstone target that vanished between scan and lookup, falls
/// through to `None` rather than being handed back as a real path).
fn resolve_target(vault_root: &Path, candidate: &Path, note: &Note, index: &VaultIndex) -> Option<PathBuf> {
    let resolved = match superseded_by_of(&note.frontmatter) {
        Some(target_stem) => follow_tombstone_chain(vault_root, &index.stem_index, &target_stem)?,
        None => candidate.to_path_buf(),
    };
    if !resolved.exists() {
        log::debug!(
            "harvest::identity: resolved path {} vanished before return (stale index entry)",
            resolved.display()
        );
        return None;
    }
    Some(resolved)
}

/// Parse `path` into a [`Note`], returning `None` (logged) on a missing file
/// or a parse failure. `path` must be absolute.
fn parse_existing(vault_root: &Path, path: &Path) -> Option<Note> {
    if !path.exists() {
        log::debug!(
            "harvest::identity: candidate {} does not exist (stale index/receipt)",
            path.display()
        );
        return None;
    }
    match note::parse_note(vault_root, path) {
        Ok(n) => Some(n),
        Err(e) => {
            log::warn!("harvest::identity: failed to parse candidate {}: {e:#}", path.display());
            None
        }
    }
}

/// Whether the note at `path` is itself a tombstone (carries `superseded-by:`).
/// Used only to disambiguate a multi-match stem lookup (see
/// [`follow_tombstone_chain`]) - a parse failure here is treated as "not a
/// tombstone" so a genuinely broken file does not silently vanish from
/// disambiguation; it will fail its own guard/parse when actually resolved.
fn path_is_tombstone(vault_root: &Path, path: &Path) -> bool {
    match note::parse_note(vault_root, path) {
        Ok(n) => superseded_by_of(&n.frontmatter).is_some(),
        Err(_) => false,
    }
}

/// Follow a `superseded-by:` chain from `start_stem` to its live survivor,
/// transitively (a survivor can itself later be superseded again). Depth-
/// bounded and cycle-guarded via a visited set. Refuses (returns `None`,
/// WARNing) if: the stem resolves to nothing, the stem is ambiguous even
/// after the tombstone tie-break, a cycle is detected, or the depth bound
/// (`MAX_TOMBSTONE_DEPTH`) is exceeded.
///
/// **Multi-match tie-break skips tombstones entirely**: when a stem has more
/// than one file (33 duplicate filename stems exist vault-wide), a tombstone
/// among the ties is never the answer - a tombstone can never be the live
/// destination of an UNRELATED chain that merely happens to share its
/// filename stem - so ties are broken by considering only non-tombstone
/// candidates. A stem with exactly one match is used regardless of whether
/// that single match is itself a tombstone (that is the transitive-follow
/// case, not a tie).
fn follow_tombstone_chain(
    vault_root: &Path,
    stem_index: &HashMap<String, Vec<PathBuf>>,
    start_stem: &str,
) -> Option<PathBuf> {
    let mut visited: HashSet<String> = HashSet::new();
    let mut current_stem = start_stem.to_string();

    for _ in 0..MAX_TOMBSTONE_DEPTH {
        if !visited.insert(current_stem.clone()) {
            log::warn!(
                "harvest::identity: superseded-by cycle detected at stem '{current_stem}' - refusing to resolve"
            );
            return None;
        }

        let candidates = match stem_index.get(&current_stem) {
            Some(c) if !c.is_empty() => c,
            _ => {
                log::warn!(
                    "harvest::identity: superseded-by stem '{current_stem}' resolves to no file - refusing to resolve"
                );
                return None;
            }
        };

        let chosen: &PathBuf = if candidates.len() == 1 {
            &candidates[0]
        } else {
            let live: Vec<&PathBuf> = candidates
                .iter()
                .filter(|p| !path_is_tombstone(vault_root, p))
                .collect();
            match live.len() {
                1 => live[0],
                0 => {
                    log::warn!(
                        "harvest::identity: superseded-by stem '{current_stem}' is ambiguous ({} candidates, all tombstones) - refusing to resolve",
                        candidates.len()
                    );
                    return None;
                }
                n => {
                    log::warn!(
                        "harvest::identity: superseded-by stem '{current_stem}' is ambiguous ({n} live candidates) - refusing to resolve"
                    );
                    return None;
                }
            }
        };

        match note::parse_note(vault_root, chosen) {
            Ok(target_note) => match superseded_by_of(&target_note.frontmatter) {
                Some(next_stem) => {
                    current_stem = next_stem;
                    continue;
                }
                None => return Some(chosen.clone()),
            },
            Err(e) => {
                log::warn!(
                    "harvest::identity: failed to parse superseded-by target {}: {e:#} - refusing to resolve",
                    chosen.display()
                );
                return None;
            }
        }
    }

    log::warn!(
        "harvest::identity: superseded-by chain from '{start_stem}' exceeded depth bound ({MAX_TOMBSTONE_DEPTH}) - refusing to resolve"
    );
    None
}

#[cfg(test)]
mod tests;
