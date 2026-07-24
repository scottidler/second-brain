//! `cortex associate`: groups harvest session notes that share a
//! content-derived `slug:` (borg's deterministic collision naming, shipped
//! v0.12.2) and, per pairwise similarity, decides whether to merge them into
//! one note or cross-link them (2026-07-24 cortex-association-sweep design).
//!
//! Phase 1 landed the pure grouping core plus the shared config/opts shapes;
//! Phase 2 adds the pure similarity decision core (`decide`): pairwise
//! similarity (embedding cosine primary, claim TF-IDF fallback, uncomputable
//! treated as below-threshold) fed into union-find transitive clustering.
//! Phase 3 adds the merge executor (`execute_merge`): it enriches the survivor
//! with the idempotent union of every absorbed note's claims,
//! `## Session Details`, and `cortex-session-ids`, then soft-retires each
//! absorbed note to a `superseded-by:` tombstone (no deletion). Phase 4 adds
//! the cross-link executor (`execute_cross_link`): it inserts a reciprocal
//! `## Related` `[[wikilink]]` in every note named by an
//! `AssociationOutcome::CrossLink` (the distinct clusters' representatives),
//! skipping any link already present. Phase 5 (this phase) wires it all
//! together: the pure `apply` orchestrator (group -> whole-group quiescence
//! guard -> decide -> conditionally execute), the `run` composition root that
//! opens the oracle index + embed lock the way `graph::run` does, and
//! `daemon_tick` for the daemon's own periodic interval arm - see
//! `docs/design/2026-07-24-cortex-association-sweep.md`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use eyre::{Result, WrapErr};
use vault::schema::NoteType;

use crate::config::{AssociationConfig, Config, SimilaritySource};
use crate::opts::AssociateOpts;
use crate::vault::Note;

/// Group session notes that share the same content-derived `slug:`
/// frontmatter value (borg's harvest naming, v0.12.2 - a slug collision is
/// an association signal, not a naming accident, per the design's Problem
/// Statement).
///
/// Scoped to `content_type == Session`: this action never associates
/// non-session notes (that is cross-slug work, `cortex::duplicates`' job).
/// Skips notes with `slug == None` (legacy pre-slug notes; a separate
/// harvest-slug migration re-slugs them, out of scope here) and notes
/// carrying a `superseded-by:` tombstone - an already-absorbed note must
/// never re-group, which is what makes the future merge executor's
/// soft-retire idempotent.
///
/// Groups with fewer than two members are dropped: a lone note has nothing
/// to associate with.
///
/// Returned as index groups into the input `notes` slice (not cloned
/// `Note`s) so the caller controls ownership. BTreeMap-ordered by slug so
/// the group order - and therefore any downstream deterministic tie-break -
/// never depends on `notes`' scan order or hash-map iteration order.
pub fn group_by_slug(notes: &[Note]) -> Vec<Vec<usize>> {
    log::debug!("association::group_by_slug: notes={}", notes.len());
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, note) in notes.iter().enumerate() {
        if note.frontmatter.note_type.as_deref() != Some(NoteType::Session.as_str()) {
            continue;
        }
        if note.frontmatter.extra.contains_key("superseded-by") {
            continue;
        }
        let Some(slug) = note.frontmatter.extra.get("slug").and_then(|v| v.as_str()) else {
            continue;
        };
        groups.entry(slug.to_string()).or_default().push(i);
    }
    let result: Vec<Vec<usize>> = groups.into_values().filter(|members| members.len() >= 2).collect();
    log::debug!(
        "association::group_by_slug: groups={} (singletons, legacy, and tombstoned notes dropped)",
        result.len()
    );
    result
}

/// What `decide` concludes for a same-slug group. Typed so `sb` (and the
/// Phase 3-5 executors) format/act without re-inspecting opts, mirroring the
/// `SweepMode` precedent.
///
/// A `Merge` names the survivor plus the notes it absorbs and the deduped
/// union of every cluster member's `cortex-session-ids` (the survivor keeps
/// its OWN filename - no rename - per the design's Merge semantics). A
/// `CrossLink` names the representatives of the distinct clusters in a group
/// that must gain reciprocal `[[wikilink]]`s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssociationOutcome {
    /// One survivor absorbs the other cluster members. `absorbed` and
    /// `session_ids` are sorted for deterministic output; `survivor` is chosen
    /// by earliest `date`, ties broken by smallest primary session id (the
    /// Phase 3 executor consumes these fields as-is).
    Merge {
        survivor: PathBuf,
        absorbed: Vec<PathBuf>,
        session_ids: Vec<String>,
    },
    /// The named notes are distinct-but-related within one slug-group and get
    /// reciprocal wikilinks (one representative per cluster: a merge cluster's
    /// survivor, or a singleton's sole member).
    CrossLink { notes: Vec<PathBuf> },
}

/// Outcome of a `cortex associate` invocation (Phase 5). Mirrors the
/// `SweepMode` precedent: dry-run and apply produce distinct variants so `sb`
/// formats the result without re-inspecting `AssociateOpts`.
///
/// `WouldAssociate` is the full plan `decide` produced across every eligible
/// group (nothing written). `Associated` carries only the outcomes that
/// `apply` actually executed AND that changed at least one byte on disk - an
/// outcome whose executor reported zero changed paths (already up to date, or
/// every write failed and WARN-skipped) is dropped rather than reported as
/// associated, so the daemon's own logging and any future fingerprinting only
/// ever see real writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssociationReport {
    /// Dry-run: the plan `sb cortex associate` (no `--apply`) prints.
    WouldAssociate(Vec<AssociationOutcome>),
    /// `--apply`: the outcomes that were executed and changed real files.
    Associated(Vec<AssociationOutcome>),
}

