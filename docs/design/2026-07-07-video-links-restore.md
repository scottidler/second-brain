# Design Document: Video Links Restore

**Author:** Scott A. Idler
**Date:** 2026-07-07
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

Reingested video notes on v0.10.0 publish with no `## Links` section; the April baseline had one (curated tool/repo URLs). Root cause: a video's link source is its description, and the description was dropped as a link input at the 2026-05-16 structured-distiller cutover. The distiller now sees only the transcript, which contains no URLs, so `Distilled.links` comes back empty and render omits the heading. This design restores `## Links` for videos with a deterministic, no-LLM parse of the description URLs -- mirroring the existing `github::extract_repo_slugs` -- wired in at the same seam that already harvests repo slugs.

## Problem Statement

### Background

- Pre-2026-05-16 the `obsidian-note.md` fabric pattern was fed the video description and the LLM lifted a `References` section from it (github repos AND non-github URLs like `skool.com`/`chaseai.io`). April's `References` == today's `## Links` (2026-07-07-distillation-output-restore.md:78).
- The 2026-05-16 structured-distiller cutover changed the video distiller input to the transcript only. `Distilled.links` is now populated solely from the LLM's parse of the transcript (`distillers/src/video.rs:475-489` single-call, `:273-282` map-reduce).
- The 2026-07-07 distillation-output-restore correctly restored the `## Links` heading and the `Distilled.links` field, but did not restore a link *source* for videos -- it assumed the distiller would populate links, which for videos it structurally cannot.
- Separately, the 2026-06-08 design added `github::extract_repo_slugs(&metadata.description)` -> a first-class `github:` frontmatter block (`render.rs:145-150`). That captures the github repos from the description, but nothing else, and never feeds `## Links`.

### Problem

- Video notes lose their curated `## Links` section. Confirmed live: reingested `ht-4cbdf8` staged `distilled.yml:103` has `links: []`; the note has no `## Links`.
- Concrete data loss: description URLs that are NOT github repos are dropped entirely -- not in `github:` frontmatter, not in any body section. The Top-10 test video lost `https://python.useinstructor.com/`; the April baseline had `skool.com` and `chaseai.io`.
- This is a design gap the reingest surfaced, not an implementation deviation: the code faithfully populates links from the distiller output; the design never wired the description as the video link source.

### Goals

- Restore `## Links` on video notes, sourced deterministically from the video description (no new LLM call).
- List ALL description URLs (github + non-github), matching the April baseline (owner decision, Resolved below).
- Recover the non-github URLs that are currently lost outright.
- Keep the staged `distilled.yml` and the rendered note consistent (populate `links` before the staged write).

### Non-Goals

- No change to article or repo link extraction. Their links come from an LLM parse of the fetched body / README (`article.rs:197-206,359-368`; `repo.rs:161-170`), populate when the source has hyperlinks, and are NOT empty-by-construction. Link *quality* there is a separate LLM-dependent concern, out of scope.
- No render change. `render.rs::push_links` (`:334-355`) already emits `## Links` correctly once `links` is non-empty.
- No removal of the `github:` frontmatter block. It stays first-class; `## Links` listing the same github URLs is accepted redundancy (owner decision).
- No re-distillation of legacy notes here. Existing video notes get the restored `## Links` when reingested (or by the future re-distill verb); this design does not add a backfill.
- No change to the `Distilled.links` type or the transcript-derived link path (the LLM may still contribute links; the description parse is merged on top).

## Proposed Solution

### Overview

- Add `description::extract_urls(&str) -> Vec<Link>`: a deterministic regex URL scan over the FILTERED description, dedup first-seen, mirroring `github::extract_repo_slugs`.
- At the video distill seam (`distill.rs`, inside `distill_for_publish_video`, after `distilled` is built and BEFORE `write_distilled_yml`), run `filter_description` -> `extract_urls` and merge the result into `distilled.links` (dedup against any LLM-emitted links, case-insensitive).
- Render is unchanged: a now-non-empty `links` makes `push_links` emit `## Links`.

### Architecture

Video link custody after this design:

