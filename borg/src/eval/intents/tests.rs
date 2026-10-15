use super::*;
use std::cell::RefCell;

const MINIMAL_DISTILLED: &str = "summary: a summary\n\
claims:\n\
- text: a claim\n  anchor: null\n\
meta:\n  extractor: x\n  model: m\n  produced-at: \"2026-07-05T00:00:00Z\"\n";

fn write_fixture(root: &Path, kind: &str, slug: &str, source: &str) -> PathBuf {
    let dir = root.join(kind).join(slug);
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("source.md"), source).expect("source");
    std::fs::write(dir.join("distilled.yml"), MINIMAL_DISTILLED).expect("distilled");
    dir
}

/// Records every call and answers with a note derived from the source.
#[derive(Default)]
struct FakeWriter {
    calls: RefCell<Vec<(String, String)>>,
}

impl IntentWriter for FakeWriter {
    fn state_intent(&self, kind: &str, source: &str) -> Result<String> {
        self.calls.borrow_mut().push((kind.to_string(), source.to_string()));
        Ok(format!("want to remember {source}"))
    }
}

struct FailingWriter;

impl IntentWriter for FailingWriter {
    fn state_intent(&self, _kind: &str, _source: &str) -> Result<String> {
        bail!("could not find vendor")
    }
}

#[test]
fn synth_writes_url_kinds_only_and_never_overwrites() {
    let tmp = tempfile::tempdir().expect("tmp");
    let video = write_fixture(tmp.path(), "video", "v", "the video");
    let repo = write_fixture(tmp.path(), "repo", "r", "the repo");
    let image = write_fixture(tmp.path(), "image", "i", "the image");
    std::fs::write(repo.join(CAPTURE_NOTE_FILE), "kept as is\n").expect("existing");

    let writer = FakeWriter::default();
    let report = synth_intents(tmp.path(), &writer).expect("synth");

    assert_eq!(report.written, vec![video.join(CAPTURE_NOTE_FILE)]);
    assert_eq!(report.existing, vec!["repo/r".to_string()]);
    assert_eq!(report.skipped_kind, 1);
    assert_eq!(
        *writer.calls.borrow(),
        vec![("video".to_string(), "the video".to_string())]
    );
    assert_eq!(
        std::fs::read_to_string(video.join(CAPTURE_NOTE_FILE)).expect("read"),
        "want to remember the video\n"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join(CAPTURE_NOTE_FILE)).expect("read"),
        "kept as is\n"
    );
    assert!(!image.join(CAPTURE_NOTE_FILE).exists());

    // A second run writes nothing.
    let again = synth_intents(tmp.path(), &writer).expect("rerun");
    assert!(again.written.is_empty());
    assert_eq!(again.existing.len(), 2);
}

#[test]
fn synth_surfaces_the_writer_error_and_writes_no_file() {
    let tmp = tempfile::tempdir().expect("tmp");
    let video = write_fixture(tmp.path(), "video", "v", "the video");
    let err = synth_intents(tmp.path(), &FailingWriter).expect_err("must fail");
    let chain = format!("{err:#}");
    assert!(chain.contains("video/v"), "{chain}");
    assert!(chain.contains("could not find vendor"), "{chain}");
    assert!(!video.join(CAPTURE_NOTE_FILE).exists());
}

#[test]
fn clean_intent_strips_fences_quotes_and_whitespace() {
    assert_eq!(
        clean_intent("```\n\"Saving this for the\n  tokio section.\"\n```\n").expect("clean"),
        "Saving this for the tokio section."
    );
}

#[test]
fn clean_intent_rejects_empty_and_overlong_replies() {
    assert!(clean_intent("```\n```").is_err());
    assert!(clean_intent("  \"\" ").is_err());
    assert!(clean_intent(&"word ".repeat(MAX_INTENT_CHARS)).is_err());
}

#[test]
fn render_counts_each_bucket() {
    let report = SynthReport {
        written: vec![PathBuf::from("/f/video/v/capture-note.txt")],
        existing: vec!["repo/r".to_string()],
        skipped_kind: 3,
    };
    let text = report.render();
    assert!(text.contains("wrote /f/video/v/capture-note.txt\n"));
    assert!(text.contains("synth-intents: 1 written, 1 already present (kept), 3 fixtures outside"));
}
