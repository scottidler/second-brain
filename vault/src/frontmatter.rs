use eyre::{Context, Result};
use std::collections::HashMap;

/// Coerce a YAML scalar to its natural string form for a string-typed
/// frontmatter field. A number/bool renders as its plain text; null and
/// non-scalar values (sequence/mapping/tagged) yield None. Never stores a
/// `{:?}` debug rendering of the `Value` enum (the `"Number(2023)"` bug).
fn scalar_to_string(val: serde_yaml::Value) -> Option<String> {
    match val {
        serde_yaml::Value::String(s) => Some(s),
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        serde_yaml::Value::Bool(b) => Some(b.to_string()),
        serde_yaml::Value::Null => None,
        serde_yaml::Value::Sequence(_) | serde_yaml::Value::Mapping(_) | serde_yaml::Value::Tagged(_) => None,
    }
}

/// Parsed frontmatter. Known fields extracted; everything else in extra.
#[derive(Debug, Clone, Default)]
pub struct Frontmatter {
    pub title: Option<String>,
    pub date: Option<String>,
    pub note_type: Option<String>,
    pub domain: Option<String>,
    pub origin: Option<String>,
    pub status: Option<String>,
    pub tags: Option<Vec<String>>,
    pub source: Option<String>,
    pub creator: Option<String>,
    /// Set to `Some(true)` to mark a note as pinned: it is excluded from
    /// the cold-note report and treated as L3 (promoted) for any future
    /// signal-aware tooling. Strict bool-only: `pinned: "true"`,
    /// `pinned: 1`, and `pinned:` (null) all parse as `None` and index
    /// as 0; a typo in the user's frontmatter is silently treated as
    /// "not pinned" rather than breaking reindex.
    pub pinned: Option<bool>,
    pub extra: HashMap<String, serde_yaml::Value>,
}

impl Frontmatter {
    /// Parse from a serde_yaml::Value (typically a Mapping).
    /// Known fields are extracted; everything else goes into extra.
    pub fn from_value(value: serde_yaml::Value) -> Result<Self> {
        let mapping = match value {
            serde_yaml::Value::Mapping(m) => m,
            _ => return Ok(Self::default()),
        };

        let mut title = None;
        let mut date = None;
        let mut note_type = None;
        let mut domain = None;
        let mut origin = None;
        let mut status = None;
        let mut tags = None;
        let mut source = None;
        let mut creator = None;
        let mut pinned: Option<bool> = None;
        let mut extra = HashMap::new();

        for (key, val) in mapping {
            let key_str = match &key {
                serde_yaml::Value::String(s) => s.clone(),
                // Non-string scalar keys (e.g. `2023:`) route through
                // scalar_to_string so they land as `"2023"`, not the Rust Debug
                // repr `"Number(2023)"`. They then flow through the field match
                // below (none will match a known field, so they reach `extra`).
                other => match scalar_to_string(other.clone()) {
                    Some(s) => s,
                    None => {
                        // Non-scalar key (sequence/map/null) - extremely unusual;
                        // keep a stable repr so it round-trips into `extra`.
                        let s = format!("{other:?}");
                        extra.insert(s, val);
                        continue;
                    }
                },
            };

            match key_str.as_str() {
                "title" => {
                    title = scalar_to_string(val);
                }
                "date" => {
                    date = scalar_to_string(val);
                }
                "type" => {
                    note_type = scalar_to_string(val);
                }
                "domain" => {
                    domain = scalar_to_string(val);
                }
                "origin" => {
                    origin = scalar_to_string(val);
                }
                "status" => {
                    status = scalar_to_string(val);
                }
                "tags" => {
                    if let serde_yaml::Value::Sequence(seq) = val {
                        tags = Some(
                            seq.into_iter()
                                .filter_map(|v| match v {
                                    serde_yaml::Value::String(s) => Some(s),
                                    _ => None,
                                })
                                .collect(),
                        );
                    }
                }
                "source" => {
                    source = scalar_to_string(val);
                }
                "creator" => {
                    creator = scalar_to_string(val);
                }
                "pinned" => {
                    // Strict bool-only: a typo (`pinned: "yes"`, `pinned: 1`)
                    // resolves to None / indexed as 0 rather than breaking
                    // reindex. The lenient `format!("{other:?}")` branch the
                    // string fields use would yield `Some("Bool(true)")`
                    // which is the wrong shape for a typed bool.
                    pinned = match val {
                        serde_yaml::Value::Bool(b) => Some(b),
                        _ => None,
                    };
                }
                _ => {
                    extra.insert(key_str, val);
                }
            }
        }

        Ok(Frontmatter {
            title,
            date,
            note_type,
            domain,
            origin,
            status,
            tags,
            source,
            creator,
            pinned,
            extra,
        })
    }

