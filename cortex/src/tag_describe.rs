//! `sb cortex tag-describe`: draft a vocabulary tag's description from notes
//! that carry it, cache the draft for review, and write the cached draft into
//! `canonical-tags.yml` on `--apply` (design doc
//! `2026-09-25-described-tag-classification.md`, Phase 4).
//!
//! A dry run drafts (or takes `--set` wording) and caches; `--apply` writes the
//! cache and never drafts, so the description Scott reviewed is the one that
//! lands. Protect-list tags (`no-classifier-tags`) refuse an LLM draft unless
//! `--allow-protected` is passed; `--set` wording is always allowed.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use eyre::{Context, Result};
use serde::{Deserialize, Serialize};
use vault::canonical::{CanonicalTagsFile, TagEntry};
use vault::note::Note;

use crate::config::{Config, FabricConfig};

/// The fabric pattern that drafts a description (`borg/patterns/describe-tag.md`).
pub const DESCRIBE_TAG_PATTERN: &str = "describe-tag";

/// Sample notes per draft when `--samples` is not given. The pattern asks for
/// 5-10; Phase 0 drafted from up to 8.
pub const DEFAULT_SAMPLES: usize = 8;

/// The largest `--samples` accepted.
pub const MAX_SAMPLES: usize = 10;

/// What a `tag-describe` run was asked to do.
#[derive(Debug, Clone, Default)]
pub struct DescribeOpts {
    /// Tags to draft (dry run) or write (apply). `--set` tags need not repeat here.
    pub tags: Vec<String>,
    pub samples: usize,
    /// Vault-relative paths never used as samples (the gold sets).
    pub exclude: HashSet<String>,
    /// `--set TAG=TEXT`: Scott's own wording, used instead of a draft.
    pub set: Vec<(String, String)>,
    pub allow_protected: bool,
}

/// Where a cached description came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DraftSource {
    /// Drafted by the `describe-tag` pattern from `samples`.
    Fabric,
    /// Given on the command line with `--set`.
    Set,
}

/// One cached draft, `<cache_dir>/<tag>.json`. `samples` are the vault paths
/// the description was drafted from, so the P5 confirmation harness can
/// assert they are disjoint from both gold sets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedDraft {
    pub tag: String,
    pub group: String,
    pub description: String,
    pub source: DraftSource,
    pub samples: Vec<String>,
}

impl CachedDraft {
    pub fn path(cache_dir: &Path, tag: &str) -> PathBuf {
        cache_dir.join(format!("{tag}.json"))
    }

    /// `pub(crate)`: `proposals::promote_tags` (Phase 7) reuses this cache
    /// format and directory for a promoted tag's draft, so a promotion's
    /// dry-run and `--apply` share the same cache-then-write contract this
    /// module established.
    pub(crate) fn save(&self, cache_dir: &Path) -> Result<PathBuf> {
        std::fs::create_dir_all(cache_dir).with_context(|| format!("failed to create {}", cache_dir.display()))?;
        let path = Self::path(cache_dir, &self.tag);
        let json = serde_json::to_string_pretty(self).wrap_err("failed to serialize draft")?;
        vault::note::write_atomic(&path, json.as_bytes())
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(path)
    }

    /// Read `tag`'s cached draft, held to the same description rules the
    /// vocabulary parser enforces. A missing cache is an error naming the
    /// dry run that makes one. `pub(crate)`, same reason as `save`.
    pub(crate) fn load(cache_dir: &Path, tag: &str) -> Result<Self> {
        let path = Self::path(cache_dir, tag);
        if !path.exists() {
            eyre::bail!(
                "no cached draft for {tag:?} at {}; run `sb cortex tag-describe {tag}` first and review it",
                path.display()
            );
        }
        let text = std::fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
        let draft: Self = serde_json::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))?;
        if draft.tag != tag {
            eyre::bail!("{} holds a draft for {:?}, not {tag:?}", path.display(), draft.tag);
        }
        TagEntry::described(&draft.tag, &draft.description)
            .with_context(|| format!("cached draft {} breaks the description rules", path.display()))?;
        Ok(draft)
    }
}

