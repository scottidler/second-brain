#![allow(clippy::unwrap_used)]
use super::*;
use crate::config::Config;
use tempfile::TempDir;

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
        &config,
        "harvest-test-missing-primary",
    )
    .await
    .expect_err("missing primary id must be a loud error, never a silent publish");

    assert!(format!("{err:#}").contains("not-present"));
}
