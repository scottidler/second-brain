//! `sb borg eval --synth-intents`: give each URL-kind fixture a generated
//! `capture-note.txt`, so `--regenerate` can distill with a capture note and
//! no note is ever hand-written (design 2026-10-14, Resolved Decision "No step
//! requires Scott to read, label, review or write notes").
//!
//! The writer is injected via [`IntentWriter`] so the walk is unit-testable
//! without a live fabric call; production uses [`FabricIntentWriter`], the same
//! `vault::fabric::run_pattern` seam the eval judges use.

use std::io::Write;
use std::path::{Path, PathBuf};

use eyre::{Context, Result, bail};

use crate::eval::fixtures::{self, CAPTURE_NOTE_FILE};

/// Fixture kinds that get a synthesized capture note: the kinds whose distill
/// patterns take one (video, article, thread, session, repo). Image, voicenote
/// and idea captures carry their own text, so a capture note adds nothing.
pub const INTENT_KINDS: &[&str] = &["article", "repo", "session", "thread", "video"];

/// Longest note accepted from the model. A capture note is a sentence or two;
/// a reply past this is the model ignoring the pattern, which is an error, not
/// something to truncate into a note.
pub const MAX_INTENT_CHARS: usize = 400;

/// Char budget for the source text sent to the intent writer.
const DEFAULT_INTENT_MAX_CHARS: usize = 24_000;
/// Per-call fabric timeout for the intent writer.
const DEFAULT_INTENT_TIMEOUT_SECS: u64 = 90;

/// States a plausible reason someone saved `source` (of fixture `kind`).
pub trait IntentWriter {
    fn state_intent(&self, kind: &str, source: &str) -> Result<String>;
}

/// Production writer: runs the `synth-capture-intent` Fabric pattern.
#[derive(Debug, Clone)]
pub struct FabricIntentWriter {
    pub binary: String,
    pub model: String,
    pub pattern: String,
    /// Name of the env var (or file) holding the Anthropic credential; empty
    /// leaves fabric on its own `.env`.
    pub api_key: String,
    pub max_chars: usize,
    pub timeout_secs: u64,
}

impl FabricIntentWriter {
    pub fn new(model: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            binary: "fabric".to_string(),
            model: model.into(),
            pattern: "synth-capture-intent".to_string(),
            api_key: api_key.into(),
            max_chars: DEFAULT_INTENT_MAX_CHARS,
            timeout_secs: DEFAULT_INTENT_TIMEOUT_SECS,
        }
    }
}

impl IntentWriter for FabricIntentWriter {
    fn state_intent(&self, kind: &str, source: &str) -> Result<String> {
        log::debug!(
            "FabricIntentWriter::state_intent: kind={} pattern={} model={} source_len={}",
            kind,
            self.pattern,
            self.model,
            source.len()
        );
        let input = format!("# KIND\n{kind}\n\n# SOURCE\n{source}\n");
        let reply = vault::fabric::run_pattern(
            &self.pattern,
            &input,
            &self.binary,
            &self.api_key,
            &self.model,
            self.max_chars,
            self.timeout_secs,
        )?;
        clean_intent(&reply)
    }
}

/// Normalize a model reply into one capture note: code fences and wrapping
/// quotes stripped, whitespace collapsed. An empty or over-long reply is an
/// error, never written.
pub fn clean_intent(reply: &str) -> Result<String> {
    let unfenced: Vec<&str> = reply.lines().filter(|l| !l.trim_start().starts_with("```")).collect();
    let collapsed = unfenced.join(" ").split_whitespace().collect::<Vec<_>>().join(" ");
    let note = collapsed
        .trim_matches(|c: char| matches!(c, '"' | '\'' | '“' | '”'))
        .trim()
        .to_string();
    if note.is_empty() {
        bail!("intent writer returned an empty note: {:?}", truncate_for_error(reply));
    }
    let chars = note.chars().count();
    if chars > MAX_INTENT_CHARS {
        bail!(
            "intent writer returned {chars} chars, over the {MAX_INTENT_CHARS}-char note ceiling: {:?}",
            truncate_for_error(reply)
        );
    }
    Ok(note)
}

fn truncate_for_error(s: &str) -> String {
    s.chars().take(160).collect()
}

/// What a `--synth-intents` run did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SynthReport {
    /// `capture-note.txt` files written this run.
    pub written: Vec<PathBuf>,
    /// Fixture ids that already had a `capture-note.txt` (never overwritten).
    pub existing: Vec<String>,
    /// Fixtures outside [`INTENT_KINDS`].
    pub skipped_kind: usize,
}

impl SynthReport {
    /// Plain-text summary for `sb` to print.
    pub fn render(&self) -> String {
        let mut o = String::new();
        for path in &self.written {
            o.push_str(&format!("wrote {}\n", path.display()));
        }
        o.push_str(&format!(
            "synth-intents: {} written, {} already present (kept), {} fixtures outside {}\n",
            self.written.len(),
            self.existing.len(),
            self.skipped_kind,
            INTENT_KINDS.join("/"),
        ));
        o
    }
}

/// Write a `capture-note.txt` for every fixture under `fixtures_dir` whose kind
/// is in [`INTENT_KINDS`] and that has none yet. An existing file is never
/// overwritten (opened `create_new`, so a file appearing mid-run is kept too).
/// A writer error stops the run; notes already written stay, and a rerun picks
/// up where it stopped.
pub fn synth_intents(fixtures_dir: &Path, writer: &dyn IntentWriter) -> Result<SynthReport> {
    log::debug!("eval::intents::synth_intents: fixtures_dir={}", fixtures_dir.display());
    let mut report = SynthReport::default();
    for fx in fixtures::load(fixtures_dir)? {
        if !INTENT_KINDS.contains(&fx.kind.as_str()) {
            report.skipped_kind += 1;
            continue;
        }
        let path = fixtures_dir.join(&fx.kind).join(&fx.slug).join(CAPTURE_NOTE_FILE);
        if path.exists() {
            report.existing.push(fx.id.clone());
            continue;
        }
        let note = writer
            .state_intent(&fx.kind, &fx.source)
            .with_context(|| format!("synthesizing the capture note of fixture {}", fx.id))?;
        let mut file = match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                report.existing.push(fx.id.clone());
                continue;
            }
            Err(e) => return Err(e).with_context(|| format!("creating {}", path.display())),
        };
        file.write_all(format!("{note}\n").as_bytes())
            .with_context(|| format!("writing {}", path.display()))?;
        log::debug!("eval::intents::synth_intents: wrote {}", path.display());
        report.written.push(path);
    }
    log::debug!(
        "eval::intents::synth_intents: written={} existing={} skipped_kind={}",
        report.written.len(),
        report.existing.len(),
        report.skipped_kind
    );
    Ok(report)
}

#[cfg(test)]
mod tests;
