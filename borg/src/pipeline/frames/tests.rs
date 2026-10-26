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
    std::fs::write(dir.join("frames.yml"), b"frames: []").expect("frames.yml");
    std::fs::write(dir.join("slides.yml"), b"slides: []").expect("slides.yml");
    std::fs::write(dir.join("slides/slide-001.jpg"), b"s1").expect("slide");
    dir
}

#[test]
fn a_guard_with_no_destination_removes_the_dir() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = work_dir(tmp.path());
    drop(WorkDirGuard::new(dir.clone(), None));
    assert!(!dir.exists());
}

#[test]
fn a_guard_with_a_destination_copies_everything_but_the_video_then_removes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = work_dir(tmp.path());
    std::os::unix::fs::symlink(dir.join("slides.yml"), dir.join("link.yml")).expect("symlink");
    let dest = tmp.path().join("scratch/frames");

    drop(WorkDirGuard::new(dir.clone(), Some(dest.clone())));

    assert!(!dir.exists(), "the work dir is removed after the copy");
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
    let guard = WorkDirGuard::new(dir.clone(), Some(dest.clone()));
    assert_eq!(guard.path(), dir.as_path());
    guard.retire();
    assert!(!dir.exists());
    assert!(dest.join("frames/frame_0001.jpg").is_file());
}

#[test]
fn a_missing_work_dir_preserves_nothing_and_does_not_panic() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path().join("frames");
    drop(WorkDirGuard::new(tmp.path().join("never-made"), Some(dest.clone())));
    assert!(!dest.exists());
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