impl AssociationReport {
    /// The outcomes carried by either variant, for callers that format or
    /// count without caring whether this was a dry-run or a real apply.
    pub fn outcomes(&self) -> &[AssociationOutcome] {
        match self {
            AssociationReport::WouldAssociate(outcomes) | AssociationReport::Associated(outcomes) => outcomes,
        }
    }

    /// True when `apply` actually executed (this is `Associated`), false for
    /// a dry-run `WouldAssociate`.
    pub fn applied(&self) -> bool {
        matches!(self, AssociationReport::Associated(_))
    }
}

/// Exact pairwise embedding cosine between two notes' `kind=summary`
/// embeddings. A port (`vault::search::SearchIndex` is the production impl)
/// so `decide` stays pure and unit-testable with a deterministic fake - no
/// SQLite index required in tests. `Ok(None)` means "either note lacks a
/// summary embedding" (uncomputable via this signal); the `Result` wrapper
/// carries a genuine DB error up rather than silently degrading it to
/// uncomputable (Phase 1 `cosine_between` deviation note).
pub trait EmbeddingCosine {
    fn cosine_between(&self, note_a: &Path, note_b: &Path) -> Result<Option<f32>>;
}

impl EmbeddingCosine for vault::search::SearchIndex {
    fn cosine_between(&self, note_a: &Path, note_b: &Path) -> Result<Option<f32>> {
        vault::search::SearchIndex::cosine_between(self, note_a, note_b)
    }
}

/// Everything `decide` needs beyond the group itself: the merge threshold, the
/// active similarity methodology (`cortex.yml` `actions.association.
/// similarity-source`), and the embedding port. Generic over the port for DI
/// (no `dyn`, per the repo's Rust conventions).
pub struct DecideCtx<'a, E: EmbeddingCosine> {
    /// Merge iff pairwise similarity `>= threshold`.
    pub threshold: f64,
    /// Which similarity signal(s) to compute.
    pub similarity_source: SimilaritySource,
    /// Embedding cosine provider.
    pub embeddings: &'a E,
}

/// Decide, for ONE same-slug group, which members merge and which cross-link,
/// via transitive similarity clustering (real pairwise, not star topology).
///
/// For every pair in the group similarity is computed per
/// `ctx.similarity_source`; any pair `>= ctx.threshold` unions its two members
/// into the same merge-cluster (union-find). Each resulting multi-member
/// cluster becomes one `Merge`; the representatives of every distinct cluster
/// (a merge survivor, or a singleton's sole member) are `CrossLink`ed when the
/// group resolves to two or more clusters.
///
/// **Fail-safe:** an *uncomputable* pair (no embedding AND no claim overlap)
/// is treated as below-threshold, so it is never unioned - an unknown can
/// only ever cross-link, never trigger the destructive merge path.
///
/// Deterministic by construction: pairs are visited in sorted `(i, j)` order,
/// union-find canonicalizes each cluster's root to its smallest member index,
/// clusters iterate in ascending-root order via a `BTreeMap`, and members /
/// absorbed / session-id lists are all sorted. `Merge`s are emitted in cluster
/// order, followed by the single group `CrossLink` (if any).
///
/// Returns `Result` because the embedding signal is a fallible SQLite read;
/// `Ok(None)` from the port is the "uncomputable" case (same effect, correct
/// seam vs the design's bare `Vec` signature - Phase 1 recommended this).
pub fn decide<E: EmbeddingCosine>(group: &[&Note], ctx: &DecideCtx<'_, E>) -> Result<Vec<AssociationOutcome>> {
    log::debug!(
        "association::decide: members={} threshold={} source={:?}",
        group.len(),
        ctx.threshold,
        ctx.similarity_source
    );

    let n = group.len();
    let mut uf = UnionFind::new(n);
    for i in 0..n {
        for j in (i + 1)..n {
            match pairwise_similarity(ctx, group[i], group[j])? {
                Some(sim) if sim >= ctx.threshold => {
                    log::trace!("association::decide: union {i}~{j} sim={sim:.4} >= {}", ctx.threshold);
                    uf.union(i, j);
                }
                Some(sim) => {
                    log::trace!("association::decide: {i}!~{j} sim={sim:.4} < {}", ctx.threshold);
                }
                None => {
                    log::trace!("association::decide: {i}?~{j} uncomputable -> below-threshold");
                }
            }
        }
    }

    // Canonicalize clusters: BTreeMap keyed on each cluster's root, which
    // union-by-min pins to the smallest member index - a stable cluster id
    // independent of union order. Members within a cluster are pushed in
    // ascending index order, so they stay sorted.
    let mut clusters: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        let root = uf.find(i);
        clusters.entry(root).or_default().push(i);
    }

    let mut merges: Vec<AssociationOutcome> = Vec::new();
    let mut representatives: Vec<PathBuf> = Vec::new();
    for members in clusters.values() {
        if members.len() >= 2 {
            let (survivor, absorbed, session_ids) = resolve_merge(group, members);
            representatives.push(survivor.clone());
            merges.push(AssociationOutcome::Merge {
                survivor,
                absorbed,
                session_ids,
            });
        } else {
            representatives.push(group[members[0]].path.clone());
        }
    }

    let mut outcomes = merges;
    if representatives.len() >= 2 {
        outcomes.push(AssociationOutcome::CrossLink { notes: representatives });
    }
    log::debug!("association::decide: outcomes={}", outcomes.len());
    Ok(outcomes)
}

