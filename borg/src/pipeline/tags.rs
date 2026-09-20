use super::*;
use distillers::tags::{CandidateSource, TagCandidate};

pub(crate) async fn get_or_init_canonical(config: &Config) -> Option<std::sync::Arc<CanonicalState>> {
    use std::sync::Arc;
    use tokio::sync::Mutex as TokioMutex;
    static CACHED: LazyLock<TokioMutex<Option<Arc<CanonicalState>>>> = LazyLock::new(|| TokioMutex::new(None));

    let mut guard = CACHED.lock().await;
    if let Some(ref cached) = *guard {
        return Some(Arc::clone(cached));
    }

    let canonical_path = Path::new(&config.tags.canonical_path);
    let mapping_path = Path::new(&config.tags.mapping_path);

    // Borg's startup precondition (`borg::startup::validate_canonical_assets`)
    // guarantees both files exist and parse before serve_init returns. A
    // failure here is therefore a genuine I/O/parse regression, not a
    // missing-file condition the operator can fix via `sb bootstrap`. Bail
    // with context rather than soft-failing into an unfiltered tag pipeline.
    let canonical_file = match CanonicalTagsFile::load(canonical_path) {
        Ok(f) => f,
        Err(e) => {
            log::error!("canonical-tags load failed after startup validation: {e}");
            return None;
        }
    };

    let mapping = match canonical::load_tag_mapping(mapping_path) {
        Ok(m) => m,
        Err(e) => {
            log::error!("tag-mapping load failed after startup validation: {e}");
            TagMapping::new()
        }
    };

    let state = Arc::new(CanonicalState {
        canon: canonical_file.canonical_set(),
        mapping,
        reject_concatenated: config.tags.reject_concatenated,
    });
    *guard = Some(Arc::clone(&state));
    Some(state)
}

/// What a call site knows about a note before its tags are assigned.
///
/// Provenance is the point: the scoring classifiers ignore `model` candidates
/// (yesterday's LLM output would carry its drift forward) and union `author`
/// candidates at tier 0 (a publisher's own hashtag is a fact, not a guess).
/// After `merge_proposed_tags` the four sources are one `Vec` and the
/// provenance is gone, which is why candidates are built before it.
pub(crate) struct TagSources<'a> {
    pub title: &'a str,
    /// The note summary when one exists, else a body excerpt. Never the
    /// transcript.
    pub text: &'a str,
    /// Publisher hashtags, yt-dlp tags, the note's own `author-tags`.
    pub author: Vec<String>,
    /// Distiller / LLM output from this ingest.
    pub model: Vec<String>,
}

impl<'a> TagSources<'a> {
    pub fn new(title: &'a str, text: &'a str) -> Self {
        Self {
            title,
            text,
            author: Vec::new(),
            model: Vec::new(),
        }
    }
}

pub(crate) struct TagOutcome {
    pub tags: Vec<String>,
    /// The canonical-mapped `author` candidates, written to the `author-tags`
    /// frontmatter key so a later `--retag` can union them back in. Without it
    /// a creator's hashtag is lost the first time the classifier fails to
    /// re-derive it from the text.
    pub author_tags: Vec<String>,
    /// The selected classifier errored and the fallback produced this set.
    /// Borg marks the receipt `degraded=true` so it shows up in
    /// `sb doctor`'s `degraded_24h` rather than passing as a clean ingest.
    pub degraded: bool,
}

