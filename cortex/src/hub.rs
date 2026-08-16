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
use std::path::{Path, PathBuf};

use eyre::{Result, WrapErr};
use vault::search::SearchIndex;

use crate::config::{Config, RenderConfig};
use crate::opts::HubOpts;

pub mod render;

pub use render::{HubMember, Vector, render_hub_body};

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

/// Outcome of a hub pass. The body-pass fields are per-BRANCH counts, not one
/// "synthesized" total: a systematic member-load breakage has to be visible in
/// the run report instead of hiding inside a success count.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct HubReport {
    pub created: usize,
    pub existing: usize,
    /// Stubs that would be created (dry-run) or were created (apply).
    pub stubs: Vec<String>,
    pub entities_recorded: usize,
    /// Branch 3a: a body was rendered from membership and written.
    pub bodies_written: usize,
    /// Branch 3b: the rendered body was byte-identical to what is on disk.
    pub bodies_unchanged: usize,
    /// Branch 4a: nothing rendered, so the body was reset to the stub sentence.
    pub bodies_reset: usize,
    /// Branch 4b: nothing rendered and the stub sentence was already there.
    pub stubs_kept: usize,
    /// Branch 1: `hub-body: manual` - never touched.
    pub bodies_manual: usize,
    /// Branch 2: a member (or the hub file) could not be loaded, so the body was
    /// preserved byte-identical. An error never licenses a reset.
    pub bodies_preserved: usize,
    /// Member notes skipped across the run as missing/unreadable.
    pub members_skipped: usize,
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

/// Host of a note's `source:` URL — the ONE host implementation both the Source
/// hub stub and the graph pass's `source-member` edge read. Delegates to
/// `vault::search::extract_host` (which owns the parsing and its tests), so the
/// hub and graph layers cannot disagree about what a host is. Before this there
/// were two copies with different signatures: the hub side returned `None` on
/// schemeless input, the graph side lowercased it and passed it through, so the
/// hub minted nothing exactly where the graph yielded a bucket key.
///
/// `None` on schemeless input (`clyde://<uuid>`, bare provenance markers) is the
/// deliberate contract: `collect_stubs` cannot mint a hub for those, so an edge
/// pointed at one would be dropped forever by resolve-or-skip.
pub fn source_host(source: &str) -> Option<String> {
    vault::search::extract_host(source)
}

