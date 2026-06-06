use super::*;
use std::io::Write;

fn write_tmp(yaml: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().expect("tmp");
    f.write_all(yaml.as_bytes()).expect("write");
    f
}

#[test]
fn loads_valid_query_set() {
    let f = write_tmp("queries:\n  - id: a\n    query: hello world\n    domain: ai\n  - id: b\n    query: another\n");
    let q = Queries::load(f.path()).expect("load");
    assert_eq!(q.queries.len(), 2);
    assert_eq!(q.queries[0].id, "a");
    assert_eq!(q.queries[0].domain.as_deref(), Some("ai"));
    assert!(q.queries[1].domain.is_none());
}

#[test]
fn rejects_duplicate_ids() {
    let f = write_tmp("queries:\n  - id: dup\n    query: one\n  - id: dup\n    query: two\n");
    let err = Queries::load(f.path()).expect_err("must reject dup id");
    assert!(format!("{err}").contains("duplicate query id"));
}

#[test]
fn rejects_empty_query_set() {
    let f = write_tmp("queries: []\n");
    let err = Queries::load(f.path()).expect_err("must reject empty");
    assert!(format!("{err}").contains("no queries"));
}

#[test]
fn rejects_out_of_range_calibration_score() {
    let f = write_tmp("queries:\n  - id: a\n    query: q\n    calibration:\n      \"notes/x.md\": 5\n");
    let err = Queries::load(f.path()).expect_err("must reject >3");
    assert!(format!("{err}").contains("must be 0..3"));
}

#[test]
fn calibration_iterator_selects_only_labeled_queries() {
    let f = write_tmp(
        "queries:\n  - id: plain\n    query: q\n  - id: cal\n    query: q2\n    calibration:\n      \"notes/x.md\": 3\n",
    );
    let q = Queries::load(f.path()).expect("load");
    let cal: Vec<&str> = q.calibration().map(|e| e.id.as_str()).collect();
    assert_eq!(cal, vec!["cal"]);
}