/// One sample note as the drafter sees it: its path, and its title and
/// summary through `clip_classifier_text`, the same string the classifier
/// scores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    pub path: String,
    pub text: String,
}

/// Everything the drafter is given for one tag.
#[derive(Debug, Clone)]
pub struct DescribeRequest {
    pub tag: String,
    pub group: String,
    /// The other entries of `group`, described or bare.
    pub siblings: Vec<TagEntry>,
    pub samples: Vec<Sample>,
}

impl DescribeRequest {
    /// The pattern's input text.
    pub fn input(&self) -> String {
        let mut out = format!(
            "TAG: {}\nGROUP: {}\n\nSIBLINGS (the other tags in this group):\n",
            self.tag, self.group
        );
        if self.siblings.is_empty() {
            out.push_str("(none)\n");
        }
        for sibling in &self.siblings {
            out.push_str(&format!("- {}\n", sibling.yaml_line()));
        }
        out.push_str(&format!("\nSAMPLE NOTES ({}):\n", self.samples.len()));
        if self.samples.is_empty() {
            out.push_str("(none: no note outside the excluded set carries this tag)\n");
        }
        for (i, sample) in self.samples.iter().enumerate() {
            out.push_str(&format!("\n### {}\n{}\n", i + 1, sample.text));
        }
        out
    }
}

/// Drafts one description. Injected so the command is testable without a
/// live LLM (per the DI convention).
pub trait TagDrafter {
    fn draft(&self, request: &DescribeRequest) -> Result<String>;
}

/// Production drafter: runs the `describe-tag` fabric pattern.
pub struct FabricDrafter<'a> {
    pub fabric: &'a FabricConfig,
    pub pattern: &'a str,
    pub timeout_secs: u64,
}

impl TagDrafter for FabricDrafter<'_> {
    fn draft(&self, request: &DescribeRequest) -> Result<String> {
        let output = crate::fabric::run_pattern(self.fabric, self.pattern, &request.input(), self.timeout_secs)?;
        parse_description(&request.tag, &output)
    }
}

#[derive(Deserialize)]
struct DrafterOutput {
    description: String,
}

/// Read the pattern's `{"description": "..."}` answer, tolerating a code
/// fence or prose around the object, and hold it to the vocabulary's entry
/// rules. A description that breaks them is an error, never trimmed to fit.
pub fn parse_description(tag: &str, output: &str) -> Result<String> {
    let preview: String = output.chars().take(200).collect();
    let (Some(start), Some(end)) = (output.find('{'), output.rfind('}')) else {
        eyre::bail!("drafter output for {tag:?} holds no JSON object: {preview:?}");
    };
    if end < start {
        eyre::bail!("drafter output for {tag:?} holds no JSON object: {preview:?}");
    }
    let parsed: DrafterOutput = serde_json::from_str(&output[start..=end])
        .with_context(|| format!("drafter output for {tag:?} is not `{{\"description\": ...}}`: {preview:?}"))?;
    let description = parsed.description.trim().to_string();
    TagEntry::described(tag, &description)?;
    Ok(description)
}

/// Read an `--exclude-from` file: one vault-relative path per line, blank
/// lines skipped. An absolute path is an error, because note paths are
/// vault-relative and it would silently exclude nothing.
pub fn load_exclude_list(path: &Path) -> Result<HashSet<String>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut out = HashSet::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if Path::new(line).is_absolute() {
            eyre::bail!(
                "{} line {}: {line:?} is absolute; list vault-relative paths (e.g. notes/foo.md)",
                path.display(),
                i + 1
            );
        }
        out.insert(line.trim_start_matches("./").to_string());
    }
    log::debug!(
        "tag_describe::load_exclude_list: {} path(s) from {}",
        out.len(),
        path.display()
    );
    Ok(out)
}

