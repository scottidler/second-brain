//! Classify inbox notes and promote them to notes/.
//!
//! `tags` are assigned by the shared closed-vocabulary classifier
//! (`distillers::tags`), the same seam borg calls at ingest, and its
//! `Confidence` drives promotion: High or Medium promotes, Low holds the note
//! for the next tick. When the configured implementation errors the configured
//! `fallback` runs over the note's existing tags and `author-tags` - today's
//! Tier 1 - so a classifier outage only holds notes that carry no tags at all.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use eyre::{Result, WrapErr};

use crate::config::{Config, FabricConfig, FrontmatterConfig};
use crate::opts::ClassifyOpts;
use crate::report::{Fix, Report, Severity, Violation};
use crate::scope::insert_frontmatter_fields;
use crate::vault::{Note, scan_vault};
use ::vault::canonical::{self, CanonicalSet, CanonicalTagsFile, TagMapping};
use distillers::tags::{
    CandidateSource, ClassifierKind, Confidence, FabricRunner, ShellFabricRunner, TagCandidate, TagClassifier,
    TagInput, TagOutput, TagsClassifierConfig,
};

/// Top-level orchestrator for `sb cortex classify`. Scans the vault and
/// dispatches to `apply_classify` or `lint_classify` based on `opts.apply`.
///
/// Returns `(Report, written_paths)`. `written_paths` is the concrete list of
/// vault-relative paths this call ACTUALLY wrote (promotions + catch-up
/// enrichment + needs-review marks) - the daemon's oscillation fingerprint
/// draws only from this, never from the report's violation messages. This
/// mirrors the Phase 1 lint seam (`lint_with_notes` -> `LintApplyReport`).
pub fn run(vault_root: &Path, config: &Config, opts: &ClassifyOpts) -> Result<(Report, Vec<String>)> {
    crate::startup::validate_canonical_assets()?;
    log::info!("starting classify command (vault_root={})", vault_root.display());
    let notes = scan_vault(vault_root, &config.vault)?;
    run_with_notes(&notes, vault_root, config, opts)
}

/// Same as `run`, but takes an already-scanned note list instead of scanning
/// the vault itself. Phase 5 (design doc
/// `2026-07-05-cortex-daemon-oscillation-loop.md`) seam: the daemon scans
/// once per cycle and shares the result across every action - `run` stays
/// the scan-then-delegate entry point every other caller (CLI, tests) keeps
/// using unmodified.
pub fn run_with_notes(
    notes: &[Note],
    vault_root: &Path,
    config: &Config,
    opts: &ClassifyOpts,
) -> Result<(Report, Vec<String>)> {
    log::debug!(
        "classify::run_with_notes: vault_root={} note_count={} apply={}",
        vault_root.display(),
        notes.len(),
        opts.apply
    );
    // Built once per run, never per note: `ClassifierDev` reloads nothing, but
    // the vocabulary behind it is two YAML files and a full vault pass would
    // read them thousands of times.
    let classifiers = Classifiers::load(config)?;

    if opts.apply {
        apply_classify(vault_root, notes, config, opts, &classifiers)
    } else {
        // Dry-run writes nothing, so the written-paths list is always empty.
        Ok((lint_classify(notes, vault_root, config, opts, &classifiers), Vec::new()))
    }
}

/// The configured tag classifier plus the `fallback` that runs when it errors,
/// and the vocabulary both were built from.
///
/// Cortex holds the pair rather than one classifier because a classifier.dev
/// outage must not stall promotion of notes borg already tagged at ingest: the
/// fallback (`deterministic` by default) confirms those locally from the note's
/// own `tags` and `author-tags`, and only a `Low` result holds a note back.
///
/// The protect list (`no-classifier-tags`) rides on `canon`, not beside it:
/// it has to reach every capping path, and sweep's did not when it was a
/// separate field here (implementation audit r1, M1).
pub struct Classifiers {
    primary: Box<dyn TagClassifier>,
    fallback: Box<dyn TagClassifier>,
    canon: CanonicalSet,
}

