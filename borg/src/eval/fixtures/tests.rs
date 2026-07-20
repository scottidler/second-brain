use super::*;
use vault::distilled::{Claim, Distilled, KindPayload, ThreadPayload};

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

// --- render_options_for_kind (Phase 7b: sb borg eval note-size wiring) -----

#[test]
fn render_options_video_article_repo_are_transcript_free() {
    let d = Distilled::default();
    for kind in ["video", "article", "repo"] {
        let opts = render_options_for_kind(kind, &d);
        assert!(!opts.include_transcript, "{kind} publish must be transcript-free");
    }
}

#[test]
fn render_options_thread_keeps_its_transcript() {
    let d = Distilled {
        kind_specific: Some(KindPayload::Thread(ThreadPayload::default())),
        ..Default::default()
    };
    assert!(render_options_for_kind("thread", &d).include_transcript);
}

#[test]
fn render_options_verbatim_kinds_keep_their_transcript() {
    let d = Distilled::default();
    for kind in ["image", "voicenote", "idea", "vocabulary"] {
        assert!(
            render_options_for_kind(kind, &d).include_transcript,
            "{kind} is a verbatim-preservation kind"
        );
    }
}

#[test]
fn session_kind_loads_and_renders_transcript_free() {
    // Phase 7: `sb borg eval` must score the session kind. The loader is
    // kind-agnostic, so a session/<slug>/{source.md,distilled.yml} pair is
    // picked up; and session notes publish transcript-free, so the eval
    // note-size excludes the transcript (matching the harvest publish path).
    let tmp = tempfile::tempdir().expect("tmp");
    write_fixture(
        tmp.path(),
        "session",
        "s-1",
        "USER: hi\nASSISTANT: decided X",
        MINIMAL_DISTILLED,
    );
    let fixtures = load(tmp.path()).expect("load");
    assert!(
        fixtures.iter().any(|f| f.kind == "session"),
        "loader picks up the session kind"
    );

    let distilled: Distilled = serde_yaml::from_str(MINIMAL_DISTILLED).expect("parse");
    let opts = render_options_for_kind("session", &distilled);
    assert!(
        !opts.include_transcript,
        "session notes publish transcript-free, so eval excludes the transcript too"
    );
}

#[test]
fn real_repo_fixtures_load_and_include_session() {
    // Guards the checked-in fixture tree (including the Phase 7 session
    // fixture): every distilled.yml must parse as a valid Distilled, and the
    // session kind must be present so `sb borg eval` scores it.
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../config/eval/distill-fixtures");
    let fixtures = load(std::path::Path::new(dir)).expect("real repo fixtures load");
    assert!(
        fixtures.iter().any(|f| f.kind == "session"),
        "the checked-in fixture tree includes a session fixture"
    );
}