/// The notes a draft is written from: those carrying `tag`, minus `exclude`,
/// newest first by `date` (then `ingested`), ties broken by path so a re-run
/// picks the same set.
pub fn select_samples<'a>(notes: &'a [Note], tag: &str, exclude: &HashSet<String>, n: usize) -> Vec<&'a Note> {
    let mut picked: Vec<&Note> = notes
        .iter()
        .filter(|note| note.frontmatter.tags.as_deref().unwrap_or(&[]).iter().any(|t| t == tag))
        .filter(|note| !exclude.contains(note.path.to_string_lossy().as_ref()))
        .collect();
    let when = |note: &Note| {
        note.frontmatter
            .date
            .clone()
            .or_else(|| note.frontmatter.ingested.clone())
            .unwrap_or_default()
    };
    picked.sort_by(|a, b| when(b).cmp(&when(a)).then_with(|| a.path.cmp(&b.path)));
    picked.truncate(n);
    picked
}

/// `pub(crate)`: `proposals::promote_tags` (Phase 7) builds the same shape of
/// sample for a note-derived proposal source.
pub(crate) fn sample_of(note: &Note) -> Sample {
    Sample {
        path: note.path.to_string_lossy().to_string(),
        text: distillers::tags::clip_classifier_text(
            crate::classify::note_title(note),
            &crate::classify::classifier_text(note),
        ),
    }
}

/// One tag the run acts on.
#[derive(Debug, Clone)]
struct Target {
    slug: String,
    group: String,
    siblings: Vec<TagEntry>,
    protected: bool,
    set: Option<String>,
}

/// Resolve every requested tag against the vocabulary before anything is
/// drafted or written: an unknown tag fails the whole run.
fn resolve_targets(file: &CanonicalTagsFile, canonical_path: &Path, opts: &DescribeOpts) -> Result<Vec<Target>> {
    let mut slugs: Vec<String> = Vec::new();
    for slug in opts.tags.iter().chain(opts.set.iter().map(|(t, _)| t)) {
        if !slugs.contains(slug) {
            slugs.push(slug.clone());
        }
    }
    if slugs.is_empty() {
        eyre::bail!("no tags given: name at least one tag, or pass --set TAG=TEXT");
    }
    let protected: HashSet<&String> = file.no_classifier_tags.iter().collect();
    let mut targets = Vec::new();
    for slug in slugs {
        let homes: Vec<(&String, &Vec<TagEntry>)> = file
            .tags
            .iter()
            .filter(|(_, entries)| entries.iter().any(|e| e.slug == slug))
            .collect();
        let (group, entries) = match homes.as_slice() {
            [one] => *one,
            [] => eyre::bail!("tag {slug:?} is not in {}", canonical_path.display()),
            many => {
                let mut groups: Vec<&String> = many.iter().map(|(g, _)| *g).collect();
                groups.sort();
                eyre::bail!(
                    "tag {slug:?} is in more than one group of {}: {groups:?}",
                    canonical_path.display()
                );
            }
        };
        let set = opts.set.iter().rev().find(|(t, _)| *t == slug).map(|(_, d)| d.clone());
        targets.push(Target {
            siblings: entries.iter().filter(|e| e.slug != slug).cloned().collect(),
            group: group.clone(),
            protected: protected.contains(&slug),
            slug,
            set,
        });
    }
    Ok(targets)
}

/// Refuse an LLM-drafted description for a protect-list tag unless
/// `--allow-protected` was passed: the classifier is weakest on those tags, so
/// their wording should be the operator's own.
fn ensure_drafting_allowed(target: &Target, canonical_path: &Path, allow_protected: bool) -> Result<()> {
    if target.protected && !allow_protected {
        eyre::bail!(
            "tag {slug:?} is on the protect list (`no-classifier-tags` in {path}); write its description yourself with \
             --set {slug}=TEXT, or pass --allow-protected to use a drafted one",
            slug = target.slug,
            path = canonical_path.display()
        );
    }
    Ok(())
}

