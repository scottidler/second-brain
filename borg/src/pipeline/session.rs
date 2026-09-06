//! `ContentKind::Session` handler (harvest-clyde-sessions design, Phase 5).
//!
//! Mirrors `process_text`'s shape: a thin timing/error wrapper
//! (`process_session`) around the real work (`process_session_inner`), which
//! distills the harvested thread, renders the note, and publishes it via the
//! shared atomic-publish path. Unlike every other content kind, the input
//! here was already selected/clustered/fetched upstream by
//! `harvest::publish::publish_thread` - this handler's job is distill +
//! render + publish, not fetch.

use super::*;
use crate::harvest::contract::SessionRecord;
use crate::harvest::identity::{self, ResolveIntent};
use crate::harvest::watermark;
use chrono::{DateTime, FixedOffset};
use distillers::{SessionConfig, SessionMetadata};
use std::collections::{BTreeMap, HashSet};

/// Length of the primary-session-id prefix used to disambiguate a harvest
/// filename collision (harvest-content-slug-naming Phase 3). Clyde ids are
/// UUIDs; the first 8 hex chars are ample to separate two same-slug sessions.
const SESSION_SLUG_SUFFIX_LEN: usize = 8;

/// Resolve a harvest session note's filename stem (harvest-content-slug-naming,
/// 2026-07-24). The distiller's content-derived slug names the note's real
/// subject; the generic clyde title is only the fallback when the distiller
/// omitted a slug. Returns `(stem, used_title_fallback)` so the caller can WARN
/// on the fallback. Both branches pass through `hygiene::note_filename` for
/// filesystem safety and the empty-slug fallback, so the returned stem is
/// always a safe, non-empty filename base.
pub(crate) fn harvest_slug_stem(slug: Option<&str>, title: &str, trace_id: &str) -> (String, bool) {
    match slug.map(str::trim).filter(|s| !s.is_empty()) {
        Some(slug) => (hygiene::note_filename(slug, trace_id), false),
        None => (hygiene::note_filename(title, trace_id), true),
    }
}

/// Resolve the harvest note path DETERMINISTICALLY (harvest-content-slug-naming
/// Phase 3). The shared `atomic::resolve_publish_path` disambiguates collisions
/// with an order-dependent `-N` counter, which is nondeterministic across
/// re-harvests (a re-run can renumber notes, breaking the watermark idempotency
/// contract). A harvest collision on the content-slug is instead disambiguated
/// with a suffix derived from the PRIMARY SESSION ID, so the same session always
/// resolves to the same filename regardless of publish order. `force` (a
/// deliberate re-distill) overwrites the bare-slug note in place.
///
/// This names a NEW note only. A trace that already has a landed note never
/// reaches here: `process_session_inner` resolves that note through
/// `harvest::identity::resolve_prior_note` and short-circuits to its current
/// path (design doc `2026-08-15-harvest-note-identity-trace-keyed-replace.md`,
/// which is also why `force` is no longer what makes a replay land in place).
///
/// The residual "which of two same-slug sessions gets the bare slug" question is
/// deliberately NOT resolved here: cortex's association sweep merges or
/// cross-links same-base-slug notes by similarity (`cortex.yml` threshold). The
/// filename is display/addressing only; the load-bearing identity anchor stays
/// the session id in frontmatter and the receipts DB, never the filename.
fn harvest_publish_path(dir: &std::path::Path, slug_stem: &str, primary_id: &str, force: bool) -> std::path::PathBuf {
    let base = dir.join(format!("{slug_stem}.md"));
    if force || !base.exists() {
        return base;
    }
    let short: String = primary_id.chars().take(SESSION_SLUG_SUFFIX_LEN).collect();
    dir.join(format!("{slug_stem}--{short}.md"))
}