```
description (yt-dlp metadata)
  |-> github::extract_repo_slugs -> github: frontmatter        (existing, unchanged)
  |-> description::filter_description -> extract_urls -> Vec<Link>
                                              |-> merged into Distilled.links
                                                      |-> staged distilled.yml   (before write)
                                                      |-> note body ## Links      (render, unchanged)
transcript -> LLM parse -> Distilled.links (may add cited URLs; description merge is additive)
```

Seam ordering (critical, staging-fidelity invariant `distilled.rs:72-77`): the merge runs BEFORE `write_distilled_yml` so the staged `distilled.yml` and the rendered note carry the same `links`. Populating after the staged write would make staging and note diverge -- the same ordering trap the distillation-output-restore source-gate fix hit.

### Data Model

No type change. `Distilled.links: Vec<Link>` where `Link { url: String, label: Option<String> }` (`vault/src/distilled.rs:236-242,49-52`). `extract_urls` returns `Vec<Link>`:

- `url`: the absolute URL as it appears in the (filtered) description, after unwrap/trim (below), validated by `Url::parse`.
- `label`: derived per the label rule (below) when a clean name is present; `None` (bare URL renders) otherwise.

**URL unwrap/trim rule (finding 3 -- a bad URL renders a broken link, `render.rs:341` writes the label straight into markdown, so this is correctness, not cosmetics):**
- Markdown-wrapped `[text](url)`: strip the OUTER `[...]( )` wrapping, keep the INNER url; the `text` becomes the label.
- Trailing prose punctuation: trim only UNBALANCED trailing `.`/`,`/`)`/`>`/`]`. A `)` whose matching `(` is inside the url is kept (e.g. `.../Rust_(programming_language)`) -- count parens, trim a trailing `)` only when unbalanced.
- After unwrap/trim, `Url::parse` must succeed or the token is dropped (and logged, below). Scheme-less tokens (`www.x.com`) fail parse and are dropped-with-log, not silently lost (accepted; scheme inference is a non-goal, see Resolved).

**Label-derivation rule (finding 3):** first match wins, else `None`:
1. `[text](url)` -> label = `text`.
2. Leading list marker + `Name: URL` (`- Name: url`, `1. Name: url`, emoji/whitespace prefix stripped) -> label = `Name` (trimmed; a `Name (owner)` form kept whole).
3. Otherwise bare url, `label = None`.
Multiple URLs on one line: each becomes its own `Link`; the label rule applies only to a URL that is the sole url on its line, else all bare.

### API Design

```rust
// borg/src/description.rs -- beside filter_description / extract_hashtags
/// Extract absolute URLs from a (filtered) video description as Links, in
/// first-seen order, deduped by EXACT full-url string (HTTP paths/queries are
/// case-sensitive -- unlike github slugs; finding 4). Applies the unwrap/trim
/// and label rules above; a token that fails Url::parse after trimming is
/// dropped. Deterministic; no network, no LLM.
pub fn extract_urls(description: &str) -> Vec<Link>;

/// Seam-level, injectable, unit-testable helper: extract the FILTERED
/// description's URLs and merge them into `links` (dedup EXACT on url vs any
/// existing/LLM-emitted links, keep first-seen). Returns (added, dropped) counts
/// for the seam's debug log. Takes a &str so tests hit the load-bearing step
/// directly -- distillers::VideoMetadata has NO description field (finding 2),
/// and the raw description lives in borg::youtube::VideoMetadata, fetched inside
/// the non-injectable distill_for_publish_video.
pub fn merge_description_links(links: &mut Vec<Link>, filtered_description: &str) -> (usize, usize);
```

Seam wiring (finding 1 -- order matters against the existing log):

```rust
// distill.rs, in distill_for_publish_video: `distilled` is `let mut`, and the
// merge runs BEFORE the existing `links={}` info log (:738-739), THEN before
// write_distilled_yml (:750). filter_description returns Option<String>.
let filtered = description::filter_description(&metadata.description).unwrap_or_default();
let (added, dropped) = description::merge_description_links(&mut distilled.links, &filtered);
log::debug!("distill_for_publish_video: description links added={added} dropped={dropped} total={}", distilled.links.len());
// ... existing links={} info log at :739 now reports the POST-merge count ...
// ... write_distilled_yml(:750) persists the merged links ...
```