/// Pairwise similarity for one pair per the configured source.
/// `Ok(Some(s))` is a computed similarity (may be below threshold); `Ok(None)`
/// is *uncomputable* (no embedding AND no claim overlap) which `decide` treats
/// as below-threshold.
fn pairwise_similarity<E: EmbeddingCosine>(ctx: &DecideCtx<'_, E>, a: &Note, b: &Note) -> Result<Option<f64>> {
    match ctx.similarity_source {
        SimilaritySource::Embedding => Ok(ctx.embeddings.cosine_between(&a.path, &b.path)?.map(f64::from)),
        SimilaritySource::Claim => Ok(claim_similarity(a, b)),
        SimilaritySource::Both => match ctx.embeddings.cosine_between(&a.path, &b.path)? {
            Some(cosine) => Ok(Some(f64::from(cosine))),
            None => Ok(claim_similarity(a, b)),
        },
    }
}

/// Claim-text TF cosine fallback: tokenize each note's `## Claims` section into
/// term counts and cosine them (the exact primitive the Phase 1 promotion
/// exposed). `None` when either note has no claim tokens - there is nothing to
/// compare, so the pair is uncomputable via this signal (never a spurious
/// zero-similarity merge). Two notes that both HAVE claims but share no terms
/// return `Some(0.0)` - a real below-threshold measurement, not uncomputable.
fn claim_similarity(a: &Note, b: &Note) -> Option<f64> {
    let text_a = claim_text(a);
    let text_b = claim_text(b);
    let tok_a = crate::duplicates::tokenize(&text_a);
    let tok_b = crate::duplicates::tokenize(&text_b);
    if tok_a.is_empty() || tok_b.is_empty() {
        return None;
    }
    let vec_a: HashMap<&str, f64> = tok_a.iter().map(|(term, &count)| (*term, count as f64)).collect();
    let vec_b: HashMap<&str, f64> = tok_b.iter().map(|(term, &count)| (*term, count as f64)).collect();
    Some(crate::duplicates::cosine_similarity(&vec_a, &vec_b))
}

