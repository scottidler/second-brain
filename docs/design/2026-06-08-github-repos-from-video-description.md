# Design Document: GitHub Repos from YouTube Video Descriptions

**Author:** Scott Idler
**Date:** 2026-06-08
**Status:** Implemented
**Review Passes Completed:** 4/5 (incl. Architect design review, 2026-06-08)

## Summary

Many ingested YouTube videos reference a GitHub repository in their Description
(e.g. "code on GitHub: github.com/owner/repo"). Borg already fetches the
description via yt-dlp but discards it after hashtag extraction. This change
harvests `owner/repo` slugs from the description and writes them to a new
`github:` frontmatter list on the video note. The wiring is a one-seam change;
the design substance is the extractor's correctness rules.

## Problem Statement

### Background

Borg's video ingest path (`borg::stages::distill::distill_for_publish_video`)
fetches video metadata through yt-dlp. The fetched `crate::youtube::VideoMetadata`
includes a `description: String` field (`borg/src/youtube.rs:82`). Today that
description is used for two things only:

- hashtag extraction merged into tags (`borg/src/pipeline.rs:635`)
- a filtered display callout in the note body (`borg/src/pipeline.rs:648`)

The per-video frontmatter that survives to the note is produced through the
distilled-payload pipeline:

`VideoMetadata` (distillers input, `distillers/src/video.rs:48`)
→ `attach_payload` (`distillers/src/video.rs:439`)
→ `VideoPayload` (`vault/src/distilled.rs:109`)
→ `render` (`distillers/src/render.rs:73`) which emits `cortex-video-*` keys
→ generic frontmatter merge (`borg/src/markdown.rs`, no allowlist).

The single production builder of the distiller-side `VideoMetadata` is
`video_metadata_from_yt_dlp` (`borg/src/stages/distill.rs:49`), called exactly
once in production at `distill.rs:567` inside `distill_for_publish_video`. That
call site has the full `metadata` (with `.description`) in scope.

### Problem

A video that points at a GitHub repo loses that link. The note has no
machine-queryable record that "this video is about `owner/repo`", so oracle
cannot surface "videos about repo X" and the connection is invisible in the
vault graph.

### Goals

- Extract GitHub `owner/repo` slugs from a YouTube video's Description.
- Write them to a new `github:` frontmatter list on the video note.
- Reuse the existing `VideoMetadata` → `VideoPayload` → `render` data flow; add
  no new pipeline stage.

### Non-Goals

- **Description only.** We scan the video Description, not the transcript and not
  the note body. "Videos talking about a repo" is satisfied by the description
  link the creator put there; transcript mining is out of scope.
