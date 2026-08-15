#![allow(clippy::unwrap_used)]
use super::*;
use crate::config::Config;
use tempfile::TempDir;

/// Point `XDG_DATA_HOME` at a throwaway directory for the duration of a test,
/// serialized against every other test that does the same
/// (`crate::harvest::TEST_XDG_LOCK` is the ONE shared lock; a per-file lock
/// would let these race `harvest::tests`'s live-run test over the same env
/// var). Every publish path this module drives now touches XDG-rooted state -
/// the receipts DB (prior-note resolution + the post-publish `note_path`
/// repair) and the success ledger - so without this the tests would read and
/// write the operator's REAL `~/.local/share/sb/borg/` state.
struct XdgSandbox {
    #[allow(dead_code)]
    lock: tokio::sync::MutexGuard<'static, ()>,
    #[allow(dead_code)]
    data_home: TempDir,
    prior: Option<String>,
}

impl XdgSandbox {
    async fn new() -> Self {
        let lock = crate::harvest::TEST_XDG_LOCK.lock().await;
        let data_home = TempDir::new().unwrap();
        let prior = std::env::var("XDG_DATA_HOME").ok();
        unsafe { std::env::set_var("XDG_DATA_HOME", data_home.path()) };
        Self { lock, data_home, prior }
    }
}

impl Drop for XdgSandbox {
    fn drop(&mut self) {
        match &self.prior {
            Some(v) => unsafe { std::env::set_var("XDG_DATA_HOME", v) },
            None => unsafe { std::env::remove_var("XDG_DATA_HOME") },
        }
    }
}

fn session_record(id: &str, created: &str, modified: &str, n_msgs: i64) -> SessionRecord {
    SessionRecord {
        session_id: id.to_string(),
        host: "desk".to_string(),
        scope: "work".to_string(),
        cwd: Some("/home/saidler/repos/tatari-tv/marquee".to_string()),
        project_dir: None,
        repo: Some("tatari-tv/marquee".to_string()),
        git_branch: Some("main".to_string()),
        created: Some(created.to_string()),
        modified: modified.to_string(),
        updated_at: None,
        duration_secs: Some(600),
        dormant: true,
        title: Some(format!("session {id} work")),
        first_prompt: Some("do the thing".to_string()),
        n_msgs,
        model: None,
        summary: None,
        tags: vec![],
        enrich_status: None,
        redaction_count: 0,
        transcript_path: None,
        staged_path: None,
        archived: false,
        repos_touched: None,
        files_touched: None,
        body: None,
        body_truncated: false,
        body_error: None,
    }
}

#[test]
fn build_session_metadata_puts_primary_id_first() {
    let members = vec![
        session_record("s1", "2026-07-01T00:00:00+00:00", "2026-07-01T01:00:00+00:00", 10),
        session_record("s2", "2026-07-01T01:05:00+00:00", "2026-07-01T02:00:00+00:00", 40),
    ];
    // s2 is the primary (most messages), created SECOND - session_ids must
    // still list it first per the distillers-crate SessionMetadata contract.
    let meta = build_session_metadata(&members, "s2", false);
    assert_eq!(meta.session_ids, vec!["s2".to_string(), "s1".to_string()]);
    assert_eq!(meta.msg_count, 50);
    assert_eq!(meta.repo.as_deref(), Some("tatari-tv/marquee"));
    assert_eq!(meta.date_start.as_deref(), Some("2026-07-01T00:00:00+00:00"));
    assert_eq!(meta.date_end.as_deref(), Some("2026-07-01T02:00:00+00:00"));
    assert!(!meta.body_truncated);
}

#[test]
fn build_session_metadata_repo_is_none_when_primary_has_no_anchor() {
    let mut member = session_record("s1", "2026-07-01T00:00:00+00:00", "2026-07-01T01:00:00+00:00", 10);
    member.repo = None;
    let meta = build_session_metadata(&[member], "s1", true);
    assert_eq!(meta.repo, None);
    assert!(meta.body_truncated);
}

#[test]
fn earliest_created_skips_unparseable_timestamps() {
    let mut bad = session_record("bad", "not-a-timestamp", "2026-07-01T01:00:00+00:00", 5);
    bad.created = Some("not-a-timestamp".to_string());
    let good = session_record("good", "2026-07-01T00:00:00+00:00", "2026-07-01T01:00:00+00:00", 5);
    let members = vec![bad, good];
    assert_eq!(earliest_created(&members).as_deref(), Some("2026-07-01T00:00:00+00:00"));
}

#[test]
fn earliest_created_skips_null_created() {
    // A present-null `created` (harvest-completion Phase 1 relaxation) is
    // skipped from the min/max, never panicking - the selection guard rejects
    // these upstream, so reaching here is a warn-and-skip backstop.
    let mut null_created = session_record("null", "2026-07-01T00:00:00+00:00", "2026-07-01T01:00:00+00:00", 5);
    null_created.created = None;
    let good = session_record("good", "2026-07-02T00:00:00+00:00", "2026-07-02T01:00:00+00:00", 5);
    let members = vec![null_created, good];
    assert_eq!(earliest_created(&members).as_deref(), Some("2026-07-02T00:00:00+00:00"));
}

