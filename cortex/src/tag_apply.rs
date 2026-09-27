//! `sb cortex tag-apply`: add a deployed vocabulary tag to the existing notes
//! it belongs on (design doc `2026-09-25-described-tag-classification.md`,
//! Phase 8).
//!
//! Promotion only reaches future ingests, so a newly promoted tag lands on no
//! note that already exists. This verb scores candidate notes with the
//! production classifier.dev call (full vocabulary, both shards) and reads
//! the target slug's own, uncapped score, so a tag that ranks ninth on a note
//! is still visible. Every score is paid for once: it lives in a per-tag
//! cursor, and a run that stops at the day's allowance resumes on the next.
//!
//! A dry run scores and caches; `--apply` first scores whatever the day
//! still allows, then writes from the cursor. The write only ever adds the
//! tag, and only to a note with room under `max-per-note`, judged against
//! the note's frontmatter as it is on disk at write time.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use eyre::{Context, Result};
use serde::{Deserialize, Serialize};
use vault::canonical::{self, CanonicalSet, CanonicalTagsFile};
use vault::note::Note;

use crate::config::Config;
use crate::scope::insert_frontmatter_fields;
use distillers::tags::bulk::{BulkItem, BulkOutcome, BulkRun, ScoreCursor, wire_text_hash};
use distillers::tags::spend::{DaySpend, SpendLedger, SystemClock};
use distillers::tags::{ClassifierDev, TagInput};

/// The ledger's `path` field for this verb's rows.
pub const LEDGER_PATH: &str = "tag-apply";

/// Which notes a cursor scores. Widening (`screen:300` -> `screen:600` ->
/// `all`) extends the candidate list and keeps every still-valid score;
/// narrowing keeps the wider list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum CandidateMode {
    /// The top N notes by summary cosine to the tag, plus every note whose
    /// content holds the slug as a phrase.
    Screen(usize),
    /// Every eligible note.
    All,
}

impl CandidateMode {
    /// The wider of two modes: `all` over any screen, the larger screen
    /// over a smaller one.
    pub fn widest(self, other: Self) -> Self {
        match (self, other) {
            (Self::All, _) | (_, Self::All) => Self::All,
            (Self::Screen(a), Self::Screen(b)) => Self::Screen(a.max(b)),
        }
    }
}

impl std::fmt::Display for CandidateMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Screen(n) => write!(f, "screen:{n}"),
            Self::All => write!(f, "all"),
        }
    }
}

impl From<CandidateMode> for String {
    fn from(mode: CandidateMode) -> Self {
        mode.to_string()
    }
}

impl TryFrom<String> for CandidateMode {
    type Error = String;

    fn try_from(s: String) -> std::result::Result<Self, Self::Error> {
        if s == "all" {
            return Ok(Self::All);
        }
        match s.strip_prefix("screen:").map(str::parse::<usize>) {
            Some(Ok(n)) if n > 0 => Ok(Self::Screen(n)),
            _ => Err(format!("{s:?} is not a candidate mode (`all` or `screen:N`, N > 0)")),
        }
    }
}

/// `~/.local/share/sb/cortex/tag-apply/<tag>.json`: the candidates a tag is
/// being applied over and every score paid for. `scores` and `failed` are
/// the shared bulk cursor, flattened beside `tag`, `candidate_mode` and
/// `candidates`, so the file reads as one object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TagApplyCursor {
    pub tag: String,
    pub candidate_mode: CandidateMode,
    pub candidates: Vec<String>,
    #[serde(flatten)]
    pub scores: ScoreCursor,
}

impl TagApplyCursor {
    pub fn path(dir: &Path, tag: &str) -> PathBuf {
        dir.join(format!("{tag}.json"))
    }

    /// Read a cursor, `None` when the tag has none yet. One that does not
    /// parse is an error: reading it as empty would pay for every score again.
    pub fn load(path: &Path) -> Result<Option<Self>> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .map(Some)
                .wrap_err_with(|| format!("failed to parse the tag-apply cursor {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).wrap_err_with(|| format!("failed to read {}", path.display())),
        }
    }

