//! `cortex associate`: groups harvest session notes that share a
//! content-derived `slug:` (borg's deterministic collision naming, shipped
//! v0.12.2) and, per pairwise similarity, decides whether to merge them into
//! one note or cross-link them (2026-07-24 cortex-association-sweep design).
//!
//! Phase 1 landed the pure grouping core plus the shared config/opts shapes;
//! Phase 2 adds the pure similarity decision core (`decide`): pairwise
//! similarity (embedding cosine primary, claim TF-IDF fallback, uncomputable
//! treated as below-threshold) fed into union-find transitive clustering.
//! The merge executor, cross-link executor, and CLI/daemon wiring are later
//! phases of the same design - see
//! `docs/design/2026-07-24-cortex-association-sweep.md`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use eyre::Result;
use vault::schema::NoteType;

use crate::config::SimilaritySource;
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

#[cfg(test)]
mod tests;