/// Frontmatter keys this handler adds on top of [`markdown::RENDER_NOTE_KEYS`]
/// (design doc: Data Model, "plus session additions"). `follows` (Phase 4) is
/// borg-owned like `slug:`: a replace REWRITES it from a fresh derivation
/// (either the current publish's own follow-up prior, or - on a plain replay -
/// the value carried off the note being replaced), rather than leaving the
/// generic carry-forward loop touch it. See [`FOLLOWS_KEY`] /
/// [`resolve_follows_stem`].
const SESSION_OWNED_KEYS: &[&str] = &["repo", "trace-expires", "slug", "harvest-body-hash", "follows"];

/// Frontmatter keys `distillers::render` contributes for a session note
/// (`distillers/src/render.rs`, the `KindPayload::Session` arm plus the two
/// unconditional keys). Borg-owned: a replace re-derives them from the fresh
/// distill pass.
const DISTILLER_OWNED_KEYS: &[&str] = &[
    "distilled",
    "distilled-extractor",
    "cortex-session-msg-count",
    "cortex-session-ids",
];

/// `status:` is written by [`markdown::render_note`] and so is nominally
/// borg-owned, but on a REPLACE its value is user state (design doc: Data
/// Model, "`status:` is a deliberate ownership change ... Phase 3 reads the
/// existing note's `status:` and feeds it back so a replay does not reset a
/// note the user marked `read`"). It is therefore the one writer key excluded
/// from the drop-list when merging a prior note.
const STATUS_KEY: &str = "status";

/// `follows:` frontmatter key (design doc: Data Model, Phase 4). Holds a bare
/// filename STEM, no extension, no directory - the same convention
/// `superseded-by:` already uses (`cortex::association::tombstone_content`).
/// A bare stem is deliberate: Obsidian resolves a `[[stem]]` wikilink by
/// filename across the WHOLE vault, not by path, so it keeps working even
/// after cortex moves the target between directories without this handler
/// re-resolving it on every later replay. See [`resolve_follows_stem`] for how
/// the value is derived and [`render_follows_link`] for the body wikilink it
/// is re-emitted as on every render.
const FOLLOWS_KEY: &str = "follows";

/// The complete borg-owned frontmatter key set: everything
/// [`markdown::render_note`] emits from its own fields, plus this handler's
/// session additions, plus the session distiller's additions. On a replace
/// these are rewritten from the fresh publish; EVERY other key (`domain`,
/// `cortex-classified*`, `cortex-quality*`, `superseded-by`, user keys) is
/// carried forward verbatim.
///
/// Derived from the writer rather than hand-listed, per the design doc's
/// governing rule - `borg_owned_key_policy_matches_the_writer` fails if
/// `render_note` gains a key this policy has not accounted for.
pub(crate) fn borg_owned_keys() -> HashSet<&'static str> {
    markdown::RENDER_NOTE_KEYS
        .iter()
        .chain(SESSION_OWNED_KEYS)
        .chain(DISTILLER_OWNED_KEYS)
        .copied()
        .collect()
}

/// What a replace carries off the note it is about to overwrite: every
/// non-borg-owned frontmatter key, verbatim, plus the prior `status:` value.
struct PriorFrontmatter {
    /// The prior `status:` value, RAW (not parsed through
    /// `vault::schema::Status`) so an off-schema operator value survives
    /// byte-for-byte instead of being silently reset to `unread`.
    status: Option<serde_yaml::Value>,
    /// The prior `follows:` value (a bare filename stem), if the note being
    /// replaced carried one. `resolve_follows_stem`'s fallback source on a
    /// plain replay (no fresh follow-up prior for THIS publish) - the value
    /// is carried forward unchanged rather than re-resolved, since a bare
    /// stem already survives a cortex move by construction (see
    /// [`FOLLOWS_KEY`]).
    follows: Option<String>,
    /// Non-borg-owned keys, verbatim.
    carried: BTreeMap<String, serde_yaml::Value>,
}