#[test]
fn render_member_details_lists_every_member() {
    let members = vec![
        session_record("s1", "2026-07-01T00:00:00+00:00", "2026-07-01T01:00:00+00:00", 10),
        session_record("s2", "2026-07-01T01:05:00+00:00", "2026-07-01T02:00:00+00:00", 40),
    ];
    let rendered = render_member_details(&members);
    assert!(rendered.contains("## Session Details"));
    assert!(rendered.contains("clyde://s1"));
    assert!(rendered.contains("clyde://s2"));
    assert!(rendered.contains("tatari-tv/marquee"));
}

/// Build a minimal `Config` rooted at temp directories so the note lands
/// under a throwaway inbox and staging tree - no real vault/XDG state is
/// touched. `fabric.binary` names a binary guaranteed absent so distillation
/// gracefully falls back (degraded) rather than hanging on a real subprocess,
/// matching every other `distill_for_publish_*` handler's test posture.
fn test_config(vault_root: &std::path::Path, staging_root: &std::path::Path) -> Config {
    let mut config = Config::default();
    config.vault.root_path = Some(vault_root.to_string_lossy().to_string());
    config.staging.enabled = true;
    config.staging.root = staging_root.to_path_buf();
    config.fabric.binary = "borg-test-fabric-binary-does-not-exist".to_string();
    // `finalize_tags` caches the loaded canonical vocabulary in a process-wide
    // (not per-config) slot on first success - pointing at the REAL default
    // `~/.config/sb/canonical-tags.yml` would, on a machine that has one,
    // permanently poison that shared cache for every other test in this
    // binary with this dev box's real tag catalogue. Point at guaranteed-
    // absent paths instead: the load fails, `finalize_tags` no-ops for this
    // call, and the shared cache is never touched (see
    // `pipeline::tags::get_or_init_canonical`).
    config.tags.canonical_path = staging_root.join("no-such-canonical-tags.yml").display().to_string();
    config.tags.mapping_path = staging_root.join("no-such-tag-mapping.yml").display().to_string();
    config
}

/// End-to-end: fixture members -> distill (graceful fallback, no real
/// fabric) -> a rendered `inbox/` note carrying both `trace:` and
/// `source: clyde://<primary-id>` (Phase 5 success criterion).
#[tokio::test]
async fn process_session_inner_publishes_note_with_trace_and_source() {
    let _sandbox = XdgSandbox::new().await;
    let vault_dir = TempDir::new().unwrap();
    let staging_dir = TempDir::new().unwrap();
    let config = test_config(vault_dir.path(), staging_dir.path());

    let members = vec![
        session_record(
            "871f6428",
            "2026-07-02T04:51:21+00:00",
            "2026-07-02T06:08:39+00:00",
            486,
        ),
        session_record(
            "4ae69e3a",
            "2026-07-02T06:08:54+00:00",
            "2026-07-03T03:10:20+00:00",
            320,
        ),
    ];
    let body = "human: let's build the thing\nassistant: sure, here's a plan\n";

    let result = process_session_inner(
        body,
        &members,
        "871f6428",
        false,
        vec![],
        IngestMethod::Harvest,
        false,
        ResolveIntent::NewNote,
        None,
        &config,
        "harvest-test-trace",
    )
    .await
    .expect("process_session_inner should succeed even on distill fallback");

    assert!(matches!(result.status, IngestStatus::Completed));
    let note_path = result.note_path.expect("note_path set on success");
    let contents = std::fs::read_to_string(&note_path).expect("read published note");

    assert!(contents.contains("trace: harvest-test-trace"), "{contents}");
    assert!(contents.contains("source:"), "{contents}");
    assert!(contents.contains("clyde://871f6428"), "{contents}");
    assert!(contents.contains("type: session"), "{contents}");
    assert!(contents.contains("origin: generated"), "{contents}");
    assert!(contents.contains("status: unread"), "{contents}");
    assert!(contents.contains("repo:"), "{contents}");
    assert!(contents.contains("tatari-tv/marquee"), "{contents}");
    assert!(contents.contains("scope-work"), "{contents}");
    // Two members: the richer per-member footer should be present.
    assert!(contents.contains("## Session Details"), "{contents}");

    let inbox = vault_dir.path().join("inbox");
    assert!(std::path::Path::new(&note_path).starts_with(&inbox));
}

#[tokio::test]
async fn process_session_inner_tags_redacted_source_when_any_member_redacted() {
    let _sandbox = XdgSandbox::new().await;
    let vault_dir = TempDir::new().unwrap();
    let staging_dir = TempDir::new().unwrap();
    let config = test_config(vault_dir.path(), staging_dir.path());

    let mut member = session_record("only-one", "2026-07-02T04:51:21+00:00", "2026-07-02T06:08:39+00:00", 42);
    member.redaction_count = 3;

    let result = process_session_inner(
        "human: hi\nassistant: hello\n",
        &[member],
        "only-one",
        false,
        vec![],
        IngestMethod::Harvest,
        false,
        ResolveIntent::NewNote,
        None,
        &config,
        "harvest-test-redacted",
    )
    .await
    .expect("process_session_inner should succeed");

    let contents = std::fs::read_to_string(result.note_path.expect("note_path")).expect("read note");
    assert!(contents.contains("redacted-source"), "{contents}");
    // A single-member thread gets no member-details footer.
    assert!(!contents.contains("## Session Details"), "{contents}");
}

