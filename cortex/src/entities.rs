//! `sb cortex entities --discover`: an off-hot-path LLM pass that proposes new
//! glossary entries from distilled notes into `entity-proposals.yml` (Phase 4
//! of graph-augmented-memory).
//!
//! Mirrors `tag-proposals.yml`: it never auto-promotes — a human reviews the
//! proposals and moves the good ones into `glossary.yml`. It only grows the
//! vocabulary; it never links inline (that is Phase 2's `cortex link`).
//!
//! Scoped to *ingested* notes (`origin: assisted`) per the ingested-only
//! convention, and bounded by `max_per_run` notes per pass so a backlog cannot
//! fan unbounded LLM calls (the no-unbounded-fanout rule). Extraction runs
//! sequentially (concurrency = 1).

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use eyre::{Result, WrapErr};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::hub::slugify;
use crate::opts::EntitiesOpts;
use crate::vault::Note;

/// A proposed glossary entity awaiting human promotion. `slug` is the kebab
/// form a curator would add to `glossary.yml` `concepts`; `surface` is the most
/// common surface form observed; `frequency` is how many ingested notes
/// mentioned it; `notes` is a capped sample of source paths for provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct EntityProposal {
    pub slug: String,
    pub surface: String,
    pub frequency: usize,
    pub notes: Vec<String>,
}

/// The `entity-proposals.yml` document (mirrors `ProposalsFile`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EntityProposalsFile {
    pub proposals: Vec<EntityProposal>,
}

/// Outcome of a discovery run.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct EntityReport {
    pub notes_scanned: usize,
    pub proposals: usize,
    pub proposals_path: Option<String>,
}

/// Extracts candidate entity surface forms from a note body. Injected so the
/// discovery logic is testable without a live LLM (per the DI convention).
pub trait EntityExtractor {
    fn extract(&self, note_body: &str) -> Vec<String>;
}

/// Production extractor: runs a Fabric pattern and splits its output into one
/// entity per line. An LLM/subprocess failure yields no entities for that note
/// (logged), never aborting the pass.
pub struct FabricExtractor<'a> {
    pub fabric: &'a crate::config::FabricConfig,
    pub pattern: &'a str,
    pub max_input_tokens: usize,
    pub timeout_secs: u64,
}

impl EntityExtractor for FabricExtractor<'_> {
    fn extract(&self, note_body: &str) -> Vec<String> {
        let input = crate::fabric::truncate_input(note_body, self.max_input_tokens);
        match crate::fabric::run_pattern(self.fabric, self.pattern, input, self.timeout_secs) {
            Ok(out) => out
                .lines()
                .map(|l| l.trim().trim_start_matches(['-', '*', '•']).trim())
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .collect(),
            Err(e) => {
                log::warn!("cortex::entities: extraction failed for a note: {e}");
                Vec::new()
            }
        }
    }
}

/// True when a note is ingested (`origin: assisted`) — the candidate set for
/// vocabulary discovery per the ingested-only convention.
fn is_ingested(note: &Note) -> bool {
    note.frontmatter.origin.as_deref() == Some(vault::schema::Origin::Assisted.as_str())
}

/// Run discovery over `notes` using `extractor`, excluding any entity whose
/// slug is already `known` (glossary concepts, alias targets, canonical tags).
/// Bounded to `limit` ingested notes. Pure aside from the injected extractor.
pub fn discover<E: EntityExtractor>(
    notes: &[Note],
    known: &HashSet<String>,
    extractor: &E,
    limit: usize,
) -> (Vec<EntityProposal>, usize) {
    // Aggregate by slug: frequency + a capped sample of source note paths + the
    // most common surface form.
    struct Acc {
        surface_counts: BTreeMap<String, usize>,
        notes: Vec<String>,
        frequency: usize,
    }
    let mut agg: BTreeMap<String, Acc> = BTreeMap::new();
    const MAX_SAMPLE_NOTES: usize = 5;

    let mut scanned = 0usize;
    for note in notes.iter().filter(|n| is_ingested(n)).take(limit) {
        scanned += 1;
        let path = note.path.to_string_lossy().to_string();
        let mut seen_in_note: HashSet<String> = HashSet::new();
        for surface in extractor.extract(&note.body) {
            let slug = slugify(&surface);
            if slug.is_empty() || known.contains(&slug) {
                continue;
            }
            if !seen_in_note.insert(slug.clone()) {
                continue; // count each entity once per note
            }
            let acc = agg.entry(slug).or_insert_with(|| Acc {
                surface_counts: BTreeMap::new(),
                notes: Vec::new(),
                frequency: 0,
            });
            acc.frequency += 1;
            *acc.surface_counts.entry(surface).or_insert(0) += 1;
            if acc.notes.len() < MAX_SAMPLE_NOTES {
                acc.notes.push(path.clone());
            }
        }
    }

    let mut proposals: Vec<EntityProposal> = agg
        .into_iter()
        .map(|(slug, acc)| {
            // Most frequent surface form (ties broken alphabetically by BTreeMap order).
            let surface = acc
                .surface_counts
                .iter()
                .max_by_key(|(_, c)| **c)
                .map(|(s, _)| s.clone())
                .unwrap_or_else(|| slug.clone());
            EntityProposal {
                slug,
                surface,
                frequency: acc.frequency,
                notes: acc.notes,
            }
        })
        .collect();
    // Most-mentioned first; stable slug order for ties.
    proposals.sort_by(|a, b| b.frequency.cmp(&a.frequency).then_with(|| a.slug.cmp(&b.slug)));
    (proposals, scanned)
}