    /// Write atomically, so a run killed mid-write leaves the last cursor.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).wrap_err_with(|| format!("failed to create {}", dir.display()))?;
        }
        let text = serde_json::to_string_pretty(self)?;
        vault::note::write_atomic(path, text.as_bytes()).wrap_err_with(|| format!("failed to write {}", path.display()))
    }
}

/// The screen's query text: `"<tag>: <description>"`, or the bare slug for
/// an undescribed tag.
pub fn screen_query(tag: &str, canon: &CanonicalSet) -> String {
    match canon.descriptions.get(tag) {
        Some(description) => format!("{tag}: {description}"),
        None => tag.to_string(),
    }
}

/// Port over the candidate screen, so tests need no embedding model.
pub trait CandidateScreen {
    /// The top `n` notes by summary cosine to `query`, unioned with every
    /// note whose title, body, or summary holds `slug` as a phrase.
    fn screen(&self, query: &str, slug: &str, n: usize) -> Result<Vec<String>>;
}

/// The screen over oracle's search index: summary-kind vectors only, since
/// `search_vector` pools every kind and a note's transcript chunks would get
/// extra chances, plus the content-column phrase match.
pub struct IndexScreen {
    pub db_path: PathBuf,
}

impl CandidateScreen for IndexScreen {
    fn screen(&self, query: &str, slug: &str, n: usize) -> Result<Vec<String>> {
        let index = vault::search::SearchIndex::open(&self.db_path)
            .wrap_err_with(|| format!("failed to open search index at {}", self.db_path.display()))?;
        let model = index.active_embedding_model()?;
        let query_vec = vault::embedding::embed_query(query, &model)
            .wrap_err_with(|| format!("failed to embed the screen query {query:?}"))?;
        let limit = u32::try_from(n).unwrap_or(u32::MAX);
        let mut paths: Vec<String> = index
            .search_summary_vectors(&query_vec, limit)?
            .into_iter()
            .map(|hit| hit.note_path)
            .collect();
        let vector_hits = paths.len();
        let mut seen: HashSet<String> = paths.iter().cloned().collect();
        for path in index.content_phrase_matches(slug)? {
            if seen.insert(path.clone()) {
                paths.push(path);
            }
        }
        log::info!(
            "IndexScreen::screen: {vector_hits} by summary cosine, {} more by phrase match on {slug:?}",
            paths.len() - vector_hits
        );
        Ok(paths)
    }
}

/// What a run was asked to do.
#[derive(Debug, Clone, Copy, Default)]
pub struct TagApplyOpts {
    /// `--candidates N` (`screen:N`) or `--all`. `None` resumes the cursor's
    /// own mode, or `all` for a tag with no cursor yet.
    pub mode: Option<CandidateMode>,
    pub max_usd: Option<f64>,
    pub apply: bool,
}

/// Everything a run scores with and writes under.
pub struct TagApplyRun<'a> {
    pub tag: &'a str,
    /// The deployed vocabulary: the one the daemons read.
    pub canon: &'a CanonicalSet,
    /// Where `canon` was read from, for the refusal message.
    pub canonical_path: &'a Path,
    pub classifier: &'a ClassifierDev,
    pub spend: &'a DaySpend,
    pub threshold: f32,
    pub cursor_path: &'a Path,
    pub screen: &'a dyn CandidateScreen,
}

/// One note and its target-tag score.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredNote {
    pub path: String,
    pub score: f32,
}