/// Read the frontmatter of the note being replaced and split it into the
/// carried-forward keys plus the preserved `status:`.
///
/// Fails CLOSED on an unreadable or unparseable note: the alternative is to
/// overwrite it with a note that has silently lost its cortex classification
/// and quality fields. The resolver parsed this same file moments earlier, so
/// reaching either error means the file changed underneath us.
fn read_prior_frontmatter(path: &Path) -> Result<PriorFrontmatter> {
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("read the note being replaced: {}", path.display()))?;
    let Some((yaml, _body)) = vault::frontmatter::split_raw(&raw) else {
        log::warn!(
            "process_session_inner: note being replaced has no frontmatter block ({}) - nothing to carry forward",
            path.display()
        );
        return Ok(PriorFrontmatter {
            status: None,
            follows: None,
            carried: BTreeMap::new(),
        });
    };
    let map: serde_yaml::Mapping = serde_yaml::from_str(yaml)
        .with_context(|| format!("parse frontmatter of the note being replaced: {}", path.display()))?;

    let owned = borg_owned_keys();
    let mut status = None;
    let mut follows = None;
    let mut carried = BTreeMap::new();
    for (key, value) in map {
        let Some(key) = key.as_str() else {
            log::warn!(
                "process_session_inner: non-string frontmatter key in {} - dropped on replace",
                path.display()
            );
            continue;
        };
        if key == STATUS_KEY {
            status = Some(value);
            continue;
        }
        if key == FOLLOWS_KEY {
            // A non-string `follows:` (never written by this handler) is
            // dropped rather than propagated - `resolve_follows_stem`'s
            // fallback source is a bare stem or nothing.
            follows = value.as_str().map(str::to_string);
            continue;
        }
        if owned.contains(key) {
            continue;
        }
        carried.insert(key.to_string(), value);
    }
    log::debug!(
        "process_session_inner: replacing {} - carrying {} non-borg key(s) forward, prior status={:?}, prior follows={:?}",
        path.display(),
        carried.len(),
        status,
        follows
    );
    Ok(PriorFrontmatter {
        status,
        follows,
        carried,
    })
}

/// Resolve the note this publish should replace, failing the publish CLOSED on
/// a receipts DB error (design doc: Concurrency and failure modes - "Resolver
/// DB error fails the publish CLOSED, with the trace id and the SQLite error
/// in the message; the note is not written and the receipts row is left for a
/// later replay"). Called BEFORE the distill pass so a broken DB costs no LLM
/// work.
fn resolve_prior_note(
    vault_root: &Path,
    trace_id: &str,
    source_url: &str,
    body_hash: &str,
    intent: ResolveIntent,
) -> Result<Option<PathBuf>> {
    let closed = || {
        format!(
            "[{trace_id}] session publish aborted (fail-closed): prior-note resolution failed, \
             so the note was NOT written and the receipts row is left for a later replay"
        )
    };
    let conn = receipts::open_default().with_context(closed)?;
    identity::resolve_prior_note(&conn, vault_root, trace_id, source_url, body_hash, intent).with_context(closed)
}

