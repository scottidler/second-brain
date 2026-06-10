use super::*;
use crate::config::RerankConfig;
use std::path::PathBuf;
use vault::frontmatter::Frontmatter;
use vault::note::Note;
use vault::search::{MockReranker, SearchIndex};

fn seed(db: &SearchIndex, path: &str, body: &str) {
    let fm = Frontmatter {
        title: Some(path.to_string()),
        note_type: Some("article".to_string()),
        origin: Some("assisted".to_string()),
        domain: Some("ai".to_string()),
        ..Frontmatter::default()
    };
    let note = Note {
        path: PathBuf::from(path),
        frontmatter: fm,
        body: body.to_string(),
        raw: format!("---\n---\n{body}"),
    };
    db.index_one(&note, 100).expect("seed note");
}

fn rerank_cfg(input_k: u32, latency_budget_ms: u64) -> RerankConfig {
    RerankConfig {
        enabled: true,
        input_k,
        latency_budget_ms,
        ..RerankConfig::default()
    }
}

/// The head (top `input_k`) is reranked by the injected scorer; the tail keeps
/// its fused order and is appended untouched.
#[test]
fn rerank_reorders_head_preserves_tail() {
    let db = SearchIndex::open_memory().expect("db");
    seed(&db, "notes/p0.md", "alpha content");
    seed(&db, "notes/p1.md", "query content"); // contains the query token
    seed(&db, "notes/p2.md", "gamma content");
    seed(&db, "notes/p3.md", "delta content");

    let fused = vec![
        "notes/p0.md".to_string(),
        "notes/p1.md".to_string(),
        "notes/p2.md".to_string(),
        "notes/p3.md".to_string(),
    ];
    // Generous budget so the probe never trips.
    let cfg = rerank_cfg(2, u64::MAX);
    let reranker = MockReranker::new();

    let outcome = OracleMcpServer::rerank_within_budget(&db, &cfg, "query", fused, &reranker).expect("rerank");
    match outcome {
        RerankOutcome::Reordered(paths) => {
            // Head [p0,p1] reranked: p1 (overlaps "query") outranks p0.
            // Tail [p2,p3] preserved in fused order.
            assert_eq!(
                paths,
                vec![
                    "notes/p1.md".to_string(),
                    "notes/p0.md".to_string(),
                    "notes/p2.md".to_string(),
                    "notes/p3.md".to_string(),
                ]
            );
        }
        _ => panic!("expected Reordered"),
    }
}

/// `input_k = 0` short-circuits to fail-open (no reorder, no disable).
#[test]
fn rerank_input_k_zero_fails_open() {
    let db = SearchIndex::open_memory().expect("db");
    seed(&db, "notes/p0.md", "alpha");
    seed(&db, "notes/p1.md", "query");
    let fused = vec!["notes/p0.md".to_string(), "notes/p1.md".to_string()];
    let cfg = rerank_cfg(0, u64::MAX);
    let reranker = MockReranker::new();

    let outcome = OracleMcpServer::rerank_within_budget(&db, &cfg, "query", fused.clone(), &reranker).expect("rerank");
    match outcome {
        RerankOutcome::FailOpen(paths) => assert_eq!(paths, fused),
        _ => panic!("expected FailOpen"),
    }
}

/// A single candidate short-circuits to fail-open.
#[test]
fn rerank_single_candidate_fails_open() {
    let db = SearchIndex::open_memory().expect("db");
    seed(&db, "notes/p0.md", "alpha");
    let fused = vec!["notes/p0.md".to_string()];
    let cfg = rerank_cfg(5, u64::MAX);
    let reranker = MockReranker::new();

    let outcome = OracleMcpServer::rerank_within_budget(&db, &cfg, "query", fused.clone(), &reranker).expect("rerank");
    match outcome {
        RerankOutcome::FailOpen(paths) => assert_eq!(paths, fused),
        _ => panic!("expected FailOpen"),
    }
}

/// A reranker slow enough that the projected batch cost exceeds a zero budget
/// trips the budget branch: fused order returned, caller told to disable.
#[test]
fn rerank_over_budget_disables() {
    struct SlowMock;
    impl vault::search::Reranker for SlowMock {
        fn model_id(&self) -> &str {
            "slow-mock"
        }
        fn score(&self, _query: &str, docs: &[&str]) -> eyre::Result<Vec<f32>> {
            // Sleep so the probe's measured per-pair cost is reliably positive,
            // making `projected > 0` deterministic against a zero budget.
            std::thread::sleep(std::time::Duration::from_millis(3));
            Ok(vec![0.0; docs.len()])
        }
    }

    let db = SearchIndex::open_memory().expect("db");
    seed(&db, "notes/p0.md", "alpha");
    seed(&db, "notes/p1.md", "beta");
    let fused = vec!["notes/p0.md".to_string(), "notes/p1.md".to_string()];
    let cfg = rerank_cfg(2, 0);

    let outcome = OracleMcpServer::rerank_within_budget(&db, &cfg, "query", fused.clone(), &SlowMock).expect("rerank");
    match outcome {
        RerankOutcome::Disable(paths) => assert_eq!(paths, fused),
        _ => panic!("expected Disable (over budget)"),
    }
}