- **GitHub only.** GitLab, Bitbucket, sr.ht, etc. are out of scope ("github for
  now"). The field name `github` leaves room for sibling fields later.
- **No backfill of existing notes** (see Open Questions and Known Limitations for
  the rationale — the gap is structural, not a soft choice).
- **This change adds no yt-dlp call.** It reads the description from metadata
  `distill_for_publish_video` already fetches. Note: that fetch is itself a
  pre-existing duplicate of `process_youtube`'s fetch; this feature neither
  causes nor fixes the duplication (see Known Limitations).
- No verification that the repo exists / is reachable. We extract the literal
  reference; we do not call the GitHub API. (Verification would burn the
  unauthenticated 60-req/hr GitHub limit — see Known Limitations.)

## Proposed Solution

### Overview

1. A new lenient extractor `borg::github::extract_repo_slugs(text) -> Vec<String>`
   scans free text for GitHub repo references and returns deduplicated
   `owner/repo` slugs in first-seen order.
2. `distill_for_publish_video` calls it on `metadata.description` and sets the
   result on the (now mutable) `video_metadata`.
3. A new `repos: Vec<String>` field carries the slugs through the distiller-side
   `VideoMetadata` and the vault-side `VideoPayload`.
4. `render` emits a `github:` YAML sequence when `repos` is non-empty.

### Architecture

The only new behavior is the extractor. Everything else mirrors the existing
`channel` / `duration_seconds` / `published_at` fields exactly.

`video_metadata_from_yt_dlp` stays a pure mapper (no regex, no denylist) — the
github concern lives in `borg::github` and is invoked at the `distill_for_publish_video`
seam where `metadata.description` is in scope:

```
distill_for_publish_video
  metadata = youtube::fetch_metadata(url)            // has .description
  let mut video_metadata = video_metadata_from_yt_dlp(&metadata)  // pure mapper
  video_metadata.repos = github::extract_repo_slugs(&metadata.description)
  stage.distill_with_video_metadata(..., Some(&video_metadata))
```

### The extractor (the real design content)

`borg::github::parse_repo_url` already exists but is deliberately strict (it
rejects any path deeper than `/owner/repo` because it gates fetcher routing) and
does **no** reserved-name filtering — it would parse `github.com/sponsors/foo`
as `owner=sponsors`. It is therefore the wrong tool for scanning prose. We add a
separate function with explicit rules:

`extract_repo_slugs(text: &str) -> Vec<String>`

Rules, each unit-tested directly:

1. **Find candidates.** Match `github.com/<seg>/<seg>` occurrences in free text,
   tolerating a `https://`, `http://`, or bare `www.` prefix or no scheme at all
   (descriptions often write `github.com/owner/repo`). Host must be exactly
   `github.com` or `www.github.com`, compared case-insensitively
   (`GitHub.com`, `GITHUB.COM` match).
2. **Host exclusions.** Reject `gist.github.com` and `raw.githubusercontent.com`
   (different hosts; not repo roots).
3. **First two path segments only.** `/owner/repo/tree/main/src` → `owner/repo`.
4. **Trim noise.** Strip a trailing `.git`, strip query string (`?...`) and
   fragment (`#...`), and strip trailing punctuation that prose appends
   (`. , ) ] > " '` and `/`).
5. **Reserved-owner denylist (best-effort).** Reject slugs whose owner is a
   GitHub reserved path, not a user/org. Denylist:
   `sponsors`, `features`, `marketplace`, `orgs`, `settings`, `about`, `pricing`,
   `topics`, `collections`, `trending`, `notifications`, `explore`, `login`,
   `logout`, `join`, `signup`, `new`, `search`, `apps`, `customer-stories`,
   `readme`, `security`, `enterprise`, `contact`, `site`, `dashboard`,
   `account`, `codespaces`, `issues`, `pulls`, `watching`, `stars`, `blog`,
   `users`, `mobile`, `team`, `premium-support`, `git-guides`, `solutions`,
   `resources`, `events`, `sponsors-account`.
   This is a denylist, not an allowlist, so it cannot be exhaustive — GitHub can
   add a new top-level product route at any time and a creator who links it
   would mint a bogus slug (e.g. a future `github.com/foo/...`). This is
   **accepted by design**: extracted slugs are inert (written to frontmatter,
   never fetched or acted on), so a rare false positive costs one slightly-wrong
   list entry and nothing else. The alternative — verifying each candidate
   against the GitHub API — is rejected because it would consume the
   unauthenticated 60-req/hr rate limit (see Known Limitations). The denylist is
   the deliberate floor, not an attempt at completeness.
6. **Validity.** Owner and repo non-empty after trimming; reject if either still
   contains a `/` or whitespace.
7. **Dedup.** Case-insensitive dedup (GitHub routing is case-insensitive),
   preserving the first occurrence's original casing.

Output order is first-seen, so the YAML list is stable and diff-friendly given a
stable description.

### Data Model

Add `repos` to two structs, mirroring the existing optional fields.

`distillers/src/video.rs` (input type):

```rust
pub struct VideoMetadata {
    pub channel: Option<String>,
    pub duration_seconds: Option<u32>,
    pub published_at: Option<String>,
    pub repos: Vec<String>,   // NEW: owner/repo slugs from the description
}
```

`vault/src/distilled.rs` (payload type):

```rust
pub struct VideoPayload {
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub duration_seconds: Option<u32>,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub repos: Vec<String>,   // NEW
}
```

Both already derive `Default`, so existing constructors and the `..Default::default()`
call sites compile unchanged; `repos` defaults to an empty `Vec`. `#[serde(default)]`
on the payload means legacy `distilled.yml` artifacts without the field deserialize
fine.

### Frontmatter output

`render` (`distillers/src/render.rs`) gains one block in the `KindPayload::Video`
arm, mirroring `cortex-repo-topics`:

```rust
if !p.repos.is_empty() {
    fm.insert(
        "github".to_string(),
        serde_yaml::Value::Sequence(
            p.repos.iter().cloned().map(serde_yaml::Value::String).collect(),
        ),
    );
}
```

Resulting frontmatter on a video note:

```yaml
github:
  - coleam00/archon
  - scottidler/second-brain
```

The key is `github` (not `cortex-video-repos`) per the user's "go with github"
decision. `markdown.rs` merges any rendered key without an allowlist, so no
writer change is needed.

### attach_payload

`attach_payload` (`distillers/src/video.rs:439`) copies the new field and its
"any field populated" guard gains the repos check, so a video whose only metadata
is a repo link still gets a payload:

```rust
if m.channel.is_none()
    && m.duration_seconds.is_none()
    && m.published_at.is_none()
    && m.repos.is_empty()
{
    return;
}
distilled.kind_specific = Some(KindPayload::Video(VideoPayload {
    channel: m.channel.clone(),
    duration_seconds: m.duration_seconds,
    published_at: m.published_at.clone(),
    repos: m.repos.clone(),
}));
```

### Implementation Plan

#### Phase 1: Extractor + data model
**Model:** opus
- Add `extract_repo_slugs` to `borg/src/github.rs` with all rules above; unit
  tests in `borg/src/github/tests.rs` covering: bare `github.com/o/r`, scheme +
  `www.`, deeper path truncation, `.git` strip, query/fragment strip, trailing
  punctuation, every reserved owner, `gist.`/`raw.` exclusion, case-insensitive
  dedup, multiple repos in one description, no-repo description → empty.
- Add `repos: Vec<String>` to `distillers::video::VideoMetadata` and
  `vault::distilled::VideoPayload` (`#[serde(default)]`).

#### Phase 2: Wiring + render
**Model:** sonnet
- In `distill_for_publish_video`, make `video_metadata` mutable and set
  `video_metadata.repos = crate::github::extract_repo_slugs(&metadata.description)`.
- Update `attach_payload` (copy field + extend guard).
- Add the `github` render block to `render.rs`.
- Keep `video_metadata_from_yt_dlp` a pure mapper (unchanged signature/body).

#### Phase 3: Integration test + cleanup
**Model:** sonnet
- Render-level test: a `VideoPayload` with two repos renders a `github:`
  sequence; empty repos renders no key.
- `attach_payload` test: repos-only metadata still attaches a payload.
- `otto ci` green.

## Known Limitations

These are accepted, documented gaps — not bugs introduced by this change.

### Pre-existing yt-dlp double-fetch (this change rides it, does not add to it)

Every YouTube ingest already shells out to `yt-dlp --dump-json` twice:
`process_youtube` fetches metadata at `borg/src/pipeline.rs:833` (using it for
title, duration, and the returned description), and `distill_for_publish_video`
re-fetches the same metadata internally at `borg/src/stages/distill.rs:551`. This
feature reads the description from the *second* (already-existing) fetch, so it
adds no new network call — but it also does not fix the duplication.

The architecturally correct fix is to thread the `metadata` already owned by
`process_youtube` into `distill_for_publish_video` (changing its signature) and
delete the internal fetch. That is not a one-liner: it also requires untangling
the `tokio::join!(metadata_future, subtitles_future)` at `distill.rs:553`
(metadata would arrive as a parameter while subtitles still fetch inline),
reworking the `transcript_fallback` chain, and touching the title-fallback logic
at `distill.rs:595`. That is a standalone refactor with its own risk surface,
kept **out of scope** here to honor the "ever so slightly" framing (consensus
with the Architect, 2026-06-08). It is recorded as a worthwhile separate
cleanup; this feature does not depend on it and is not blocked by it.

### cortex backfill cannot populate `github` (structural)

`cortex summarize --backfill` rebuilds a note's distilled sections from the note
body and explicitly passes `video_metadata: None` (`cortex/src/summarize.rs:261`)
— cortex never fetches yt-dlp, so it has no description to scan. Consequently the
"no backfill" non-goal is **structural, not a soft choice**: even an
`audit --fix github-repos-missing` verb routed through cortex would produce
nothing. The only way to populate `github` on an already-published video note is
to re-run the borg ingest path (which re-fetches via yt-dlp) for that note.

A future fix, if backfill is ever wanted, is to cache the raw description in the
`VideoPayload` (or a sidecar) at publish time so cortex can re-extract without a
network call. Out of scope here; noted so the limitation is explicit rather than
discovered later.

## Alternatives Considered

### Alternative 1: Reuse `parse_repo_url`
- **Description:** Tokenize the description and feed each token to the existing
  `parse_repo_url`.
- **Pros:** No new function.
- **Cons:** `parse_repo_url` rejects depth > 2, so `/owner/repo/tree/main` (a
  very common description form) yields nothing; and it has no reserved-name
  filter, so `github.com/sponsors/x` becomes a bogus slug. Wrong tool — it gates
  fetcher routing, a different contract.
- **Why not chosen:** Correctness. Prose links need lenient parsing + a denylist
  that `parse_repo_url` must not have.

### Alternative 2: Top-level extraction in `pipeline.rs`, written as a typed `Frontmatter` field
- **Description:** Extract in `pipeline.rs` near the hashtag code and add a typed
  `github: Option<Vec<String>>` to `vault::frontmatter::Frontmatter`.
- **Pros:** Extraction sits next to the other description consumer.
- **Cons:** Splits video frontmatter production across two mechanisms (the
  `VideoPayload`/`render` path and a bespoke field), and a typed `Frontmatter`
  field implies it's a first-class universal key rather than a video-derived one.
- **Why not chosen:** The `VideoPayload` → `render` path already exists for
  exactly this (per-video derived frontmatter); reusing it keeps one mechanism.

### Alternative 3: Full URLs instead of slugs
- **Description:** Store `https://github.com/owner/repo`.
- **Pros:** Clickable in Obsidian.
- **Cons:** Noisier; harder to dedup across `http`/`https`/`www`/trailing-slash
  variants; doesn't match the `owner/repo` form repos are named by everywhere
  else (a repo note's `title` is `owner/repo`).
- **Why not chosen:** Slug matches the codebase's existing repo-naming convention
  and dedups cleanly. (Flagged in Open Questions — vetoable.)

## Technical Considerations

### Dependencies
- `url` crate (already a borg dependency, used by `parse_repo_url`).
- A regex or manual scan for candidate spans. `regex` is already in the borg
  dependency tree (`description::extract_hashtags` uses it); reuse it.

### Frontmatter round-trip (verified)
The new top-level `github:` sequence is not a typed `Frontmatter` field, so
`vault::frontmatter::Frontmatter::from_value` shunts it into the
`extra: HashMap<String, serde_yaml::Value>` catch-all unchanged. `cortex sweep`
and `cortex summarize` round-trip `extra` without attempting to cast it to a
scalar, so the sequence survives re-serialization intact and does not collide
with the canonical-tag / quality machinery. (Confirmed against
`vault/src/frontmatter.rs` and the cortex rewrite path.)

### Performance
- One extra linear pass over the description string per video ingest. Negligible
  against the yt-dlp fetch and the Fabric LLM calls that dominate the path.

### Security
- No new network calls, no new file writes, no shelling out. Extraction is pure
  string processing over data borg already holds. The denylist reduces (but, as a
  denylist, cannot eliminate) garbage slugs from GitHub product URLs; extracted
  slugs are inert (never fetched or acted on), so the residual risk is a
  cosmetically-wrong frontmatter entry, not a security or correctness hazard.

### Testing Strategy
- Extractor unit tests are the core (enumerated in Phase 1).
- Render + `attach_payload` tests confirm the wiring.
- `otto ci` for the workspace.

### Rollout Plan
- Forward-only: takes effect for videos ingested after deploy. Standard
  `bump && otto deploy` (no extension re-sign — this touches no `IngestRequest`
  field).

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Garbage slugs from GitHub product URLs (`/sponsors/...`, `/blog/...`) | Med | Low | Best-effort denylist, unit-tested per entry; residual false positives are inert frontmatter entries (accepted by design) |
| Description references repo without a github.com URL (plain "owner/repo") | High | Low | Out of scope; we only match github.com URLs. Avoids false positives from arbitrary `a/b` tokens |
| Deep/decorated URLs not truncated | Med | Med | First-two-segments rule + punctuation/query/fragment trim, unit-tested |
| Existing notes lack the field | High | Low | Forward-only by design; backfill gap is structural (see Known Limitations), not just deferred |

## Open Questions

- [ ] **Entry format — slug vs full URL.** Recommendation: `owner/repo` slug
  (matches repo-note `title` convention, dedups cleanly). Vetoable at spec
  review; switching to full URLs is a one-line change in `render.rs` plus the
  extractor's output form.
- [ ] **Backfill existing video notes.** Recommendation: no. The gap is
  structural (see Known Limitations): cortex never has the description
  (`summarize.rs:261` passes `video_metadata: None`), so an `audit --fix`
  /`cortex summarize` route cannot populate the field. The only backfill that
  works is re-running borg ingest (re-fetching via yt-dlp) — heavy,
  network-bound, rate-limited, contrary to the "ever so slightly" framing.
  Flagged, not built.
- [ ] **Double-fetch refactor (separate cleanup).** Should the pre-existing
  yt-dlp double-fetch (Known Limitations) be fixed as part of this work, or
  tracked as a standalone refactor? Recommendation: standalone — this feature
  neither causes nor depends on it.
- [ ] **Field name.** Using `github` per "go with github." If a sibling forge is
  ever added, this becomes one of several (`gitlab`, etc.) rather than a generic
  `repos` — confirm `github` is the intended key.

## References
- `borg/src/youtube.rs:82` — description captured from yt-dlp
- `borg/src/stages/distill.rs:49,567` — `video_metadata_from_yt_dlp` and its sole caller
- `borg/src/github.rs:63` — strict `parse_repo_url` (why a new extractor is needed)
- `distillers/src/video.rs:48,439` — `VideoMetadata` input + `attach_payload`
- `vault/src/distilled.rs:109` — `VideoPayload`
- `distillers/src/render.rs:73` — video render arm (`cortex-video-*` pattern)
- `borg/src/markdown.rs` — allowlist-free frontmatter merge
- `borg/src/pipeline.rs:833,927` — `process_youtube`'s metadata fetch + call into `distill_for_publish_video` (the double-fetch)
- `cortex/src/summarize.rs:261` — backfill hardcodes `video_metadata: None` (structural backfill gap)
- `vault/src/frontmatter.rs` — `extra` catch-all that round-trips the `github:` sequence
- Recent commit `65585a9` — `audit --fix github-creator-missing` (backfill precedent)