impl Classifiers {
    /// Load the vocabulary from `sweep.canonical-path` / `sweep.mapping-path`
    /// and build the pair named by the top-level `tags.classifier` block.
    /// Fails loudly: without a vocabulary there is no classification, and a
    /// silent empty one would strip every tag it touched.
    pub fn load(config: &Config) -> Result<Self> {
        let canonical_file = CanonicalTagsFile::load(&config.sweep.canonical_path)
            .wrap_err("failed to load canonical tags for the tag classifier")?;
        let mapping = canonical::load_tag_mapping(&config.sweep.mapping_path)
            .wrap_err("failed to load the tag mapping for the tag classifier")?;
        let canon = canonical_file.canonical_set();
        let cfg = &config.tags.classifier;
        log::debug!(
            "Classifiers::load: classifier={:?} fallback={:?} vocabulary={} protected={}",
            cfg.classifier,
            cfg.fallback,
            canon.all.len(),
            canon.no_classifier.len(),
        );
        let fabric = fabric_runner_for(cfg, &config.fabric);
        Ok(Self::from_parts(cfg, canon, &mapping, fabric))
    }

    /// Build the pair from an already-loaded vocabulary and an optional Fabric
    /// runner. `load` resolves both from config; tests hand in a fake runner
    /// to prove the `fabric-closed` kind is selected when configured.
    pub fn from_parts(
        cfg: &TagsClassifierConfig,
        canon: CanonicalSet,
        mapping: &TagMapping,
        fabric: Option<Arc<dyn FabricRunner>>,
    ) -> Self {
        log::debug!(
            "Classifiers::from_parts: classifier={:?} fallback={:?} fabric_runner={}",
            cfg.classifier,
            cfg.fallback,
            fabric.is_some()
        );
        Self {
            primary: distillers::tags::build(cfg, &canon, mapping, fabric.clone()),
            fallback: distillers::tags::build_fallback(cfg, &canon, mapping, fabric),
            canon,
        }
    }

    /// Build a pair directly, for tests and for callers that inject a double.
    pub fn new(primary: Box<dyn TagClassifier>, fallback: Box<dyn TagClassifier>, canon: CanonicalSet) -> Self {
        Self {
            primary,
            fallback,
            canon,
        }
    }

    /// Run a note through the configured classifier, falling back on error.
    /// Never returns an `Err`: a total failure is a `Low` result, which holds
    /// the note exactly the way "no signal" always has.
    fn classify(&self, input: &TagInput) -> TagOutput {
        match self.primary.classify(input) {
            Ok(out) => out,
            Err(e) => {
                log::warn!("tag classifier failed, running the configured fallback: {e:#}");
                match self.fallback.classify(input) {
                    Ok(out) => out,
                    Err(e) => {
                        log::error!("tag classifier fallback also failed, holding the note: {e:#}");
                        TagOutput {
                            tags: Vec::new(),
                            scores: None,
                            confidence: Confidence::Low,
                            method: self.fallback.method(),
                        }
                    }
                }
            }
        }
    }
}

/// The Fabric runner `fabric-closed` needs, built from cortex's `fabric:`
/// block, or `None` when neither the classifier nor its fallback is that
/// kind. `fabric.api_key` is the NAME of the credential var; `Config::load`
/// copies it from `llm.api-key`, and `vault::fabric::run_pattern` resolves it
/// on the child process (the same path `summarize --backfill` takes).
fn fabric_runner_for(cfg: &TagsClassifierConfig, fabric: &FabricConfig) -> Option<Arc<dyn FabricRunner>> {
    let wanted = [cfg.classifier, cfg.fallback].contains(&ClassifierKind::FabricClosed);
    log::debug!(
        "classify::fabric_runner_for: wanted={wanted} binary={} model={} timeout_secs={}",
        fabric.binary,
        fabric.model,
        fabric.timeout_secs
    );
    if !wanted {
        return None;
    }
    Some(Arc::new(ShellFabricRunner {
        binary: fabric.binary.clone(),
        api_key: fabric.api_key.clone(),
        model: fabric.model.clone(),
        max_chars: fabric.max_content_chars,
        timeout_secs: fabric.timeout_secs,
    }))
}

/// What classifying one note produced.
///
/// `tags` is the answer that matters: its `confidence` decides promotion and
/// its `method` is written to `cortex-classified-by`.
#[derive(Debug)]
pub struct ClassifyResult {
    pub tags: TagOutput,
    pub reason: String,
}

impl ClassifyResult {
    pub fn confidence(&self) -> Confidence {
        self.tags.confidence
    }

    pub fn method(&self) -> &'static str {
        self.tags.method.as_str()
    }
}

