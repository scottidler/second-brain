use super::*;
use crate::vault::Note;

/// Extractor that returns the candidate entities whose name appears
/// (case-insensitively) in the body — deterministic, no LLM.
struct MockExtractor {
    candidates: Vec<String>,
}

impl EntityExtractor for MockExtractor {
    fn extract(&self, note_body: &str) -> Vec<String> {
        let lower = note_body.to_lowercase();
        self.candidates
            .iter()
            .filter(|c| lower.contains(&c.to_lowercase()))
            .cloned()
            .collect()
    }
}

fn ingested(path: &str, body: &str) -> Note {
    let fm = vault::frontmatter::Frontmatter {
        origin: Some("assisted".to_string()),
        ..Default::default()
    };
    Note {
        path: std::path::PathBuf::from(path),
        frontmatter: fm,
        body: body.to_string(),
        raw: String::new(),
    }
}

fn authored(path: &str, body: &str) -> Note {
    let fm = vault::frontmatter::Frontmatter {
        origin: Some("authored".to_string()),
        ..Default::default()
    };
    Note {
        path: std::path::PathBuf::from(path),
        frontmatter: fm,
        body: body.to_string(),
        raw: String::new(),
    }
}

fn known(slugs: &[&str]) -> std::collections::HashSet<String> {
    slugs.iter().map(|s| s.to_string()).collect()
}

#[test]
fn discover_excludes_known_glossary_slugs() {
    let extractor = MockExtractor {
        candidates: vec!["LangChain".into(), "GraphRAG".into()],
    };
    let notes = vec![ingested("notes/a.md", "We use LangChain and GraphRAG together.")];
    // langchain already known -> only graphrag proposed.
    let (proposals, scanned) = discover(&notes, &known(&["langchain"]), &extractor, 100);
    assert_eq!(scanned, 1);
    let slugs: Vec<&str> = proposals.iter().map(|p| p.slug.as_str()).collect();
    assert_eq!(slugs, vec!["graphrag"]);
}

#[test]
fn discover_aggregates_frequency_and_surface() {
    let extractor = MockExtractor {
        candidates: vec!["GraphRAG".into()],
    };
    let notes = vec![
        ingested("notes/a.md", "GraphRAG is great."),
        ingested("notes/b.md", "More on GraphRAG here."),
    ];
    let (proposals, _) = discover(&notes, &known(&[]), &extractor, 100);
    assert_eq!(proposals.len(), 1);
    assert_eq!(proposals[0].slug, "graphrag");
    assert_eq!(proposals[0].surface, "GraphRAG");
    assert_eq!(proposals[0].frequency, 2, "mentioned in two notes");
    assert_eq!(proposals[0].notes.len(), 2, "both source notes sampled");
}

#[test]
fn discover_scopes_to_ingested_notes_only() {
    let extractor = MockExtractor {
        candidates: vec!["GraphRAG".into()],
    };
    let notes = vec![
        authored("notes/hand.md", "GraphRAG in a hand-authored note."),
        ingested("notes/ingest.md", "GraphRAG in an ingested note."),
    ];
    let (proposals, scanned) = discover(&notes, &known(&[]), &extractor, 100);
    assert_eq!(scanned, 1, "only the ingested note is scanned");
    assert_eq!(proposals[0].frequency, 1, "authored note excluded from frequency");
}

#[test]
fn discover_honors_limit() {
    let extractor = MockExtractor {
        candidates: vec!["GraphRAG".into()],
    };
    let notes = vec![
        ingested("notes/a.md", "GraphRAG one."),
        ingested("notes/b.md", "GraphRAG two."),
        ingested("notes/c.md", "GraphRAG three."),
    ];
    let (proposals, scanned) = discover(&notes, &known(&[]), &extractor, 2);
    assert_eq!(scanned, 2, "limit caps notes processed");
    assert_eq!(proposals[0].frequency, 2);
}

#[test]
fn discover_counts_entity_once_per_note() {
    let extractor = MockExtractor {
        candidates: vec!["GraphRAG".into()],
    };
    // Body mentions it many times; MockExtractor returns it once, but even if
    // it returned duplicates the per-note dedup caps frequency at 1 per note.
    let notes = vec![ingested("notes/a.md", "GraphRAG GraphRAG GraphRAG.")];
    let (proposals, _) = discover(&notes, &known(&[]), &extractor, 100);
    assert_eq!(proposals[0].frequency, 1);
}

#[test]
fn proposals_file_serde_roundtrips() {
    let file = EntityProposalsFile {
        proposals: vec![EntityProposal {
            slug: "graphrag".into(),
            surface: "GraphRAG".into(),
            frequency: 3,
            notes: vec!["notes/a.md".into()],
        }],
    };
    let yaml = serde_yaml::to_string(&file).expect("ser");
    let back: EntityProposalsFile = serde_yaml::from_str(&yaml).expect("de");
    assert_eq!(back.proposals, file.proposals);
    // kebab-case keys in the YAML.
    assert!(yaml.contains("frequency:"));
}

#[test]
fn write_proposals_merges_without_clobbering_existing() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("entity-proposals.yml");

    super::write_proposals(
        &path,
        vec![EntityProposal {
            slug: "graphrag".into(),
            surface: "GraphRAG".into(),
            frequency: 1,
            notes: vec![],
        }],
    )
    .expect("write1");

    // A second run proposes a new one plus the existing slug; existing wins.
    super::write_proposals(
        &path,
        vec![
            EntityProposal {
                slug: "graphrag".into(),
                surface: "graph rag".into(),
                frequency: 9,
                notes: vec![],
            },
            EntityProposal {
                slug: "cognee".into(),
                surface: "Cognee".into(),
                frequency: 2,
                notes: vec![],
            },
        ],
    )
    .expect("write2");

    let content = std::fs::read_to_string(&path).expect("read");
    let file: EntityProposalsFile = serde_yaml::from_str(&content).expect("de");
    let slugs: std::collections::HashSet<&str> = file.proposals.iter().map(|p| p.slug.as_str()).collect();
    assert!(slugs.contains("graphrag") && slugs.contains("cognee"));
    // Existing graphrag kept its original frequency (1), not the re-proposed 9.
    let gr = file.proposals.iter().find(|p| p.slug == "graphrag").unwrap();
    assert_eq!(gr.frequency, 1, "existing proposal not clobbered");
}