    /// Serialize back to YAML string, preserving extra fields.
    /// Fields emitted in canonical order: title, date, type, domain, origin, tags,
    /// status, source, creator, then extra fields alphabetically.
    pub fn to_yaml(&self) -> Result<String> {
        let mut mapping = serde_yaml::Mapping::new();

        if let Some(ref title) = self.title {
            mapping.insert(
                serde_yaml::Value::String("title".to_string()),
                serde_yaml::Value::String(title.clone()),
            );
        }
        if let Some(ref date) = self.date {
            mapping.insert(
                serde_yaml::Value::String("date".to_string()),
                serde_yaml::Value::String(date.clone()),
            );
        }
        if let Some(ref note_type) = self.note_type {
            mapping.insert(
                serde_yaml::Value::String("type".to_string()),
                serde_yaml::Value::String(note_type.clone()),
            );
        }
        if let Some(ref domain) = self.domain {
            mapping.insert(
                serde_yaml::Value::String("domain".to_string()),
                serde_yaml::Value::String(domain.clone()),
            );
        }
        if let Some(ref origin) = self.origin {
            mapping.insert(
                serde_yaml::Value::String("origin".to_string()),
                serde_yaml::Value::String(origin.clone()),
            );
        }
        if let Some(ref tags) = self.tags {
            let seq: Vec<serde_yaml::Value> = tags.iter().map(|t| serde_yaml::Value::String(t.clone())).collect();
            mapping.insert(
                serde_yaml::Value::String("tags".to_string()),
                serde_yaml::Value::Sequence(seq),
            );
        }
        if let Some(ref status) = self.status {
            mapping.insert(
                serde_yaml::Value::String("status".to_string()),
                serde_yaml::Value::String(status.clone()),
            );
        }
        if let Some(ref source) = self.source {
            mapping.insert(
                serde_yaml::Value::String("source".to_string()),
                serde_yaml::Value::String(source.clone()),
            );
        }
        if let Some(ref creator) = self.creator {
            mapping.insert(
                serde_yaml::Value::String("creator".to_string()),
                serde_yaml::Value::String(creator.clone()),
            );
        }
        if let Some(pinned) = self.pinned {
            mapping.insert(
                serde_yaml::Value::String("pinned".to_string()),
                serde_yaml::Value::Bool(pinned),
            );
        }

        // Add extra fields alphabetically
        let mut extra_keys: Vec<&String> = self.extra.keys().collect();
        extra_keys.sort();
        for key in extra_keys {
            if let Some(value) = self.extra.get(key) {
                mapping.insert(serde_yaml::Value::String(key.clone()), value.clone());
            }
        }

        let yaml =
            serde_yaml::to_string(&serde_yaml::Value::Mapping(mapping)).context("failed to serialize frontmatter")?;
        Ok(yaml)
    }

    /// Check if frontmatter is completely empty (no fields set).
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.date.is_none()
            && self.note_type.is_none()
            && self.domain.is_none()
            && self.origin.is_none()
            && self.status.is_none()
            && self.tags.is_none()
            && self.source.is_none()
            && self.creator.is_none()
            && self.pinned.is_none()
            && self.extra.is_empty()
    }
}

