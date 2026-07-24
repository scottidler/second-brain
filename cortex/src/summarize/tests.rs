use super::*;
use crate::config::{BackfillConfig, Config, StateConfig, VaultConfig};
use crate::opts::SummarizeOpts;
use distillers::{Dispatcher, FakeFabric};
use std::sync::Arc;
use tempfile::TempDir;

fn opts_default() -> SummarizeOpts {
    SummarizeOpts {
        backfill: true,
        since: None,
        domain: None,
        extractor: None,
        dry_run: false,
        resume: true,
    }
}

/// Minimal vault fixture with no pre-seeded notes; tests write the exact
/// notes they need so kind inference is deterministic.
struct MiniVault {
    tmp: TempDir,
}

impl MiniVault {
    fn new() -> Self {
        Self {
            tmp: tempfile::tempdir().expect("tempdir"),
        }
    }

    fn root(&self) -> &Path {
        self.tmp.path()
    }

    fn add(&self, relative: &str, content: &str) {
        let abs = self.root().join(relative);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(&abs, content).expect("write note");
    }

    fn read(&self, relative: &str) -> String {
        std::fs::read_to_string(self.root().join(relative)).expect("read note")
    }

    fn config(&self) -> Config {
        Config {
            vault: VaultConfig {
                root_path: Some(self.root().to_string_lossy().into_owned()),
                ignore: vec![".cortex".to_string()],
                exclude: Vec::new(),
                include: Vec::new(),
            },
            state: StateConfig {
                cache_dir: ".cortex".to_string(),
            },
            backfill: BackfillConfig::default(),
            ..Config::default()
        }
    }
}

fn note(fm: &str, body: &str) -> String {
    // Default fixtures to `origin: assisted` so they survive the
    // ingested-notes-only filter in `filter_notes`. Tests that need an
    // authored note can pass `origin: <other>` in the frontmatter
    // explicitly; an existing `origin:` line is honored as-is.
    let fm_with_origin = if fm.contains("origin:") {
        fm.to_string()
    } else {
        format!("{fm}origin: assisted\n")
    };
    format!("---\n{fm_with_origin}---\n{body}")
}

fn fake_with_response(pattern: &str, yaml: &str) -> Arc<FakeFabric> {
    let fake = Arc::new(FakeFabric::new());
    fake.set_response(pattern, yaml);
    fake
}

fn dispatcher_for(fake: Arc<FakeFabric>) -> Dispatcher<Arc<FakeFabric>> {
    Dispatcher::new(fake, ArticleConfig::default())
}

#[test]
fn parse_since_accepts_days_weeks_months() {
    // parse_since calls Utc::now() internally; subtracting from a later
    // Utc::now() here can truncate by a microsecond, so accept N-1..=N+1
    // days of slack rather than a tight equality.
    fn within_days(actual: chrono::Duration, expected_days: i64) -> bool {
        let mins = actual.num_minutes();
        let expected_mins = expected_days * 24 * 60;
        (mins - expected_mins).abs() <= 5
    }
    let now = Utc::now();
    let d30 = parse_since("30d").expect("days");
    assert!(within_days(now - d30, 30), "30d off: {:?}", now - d30);
    let w2 = parse_since("2w").expect("weeks");
    assert!(within_days(now - w2, 14), "2w off: {:?}", now - w2);
    let mo3 = parse_since("3mo").expect("months");
    assert!(within_days(now - mo3, 90), "3mo off: {:?}", now - mo3);
}

#[test]
fn parse_since_rejects_garbage() {
    assert!(parse_since("nope").is_none());
    assert!(parse_since("d").is_none());
    assert!(parse_since("30y").is_none());
    assert!(parse_since("").is_none());
}

#[test]
fn infer_distill_kind_from_explicit_note_type() {
    let fm = Frontmatter {
        note_type: Some("article".to_string()),
        ..Frontmatter::default()
    };
    let n = Note {
        path: PathBuf::from("a.md"),
        frontmatter: fm,
        body: String::new(),
        raw: String::new(),
    };
    assert_eq!(infer_distill_kind(&n), Some(DistillKind::Article));
}

#[test]
fn infer_distill_kind_falls_back_to_source_url() {
    let fm = Frontmatter {
        source: Some("https://github.com/scottidler/x".to_string()),
        ..Frontmatter::default()
    };
    let n = Note {
        path: PathBuf::from("a.md"),
        frontmatter: fm,
        body: String::new(),
        raw: String::new(),
    };
    assert_eq!(infer_distill_kind(&n), Some(DistillKind::Repo));
}