fn read_vocabulary(canonical_path: &Path) -> Result<(String, CanonicalTagsFile)> {
    let text = std::fs::read_to_string(canonical_path)
        .with_context(|| format!("failed to read {}", canonical_path.display()))?;
    let file: CanonicalTagsFile =
        serde_yaml::from_str(&text).with_context(|| format!("failed to parse {}", canonical_path.display()))?;
    Ok((text, file))
}

/// A description that was drafted, set, or written, and where its draft is cached.
#[derive(Debug, Clone)]
pub struct Described {
    pub draft: CachedDraft,
    pub cache_path: PathBuf,
}

/// Outcome of one `tag-describe` run.
#[derive(Debug, Clone, Default)]
pub struct DescribeReport {
    /// True when `canonical-tags.yml` was written.
    pub applied: bool,
    pub described: Vec<Described>,
    /// Tags whose draft failed, with the error. A dry run keeps drafting the
    /// rest; the caller exits non-zero when this is non-empty.
    pub failed: Vec<(String, String)>,
}

/// Dry run: draft (or take `--set` wording for) each tag, check it would land
/// in the file, and cache it. Writes only the cache, never `canonical_path`.
pub fn draft_descriptions(
    canonical_path: &Path,
    cache_dir: &Path,
    notes: &[Note],
    opts: &DescribeOpts,
    drafter: &dyn TagDrafter,
) -> Result<DescribeReport> {
    log::debug!(
        "tag_describe::draft_descriptions: tags={:?} set={} samples={} exclude={} canonical={}",
        opts.tags,
        opts.set.len(),
        opts.samples,
        opts.exclude.len(),
        canonical_path.display()
    );
    let (text, file) = read_vocabulary(canonical_path)?;
    let targets = resolve_targets(&file, canonical_path, opts)?;
    for target in targets.iter().filter(|t| t.set.is_none()) {
        ensure_drafting_allowed(target, canonical_path, opts.allow_protected)?;
    }

    let mut report = DescribeReport::default();
    for target in targets {
        let slug = target.slug.clone();
        match draft_one(&text, &target, notes, opts, drafter) {
            Ok(draft) => {
                let cache_path = draft.save(cache_dir)?;
                log::info!("tag_describe: drafted {slug:?} -> {}", cache_path.display());
                report.described.push(Described { draft, cache_path });
            }
            Err(e) => {
                log::warn!("tag_describe: draft for {slug:?} failed: {e:#}");
                report.failed.push((slug, format!("{e:#}")));
            }
        }
    }
    Ok(report)
}

fn draft_one(
    text: &str,
    target: &Target,
    notes: &[Note],
    opts: &DescribeOpts,
    drafter: &dyn TagDrafter,
) -> Result<CachedDraft> {
    let (description, source, samples) = match &target.set {
        Some(wording) => (wording.clone(), DraftSource::Set, Vec::new()),
        None => {
            let samples: Vec<Sample> = select_samples(notes, &target.slug, &opts.exclude, opts.samples)
                .into_iter()
                .map(sample_of)
                .collect();
            let request = DescribeRequest {
                tag: target.slug.clone(),
                group: target.group.clone(),
                siblings: target.siblings.clone(),
                samples,
            };
            let description = drafter.draft(&request)?;
            let paths = request.samples.into_iter().map(|s| s.path).collect();
            (description, DraftSource::Fabric, paths)
        }
    };
    // Prove the draft lands before caching it: the rules hold and the entry
    // line is in a form `--apply` can rewrite.
    crate::proposals::describe_entry_in_group(text, &target.group, &target.slug, &description)?;
    Ok(CachedDraft {
        tag: target.slug.clone(),
        group: target.group.clone(),
        description,
        source,
        samples,
    })
}

