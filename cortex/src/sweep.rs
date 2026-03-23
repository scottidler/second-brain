use eyre::{Result, WrapErr};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use vault::canonical::{self, CanonicalTagsFile};

use crate::config::SweepConfig;
use crate::tags::replace_tags_in_frontmatter;
use crate::vault::Note;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Proposal {
    pub tag: String,
    pub frequency: usize,
    pub suggested_canonical: Option<String>,
    pub action: String,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposalsFile {
    pub proposals: Vec<Proposal>,
}

/// Run tag sweep migration on all notes in the vault.
///
/// Rewrites each note's tags using the canonical mapping.
/// Returns the number of notes modified.
pub fn run_migrate(vault_root: &Path, notes: &[Note], config: &SweepConfig, dry_run: bool) -> Result<usize> {
    let canonical_file =
        CanonicalTagsFile::load(Path::new(&config.canonical_path)).wrap_err("failed to load canonical tags")?;
    let mapping =
        canonical::load_tag_mapping(Path::new(&config.mapping_path)).wrap_err("failed to load tag mapping")?;
    let canonical_set = canonical_file.all_tags();
    let max_per_note = canonical_file.max_per_note;

    let mut modified_count = 0;

    for note in notes {
        let tags = note.frontmatter.tags.clone().unwrap_or_default();

        if tags.is_empty() {
            continue;
        }

        let new_tags = canonical::filter_and_cap(&tags, &canonical_set, &mapping, max_per_note);

        if new_tags != tags {
            if dry_run {
                let dropped: Vec<_> = tags.iter().filter(|t| !new_tags.contains(t)).collect();
                log::info!(
                    "would rewrite {}: {} -> {} tags (drop: {:?})",
                    note.path.display(),
                    tags.len(),
                    new_tags.len(),
                    dropped
                );
            } else {
                let full_path = vault_root.join(&note.path);
                rewrite_note_tags(&full_path, &new_tags)?;
                log::info!(
                    "rewrote {}: {} -> {} tags",
                    note.path.display(),
                    tags.len(),
                    new_tags.len()
                );
            }
            modified_count += 1;
        }
    }

    Ok(modified_count)
}

/// Scan notes for non-canonical tags and generate proposals.
pub fn scan_proposals(notes: &[Note], config: &SweepConfig) -> Result<Vec<Proposal>> {
    let canonical_file =
        CanonicalTagsFile::load(Path::new(&config.canonical_path)).wrap_err("failed to load canonical tags")?;
    let mapping =
        canonical::load_tag_mapping(Path::new(&config.mapping_path)).wrap_err("failed to load tag mapping")?;
    let canonical_set = canonical_file.all_tags();

    // Count non-canonical tags across all notes
    let mut non_canonical: HashMap<String, Vec<String>> = HashMap::new();

    for note in notes {
        let tags = note.frontmatter.tags.clone().unwrap_or_default();

        for tag in &tags {
            let matches = canonical::match_to_canonical(tag, &canonical_set, &mapping);
            if matches.is_empty() {
                non_canonical
                    .entry(tag.clone())
                    .or_default()
                    .push(note.path.to_string_lossy().to_string());
            }
        }
    }

    // Filter to tags meeting proposal threshold
    let threshold = config.proposal_threshold;
    let proposals: Vec<Proposal> = non_canonical
        .into_iter()
        .filter(|(_, notes)| notes.len() >= threshold)
        .map(|(tag, notes)| Proposal {
            frequency: notes.len(),
            suggested_canonical: None,
            action: "review".to_string(),
            notes,
            tag,
        })
        .collect();

    Ok(proposals)
}

/// Write proposals to the proposals file, merging with existing.
pub fn write_proposals(config: &SweepConfig, new_proposals: Vec<Proposal>) -> Result<()> {
    let path = shellexpand::tilde(&config.proposals_path).to_string();
    let mut existing = load_proposals(&path).unwrap_or(ProposalsFile { proposals: Vec::new() });

    // Merge: update frequency for existing tags, add new ones
    for proposal in new_proposals {
        if let Some(existing_proposal) = existing.proposals.iter_mut().find(|p| p.tag == proposal.tag) {
            existing_proposal.frequency = proposal.frequency;
            existing_proposal.notes = proposal.notes;
        } else {
            existing.proposals.push(proposal);
        }
    }

    let yaml = serde_yaml::to_string(&existing).wrap_err("failed to serialize proposals")?;
    std::fs::write(&path, yaml).wrap_err("failed to write proposals file")?;
    Ok(())
}

fn load_proposals(path: &str) -> Result<ProposalsFile> {
    let content = std::fs::read_to_string(path).wrap_err("failed to read proposals file")?;
    let file: ProposalsFile = serde_yaml::from_str(&content).wrap_err("failed to parse proposals YAML")?;
    Ok(file)
}

fn rewrite_note_tags(path: &Path, new_tags: &[String]) -> Result<()> {
    let content = std::fs::read_to_string(path).wrap_err("failed to read note")?;
    if let Some(new_content) = replace_tags_in_frontmatter(&content, new_tags) {
        std::fs::write(path, new_content).wrap_err("failed to write note")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::NoteBuilder;

    fn make_config(dir: &Path) -> SweepConfig {
        let canonical_path = dir.join("canonical-tags.yml");
        let mapping_path = dir.join("tag-mapping.yml");
        let proposals_path = dir.join("tag-proposals.yml");

        std::fs::write(
            &canonical_path,
            "max-per-note: 3\nmax-canonical: 300\ntags:\n  ai:\n    - ai\n    - claude\n    - llm\n  tech:\n    - rust\n    - python\n",
        )
        .expect("write canonical");
        std::fs::write(
            &mapping_path,
            "ai-agents: ai\nai-coding: ai\nclaudecodeai: null\nrustlang: rust\n",
        )
        .expect("write mapping");
        std::fs::write(&proposals_path, "proposals: []\n").expect("write proposals");

        SweepConfig {
            canonical_path: canonical_path.to_string_lossy().to_string(),
            mapping_path: mapping_path.to_string_lossy().to_string(),
            proposals_path: proposals_path.to_string_lossy().to_string(),
            sweep_interval: "1h".to_string(),
            proposal_threshold: 2,
        }
    }

    #[test]
    fn test_scan_proposals_finds_non_canonical() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let config = make_config(dir.path());

        let notes = vec![
            NoteBuilder::new("notes/a.md").tags(&["unknown-tag", "ai"]).build(),
            NoteBuilder::new("notes/b.md").tags(&["unknown-tag", "rust"]).build(),
            NoteBuilder::new("notes/c.md").tags(&["other-tag", "python"]).build(),
        ];

        let proposals = scan_proposals(&notes, &config).expect("scan");
        // "unknown-tag" appears on 2 notes, meets threshold of 2
        assert!(proposals.iter().any(|p| p.tag == "unknown-tag"));
        // "other-tag" appears on 1 note, below threshold
        assert!(!proposals.iter().any(|p| p.tag == "other-tag"));
    }

    #[test]
    fn test_scan_proposals_mapped_tags_not_proposed() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let config = make_config(dir.path());

        let notes = vec![
            NoteBuilder::new("notes/a.md").tags(&["ai-agents", "rustlang"]).build(),
            NoteBuilder::new("notes/b.md").tags(&["ai-agents", "python"]).build(),
        ];

        let proposals = scan_proposals(&notes, &config).expect("scan");
        // ai-agents maps to "ai" in the mapping file, so it should NOT be proposed
        assert!(proposals.is_empty());
    }
}