/// What a run did and would do.
#[derive(Debug)]
pub struct TagApplyReport {
    pub tag: String,
    pub candidate_mode: CandidateMode,
    pub candidates: usize,
    /// Candidates no longer among the eligible notes (moved or deleted).
    pub missing: usize,
    /// Whether a changed scoring config threw the cursor's scores away.
    pub scores_discarded: bool,
    /// This run's scoring pass.
    pub scoring: BulkOutcome,
    pub threshold: f32,
    pub max_per_note: usize,
    /// Scored at or above the threshold with room under the cap: written on
    /// `--apply`, would be written on a dry run.
    pub added: Vec<ScoredNote>,
    /// Scored at or above the threshold, but the note already holds
    /// `max-per-note` tags. Never changed: tag-apply does not evict.
    pub at_cap: Vec<ScoredNote>,
    pub already_tagged: usize,
    pub below_threshold: usize,
    /// Scoring failed, or the note could not be read or written.
    pub failed: Vec<(String, String)>,
    /// Candidates with no current score yet (the day ran out first).
    pub unscored: usize,
    pub applied: bool,
    pub cursor_path: PathBuf,
}

/// How adding the tag to one note's current text goes.
#[derive(Debug, PartialEq)]
enum AddPlan {
    AlreadyTagged,
    AtCap,
    Add(String),
}

/// Plan the add against `content`, the note as it is on disk now: its own
/// current tags, plus this one, only while they are under the cap. Nothing
/// is removed, and a tag another writer added since the scan is kept.
fn plan_add(content: &str, tag: &str, max_per_note: usize) -> Result<AddPlan> {
    let (frontmatter, _) = vault::frontmatter::parse_frontmatter(content)?;
    let mut tags = frontmatter.tags.unwrap_or_default();
    if tags.iter().any(|t| t == tag) {
        return Ok(AddPlan::AlreadyTagged);
    }
    if tags.len() >= max_per_note {
        return Ok(AddPlan::AtCap);
    }
    tags.push(tag.to_string());
    let value = serde_yaml::Value::Sequence(tags.into_iter().map(serde_yaml::Value::String).collect());
    insert_frontmatter_fields(content, &[("tags".to_string(), value)])
        .map(AddPlan::Add)
        .ok_or_else(|| eyre::eyre!("the note has no frontmatter block to add a tag to"))
}

/// Refuse a tag the deployed vocabulary lacks: `tag-promote --apply` writes
/// the repo copy, and the daemons (and this verb's scoring call) read the
/// deployed one only after `otto deploy`.
pub fn ensure_deployed(tag: &str, canon: &CanonicalSet, canonical_path: &Path) -> Result<()> {
    if canon.all.contains(tag) {
        return Ok(());
    }
    eyre::bail!(
        "tag {tag:?} is not in the deployed vocabulary ({}). Promote it into config/canonical-tags.yml with \
         `sb cortex tag-promote`, commit, and run `otto deploy`, then rerun tag-apply.",
        canonical_path.display()
    )
}