/// Extract the `## Claims` section body from a note (the section
/// `distillers::render` writes). Returns the lines between the `## Claims`
/// heading and the next `## ` heading (or EOF); empty when the note has no
/// such section.
fn claim_text(note: &Note) -> String {
    let mut out = String::new();
    let mut in_claims = false;
    for line in note.body.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("## ") {
            if in_claims {
                break;
            }
            in_claims = trimmed.trim_start_matches("## ").trim().eq_ignore_ascii_case("claims");
            continue;
        }
        if in_claims {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// The deterministic survivor + absorbed + session-id union for a merge
/// cluster. Survivor = earliest `date`, ties broken by smallest primary
/// session id, then smallest path (a total order, so no run can differ).
fn resolve_merge(group: &[&Note], members: &[usize]) -> (PathBuf, Vec<PathBuf>, Vec<String>) {
    let survivor_idx = *members
        .iter()
        .min_by(|&&a, &&b| survivor_key(group[a]).cmp(&survivor_key(group[b])))
        .expect("a cluster always has at least one member");

    let survivor = group[survivor_idx].path.clone();
    let mut absorbed: Vec<PathBuf> = members
        .iter()
        .filter(|&&m| m != survivor_idx)
        .map(|&m| group[m].path.clone())
        .collect();
    absorbed.sort();

    let mut ids: BTreeSet<String> = BTreeSet::new();
    for &m in members {
        for id in session_ids(group[m]) {
            ids.insert(id);
        }
    }
    (survivor, absorbed, ids.into_iter().collect())
}

/// Survivor sort key: `(date, primary-session-id, path)`. A missing/unparseable
/// `date` sorts LAST (`NaiveDate::MAX`) so a dated note always wins survivorship
/// over an undated one; the path is the final total-order tiebreak.
fn survivor_key(note: &Note) -> (chrono::NaiveDate, String, String) {
    let date = note
        .frontmatter
        .date
        .as_deref()
        .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
        .unwrap_or(chrono::NaiveDate::MAX);
    let path = note.path.to_string_lossy().into_owned();
    let primary = primary_session_id(note).unwrap_or_else(|| path.clone());
    (date, primary, path)
}

/// A note's `cortex-session-ids` frontmatter as a `Vec<String>` (empty when
/// absent or not a sequence of strings).
fn session_ids(note: &Note) -> Vec<String> {
    note.frontmatter
        .extra
        .get("cortex-session-ids")
        .and_then(|v| v.as_sequence())
        .map(|seq| seq.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

/// The primary session id (first `cortex-session-ids` entry - borg puts the
/// primary session first, `borg::pipeline::session`), or `None`.
fn primary_session_id(note: &Note) -> Option<String> {
    session_ids(note).into_iter().next()
}

/// Union-find with union-by-min so a cluster's root is always its smallest
/// member index - giving `decide` stable cluster ids and deterministic order
/// regardless of the order pairs are unioned.
struct UnionFind {
    parent: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
        }
    }

    fn find(&mut self, x: usize) -> usize {
        let mut root = x;
        while self.parent[root] != root {
            root = self.parent[root];
        }
        // Path-compress so repeated finds stay near-constant.
        let mut cur = x;
        while self.parent[cur] != cur {
            let next = self.parent[cur];
            self.parent[cur] = root;
            cur = next;
        }
        root
    }

    fn union(&mut self, a: usize, b: usize) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra == rb {
            return;
        }
        // Smaller index becomes root -> stable, min-indexed cluster id.
        let (root, child) = if ra < rb { (ra, rb) } else { (rb, ra) };
        self.parent[child] = root;
    }
}

// -- Phase 5: CLI + daemon orchestration ------------------------------------

/// Glob-exclude check, mirroring `duplicates::matches_exclude` exactly (not
/// promoted/shared: it is three lines and association's own `exclude` list is
/// a distinct config field from duplicates', so cross-module coupling here
/// would buy nothing).
fn matches_exclude(note: &Note, patterns: &[glob::Pattern]) -> bool {
    patterns.iter().any(|pat| {
        let path_str = note.path.to_string_lossy();
        pat.matches(&path_str)
            || note
                .path
                .file_name()
                .map(|f| pat.matches(f.to_string_lossy().as_ref()))
                .unwrap_or(false)
    })
}

/// Parse `AssociationConfig.exclude` glob strings, mirroring
/// `duplicates::parse_exclude_patterns`. An invalid pattern is WARN-and-skip,
/// never a hard error - one typo'd glob must not take down the whole run.
fn parse_exclude_patterns(patterns: &[String]) -> Vec<glob::Pattern> {
    patterns
        .iter()
        .filter_map(|p| match glob::Pattern::new(p) {
            Ok(pat) => Some(pat),
            Err(e) => {
                log::warn!("association::apply: invalid exclude pattern, skipping: {p}: {e}");
                None
            }
        })
        .collect()
}

/// Whole-group quiescence guard: true when ANY member's file mtime falls
/// within `min_quiescence_secs` of now, in which case the ENTIRE group is
/// skipped this run (design's "quiescence is whole-group" - never a
/// half-merged group because one member happened to be mid-edit).
///
/// Fail-safe on the unreadable/unknown cases too: a note whose mtime cannot be
/// read, or whose mtime is somehow in the future (clock skew across a synced
/// vault), is treated as "still quiescing" - skip the group rather than risk
/// merging a note whose edit state is unknown.
fn group_is_quiescing(vault_root: &Path, notes: &[Note], members: &[usize], min_quiescence_secs: u64) -> bool {
    let quiescence = Duration::from_secs(min_quiescence_secs);
    let now = SystemTime::now();
    members.iter().any(|&i| {
        let note = &notes[i];
        let abs_path = vault_root.join(&note.path);
        match fs::metadata(&abs_path).and_then(|m| m.modified()) {
            Ok(mtime) => match now.duration_since(mtime) {
                Ok(elapsed) => elapsed < quiescence,
                Err(_) => {
                    log::warn!(
                        "association::apply: mtime in the future (clock skew?), treating as quiescing {}",
                        note.path.display()
                    );
                    true
                }
            },
            Err(e) => {
                log::warn!(
                    "association::apply: mtime unreadable, treating group as quiescing (skip) {}: {e}",
                    note.path.display()
                );
                true
            }
        }
    })
}

/// Pure(ish) top-level orchestrator: group -> whole-group quiescence guard ->
/// decide -> (only when `do_apply`) execute. This is exactly the composition
/// `association/tests.rs`'s `associate_run` fixture already exercises for
/// Phases 3/4; Phase 5 wraps it in a public, generic-over-the-embedding-port
/// entry point and adds the exclude filter and the quiescence guard.
///
/// Same-effect, correct-seam deviation from the design's bare
/// `apply(vault_root, notes, config) -> Result<AssociationReport>` signature:
/// the embedding port and the apply-vs-dry-run flag are threaded as explicit
/// arguments rather than implied by `config` or re-derived from
/// `AssociateOpts`, so this stays unit-testable with the same `FakeEmbeddings`
/// fixture Phase 2 already built, with no SQLite index required. `run` below
/// is the production composition root that supplies the real port (a
/// `vault::search::SearchIndex`) and the real `AssociateOpts.apply` value.
///
/// Per-outcome execution failure never `?`-aborts the run (the
/// `duplicates.rs:189` contract, inherited from `execute_merge`/
/// `execute_cross_link`'s own WARN-and-skip internals): an outcome whose
/// executor changed zero files (already up to date, or every write failed) is
/// simply omitted from `Associated`, not treated as an error.
pub fn apply<E: EmbeddingCosine>(
    vault_root: &Path,
    notes: &[Note],
    config: &AssociationConfig,
    embeddings: &E,
    do_apply: bool,
) -> Result<AssociationReport> {
    log::debug!(
        "association::apply: vault_root={} notes={} threshold={} source={:?} do_apply={}",
        vault_root.display(),
        notes.len(),
        config.threshold,
        config.similarity_source,
        do_apply
    );

    let exclude_patterns = parse_exclude_patterns(&config.exclude);
    let eligible: Vec<Note> = notes
        .iter()
        .filter(|n| !matches_exclude(n, &exclude_patterns))
        .cloned()
        .collect();

    let groups = group_by_slug(&eligible);
    log::debug!(
        "association::apply: eligible={} groups={}",
        eligible.len(),
        groups.len()
    );

    let ctx = DecideCtx {
        threshold: config.threshold,
        similarity_source: config.similarity_source,
        embeddings,
    };

    let mut outcomes: Vec<AssociationOutcome> = Vec::new();
    let mut skipped_groups = 0usize;
    for members in &groups {
        if group_is_quiescing(vault_root, &eligible, members, config.min_quiescence_secs) {
            skipped_groups += 1;
            continue;
        }
        let group_refs: Vec<&Note> = members.iter().map(|&i| &eligible[i]).collect();
        outcomes.extend(decide(&group_refs, &ctx)?);
    }
    if skipped_groups > 0 {
        log::info!(
            "association::apply: skipped {skipped_groups} group(s) whole (a member is within min-quiescence-secs)"
        );
    }

    if !do_apply {
        log::debug!("association::apply: dry-run, outcomes={}", outcomes.len());
        return Ok(AssociationReport::WouldAssociate(outcomes));
    }

    let writer = AtomicWriter;
    let mut associated: Vec<AssociationOutcome> = Vec::new();
    let mut changed_files = 0usize;
    for outcome in outcomes {
        // execute_merge/execute_cross_link already WARN-and-skip every
        // per-file failure internally and return `Ok` in practice; this
        // outer match never `?`-aborts the run even so, per the
        // `duplicates.rs:189` contract, in case a future executor change
        // introduces a genuine top-level `Err`.
        let changed = match &outcome {
            AssociationOutcome::Merge {
                survivor,
                absorbed,
                session_ids,
            } => execute_merge(vault_root, survivor, absorbed, session_ids, &writer),
            AssociationOutcome::CrossLink { notes: members } => execute_cross_link(vault_root, members, &writer),
        };
        let changed = match changed {
            Ok(changed) => changed,
            Err(e) => {
                log::warn!("association::apply: outcome execution failed, skipping: {e}");
                continue;
            }
        };
        if changed.is_empty() {
            // Idempotent no-op (already merged/linked) - never reported as a
            // fresh association.
            continue;
        }
        changed_files += changed.len();
        associated.push(outcome);
    }
    log::info!(
        "association::apply: associated {} outcome(s), {} file(s) changed",
        associated.len(),
        changed_files
    );
    Ok(AssociationReport::Associated(associated))
}

/// Composition root for `sb cortex associate`: scans the vault, opens its own
/// oracle-DB connection (cortex commands do not share oracle's
/// `Mutex<SearchIndex>` - the `graph.rs:87` precedent), takes the shared embed
/// file lock before reading `note_embeddings` so this can never interleave
/// with a concurrent `cortex embed` write, and delegates to `apply`.
pub fn run(vault_root: &Path, config: &Config, opts: &AssociateOpts) -> Result<AssociationReport> {
    log::debug!(
        "association::run: vault_root={} apply={}",
        vault_root.display(),
        opts.apply
    );
    let notes = crate::vault::scan_vault(vault_root, &config.vault)?;

    let db_path = config.oracle_db_path();
    let index = vault::search::SearchIndex::open(&db_path)
        .wrap_err_with(|| format!("failed to open search index at {}", db_path.display()))?;

    let lock = crate::embed::acquire_lock()?;
    log::debug!("association::run: acquired embed file lock");
    let report = apply(vault_root, &notes, &config.actions.association, &index, opts.apply)?;
    drop(lock);

    Ok(report)
}

/// Daemon tick (Phase 5): the NEW periodic interval arm, modeled on the
/// `embed`/`cold`/`graph` ticks - always AUTO-APPLIES (there is no per-tick
/// dry-run; the daemon either associates or, per `is_enabled("association")`,
/// does not run at all). The caller (`daemon::start_watching`) is responsible
/// for the `is_enabled` gate and for wrapping this in `block_in_place` (it
/// does blocking SQLite IO under the embed lock, same as `graph::daemon_tick`).
pub fn daemon_tick(vault_root: &Path, config: &Config) -> Result<AssociationReport> {
    log::debug!("association::daemon_tick: vault_root={}", vault_root.display());
    run(vault_root, config, &AssociateOpts { apply: true })
}

// -- Phase 3: merge executor -----------------------------------------------

/// The `## Claims` section heading (as `distillers::render` writes it).
const CLAIMS_HEADING: &str = "## Claims";
/// The `## Session Details` section heading (as `borg::pipeline::session`
/// writes it).
const SESSION_DETAILS_HEADING: &str = "## Session Details";
/// The `## Related` cross-link section heading Phase 4's `execute_cross_link`
/// writes/appends to.
const RELATED_HEADING: &str = "## Related";

/// The single note-write seam the merge executor writes through. Production is
/// `AtomicWriter` (delegating to `vault::note::write_atomic`); tests inject a
/// writer that fails for a chosen path so the partial-failure self-heal can be
/// asserted deterministically without racing the real filesystem. Generic over
/// the port (no `dyn`) per the repo's Rust conventions.
///
/// The sibling `apply_scope`/`apply_duplicates` call `write_atomic` directly;
/// the port here is the one deviation, earned by Phase 3's requirement to test
/// a mid-cluster tombstone-write failure (a break-the-code self-heal proof).
pub trait NoteWriter {
    fn write(&self, dest: &Path, bytes: &[u8]) -> Result<()>;
}

/// The production writer: an atomic, Syncthing-safe write via the shared
/// workspace primitive (`vault::note::write_atomic`).
pub struct AtomicWriter;

impl NoteWriter for AtomicWriter {
    fn write(&self, dest: &Path, bytes: &[u8]) -> Result<()> {
        vault::note::write_atomic(dest, bytes)
    }
}

/// Execute ONE merge cluster: enrich the survivor with the deduped union of
/// every absorbed note's claims, `## Session Details` bullets, and
/// `cortex-session-ids`, then soft-retire each absorbed note to a tombstone.
///
/// `survivor`, `absorbed`, and `session_ids` come straight off the
/// `AssociationOutcome::Merge` variant `decide` produced (survivor already
/// chosen by earliest date, ids already the sorted dedup union). Paths are
/// vault-relative; each is joined onto `vault_root` for the read/write.
///
/// Apply order (multi-file safety - `write_atomic` is per-file, a merge touches
/// N+1 files): (1) preflight-read the survivor and every absorbed note; (2)
/// write the enriched survivor FIRST (it holds the full union); (3) then write
/// each absorbed tombstone. If the survivor read/write fails the whole cluster
/// is skipped (no absorbed note is retired, so nothing is stranded). If a
/// single tombstone write fails it WARN-and-continues (the `duplicates.rs:189`
/// contract): the survivor already holds the union and the un-retired absorbed
/// note keeps its `slug:`, so the next run re-groups and re-absorbs it - and
/// because the union is idempotent, no duplication results (self-heal).
///
/// Returns the vault-relative paths this call actually byte-changed (the
/// survivor when enriched, plus each retired tombstone), mirroring
/// `scope::apply_scope` / `duplicates::apply_duplicates` so the daemon's
/// oscillation fingerprint draws only from real writes. A byte-identical
/// survivor is NOT rewritten, so a re-absorption during self-heal reports only
/// the newly-retired tombstone.
pub fn execute_merge<W: NoteWriter>(
    vault_root: &Path,
    survivor: &Path,
    absorbed: &[PathBuf],
    session_ids: &[String],
    writer: &W,
) -> Result<Vec<String>> {
    log::debug!(
        "association::execute_merge: vault_root={} survivor={} absorbed={} ids={}",
        vault_root.display(),
        survivor.display(),
        absorbed.len(),
        session_ids.len()
    );
    let mut changed: Vec<String> = Vec::new();

    let survivor_abs = vault_root.join(survivor);
    let original = match fs::read_to_string(&survivor_abs) {
        Ok(c) => c,
        Err(e) => {
            // Survivor unreadable -> skip the whole cluster. Retiring an
            // absorbed note now would strand its content with no enriched home.
            log::warn!(
                "association::execute_merge: skipping cluster, survivor unreadable {}: {e}",
                survivor.display()
            );
            return Ok(changed);
        }
    };

    // Preflight-read every absorbed note. A note that fails to read is skipped
    // (neither unioned nor retired): it keeps its slug and re-groups next run.
    let mut readable: Vec<(&PathBuf, String)> = Vec::new();
    for path in absorbed {
        match fs::read_to_string(vault_root.join(path)) {
            Ok(c) => readable.push((path, c)),
            Err(e) => log::warn!(
                "association::execute_merge: skipping absorbed note (unreadable) {}: {e}",
                path.display()
            ),
        }
    }

    // Build the enriched survivor: append the absorbed claim bullets, then the
    // absorbed session-detail bullets, then set cortex-session-ids to the union.
    let claim_blocks: Vec<Vec<String>> = readable
        .iter()
        .flat_map(|(_, c)| bullet_blocks(c, CLAIMS_HEADING))
        .collect();
    let detail_blocks: Vec<Vec<String>> = readable
        .iter()
        .flat_map(|(_, c)| bullet_blocks(c, SESSION_DETAILS_HEADING))
        .collect();

    let mut enriched = append_bullets(&original, CLAIMS_HEADING, &claim_blocks, claim_key);
    enriched = append_bullets(&enriched, SESSION_DETAILS_HEADING, &detail_blocks, session_detail_key);
    if !session_ids.is_empty() {
        let seq = serde_yaml::Value::Sequence(
            session_ids
                .iter()
                .map(|id| serde_yaml::Value::String(id.clone()))
                .collect(),
        );
        if let Some(with_ids) =
            crate::scope::insert_frontmatter_fields(&enriched, &[("cortex-session-ids".to_string(), seq)])
        {
            enriched = with_ids;
        }
    }

    // Write the survivor FIRST, only if it actually changed (a byte-identical
    // re-run must not churn the file or the daemon fingerprint).
    if enriched != original {
        if let Err(e) = writer.write(&survivor_abs, enriched.as_bytes()) {
            // Survivor write failed -> do NOT retire any absorbed note.
            log::warn!(
                "association::execute_merge: survivor write failed, cluster skipped {}: {e}",
                survivor.display()
            );
            return Ok(changed);
        }
        log::info!("association::execute_merge: enriched survivor {}", survivor.display());
        changed.push(survivor.to_string_lossy().into_owned());
    }

    // Soft-retire each readable absorbed note. A failed tombstone write WARNs
    // and continues; the note self-heals on the next run.
    for (path, content) in &readable {
        let Some(tombstone) = tombstone_content(content, survivor) else {
            log::warn!(
                "association::execute_merge: absorbed note has no frontmatter, not retiring {}",
                path.display()
            );
            continue;
        };
        if let Err(e) = writer.write(&vault_root.join(path), tombstone.as_bytes()) {
            log::warn!(
                "association::execute_merge: tombstone write failed (self-heals next run) {}: {e}",
                path.display()
            );
            continue;
        }
        log::info!(
            "association::execute_merge: soft-retired {} -> superseded-by {}",
            path.display(),
            survivor.display()
        );
        changed.push(path.to_string_lossy().into_owned());
    }

    log::debug!("association::execute_merge: changed={}", changed.len());
    Ok(changed)
}

/// True for a markdown list-item line (`- ` or `* `), inspected trimmed.
fn is_bullet(trimmed: &str) -> bool {
    trimmed.starts_with("- ") || trimmed.starts_with("* ")
}

/// The bullet content with its `- `/`* ` marker stripped (unchanged if it is
/// not a bullet).
fn strip_bullet(trimmed: &str) -> &str {
    trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .unwrap_or(trimmed)
}

/// Extract the bullet "blocks" of a `## H2` section: each block is a bullet
/// line plus any `> ...` quote-continuation lines that belong to it (matching
/// `vault::search::parse_body_claims`'s continuation handling), so a claim's
/// verbatim quote rides along when it is unioned into the survivor. Lines are
/// returned verbatim so formatting is preserved. Empty when the section is
/// absent.
fn bullet_blocks(content: &str, heading: &str) -> Vec<Vec<String>> {
    let mut blocks: Vec<Vec<String>> = Vec::new();
    let mut in_section = false;
    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("## ") {
            if in_section {
                break;
            }
            in_section = trimmed == heading;
            continue;
        }
        if !in_section {
            continue;
        }
        if is_bullet(trimmed) {
            blocks.push(vec![line.to_string()]);
        } else if trimmed.starts_with("> ")
            && let Some(last) = blocks.last_mut()
        {
            last.push(line.to_string());
        }
    }
    blocks
}

