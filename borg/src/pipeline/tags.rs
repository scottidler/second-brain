use super::*;

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
        canonical_set: canonical_file.all_tags(),
        max_per_note: canonical_file.max_per_note,
        mapping,
        reject_concatenated: config.tags.reject_concatenated,
    });
    *guard = Some(Arc::clone(&state));
    Some(state)
}

/// Sort, dedup, and filter tags through the canonical vocabulary.
/// Falls back to simple sort+dedup if canonical config is not available.
pub(crate) async fn finalize_tags(tags: &mut Vec<String>, config: &Config) {
    tags.sort();
    tags.dedup();

    if let Some(state) = get_or_init_canonical(config).await {
        // Reject concatenated words
        if state.reject_concatenated {
            tags.retain(|t| !canonical::is_concatenated_word(t, &state.canonical_set));
        }

        // Filter and cap through canonical vocabulary
        *tags = canonical::filter_and_cap(tags, &state.canonical_set, &state.mapping, state.max_per_note);
    }
}

#[cfg(test)]
mod tests;
