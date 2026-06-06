//! Phase 5 (MemGraphRAG): typed `fact` edges + the three consolidation agents.
//!
//! Per the design, the value here concentrates in **cluster bridging** (the
//! islands are real); noise removal is moderate and contradiction resolution is
//! near-idle on a single-curator opinion corpus. It is built for completeness
//! and future heterogeneous sources, not because it pays off heavily today.
//!
//! - **Factual layer:** `extract_facts` pulls subject-predicate-object triples
//!   from ingested notes (LLM, bounded, DI'd), resolves subject/object to their
//!   entity hub note paths, and writes `kind = 'fact'` edges carrying the
//!   relation in `predicate` and the originating note in `src_note`. Both
//!   endpoints must resolve to existing notes (resolve-endpoint-or-skip).
//! - **Consolidation agents:** `remove_noise` (drop generic predicates),
//!   `detect_contradictions` (functional predicate with >1 object → flag, never
//!   overwrite), `bridge_clusters` (connect isolated notes to their nearest
//!   semantic neighbor).

use std::collections::HashSet;

use eyre::Result;
use vault::search::{Edge, SearchIndex};

use crate::config::GraphConfig;
use crate::hub::{HUB_DIR, slugify};
use crate::vault::Note;

/// A subject-predicate-object triple extracted from a note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Triple {
    pub subject: String,
    pub predicate: String,
    pub object: String,
}

/// Extracts triples from a note body. Injected so fact extraction is testable
/// without a live LLM (DI convention).
pub trait TripleExtractor {
    fn extract(&self, note_body: &str) -> Vec<Triple>;
}

/// Production extractor: runs a Fabric pattern and parses `subject | predicate |
/// object` lines. A failure yields no triples for that note (logged).
pub struct FabricTripleExtractor<'a> {
    pub fabric: &'a crate::config::FabricConfig,
    pub pattern: &'a str,
    pub max_input_tokens: usize,
    pub timeout_secs: u64,
}

impl TripleExtractor for FabricTripleExtractor<'_> {
    fn extract(&self, note_body: &str) -> Vec<Triple> {
        let input = crate::fabric::truncate_input(note_body, self.max_input_tokens);
        match crate::fabric::run_pattern(self.fabric, self.pattern, input, self.timeout_secs) {
            Ok(out) => out.lines().filter_map(parse_triple).collect(),
            Err(e) => {
                log::warn!("cortex::memgraph: triple extraction failed for a note: {e}");
                Vec::new()
            }
        }
    }
}

/// Parse one `subject | predicate | object` line into a `Triple`. Returns
/// `None` for malformed lines (wrong field count, any empty field).
fn parse_triple(line: &str) -> Option<Triple> {
    let parts: Vec<&str> = line.split('|').map(|p| p.trim()).collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    Some(Triple {
        subject: parts[0].to_string(),
        predicate: slugify(parts[1]),
        object: parts[2].to_string(),
    })
}

/// True when a note is ingested (`origin: assisted`).
fn is_ingested(note: &Note) -> bool {
    matches!(note.frontmatter.origin.as_deref(), Some("assisted"))
}

/// The hub note path for an entity surface form (`entities/<slug>.md`).
fn hub_path(surface: &str) -> Option<String> {
    let slug = slugify(surface);
    if slug.is_empty() { None } else { Some(format!("{HUB_DIR}/{slug}.md")) }
}

/// Outcome of the fact-extraction pass.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct FactStats {
    pub notes_scanned: usize,
    pub triples_extracted: usize,
    pub facts_written: usize,
    pub facts_skipped: usize,
}

/// Extract typed `fact` edges from ingested notes (bounded to `limit`). A
/// triple becomes an edge `subject_hub --predicate--> object_hub` only when
/// both hubs resolve to existing notes (resolve-endpoint-or-skip in
/// `insert_edges`), with the originating note in `src_note` for provenance.
pub fn extract_facts<E: TripleExtractor>(
    index: &mut SearchIndex,
    notes: &[Note],
    extractor: &E,
    cfg: &GraphConfig,
    limit: usize,
) -> Result<FactStats> {
    let mut stats = FactStats::default();
    for note in notes.iter().filter(|n| is_ingested(n)).take(limit) {
        stats.notes_scanned += 1;
        let note_path = note.path.to_string_lossy().to_string();
        let triples = extractor.extract(&note.body);
        stats.triples_extracted += triples.len();

        let mut edges: Vec<Edge> = Vec::new();
        for t in triples {
            if t.predicate.is_empty() {
                continue;
            }
            let (Some(subj), Some(obj)) = (hub_path(&t.subject), hub_path(&t.object)) else {
                continue;
            };
            edges.push(Edge::fact(subj, obj, t.predicate, cfg.fact_weight, note_path.clone()));
        }
        let (written, skipped) = index.insert_edges(&edges)?;
        stats.facts_written += written;
        stats.facts_skipped += skipped;
    }
    log::info!(
        "cortex::memgraph: facts scanned={} triples={} written={} skipped={}",
        stats.notes_scanned,
        stats.triples_extracted,
        stats.facts_written,
        stats.facts_skipped
    );
    Ok(stats)
}