// ---- harvest-content-slug-naming: filename stem resolution ----

#[test]
fn harvest_slug_stem_prefers_content_slug() {
    // A clean lowercase-kebab slug is a sanitize fixed point and wins over the
    // generic title; no fallback.
    let (stem, fallback) = harvest_slug_stem(
        Some("slack-cli-idcache-groups-list-vs-string-bug"),
        "Review Slack thread",
    );
    assert_eq!(stem, "slack-cli-idcache-groups-list-vs-string-bug");
    assert!(!fallback);
}

#[test]
fn harvest_slug_stem_sanitizes_a_dirty_slug() {
    // Spaces/punctuation/case in a slug are sanitized to a safe stem, still no
    // fallback (the distiller DID emit a slug).
    let (stem, fallback) = harvest_slug_stem(Some("Has Spaces & Junk!"), "irrelevant title");
    assert_eq!(stem, "has-spaces-junk");
    assert!(!fallback);
}

#[test]
fn harvest_slug_stem_falls_back_to_title_when_slug_absent_or_blank() {
    // None and whitespace-only both fall back to the title-slug, flagged so the
    // caller WARNs.
    let (stem_none, fb_none) = harvest_slug_stem(None, "Review Slack Thread");
    assert_eq!(stem_none, "review-slack-thread");
    assert!(fb_none);

    let (stem_blank, fb_blank) = harvest_slug_stem(Some("   "), "Review Slack Thread");
    assert_eq!(stem_blank, "review-slack-thread");
    assert!(fb_blank);
}

#[test]
fn harvest_publish_path_uses_bare_slug_when_free() {
    let dir = TempDir::new().unwrap();
    let p = harvest_publish_path(dir.path(), "gha-uv-sync-review", "871f6428-c866-4c08", false);
    assert_eq!(p, dir.path().join("gha-uv-sync-review.md"));
}

#[test]
fn harvest_publish_path_collision_is_deterministic_session_suffix_never_dash_n() {
    let dir = TempDir::new().unwrap();
    // A different note already occupies the bare slug.
    std::fs::write(dir.path().join("gha-uv-sync-review.md"), b"other").unwrap();
    let p = harvest_publish_path(dir.path(), "gha-uv-sync-review", "871f6428-c866-4c08", false);
    // Deterministic session-keyed suffix, NOT the order-dependent `-2`.
    assert_eq!(p, dir.path().join("gha-uv-sync-review--871f6428.md"));
    assert!(!p.to_string_lossy().ends_with("-2.md"), "must never use the -N counter");
    // Same session + same collision -> same path every time (idempotent).
    let p2 = harvest_publish_path(dir.path(), "gha-uv-sync-review", "871f6428-c866-4c08", false);
    assert_eq!(p, p2);
}

#[test]
fn harvest_publish_path_force_overwrites_bare_slug_in_place() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("gha-uv-sync-review.md"), b"prior").unwrap();
    // force = a deliberate re-distill: reuse the bare-slug note, do not fork.
    let p = harvest_publish_path(dir.path(), "gha-uv-sync-review", "871f6428-c866-4c08", true);
    assert_eq!(p, dir.path().join("gha-uv-sync-review.md"));
}