/// Score the candidates the cursor lacks, as far as the day allows, then
/// report (dry run) or write (`--apply`) every note whose target-tag score
/// clears the threshold.
///
/// `notes` are the notes a tag may be applied to; a candidate outside them
/// is counted as missing and never scored.
pub fn tag_apply(run: &TagApplyRun, vault_root: &Path, notes: &[&Note], opts: &TagApplyOpts) -> Result<TagApplyReport> {
    let tag = run.tag;
    ensure_deployed(tag, run.canon, run.canonical_path)?;

    let stored = TagApplyCursor::load(run.cursor_path)?;
    if let Some(cursor) = &stored
        && cursor.tag != tag
    {
        eyre::bail!(
            "{} holds the tag-apply cursor for {:?}, not {tag:?}",
            run.cursor_path.display(),
            cursor.tag
        );
    }
    let (stored_mode, mut candidates, mut scores) = match stored {
        Some(c) => (Some(c.candidate_mode), c.candidates, c.scores),
        None => (None, Vec::new(), ScoreCursor::default()),
    };
    let mode = match (stored_mode, opts.mode) {
        (None, None) => CandidateMode::All,
        (Some(mode), None) | (None, Some(mode)) => mode,
        (Some(stored), Some(requested)) => stored.widest(requested),
    };
    // A screen is recomputed only when asked for at the width that wins; a
    // narrower request, or a bare resume, keeps the stored list as it is.
    let fresh: Vec<String> = match mode {
        CandidateMode::All => notes.iter().map(|n| n.path.to_string_lossy().to_string()).collect(),
        CandidateMode::Screen(n) if opts.mode == Some(mode) => {
            run.screen.screen(&screen_query(tag, run.canon), tag, n)?
        }
        CandidateMode::Screen(_) => Vec::new(),
    };
    let mut known: HashSet<String> = candidates.iter().cloned().collect();
    candidates.extend(fresh.into_iter().filter(|p| known.insert(p.clone())));
    log::info!(
        "tag_apply: tag={tag} mode={mode} (stored {stored_mode:?}, requested {:?}) candidates={}",
        opts.mode,
        candidates.len()
    );

    let scores_discarded = scores.reconcile(&run.classifier.scoring_config_hash());
    if scores_discarded {
        log::info!("tag_apply: the scoring config changed; every cached score for {tag} was discarded");
    }

    let by_path: HashMap<String, &Note> = notes
        .iter()
        .map(|n| (n.path.to_string_lossy().to_string(), *n))
        .collect();
    let mut owned: Vec<(String, &Note, String)> = Vec::new();
    let mut missing = 0usize;
    for path in &candidates {
        match by_path.get(path) {
            Some(note) => owned.push((path.clone(), note, crate::classify::classifier_text(note))),
            None => {
                log::debug!("tag_apply: candidate {path} is not an eligible note; skipping");
                missing += 1;
            }
        }
    }
    let items: Vec<BulkItem> = owned
        .iter()
        .map(|(path, note, text)| BulkItem {
            id: path,
            input: TagInput {
                title: crate::classify::note_title(note),
                text,
                candidates: &[],
            },
        })
        .collect();

    let snapshot = |scores: &ScoreCursor| TagApplyCursor {
        tag: tag.to_string(),
        candidate_mode: mode,
        candidates: candidates.clone(),
        scores: scores.clone(),
    };
    // Saved before scoring, so a widened candidate list is kept even when
    // the day has nothing left to score it with.
    snapshot(&scores).save(run.cursor_path)?;
    let keep = [tag.to_string()];
    let bulk = BulkRun {
        classifier: run.classifier,
        spend: run.spend,
        max_usd: opts.max_usd,
        path: LEDGER_PATH,
        tag: Some(tag),
        keep: Some(&keep),
    };
    let scoring = bulk.run(&items, &mut scores, &mut |s| snapshot(s).save(run.cursor_path))?;

    let mut report = TagApplyReport {
        tag: tag.to_string(),
        candidate_mode: mode,
        candidates: candidates.len(),
        missing,
        scores_discarded,
        scoring,
        threshold: run.threshold,
        max_per_note: run.canon.max_per_note,
        added: Vec::new(),
        at_cap: Vec::new(),
        already_tagged: 0,
        below_threshold: 0,
        failed: Vec::new(),
        unscored: 0,
        applied: opts.apply,
        cursor_path: run.cursor_path.to_path_buf(),
    };
    for item in &items {
        let id = item.id;
        if !scores.holds(id, &wire_text_hash(&item.input)) {
            match scores.failed.get(id) {
                Some(error) => report.failed.push((id.to_string(), format!("scoring failed: {error}"))),
                None => report.unscored += 1,
            }
            continue;
        }
        let Some(score) = scores.scores.get(id).and_then(|s| s.scores.get(tag)).copied() else {
            report
                .failed
                .push((id.to_string(), format!("the cursor holds no {tag} score for this note")));
            continue;
        };
        if score < run.threshold {
            report.below_threshold += 1;
            continue;
        }
        let scored = ScoredNote {
            path: id.to_string(),
            score,
        };
        match write_one(vault_root, id, tag, run.canon.max_per_note, opts.apply) {
            Ok(AddPlan::Add(_)) => report.added.push(scored),
            Ok(AddPlan::AtCap) => report.at_cap.push(scored),
            Ok(AddPlan::AlreadyTagged) => report.already_tagged += 1,
            Err(e) => report.failed.push((id.to_string(), format!("{e:#}"))),
        }
    }
    log::info!(
        "tag_apply: tag={tag} apply={} added={} at_cap={} already={} below={} failed={} unscored={}",
        opts.apply,
        report.added.len(),
        report.at_cap.len(),
        report.already_tagged,
        report.below_threshold,
        report.failed.len(),
        report.unscored
    );
    Ok(report)
}