### Implementation Plan

Ship order: single repo (`second-brain`), no cross-repo blast radius. One daemon-host deploy after Phase 3. Legacy video notes pick up `## Links` on their next reingest -- the same reingest path already exercised in the distillation-output-restore live verification.

#### Phase 1: `description::extract_urls` + `merge_description_links`
**Model:** sonnet
- Add both functions to `borg/src/description.rs` (API Design above): regex absolute-URL scan; markdown-unwrap + balanced-paren-aware trailing-punctuation trim; `Url::parse` validate-or-drop; label derivation per the rule; dedup EXACT on the full url string (NOT case-insensitive -- HTTP paths/queries are case-sensitive; `extract_repo_slugs` is case-insensitive only because github routing is, which does not generalize). Reuse the `regex`/`url` crates already in borg deps.
- **Success criteria:** (a) fed the FILTERED description of `ht-4cbdf8`, returns all 10 URLs including the non-github `https://python.useinstructor.com/`; (b) fixture `"Support me: https://patreon.com/foo"` present in a RAW description is absent after `filter_description` -> `extract_urls` (proves ordering: extract from filtered, not raw); (c) two urls differing only in path case (`.../Foo` vs `.../foo`) both survive (exact dedup); (d) `[Instructor](https://x)` -> `label=Some("Instructor")`, `- LiteLLM: https://y` -> `label=Some("LiteLLM")`, bare `https://z` -> `label=None`; (e) `https://en.wikipedia.org/wiki/Rust_(programming_language)` keeps its balanced trailing `)`, while `see https://x.com/.` trims to `https://x.com/`, and a scheme-less `www.x.com` is dropped.

#### Phase 2: Wire the merge at the video distill seam
**Model:** opus
- In `distill_for_publish_video` (`distill.rs:714-753`): make `distilled` mutable; call `merge_description_links(&mut distilled.links, &filter_description(&metadata.description).unwrap_or_default())` AFTER `distilled` is built and BEFORE the existing `links={}` info log at `:738-739` (finding 1 -- placing it after the log leaves the log reporting the pre-merge empty count), then before `write_distilled_yml` (`:750`). Emit the `debug!` with added/dropped/total counts. List ALL URLs (github + non-github) per the owner decision.
- **Success criteria:** (a) reingesting `ht-4cbdf8` yields a staged `distilled.yml` with non-empty `links` including the non-github URL; (b) the published note renders `## Links` with those URLs; (c) the existing `:739` info log reports the POST-merge count (== `distilled.links.len()`); (d) an empty/absent description still publishes with no panic and no empty `## Links` heading; (e) a failed second yt-dlp fetch (fallback path `:668`) publishes with no `## Links` and does not panic (acknowledged failure mode, below).

#### Phase 3: Regression test
**Model:** sonnet
- Unit-test `merge_description_links` directly (it takes a `&str`, so the load-bearing step is injectable -- `distillers::VideoMetadata` has no description field, finding 2): a mixed github/non-github filtered description merges into a `Vec<Link>` containing the non-github url with correct labels; feed that `links` through `distillers::render` + `RenderOptions::for_url_publish` and assert `## Links` contains the non-github url. Negative: empty description -> empty merge -> no `## Links` heading. Break-the-code: deleting the merge line makes the positive test fail.
- **Success criteria:** the positive test asserts the non-github url appears in the rendered `## Links`; the negative asserts no `## Links` heading; both green under `otto ci`; the positive test fails when the Phase 2 merge call is reverted.

## Acceptance Criteria

- [ ] A freshly reingested "Top N" video note publishes a `## Links` section listing all description URLs (github + non-github), with the non-github URL(s) present, and the staged `distilled.yml` `links` matches.
- [ ] A video with no description (or no URLs) publishes with no `## Links` heading and no panic.
- [ ] `description::extract_urls` is deterministic and makes zero network/LLM calls (unit-proven).
- [ ] The Phase 3 regression test fails when the merge is removed (test bites).
- [ ] Article and repo note link behavior is unchanged (their existing tests stay green).

