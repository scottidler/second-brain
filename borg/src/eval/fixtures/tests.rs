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
    // no tools, no TOOLS section
    assert!(!text.contains("TOOLS:"));
}

#[test]
fn judge_note_text_renders_quote_and_who_under_each_claim() {
    let d = Distilled {
        summary: "s".to_string(),
        claims: vec![
            Claim {
                text: "attributed claim".to_string(),
                anchor: Some("01:02".to_string()),
                who: Some("@simonw".to_string()),
                quote: Some("agents need a sandbox".to_string()),
                ..Default::default()
            },
            Claim {
                text: "blank extras".to_string(),
                who: Some("  ".to_string()),
                quote: Some(String::new()),
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let text = judge_note_text(&d);
    assert!(
        text.contains("- attributed claim [01:02]\n  who: @simonw\n  quote: \"agents need a sandbox\"\n"),
        "{text}"
    );
    // Blank who/quote render nothing, not an empty line.
    assert!(text.contains("- blank extras\n"), "{text}");
    assert_eq!(text.matches("who:").count(), 1, "{text}");
    assert_eq!(text.matches("quote:").count(), 1, "{text}");
}

#[test]
fn judge_note_text_includes_the_tools_section() {
    let d = Distilled {
        summary: "s".to_string(),
        tools: vec![
            vault::distilled::Tool {
                name: "ripgrep".to_string(),
                url: Some("https://github.com/BurntSushi/ripgrep".to_string()),
                command: Some("cargo install ripgrep".to_string()),
            },
            vault::distilled::Tool {
                name: "fd".to_string(),
                url: None,
                command: None,
            },
        ],
        kind_specific: Some(KindPayload::Repo(vault::distilled::RepoPayload {
            install: Some("npm install -g granola-cli".to_string()),
            ..Default::default()
        })),
        ..Default::default()
    };
    let text = judge_note_text(&d);
    assert!(
        text.contains(
            "\nTOOLS:\n- ripgrep https://github.com/BurntSushi/ripgrep: cargo install ripgrep\n- fd\n- Install: npm install -g granola-cli\n"
        ),
        "{text}"
    );
}

#[test]
fn load_reads_an_optional_trimmed_capture_note() {
    let tmp = tempfile::tempdir().expect("tmp");
    write_fixture(tmp.path(), "video", "noted", "src", MINIMAL_DISTILLED);
    write_fixture(tmp.path(), "video", "bare", "src", MINIMAL_DISTILLED);
    write_fixture(tmp.path(), "video", "blank", "src", MINIMAL_DISTILLED);
    std::fs::write(
        tmp.path().join("video/noted").join(CAPTURE_NOTE_FILE),
        "  saving this for X  \n",
    )
    .expect("note");
    std::fs::write(tmp.path().join("video/blank").join(CAPTURE_NOTE_FILE), " \n\t").expect("note");

    let fixtures = load(tmp.path()).expect("load");
    let note = |id: &str| fixtures.iter().find(|f| f.id == id).expect(id).capture_note.clone();
    assert_eq!(note("video/noted").as_deref(), Some("saving this for X"));
    assert_eq!(note("video/bare"), None);
    assert_eq!(note("video/blank"), None);
}

// --- render_options_for_kind (sb borg eval note-size wiring) -----

#[test]
fn render_options_video_article_repo_are_transcript_free() {
    let d = Distilled::default();
    for kind in ["video", "article", "repo"] {
        let opts = render_options_for_kind(kind, &d);
        assert!(!opts.include_transcript, "{kind} publish must be transcript-free");
    }
}

#[test]
fn render_options_thread_is_transcript_free() {
    let d = Distilled {
        tools: Vec::new(),
        kind_specific: Some(KindPayload::Thread(ThreadPayload::default())),
        ..Default::default()
    };
    assert!(!render_options_for_kind("thread", &d).include_transcript);
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
    // `sb borg eval` must score the session kind. The loader is
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
    // Guards the checked-in fixture tree (including the session
    // fixture): every distilled.yml must parse as a valid Distilled, and the
    // session kind must be present so `sb borg eval` scores it.
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../config/eval/distill-fixtures");
    let fixtures = load(std::path::Path::new(dir)).expect("real repo fixtures load");
    assert!(
        fixtures.iter().any(|f| f.kind == "session"),
        "the checked-in fixture tree includes a session fixture"
    );
}

fn write_trace(root: &std::path::Path, trace: &str, kind: &str, distilled: &str, files: &[(&str, &str)]) {
    let dir = root.join(trace);
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join(ENVELOPE_FILE), format!("trace: {trace}\nkind: {kind}\n")).expect("write envelope");
    std::fs::write(dir.join(DISTILLED_FILE), distilled).expect("write distilled");
    for (name, body) in files {
        std::fs::write(dir.join(name), body).expect("write file");
    }
}

fn with_transcript(transcript: &str) -> String {
    format!("{MINIMAL_DISTILLED}transcript: \"{transcript}\"\n")
}

#[test]
fn load_staged_picks_source_by_precedence_and_skips_unsourced() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    write_trace(root, "ht-1", "youtube-url", &with_transcript("the transcript"), &[]);
    write_trace(
        root,
        "cl-2",
        "git-hub-url",
        MINIMAL_DISTILLED,
        &[
            ("fetched.html", "# readme body"),
            ("body.txt", "https://github.com/x/y"),
        ],
    );
    write_trace(
        root,
        "hv-3",
        "session",
        MINIMAL_DISTILLED,
        &[("body.txt", "user: hi\nassistant: hello")],
    );
    write_trace(
        root,
        "cl-4",
        "article-url",
        MINIMAL_DISTILLED,
        &[("body.txt", "https://example.com/a")],
    );
    write_trace(
        root,
        "cl-5",
        "vocabulary-en",
        MINIMAL_DISTILLED,
        &[("body.txt", "word list")],
    );
    std::fs::create_dir_all(root.join("cl-6")).expect("mkdir stray");

    let fx = load_staged(root, None).expect("load");
    let got: Vec<(&str, &str)> = fx.iter().map(|f| (f.id.as_str(), f.source.as_str())).collect();
    assert_eq!(
        got,
        vec![
            ("repo/cl-2", "# readme body"),
            ("session/hv-3", "user: hi\nassistant: hello"),
            ("video/ht-1", "the transcript"),
        ]
    );
}

#[test]
fn load_staged_sample_is_stratified_and_deterministic() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    for i in 0..6 {
        write_trace(root, &format!("ht-{i}"), "youtube-url", &with_transcript("t"), &[]);
    }
    write_trace(
        root,
        "hv-0",
        "session",
        MINIMAL_DISTILLED,
        &[("body.txt", "a session body")],
    );

    let a = load_staged(root, Some(3)).expect("load");
    let b = load_staged(root, Some(3)).expect("load");
    let ids = |v: &[Fixture]| v.iter().map(|f| f.id.clone()).collect::<Vec<_>>();
    assert_eq!(ids(&a), ids(&b));
    assert_eq!(a.len(), 3);
    assert_eq!(a.iter().filter(|f| f.kind == "session").count(), 1, "{:?}", ids(&a));
    assert_eq!(load_staged(root, Some(100)).expect("load").len(), 7);
}

