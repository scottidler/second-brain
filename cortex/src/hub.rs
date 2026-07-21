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
    /// A `<org>/<repo>` GitHub repo (harvest-clyde-sessions design, Phase 10).
    /// Minted from a note's `repo:` frontmatter. Its `entities` id (slug) is the
    /// injective `repo-<org>--<repo>` from `repo_hub_slug` - a namespace disjoint
    /// from the bare-token Concept/Creator/Source/Tag slugs. Its ON-DISK path is
    /// NOT the flat slug: repo hubs nest at `entities/repos/<org>/<repo>.md`
    /// (mirrors `~/repos`) via `repo_hub_path` (Scott, 2026-07-20). Slug =
    /// stable DB id; path = human-navigable nested file.
    Repo,
}

impl HubKind {
    fn as_str(self) -> &'static str {
        match self {
            HubKind::Concept => "concept",
            HubKind::Creator => "creator",
            HubKind::Source => "source",
            HubKind::Tag => "tag",
            HubKind::Repo => "repo",
        }
    }

    /// Default `ontotype` for this hub kind.
    fn ontotype(self) -> &'static str {
        match self {
            HubKind::Concept => "technology",
            HubKind::Creator => "creator",
            HubKind::Source => "source",
            HubKind::Tag => "topic",
            HubKind::Repo => "repo",
        }
    }
}

/// Hub slug for a `<org>/<repo>` value (harvest-clyde-sessions design, Phase
/// 10). Flat hub filenames can't hold the `/`, and the generic `slugify` would
/// collapse `a/b-c` and `a-b/c` to the same `a-b-c`. So repo hubs get a
/// dedicated slug: `repo-<slugify(org)>--<slugify(repo)>`, splitting on the
/// single `/`. INJECTIVE on the org/repo split - the `--` boundary can't be
/// forged because `slugify` collapses non-alphanumeric runs to a SINGLE `-`
/// and trims boundary hyphens, so no slugified component ever contains `--` or
/// a boundary hyphen. NOT fully injective per-component (`slugify` also folds
/// `.`/`_` to `-`, so `org/.github` and `org/github` collide) - accepted: the
/// `repo:` frontmatter stays byte-truthful, only hub MEMBERSHIP merges, and it
/// is the same lossiness Creator/Source hubs already carry. Case-folding is
/// inherited and correct (GitHub names are case-insensitive). The caller MUST
/// have passed the value through `validate_repo_slug` first (exactly one `/`).
pub fn repo_hub_slug(repo: &str) -> String {
    let (org, name) = repo.split_once('/').unwrap_or((repo, ""));
    format!("repo-{}--{}", slugify(org), slugify(name))
}

/// On-disk (vault-relative) path of a repo hub note: nested folders mirroring
/// `~/repos/<org>/<repo>` under `entities/repos/` (Scott, 2026-07-20),
/// superseding the flat `entities/repo-<org>--<repo>.md` scheme. Real directory
/// nesting makes the path INJECTIVE on the `<org>/<repo>` split for free: the
/// adversarial pair `a/b-c` and `a-b/c` land at DISTINCT paths
/// (`entities/repos/a/b-c.md` vs `entities/repos/a-b/c.md`) because the `/`
/// becomes a real directory boundary, no separator-encoding needed. Same
/// per-component `slugify` lossiness the flat slug carried (`.`/`_` fold to `-`),
/// which is accepted and membership-only; `repo:` frontmatter stays
/// byte-truthful. The caller MUST have passed the value through
/// `validate_repo_slug` first (exactly one `/`).
pub fn repo_hub_path(repo: &str) -> String {
    let (org, name) = repo.split_once('/').unwrap_or((repo, ""));
    format!("{HUB_DIR}/repos/{}/{}.md", slugify(org), slugify(name))
}

