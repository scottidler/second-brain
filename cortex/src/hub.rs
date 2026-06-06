//! `sb cortex hub`: stub/refresh entity hub notes (Phase 3 of
//! graph-augmented-memory).
//!
//! For each glossary concept (and alias target), each distinct note `creator`,
//! each distinct source host, and each over-fan-out-cap tag, a hub note is
//! stubbed under `entities/<slug>.md` if absent (idempotent otherwise). Hub
//! notes carry `type: entity` frontmatter and an `ontotype`, double as
//! human-navigable knowledge, and serve as the resolved targets that Phase 2's
//! `[[concept]]` wikilinks point at and that over-cap shared-tag buckets route
//! through (Phase 1) instead of exploding into pairwise edges.
//!
//! The pass also populates the `entities` table (id / kind / hub_path /
//! ontotype) in oracle's index so cortex/oracle share one entity catalogue.

use std::collections::BTreeMap;
use std::path::Path;

use eyre::{Result, WrapErr};
use vault::search::SearchIndex;

use crate::config::Config;
use crate::opts::HubOpts;

/// Directory (vault-relative) that holds every entity hub note. Scanned and
/// watched by default (it is on neither `ScanConfig`'s nor `WatcherConfig`'s
/// ignore list), so hubs are indexed and `[[concept]]` wikilinks resolve.
pub const HUB_DIR: &str = "entities";

/// Ontology class for a hub, written into `ontotype` frontmatter + the
/// `entities` table. Phase 5 refines these against `vault::schema`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HubKind {
    Concept,
    Creator,
    Source,
    Tag,
}

impl HubKind {
    fn as_str(self) -> &'static str {
        match self {
            HubKind::Concept => "concept",
            HubKind::Creator => "creator",
            HubKind::Source => "source",
            HubKind::Tag => "tag",
        }
    }

    /// Default `ontotype` for this hub kind.
    fn ontotype(self) -> &'static str {
        match self {
            HubKind::Concept => "technology",
            HubKind::Creator => "creator",
            HubKind::Source => "source",
            HubKind::Tag => "topic",
        }
    }
}

/// One hub to materialize: its slug (== filename stem), kind, and the display
/// title shown in the stub body/frontmatter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubStub {
    pub slug: String,
    pub kind: HubKind,
    pub title: String,
}

impl HubStub {
    fn hub_path(&self) -> String {
        format!("{HUB_DIR}/{}.md", self.slug)
    }
}

/// Outcome of a hub pass.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct HubReport {
    pub created: usize,
    pub existing: usize,
    /// Stubs that would be created (dry-run) or were created (apply).
    pub stubs: Vec<String>,
    pub entities_recorded: usize,
}