#[test]
fn infer_distill_kind_returns_none_for_system_kinds() {
    let fm = Frontmatter {
        note_type: Some("system".to_string()),
        ..Frontmatter::default()
    };
    let n = Note {
        path: PathBuf::from("a.md"),
        frontmatter: fm,
        body: String::new(),
        raw: String::new(),
    };
    assert!(infer_distill_kind(&n).is_none());
}

#[test]
fn is_already_distilled_reads_extra_flag() {
    let mut extra = std::collections::HashMap::new();
    extra.insert("distilled".to_string(), serde_yaml::Value::Bool(true));
    let fm = Frontmatter {
        extra,
        ..Frontmatter::default()
    };
    assert!(is_already_distilled(&fm));

    let mut extra = std::collections::HashMap::new();
    extra.insert("distilled".to_string(), serde_yaml::Value::Bool(false));
    let fm = Frontmatter {
        extra,
        ..Frontmatter::default()
    };
    assert!(!is_already_distilled(&fm));

    let blank = Frontmatter::default();
    assert!(!is_already_distilled(&blank));
}

#[test]
fn note_date_at_or_after_keeps_notes_without_a_date() {
    let cutoff = Utc::now() - Duration::days(30);
    let n = Note {
        path: PathBuf::from("a.md"),
        frontmatter: Frontmatter::default(),
        body: String::new(),
        raw: String::new(),
    };
    assert!(note_date_at_or_after(&n, cutoff));
}

#[test]
fn note_date_at_or_after_filters_old_notes() {
    let cutoff = Utc::now() - Duration::days(30);
    let fm = Frontmatter {
        date: Some("2020-01-01".to_string()),
        ..Frontmatter::default()
    };
    let n = Note {
        path: PathBuf::from("a.md"),
        frontmatter: fm,
        body: String::new(),
        raw: String::new(),
    };
    assert!(!note_date_at_or_after(&n, cutoff));
}

#[tokio::test]
async fn backfill_filters_to_ingested_notes_only() {
    // Only borg-ingested notes (origin: assisted) enter the candidate pool.
    // Authored variants (`authored` / `human`) are excluded - daily journals,
    // MOC pages, system views, CLAUDE.md, home.md, etc. must never get
    // rewritten by backfill, even if they happen to carry a recognised
    // `type:` field. See [[feedback_ingested_only]].
    let v = MiniVault::new();
    v.add(
        "ingested.md",
        &note(
            "title: A\ntype: article\nsource: https://example.com/x\norigin: assisted\n",
            "Ingested article prose.\n",
        ),
    );
    v.add(
        "authored.md",
        &note(
            // CLAUDE.md shape: looks like a video but is hand-written.
            "title: Notes\ntype: youtube\norigin: authored\n",
            "Hand-written.\n",
        ),
    );
    v.add(
        "journal.md",
        &note(
            "title: 2025-02-14\ntype: daily\norigin: human\n",
            "Daily journal entry.\n",
        ),
    );

    let dispatcher = dispatcher_for(fake_with_response(
        "distill-article",
        "summary: \"S\"\nclaims: []\ntags: []\nlinks: []\n",
    ));
    let cfg = v.config();
    let opts = opts_default();
    let summary = backfill_with_dispatcher(v.root(), &cfg, &opts, dispatcher)
        .await
        .expect("run");

    // Only the ingested note is a candidate; the authored + human ones never
    // appear in the candidate count (not even as skips).
    assert_eq!(summary.attempted, 1, "only assisted notes enter the pool");
    assert_eq!(summary.distilled, 1);
    assert_eq!(summary.skipped, 0);
    // Authored notes are byte-identical to what we wrote - no rewrite.
    assert!(v.read("authored.md").contains("Hand-written."));
    assert!(v.read("journal.md").contains("Daily journal entry."));
}

#[tokio::test]
async fn backfill_dry_run_writes_no_files() {
    let v = MiniVault::new();
    v.add(
        "article.md",
        &note(
            "title: A\ntype: article\nsource: https://example.com/x\n",
            "Legacy prose summary about the article.\n",
        ),
    );

    let dispatcher = dispatcher_for(fake_with_response(
        "distill-article",
        "summary: \"S\"\nclaims: []\ntags: []\nlinks: []\n",
    ));
    let cfg = v.config();
    let mut opts = opts_default();
    opts.dry_run = true;

    let summary = backfill_with_dispatcher(v.root(), &cfg, &opts, dispatcher)
        .await
        .expect("run");
    assert_eq!(summary.attempted, 1);
    assert_eq!(summary.distilled, 0);
    assert!(v.read("article.md").contains("Legacy prose summary"));
}