/// The note text a classifier scores: its `## Summary` when it has one, else
/// the head of the body, clipped through the shared
/// `distillers::tags::body_excerpt` so borg and cortex send the same amount
/// of a summary-less note to classifier.dev.
fn classifier_text(note: &Note) -> String {
    ::vault::search::parse_body_summary(&note.body).unwrap_or_else(|| distillers::tags::body_excerpt(&note.body))
}

/// Publisher hashtags borg recorded at ingest. Read back as `Author`
/// candidates so a `--retag` can union a creator's own tags in again instead
/// of losing them the first time the classifier fails to re-derive them.
fn note_author_tags(note: &Note) -> Vec<String> {
    match note.frontmatter.extra.get("author-tags") {
        Some(serde_yaml::Value::Sequence(items)) => {
            items.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()
        }
        Some(serde_yaml::Value::String(one)) => vec![one.clone()],
        _ => Vec::new(),
    }
}

fn note_tags(note: &Note) -> &[String] {
    note.frontmatter.tags.as_deref().unwrap_or(&[])
}

/// The candidates cortex hands the classifier: the note's existing canonical
/// tags as `Preserved` (what today's Tier 1 reads) and its `author-tags` as
/// `Author`. The scoring implementations ignore `Preserved` by design; only
/// `Deterministic` consumes it.
fn note_candidates(note: &Note) -> Vec<TagCandidate> {
    let mut candidates: Vec<TagCandidate> = note_tags(note)
        .iter()
        .map(|t| TagCandidate::new(t.clone(), CandidateSource::Preserved))
        .collect();
    candidates.extend(
        note_author_tags(note)
            .into_iter()
            .map(|t| TagCandidate::new(t, CandidateSource::Author)),
    );
    candidates
}

/// Merge preserved and fresh tags the way a reingest does: preserved first,
/// deduped, capped through `canonical::cap_protecting` so a
/// `no-classifier-tags` entry survives even when the note's own tags already
/// fill the cap. The borg twin is `borg::pipeline::atomic::union_capped`; the
/// policy is stated once in the design doc and enforced at both writers.
fn union_capped(preserved: &[String], fresh: &[String], canon: &CanonicalSet) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tag in preserved.iter().chain(fresh.iter()) {
        if !out.contains(tag) {
            out.push(tag.clone());
        }
    }
    if out.len() > canon.max_per_note {
        log::info!(
            "classify::union_capped: capping {} merged tags to {}; the fresh set loses the overflow first, protected tags never",
            out.len(),
            canon.max_per_note
        );
    }
    canonical::cap_protecting(out, canon.max_per_note, &canon.no_classifier)
}

/// How a `--retag` result splits: tags kept from the note's previous list
/// because they are on the protect list, and tags the classifier selected.
/// Reported separately so a retag run shows how much of the result the
/// model actually produced (design review r4, guardrail 3).
fn retag_split(previous: &[String], fresh: &[String], canon: &CanonicalSet) -> (usize, usize) {
    let retained_protected = fresh
        .iter()
        .filter(|t| previous.contains(t) && canon.no_classifier.contains(*t))
        .count();
    let model_selected = fresh.len() - retained_protected;
    log::debug!("classify::retag_split: retained_protected={retained_protected} model_selected={model_selected}");
    (retained_protected, model_selected)
}

fn tags_value(tags: &[String]) -> serde_yaml::Value {
    serde_yaml::Value::Sequence(tags.iter().cloned().map(serde_yaml::Value::String).collect())
}

/// Select the notes a run targets: the `--retag` set when one is given, else
/// the inbox plus the catch-up population.
fn select_targets<'a>(notes: &'a [Note], vault_root: &Path, config: &Config, opts: &ClassifyOpts) -> Vec<&'a Note> {
    if !opts.retag.is_empty() {
        return filter_retag_notes(notes, &opts.retag, vault_root);
    }
    let inbox = filter_inbox_notes(notes, opts.force, opts.review_only);
    let unclassified = filter_unclassified_notes(notes, &config.actions.frontmatter);
    inbox.into_iter().chain(unclassified).collect()
}