/// Dedup key for a `## Claims` bullet: its trimmed text with the bullet marker
/// stripped (the design's "claims whose trimmed text is not already present").
fn claim_key(bullet_line: &str) -> String {
    strip_bullet(bullet_line.trim_start()).trim().to_string()
}

/// Dedup key for a `## Session Details` bullet: the `clyde://<id>` session id,
/// so the same session is never listed twice even if its rendered title/repo
/// columns differ across notes. Falls back to the trimmed text when the bullet
/// carries no `clyde://` id.
fn session_detail_key(bullet_line: &str) -> String {
    let text = strip_bullet(bullet_line.trim_start()).trim();
    if let Some(rest) = text.strip_prefix("clyde://") {
        let id: String = rest.chars().take_while(|c| !c.is_whitespace()).collect();
        if !id.is_empty() {
            return id;
        }
    }
    text.to_string()
}

/// Append `incoming` bullet blocks to `content`'s `heading` section, skipping
/// any block whose `key` already appears there (idempotent union). Incoming
/// blocks are also deduped against each other so two absorbed notes carrying
/// the same claim add it once. If the section is absent it is created at the
/// end of the document. Returns `content` unchanged (byte-identical) when every
/// incoming block is already present.
fn append_bullets(content: &str, heading: &str, incoming: &[Vec<String>], key: fn(&str) -> String) -> String {
    if incoming.is_empty() {
        return content.to_string();
    }

    let mut seen: BTreeSet<String> = bullet_blocks(content, heading)
        .iter()
        .filter_map(|b| b.first())
        .map(|line| key(line))
        .collect();

    let mut to_add: Vec<&Vec<String>> = Vec::new();
    for block in incoming {
        let Some(first) = block.first() else { continue };
        if seen.insert(key(first)) {
            to_add.push(block);
        }
    }
    if to_add.is_empty() {
        return content.to_string();
    }

    let ends_with_newline = content.ends_with('\n');
    let mut lines: Vec<String> = content.lines().map(String::from).collect();

    match lines.iter().position(|l| l.trim_start() == heading) {
        Some(start) => {
            // Section end = the next `## ` heading after start, else EOF.
            let end = lines[start + 1..]
                .iter()
                .position(|l| l.trim_start().starts_with("## "))
                .map(|off| start + 1 + off)
                .unwrap_or(lines.len());
            // Insert after the last non-blank line within the section so new
            // bullets sit with the existing ones, not after trailing blanks.
            let mut insert_at = start + 1;
            for (i, line) in lines.iter().enumerate().take(end).skip(start + 1) {
                if !line.trim().is_empty() {
                    insert_at = i + 1;
                }
            }
            let flat: Vec<String> = to_add.into_iter().flatten().cloned().collect();
            lines.splice(insert_at..insert_at, flat);
        }
        None => {
            // Section absent: create it at EOF.
            if lines.last().map(|l| !l.trim().is_empty()).unwrap_or(false) {
                lines.push(String::new());
            }
            lines.push(heading.to_string());
            lines.push(String::new());
            for block in to_add {
                lines.extend(block.iter().cloned());
            }
        }
    }

    let mut out = lines.join("\n");
    if ends_with_newline {
        out.push('\n');
    }
    out
}