/// Slugify an arbitrary surface form (creator name, source host) into a
/// kebab-case filename stem: lowercase, runs of non-alphanumerics collapse to a
/// single hyphen, trimmed. Concept/tag slugs are already kebab and pass through
/// unchanged.
pub fn slugify(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut prev_hyphen = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_hyphen = false;
        } else if !prev_hyphen {
            out.push('-');
            prev_hyphen = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// Extract the host from a source URL (mirrors the graph pass's `source_host`).
fn source_host(source: &str) -> Option<String> {
    if source.is_empty() {
        return None;
    }
    let stripped = source
        .strip_prefix("https://")
        .or_else(|| source.strip_prefix("http://"))?;
    let host = stripped.split('/').next().unwrap_or(stripped);
    let host = host.split('?').next().unwrap_or(host);
    let host = host.strip_prefix("www.").unwrap_or(host);
    if host.is_empty() { None } else { Some(host.to_lowercase()) }
}

/// Derive the full set of hub stubs from the glossary + the scanned notes.
/// Deterministic (BTreeMap keyed by slug; a slug claimed by several kinds keeps
/// the first by precedence concept > creator > source > tag).
pub fn collect_stubs(
    concepts: &[String],
    alias_targets: &[String],
    notes: &[crate::vault::Note],
    fanout_cap: usize,
) -> Vec<HubStub> {
    let mut stubs: BTreeMap<String, HubStub> = BTreeMap::new();
    let mut insert = |slug: String, kind: HubKind, title: String| {
        stubs.entry(slug.clone()).or_insert(HubStub { slug, kind, title });
    };

    // Concepts + alias targets (already kebab slugs).
    for slug in concepts.iter().chain(alias_targets.iter()) {
        if slug.is_empty() {
            continue;
        }
        insert(slug.clone(), HubKind::Concept, slug.clone());
    }

    // Distinct creators and source hosts.
    let mut tag_counts: BTreeMap<String, usize> = BTreeMap::new();
    for note in notes {
        if let Some(creator) = note.frontmatter.creator.as_deref()
            && !creator.is_empty()
        {
            let slug = slugify(creator);
            if !slug.is_empty() {
                insert(slug, HubKind::Creator, creator.to_string());
            }
        }
        if let Some(source) = note.frontmatter.source.as_deref()
            && let Some(host) = source_host(source)
        {
            let slug = slugify(&host);
            if !slug.is_empty() {
                insert(slug, HubKind::Source, host);
            }
        }
        if let Some(tags) = note.frontmatter.tags.as_ref() {
            for tag in tags {
                *tag_counts.entry(tag.clone()).or_insert(0) += 1;
            }
        }
    }

    // Over-cap tags get a hub so the graph pass can route their dense buckets
    // through it instead of emitting pairwise edges.
    for (tag, count) in &tag_counts {
        if *count > fanout_cap && !tag.is_empty() {
            insert(tag.clone(), HubKind::Tag, tag.clone());
        }
    }

    stubs.into_values().collect()
}

/// Render a hub note's markdown (frontmatter + a short stub body).
fn render_hub(stub: &HubStub, today: &str) -> String {
    format!(
        "---\ntitle: {title}\ntype: entity\nontotype: {ontotype}\ndate: {today}\ntags: []\n---\n\n# {title}\n\nHub note for the **{kind}** entity `{slug}`. Auto-stubbed by `sb cortex hub`; it resolves `[[{slug}]]` wikilinks and serves as a knowledge bundle for this entity.\n",
        title = stub.title,
        ontotype = stub.kind.ontotype(),
        kind = stub.kind.as_str(),
        slug = stub.slug,
    )
}

/// Write the hub notes that don't yet exist (when `apply`), returning the
/// report counts and the set of slugs whose hub note exists on disk afterward
/// (created this run or pre-existing). Pure filesystem work, no DB — testable
/// against a tempdir.
pub fn write_stubs(vault_root: &Path, stubs: &[HubStub], apply: bool, today: &str) -> Result<(HubReport, Vec<String>)> {
    let hub_dir = vault_root.join(HUB_DIR);
    let mut report = HubReport::default();
    let mut materialized: Vec<String> = Vec::new();
    for stub in stubs {
        let abs = vault_root.join(stub.hub_path());
        if abs.exists() {
            report.existing += 1;
            materialized.push(stub.slug.clone());
            continue;
        }
        if apply {
            std::fs::create_dir_all(&hub_dir).wrap_err_with(|| format!("create hub dir {}", hub_dir.display()))?;
            std::fs::write(&abs, render_hub(stub, today))
                .wrap_err_with(|| format!("write hub note {}", abs.display()))?;
            log::info!("cortex::hub: stubbed {}", stub.hub_path());
            materialized.push(stub.slug.clone());
        }
        report.created += 1;
        report.stubs.push(stub.hub_path());
    }
    Ok((report, materialized))
}

/// Upsert every stub into the `entities` table. `hub_path` is set only for
/// slugs whose hub note exists on disk now (a dry-run records entities with a
/// NULL hub_path until the hub is materialized). Returns the count recorded.
pub fn populate_entities(index: &SearchIndex, stubs: &[HubStub], materialized: &[String]) -> Result<usize> {
    let live: std::collections::HashSet<&str> = materialized.iter().map(|s| s.as_str()).collect();
    for stub in stubs {
        let hub_path = stub.hub_path();
        let hp = live.contains(stub.slug.as_str()).then_some(hub_path.as_str());
        index.upsert_entity(&stub.slug, stub.kind.as_str(), hp, Some(stub.kind.ontotype()))?;
    }
    Ok(stubs.len())
}

/// Stub/refresh hub notes and populate the `entities` table.
pub fn run(vault_root: &Path, config: &Config, opts: &HubOpts) -> Result<HubReport> {
    log::debug!(
        "cortex::hub::run: vault_root={} apply={}",
        vault_root.display(),
        opts.apply
    );

    let glossary = crate::linking::load_glossary(&vault::paths::glossary())?;
    let alias_targets: Vec<String> = glossary.aliases.values().cloned().collect();
    let notes = crate::vault::scan_vault(vault_root, &config.vault)?;
    let stubs = collect_stubs(&glossary.concepts, &alias_targets, &notes, config.graph.fanout_cap);
    log::info!("cortex::hub: {} candidate hub(s)", stubs.len());

    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let (mut report, materialized) = write_stubs(vault_root, &stubs, opts.apply, &today)?;

    // Populate the entities table from the oracle index when present.
    if let Ok(index) = SearchIndex::open(&config.oracle_db_path()) {
        report.entities_recorded = populate_entities(&index, &stubs, &materialized)?;
    } else {
        log::warn!("cortex::hub: oracle index unavailable; skipped entities-table population");
    }

    Ok(report)
}

#[cfg(test)]
mod tests;