/// Dry-run: returns planned classifications as violations in a Report
pub fn lint_classify(
    notes: &[Note],
    vault_root: &Path,
    config: &Config,
    opts: &ClassifyOpts,
    classifiers: &Classifiers,
) -> Report {
    let mut report = Report::default();
    let targets = select_targets(notes, vault_root, config, opts);

    for note in &targets {
        let already_in_notes = note.path.to_string_lossy().starts_with("notes/");
        let result = classify_note(note, classifiers);

        if !opts.retag.is_empty() {
            if result.confidence() == Confidence::Low {
                report.add(Violation {
                    path: note.path.clone(),
                    rule: "classify".to_string(),
                    severity: Severity::Warning,
                    message: "would leave the tags unchanged: the classifier returned none".to_string(),
                    fix: None,
                });
                continue;
            }
            let fresh = distillers::tags::apply_retag(note_tags(note), &result.tags, &classifiers.canon);
            let (retained_protected, model_selected) = retag_split(note_tags(note), &fresh, &classifiers.canon);
            report.add(Violation {
                path: note.path.clone(),
                rule: "classify".to_string(),
                severity: Severity::Info,
                message: format!(
                    "would retag as [{}] (was [{}], retained-protected={retained_protected}, model-selected={model_selected}, method={})",
                    fresh.join(", "),
                    note_tags(note).join(", "),
                    result.method(),
                ),
                fix: None,
            });
            continue;
        }

        if result.confidence() == Confidence::Low {
            if !already_in_notes {
                report.add(Violation {
                    path: note.path.clone(),
                    rule: "classify".to_string(),
                    severity: Severity::Warning,
                    message: format!(
                        "no tag cleared the classifier, would hold for review: {}",
                        result.reason
                    ),
                    fix: None,
                });
            }
            continue;
        }

        if already_in_notes {
            report.add(Violation {
                path: note.path.clone(),
                rule: "classify".to_string(),
                severity: Severity::Info,
                message: format!(
                    "would catch-up classify tags=[{}], method={}",
                    result.tags.tags.join(", "),
                    result.method(),
                ),
                fix: None,
            });
        } else {
            report.add(Violation {
                path: note.path.clone(),
                rule: "classify".to_string(),
                severity: Severity::Info,
                message: format!(
                    "would classify as tags=[{}], confidence={}, method={}: {}",
                    result.tags.tags.join(", "),
                    result.confidence().as_str(),
                    result.method(),
                    result.reason,
                ),
                fix: Some(Fix::MoveFile {
                    from: note.path.clone(),
                    to: PathBuf::from("notes").join(note.path.file_name().unwrap_or_default()),
                }),
            });
        }
    }

    log::info!("classify lint complete: {} target note(s)", targets.len());
    report
}

