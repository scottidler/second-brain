use super::*;

/// A work dir shaped like the real one: the source video, the extracted
/// frames, the manifests, and one materialized slide.
fn work_dir(root: &Path) -> PathBuf {
    let dir = root.join("work");
    std::fs::create_dir_all(dir.join("frames")).expect("mkdir frames");
    std::fs::create_dir_all(dir.join("slides")).expect("mkdir slides");
    std::fs::write(dir.join("vid.mp4"), b"video").expect("mp4");
    std::fs::write(dir.join("vid.f140.m4a"), b"audio").expect("m4a");
    std::fs::write(dir.join("frames/frame_0001.jpg"), b"f1").expect("f1");
    std::fs::write(dir.join("frames/frame_0002.jpg"), b"f2").expect("f2");
    std::fs::write(dir.join("frames.yml"), sidecar_yaml(&dir)).expect("frames.yml");
    std::fs::write(dir.join("slides.yml"), b"slides: []").expect("slides.yml");
    std::fs::write(dir.join("slides/slide-001.jpg"), b"s1").expect("slide");
    dir
}

/// A `frames.yml` as `extract_frames` writes it: absolute paths into `dir`.
fn sidecar_yaml(dir: &Path) -> String {
    let frames = (1..=2)
        .map(|index| crate::youtube::FrameRef {
            index,
            path: dir.join(format!("frames/frame_{index:04}.jpg")),
            timestamp_secs: f64::from(index - 1),
        })
        .collect();
    serde_yaml::to_string(&crate::youtube::FramesSidecar {
        video_duration_secs: 2.0,
        frame_budget: 2,
        effective_fps: 1.0,
        frames_extracted: 2,
        frames,
    })
    .expect("sidecar yaml")
}

fn preserved_sidecar(dest: &Path) -> crate::youtube::FramesSidecar {
    serde_yaml::from_str(&std::fs::read_to_string(dest.join("frames.yml")).expect("read")).expect("parse")
}

#[test]
fn a_guard_with_no_destination_removes_the_dir() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = work_dir(tmp.path());
    let report = PreserveReport::default();
    drop(WorkDirGuard::new(dir.clone(), None, report.clone()));
    assert!(!dir.exists());
    assert_eq!(report.failure(), None, "a guard that preserves nothing never reports");
}

#[test]
fn a_guard_with_a_destination_copies_everything_but_the_video_then_removes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = work_dir(tmp.path());
    std::os::unix::fs::symlink(dir.join("slides.yml"), dir.join("link.yml")).expect("symlink");
    let dest = tmp.path().join("scratch/frames");

    let report = PreserveReport::default();
    drop(WorkDirGuard::new(dir.clone(), Some(dest.clone()), report.clone()));

    assert!(!dir.exists(), "the work dir is removed after the copy");
    assert_eq!(report.failure(), None, "a clean preserve reports nothing");
    for kept in [
        "frames/frame_0001.jpg",
        "frames/frame_0002.jpg",
        "frames.yml",
        "slides.yml",
        "slides/slide-001.jpg",
    ] {
        assert!(dest.join(kept).is_file(), "{kept} preserved");
    }
    assert_eq!(std::fs::read(dest.join("frames/frame_0002.jpg")).expect("read"), b"f2");
    for dropped in ["vid.mp4", "vid.f140.m4a", "link.yml"] {
        assert!(!dest.join(dropped).exists(), "{dropped} not preserved");
    }
}

#[test]
fn retire_preserves_exactly_as_drop_does() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = work_dir(tmp.path());
    let dest = tmp.path().join("frames");
    let report = PreserveReport::default();
    let guard = WorkDirGuard::new(dir.clone(), Some(dest.clone()), report.clone());
    assert_eq!(guard.path(), dir.as_path());
    let live = report
        .failure()
        .expect("a live preserving guard is not a clean preserve");
    assert!(live.contains("never retired"), "{live}");
    guard.retire();
    assert!(!dir.exists());
    assert!(dest.join("frames/frame_0001.jpg").is_file());
    assert_eq!(report.failure(), None, "retired cleanly");
}

#[test]
fn a_missing_work_dir_preserves_nothing_and_does_not_panic() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path().join("frames");
    let report = PreserveReport::default();
    drop(WorkDirGuard::new(
        tmp.path().join("never-made"),
        Some(dest.clone()),
        report.clone(),
    ));
    assert!(!dest.exists());
    assert_eq!(report.failure(), None);
}

/// The preserved `frames.yml` names each frame relative to the preserved dir,
/// so it resolves after the /tmp work dir is gone.
#[test]
fn the_preserved_frames_yml_points_inside_the_preserved_dir() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = work_dir(tmp.path());
    let dest = tmp.path().join("scratch/frames");
    drop(WorkDirGuard::new(
        dir.clone(),
        Some(dest.clone()),
        PreserveReport::default(),
    ));

    let sidecar = preserved_sidecar(&dest);
    assert_eq!(sidecar.frames.len(), 2);
    for frame in &sidecar.frames {
        assert!(frame.path.is_relative(), "{}", frame.path.display());
        assert!(dest.join(&frame.path).is_file(), "{} resolves", frame.path.display());
    }
    assert_eq!(sidecar.frames[0].path, PathBuf::from("frames/frame_0001.jpg"));
}