/// Assign a note's tags through the shared closed-vocabulary classifier.
///
/// Replaces the old `finalize_tags(&mut Vec<String>)`, which sorted, deduped
/// and canonical-filtered whatever the call site had accumulated. The
/// classifier is synchronous (cortex calls it from a sync fn with no runtime),
/// so it runs on the blocking pool here.
pub(crate) async fn finalize_tags(sources: TagSources<'_>, config: &Config) -> TagOutcome {
    let Some(state) = get_or_init_canonical(config).await else {
        // No vocabulary loaded: fall back to the old behavior rather than
        // dropping every tag on the floor.
        let mut tags: Vec<String> = sources.author.iter().chain(sources.model.iter()).cloned().collect();
        tags.sort();
        tags.dedup();
        return TagOutcome {
            tags,
            author_tags: Vec::new(),
            degraded: false,
        };
    };

    // Reject raw candidates that are two-or-more canonical words mashed
    // together with no separator (e.g. "claudecodeai"), same guard and same
    // pre-canonicalization placement the old single-`Vec` `finalize_tags`
    // applied. `filter_and_cap`'s own tiers (mapping / exact / hyphen-segment)
    // already drop most of these on their own - a mash never exactly matches
    // a mapping key or a whole canonical tag, and it has no hyphen to segment
    // - but the config knob is borg-local and explicit, so it stays honored
    // at the candidate-building seam rather than silently becoming a no-op.
    let reject_concatenated =
        |t: &&String| !state.reject_concatenated || !canonical::is_concatenated_word(t, &state.canon.all);
    let author_candidates: Vec<String> = sources.author.iter().filter(reject_concatenated).cloned().collect();
    let model_candidates: Vec<String> = sources.model.iter().filter(reject_concatenated).cloned().collect();

    let mut candidates: Vec<TagCandidate> = Vec::new();
    for t in &author_candidates {
        candidates.push(TagCandidate::new(t.clone(), CandidateSource::Author));
    }
    for t in &model_candidates {
        candidates.push(TagCandidate::new(t.clone(), CandidateSource::Model));
    }

    let author_tags = canonical::filter_and_cap(&author_candidates, &state.canon, &state.mapping);

    let cfg = config.tags.classifier.clone();
    let canon = state.canon.clone();
    let mapping = state.mapping.clone();
    let title = sources.title.to_string();
    let text = sources.text.to_string();

    // `spawn_blocking`: the trait is sync by design, and `ClassifierDev` makes
    // a blocking HTTP call. Running it on the async worker would stall the
    // runtime for the length of the request.
    let result = tokio::task::spawn_blocking(move || {
        let classifier = distillers::tags::build(&cfg, &canon, &mapping, None);
        let input = distillers::tags::TagInput {
            title: &title,
            text: &text,
            candidates: &candidates,
        };
        match classifier.classify(&input) {
            Ok(out) => (out.tags, false),
            Err(e) => {
                log::warn!("tag classifier failed, running the configured fallback: {e:#}");
                let fallback = distillers::tags::build_fallback(&cfg, &canon, &mapping, None);
                match fallback.classify(&input) {
                    Ok(out) => (out.tags, true),
                    Err(e) => {
                        log::error!("tag classifier fallback also failed, publishing untagged: {e:#}");
                        (Vec::new(), true)
                    }
                }
            }
        }
    })
    .await;

    let (tags, degraded) = match result {
        Ok(pair) => pair,
        Err(e) => {
            log::error!("tag classifier task panicked: {e}");
            (Vec::new(), true)
        }
    };

    TagOutcome {
        tags,
        author_tags,
        degraded,
    }
}

/// Write `author-tags` into a note's frontmatter additions when non-empty, so
/// a later `--retag` can union a publisher's own hashtags back in (API
/// Design: "borg-owned render key, omitted when empty"). Kept out of
/// `markdown::RENDER_NOTE_KEYS` deliberately: that constant is for keys
/// `render_note` emits from its own `NoteContent` fields, and `author-tags`
/// is caller-computed data exactly like `trace-expires` or `repo` already
/// are - the correct seam is the same `frontmatter_additions` map every call
/// site already threads through.
pub(crate) fn insert_author_tags(
    mut additions: std::collections::BTreeMap<String, serde_yaml::Value>,
    author_tags: &[String],
) -> std::collections::BTreeMap<String, serde_yaml::Value> {
    if !author_tags.is_empty() {
        additions.insert(
            "author-tags".to_string(),
            serde_yaml::Value::Sequence(author_tags.iter().cloned().map(serde_yaml::Value::String).collect()),
        );
    }
    additions
}

#[cfg(test)]
mod tests;