/// Rewrite an absorbed note into a soft-retire tombstone: frontmatter gains
/// `superseded-by: <survivor-stem>` and loses `slug:` (so it never re-groups),
/// and the body becomes a single `Merged into [[survivor-stem]].` redirect.
/// No `status:` change - `vault::schema::Status` has no Archived variant, so
/// `superseded-by:` IS the tombstone marker (schema-is-law). Returns `None`
/// when the note has no frontmatter block (cannot be marked).
fn tombstone_content(content: &str, survivor: &Path) -> Option<String> {
    let stem = note_stem(survivor);
    // Drop slug first (if present), then set superseded-by.
    let without_slug =
        crate::scope::remove_frontmatter_fields(content, &["slug".to_string()]).unwrap_or_else(|| content.to_string());
    let marked = crate::scope::insert_frontmatter_fields(
        &without_slug,
        &[("superseded-by".to_string(), serde_yaml::Value::String(stem.clone()))],
    )?;
    let redirect = format!("Merged into [[{stem}]].\n");
    swap_body(&marked, &redirect)
}

/// A note's filename without its `.md` extension - the wikilink target for
/// both the merge tombstone's `superseded-by:`/redirect (e.g.
/// `foo--a1b2c3d4.md` -> `foo--a1b2c3d4`) and the Phase 4 cross-link
/// executor. Always the actual filename stem, never the shared `slug:`
/// frontmatter value: same-slug notes are exactly what a same-slug GROUP is,
/// so the slug alone cannot disambiguate which sibling a wikilink targets -
/// only the real, unique filename resolves in Obsidian.
fn note_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Replace a note's body (everything after the closing `---` fence) with
/// `new_body`, preserving the frontmatter block verbatim. Mirrors the
/// fence-parsing in `scope::insert_frontmatter_fields`. `None` when there is no
/// frontmatter block.
fn swap_body(content: &str, new_body: &str) -> Option<String> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }
    let after_opening = trimmed[3..].trim_start_matches(['\r', '\n']);
    let end_pos = after_opening.find("\n---")?;
    let fm_block = &after_opening[..end_pos];
    // rest = "\n---...\n<body>"; skip the leading '\n', the fence line runs to
    // the next newline.
    let rest = &after_opening[end_pos + 1..];
    let fence_end = rest.find('\n').unwrap_or(rest.len());
    let closing_fence = &rest[..fence_end];
    let offset = content.len() - trimmed.len();
    let prefix = &content[..offset];
    Some(format!("{prefix}---\n{fm_block}\n{closing_fence}\n\n{new_body}"))
}