/// `--apply`: write each tag's cached draft (or its `--set` wording) into
/// `canonical_path` through `describe_entry_in_group`. Never drafts. Every
/// tag must have its text before anything is written; the edited buffer is
/// parsed as a whole file before one atomic write.
pub fn apply_descriptions(canonical_path: &Path, cache_dir: &Path, opts: &DescribeOpts) -> Result<DescribeReport> {
    log::debug!(
        "tag_describe::apply_descriptions: tags={:?} set={} canonical={}",
        opts.tags,
        opts.set.len(),
        canonical_path.display()
    );
    let (text, file) = read_vocabulary(canonical_path)?;
    let targets = resolve_targets(&file, canonical_path, opts)?;

    let mut drafts = Vec::new();
    for target in &targets {
        let draft = match &target.set {
            Some(wording) => CachedDraft {
                tag: target.slug.clone(),
                group: target.group.clone(),
                description: wording.clone(),
                source: DraftSource::Set,
                samples: Vec::new(),
            },
            None => {
                let draft = CachedDraft::load(cache_dir, &target.slug)?;
                if draft.source == DraftSource::Fabric {
                    ensure_drafting_allowed(target, canonical_path, opts.allow_protected)?;
                }
                draft
            }
        };
        drafts.push((target, draft));
    }

    let mut updated = text;
    for (target, draft) in &drafts {
        updated = crate::proposals::describe_entry_in_group(&updated, &target.group, &target.slug, &draft.description)?;
    }
    serde_yaml::from_str::<CanonicalTagsFile>(&updated).with_context(|| {
        format!(
            "the edited {} would not parse; refusing to write",
            canonical_path.display()
        )
    })?;
    vault::note::write_atomic(canonical_path, updated.as_bytes())
        .with_context(|| format!("failed to write {}", canonical_path.display()))?;

    let mut report = DescribeReport {
        applied: true,
        ..Default::default()
    };
    for (_, mut draft) in drafts {
        // The group the entry was written into, which may differ from the
        // one it was drafted under if the tag moved since.
        draft.group = targets
            .iter()
            .find(|t| t.slug == draft.tag)
            .map(|t| t.group.clone())
            .unwrap_or(draft.group);
        let cache_path = if draft.source == DraftSource::Set {
            draft.save(cache_dir)?
        } else {
            CachedDraft::path(cache_dir, &draft.tag)
        };
        report.described.push(Described { draft, cache_path });
    }
    log::info!(
        "tag_describe: wrote {} description(s) into {}",
        report.described.len(),
        canonical_path.display()
    );
    Ok(report)
}

/// The command's production entry for a dry run: scan the vault, keep the
/// notes the vault config governs (`vault.exclude` / `vault.include`, which
/// keeps quarantine copies out), and draft through fabric.
pub fn run_draft(
    vault_root: &Path,
    config: &Config,
    canonical_path: &Path,
    cache_dir: &Path,
    opts: &DescribeOpts,
) -> Result<DescribeReport> {
    let notes = crate::vault::scan_vault(vault_root, &config.vault)?;
    let exclude_patterns = crate::parse_patterns(&config.vault.exclude);
    let include_patterns = crate::parse_patterns(&config.vault.include);
    let governed: Vec<Note> = notes
        .into_iter()
        .filter(|n| !crate::is_excluded(n, &exclude_patterns, &include_patterns))
        .collect();
    log::debug!("tag_describe::run_draft: {} governed note(s)", governed.len());
    let drafter = FabricDrafter {
        fabric: &config.fabric,
        pattern: DESCRIBE_TAG_PATTERN,
        timeout_secs: config.fabric.timeout_secs,
    };
    draft_descriptions(canonical_path, cache_dir, &governed, opts, &drafter)
}

#[cfg(test)]
mod tests;