#[tokio::test]
async fn backfill_rewrites_article_note_with_structured_sections() {
    let v = MiniVault::new();
    v.add(
        "article.md",
        &note(
            "title: Raft\ntype: article\nsource: https://example.com/raft\n",
            "Legacy prose summary about consensus.\n",
        ),
    );

    let dispatcher = dispatcher_for(fake_with_response(
        "distill-article",
        "summary: \"A short article.\"\nclaims:\n  - text: \"Raft simplifies replication.\"\n    anchor: null\ntags: []\nlinks: []\n",
    ));
    let cfg = v.config();

    let summary = backfill_with_dispatcher(v.root(), &cfg, &opts_default(), dispatcher)
        .await
        .expect("run");
    assert_eq!(summary.attempted, 1);
    assert_eq!(summary.distilled, 1);
    assert_eq!(summary.failed, 0);

    let raw = v.read("article.md");
    assert!(raw.contains("## Summary"), "expected Summary section:\n{raw}");
    assert!(raw.contains("Raft simplifies replication."));
    assert!(raw.contains("distilled: true"));
    assert!(raw.contains("distilled-extractor: distill-article-v1"));
    // Phase 7: articles now preserve their fetched markdown in-note under
    // `## Transcript` (both live ingest and backfill), matching the
    // video/voicenote precedent this backfill path already followed. On
    // backfill the "transcript" input is the legacy note body, so the prior
    // prose survives verbatim under `## Transcript` rather than being
    // discarded. (Was `!raw.contains(...)` pre-Phase-7.)
    assert!(raw.contains("## Transcript"), "expected Transcript section:\n{raw}");
    assert!(
        raw.contains("Legacy prose summary about consensus."),
        "legacy article body must be preserved verbatim under ## Transcript:\n{raw}"
    );
}

#[tokio::test]
async fn backfill_skips_notes_already_distilled() {
    let v = MiniVault::new();
    v.add(
        "already.md",
        &note(
            "title: x\ntype: article\nsource: https://example.com/\ndistilled: true\n",
            "## Summary\n\nAlready structured.\n",
        ),
    );

    let dispatcher = dispatcher_for(fake_with_response(
        "distill-article",
        "summary: \"S\"\nclaims: []\ntags: []\nlinks: []\n",
    ));
    let cfg = v.config();
    let summary = backfill_with_dispatcher(v.root(), &cfg, &opts_default(), dispatcher)
        .await
        .expect("run");
    assert_eq!(summary.attempted, 1);
    assert_eq!(summary.distilled, 0);
    assert_eq!(summary.skipped, 1);
}

#[tokio::test]
async fn backfill_extractor_override_forces_redistill() {
    let v = MiniVault::new();
    v.add(
        "already.md",
        &note(
            "title: x\ntype: article\nsource: https://example.com/\ndistilled: true\n",
            "## Summary\n\nOld structured.\n",
        ),
    );

    let dispatcher = dispatcher_for(fake_with_response(
        "distill-article",
        "summary: \"Refreshed.\"\nclaims: []\ntags: []\nlinks: []\n",
    ));
    let cfg = v.config();
    let mut opts = opts_default();
    opts.extractor = Some("distill-article-v2".to_string());

    let summary = backfill_with_dispatcher(v.root(), &cfg, &opts, dispatcher)
        .await
        .expect("run");
    assert_eq!(summary.distilled, 1);
    assert_eq!(summary.skipped, 0);
    let raw = v.read("already.md");
    assert!(raw.contains("Refreshed."), "{raw}");
}

#[tokio::test]
async fn backfill_filters_by_domain_frontmatter() {
    let v = MiniVault::new();
    v.add(
        "rust.md",
        &note(
            "title: r\ntype: article\nsource: https://example.com/r\ndomain: technical\n",
            "rust prose.\n",
        ),
    );
    v.add(
        "news.md",
        &note(
            "title: n\ntype: article\nsource: https://example.com/n\ndomain: leisure\n",
            "news prose.\n",
        ),
    );

    let dispatcher = dispatcher_for(fake_with_response(
        "distill-article",
        "summary: \"S\"\nclaims: []\ntags: []\nlinks: []\n",
    ));
    let cfg = v.config();
    let mut opts = opts_default();
    opts.domain = Some("technical".to_string());

    let summary = backfill_with_dispatcher(v.root(), &cfg, &opts, dispatcher)
        .await
        .expect("run");
    assert_eq!(summary.distilled, 1);
    assert!(v.read("news.md").contains("news prose"));
}

