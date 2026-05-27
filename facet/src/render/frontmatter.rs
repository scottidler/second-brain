//! Frontmatter render + merge. `facet-*` keys are facet-owned (always
//! overwritten); other keys are operator-extensible (preserved across
//! re-renders). `tags` is a union: facet-managed tags always present,
//! operator-added tags survive.

use crate::extract::JudgmentMoment;
use crate::workitem::WorkItem;

/// Built-in facet-managed tags: always present after merge.
const MANAGED_TAGS: &[&str] = &["facet", "judgment"];

/// Render the frontmatter block (between `---` fences). If `existing`
/// holds the previous note's full body, the operator-set keys are
/// preserved.
pub fn render(workitem: &WorkItem, moments: &[JudgmentMoment], existing: Option<&str>) -> String {
    let mut managed = managed_mapping(workitem, moments);
    let operator = existing.and_then(parse_existing_frontmatter).unwrap_or_default();
    merge_operator_keys(&mut managed, operator);
    let yaml = serde_yaml::to_string(&serde_yaml::Value::Mapping(managed))
        .unwrap_or_else(|_| String::from("# yaml render failed\n"));
    format!("---\n{yaml}---\n")
}

/// Build the facet-managed half of the frontmatter.
fn managed_mapping(workitem: &WorkItem, moments: &[JudgmentMoment]) -> serde_yaml::Mapping {
    let mut m = serde_yaml::Mapping::new();
    let title_value = serde_yaml::Value::String(workitem.title.clone());
    m.insert(serde_yaml::Value::String("title".into()), title_value);
    m.insert(
        serde_yaml::Value::String("date".into()),
        serde_yaml::Value::String(workitem.updated_at.format("%Y-%m-%d").to_string()),
    );
    m.insert(
        serde_yaml::Value::String("type".into()),
        serde_yaml::Value::String("facet-workitem".into()),
    );
    m.insert(
        serde_yaml::Value::String("origin".into()),
        serde_yaml::Value::String("assisted".into()),
    );
    m.insert(
        serde_yaml::Value::String("method".into()),
        serde_yaml::Value::String("facet".into()),
    );
    m.insert(
        serde_yaml::Value::String("status".into()),
        serde_yaml::Value::String("unread".into()),
    );
    m.insert(
        serde_yaml::Value::String("domain".into()),
        serde_yaml::Value::String("ai".into()),
    );
    // facet-* managed keys
    m.insert(
        serde_yaml::Value::String("facet-workitem-id".into()),
        serde_yaml::Value::Number(workitem.id.into()),
    );
    m.insert(
        serde_yaml::Value::String("facet-slug".into()),
        serde_yaml::Value::String(workitem.slug.clone()),
    );
    m.insert(
        serde_yaml::Value::String("facet-status".into()),
        serde_yaml::Value::String(workitem.status.as_str().into()),
    );
    m.insert(
        serde_yaml::Value::String("facet-sessions-count".into()),
        serde_yaml::Value::Number((workitem.sessions_count as u64).into()),
    );
    m.insert(
        serde_yaml::Value::String("facet-repos".into()),
        serde_yaml::Value::Sequence(
            workitem
                .repos
                .iter()
                .map(|r| serde_yaml::Value::String(r.clone()))
                .collect(),
        ),
    );
    let mode_set: std::collections::BTreeSet<String> = moments.iter().map(|m| m.mode.clone()).collect();
    m.insert(
        serde_yaml::Value::String("facet-modes".into()),
        serde_yaml::Value::Sequence(mode_set.into_iter().map(serde_yaml::Value::String).collect()),
    );
    m.insert(
        serde_yaml::Value::String("facet-first-seen".into()),
        serde_yaml::Value::String(workitem.created_at.format("%Y-%m-%d").to_string()),
    );
    m.insert(
        serde_yaml::Value::String("facet-last-seen".into()),
        serde_yaml::Value::String(workitem.updated_at.format("%Y-%m-%d").to_string()),
    );
    m.insert(
        serde_yaml::Value::String("facet-extractor".into()),
        serde_yaml::Value::String("facet-v1".into()),
    );
    let tags: Vec<serde_yaml::Value> = managed_tags(workitem)
        .iter()
        .map(|t| serde_yaml::Value::String((*t).to_string()))
        .collect();
    m.insert(
        serde_yaml::Value::String("tags".into()),
        serde_yaml::Value::Sequence(tags),
    );
    m
}

fn managed_tags(workitem: &WorkItem) -> Vec<String> {
    let mut tags: Vec<String> = MANAGED_TAGS.iter().map(|s| (*s).to_string()).collect();
    for repo in &workitem.repos {
        if let Some((_, name)) = repo.rsplit_once('/')
            && !name.is_empty()
        {
            tags.push(name.to_string());
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    tags.retain(|t| seen.insert(t.clone()));
    tags
}

fn merge_operator_keys(managed: &mut serde_yaml::Mapping, operator: serde_yaml::Mapping) {
    for (k, v) in operator {
        let key_str = match &k {
            serde_yaml::Value::String(s) => s.clone(),
            _ => continue,
        };
        if key_str == "tags" {
            // union with managed tags (already in `managed`)
            let mut union: Vec<serde_yaml::Value> = match managed.get(&k) {
                Some(serde_yaml::Value::Sequence(seq)) => seq.clone(),
                _ => Vec::new(),
            };
            if let serde_yaml::Value::Sequence(op_seq) = v {
                let mut seen: std::collections::BTreeSet<String> = union
                    .iter()
                    .filter_map(|t| match t {
                        serde_yaml::Value::String(s) => Some(s.clone()),
                        _ => None,
                    })
                    .collect();
                for t in op_seq {
                    if let serde_yaml::Value::String(s) = t
                        && seen.insert(s.clone())
                    {
                        union.push(serde_yaml::Value::String(s));
                    }
                }
                managed.insert(k, serde_yaml::Value::Sequence(union));
            }
        } else if !is_facet_managed_key(&key_str) && !managed.contains_key(&k) {
            managed.insert(k, v);
        }
    }
}

fn is_facet_managed_key(k: &str) -> bool {
    matches!(k, "title" | "date" | "type" | "origin" | "method" | "status" | "domain") || k.starts_with("facet-")
}

fn parse_existing_frontmatter(body: &str) -> Option<serde_yaml::Mapping> {
    let inner = extract_frontmatter_block(body)?;
    let value: serde_yaml::Value = serde_yaml::from_str(inner).ok()?;
    match value {
        serde_yaml::Value::Mapping(m) => Some(m),
        _ => None,
    }
}

/// Extract the frontmatter inner text between the FIRST pair of `---`
/// fences. Looks past the leading `<!-- facet:auto:begin frontmatter -->`
/// marker if it is the very first thing in the body.
fn extract_frontmatter_block(body: &str) -> Option<&str> {
    let stripped = body
        .strip_prefix("<!-- facet:auto:begin frontmatter -->\n")
        .unwrap_or(body);
    let rest = stripped.strip_prefix("---\n")?;
    let end = rest.find("\n---\n")?;
    Some(&rest[..end])
}

#[cfg(test)]
mod tests;