## Resolved Decisions

- **2026-07-07 -- `## Links` lists ALL description URLs (github + non-github).** Owner decision (April parity). The `github:` frontmatter block still renders the github repos; the same URLs also appearing in `## Links` is accepted redundancy -- `github:` serves Dataview, `## Links` serves human clicks and the FTS body parser. Rejected: non-github-only in `## Links`.
- **2026-07-07 -- deterministic parse, no LLM.** The URLs are literals in the description; a regex parse mirroring `extract_repo_slugs` is the correct mechanism, not a prompt change. Avoids an LLM call and the transcript-has-no-URLs dead end.
- **2026-07-07 -- extract from the FILTERED description.** Run `filter_description` first so affiliate/patreon/"follow me" boilerplate URLs are stripped before extraction -- April's curated References matched the filtered set, not the raw description.
- **2026-07-07 -- video-only scope.** Article/repo links are LLM-parsed from the fetched body/README and are not empty-by-construction; this gap is video-specific.
- **2026-07-07 -- labels derived, bare-url fallback.** `[text](url)` and leading `Name: URL` line shapes yield a label; otherwise a bare url renders. Full rule in Data Model. A bad label is a broken rendered link (`render.rs:341`), so the rule is explicit with fixtures, not "cosmetic."
- **2026-07-07 -- dedup EXACT on the full url string** (review finding 4). Not case-insensitive: HTTP paths/queries are case-sensitive, so two urls differing only in path case are distinct. `extract_repo_slugs`' case-insensitivity is a github-routing property that does not generalize.
- **2026-07-07 -- scheme-less tokens dropped, with a log** (review finding 5). `www.x.com` fails `Url::parse` and is dropped; the seam emits `debug!` added/dropped counts so the drop is visible (fail-loud), never silent. Inferring a scheme is a non-goal (risk of guessing wrong protocol); revisit only if real descriptions prove scheme-less URLs common.
- **2026-07-07 -- ride the existing second-fetch description source** (review finding 6). `## Links` (and the pre-existing `github:` block) source from the description fetched inside `distill_for_publish_video` (`:664`), a SECOND yt-dlp fetch distinct from the first in `process_youtube`. If the second fetch fails, the fallback (`:668`) returns and no `## Links` is produced even though the caller held a usable description. This is pre-existing (the `github:` repo-slug harvest at `:685` already rides it) and this feature is symmetric with it (siblings behave identically). Accepted; the fetch-dedup refactor is parked (Alternative 4).

## Alternatives Considered

### Alternative 1: Non-github URLs only in `## Links`
- **Description:** `## Links` carries only the URLs currently lost (non-github); github repos stay solely in `github:` frontmatter.
- **Pros:** no two-signals redundancy; recovers exactly the lost data.
- **Cons:** diverges from the April baseline, which listed all URLs; splits "the links for this video" across two places for a human reader.
- **Why not chosen:** owner chose April parity; the `github:` frontmatter and `## Links` serve different consumers.

### Alternative 2: Feed the description to the video distiller (LLM)
- **Description:** append the description to the transcript in the distiller input so the LLM lifts links (as the old `obsidian-note.md` did).
- **Pros:** matches the pre-cutover mechanism; LLM can label/curate.
- **Cons:** an LLM call to extract literal URLs is wasteful and nondeterministic; risks the LLM hallucinating or dropping URLs; the URLs are already structured data.
- **Why not chosen:** deterministic parse is cheaper, exact, and testable; fail-loud/closed on a parse beats LLM variance for literal data.

### Alternative 3: Backfill legacy video notes with a strip-style sweep
- **Description:** a one-shot binary that reparses every legacy video note's description into `## Links`.
- **Pros:** fixes existing notes without reingest.
- **Cons:** legacy notes may lack the raw description; reingest already produces the correct shape end-to-end; unrequested scope.
- **Why not chosen (parked):** revisit with the future re-distill verb; reingest covers the need now.