/// Apply: classify and move notes from inbox/ to notes/.
///
/// Two write shapes, selected by `opts`: `--retag` REPLACES a named note's
/// tags (protect list honored, never writes empty), and the default run
/// promotes inbox notes and catch-up-enriches tag-less ones in `notes/`.
///
/// Returns `(Report, written_paths)`. `written_paths` is the union of the real,
/// vault-relative paths this call actually WROTE - promotions, catch-up
/// enrichment, retags, and needs-review marks - never the paths of notes
/// merely inspected. It is the classify equivalent of
/// `LintApplyReport.written_paths` and is the ONLY thing the daemon's
/// oscillation fingerprint may draw from for the classify action.
pub fn apply_classify(
    vault_root: &Path,
    notes: &[Note],
    config: &Config,
    opts: &ClassifyOpts,
    classifiers: &Classifiers,
) -> Result<(Report, Vec<String>)> {
    let target_notes = select_targets(notes, vault_root, config, opts);
    let is_retag = !opts.retag.is_empty();
    let mut report = Report::default();
    let mut moves: Vec<(PathBuf, PathBuf)> = Vec::new();
    // Real, byte-changed paths this call wrote - the daemon fingerprints this,
    // never `report.violations` (which named catch-up/needs-review notes that
    // may not have changed on disk, or promotions the daemon used to sniff by
    // matching the substring "promoted" in a violation message).
    let mut written: Vec<String> = Vec::new();

    for note in &target_notes {
        let already_in_notes = note.path.to_string_lossy().starts_with("notes/");
        let result = classify_note(note, classifiers);

        // `--retag`: replace, not union. The fresh classification wins outright;
        // the previous tags survive only through the protect list or when the
        // fresh result is empty or Low (`distillers::tags::apply_retag`).
        if is_retag {
            // A Low result has nothing to replace the note's tags WITH, so the
            // note is left exactly as it is - including its existing
            // provenance keys. Stamping `cortex-confidence: low` here would
            // rewrite the note to record that a classification did not happen.
            if result.confidence() == Confidence::Low {
                log::info!(
                    "retag left {} unchanged: the classifier returned no tags",
                    note.path.display()
                );
                continue;
            }
            let fresh = distillers::tags::apply_retag(note_tags(note), &result.tags, &classifiers.canon);
            if fresh.is_empty() {
                log::warn!(
                    "retag produced no tags and the note has none to keep: {}",
                    note.path.display()
                );
                continue;
            }
            let (retained_protected, model_selected) = retag_split(note_tags(note), &fresh, &classifiers.canon);
            let mut fields = vec![("tags".to_string(), tags_value(&fresh))];
            push_classification_fields(&mut fields, &result);
            if let Some(path) = write_fields(vault_root, note, &fields, "retag")? {
                written.push(path);
                report.add(Violation {
                    path: note.path.clone(),
                    rule: "classify".to_string(),
                    severity: Severity::Info,
                    message: format!(
                        "retagged as [{}] (retained-protected={retained_protected}, model-selected={model_selected}, method={})",
                        fresh.join(", "),
                        result.method()
                    ),
                    fix: None,
                });
                log::info!(
                    "retagged {} (tags=[{}], retained-protected={retained_protected}, model-selected={model_selected}, method={})",
                    note.path.display(),
                    fresh.join(", "),
                    result.method(),
                );
            }
            continue;
        }

        if result.confidence() == Confidence::Low {
            if !already_in_notes && mark_needs_review(vault_root, note)? {
                written.push(note.path.to_string_lossy().to_string());
            }
            log::info!(
                "held for review (no tag cleared the classifier): {}",
                note.path.display()
            );
            continue;
        }

        // Catch-up: enrich tag-less notes already in notes/ in place
        if already_in_notes {
            let fields = build_enrichment_fields(&result, note, &classifiers.canon);
            if let Some(path) = write_fields(vault_root, note, &fields, "catch-up classify")? {
                written.push(path);
                report.add(Violation {
                    path: note.path.clone(),
                    rule: "classify".to_string(),
                    severity: Severity::Info,
                    message: format!(
                        "catch-up classified tags=[{}] (method={})",
                        result.tags.tags.join(", "),
                        result.method(),
                    ),
                    fix: None,
                });

                log::info!(
                    "catch-up classified {} (tags=[{}], method={})",
                    note.path.display(),
                    result.tags.tags.join(", "),
                    result.method(),
                );
            }
            continue;
        }

        // Enrich frontmatter and promote (inbox -> notes/)
        let mut enrichment_fields = build_enrichment_fields(&result, note, &classifiers.canon);
        ensure_origin(&mut enrichment_fields, note);
        let enrichment_fields = enrichment_fields;
        let abs_path = vault_root.join(&note.path);
        let content = match std::fs::read_to_string(&abs_path) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("skipping promote for {}: {e}", note.path.display());
                continue;
            }
        };

        if let Some(new_content) = insert_frontmatter_fields(&content, &enrichment_fields)
            && let Err(e) = vault::note::write_atomic(&abs_path, new_content.as_bytes())
        {
            log::warn!("skipping promote for {}: {e}", note.path.display());
            continue;
        }

        // Move from inbox/ to notes/
        let filename = note.path.file_name().unwrap_or_default();
        let dest_relative = PathBuf::from("notes").join(filename);
        let dest_abs = vault_root.join(&dest_relative);

        // Handle filename collision - pass source URL so we can detect
        // reingest replacements (same source = overwrite, not -2 suffix)
        let source_url = note.frontmatter.source.as_deref();
        let dest_abs = resolve_collision(&dest_abs, source_url);
        let dest_relative = dest_abs.strip_prefix(vault_root).unwrap_or(&dest_abs).to_path_buf();

        // copy+delete instead of rename: rename() emits MOVED_TO which Dataview
        // doesn't re-index; copy produces a CREATE event that triggers indexing.
        // Per-note errors WARN and skip rather than `?`-aborting the run.
        let move_result = dest_abs
            .parent()
            .map(std::fs::create_dir_all)
            .unwrap_or(Ok(()))
            .and_then(|()| std::fs::copy(&abs_path, &dest_abs).map(|_| ()))
            .and_then(|()| std::fs::remove_file(&abs_path));
        if let Err(e) = move_result {
            log::warn!("skipping promote move for {}: {e}", note.path.display());
            continue;
        }

        // Only redirect wikilinks for a plain inbox->notes move (same stem). A
        // suffix-collision move (foo.md -> foo-2.md) is a DIFFERENT note from
        // the [[foo]] that other notes link to, so rewriting [[foo]] ->
        // [[foo-2]] would point them at the wrong note.
        if note.path.file_stem() == dest_relative.file_stem() {
            moves.push((note.path.clone(), dest_relative.clone()));
        } else {
            log::info!(
                "suffix-collision move {} -> {}: skipping wikilink rewrite",
                note.path.display(),
                dest_relative.display()
            );
        }

        // A promotion always mutates the vault (the inbox file is removed and a
        // notes/ file created), independent of whether the enrichment write
        // above changed bytes - so it is always a real write to fingerprint.
        written.push(note.path.to_string_lossy().to_string());
        report.add(Violation {
            path: note.path.clone(),
            rule: "classify".to_string(),
            severity: Severity::Info,
            message: format!(
                "promoted to {} (tags=[{}], method={})",
                dest_relative.display(),
                result.tags.tags.join(", "),
                result.method(),
            ),
            fix: None,
        });

        log::info!(
            "promoted {} -> {} (tags=[{}], confidence={}, method={})",
            note.path.display(),
            dest_relative.display(),
            result.tags.tags.join(", "),
            result.confidence().as_str(),
            result.method(),
        );
    }

    // Update wikilinks across vault for moved files
    if !moves.is_empty() {
        let all_notes = crate::vault::scan_vault(vault_root, &crate::config::VaultConfig::default())?;
        crate::naming::update_wikilinks_batch(vault_root, &all_notes, &moves)?;
    }

    written.sort();
    written.dedup();
    // Surface the real write count/paths on the report too, matching the Phase 1
    // lint seam (`report.applied` / `report.applied_paths`); sb prints this.
    report.applied = written.len();
    report.applied_paths = written.clone();

    log::debug!("classify::apply_classify: written={}", written.len());

    // Caller (sb) formats the report; classify::run returns it directly.
    Ok((report, written))
}