// -- Phase 4: cross-link executor -------------------------------------------

/// Execute ONE `AssociationOutcome::CrossLink`: insert a reciprocal
/// `## Related` `[[wikilink]]` bullet in every named note, pointing at each
/// OTHER named note by its own filename stem (`note_stem`) - the house
/// wikilink form used throughout `cortex/src/linking.rs`.
///
/// `notes` is exactly `CrossLink.notes` off `decide`'s output: the
/// representatives of the group's distinct clusters (a merge survivor, or a
/// singleton's sole member) - never an absorbed tombstone, which already
/// carries its own `Merged into [[survivor]].` redirect and has nothing to
/// gain from a second link.
///
/// Preflight-reads every member first (mirrors `execute_merge`'s apply
/// order); a note that fails to read is WARN-and-skipped - it is neither
/// written to nor referenced as a link target for its siblings, so no
/// sibling ever gains a wikilink pointing at a note that could not be
/// confirmed to exist. Idempotent via `append_bullets` + `related_key`: a
/// bullet whose wikilink target is already present in the note's `## Related`
/// section is never re-added, so a second run on an already-linked group
/// writes zero bytes (skip-if-unchanged, the same contract `execute_merge`'s
/// survivor enrichment uses).
///
/// Returns the vault-relative paths this call actually byte-changed, mirroring
/// `execute_merge` / `scope::apply_scope` / `duplicates::apply_duplicates` so
/// the daemon's oscillation fingerprint draws only from real writes.
pub fn execute_cross_link<W: NoteWriter>(vault_root: &Path, notes: &[PathBuf], writer: &W) -> Result<Vec<String>> {
    log::debug!(
        "association::execute_cross_link: vault_root={} notes={}",
        vault_root.display(),
        notes.len()
    );
    let mut changed: Vec<String> = Vec::new();

    // Preflight-read every member. An unreadable note is skipped entirely: it
    // is never written to, and never offered as a link target to its
    // siblings (a wikilink to a note that could not be confirmed present is
    // worse than no link).
    let mut readable: Vec<(&PathBuf, String)> = Vec::new();
    for path in notes {
        match fs::read_to_string(vault_root.join(path)) {
            Ok(c) => readable.push((path, c)),
            Err(e) => log::warn!(
                "association::execute_cross_link: skipping note (unreadable) {}: {e}",
                path.display()
            ),
        }
    }

    if readable.len() < 2 {
        log::debug!("association::execute_cross_link: fewer than two readable members, nothing to cross-link");
        return Ok(changed);
    }

    let stems: Vec<String> = readable.iter().map(|(path, _)| note_stem(path)).collect();

    for (idx, (path, content)) in readable.iter().enumerate() {
        let sibling_blocks: Vec<Vec<String>> = stems
            .iter()
            .enumerate()
            .filter(|&(j, _)| j != idx)
            .map(|(_, stem)| vec![format!("- [[{stem}]]")])
            .collect();

        let updated = append_bullets(content, RELATED_HEADING, &sibling_blocks, related_key);
        if &updated == content {
            continue;
        }
        if let Err(e) = writer.write(&vault_root.join(path), updated.as_bytes()) {
            log::warn!("association::execute_cross_link: write failed {}: {e}", path.display());
            continue;
        }
        log::info!("association::execute_cross_link: cross-linked {}", path.display());
        changed.push(path.to_string_lossy().into_owned());
    }

    log::debug!("association::execute_cross_link: changed={}", changed.len());
    Ok(changed)
}

/// Dedup key for a `## Related` bullet: the wikilink TARGET (the text before
/// any `|` alias, inside `[[...]]`), lowercased - so `[[Foo]]` and
/// `[[foo|Foo Title]]` are treated as the same link and a second run never
/// re-adds a link already present regardless of its exact piped form. Falls
/// back to the trimmed bullet text when the bullet carries no `[[...]]`
/// wikilink (defensive; every bullet this executor emits is always one).
fn related_key(bullet_line: &str) -> String {
    let text = strip_bullet(bullet_line.trim_start()).trim();
    let inner = text
        .strip_prefix("[[")
        .and_then(|s| s.strip_suffix("]]"))
        .unwrap_or(text);
    inner.split('|').next().unwrap_or(inner).trim().to_lowercase()
}

#[cfg(test)]
mod tests;