### Alternative 4: Dedup the yt-dlp fetch (pass the first-fetch description into the seam)
- **Description:** thread the description already fetched in `process_youtube` (first fetch, `handlers.rs:94` / `pipeline.rs:805`) into `distill_for_publish_video` instead of the second fetch at `:664`, so both `## Links` and the `github:` block source from one fetch and survive a second-fetch failure.
- **Pros:** removes a duplicate yt-dlp call; fixes the failure-divergence (visible Video-Description callout but empty `## Links`) for BOTH the new links and the pre-existing repo-slug path.
- **Cons:** touches the pre-existing repo-slug harvest and the video handler plumbing -- broader than this doc's link-restore intent; a separate, testable refactor.
- **Why not chosen (parked):** pre-existing behavior, and this feature is symmetric with the shipped repo-slug harvest at the same seam. Recorded here so the fetch-dedup isn't re-discovered from scratch; revisit as its own targeted change. (Surfaced to owner at review; ride accepted.)

## Technical Considerations

### Dependencies
- No new crates. `regex` and `url` are already direct borg dependencies. `distillers` crate needs no change (`push_links` already correct).

### Performance
- One regex pass over the (already-fetched) description per video ingest. Negligible.

### Security
- None new. The description is already fetched and rendered into the note; extracting its URLs crosses no trust boundary.

### Testing Strategy
- Unit: `extract_urls` on real filtered descriptions (mixed github/non-github, labeled/bare, dedup, affiliate-stripped-upstream). Break-the-code: point the extractor at a raw (unfiltered) description and confirm the affiliate URL would leak -- proving `filter_description` ordering matters.
- Integration: reingest-shape fixture asserting `## Links` content + staged `distilled.yml` `links` parity; negative (no description).
- Regression: the merge-removed test bites.

### Rollout Plan
- Phases 1-3 ship as normal commits (otto ci green each); one daemon-host deploy after Phase 3. Live verification: reingest the Top-10 test video (`ht-09d970`) and confirm `## Links` lists all 10 URLs including `python.useinstructor.com`. Legacy video notes get `## Links` on their next reingest.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Raw-description scan drags in affiliate/spam URLs | Med | Low | Extract from `filter_description` output, not raw; unit test proves the ordering |
| Label parse misreads a line shape | Med | Low | Bare-url fallback always valid; label is cosmetic |
| Description absent on some videos | Med | Low | `filter_description` returns `Option`; `None`/empty -> no URLs -> no `## Links` heading (render already handles empty) |
| Case-insensitive dedup drops a distinct case-sensitive URL | Low | Med | Dedup EXACT on the full url string (finding 4); path/query case preserved |
| Trailing punctuation / markdown-wrap corrupts a URL -> broken rendered link | Med | Med | Balanced-paren-aware trim + markdown-unwrap + `Url::parse` validate in Phase 1; unparseable tokens dropped-with-log, never emitted raw (render.rs:341 writes label straight to markdown) |
| Second yt-dlp fetch fails -> no `## Links` despite caller holding a description | Low | Low | Acknowledged pre-existing failure mode (finding 6); fallback publishes cleanly, no panic; fetch-dedup parked (Alternative 4) |

## Open Questions

*(empty -- Q1 links-scope and Q2 labels closed in Resolved Decisions above)*

## References

- Baseline example: `~/repos/scottidler/obsidian/notes/top-10-claude-code-skills-plugins-clis-april-2026.md:45` (`## Links`)
- Surfaced by: reingest test of `ht-4cbdf8` (staged `distilled.yml:103` `links: []`)
- Prior art to mirror: `borg/src/github.rs:173` (`extract_repo_slugs`), `docs/design/2026-06-08-github-repos-from-video-description.md`
- Parent design: `docs/design/2026-07-07-distillation-output-restore.md` (restored `## Links` heading + field; this doc restores the video link source)
- Render (unchanged): `distillers/src/render.rs:334-355` (`push_links`)
- Seam: `borg/src/stages/distill.rs:714-753` (`distill_for_publish_video`), description harvest at `:685`