/// Obsidian wikilink TARGET that resolves to a repo hub's nested note. This is
/// the full vault-relative path MINUS the `.md` extension
/// (`entities/repos/<org>/<repo>`). The full-path form is chosen over a bare
/// basename because it resolves UNCONDITIONALLY (Obsidian matches the literal
/// vault-relative path) and is collision-proof across orgs that share a repo
/// basename (`scottidler/loopr` vs `tatari-tv/loopr` disambiguate on the `<org>`
/// directory), where a bare `[[loopr]]` would be ambiguous. Pair it with an
/// `<org>/<repo>` display alias for a clean render.
pub fn repo_hub_wikilink_target(repo: &str) -> String {
    let path = repo_hub_path(repo);
    path.strip_suffix(".md").unwrap_or(&path).to_string()
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
    /// Vault-relative path of this hub's note. Kind-aware: repo hubs nest at
    /// `entities/repos/<org>/<repo>.md` (from the byte-truthful `<org>/<repo>`
    /// title); every other kind stays flat at `entities/<slug>.md`.
    fn hub_path(&self) -> String {
        match self.kind {
            HubKind::Repo => repo_hub_path(&self.title),
            _ => format!("{HUB_DIR}/{}.md", self.slug),
        }
    }

    /// The Obsidian wikilink `[[...]]` markup that resolves to THIS hub note.
    /// Repo hubs use the full nested-path target with an `<org>/<repo>` display
    /// alias (a bare slug won't resolve to a nested file); other kinds use the
    /// flat slug (its note is `entities/<slug>.md`, so the bare slug resolves).
    fn self_link(&self) -> String {
        match self.kind {
            HubKind::Repo => format!("[[{}|{}]]", repo_hub_wikilink_target(&self.title), self.title),
            _ => format!("[[{}]]", self.slug),
        }
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
    /// Hubs whose body was re-synthesized (Phase 12, `--synthesize`).
    pub synthesized: usize,
    /// Hubs whose prior body was preserved because synthesis failed/was empty.
    pub synth_preserved: usize,
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
        // Repo hub (Phase 10): unconditional - every note carrying a well-formed
        // `repo:` mints/joins its repo hub. The `repo-<org>--<repo>` slug is a
        // namespace disjoint from the bare-token kinds above, so it never
        // collides. A malformed slug skips the edge and logs loudly, but the
        // note is still indexed (knowledge is never discarded).
        if let Some(repo) = note.frontmatter.repo.as_deref()
            && !repo.is_empty()
        {
            if vault::schema::validate_repo_slug(repo) {
                insert(repo_hub_slug(repo), HubKind::Repo, repo.to_string());
            } else {
                log::warn!(
                    "cortex::hub: note {} has malformed repo slug {repo:?} - skipping repo hub edge (note still indexed)",
                    note.path.display()
                );
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

/// Render a hub note's markdown (frontmatter + a short stub body). The body
/// carries a LIVE self-link (`self_link`) that resolves to this hub's note - a
/// nested full-path link for repo hubs, the flat slug for every other kind.
fn render_hub(stub: &HubStub, today: &str) -> String {
    let body = match stub.kind {
        HubKind::Repo => format!(
            "Hub note for the **repo** entity `{title}`. Auto-stubbed by `sb cortex hub`; it gathers every note carrying `repo: {title}` and serves as a knowledge bundle for this repository. Canonical link: {link}.",
            title = stub.title,
            link = stub.self_link(),
        ),
        _ => format!(
            "Hub note for the **{kind}** entity `{slug}`. Auto-stubbed by `sb cortex hub`; it resolves `[[{slug}]]` wikilinks and serves as a knowledge bundle for this entity.",
            kind = stub.kind.as_str(),
            slug = stub.slug,
        ),
    };
    format!(
        "---\ntitle: {title}\ntype: entity\nontotype: {ontotype}\ndate: {today}\ntags: []\n---\n\n# {title}\n\n{body}\n",
        title = stub.title,
        ontotype = stub.kind.ontotype(),
    )
}

/// Write the hub notes that don't yet exist (when `apply`), returning the
/// report counts and the set of slugs whose hub note exists on disk afterward
/// (created this run or pre-existing). Pure filesystem work, no DB — testable
/// against a tempdir.
pub fn write_stubs(vault_root: &Path, stubs: &[HubStub], apply: bool, today: &str) -> Result<(HubReport, Vec<String>)> {
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
            // Create the note's OWN parent dir (not just `entities/`): repo hubs
            // nest at `entities/repos/<org>/<repo>.md`, so the intermediate
            // `<org>` dirs must exist before the write.
            if let Some(parent) = abs.parent() {
                std::fs::create_dir_all(parent).wrap_err_with(|| format!("create hub dir {}", parent.display()))?;
            }
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

        // Phase 12: --synthesize re-synthesizes each materialized hub's body
        // from its current membership (the note->hub edges). Loud fail-safe per
        // hub: a failed/empty pass preserves the prior body, never re-slugs or
        // deletes the hub. Only runs under --apply (it writes note bodies).
        if opts.synthesize && opts.apply {
            let synth = FabricHubSynthesizer {
                fabric: &config.fabric,
                pattern: HUB_SYNTH_PATTERN,
                timeout_secs: config.entities.fabric_timeout_secs,
                max_input_tokens: config.entities.max_input_tokens,
            };
            for stub in &stubs {
                let hub_rel = stub.hub_path();
                let hub_abs = vault_root.join(&hub_rel);
                if !hub_abs.exists() {
                    continue;
                }
                let members = index.hub_members(&hub_rel)?;
                match synthesize_hub(&hub_abs, &stub.title, &members, &synth)? {
                    SynthOutcome::Synthesized => report.synthesized += 1,
                    SynthOutcome::Preserved => report.synth_preserved += 1,
                }
            }
        }
    } else {
        log::warn!("cortex::hub: oracle index unavailable; skipped entities-table population");
        if opts.synthesize {
            log::warn!("cortex::hub: --synthesize needs the oracle index for membership; skipped");
        }
    }

    Ok(report)
}

/// Fabric pattern used to synthesize a hub body from its membership. Reuses the
/// general summarizer (a dedicated `synthesize-hub` pattern can supersede it).
const HUB_SYNTH_PATTERN: &str = "summarize";

/// Produces a hub body from its member note paths. Injected so the
/// failure-preservation logic is testable without a live LLM.
pub trait HubSynthesizer {
    fn synthesize(&self, hub_title: &str, members: &[String]) -> Result<String>;
}

/// Production synthesizer: runs a Fabric pattern over the member list.
pub struct FabricHubSynthesizer<'a> {
    pub fabric: &'a crate::config::FabricConfig,
    pub pattern: &'a str,
    pub timeout_secs: u64,
    pub max_input_tokens: usize,
}

impl HubSynthesizer for FabricHubSynthesizer<'_> {
    fn synthesize(&self, hub_title: &str, members: &[String]) -> Result<String> {
        let input = format!(
            "Hub subject: {hub_title}\n\nMember notes ({}):\n{}",
            members.len(),
            members.iter().map(|m| format!("- {m}")).collect::<Vec<_>>().join("\n")
        );
        let input = crate::fabric::truncate_input(&input, self.max_input_tokens);
        crate::fabric::run_pattern(self.fabric, self.pattern, input, self.timeout_secs)
    }
}

/// Outcome of a single hub synthesis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SynthOutcome {
    /// The body was re-synthesized and written.
    Synthesized,
    /// The synthesizer failed (or returned empty); the prior body is left
    /// byte-identical (loud warn, never a blank/partial overwrite).
    Preserved,
}

