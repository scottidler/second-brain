use super::*;
use distillers::{Dispatch, DistillInputs, DistillKind};
use std::sync::{Arc, Mutex};

/// Records the pattern of every request and fails it, so no distill output
/// matters: only what reached fabric does.
#[derive(Clone, Default)]
struct RecordingFabric {
    patterns: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl FabricCaller for RecordingFabric {
    async fn call(&self, request: FabricRequest) -> Result<String> {
        self.patterns.lock().expect("lock").push(request.pattern);
        bail!("recording fabric: no reply")
    }
}

/// A temp patterns dir holding a copy of every repo pattern.
fn repo_patterns_copy() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tmp");
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("patterns");
    for entry in std::fs::read_dir(&repo).expect("read borg/patterns") {
        let path = entry.expect("entry").path();
        std::fs::copy(&path, tmp.path().join(path.file_name().expect("name"))).expect("copy");
    }
    tmp
}

#[test]
fn pattern_in_without_override_keeps_the_name() {
    assert_eq!(pattern_in(None, "judge-claims").expect("name"), "judge-claims");
}

#[test]
fn pattern_in_maps_to_the_dir_file_and_fails_when_missing() {
    let tmp = tempfile::tempdir().expect("tmp");
    std::fs::write(tmp.path().join("judge-claims.md"), "x").expect("pattern");
    let expected = tmp.path().join("judge-claims.md").to_string_lossy().to_string();
    assert_eq!(pattern_in(Some(tmp.path()), "judge-claims").expect("named"), expected);
    assert_eq!(pattern_in(Some(tmp.path()), "judge-claims.md").expect("file"), expected);
    let err = pattern_in(Some(tmp.path()), "distill-video").expect_err("missing");
    assert!(err.to_string().contains("distill-video"), "{err}");
}

#[test]
fn resolve_patterns_dir_is_absolute_and_rejects_missing_or_file() {
    let tmp = tempfile::tempdir().expect("tmp");
    let resolved = resolve_patterns_dir(tmp.path()).expect("dir");
    assert!(resolved.is_absolute());
    assert_eq!(resolved, std::fs::canonicalize(tmp.path()).expect("canon"));
    assert!(resolve_patterns_dir(&tmp.path().join("nope")).is_err());
    let file = tmp.path().join("f.md");
    std::fs::write(&file, "x").expect("file");
    assert!(resolve_patterns_dir(&file).is_err());
}

#[tokio::test]
async fn dispatcher_reads_every_distill_pattern_from_the_override_dir() {
    let tmp = repo_patterns_copy();
    let dir = resolve_patterns_dir(tmp.path()).expect("dir");
    let recorder = RecordingFabric::default();
    let config = crate::config::FabricConfig::default();
    let dispatcher = crate::stages::distill::dispatcher_with_caller(
        &config,
        PatternDirFabric {
            inner: recorder.clone(),
            dir: Some(dir.clone()),
        },
    );
    for kind in [
        DistillKind::Article,
        DistillKind::Repo,
        DistillKind::Video,
        DistillKind::Thread,
    ] {
        let _ = dispatcher
            .distill(
                kind,
                DistillInputs {
                    transcript: "a short source about ripgrep",
                    capture_note: Some("why"),
                    ..Default::default()
                },
            )
            .await;
    }
    let seen = recorder.patterns.lock().expect("lock").clone();
    assert!(!seen.is_empty(), "no fabric call reached the recorder");
    for pattern in &seen {
        assert!(
            Path::new(pattern).starts_with(&dir),
            "{pattern} not under {}",
            dir.display()
        );
    }
    for name in [
        "distill-article.md",
        "distill-repo.md",
        "distill-video.md",
        "distill-thread.md",
    ] {
        let want = dir.join(name).to_string_lossy().to_string();
        assert!(seen.contains(&want), "{want} not in {seen:?}");
    }
}

#[tokio::test]
async fn a_pattern_missing_from_the_override_dir_never_reaches_fabric() {
    let tmp = tempfile::tempdir().expect("empty dir");
    let recorder = RecordingFabric::default();
    let caller = PatternDirFabric {
        inner: recorder.clone(),
        dir: Some(tmp.path().to_path_buf()),
    };
    let err = caller
        .call(FabricRequest {
            pattern: "distill-video".to_string(),
            input: String::new(),
            model: String::new(),
            max_chars: 10,
            timeout_secs: 1,
        })
        .await
        .expect_err("missing pattern");
    assert!(err.to_string().contains("distill-video"), "{err}");
    assert!(recorder.patterns.lock().expect("lock").is_empty());
}

#[test]
fn judges_and_intent_writer_resolve_from_the_override_dir() {
    let tmp = repo_patterns_copy();
    let dir = resolve_patterns_dir(tmp.path()).expect("dir");
    let opts = crate::eval::EvalOpts {
        patterns_dir: Some(dir.clone()),
        ..crate::eval::EvalOpts::default()
    };
    let (judge, claim_judge) = crate::eval::fabric_judges(&opts).expect("judges");
    assert_eq!(judge.pattern, dir.join("judge-distillation.md").to_string_lossy());
    assert_eq!(claim_judge.pattern, dir.join("judge-claims.md").to_string_lossy());
    let writer = crate::eval::fabric_intent_writer(&opts).expect("writer");
    assert_eq!(writer.pattern, dir.join("synth-capture-intent.md").to_string_lossy());

    let (judge, _) = crate::eval::fabric_judges(&crate::eval::EvalOpts::default()).expect("default");
    assert_eq!(judge.pattern, "judge-distillation");
}