#[test]
fn load_staged_errors_when_nothing_loads() {
    let tmp = tempfile::tempdir().expect("tempdir");
    assert!(load_staged(tmp.path(), None).is_err());
    assert!(load_staged(&tmp.path().join("missing"), None).is_err());
}

fn stage_trace(root: &std::path::Path, trace: &str, kind: &str, distilled: &str, transcript: Option<&str>) {
    let dir = root.join(trace);
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join(ENVELOPE_FILE), format!("kind: {kind}\n")).expect("envelope");
    std::fs::write(dir.join(DISTILLED_FILE), distilled).expect("distilled");
    if let Some(t) = transcript {
        std::fs::write(dir.join(TRANSCRIPT_FILE), t).expect("transcript");
    }
}

#[test]
fn load_staged_trace_returns_the_one_trace_with_its_source() {
    let tmp = tempfile::tempdir().expect("tmp");
    stage_trace(tmp.path(), "tr-1", "article-url", MINIMAL_DISTILLED, Some("the source"));
    stage_trace(tmp.path(), "tr-2", "article-url", MINIMAL_DISTILLED, Some("another"));
    let fx = load_staged_trace(tmp.path(), "tr-1").expect("load").expect("some");
    assert_eq!(fx.id, "article/tr-1");
    assert_eq!(fx.slug, "tr-1");
    assert_eq!(fx.source, "the source");
}

#[test]
fn load_staged_trace_is_none_for_absent_unsourced_unscored_and_non_bare_ids() {
    let tmp = tempfile::tempdir().expect("tmp");
    stage_trace(tmp.path(), "no-source", "article-url", MINIMAL_DISTILLED, None);
    stage_trace(tmp.path(), "vocab", "vocabulary-en", MINIMAL_DISTILLED, Some("src"));
    for trace in ["missing", "no-source", "vocab", "", ".", "..", "../x", "a/b", "a\\b"] {
        assert!(
            load_staged_trace(tmp.path(), trace).expect("not an error").is_none(),
            "{trace:?}"
        );
    }
}

#[test]
fn load_staged_trace_errors_on_artifacts_that_do_not_parse() {
    let tmp = tempfile::tempdir().expect("tmp");
    stage_trace(tmp.path(), "tr-bad", "article-url", ": : [", Some("src"));
    assert!(load_staged_trace(tmp.path(), "tr-bad").is_err());
}

#[test]
fn probe_staged_trace_loads_a_staged_trace_with_its_source() {
    let tmp = tempfile::tempdir().expect("tmp");
    stage_trace(tmp.path(), "tr-1", "article-url", MINIMAL_DISTILLED, Some("the source"));
    match probe_staged_trace(tmp.path(), "tr-1") {
        StagedTrace::Loaded(fx) => assert_eq!(fx.source, "the source"),
        other => panic!("expected Loaded, got {other:?}"),
    }
}

#[test]
fn probe_staged_trace_tells_a_swept_trace_from_an_unreadable_one() {
    let tmp = tempfile::tempdir().expect("tmp");
    stage_trace(tmp.path(), "no-source", "article-url", MINIMAL_DISTILLED, None);
    stage_trace(tmp.path(), "vocab", "vocabulary-en", MINIMAL_DISTILLED, Some("src"));
    stage_trace(tmp.path(), "tr-bad", "article-url", ": : [", Some("src"));
    std::fs::create_dir_all(tmp.path().join("bare-dir")).expect("mkdir");

    assert!(matches!(probe_staged_trace(tmp.path(), "missing"), StagedTrace::Absent));
    for trace in ["no-source", "vocab", "tr-bad", "bare-dir", "", "..", "a/b"] {
        assert!(
            matches!(probe_staged_trace(tmp.path(), trace), StagedTrace::Unreadable(_)),
            "{trace:?}"
        );
    }
    match probe_staged_trace(tmp.path(), "tr-bad") {
        StagedTrace::Unreadable(why) => assert!(why.contains("parsing"), "{why}"),
        other => panic!("expected Unreadable, got {other:?}"),
    }
}