/// Write frontmatter fields to a note in place, returning the vault-relative
/// path when bytes actually changed.
///
/// Byte guard: `insert_frontmatter_fields` does remove-then-reappend with no
/// value comparison, so it can re-emit identical content; an unconditional
/// write there fires the daemon watcher for nothing. Per-note errors WARN and
/// skip rather than aborting the run - a note deleted between scan and apply is
/// routine on Syncthing.
fn write_fields(
    vault_root: &Path,
    note: &Note,
    fields: &[(String, serde_yaml::Value)],
    what: &str,
) -> Result<Option<String>> {
    let abs_path = vault_root.join(&note.path);
    let content = match std::fs::read_to_string(&abs_path) {
        Ok(c) => c,
        Err(e) => {
            log::warn!("skipping {what} for {}: {e}", note.path.display());
            return Ok(None);
        }
    };
    match insert_frontmatter_fields(&content, fields) {
        Some(new_content) if new_content != content => {
            if let Err(e) = vault::note::write_atomic(&abs_path, new_content.as_bytes()) {
                log::warn!("skipping {what} for {}: {e}", note.path.display());
                return Ok(None);
            }
            Ok(Some(note.path.to_string_lossy().to_string()))
        }
        _ => Ok(None),
    }
}

/// Classify a single note: tags from the shared classifier, nothing else.
fn classify_note(note: &Note, classifiers: &Classifiers) -> ClassifyResult {
    let candidates = note_candidates(note);
    let text = classifier_text(note);
    let title = note.frontmatter.title.as_deref().unwrap_or("Untitled");
    let tags = classifiers.classify(&TagInput {
        title,
        text: &text,
        candidates: &candidates,
    });

    if tags.confidence == Confidence::Low {
        return ClassifyResult {
            tags,
            reason: "no tag cleared the classifier".to_string(),
        };
    }

    ClassifyResult {
        tags,
        reason: "tags classifier".to_string(),
    }
}

