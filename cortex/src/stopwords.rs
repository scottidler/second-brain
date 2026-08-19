//! The one wikilink stopword vocabulary.
//!
//! Three layers can act on a `[[target]]` and all three must judge it the
//! same way, or they drift:
//!
//! - the auto-linker (`crate::linking`) WRITES the markup into note bodies,
//! - the graph builder (`crate::graph`) turns landed markup into an edge,
//! - the retraction sweep (`crate::unlink`) strips markup already landed.
//!
//! The list itself is config (`graph.wikilink-stopwords`, defaults EMPTY —
//! code never silently suppresses a link). This module owns the only
//! *predicate* over it, and the type exists so the list is threaded
//! explicitly: a layer that forgets to consult the vocabulary is a compile
//! error, not a silent no-op that lets a false link back in.
//!
//! Every layer judges the RAW `[[target]]`, before any path resolution —
//! never the surface text, never the resolved path. `[[every|Every]]` is
//! judged on `every`.

/// A wikilink stopword vocabulary: targets that must never be written,
/// never mint an edge, and are retractable by `sb cortex unlink`.
#[derive(Debug, Clone, Default)]
pub struct Stopwords {
    /// Kept as authored (not lowercased) so `iter` can report the operator's
    /// own spelling back; matching is case-insensitive at compare time.
    words: Vec<String>,
}

impl Stopwords {
    /// Build from the configured list. Blank entries are dropped so a stray
    /// `- ""` in YAML cannot match every target.
    pub fn new(words: &[String]) -> Self {
        let words: Vec<String> = words
            .iter()
            .map(|w| w.trim().to_string())
            .filter(|w| !w.is_empty())
            .collect();
        log::debug!("Stopwords::new: {} entr(ies)", words.len());
        Self { words }
    }

    /// Is this raw wikilink target stoplisted? Case-insensitive, so a
    /// lowercase config entry also catches the auto-linker's `[[Every]]`.
    pub fn contains(&self, target: &str) -> bool {
        let target = target.trim();
        self.words.iter().any(|stop| stop.eq_ignore_ascii_case(target))
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    pub fn len(&self) -> usize {
        self.words.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.words.iter().map(|w| w.as_str())
    }
}

#[cfg(test)]
mod tests;