/// A detected conflict: a functional predicate with more than one distinct
/// object for the same subject. The conflict is recorded, never silently
/// resolved — all objects remain as edges (the design's flag-only policy).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contradiction {
    pub subject: String,
    pub predicate: String,
    pub objects: Vec<String>,
}

/// Detect contradictions among `fact` edges: for each functional predicate,
/// group by subject and flag any subject with multiple distinct objects.
pub fn detect_contradictions(index: &SearchIndex, functional: &HashSet<String>) -> Result<Vec<Contradiction>> {
    use std::collections::BTreeMap;
    let mut by_key: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for f in index.fact_edges()? {
        if functional.contains(&f.predicate) {
            by_key.entry((f.src, f.predicate)).or_default().push(f.dst);
        }
    }
    let mut out = Vec::new();
    for ((subject, predicate), mut objects) in by_key {
        objects.sort();
        objects.dedup();
        if objects.len() > 1 {
            out.push(Contradiction {
                subject,
                predicate,
                objects,
            });
        }
    }
    Ok(out)
}

/// Noise-removal agent: drop `fact` edges whose predicate is in the configured
/// noise set (too generic to carry retrieval value). Returns the count removed.
pub fn remove_noise(index: &SearchIndex, noise: &HashSet<String>) -> Result<usize> {
    let mut removed = 0;
    for f in index.fact_edges()? {
        if noise.contains(&f.predicate) {
            removed += index.delete_fact_edge(&f.src, &f.dst, &f.predicate)?;
        }
    }
    log::info!("cortex::memgraph: noise removal dropped {removed} fact edge(s)");
    Ok(removed)
}

/// Cluster-bridging agent: for each note with NO incident edge, add a `bridge`
/// edge to its nearest semantic neighbor (cosine `>= bridge_min_cosine`), so no
/// embedded note is left a disconnected island. Reuses `note_embeddings`; a
/// note with no embedding or no qualifying neighbor stays isolated. Returns the
/// number of bridges added.
pub fn bridge_clusters(index: &mut SearchIndex, cfg: &GraphConfig) -> Result<usize> {
    let isolated = index.notes_without_edges()?;
    log::debug!(
        "cortex::memgraph: {} isolated note(s) to consider for bridging",
        isolated.len()
    );
    let mut bridges = 0;
    for path in isolated {
        let neighbors = index.semantic_neighbors(&path, 1, cfg.bridge_min_cosine)?;
        if let Some((neighbor, cosine)) = neighbors.into_iter().next() {
            let (written, _) = index.insert_edges(&[Edge::deterministic(path.clone(), neighbor, "bridge", cosine)])?;
            bridges += written;
        }
    }
    log::info!("cortex::memgraph: bridging added {bridges} edge(s)");
    Ok(bridges)
}

/// Outcome of a full consolidation run.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ConsolidationReport {
    pub noise_removed: usize,
    pub contradictions: Vec<Contradiction>,
    pub bridges_added: usize,
}

/// Run all three consolidation agents in order: noise removal, contradiction
/// detection (flag-only), cluster bridging.
pub fn consolidate(index: &mut SearchIndex, cfg: &GraphConfig) -> Result<ConsolidationReport> {
    let noise: HashSet<String> = cfg.noise_predicates.iter().cloned().collect();
    let functional: HashSet<String> = cfg.functional_predicates.iter().cloned().collect();

    let noise_removed = remove_noise(index, &noise)?;
    let contradictions = detect_contradictions(index, &functional)?;
    let bridges_added = bridge_clusters(index, cfg)?;

    for c in &contradictions {
        log::warn!(
            "cortex::memgraph: contradiction on functional predicate '{}' for subject '{}': objects {:?}",
            c.predicate,
            c.subject,
            c.objects
        );
    }
    Ok(ConsolidationReport {
        noise_removed,
        contradictions,
        bridges_added,
    })
}

#[cfg(test)]
mod tests;