/// End-to-end fallback: with no real fabric the distillation degrades and emits
/// no slug, so the note filename is the title-slug and `slug:` is persisted to
/// frontmatter matching that stem (harvest-content-slug-naming Phase 2).
#[tokio::test]
async fn process_session_inner_names_file_from_title_slug_on_distiller_fallback() {
    let _sandbox = XdgSandbox::new().await;
    let vault_dir = TempDir::new().unwrap();
    let staging_dir = TempDir::new().unwrap();
    let config = test_config(vault_dir.path(), staging_dir.path());

    // session_record sets title = "session <id> work"; sanitize -> that kebab.
    let members = vec![session_record(
        "abc123",
        "2026-07-02T04:51:21+00:00",
        "2026-07-02T06:08:39+00:00",
        42,
    )];

    let result = process_session_inner(
        "human: hi\nassistant: hello\n",
        &members,
        "abc123",
        false,
        vec![],
        IngestMethod::Harvest,
        false,
        ResolveIntent::NewNote,
        None,
        &config,
        "harvest-test-slug-fallback",
    )
    .await
    .expect("process_session_inner should succeed on distill fallback");

    let note_path = result.note_path.expect("note_path set on success");
    let stem = std::path::Path::new(&note_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("utf-8 stem");
    assert_eq!(stem, "session-abc123-work", "fallback filename is the title-slug");

    let contents = std::fs::read_to_string(&note_path).expect("read note");
    assert!(
        contents.contains("slug: session-abc123-work"),
        "the chosen stem is persisted to frontmatter: {contents}"
    );
}

#[tokio::test]
async fn process_session_inner_fails_loudly_when_primary_id_missing() {
    let _sandbox = XdgSandbox::new().await;
    let vault_dir = TempDir::new().unwrap();
    let staging_dir = TempDir::new().unwrap();
    let config = test_config(vault_dir.path(), staging_dir.path());
    let members = vec![session_record(
        "s1",
        "2026-07-01T00:00:00+00:00",
        "2026-07-01T01:00:00+00:00",
        10,
    )];

    let err = process_session_inner(
        "human: hi\n",
        &members,
        "not-present",
        false,
        vec![],
        IngestMethod::Harvest,
        false,
        ResolveIntent::NewNote,
        None,
        &config,
        "harvest-test-missing-primary",
    )
    .await
    .expect_err("missing primary id must be a loud error, never a silent publish");

    assert!(format!("{err:#}").contains("not-present"));
}

// ---- trace-keyed replace-in-place (2026-08-15 note-identity design, Phase 3) ----

const REPLACE_BODY: &str = "human: migrate ci.yml to the reusable workflow\nassistant: here is the plan\n";

/// One session publish through the real handler. `title` drives the distiller
/// FALLBACK slug (no real fabric in tests, so the distill degrades and the
/// filename comes from the title-slug) - which is how these tests inject a
/// DIFFERENT slug for the same trace, exactly the condition that forked 15
/// notes out of `hv-e5d240` in the live vault.
async fn publish_session(
    config: &Config,
    title: &str,
    primary_id: &str,
    trace_id: &str,
    intent: ResolveIntent,
    force: bool,
) -> Result<IngestResult> {
    publish_session_with_follows(config, title, primary_id, trace_id, intent, force, None).await
}

/// [`publish_session`] plus an explicit `follows_prior` - the follow-up
/// back-link source (Phase 4), threaded through exactly the way
/// `harvest::publish::publish_thread_inner` derives it from
/// `Reappearance::FollowUp { prior }`.
#[allow(clippy::too_many_arguments)]
async fn publish_session_with_follows(
    config: &Config,
    title: &str,
    primary_id: &str,
    trace_id: &str,
    intent: ResolveIntent,
    force: bool,
    follows_prior: Option<watermark::PublishedEntry>,
) -> Result<IngestResult> {
    let mut member = session_record(primary_id, "2026-07-02T04:51:21+00:00", "2026-07-02T06:08:39+00:00", 42);
    member.title = Some(title.to_string());
    process_session_inner(
        REPLACE_BODY,
        &[member],
        primary_id,
        false,
        vec![],
        IngestMethod::Harvest,
        force,
        intent,
        follows_prior,
        config,
        trace_id,
    )
    .await
}

/// Every `.md` file under `root`, recursively, sorted.
fn all_notes(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "md") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Splice extra frontmatter lines (each `\n`-terminated) into an existing
/// note - standing in for cortex's classify/quality writes and an operator
/// editing `status:` in Obsidian.
fn inject_frontmatter(path: &std::path::Path, extra: &str) {
    let text = std::fs::read_to_string(path).unwrap();
    let rest = text
        .strip_prefix("---\n")
        .expect("note starts with a frontmatter fence");
    let end = rest.find("\n---\n").expect("note has a closing frontmatter fence");
    let (fm, body) = rest.split_at(end);
    std::fs::write(path, format!("---\n{fm}\n{extra}{}", &body[1..])).unwrap();
}

fn frontmatter_of(path: &std::path::Path) -> serde_yaml::Mapping {
    let text = std::fs::read_to_string(path).unwrap();
    let (yaml, _body) = vault::frontmatter::split_raw(&text).expect("note has frontmatter");
    serde_yaml::from_str(yaml).expect("frontmatter parses")
}

/// Acceptance: "Replay the same trace three times -> exactly one note, same
/// path each time" AND "Filename unchanged when the model emits a different
/// slug; `slug:` equals the filename stem afterwards."
///
/// Each replay is handed a DIFFERENT title, so the fallback slug differs on
/// every pass. Before this phase that produced three files (the live vault's
/// `hv-e5d240` has 15 for exactly this reason).
#[tokio::test]
async fn replaying_one_trace_three_times_lands_exactly_one_note_at_one_path() {
    let _sandbox = XdgSandbox::new().await;
    let vault_dir = TempDir::new().unwrap();
    let staging_dir = TempDir::new().unwrap();
    let config = test_config(vault_dir.path(), staging_dir.path());
    let trace = "hv-e5d24011";

    let first = publish_session(
        &config,
        "Review CI Workflow",
        "8d6b6ef3",
        trace,
        ResolveIntent::NewNote,
        false,
    )
    .await
    .unwrap();
    let landed = std::path::PathBuf::from(first.note_path.expect("first publish lands a note"));
    assert_eq!(landed.file_stem().unwrap(), "review-ci-workflow");

    for title in ["Wholly Different Subject", "Third Slug Entirely"] {
        let replay = publish_session(&config, title, "8d6b6ef3", trace, ResolveIntent::Replay, true)
            .await
            .unwrap();
        assert_eq!(
            std::path::PathBuf::from(replay.note_path.expect("replay lands a note")),
            landed,
            "a replay of trace {trace} must write the note the trace already produced, \
             not a file named after the fresh slug"
        );
    }

    let notes = all_notes(vault_dir.path());
    assert_eq!(notes.len(), 1, "one trace, one note: {notes:?}");
    assert_eq!(notes[0], landed);

    let fm = frontmatter_of(&landed);
    assert_eq!(
        fm.get("slug").and_then(|v| v.as_str()),
        Some("review-ci-workflow"),
        "slug: names the file it actually lives in, not the slug this distill pass produced"
    );
    assert!(fm.get("harvest-body-hash").and_then(|v| v.as_str()).is_some());
}

/// Acceptance: "`cortex-classified`, `cortex-quality*`, and a user-set
/// `status: read` survive all three replays."
///
/// `status: read` is deliberately OFF-schema (`vault::schema::Status` has no
/// `read`) - the value is carried forward verbatim rather than parsed, so an
/// operator value borg does not model still survives.
#[tokio::test]
async fn replace_preserves_cortex_fields_and_a_user_set_status() {
    let _sandbox = XdgSandbox::new().await;
    let vault_dir = TempDir::new().unwrap();
    let staging_dir = TempDir::new().unwrap();
    let config = test_config(vault_dir.path(), staging_dir.path());
    let trace = "hv-c0de1234";

    let first = publish_session(
        &config,
        "Review CI Workflow",
        "8d6b6ef3",
        trace,
        ResolveIntent::NewNote,
        false,
    )
    .await
    .unwrap();
    let landed = std::path::PathBuf::from(first.note_path.unwrap());
    assert_eq!(
        frontmatter_of(&landed).get("status").and_then(|v| v.as_str()),
        Some("unread"),
        "a fresh publish still establishes status: unread"
    );

    inject_frontmatter(
        &landed,
        "cortex-classified: true\ncortex-quality-score: 87\ncortex-quality-issues: [no-outbound-links]\n\
         domain: engineering\n",
    );
    // The operator marks it read in Obsidian (replacing borg's `unread`).
    let text = std::fs::read_to_string(&landed)
        .unwrap()
        .replace("status: unread", "status: read");
    std::fs::write(&landed, text).unwrap();

    for title in ["Different Slug One", "Different Slug Two", "Different Slug Three"] {
        publish_session(&config, title, "8d6b6ef3", trace, ResolveIntent::Replay, true)
            .await
            .unwrap();
    }

    let fm = frontmatter_of(&landed);
    assert_eq!(fm.get("cortex-classified").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(fm.get("cortex-quality-score").and_then(|v| v.as_u64()), Some(87));
    assert!(fm.contains_key("cortex-quality-issues"), "{fm:?}");
    assert_eq!(fm.get("domain").and_then(|v| v.as_str()), Some("engineering"));
    assert_eq!(
        fm.get("status").and_then(|v| v.as_str()),
        Some("read"),
        "a replay must never reset a status the operator set"
    );
    // Borg-owned keys are still rewritten by the publish.
    assert_eq!(fm.get("trace").and_then(|v| v.as_str()), Some(trace));
    assert_eq!(fm.get("type").and_then(|v| v.as_str()), Some("session"));
    assert_eq!(fm.get("origin").and_then(|v| v.as_str()), Some("generated"));
    assert_eq!(all_notes(vault_dir.path()).len(), 1);
}

/// Acceptance: "A note moved `inbox/` -> `notes/` is resolved and its receipts
/// row updated."
///
/// The index reset stands in for the next process: cortex promotes notes
/// BETWEEN borg runs, and the receipts row is what carries the repair across
/// (`update_note_path`, which `mark_succeeded`'s `WHERE status='received'`
/// could never do).
#[tokio::test]
async fn a_cortex_moved_note_is_resolved_and_its_receipts_row_repaired() {
    let _sandbox = XdgSandbox::new().await;
    let vault_dir = TempDir::new().unwrap();
    let staging_dir = TempDir::new().unwrap();
    let config = test_config(vault_dir.path(), staging_dir.path());
    let trace = "hv-m0ved001";

    let conn = receipts::open_default().unwrap();
    receipts::record_received(
        &conn,
        trace,
        vault::schema::Method::Harvest,
        vault::receipts::ReceiptKind::Session,
        "clyde://8d6b6ef3",
    )
    .unwrap();

    let first = publish_session(
        &config,
        "Review CI Workflow",
        "8d6b6ef3",
        trace,
        ResolveIntent::NewNote,
        false,
    )
    .await
    .unwrap();
    let inbox_path = std::path::PathBuf::from(first.note_path.unwrap());
    assert!(inbox_path.starts_with(vault_dir.path().join("inbox")));

    // cortex promotes it.
    let notes_dir = vault_dir.path().join("notes");
    std::fs::create_dir_all(&notes_dir).unwrap();
    let moved = notes_dir.join(inbox_path.file_name().unwrap());
    std::fs::rename(&inbox_path, &moved).unwrap();
    crate::harvest::identity::reset_index_cache_for_tests(vault_dir.path());

    let replay = publish_session(
        &config,
        "Some Other Slug",
        "8d6b6ef3",
        trace,
        ResolveIntent::Replay,
        true,
    )
    .await
    .unwrap();
    assert_eq!(
        std::path::PathBuf::from(replay.note_path.unwrap()),
        moved,
        "the replay must follow the note cortex moved, not re-mint one in inbox/"
    );
    assert_eq!(all_notes(vault_dir.path()).len(), 1);

    let row = receipts::get(&conn, trace).unwrap().expect("receipts row");
    assert_eq!(
        row.note_path.as_deref(),
        Some(moved.to_string_lossy().as_ref()),
        "the receipts row is repaired to the note's current path"
    );
}

/// Acceptance: "A `FollowUp` ... produces a second, distinct note." A genuine
/// follow-up carries its own trace and must never replace the note it follows
/// (notes are immutable once published).
#[tokio::test]
async fn a_follow_up_publishes_a_second_distinct_note() {
    let _sandbox = XdgSandbox::new().await;
    let vault_dir = TempDir::new().unwrap();
    let staging_dir = TempDir::new().unwrap();
    let config = test_config(vault_dir.path(), staging_dir.path());

    let first = publish_session(
        &config,
        "Review CI Workflow",
        "8d6b6ef3",
        "hv-f1r57000",
        ResolveIntent::NewNote,
        false,
    )
    .await
    .unwrap();
    let first_path = std::path::PathBuf::from(first.note_path.unwrap());

    let follow_up = publish_session(
        &config,
        "Review CI Workflow",
        "8d6b6ef3",
        "hv-f0110w00",
        ResolveIntent::FollowUp,
        false,
    )
    .await
    .unwrap();
    let follow_up_path = std::path::PathBuf::from(follow_up.note_path.unwrap());

    assert_ne!(first_path, follow_up_path);
    assert_eq!(all_notes(vault_dir.path()).len(), 2);
}

/// Acceptance: "a `--force` re-harvest ... produces a second, distinct note."
///
/// This is the dangerous one: `classify_reappearance` returns `FollowUp` on
/// `--force` BEFORE consulting the body hash, so an unchanged `--force`
/// re-harvest presents the same `source:` AND the same `harvest-body-hash:` as
/// the landed note - every term of the confirmation guard matches. Only the
/// intent gate keeps it from overwriting.
#[tokio::test]
async fn a_force_reharvest_of_an_unchanged_session_still_forks_a_new_note() {
    let _sandbox = XdgSandbox::new().await;
    let vault_dir = TempDir::new().unwrap();
    let staging_dir = TempDir::new().unwrap();
    let config = test_config(vault_dir.path(), staging_dir.path());

    let first = publish_session(
        &config,
        "Review CI Workflow",
        "8d6b6ef3",
        "hv-f0rce001",
        ResolveIntent::NewNote,
        false,
    )
    .await
    .unwrap();
    let first_path = std::path::PathBuf::from(first.note_path.unwrap());

    // Identical body (same hash), identical source, identical title/slug -
    // only the trace differs, exactly as a `--force` re-harvest presents.
    let forced = publish_session(
        &config,
        "Review CI Workflow",
        "8d6b6ef3",
        "hv-f0rce002",
        ResolveIntent::FollowUp,
        false,
    )
    .await
    .unwrap();
    let forced_path = std::path::PathBuf::from(forced.note_path.unwrap());

    assert_ne!(
        first_path, forced_path,
        "--force must fork, never overwrite: the prior note stays as published"
    );
    assert_eq!(all_notes(vault_dir.path()).len(), 2);
}

/// Acceptance: "A trace whose note was deleted republishes cleanly as new."
/// The re-stat before return is what catches this: the index still holds the
/// self-inserted path from the first publish.
#[tokio::test]
async fn a_trace_whose_note_was_deleted_republishes_as_new() {
    let _sandbox = XdgSandbox::new().await;
    let vault_dir = TempDir::new().unwrap();
    let staging_dir = TempDir::new().unwrap();
    let config = test_config(vault_dir.path(), staging_dir.path());
    let trace = "hv-de1e7ed0";

    let first = publish_session(
        &config,
        "Review CI Workflow",
        "8d6b6ef3",
        trace,
        ResolveIntent::NewNote,
        false,
    )
    .await
    .unwrap();
    let first_path = std::path::PathBuf::from(first.note_path.unwrap());
    std::fs::remove_file(&first_path).unwrap();

    let replay = publish_session(
        &config,
        "Review CI Workflow",
        "8d6b6ef3",
        trace,
        ResolveIntent::Replay,
        true,
    )
    .await
    .unwrap();
    assert!(matches!(replay.status, IngestStatus::Completed));
    let notes = all_notes(vault_dir.path());
    assert_eq!(notes.len(), 1, "republished cleanly, exactly once: {notes:?}");
    assert_eq!(std::path::PathBuf::from(replay.note_path.unwrap()), first_path);
}

/// Design doc, Concurrency and failure modes: "Resolver DB error fails the
/// publish CLOSED, with the trace id and the SQLite error in the message; the
/// note is not written and the receipts row is left for a later replay."
///
/// The DB is broken by putting a DIRECTORY where the receipts file belongs, so
/// the open fails the way a corrupt/unopenable DB would.
#[tokio::test]
async fn a_broken_receipts_db_fails_the_publish_closed() {
    let _sandbox = XdgSandbox::new().await;
    std::fs::create_dir_all(vault::receipts::receipts_db_path().unwrap()).unwrap();

    let vault_dir = TempDir::new().unwrap();
    let staging_dir = TempDir::new().unwrap();
    let config = test_config(vault_dir.path(), staging_dir.path());

    let err = publish_session(
        &config,
        "Review CI Workflow",
        "8d6b6ef3",
        "hv-br0ken01",
        ResolveIntent::NewNote,
        false,
    )
    .await
    .expect_err("a resolver DB error must fail the publish, never publish blind");

    let rendered = format!("{err:#}");
    assert!(rendered.contains("hv-br0ken01"), "error names the trace: {rendered}");
    assert!(rendered.contains("fail-closed"), "{rendered}");
    assert!(
        all_notes(vault_dir.path()).is_empty(),
        "no note is written when resolution fails"
    );
}

// ---- follow-up back-link (2026-08-15 note-identity design, Phase 4) ----

/// Acceptance: "A follow-up note carries `follows:` pointing at the prior
/// note's CURRENT path, including when cortex has moved it."
///
/// The prior note is published to `inbox/`, then moved to `notes/` (cortex's
/// promotion) BEFORE the follow-up runs - `follows_prior.note_path` still
/// names the stale `inbox/` location, exactly what a real
/// `PublishedEntry.note_path` would hold. The follow-up must resolve through
/// `identity::resolve_prior_note` (keyed on the prior entry's OWN `trace`) to
/// find the moved file, never trust `note_path` raw.
#[tokio::test]
async fn a_follow_up_back_links_to_the_prior_notes_current_path_even_after_a_cortex_move() {
    let _sandbox = XdgSandbox::new().await;
    let vault_dir = TempDir::new().unwrap();
    let staging_dir = TempDir::new().unwrap();
    let config = test_config(vault_dir.path(), staging_dir.path());
    let prior_trace = "hv-pr10r001";

    let first = publish_session(
        &config,
        "Review CI Workflow",
        "8d6b6ef3",
        prior_trace,
        ResolveIntent::NewNote,
        false,
    )
    .await
    .unwrap();
    let inbox_path = std::path::PathBuf::from(first.note_path.unwrap());
    let prior_stem = inbox_path.file_stem().unwrap().to_str().unwrap().to_string();

    // cortex promotes it, same as `a_cortex_moved_note_is_resolved_and_its_receipts_row_repaired`.
    let notes_dir = vault_dir.path().join("notes");
    std::fs::create_dir_all(&notes_dir).unwrap();
    let moved = notes_dir.join(inbox_path.file_name().unwrap());
    std::fs::rename(&inbox_path, &moved).unwrap();
    crate::harvest::identity::reset_index_cache_for_tests(vault_dir.path());

    let prior_entry = watermark::PublishedEntry {
        note_path: inbox_path.to_string_lossy().to_string(), // stale - never used raw
        n_msgs: 42,
        body_hash: watermark::body_hash(REPLACE_BODY),
        trace: Some(prior_trace.to_string()),
    };
    let follow_up = publish_session_with_follows(
        &config,
        "Follow-up: more CI work",
        "8d6b6ef3",
        "hv-f0110w02",
        ResolveIntent::FollowUp,
        false,
        Some(prior_entry),
    )
    .await
    .unwrap();
    let follow_up_path = std::path::PathBuf::from(follow_up.note_path.unwrap());
    assert_ne!(follow_up_path, moved, "a follow-up is a distinct note, never a replace");

    let fm = frontmatter_of(&follow_up_path);
    assert_eq!(
        fm.get("follows").and_then(|v| v.as_str()),
        Some(prior_stem.as_str()),
        "follows: must name the prior note's CURRENT (post-move) stem, not the stale note_path: {fm:?}"
    );
    let body = std::fs::read_to_string(&follow_up_path).unwrap();
    assert!(
        body.contains(&format!("[[{prior_stem}]]")),
        "the body must carry the back-link wikilink: {body}"
    );
}

/// Acceptance: "An unresolvable prior note omits `follows:` and WARNs; it
/// never blocks the publish." Two unresolvable shapes in one test: no `trace`
/// on the prior entry (pre-Phase-2 watermark row) and a `trace` that never
/// resolves to any note.
#[tokio::test]
async fn an_unresolvable_prior_note_omits_follows_and_never_blocks_the_publish() {
    let _sandbox = XdgSandbox::new().await;
    let vault_dir = TempDir::new().unwrap();
    let staging_dir = TempDir::new().unwrap();
    let config = test_config(vault_dir.path(), staging_dir.path());

    let no_trace_entry = watermark::PublishedEntry {
        note_path: "inbox/some-old-note.md".to_string(),
        n_msgs: 10,
        body_hash: watermark::body_hash(REPLACE_BODY),
        trace: None,
    };
    let result = publish_session_with_follows(
        &config,
        "Follow-up with no prior trace",
        "8d6b6ef3",
        "hv-nopr10r1",
        ResolveIntent::FollowUp,
        false,
        Some(no_trace_entry),
    )
    .await
    .expect("an unresolvable prior note must never block the publish");
    assert!(matches!(result.status, IngestStatus::Completed));
    let fm = frontmatter_of(std::path::Path::new(&result.note_path.unwrap()));
    assert!(!fm.contains_key("follows"), "no prior trace -> no follows: key: {fm:?}");

    let unknown_trace_entry = watermark::PublishedEntry {
        note_path: "inbox/some-other-old-note.md".to_string(),
        n_msgs: 10,
        body_hash: watermark::body_hash(REPLACE_BODY),
        trace: Some("hv-never000".to_string()),
    };
    let result2 = publish_session_with_follows(
        &config,
        "Follow-up with unresolvable prior trace",
        "8d6b6ef3",
        "hv-nopr10r2",
        ResolveIntent::FollowUp,
        false,
        Some(unknown_trace_entry),
    )
    .await
    .expect("an unresolvable prior trace must never block the publish");
    assert!(matches!(result2.status, IngestStatus::Completed));
    let fm2 = frontmatter_of(std::path::Path::new(&result2.note_path.unwrap()));
    assert!(
        !fm2.contains_key("follows"),
        "prior trace resolves to nothing -> no follows: key: {fm2:?}"
    );
}

/// Acceptance (Phase 4, "confirm ... that `follows:` survives a replace"):
/// replaying a follow-up note re-emits BOTH the frontmatter key and the body
/// wikilink, even though the replay itself carries no `follows_prior` (a
/// stage-2 replay has no `ThreadDecision` to derive one from - see
/// `replay.rs`). The value must be carried forward off the note being
/// replaced, per the Data Model rule that the frontmatter key is the DURABLE
/// carrier and the body link is re-derived from it on every render.
#[tokio::test]
async fn follows_survives_a_replace() {
    let _sandbox = XdgSandbox::new().await;
    let vault_dir = TempDir::new().unwrap();
    let staging_dir = TempDir::new().unwrap();
    let config = test_config(vault_dir.path(), staging_dir.path());
    let prior_trace = "hv-pr10r002";
    let follow_trace = "hv-f0110w03";

    let first = publish_session(
        &config,
        "Review CI Workflow",
        "8d6b6ef3",
        prior_trace,
        ResolveIntent::NewNote,
        false,
    )
    .await
    .unwrap();
    let prior_path = std::path::PathBuf::from(first.note_path.unwrap());
    let prior_stem = prior_path.file_stem().unwrap().to_str().unwrap().to_string();

    let prior_entry = watermark::PublishedEntry {
        note_path: prior_path.to_string_lossy().to_string(),
        n_msgs: 42,
        body_hash: watermark::body_hash(REPLACE_BODY),
        trace: Some(prior_trace.to_string()),
    };
    let follow_up = publish_session_with_follows(
        &config,
        "Follow-up: more CI work",
        "8d6b6ef3",
        follow_trace,
        ResolveIntent::FollowUp,
        false,
        Some(prior_entry),
    )
    .await
    .unwrap();
    let follow_up_path = std::path::PathBuf::from(follow_up.note_path.unwrap());
    assert_eq!(
        frontmatter_of(&follow_up_path).get("follows").and_then(|v| v.as_str()),
        Some(prior_stem.as_str())
    );

    // Replay the follow-up note itself - a DIFFERENT fallback slug each time
    // (mirrors `replaying_one_trace_three_times_lands_exactly_one_note_at_one_path`),
    // and critically `follows_prior: None` (exactly what `replay.rs` passes).
    for title in ["Some Other Follow-up Title", "Yet Another Title"] {
        publish_session_with_follows(
            &config,
            title,
            "8d6b6ef3",
            follow_trace,
            ResolveIntent::Replay,
            true,
            None,
        )
        .await
        .unwrap();
    }

    assert_eq!(all_notes(vault_dir.path()).len(), 2, "prior + follow-up, no forks");
    let fm = frontmatter_of(&follow_up_path);
    assert_eq!(
        fm.get("follows").and_then(|v| v.as_str()),
        Some(prior_stem.as_str()),
        "follows: must survive a replay that carries no fresh follows_prior: {fm:?}"
    );
    let body = std::fs::read_to_string(&follow_up_path).unwrap();
    assert!(
        body.contains(&format!("[[{prior_stem}]]")),
        "the body wikilink must be re-emitted from follows: on every render: {body}"
    );
}

/// Cross-cutting acceptance: the borg-owned key policy is DERIVED from the
/// writer. `markdown::tests::render_note_keys_matches_the_writer` pins
/// `RENDER_NOTE_KEYS` to what `render_note` actually emits; this pins the
/// session policy to that constant plus this handler's own additions, so a new
/// writer key cannot silently become a carried-forward (i.e. never-updated)
/// key on a replace.
#[test]
fn borg_owned_key_policy_matches_the_writer() {
    let owned = borg_owned_keys();
    for key in markdown::RENDER_NOTE_KEYS {
        assert!(
            owned.contains(key),
            "render_note emits {key:?} but the session replace policy does not own it - \
             a replace would carry a stale value forward instead of rewriting it"
        );
    }
    for key in SESSION_OWNED_KEYS.iter().chain(DISTILLER_OWNED_KEYS) {
        assert!(owned.contains(key), "{key:?} missing from the owned set");
    }
    assert_eq!(
        owned.len(),
        markdown::RENDER_NOTE_KEYS.len() + SESSION_OWNED_KEYS.len() + DISTILLER_OWNED_KEYS.len(),
        "the three key sources must not overlap - a key owned twice hides which writer owns it"
    );
    // `status` is the one writer key a REPLACE hands back to the user
    // (design doc: "a deliberate ownership change"), so it must be in the
    // owned set AND excluded explicitly at merge time.
    assert!(owned.contains(STATUS_KEY));
}
