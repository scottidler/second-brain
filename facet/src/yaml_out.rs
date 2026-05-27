//! Shared helpers for parsing YAML/JSON emitted by fabric/LLM calls.
//!
//! Patterns explicitly tell the LLM "no markdown code fences", but
//! Haiku / Sonnet / Opus all wrap output in ```yaml ... ``` (or
//! ```json) fences a non-trivial fraction of the time. This module
//! strips them so the pattern's "ONLY valid YAML/JSON" guarantee is
//! enforced client-side too. Mirrors `distillers::*::strip_fences`.

/// Strip a leading ```<lang> opening fence and the matching trailing
/// ``` close from a fabric response. No-op when no fences are present.
/// Handles `yaml`, `yml`, `json`, and bare ``` openers.
pub fn strip_fences(raw: &str) -> &str {
    let trimmed = raw.trim();
    let without_open = trimmed
        .strip_prefix("```yaml")
        .or_else(|| trimmed.strip_prefix("```yml"))
        .or_else(|| trimmed.strip_prefix("```json"))
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    let stripped = without_open.trim_start_matches('\n');
    if let Some(close) = stripped.rfind("```") {
        stripped[..close].trim_end()
    } else {
        stripped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_when_no_fences() {
        assert_eq!(strip_fences("title: foo\n"), "title: foo");
    }

    #[test]
    fn strips_yaml_fence() {
        assert_eq!(strip_fences("```yaml\ntitle: foo\n```\n"), "title: foo");
    }

    #[test]
    fn strips_bare_fence() {
        assert_eq!(strip_fences("```\ntitle: foo\n```\n"), "title: foo");
    }

    #[test]
    fn strips_yml_fence() {
        assert_eq!(strip_fences("```yml\ntitle: foo\n```"), "title: foo");
    }

    #[test]
    fn strips_with_surrounding_whitespace() {
        assert_eq!(strip_fences("\n\n```yaml\nx: 1\n```\n\n"), "x: 1");
    }

    #[test]
    fn strips_json_fence() {
        assert_eq!(strip_fences("```json\n{\"x\": 1}\n```\n"), "{\"x\": 1}");
    }
}
