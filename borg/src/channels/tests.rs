use super::*;
use tempfile::TempDir;

const FIXTURE: &[u8] = include_bytes!("../youtube/fixtures/dump-json-53xycpcgqwe.json");
const URL_A: &str = "https://www.youtube.com/watch?v=53XYcpCgQWE";
const URL_GONE: &str = "https://www.youtube.com/watch?v=GONEGONE123";

/// Fake yt-dlp: serves the dump-json fixture for known URLs, fails for the rest,
/// and records every URL it was asked about.
struct FakeFetcher {
    ok: Vec<&'static str>,
    calls: std::sync::Mutex<Vec<String>>,
}

#[async_trait]
impl ChannelFetcher for FakeFetcher {
    async fn fetch(&self, url: &str) -> Result<VideoMetadata> {
        self.calls.lock().expect("lock").push(url.to_string());
        if self.ok.contains(&url) {
            crate::youtube::parse_metadata(FIXTURE)
        } else {
            eyre::bail!("yt-dlp failed: Video unavailable")
        }
    }
}

fn fake(ok: Vec<&'static str>) -> FakeFetcher {
    FakeFetcher {
        ok,
        calls: std::sync::Mutex::new(Vec::new()),
    }
}

fn yt_note(source: &str, extra_fm: &str) -> String {
    format!(
        "---\ntitle: \"T\"\nsource: \"{source}\"\ntype: youtube\ncreator: \"Let's Get Rusty\"\n{extra_fm}tags:\n  - rust\n---\n\n## Body\ntext\n"
    )
}

fn vault_with(notes: &[(&str, String)]) -> (TempDir, Config) {
    let dir = TempDir::new().expect("tmp");
    for (name, content) in notes {
        std::fs::write(dir.path().join(name), content).expect("write");
    }
    let mut config = Config::default();
    config.vault.root_path = Some(dir.path().display().to_string());
    (dir, config)
}

#[tokio::test]
async fn dry_run_reports_candidates_and_writes_nothing() {
    let (dir, config) = vault_with(&[
        ("a.md", yt_note(URL_A, "")),
        ("b.md", yt_note(URL_GONE, "")),
        (
            "other.md",
            "---\ntitle: x\ntype: article\nsource: \"https://example.com\"\n---\nbody\n".to_string(),
        ),
    ]);
    let before = std::fs::read_to_string(dir.path().join("a.md")).expect("read");
    let f = fake(vec![URL_A]);
    let report = run_with(&config, &f, false).await.expect("run");
    assert_eq!(report.scanned, 2);
    assert_eq!(report.to_write.len(), 2);
    assert!(
        f.calls.lock().expect("lock").is_empty(),
        "dry run makes no network call"
    );
    assert_eq!(std::fs::read_to_string(dir.path().join("a.md")).expect("read"), before);
}

#[tokio::test]
async fn apply_writes_three_keys_and_second_run_finds_nothing() {
    let (dir, config) = vault_with(&[("a.md", yt_note(URL_A, ""))]);
    let f = fake(vec![URL_A]);
    let report = run_with(&config, &f, true).await.expect("run");
    assert_eq!(report.to_write.len(), 1);
    let written = std::fs::read_to_string(dir.path().join("a.md")).expect("read");
    assert!(written.contains("channel-id: UC"), "{written}");
    assert!(written.contains("channel-handle: '@letsgetrusty'"), "{written}");
    assert!(
        written.contains("channel-url: https://www.youtube.com/channel/"),
        "{written}"
    );
    let (fm, body) = vault::frontmatter::split_raw(&written).expect("frontmatter");
    serde_yaml::from_str::<serde_yaml::Value>(fm).expect("frontmatter stays valid yaml");
    assert!(body.contains("## Body"));
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .expect("ls")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(leftovers, vec!["a.md".to_string()], "no temp file lingers");

    let again = run_with(&config, &f, true).await.expect("rerun");
    assert_eq!(again.scanned, 1);
    assert_eq!(again.already_complete, 1);
    assert!(again.to_write.is_empty());
    assert_eq!(f.calls.lock().expect("lock").len(), 1, "second run makes no call");
}

#[tokio::test]
async fn existing_channel_key_is_never_overwritten() {
    let (dir, config) = vault_with(&[("a.md", yt_note(URL_A, "channel-handle: '@mine'\n"))]);
    let f = fake(vec![URL_A]);
    let report = run_with(&config, &f, true).await.expect("run");
    assert_eq!(report.to_write.len(), 1);
    let written = std::fs::read_to_string(dir.path().join("a.md")).expect("read");
    assert!(written.contains("channel-handle: '@mine'"), "{written}");
    assert!(!written.contains("@letsgetrusty"), "{written}");
    assert_eq!(written.matches("channel-handle:").count(), 1);
    assert!(written.contains("channel-id:"), "missing keys are still filled");
}

#[tokio::test]
async fn fetch_failure_is_counted_and_skipped() {
    let (dir, config) = vault_with(&[("a.md", yt_note(URL_GONE, "")), ("b.md", yt_note(URL_A, ""))]);
    let f = fake(vec![URL_A]);
    let report = run_with(&config, &f, true)
        .await
        .expect("loop continues past the failure");
    assert_eq!(report.unavailable.len(), 1);
    assert!(report.unavailable[0].1.contains("unavailable"));
    assert_eq!(report.to_write.len(), 1);
    assert!(report.failed.is_empty());
    let skipped = std::fs::read_to_string(dir.path().join("a.md")).expect("read");
    assert!(!skipped.contains("channel-"));
    assert!(
        std::fs::read_to_string(dir.path().join("b.md"))
            .expect("read")
            .contains("channel-handle:")
    );
}

#[tokio::test]
async fn candidates_are_keyed_on_the_note_video_url_not_the_creator() {
    let (_dir, config) = vault_with(&[("a.md", yt_note(URL_A, "")), ("b.md", yt_note(URL_GONE, ""))]);
    let f = fake(vec![URL_A, URL_GONE]);
    run_with(&config, &f, true).await.expect("run");
    let mut calls = f.calls.lock().expect("lock").clone();
    calls.sort();
    assert_eq!(
        calls,
        vec![URL_A.to_string(), URL_GONE.to_string()],
        "same creator, two URLs"
    );
}

#[test]
fn insert_missing_returns_none_when_nothing_to_add() {
    let meta = crate::youtube::parse_metadata(FIXTURE).expect("fixture");
    let full = yt_note(URL_A, "channel-id: x\nchannel-handle: y\nchannel-url: z\n");
    assert!(insert_missing(&full, &meta).is_none());
}