#[tokio::test]
async fn backfill_filters_by_since_date() {
    let v = MiniVault::new();
    v.add(
        "fresh.md",
        &note(
            &format!(
                "title: f\ntype: article\nsource: https://example.com/f\ndate: {}\n",
                Utc::now().format("%Y-%m-%d")
            ),
            "fresh prose.\n",
        ),
    );
    v.add(
        "stale.md",
        &note(
            "title: s\ntype: article\nsource: https://example.com/s\ndate: 2020-01-01\n",
            "stale prose.\n",
        ),
    );

    let dispatcher = dispatcher_for(fake_with_response(
        "distill-article",
        "summary: \"S\"\nclaims: []\ntags: []\nlinks: []\n",
    ));
    let cfg = v.config();
    let mut opts = opts_default();
    opts.since = Some("30d".to_string());

    let summary = backfill_with_dispatcher(v.root(), &cfg, &opts, dispatcher)
        .await
        .expect("run");
    assert_eq!(summary.distilled, 1);
    assert!(v.read("stale.md").contains("stale prose"));
}

#[tokio::test]
async fn backfill_writes_checkpoint_after_each_completed_note() {
    let v = MiniVault::new();
    v.add(
        "one.md",
        &note(
            "title: one\ntype: article\nsource: https://example.com/1\n",
            "prose one.\n",
        ),
    );

    let dispatcher = dispatcher_for(fake_with_response(
        "distill-article",
        "summary: \"S\"\nclaims: []\ntags: []\nlinks: []\n",
    ));
    let cfg = v.config();

    let summary = backfill_with_dispatcher(v.root(), &cfg, &opts_default(), dispatcher)
        .await
        .expect("run");
    assert_eq!(summary.distilled, 1);

    let cp = checkpoint_path(v.root(), &cfg);
    assert!(cp.exists(), "checkpoint must be persisted at {}", cp.display());
    let completed = load_checkpoint(&cp);
    assert!(
        completed.contains(&PathBuf::from("one.md")),
        "completed set must contain the distilled note, got {completed:?}"
    );
}

#[tokio::test]
async fn backfill_resume_skips_through_checkpoint() {
    let v = MiniVault::new();
    v.add(
        "a.md",
        &note("title: a\ntype: article\nsource: https://example.com/a\n", "a prose.\n"),
    );
    v.add(
        "b.md",
        &note("title: b\ntype: article\nsource: https://example.com/b\n", "b prose.\n"),
    );

    let cfg = v.config();
    let cp = checkpoint_path(v.root(), &cfg);
    save_checkpoint(&cp, &PathBuf::from("a.md")).expect("seed");

    let dispatcher = dispatcher_for(fake_with_response(
        "distill-article",
        "summary: \"S\"\nclaims: []\ntags: []\nlinks: []\n",
    ));
    let summary = backfill_with_dispatcher(v.root(), &cfg, &opts_default(), dispatcher)
        .await
        .expect("run");
    assert_eq!(summary.attempted, 1);
    assert!(v.read("a.md").contains("a prose"));
    assert!(v.read("b.md").contains("## Summary"));
}

#[tokio::test]
async fn backfill_resume_false_ignores_checkpoint() {
    let v = MiniVault::new();
    v.add(
        "a.md",
        &note("title: a\ntype: article\nsource: https://example.com/a\n", "a prose.\n"),
    );
    let cfg = v.config();
    let cp = checkpoint_path(v.root(), &cfg);
    save_checkpoint(&cp, &PathBuf::from("zzz-future.md")).expect("seed");

    let dispatcher = dispatcher_for(fake_with_response(
        "distill-article",
        "summary: \"S\"\nclaims: []\ntags: []\nlinks: []\n",
    ));
    let mut opts = opts_default();
    opts.resume = false;
    let summary = backfill_with_dispatcher(v.root(), &cfg, &opts, dispatcher)
        .await
        .expect("run");
    assert_eq!(summary.distilled, 1);
}