/// Split a note's raw text into `(frontmatter_block_incl_delimiters, body)`.
/// `None` when the note has no leading `---` frontmatter block.
fn split_frontmatter(raw: &str) -> Option<(&str, &str)> {
    let rest = raw.strip_prefix("---\n")?;
    let end = rest.find("\n---\n")?;
    let fm_end = "---\n".len() + end + "\n---\n".len();
    Some((&raw[..fm_end], &raw[fm_end..]))
}

/// Re-synthesize ONE hub's body from its current membership. Loud fail-safe:
/// on synthesizer error OR empty output, the prior body is left byte-identical
/// (never blank/partial). Rewrites the SAME file (never re-slugs or deletes
/// the hub); the frontmatter block is preserved verbatim, only the body below
/// it is replaced. `hub_abs` must exist.
pub fn synthesize_hub(
    hub_abs: &Path,
    hub_title: &str,
    members: &[String],
    synth: &impl HubSynthesizer,
) -> Result<SynthOutcome> {
    log::debug!(
        "cortex::hub::synthesize_hub: hub={} title={hub_title} members={}",
        hub_abs.display(),
        members.len()
    );
    // Fail-safe: a hub with ZERO wired members is NOT sent to the LLM. Feeding
    // an empty member list produced a hallucinated body ("no member notes were
    // provided" - observed live on the tatari-tv/okta-auth-py repo hub, 2026-07-20).
    // Leave the stub body byte-intact instead; a later sweep that wires members
    // will synthesize for real.
    if members.is_empty() {
        log::warn!(
            "cortex::hub::synthesize_hub: {} has ZERO members; skipping LLM synthesis (stub body left intact)",
            hub_abs.display()
        );
        return Ok(SynthOutcome::Preserved);
    }
    let raw = std::fs::read_to_string(hub_abs).wrap_err_with(|| format!("read hub {}", hub_abs.display()))?;
    let body = match synth.synthesize(hub_title, members) {
        Ok(b) if !b.trim().is_empty() => b,
        Ok(_) => {
            log::warn!(
                "cortex::hub::synthesize_hub: {} synthesized EMPTY; leaving prior body intact",
                hub_abs.display()
            );
            return Ok(SynthOutcome::Preserved);
        }
        Err(e) => {
            log::warn!(
                "cortex::hub::synthesize_hub: {} synthesis failed ({e:#}); leaving prior body intact",
                hub_abs.display()
            );
            return Ok(SynthOutcome::Preserved);
        }
    };
    let Some((fm_block, _old_body)) = split_frontmatter(&raw) else {
        log::warn!(
            "cortex::hub::synthesize_hub: {} has no frontmatter block; refusing to overwrite",
            hub_abs.display()
        );
        return Ok(SynthOutcome::Preserved);
    };
    let new_content = format!("{fm_block}\n{}\n", body.trim());
    std::fs::write(hub_abs, new_content).wrap_err_with(|| format!("write hub {}", hub_abs.display()))?;
    log::info!("cortex::hub::synthesize_hub: re-synthesized {}", hub_abs.display());
    Ok(SynthOutcome::Synthesized)
}

#[cfg(test)]
mod tests;