/// Filter notes to the `--retag` set. Each argument is a vault-relative path
/// or a glob over one; an ABSOLUTE path is stripped of the vault root first,
/// so a shell expansion run from the vault root and one run from elsewhere
/// select the same notes. A pattern matching nothing is logged, never
/// silently ignored.
///
/// Stripping the root is what makes the match exact. A tail match is not
/// enough: `.../notes/home.md` also ends with `/home.md`, so it selected the
/// vault-root note `home.md` too, and a retag over the 36 ex-`resources`
/// notes named 37.
fn filter_retag_notes<'a>(notes: &'a [Note], patterns: &[String], vault_root: &Path) -> Vec<&'a Note> {
    let mut selected: Vec<&Note> = Vec::new();
    for raw in patterns {
        let relative = Path::new(raw)
            .strip_prefix(vault_root)
            .map_or_else(|_| raw.clone(), |p| p.to_string_lossy().to_string());
        let pattern = match glob::Pattern::new(&relative) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("--retag pattern {raw} is not a valid glob: {e}");
                continue;
            }
        };
        let before = selected.len();
        for note in notes {
            let path = note.path.to_string_lossy();
            if (pattern.matches(&path) || relative == path) && !selected.iter().any(|n| n.path == note.path) {
                selected.push(note);
            }
        }
        if selected.len() == before {
            log::warn!("--retag pattern matched no note in the vault: {raw}");
        }
    }
    log::debug!(
        "classify::filter_retag_notes: patterns={} selected={}",
        patterns.len(),
        selected.len()
    );
    selected
}

/// Filter notes to inbox-only, respecting force and review-only flags
fn filter_inbox_notes(notes: &[Note], force: bool, review_only: bool) -> Vec<&Note> {
    notes
        .iter()
        .filter(|n| {
            let path_str = n.path.to_string_lossy();
            path_str.starts_with("inbox/") || path_str.starts_with("inbox\\")
        })
        .filter(|n| {
            // Skip already-classified unless force
            if !force {
                let classified = n.frontmatter.extra.get("cortex-classified");
                if classified == Some(&serde_yaml::Value::Bool(true)) {
                    return false;
                }
            }
            true
        })
        .filter(|n| {
            // If review_only, only process notes with cortex-needs-review
            if review_only {
                let needs_review = n.frontmatter.extra.get("cortex-needs-review");
                return needs_review == Some(&serde_yaml::Value::Bool(true));
            }
            true
        })
        .collect()
}

/// Filter notes in notes/ that carry no tags (orphaned by reingest or other
/// means), EXCLUDING paths that `frontmatter.path-exempt` excuses from
/// carrying tags at all.
///
/// Without the exemption check this selected every `notes/ai/**` digest on every
/// cycle - they are exempt from `tags` by config, so they can never become
/// "classified" - burning one LLM call per digest per cycle and logging
/// `held for review (low confidence)` in perpetuity.
fn filter_unclassified_notes<'a>(notes: &'a [Note], frontmatter: &FrontmatterConfig) -> Vec<&'a Note> {
    notes
        .iter()
        .filter(|n| {
            let path_str = n.path.to_string_lossy();
            path_str.starts_with("notes/") || path_str.starts_with("notes\\")
        })
        .filter(|n| n.frontmatter.tags.as_ref().is_none_or(|t| t.is_empty()))
        .filter(|n| !crate::frontmatter::path_exempts_field("tags", &n.path, frontmatter))
        .collect()
}

/// Build the frontmatter fields an enrichment (promotion or catch-up) writes.
///
/// `tags` is the P3 union - the note's existing tags first, then the fresh
/// ones, capped - so a classification never removes a tag a reingest would
/// have preserved. `--retag` is the one operation with replace semantics and
/// it does not come through here.
fn build_enrichment_fields(
    result: &ClassifyResult,
    note: &Note,
    canon: &CanonicalSet,
) -> Vec<(String, serde_yaml::Value)> {
    let mut fields: Vec<(String, serde_yaml::Value)> = Vec::new();

    let merged = union_capped(note_tags(note), &result.tags.tags, canon);
    if !merged.is_empty() {
        fields.push(("tags".to_string(), tags_value(&merged)));
    }

    fields.push((
        "status".to_string(),
        serde_yaml::Value::String(vault::schema::Status::Unread.as_str().to_string()),
    ));
    push_classification_fields(&mut fields, result);
    fields
}

/// The three provenance keys every classification writes, whatever the write
/// shape.
fn push_classification_fields(fields: &mut Vec<(String, serde_yaml::Value)>, result: &ClassifyResult) {
    fields.push(("cortex-classified".to_string(), serde_yaml::Value::Bool(true)));
    fields.push((
        "cortex-classified-by".to_string(),
        serde_yaml::Value::String(result.method().to_string()),
    ));
    fields.push((
        "cortex-confidence".to_string(),
        serde_yaml::Value::String(result.confidence().as_str().to_string()),
    ));
}