/// Vault-relative path of the Source hub for a note's `source:` URL, or `None`
/// when the source has no host (see [`source_host`]). Mirrors `repo_hub_path`:
/// the single place that turns a frontmatter value into a hub path, so the
/// `source-member` edge `dst` is byte-identical to the stub's `hub_path()` by
/// construction (pinned by `source_hub_path_matches_stub_hub_path`).
pub fn source_hub_path(source: &str) -> Option<String> {
    let slug = slugify(&source_host(source)?);
    if slug.is_empty() {
        return None;
    }
    Some(format!("{HUB_DIR}/{slug}.md"))
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
        // Multi-repo hubs (harvest-completion Phase 4): mint a Repo stub for
        // EVERY validated element of `repos-touched`, so a session touching
        // repos X+Y gets BOTH hubs stubbed and neither secondary-repo edge is
        // dropped silently (`insert_edges` skips an edge whose `dst` hub note
        // does not exist). Deduped against `frontmatter.repo` for free: the
        // `insert` closure is keyed on the `repo_hub_slug`, so a repo appearing
        // in both `repo` and `repos_touched` (or twice in `repos_touched`) maps
        // to one BTreeMap entry. Three-state honored by iterating only the
        // populated case: `None`/`Some(vec![])` mint nothing extra.
        if let Some(repos) = note.frontmatter.repos_touched.as_ref() {
            for repo in repos {
                if repo.is_empty() {
                    continue;
                }
                if vault::schema::validate_repo_slug(repo) {
                    insert(repo_hub_slug(repo), HubKind::Repo, repo.to_string());
                } else {
                    log::warn!(
                        "cortex::hub: note {} has malformed repos-touched slug {repo:?} - skipping repo hub edge (note still indexed)",
                        note.path.display()
                    );
                }
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

/// The stub SENTENCE for a hub: the body a hub carries when it has nothing to
/// say. Written at stub time and written back by the body builder's reset
/// branch, from this one function, so a freshly stubbed hub and a reset hub are
/// byte-identical and neither churns against the other. The repo variant carries
/// a LIVE self-link (`self_link`) that resolves to this hub's nested note.
fn stub_body(stub: &HubStub) -> String {
    match stub.kind {
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
    }
}

/// Assemble a hub note from its frontmatter block, title, and body text. THE
/// one composition seam: stub creation, a rendered body, and a stub reset all go
/// through it, so the byte-compare that makes re-runs write zero bytes cannot be
/// defeated by two call sites disagreeing about a newline.
fn compose_hub_content(fm_block: &str, title: &str, body: &str) -> String {
    format!("{fm_block}\n# {title}\n\n{}\n", body.trim())
}

/// Render a hub note's markdown (frontmatter + a short stub body).
fn render_hub(stub: &HubStub, today: &str) -> String {
    let fm_block = format!(
        "---\ntitle: {title}\ntype: entity\nontotype: {ontotype}\ndate: {today}\ntags: []\n---\n",
        title = stub.title,
        ontotype = stub.kind.ontotype(),
    );
    compose_hub_content(&fm_block, &stub.title, &stub_body(stub))
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
            vault::note::write_atomic(&abs, render_hub(stub, today).as_bytes())
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

    // The `entities`-table upsert is APPLY-ONLY. It used to run whenever the
    // index opened, so a flagless `sb cortex hub` presented as a dry run while
    // writing oracle's `entities` table (and creating the DB file if absent).
    // Truth in naming: a dry run writes nothing anywhere, so a dry run does not
    // even open the index.
    if !opts.apply {
        log::info!("cortex::hub: dry run - no vault write, no entities-table upsert (pass --apply)");
        if opts.synthesize {
            log::warn!("cortex::hub: --synthesize writes note bodies and needs --apply; skipped");
        }
        return Ok(report);
    }

    match SearchIndex::open(&config.oracle_db_path()) {
        Ok(index) => {
            report.entities_recorded = populate_entities(&index, &stubs, &materialized)?;
            // --synthesize rebuilds each materialized hub's body from its
            // current DELIBERATE membership. Deterministic assembly of the
            // members' distilled claims - no Fabric call anywhere on this path.
            if opts.synthesize {
                let bodies = build_hub_bodies(vault_root, &index, &stubs, &config.entities.render)?;
                report.apply_body_stats(&bodies);
            }
        }
        Err(e) => {
            log::warn!("cortex::hub: oracle index unavailable ({e:#}); skipped entities-table population");
            if opts.synthesize {
                log::warn!("cortex::hub: --synthesize needs the oracle index for membership; skipped");
            }
        }
    }

    Ok(report)
}

/// Frontmatter key that marks a hub body as genuinely human-authored. A hub
/// body WITHOUT it is builder-owned and will be rewritten or reset without
/// warning, this run or any future one; `hub-body: manual` is the one way to
/// opt out, and vault git is the backstop. (`hub-synthesized:` grants nothing -
/// it is a provenance stamp of the 2026-07-02 Fabric pass, and those bodies are
/// generated prose the deterministic render supersedes.)
pub const MANUAL_BODY_KEY: &str = "hub-body";

/// The value `hub-body:` must carry to protect a body.
pub const MANUAL_BODY_VALUE: &str = "manual";

/// True when a hub note is marked `hub-body: manual`.
pub fn is_manual_body(raw: &str) -> bool {
    let Ok((fm, _)) = vault::frontmatter::parse_frontmatter(raw) else {
        return false;
    };
    matches!(
        fm.extra.get(MANUAL_BODY_KEY),
        Some(serde_yaml::Value::String(v)) if v.trim() == MANUAL_BODY_VALUE
    )
}

/// True when a body was produced by THIS builder: its first H2 is `## Summary`,
/// which only the renderer emits. Stub bodies (no heading at all) and the legacy
/// refusal bodies are excluded, which is what makes the run-level reset backstop
/// meaningful - the first run's ~124 refusal resets are expected and must not
/// trip it.
pub fn body_is_rendered(body: &str) -> bool {
    body.lines()
        .find(|line| line.starts_with("## "))
        .is_some_and(|line| line.trim() == "## Summary")
}

/// Outcome of deciding one hub's body. Four write branches, because "nothing to
/// say" and "could not find out" are different conditions: collapsing them lets
/// a vault-root misconfig mass-reset every builder-owned hub in one silent run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SynthOutcome {
    /// Branch 1: `hub-body: manual` - not touched.
    Manual,
    /// Branch 2: a member (or the hub file itself) could not be loaded. The body
    /// is left byte-identical and the hub is counted FAILED. An error never
    /// licenses a reset.
    Preserved,
    /// Branch 3: the body rendered and differs from what is on disk.
    Rendered,
    /// Branch 3: the body rendered and is byte-identical - nothing is written.
    Unchanged,
    /// Branch 4: nothing rendered, so the body is reset to the stub sentence.
    Reset,
    /// Branch 4: nothing rendered and the stub sentence is already there.
    StubKept,
}

/// One hub's decided outcome plus the exact bytes to write (`None` = no write).
#[derive(Debug, Clone, PartialEq)]
pub struct HubPlan {
    pub outcome: SynthOutcome,
    pub content: Option<String>,
    /// The body currently on disk was produced by the renderer. Feeds the
    /// run-level reset backstop.
    pub previously_rendered: bool,
}

impl HubPlan {
    fn preserved(previously_rendered: bool) -> Self {
        Self {
            outcome: SynthOutcome::Preserved,
            content: None,
            previously_rendered,
        }
    }
}

/// Decide what happens to ONE hub file. Pure: given the file's current bytes,
/// the rendered body (`None` = nothing to say), the stub sentence, and whether
/// any member failed to load, it returns the branch and the bytes.
///
/// Frontmatter is preserved verbatim in every write; a note without a
/// frontmatter block is never overwritten.
pub fn plan_hub_body(
    raw: &str,
    title: &str,
    rendered: Option<&str>,
    stub_sentence: &str,
    load_failed: bool,
) -> HubPlan {
    let previously_rendered = body_is_rendered(raw);
    if is_manual_body(raw) {
        return HubPlan {
            outcome: SynthOutcome::Manual,
            content: None,
            previously_rendered,
        };
    }
    if load_failed {
        return HubPlan::preserved(previously_rendered);
    }
    let Some((fm_block, _old_body)) = split_frontmatter(raw) else {
        log::warn!("cortex::hub: {title} has no frontmatter block; refusing to overwrite");
        return HubPlan::preserved(previously_rendered);
    };
    let (body, hit, miss) = match rendered {
        Some(body) => (body, SynthOutcome::Rendered, SynthOutcome::Unchanged),
        None => (stub_sentence, SynthOutcome::Reset, SynthOutcome::StubKept),
    };
    let content = compose_hub_content(fm_block, title, body);
    if content == raw {
        HubPlan {
            outcome: miss,
            content: None,
            previously_rendered,
        }
    } else {
        HubPlan {
            outcome: hit,
            content: Some(content),
            previously_rendered,
        }
    }
}

/// Load one member note for the renderer. A read or parse failure is an ERROR,
/// not an empty member: the caller turns it into the preserve-the-body branch.
pub fn load_hub_member(vault_root: &Path, rel_path: &str) -> Result<HubMember> {
    let abs = vault_root.join(rel_path);
    let raw = std::fs::read_to_string(&abs).wrap_err_with(|| format!("read hub member {}", abs.display()))?;
    let (fm, body) =
        vault::frontmatter::parse_frontmatter(&raw).wrap_err_with(|| format!("parse hub member {}", abs.display()))?;
    let title = fm
        .title
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| stem_of(rel_path));
    Ok(HubMember {
        path: rel_path.to_string(),
        title,
        note_type: fm.note_type.unwrap_or_default(),
        date: fm.date,
        claims: vault::search::parse_body_claims(&body),
    })
}

/// Filename stem of a vault-relative path (the display fallback for a member
/// note with no `title:`).
fn stem_of(rel_path: &str) -> String {
    Path::new(rel_path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| rel_path.to_string())
}

/// One hub the builder decided about, kept with its target path so every
/// outcome can be computed BEFORE any byte is written.
#[derive(Debug, Clone)]
struct PlannedHub {
    rel: String,
    abs: PathBuf,
    plan: HubPlan,
}

/// Result of the body pass: one plan per materialized hub plus the count of
/// member notes that could not be loaded.
#[derive(Debug)]
struct BodyPass {
    planned: Vec<PlannedHub>,
    members_skipped: usize,
}

/// Rebuild every materialized hub's body from its deliberate membership.
///
/// Computes ALL outcomes first, then writes. That ordering is the run-level
/// backstop for the one failure branch 2 cannot see: `parse_body_claims` is
/// infallible (a malformed body yields an empty Vec indistinguishable from
/// no-claims), so a claim-parse regression would "succeed" everywhere with zero
/// claims and reset the whole hub layer to stubs in one silent pass. If the
/// resets of previously-RENDERED bodies exceed `max-render-resets-per-run`, the
/// run aborts loudly and writes nothing.
fn build_hub_bodies(
    vault_root: &Path,
    index: &SearchIndex,
    stubs: &[HubStub],
    caps: &RenderConfig,
) -> Result<BodyPass> {
    let mut pass = BodyPass {
        planned: Vec::new(),
        members_skipped: 0,
    };
    for stub in stubs {
        let rel = stub.hub_path();
        let abs = vault_root.join(&rel);
        if !abs.exists() {
            continue;
        }
        let raw = match std::fs::read_to_string(&abs) {
            Ok(raw) => raw,
            Err(e) => {
                log::warn!("cortex::hub: cannot read hub {} ({e}); body preserved", abs.display());
                pass.planned.push(PlannedHub {
                    rel,
                    abs,
                    plan: HubPlan::preserved(false),
                });
                continue;
            }
        };
        let mut members = Vec::new();
        let mut load_failed = false;
        for member_rel in index.hub_members_deliberate(&rel)? {
            match load_hub_member(vault_root, &member_rel) {
                Ok(member) => members.push(member),
                Err(e) => {
                    log::warn!("cortex::hub: skipping member {member_rel} of {rel} ({e:#})");
                    pass.members_skipped += 1;
                    load_failed = true;
                }
            }
        }
        let rendered = if load_failed { None } else { render_hub_body(&stub.title, &members, caps) };
        let plan = plan_hub_body(&raw, &stub.title, rendered.as_deref(), &stub_body(stub), load_failed);
        pass.planned.push(PlannedHub { rel, abs, plan });
    }

    let resets: Vec<&str> = pass
        .planned
        .iter()
        .filter(|p| p.plan.outcome == SynthOutcome::Reset && p.plan.previously_rendered)
        .map(|p| p.rel.as_str())
        .collect();
    if resets.len() > caps.max_render_resets_per_run {
        eyre::bail!(
            "cortex::hub: {} previously-rendered hub bodies would reset to stubs (max {}); wrote nothing. \
             This is the claim-parse-regression backstop - fix the regression it indicates, or raise \
             entities.render.max-render-resets-per-run when the resets are genuinely intended. Hubs: {}",
            resets.len(),
            caps.max_render_resets_per_run,
            resets.join(", ")
        );
    }

    for hub in &pass.planned {
        let Some(content) = hub.plan.content.as_deref() else {
            continue;
        };
        // Atomic, always: this pass rewrites hundreds of files on a Syncthing'd
        // vault, where a torn write propagates to every machine. Each file write
        // is atomic, so a failure mid-pass leaves every already-written hub
        // complete and the next run resumes idempotently (the byte compare skips
        // the ones already done).
        vault::note::write_atomic(&hub.abs, content.as_bytes())
            .wrap_err_with(|| format!("write hub body {}", hub.abs.display()))?;
        log::info!("cortex::hub: {:?} {}", hub.plan.outcome, hub.rel);
    }
    Ok(pass)
}

/// Split a note's raw text into `(frontmatter_block_incl_delimiters, body)`.
/// `None` when the note has no leading `---` frontmatter block.
fn split_frontmatter(raw: &str) -> Option<(&str, &str)> {
    let rest = raw.strip_prefix("---\n")?;
    let end = rest.find("\n---\n")?;
    let fm_end = "---\n".len() + end + "\n---\n".len();
    Some((&raw[..fm_end], &raw[fm_end..]))
}

impl HubReport {
    /// Fold one body pass's per-branch outcomes into the run report.
    fn apply_body_stats(&mut self, pass: &BodyPass) {
        self.members_skipped += pass.members_skipped;
        for hub in &pass.planned {
            match hub.plan.outcome {
                SynthOutcome::Manual => self.bodies_manual += 1,
                SynthOutcome::Preserved => self.bodies_preserved += 1,
                SynthOutcome::Rendered => self.bodies_written += 1,
                SynthOutcome::Unchanged => self.bodies_unchanged += 1,
                SynthOutcome::Reset => self.bodies_reset += 1,
                SynthOutcome::StubKept => self.stubs_kept += 1,
            }
        }
    }
}

#[cfg(test)]
mod tests;