/// Split a raw note into its YAML frontmatter and body WITHOUT parsing the
/// YAML. Returns `None` when there is no leading `---`-delimited block.
///
/// The returned frontmatter excludes both `---` delimiters and any leading
/// newlines after the opener; the body is the RAW remainder after the closing
/// `\n---` (callers trim it if they want — `parse_frontmatter` does). This is
/// THE shared splitter — it replaced five ad-hoc copies across borg/cortex
/// (`replay`, `migrate` ×2, `audit`, `backfill`) and backs `parse_frontmatter`,
/// so the split semantics can never diverge again.
pub fn split_raw(raw: &str) -> Option<(&str, &str)> {
    let trimmed = raw.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }
    let after_opening = trimmed[3..].trim_start_matches(['\r', '\n']);
    // The closing delimiter must be a FULL `---` line. A bare `find("\n---")`
    // matched `---` inside a multi-line YAML value (e.g. a block scalar that
    // happens to contain a `---` line), truncating the frontmatter mid-value.
    // Accept a match only when the rest of that line is blank (EOF, `\n`, or
    // trailing whitespace) - which also rejects `----`/`---foo`.
    let mut search_from = 0;
    let end_pos = loop {
        let rel = after_opening[search_from..].find("\n---")?;
        let pos = search_from + rel; // index of the '\n' before "---"
        let after = pos + 4; // index just past "---"
        let line_rest = after_opening[after..].split('\n').next().unwrap_or("");
        if line_rest.trim().is_empty() {
            break pos;
        }
        search_from = pos + 1; // false match (e.g. "---" inside a value); keep scanning
    };
    let yaml = &after_opening[..end_pos];
    let body = &after_opening[end_pos + 4..];
    Some((yaml, body))
}