/// Read the note now, plan the add against what it holds now, and on
/// `apply` write it. A note at the cap, or already tagged, is never written.
fn write_one(vault_root: &Path, path: &str, tag: &str, max_per_note: usize, apply: bool) -> Result<AddPlan> {
    let abs = vault_root.join(path);
    let content = std::fs::read_to_string(&abs).wrap_err_with(|| format!("failed to read {}", abs.display()))?;
    let plan = plan_add(&content, tag, max_per_note)?;
    if apply && let AddPlan::Add(new_content) = &plan {
        vault::note::write_atomic(&abs, new_content.as_bytes())
            .wrap_err_with(|| format!("failed to write {}", abs.display()))?;
        log::info!("tag_apply: added {tag} to {path}");
    }
    Ok(plan)
}

/// The notes a tag may be applied to: governed by `vault.exclude` /
/// `vault.include`, under `notes/`, and not exempt from `tags` by
/// `frontmatter.path-exempt`. Inbox notes are left to classify's own pass.
pub fn eligible_notes<'a>(notes: &'a [Note], config: &Config) -> Vec<&'a Note> {
    let exclude = crate::parse_patterns(&config.vault.exclude);
    let include = crate::parse_patterns(&config.vault.include);
    notes
        .iter()
        .filter(|n| !crate::is_excluded(n, &exclude, &include))
        .filter(|n| n.path.starts_with("notes"))
        .filter(|n| !crate::frontmatter::path_exempts_field("tags", &n.path, &config.actions.frontmatter))
        .collect()
}

/// The production entry: the deployed vocabulary, classifier.dev over the
/// shared ledger and the system clock, the index screen, and the per-tag
/// cursor under `vault::paths::tag_apply_cursor_dir()`.
///
/// classifier.dev is built whatever `tags.classifier` names: this verb is
/// the production multi-label call by definition, and the configured
/// `threshold`, `instructions`, `repeats` and budget keys still apply.
pub fn run(vault_root: &Path, config: &Config, tag: &str, opts: &TagApplyOpts) -> Result<TagApplyReport> {
    let canonical_path = &config.sweep.canonical_path;
    let canon = CanonicalTagsFile::load(canonical_path)
        .wrap_err("failed to load the deployed canonical tags for tag-apply")?
        .canonical_set();
    ensure_deployed(tag, &canon, canonical_path)?;
    let mapping = canonical::load_tag_mapping(&config.sweep.mapping_path)
        .wrap_err("failed to load the tag mapping for tag-apply")?;
    let cfg = &config.tags.classifier;
    let classifier = ClassifierDev::new(canon.clone(), mapping, cfg);
    let spend = DaySpend::new(SpendLedger::shared(), Arc::new(SystemClock), cfg);
    let screen = IndexScreen {
        db_path: config.oracle_db_path(),
    };
    let cursor_path = TagApplyCursor::path(&vault::paths::tag_apply_cursor_dir(), tag);

    let scanned = crate::vault::scan_vault(vault_root, &config.vault)?;
    let notes = eligible_notes(&scanned, config);
    log::debug!("tag_apply::run: {} eligible note(s) of {}", notes.len(), scanned.len());
    let run = TagApplyRun {
        tag,
        canon: &canon,
        canonical_path,
        classifier: &classifier,
        spend: &spend,
        threshold: cfg.threshold,
        cursor_path: &cursor_path,
        screen: &screen,
    };
    tag_apply(&run, vault_root, &notes, opts)
}

#[cfg(test)]
mod tests;