/// Resolve the CURRENT filename stem of the note a genuine follow-up
/// continues (design doc: Data Model, `follows:` - "the prior note's path is
/// subject to the same cortex-move staleness ... resolved through
/// `resolve_prior_note` using `PublishedEntry.trace`, never used raw").
///
/// Uses `ResolveIntent::Replay`, not `NewNote`/`FollowUp`: this is a DIFFERENT
/// lookup than the current publish's own resolution above - `prior.trace` is
/// already known (Phase 2 populates it), so only the receipts-fast-path +
/// vault-index steps (with tombstone-follow) apply, which is exactly what
/// `Replay` names ("the trace is authoritative, steps 1-2 only"). Step 3's
/// crash-recovery fallback exists to find a note when NO trace is known yet;
/// it does not apply here and `NewNote`/`FollowUp` would either pull in that
/// irrelevant branch or (for `FollowUp`) refuse to resolve at all.
///
/// Never fails the publish (design doc Phase 4 acceptance: "an unresolvable
/// prior note omits `follows:` and WARNs; it never blocks the publish") - a
/// missing trace, a DB error, or a resolution miss all WARN and return `None`
/// rather than propagating.
fn resolve_follows_stem(
    vault_root: &Path,
    trace_id: &str,
    source_url: &str,
    prior: &watermark::PublishedEntry,
) -> Option<String> {
    let Some(prior_trace) = prior.trace.as_deref() else {
        log::warn!(
            "[{trace_id}] follow-up back-link: prior published entry has no trace (pre-Phase-2 watermark row) \
             - omitting follows:"
        );
        return None;
    };
    let conn = match receipts::open_default() {
        Ok(conn) => conn,
        Err(e) => {
            log::warn!("[{trace_id}] follow-up back-link: receipts open failed: {e:#} - omitting follows:");
            return None;
        }
    };
    match identity::resolve_prior_note(
        &conn,
        vault_root,
        prior_trace,
        source_url,
        &prior.body_hash,
        ResolveIntent::Replay,
    ) {
        Ok(Some(path)) => match path.file_stem().and_then(|s| s.to_str()) {
            Some(stem) => Some(stem.to_string()),
            None => {
                log::warn!(
                    "[{trace_id}] follow-up back-link: resolved path {} has no filename stem - omitting follows:",
                    path.display()
                );
                None
            }
        },
        Ok(None) => {
            log::warn!(
                "[{trace_id}] follow-up back-link: prior trace {prior_trace} did not resolve to a live note \
                 - omitting follows:"
            );
            None
        }
        Err(e) => {
            log::warn!(
                "[{trace_id}] follow-up back-link: resolution of prior trace {prior_trace} failed: {e:#} \
                 - omitting follows:"
            );
            None
        }
    }
}