/// Set origin: assisted if missing
fn ensure_origin(fields: &mut Vec<(String, serde_yaml::Value)>, note: &Note) {
    if note.frontmatter.origin.is_none() {
        fields.push((
            "origin".to_string(),
            serde_yaml::Value::String(vault::schema::Origin::Assisted.as_str().to_string()),
        ));
    }
}

/// Mark a note as needing manual review. Returns `true` iff it wrote to disk.
///
/// Idempotent by construction - this is the Phase 8 audit fix. A no-signal or
/// low-confidence INBOX note is never marked `cortex-classified`, so
/// `filter_inbox_notes` re-selects it on EVERY classify cycle; the previous
/// unconditional `insert_frontmatter_fields` + `write_atomic` therefore
/// rewrote it every cycle with an empty oscillation fingerprint - the exact
/// perpetual self-write this design doc exists to eliminate. Two guards:
///
/// 1. Semantic guard: an inbox note already holding `cortex-needs-review: true`
///    is not reprocessed at all.
/// 2. Byte guard (defense in depth): even on the first mark,
///    `insert_frontmatter_fields` does remove-then-reappend with no value
///    comparison, so only touch disk when the bytes actually change.
fn mark_needs_review(vault_root: &Path, note: &Note) -> Result<bool> {
    log::debug!("mark_needs_review: note={}", note.path.display());
    if note.frontmatter.extra.get("cortex-needs-review") == Some(&serde_yaml::Value::Bool(true)) {
        log::debug!(
            "mark_needs_review: {} already held for review, skipping",
            note.path.display()
        );
        return Ok(false);
    }

    let abs_path = vault_root.join(&note.path);
    let content = std::fs::read_to_string(&abs_path)?;

    let fields = vec![("cortex-needs-review".to_string(), serde_yaml::Value::Bool(true))];

    if let Some(new_content) = insert_frontmatter_fields(&content, &fields)
        && new_content != content
    {
        vault::note::write_atomic(&abs_path, new_content.as_bytes())?;
        log::debug!("mark_needs_review: marked {}", note.path.display());
        return Ok(true);
    }

    Ok(false)
}

/// Resolve filename collision. If the existing note has the same source URL,
/// this is a reingest replacement - return the original path (overwrite in place).
/// Only append a numeric suffix for genuinely different notes with the same slug.
fn resolve_collision(path: &Path, source_url: Option<&str>) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }

    // Same source URL means reingest replacement - overwrite, don't create -2
    if let Some(source) = source_url
        && existing_note_has_source(path, source)
    {
        log::info!(
            "collision is a reingest replacement (same source), overwriting: {}",
            path.display()
        );
        return path.to_path_buf();
    }

    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("note");
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("md");
    let parent = path.parent().unwrap_or(Path::new("."));

    for i in 2..100 {
        let candidate = parent.join(format!("{stem}-{i}.{ext}"));
        if !candidate.exists() {
            return candidate;
        }
        // Same re-check as the base path: a numeric candidate carrying the
        // SAME source is a reingest replacement too, not a distinct sibling.
        // Without this the loop walks past every same-source `-N` candidate
        // and mints `-N+1` forever (the real hv-e5d240 failure mode - see
        // docs/design/2026-08-15-harvest-note-identity-trace-keyed-replace.md).
        if let Some(source) = source_url
            && existing_note_has_source(&candidate, source)
        {
            log::info!(
                "collision is a reingest replacement (same source), overwriting: {}",
                candidate.display()
            );
            return candidate;
        }
    }

    // Extremely unlikely - fall back to original
    path.to_path_buf()
}

/// Check if an existing note file contains a matching source URL in its frontmatter.
fn existing_note_has_source(path: &Path, source_url: &str) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    use std::io::Read;
    let mut buf = vec![0u8; 2048];
    let mut reader = std::io::BufReader::new(file);
    let n = reader.read(&mut buf).unwrap_or(0);
    let header = String::from_utf8_lossy(&buf[..n]);
    // Match BOTH the quoted and unquoted frontmatter forms - notes written
    // without quotes (`source: https://...`) were previously never recognized
    // as reingest replacements, so they got a spurious `-2` suffix.
    header.contains(&format!("source: \"{source_url}\"")) || header.contains(&format!("source: {source_url}"))
}

#[cfg(test)]
mod tests;