#[tokio::test]
async fn backfill_continues_past_per_note_skips() {
    let v = MiniVault::new();
    v.add(
        "good.md",
        &note(
            "title: g\ntype: article\nsource: https://example.com/g\n",
            "good prose.\n",
        ),
    );
    v.add("system.md", &note("title: sys\ntype: system\n", "system note.\n"));

    let dispatcher = dispatcher_for(fake_with_response(
        "distill-article",
        "summary: \"S\"\nclaims: []\ntags: []\nlinks: []\n",
    ));
    let summary = backfill_with_dispatcher(v.root(), &v.config(), &opts_default(), dispatcher)
        .await
        .expect("run");
    assert_eq!(summary.distilled, 1);
    assert_eq!(summary.skipped, 1);
    assert_eq!(summary.failed, 0);
}

#[tokio::test]
async fn backfill_requires_backfill_flag() {
    let v = MiniVault::new();
    let cfg = v.config();
    let mut opts = opts_default();
    opts.backfill = false;
    let err = backfill(v.root(), &cfg, &opts)
        .await
        .expect_err("must require --backfill");
    let msg = format!("{err}");
    assert!(msg.contains("--backfill"), "error must mention the flag: {msg}");
}

#[test]
fn rewrite_note_file_is_atomic_and_merges_frontmatter() {
    let v = MiniVault::new();
    v.add(
        "x.md",
        "---\ntitle: x\ntype: article\nsource: https://example.com/\n---\noriginal.\n",
    );

    let path = v.root().join("x.md");
    let parsed = crate::vault::parse_note(v.root(), &path).expect("parse");

    let distilled = Distilled {
        slug: None,
        summary: "Rewritten.".to_string(),
        tldr: None,
        enumeration: None,
        key_ideas: Vec::new(),
        claims: Vec::new(),
        tags: Vec::new(),
        links: Vec::new(),
        kind_specific: None,
        meta: ::vault::distilled::DistilledMeta {
            extractor: "distill-article-v1".to_string(),
            model: "test".to_string(),
            input_tokens: 0,
            output_tokens: 0,
            produced_at: "2026-05-16T00:00:00Z".to_string(),
            validation: ::vault::distilled::ValidationMeta::default(),
        },
        transcript: None,
    };

    rewrite_note_file(&path, &parsed.frontmatter, &distilled).expect("rewrite");
    let raw = v.read("x.md");
    assert!(raw.contains("title: x"));
    assert!(raw.contains("source: https://example.com/"));
    assert!(raw.contains("distilled: true"));
    assert!(raw.contains("## Summary"));
    assert!(raw.contains("Rewritten."));
    let tmp = path.with_extension("md.tmp");
    assert!(!tmp.exists(), "atomic write left a .tmp file: {}", tmp.display());
}

#[test]
fn backfill_render_keeps_transcript_section() {
    // Render call site 6 (cortex/src/summarize.rs backfill) policy: backfill
    // ALWAYS emits `## Transcript`. It feeds the ENTIRE legacy note body in as
    // the transcript input and re-renders it; dropping the section here would
    // destroy the legacy body (including the April baseline this design
    // protects). This is the load-bearing reason RenderOptions exists.
    let v = MiniVault::new();
    v.add(
        "y.md",
        "---\ntitle: y\ntype: youtube\nsource: https://youtu.be/abc\n---\nold body.\n",
    );
    let path = v.root().join("y.md");
    let parsed = crate::vault::parse_note(v.root(), &path).expect("parse");

    let distilled = Distilled {
        summary: "Backfilled summary.".to_string(),
        transcript: Some("The legacy note body being re-rendered as the transcript.".to_string()),
        meta: ::vault::distilled::DistilledMeta {
            extractor: "distill-video-v1".to_string(),
            model: "test".to_string(),
            input_tokens: 0,
            output_tokens: 0,
            produced_at: "2026-05-16T00:00:00Z".to_string(),
            validation: ::vault::distilled::ValidationMeta::default(),
        },
        ..Default::default()
    };

    rewrite_note_file(&path, &parsed.frontmatter, &distilled).expect("rewrite");
    let raw = v.read("y.md");
    assert!(
        raw.contains("## Transcript"),
        "backfill MUST keep the ## Transcript section or it destroys the legacy body:\n{raw}"
    );
    assert!(raw.contains("The legacy note body being re-rendered as the transcript."));
}