/// The body wikilink for a `follows:` back-link (design doc: Data Model - "the
/// body wikilink is what Obsidian's backlink pane resolves"). Re-derived from
/// `follows:` on EVERY render rather than being carried as body text itself -
/// a replace rewrites the whole body from the fresh distill pass, so anything
/// not re-emitted from the frontmatter key would silently vanish on the next
/// replay.
fn render_follows_link(stem: &str) -> String {
    format!("**Follows:** [[{stem}]]\n\n")
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn process_session(
    body: &str,
    members: &[SessionRecord],
    primary_id: &str,
    body_truncated: bool,
    tags: Vec<String>,
    method: IngestMethod,
    force: bool,
    intent: ResolveIntent,
    follows_prior: Option<watermark::PublishedEntry>,
    config: &Config,
    trace_id: &str,
) -> IngestResult {
    let start = Instant::now();
    match process_session_inner(
        body,
        members,
        primary_id,
        body_truncated,
        tags,
        method,
        force,
        intent,
        follows_prior,
        config,
        trace_id,
    )
    .await
    {
        Ok(mut result) => {
            let elapsed = start.elapsed();
            log::info!("[{trace_id}] Session pipeline completed in {elapsed:.2?}");
            result.elapsed_secs = Some(elapsed.as_secs_f64());
            result
        }
        Err(e) => {
            let elapsed = start.elapsed();
            log::error!("[{trace_id}] Session pipeline failed in {elapsed:.2?}: {e:?}");
            IngestResult {
                status: IngestStatus::Failed {
                    reason: format!("{:#}", e),
                },
                method: Some(method),
                elapsed_secs: Some(elapsed.as_secs_f64()),
                // The body/metadata are already in hand by the time this
                // handler runs (harvest fetched them upstream), so a
                // terminal error here is a distill/publish failure, never a
                // fetch failure.
                failure_stage: Some(vault::receipts::FailureStage::PublishFailed),
                ..Default::default()
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn process_session_inner(
    body: &str,
    members: &[SessionRecord],
    primary_id: &str,
    body_truncated: bool,
    tags: Vec<String>,
    method: IngestMethod,
    force: bool,
    intent: ResolveIntent,
    follows_prior: Option<watermark::PublishedEntry>,
    config: &Config,
    trace_id: &str,
) -> Result<IngestResult> {
    let primary = members.iter().find(|m| m.session_id == primary_id).ok_or_else(|| {
        eyre::eyre!(
            "process_session: primary id {primary_id} not present among {} member(s)",
            members.len()
        )
    })?;
    log::debug!(
        "process_session_inner: trace={trace_id} primary={primary_id} members={} body_len={} body_truncated={body_truncated} intent={intent:?}",
        members.len(),
        body.len()
    );

    let session_metadata = build_session_metadata(members, primary_id, body_truncated);
    let source_url = format!("clyde://{primary_id}");

    // Resolve BEFORE naming anything (design doc: "Resolve before you name").
    // `body` here is the same canonical transcript the harvest runner hashed
    // for the watermark (`harvest/publish.rs`) and the same bytes staging
    // wrote to `body.txt`, so this hash agrees across the live and replay
    // paths - asserted by `borg/tests/body_hash_agrees_across_paths.rs`.
    let body_hash = watermark::body_hash(body);
    let vault_root = config.vault_root()?;
    let prior_note = resolve_prior_note(&vault_root, trace_id, &source_url, &body_hash, intent)?;

    // harvest.model empty inherits llm.model (design doc: Distillation >
    // Model, the established per-feature override precedent).
    let model = if config.harvest.model.is_empty() {
        config.llm.model.clone()
    } else {
        config.harvest.model.clone()
    };
    let session_config = SessionConfig {
        model,
        max_chars: config.fabric.max_content_chars,
        timeout_secs: config.fabric.timeout_secs,
        token_cap: config.harvest.token_cap,
        // Bounded per-chunk retry knob (harvest distill-parsing robustness):
        // borg owns config; the config-free distillers crate receives the
        // value here rather than reading it.
        chunk_retries: config.distill.chunk_retries,
    };

    let distilled = crate::stages::distill::distill_for_publish_session(
        &config.fabric,
        &config.staging,
        trace_id,
        &source_url,
        body,
        &session_metadata,
        session_config,
    )
    .await;

    let mut all_tags: Vec<String> = tags.iter().map(|t| hygiene::sanitize_tag(t)).collect();
    all_tags.extend(distilled.tags.iter().map(|t| hygiene::sanitize_tag(t)));
    finalize_tags(&mut all_tags, config).await;

    // Scope + redaction are governance signals the design mandates on EVERY
    // harvest note ("work sessions included, scope-tagged"; "a session with
    // a nonzero [redaction] count gets a redacted-source tag"). Neither is in
    // the 110-tag canonical interest vocabulary `finalize_tags` filters
    // against, so they are appended AFTER canonicalization rather than
    // risking silent drop (see implementation notes, Deviations).
    all_tags.push(if primary.scope == "work" { "scope-work" } else { "scope-personal" }.to_string());
    if members.iter().any(|m| m.redaction_count > 0) {
        all_tags.push("redacted-source".to_string());
    }
    all_tags.sort();
    all_tags.dedup();

    // Embedding policy (design doc: only the distilled note is embedded; the
    // staged transcript is trace-recallable, never embedded) - same
    // transcript-free render policy as Article/Repo/Video publish.
    let rendered_distilled = distillers::render(
        &distilled,
        distillers::RenderOptions {
            include_transcript: false,
        },
    );
    let distilled_body = if members.len() > 1 {
        // Richer per-member footer (id, title, repo, duration) for thread
        // notes (design doc: Data Model) - `SessionPayload` only carries the
        // thread-level lead line + bare `clyde://` ids, so this reads the
        // full clustered `SessionRecord`s borg's publish layer holds
        // (`distillers::render`'s `push_session_footer` doc comment defers
        // exactly this richer footer to here).
        append_distilled_below_slides(
            rendered_distilled.body_markdown.clone(),
            &render_member_details(members),
        )
    } else {
        rendered_distilled.body_markdown.clone()
    };

    // `title` is present-null in the contract; a null OR empty title falls back
    // to `Session <id>` (the null case is new in harvest-completion Phase 1;
    // the empty-string case is preserved).
    let title = match primary.title.as_deref() {
        Some(t) if !t.trim().is_empty() => t.to_string(),
        _ => format!("Session {primary_id}"),
    };

    let tz = config.frontmatter.timezone_tz();
    let now = chrono::Utc::now().with_timezone(&tz);
    let mut frontmatter_additions = rendered_distilled.frontmatter_additions;
    // `repo:` rides verbatim from the export contract's `repo` field
    // (present-as-null, not omitted, when the cwd has no repo anchor -
    // design doc: Data Model / Phase 9 owns validation + hub wiring; this
    // renderer only emits the field). `repos-touched:` is Phase 9's addition
    // once clyde ships files-touched.
    frontmatter_additions.insert(
        "repo".to_string(),
        match &session_metadata.repo {
            Some(r) => serde_yaml::Value::String(r.clone()),
            None => serde_yaml::Value::Null,
        },
    );
    let expires = retention::trace_expires_for(now.date_naive(), config.staging.retention_days);
    frontmatter_additions.insert("trace-expires".to_string(), serde_yaml::Value::String(expires));

    // harvest-content-slug-naming (2026-07-24): the note's filename is the
    // distiller's content-derived slug (naming the real subject/outcome), NOT
    // the generic, collision-prone clyde session title. Fall back to the
    // title-slug only when the distiller omitted a slug, and WARN so the gap is
    // visible. The chosen stem is persisted as frontmatter `slug:` so it is
    // stable across re-harvest and gives the collision-association check
    // something to match on.
    let (slug_stem, used_title_fallback) = harvest_slug_stem(distilled.slug.as_deref(), &title, trace_id);
    if used_title_fallback {
        log::warn!(
            "[{trace_id}] session distiller emitted no slug; falling back to title-slug filename (title={title:?})"
        );
    }

    // A resolved prior note short-circuits the slug-derived naming entirely
    // (design doc: Architecture - "if this trace already has a landed note,
    // write to that note's current path"). `harvest_publish_path` keeps its
    // signature and its role: naming a NEW note.
    let note_path = match &prior_note {
        Some(resolved) => {
            log::info!(
                "[{trace_id}] session note resolves to an existing note - replacing in place: {} \
                 (fresh slug would have been {slug_stem:?})",
                resolved.display()
            );
            resolved.clone()
        }
        None => {
            let dest_path = config.inbox_dir()?;
            std::fs::create_dir_all(&dest_path).context("Failed to create destination directory")?;
            harvest_publish_path(&dest_path, &slug_stem, primary_id, force)
        }
    };

    // `slug:` names the file it actually lives in - on a replace that is the
    // RESOLVED stem, not the slug this distill pass produced (design doc: Data
    // Model, "`slug:` ... is set to the RESOLVED filename stem on a replace,
    // so it stops lying about the file it names").
    let effective_stem = note_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&slug_stem)
        .to_string();
    frontmatter_additions.insert("slug".to_string(), serde_yaml::Value::String(effective_stem));
    // Readers: the confirmation guard and the crash-recovery fallback in
    // `harvest::identity`, plus Phase 6's `--rebuild-state`.
    frontmatter_additions.insert(
        "harvest-body-hash".to_string(),
        serde_yaml::Value::String(body_hash.clone()),
    );

    // Frontmatter merge on replace: borg rewrites the keys it owns and carries
    // every other key forward verbatim, `status:` included (its value is user
    // state, not borg's). A carried key never displaces a freshly-derived one.
    let mut status = Some(vault::schema::Status::Unread);
    // `follows:` back-link (design doc Phase 4). A genuine follow-up resolves
    // its prior note FRESH (`follows_prior` is `Some` only for that case); a
    // plain replace/replay falls back to whatever the note being replaced
    // already carried, so the key (and the body wikilink re-derived from it
    // below) survives a replay untouched. `None` for a brand-new,
    // never-published note.
    let mut follows_stem: Option<String> = follows_prior
        .as_ref()
        .and_then(|prior| resolve_follows_stem(&vault_root, trace_id, &source_url, prior));
    if let Some(resolved) = &prior_note {
        let prior = read_prior_frontmatter(resolved)?;
        if let Some(prior_status) = prior.status {
            // Carried as a raw value rather than through `NoteContent.status`
            // so an off-schema operator value survives verbatim. It renders in
            // the additions block instead of its usual slot - same key, same
            // value, different line.
            status = None;
            frontmatter_additions.insert(STATUS_KEY.to_string(), prior_status);
        }
        if follows_stem.is_none() {
            follows_stem = prior.follows;
        }
        for (key, value) in prior.carried {
            if frontmatter_additions.contains_key(&key) {
                log::debug!("[{trace_id}] frontmatter key {key:?} re-derived by this publish; not carried forward");
                continue;
            }
            frontmatter_additions.insert(key, value);
        }
    }
    let distilled_body = match &follows_stem {
        Some(stem) => {
            frontmatter_additions.insert(FOLLOWS_KEY.to_string(), serde_yaml::Value::String(stem.clone()));
            format!("{}{distilled_body}", render_follows_link(stem))
        }
        None => distilled_body,
    };

    let note = NoteContent {
        title: title.clone(),
        source_url: Some(source_url.clone()),
        asset_path: None,
        tags: all_tags.clone(),
        summary: distilled.summary.clone(),
        description: None,
        capture_note: None,
        content_type: ContentType::Session,
        embed_code: None,
        method: Some(method),
        trace_id: Some(trace_id.to_string()),
        slides: Vec::new(),
        distilled_body: Some(distilled_body),
        frontmatter_additions,
        origin: Some(vault::schema::Origin::Generated),
        status,
    };

    let rendered = markdown::render_note(&note, &config.frontmatter);

    vault::note::write_atomic(&note_path, rendered.as_bytes()).context("Failed to write session note to vault")?;

    log::info!(
        "[{trace_id}] Wrote session note: {} (members={} replaced={})",
        note_path.display(),
        members.len(),
        prior_note.is_some()
    );

    // Repair the trace's recorded note_path (terminal-state-safe, so it works
    // for a replay too, which never reaches `process_content`'s receipts
    // chokepoint) and self-insert into the in-process vault index. Both are
    // best-effort AFTER a landed note: the note is the durable artifact, and
    // failing an already-published note over a bookkeeping write would be a
    // lie about what happened.
    record_landed_path(trace_id, &vault_root, &note_path);

    publish_note(
        config,
        &note_path,
        method,
        source_url,
        title,
        all_tags,
        trace_id,
        distilled.meta.validation.is_degraded(),
    )
}

/// Record where this trace's note actually landed, on EVERY session publish
/// including a replay (design doc: Architecture, "Receipts write-back" and
/// "Index freshness: self-insert on write").
///
/// [`receipts::update_note_path`] is the terminal-state-safe writer:
/// `mark_succeeded` carries `WHERE status='received'` and so cannot repair the
/// row of a note cortex has since moved, and a stage-2 replay bypasses that
/// chokepoint entirely. [`identity::note_published`] keeps the process-lifetime
/// vault index exact without a rebuild.
///
/// Best-effort by design: the note has already landed by the time this runs.
fn record_landed_path(trace_id: &str, vault_root: &Path, note_path: &Path) {
    match receipts::open_default() {
        Ok(conn) => {
            if let Err(e) = receipts::update_note_path(&conn, trace_id, &note_path.to_string_lossy()) {
                log::error!("[{trace_id}] receipts::update_note_path failed after publish: {e:#}");
            }
        }
        Err(e) => log::error!("[{trace_id}] receipts open failed for post-publish note_path repair: {e:#}"),
    }
    identity::note_published(vault_root, trace_id, note_path);
}

/// Deterministic Stage-0 metadata for the distiller (design doc: Watermark +
/// durable identity / Distillation > Input). `session_ids` puts `primary_id`
/// first (the distillers-crate doc comment's stated order), then the
/// remaining members in their `created`-order arrival - purely cosmetic
/// (the footer just lists ids), but it keeps the anchor session visibly
/// first.
fn build_session_metadata(members: &[SessionRecord], primary_id: &str, body_truncated: bool) -> SessionMetadata {
    let mut session_ids = Vec::with_capacity(members.len());
    session_ids.push(primary_id.to_string());
    for m in members {
        if m.session_id != primary_id {
            session_ids.push(m.session_id.clone());
        }
    }
    let repo = members
        .iter()
        .find(|m| m.session_id == primary_id)
        .and_then(|m| m.repo.clone());
    let total: i64 = members.iter().map(|m| m.n_msgs).sum();
    let msg_count = u32::try_from(total.max(0)).unwrap_or(u32::MAX);
    let date_start = earliest_created(members);
    let date_end = latest_modified(members);
    log::debug!(
        "build_session_metadata: primary={primary_id} members={} repo={repo:?} msg_count={msg_count} body_truncated={body_truncated}",
        members.len()
    );
    SessionMetadata {
        repo,
        session_ids,
        msg_count,
        date_start,
        date_end,
        body_truncated,
    }
}

/// Earliest `created` across members. Every member here already passed
/// through Phase 3's `cluster_threads` (which errors loudly on an
/// unparseable timestamp), so a parse failure here would mean a caller
/// bypassed that gate - logged, not panicked, and simply excluded from the
/// min/max rather than failing the whole publish.
fn earliest_created(members: &[SessionRecord]) -> Option<String> {
    let mut best: Option<(DateTime<FixedOffset>, String)> = None;
    for m in members {
        // `created` is present-null; a null value is guarded at selection, so
        // reaching here with `None` means a caller bypassed that gate - warn
        // and skip it from the min/max rather than failing the whole publish.
        let Some(created) = m.created.as_deref() else {
            log::warn!(
                "build_session_metadata: null created timestamp on session {}",
                m.session_id
            );
            continue;
        };
        match DateTime::parse_from_rfc3339(created) {
            Ok(dt) => match &best {
                Some((b, _)) if dt >= *b => {}
                _ => best = Some((dt, created.to_string())),
            },
            Err(e) => log::warn!(
                "build_session_metadata: unparseable created timestamp {created:?} on session {}: {e}",
                m.session_id
            ),
        }
    }
    best.map(|(_, s)| s)
}

/// Latest `modified` across members - see [`earliest_created`].
fn latest_modified(members: &[SessionRecord]) -> Option<String> {
    let mut best: Option<(DateTime<FixedOffset>, String)> = None;
    for m in members {
        match DateTime::parse_from_rfc3339(&m.modified) {
            Ok(dt) => match &best {
                Some((b, _)) if dt <= *b => {}
                _ => best = Some((dt, m.modified.clone())),
            },
            Err(e) => log::warn!(
                "build_session_metadata: unparseable modified timestamp {:?} on session {}: {e}",
                m.modified,
                m.session_id
            ),
        }
    }
    best.map(|(_, s)| s)
}

/// Richer per-member footer (id, title, repo, duration) for thread notes
/// (design doc: Data Model - "a footer listing member sessions (id, title,
/// repo, duration) for thread notes"). Distinct from `distillers::render`'s
/// `## Sessions` lead-line + bare id list, which is faithful to the frozen
/// `SessionPayload` alone; this reads the full `SessionRecord`s only borg's
/// publish layer holds.
fn render_member_details(members: &[SessionRecord]) -> String {
    let mut out = String::from("## Session Details\n\n");
    for m in members {
        let repo = m.repo.as_deref().unwrap_or("-");
        let duration = m
            .duration_secs
            .map(|secs| format!("{}m", (secs as f64 / 60.0).round() as i64))
            .unwrap_or_else(|| "-".to_string());
        out.push_str(&format!(
            "- clyde://{} - {} - `{repo}` - {duration}\n",
            m.session_id,
            m.title.as_deref().unwrap_or("").trim()
        ));
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests;