/// A frame recorded outside the work dir cannot be made to resolve: the
/// preserve fails rather than leave a dangling path.
#[test]
fn a_frame_outside_the_work_dir_fails_the_preserve() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = work_dir(tmp.path());
    std::fs::write(dir.join("frames.yml"), sidecar_yaml(Path::new("/elsewhere"))).expect("frames.yml");
    let report = PreserveReport::default();
    drop(WorkDirGuard::new(
        dir.clone(),
        Some(tmp.path().join("frames")),
        report.clone(),
    ));
    let failure = report.failure().expect("must fail");
    assert!(failure.contains("outside the work dir"), "{failure}");
    assert!(!dir.exists(), "the work dir is still removed");
}

/// A failed copy is recorded for the pipeline to fail the run on, and the
/// /tmp work dir is still removed.
#[test]
fn a_failed_copy_is_reported_and_the_work_dir_is_still_removed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = work_dir(tmp.path());
    let dest = tmp.path().join("frames");
    std::fs::write(&dest, b"a file where the frames dir goes").expect("obstruct");
    let report = PreserveReport::default();

    drop(WorkDirGuard::new(dir.clone(), Some(dest.clone()), report.clone()));

    let failure = report.failure().expect("a failed copy must be reported");
    assert!(failure.starts_with("frames preserve failed:"), "{failure}");
    assert!(failure.contains(&dest.display().to_string()), "{failure}");
    assert!(!dir.exists(), "the work dir is removed regardless");
}

/// A failure recorded outside the guard (the `runs.yml` write) fails it too.
#[test]
fn a_recorded_runs_failure_is_reported() {
    let report = PreserveReport::default();
    report.record_failure("runs.yml write failed: disk full".to_string());
    let failure = report.failure().expect("recorded");
    assert!(failure.contains("runs.yml write failed: disk full"), "{failure}");
}

/// Only a preserving guard hands out its report, so `runs.yml` is written
/// exactly when the work dir will be kept.
#[test]
fn only_a_preserving_guard_hands_out_its_report() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let kept = WorkDirGuard::new(
        tmp.path().join("a"),
        Some(tmp.path().join("b")),
        PreserveReport::default(),
    );
    assert!(kept.preserving().is_some());
    let removed = WorkDirGuard::new(tmp.path().join("c"), None, PreserveReport::default());
    assert!(removed.preserving().is_none());
}

fn ocr_words(path: &Path, _: u64) -> Result<String> {
    Ok(format!("text of {}", path.file_name().unwrap().to_string_lossy()))
}

fn ocr_fails_on_second(path: &Path, _: u64) -> Result<String> {
    if path.ends_with("frame_0002.jpg") {
        eyre::bail!("tesseract timed out");
    }
    ocr_words(path, 0)
}

fn runs(work: &Path) -> (Vec<Run>, Vec<PathBuf>) {
    (
        vec![Run { start: 0.0, end: 4.0 }, Run { start: 4.0, end: 9.5 }],
        vec![work.join("frames/frame_0001.jpg"), work.join("frames/frame_0002.jpg")],
    )
}

#[test]
fn run_records_ocr_every_run_and_keep_failures_visible() {
    let work = PathBuf::from("/tmp/borg-youtube-frames/cl-1");
    let (windows, best) = runs(&work);
    let classes = vec![
        Ok(SlideClass {
            category: SlideCategory::TalkingHead,
            confidence: 0.8,
        }),
        Err(ClassifyError::Api(eyre::eyre!("vision 500"))),
    ];

    let records = run_records(&windows, &best, &classes, &work, ocr_fails_on_second, 5);

    assert_eq!(records.len(), 2, "one record per run");
    assert_eq!(records[0].best_frame, PathBuf::from("frames/frame_0001.jpg"));
    assert_eq!(records[0].category, Some(SlideCategory::TalkingHead));
    assert_eq!(records[0].confidence, Some(0.8));
    assert_eq!(records[0].ocr.as_deref(), Some("text of frame_0001.jpg"));
    assert_eq!(records[0].ocr_error, None);
    assert_eq!((records[1].start, records[1].end), (4.0, 9.5));
    assert_eq!(records[1].category, None);
    assert!(records[1].classify_error.as_deref().unwrap().contains("vision 500"));
    assert_eq!(records[1].ocr, None);
    assert!(records[1].ocr_error.as_deref().unwrap().contains("timed out"));
}

#[test]
fn runs_yml_carries_an_ocr_key_on_every_entry() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (windows, best) = runs(tmp.path());
    let classes = vec![
        Ok(SlideClass {
            category: SlideCategory::Chart,
            confidence: 0.9,
        }),
        Ok(SlideClass {
            category: SlideCategory::Code,
            confidence: 0.7,
        }),
    ];
    let records = run_records(&windows, &best, &classes, tmp.path(), ocr_fails_on_second, 5);

    let path = write_runs(&records, tmp.path()).expect("write");
    assert_eq!(path, tmp.path().join(RUNS_FILE));
    let parsed: Vec<serde_yaml::Mapping> =
        serde_yaml::from_str(&std::fs::read_to_string(&path).expect("read")).expect("yaml list");
    assert_eq!(parsed.len(), 2);
    for entry in &parsed {
        for key in ["start", "end", "best-frame", "category", "confidence", "ocr"] {
            assert!(entry.contains_key(key), "{key} in {entry:?}");
        }
    }
    assert_eq!(parsed[0]["category"], serde_yaml::Value::from("chart"));
    assert_eq!(
        parsed[1]["ocr"],
        serde_yaml::Value::Null,
        "a failed OCR is null, not absent"
    );
}