/// Split raw markdown into frontmatter and body.
pub fn parse_frontmatter(raw: &str) -> Result<(Frontmatter, String)> {
    log::debug!("parse_frontmatter: raw_len={}", raw.len());
    let Some((yaml_str, body)) = split_raw(raw) else {
        return Ok((Frontmatter::default(), raw.to_string()));
    };
    let body = body.trim_start_matches(['\r', '\n']);

    let value: serde_yaml::Value = match serde_yaml::from_str(yaml_str) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("parse_frontmatter: malformed YAML frontmatter, falling back to empty metadata: {e}");
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
        }
    };
    let frontmatter = Frontmatter::from_value(value)?;
    Ok((frontmatter, body.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_frontmatter_basic() {
        let raw = "---\ntitle: Test\ntype: note\n---\nBody text.";
        let (fm, body) = parse_frontmatter(raw).expect("parse");
        assert_eq!(fm.title.as_deref(), Some("Test"));
        assert_eq!(fm.note_type.as_deref(), Some("note"));
        assert_eq!(body, "Body text.");
    }

    #[test]
    fn test_parse_frontmatter_none() {
        let raw = "Just some text without frontmatter.";
        let (fm, body) = parse_frontmatter(raw).expect("parse");
        assert!(fm.is_empty());
        assert_eq!(body, raw);
    }

    #[test]
    fn test_parse_frontmatter_no_closing() {
        let raw = "---\ntitle: Test\nNo closing delimiter.";
        let (fm, _body) = parse_frontmatter(raw).expect("parse");
        assert!(fm.is_empty());
    }

    #[test]
    fn test_frontmatter_roundtrip() {
        let fm = Frontmatter {
            title: Some("Test".to_string()),
            date: Some("2026-01-01".to_string()),
            note_type: Some("note".to_string()),
            domain: Some("tech".to_string()),
            origin: Some("authored".to_string()),
            tags: Some(vec!["rust".to_string()]),
            ..Default::default()
        };

        let yaml = fm.to_yaml().expect("to_yaml");
        assert!(yaml.contains("title: Test"));
        assert!(yaml.contains("domain: tech"));
    }

    #[test]
    fn test_frontmatter_extra_fields() {
        let raw = "---\ntitle: Test\ncustom: value\n---\nBody.";
        let (fm, _) = parse_frontmatter(raw).expect("parse");
        assert_eq!(fm.title.as_deref(), Some("Test"));
        assert!(fm.extra.contains_key("custom"));
    }

    #[test]
    fn pinned_roundtrips_through_yaml() {
        let raw = "---\ntitle: P\npinned: true\n---\nBody.";
        let (fm, _) = parse_frontmatter(raw).expect("parse");
        assert_eq!(fm.pinned, Some(true));

        let emitted = fm.to_yaml().expect("to_yaml");
        assert!(emitted.contains("pinned: true"), "yaml omitted pinned: {emitted}");

        // Round-trip back.
        let raw2 = format!("---\n{emitted}---\n");
        let (fm2, _) = parse_frontmatter(&raw2).expect("reparse");
        assert_eq!(fm2.pinned, Some(true));
    }

    #[test]
    fn pinned_missing_is_none() {
        let raw = "---\ntitle: X\n---\nBody.";
        let (fm, _) = parse_frontmatter(raw).expect("parse");
        assert!(fm.pinned.is_none());
    }

    #[test]
    fn pinned_strict_bool_only() {
        // pinned: "true" (string), pinned: 1 (int), and a null pinned: all
        // resolve to None rather than `Some` of something weird. Indexing
        // these as 0 keeps a typo from breaking reindex.
        let raw = "---\ntitle: X\npinned: \"true\"\n---\nBody.";
        let (fm, _) = parse_frontmatter(raw).expect("string");
        assert_eq!(fm.pinned, None, "string value must not parse as Some");

        let raw = "---\ntitle: X\npinned: 1\n---\nBody.";
        let (fm, _) = parse_frontmatter(raw).expect("int");
        assert_eq!(fm.pinned, None, "int value must not parse as Some");

        let raw = "---\ntitle: X\npinned: ~\n---\nBody.";
        let (fm, _) = parse_frontmatter(raw).expect("null");
        assert_eq!(fm.pinned, None, "explicit null must not parse as Some");
    }

    #[test]
    fn test_frontmatter_is_empty() {
        assert!(Frontmatter::default().is_empty());
        assert!(
            !Frontmatter {
                title: Some("x".to_string()),
                ..Default::default()
            }
            .is_empty()
        );
    }

    #[test]
    fn scalar_number_coerces_to_plain_text_not_debug() {
        // `date: 2023` is a bare YAML integer; it must store "2023", not
        // the old `"Number(2023)"` debug rendering of the Value enum.
        let raw = "---\ndate: 2023\n---\nBody.";
        let (fm, _) = parse_frontmatter(raw).expect("parse");
        assert_eq!(fm.date.as_deref(), Some("2023"));
    }

    #[test]
    fn scalar_bool_coerces_to_plain_text() {
        let raw = "---\nstatus: true\n---\nBody.";
        let (fm, _) = parse_frontmatter(raw).expect("parse");
        assert_eq!(fm.status.as_deref(), Some("true"));
    }

    #[test]
    fn scalar_null_yields_none() {
        let raw = "---\ntitle: ~\n---\nBody.";
        let (fm, _) = parse_frontmatter(raw).expect("parse");
        assert_eq!(fm.title, None);
    }

    #[test]
    fn scalar_sequence_yields_none() {
        // A sequence value on a string-typed field is not coercible to a
        // scalar string; it drops to None rather than a debug rendering.
        let raw = "---\nsource:\n  - a\n  - b\n---\nBody.";
        let (fm, _) = parse_frontmatter(raw).expect("parse");
        assert_eq!(fm.source, None);
    }

    #[test]
    fn scalar_string_passes_through() {
        let raw = "---\ndate: 2023-01-13\ntitle: Hello\n---\nBody.";
        let (fm, _) = parse_frontmatter(raw).expect("parse");
        assert_eq!(fm.date.as_deref(), Some("2023-01-13"));
        assert_eq!(fm.title.as_deref(), Some("Hello"));
    }
}
