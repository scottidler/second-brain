use super::*;
use vault::distilled::{Claim, Distilled};

fn write_fixture(root: &std::path::Path, kind: &str, slug: &str, source: &str, distilled: &str) {
    let dir = root.join(kind).join(slug);
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join(SOURCE_FILE), source).expect("write source");
    std::fs::write(dir.join(DISTILLED_FILE), distilled).expect("write distilled");
}

const MINIMAL_DISTILLED: &str = "summary: a summary\n\
claims:\n\
- text: a claim\n  anchor: null\n\
meta:\n  extractor: x\n  model: m\n  produced-at: \"2026-07-05T00:00:00Z\"\n";

#[test]
fn load_reads_pairs_sorted_and_derives_kind() {
    let tmp = tempfile::tempdir().expect("tmp");
    write_fixture(tmp.path(), "video", "b-vid", "src b", MINIMAL_DISTILLED);
    write_fixture(tmp.path(), "article", "a-art", "src a", MINIMAL_DISTILLED);

    let fixtures = load(tmp.path()).expect("load");
    assert_eq!(fixtures.len(), 2);
    // sorted by id: article/a-art before video/b-vid
    assert_eq!(fixtures[0].id, "article/a-art");
    assert_eq!(fixtures[0].kind, "article");
    assert_eq!(fixtures[1].id, "video/b-vid");
    assert_eq!(fixtures[1].distilled.summary, "a summary");
}

#[test]
fn load_skips_dirs_missing_a_file_and_stray_files() {
    let tmp = tempfile::tempdir().expect("tmp");
    write_fixture(tmp.path(), "article", "good", "src", MINIMAL_DISTILLED);
    // a slug dir with only source.md (no distilled.yml) is skipped
    let incomplete = tmp.path().join("article").join("incomplete");
    std::fs::create_dir_all(&incomplete).expect("mkdir");
    std::fs::write(incomplete.join(SOURCE_FILE), "orphan").expect("write");
    // a stray top-level file is ignored
    std::fs::write(tmp.path().join("README.md"), "readme").expect("write readme");

    let fixtures = load(tmp.path()).expect("load");
    assert_eq!(fixtures.len(), 1);
    assert_eq!(fixtures[0].slug, "good");
}

#[test]
fn load_errors_on_empty_tree() {
    let tmp = tempfile::tempdir().expect("tmp");
    let err = load(tmp.path()).expect_err("empty tree is an error");
    assert!(err.to_string().contains("no distillation fixtures"));
}

#[test]
fn load_errors_on_missing_dir() {
    let missing = std::path::Path::new("/nonexistent/distill-fixtures-xyz");
    assert!(load(missing).is_err());
}

#[test]
fn judge_note_text_renders_summary_and_anchored_claims() {
    let d = Distilled {
        summary: "the summary".to_string(),
        claims: vec![
            Claim {
                text: "anchored claim".to_string(),
                anchor: Some("00:14".to_string()),
                ..Default::default()
            },
            Claim {
                text: "bare claim".to_string(),
                anchor: None,
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let text = judge_note_text(&d);
    assert!(text.contains("SUMMARY:\nthe summary"));
    assert!(text.contains("- anchored claim [00:14]"));
    assert!(text.contains("- bare claim\n"));
    // a bare claim must not render an empty anchor bracket
    assert!(!text.contains("bare claim ["));
}