/// Build the set of already-known entity slugs to exclude from proposals:
/// glossary concepts, glossary alias targets, and canonical tags.
fn known_slugs(config: &Config) -> HashSet<String> {
    let mut known: HashSet<String> = HashSet::new();
    if let Ok(g) = crate::linking::load_glossary(&::vault::paths::glossary()) {
        for c in g.concepts {
            known.insert(slugify(&c));
        }
        for t in g.aliases.values() {
            known.insert(slugify(t));
        }
    }
    if let Ok(canon) = vault::canonical::CanonicalTagsFile::load(Path::new(&config.sweep.canonical_path)) {
        for t in canon.all_tags() {
            known.insert(slugify(&t));
        }
    }
    known
}

/// Write proposals to `entity-proposals.yml`, merging with any existing file so
/// a human's in-progress review is not clobbered (existing slugs win; new ones
/// are appended). Returns the path written.
fn write_proposals(path: &Path, fresh: Vec<EntityProposal>) -> Result<()> {
    let mut existing: EntityProposalsFile = if path.exists() {
        let content = std::fs::read_to_string(path)?;
        serde_yaml::from_str(&content).unwrap_or_default()
    } else {
        EntityProposalsFile::default()
    };
    let have: HashSet<String> = existing.proposals.iter().map(|p| p.slug.clone()).collect();
    for p in fresh {
        if !have.contains(&p.slug) {
            existing.proposals.push(p);
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let yaml = serde_yaml::to_string(&existing)?;
    std::fs::write(path, yaml).wrap_err_with(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Run the discovery pass: scan ingested notes, extract via Fabric, write
/// `entity-proposals.yml`.
pub fn run(vault_root: &Path, config: &Config, opts: &EntitiesOpts) -> Result<EntityReport> {
    log::debug!(
        "cortex::entities::run: vault_root={} discover={} limit={:?}",
        vault_root.display(),
        opts.discover,
        opts.limit
    );

    let notes = crate::vault::scan_vault(vault_root, &config.vault)?;
    let known = known_slugs(config);
    let limit = opts.limit.unwrap_or(config.entities.max_per_run);

    let extractor = FabricExtractor {
        fabric: &config.fabric,
        pattern: &config.entities.fabric_pattern,
        max_input_tokens: config.entities.max_input_tokens,
        timeout_secs: config.entities.fabric_timeout_secs,
    };

    let (proposals, scanned) = discover(&notes, &known, &extractor, limit);
    log::info!(
        "cortex::entities: scanned {scanned} ingested note(s), {} proposal(s)",
        proposals.len()
    );

    let mut report = EntityReport {
        notes_scanned: scanned,
        proposals: proposals.len(),
        proposals_path: None,
    };
    if !proposals.is_empty() {
        let path = ::vault::paths::entity_proposals();
        write_proposals(&path, proposals)?;
        report.proposals_path = Some(path.display().to_string());
    }
    Ok(report)
}

/// Daemon tick: bounded discovery pass on the configured cadence.
pub fn daemon_tick(vault_root: &Path, config: &Config) -> Result<EntityReport> {
    run(
        vault_root,
        config,
        &EntitiesOpts {
            discover: true,
            limit: None,
        },
    )
}

#[cfg(test)]
mod tests;
