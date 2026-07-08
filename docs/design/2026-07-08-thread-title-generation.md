# Design Document: Thread Title Generation

**Author:** Scott A. Idler
**Date:** 2026-07-08
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

Thread/social notes (X/Twitter, Reddit, HN ingests) sometimes get the raw numeric status/post ID as both `title` frontmatter and filename, because `title` is derived from a generic article-title extractor that was never designed for thread URLs. This design adds a thread-aware title builder that uses data borg already has (the LLM-extracted author, plus the distilled tldr/summary) instead of trusting the scraped page title, so the ID can never surface as a title again.

## Problem Statement

### Background

- `borg/src/pipeline.rs:694-697` computes `title` once, before distillation, for every non-YouTube URL: `title = scraped_title.clone()` unless it's a github repo (which gets its own `owner/repo` override, `docs/design/2026-05-19-github-slug-from-api.md`). Threads (`is_thread`, X/Reddit/HN) get no such override — they fall through to the generic article path.
- `scraped_title` comes from `extract_article_title` (`borg/src/pipeline/handlers.rs:12-71`), which tries, in order: (1) a `Title:` metadata line, (2) the first `# ` markdown heading, (3) the last URL path segment, (4) the raw URL. This is an article-title heuristic, reused verbatim for thread/social kinds — no thread-specific title logic exists anywhere in the codebase.
- For a normal Jina-served fetch of an X status page, Jina's own preamble (`Title: <Name> on X: "…"`) satisfies Strategy 1 and produces a correct-looking title — e.g. `notes/aitech-cloud-network-on-x-rag-vs-ai-agents-vs-agentic-rag.md`. **That shape is Twitter's own `<title>` tag passed through by Jina, not something borg constructs.** It is not a contract borg can rely on.
- When Jina fails and `fetch_article_markdown` (`borg/src/jina.rs:18-28`) falls back to `BrowserUaFetcher`, the resulting markdown carries no `Title:`/`Markdown Content:` preamble and no top-level `# ` heading. Strategies 1 and 2 fail, and Strategy 3 fires: `url.trim_end_matches('/').rsplit('/').next()` on `https://x.com/tom_doerr/status/2067473155988332909` returns `"2067473155988332909"` — no dot, so it is used verbatim. That string becomes `title:` frontmatter and, via `hygiene::sanitize_filename(&title)` (`borg/src/pipeline.rs:905`), the filename.
- `docs/design/2026-06-07-creator-field-across-ingest-kinds.md:449-454` already documents that `fabric -u` and the Jina path can be byte-identical, and that only the `BrowserUaFetcher` rung differs in shape — the exact mechanism behind this bug, just never connected to title extraction.
- Confirmed live in `~/repos/scottidler/obsidian`: of 14 `distill-thread-v1` notes, exactly 2 currently carry a purely-numeric title — `notes/2067473155988332909.md` (`@tom_doerr`), `notes/2069342679251452268.md` (`@DanKornas`). Two more (`@GithubProjects`, `@omarsar0`) were found and manually retitled/renamed during this same audit; they are not part of this design's backfill scope, just prior evidence of the same bug.
- `ThreadDistiller` (`distillers/src/thread.rs`) already extracts `author` correctly via its own LLM call in both remaining broken notes (`cortex-thread-author: '@tom_doerr'`, `'@DanKornas'`) — the raw BrowserUaFetcher markdown still contains the handle inline (e.g. `[@tom_doerr](https://x.com/tom_doerr)`), the LLM reads it regardless of the missing Jina preamble. This is purely a title/filename-derivation bug; distillation itself is healthy.
- Filename is not derived independently: `hygiene::sanitize_filename(&title)` runs on the same `title` value (`borg/src/pipeline.rs:905`, and again at the reingest-path check `borg/src/pipeline.rs:819`). Fixing `title` fixes the filename for free — one fix, not two, per the house rule that a field derived from another must never diverge.

### Problem

- A thread note can publish with a filename and title that carry zero human-readable information — just a tweet ID — making it unfindable by title search, ugly in backlinks, and indistinguishable from any other numeric-titled note.
- The failure mode is silent: no error, no warning, no quality-gate rejection. The note publishes successfully with a garbage title.
- The dependency on Jina's incidental `<title>` tag shape is fragile and undocumented as a contract; any future fetch-path change (a different fallback fetcher, a Jina outage, a robots.txt block) can reproduce this on any thread URL.

### Goals

- Guarantee a thread/social note's title is never a bare numeric ID, regardless of which fetch rung served the page.
- Derive the title from data borg already produces for threads (LLM-extracted `author`, `platform`, and the distilled `tldr`/`summary`) rather than trusting the scraped page's `<title>` tag.
- Generalize the fix to every `is_thread_host` platform (x, reddit, hn) — the builder takes `platform` as a parameter already, so there is no meaningful extra cost to making it platform-generic rather than X-specific, even though X is the only platform with live broken examples today.
- Fix the filename as a side effect of fixing `title` (no separate filename logic).
- Backfill the 2 known-bad live vault notes via the standard reingest path once the fix ships.
- Add regression coverage that fails if the numeric-ID fallback reappears.

### Non-Goals

- No change to `extract_article_title`'s behavior for plain articles or github repos — Strategies 1-4 stay exactly as they are for non-thread URLs. This design adds a thread-specific override, it does not touch the shared extractor's article-path behavior.
- No change to the Jina/fabric-u/BrowserUaFetcher fetch chain itself, and no attempt to make `BrowserUaFetcher` emit a `Title:` preamble. The fix works regardless of which fetcher serves the page, so the fetch chain does not need to change.
- No live reddit/hn fixtures exist in the vault today (all 14 known thread notes are `platform: x`), so reddit/hn coverage is structural (the builder is generic) but not empirically tested against real reddit/hn broken-title cases. Revisit if a reddit/hn numeric-title note is ever found.
- No re-titling of the 2 notes already fixed manually during this audit (the GitHub Projects / Fjall note and the elvis / always-on-agents note) — they are done, not in scope here.
- No change to `ThreadDistiller`'s author-extraction LLM call — it already works correctly; this design only consumes its output earlier in the pipeline than it does today.

## Proposed Solution

### Overview

- Add `borg/src/thread.rs::title_for_thread(platform: &str, author: Option<&str>, tldr: Option<&str>, summary: &str) -> Option<String>`: a pure, deterministic function (no I/O, no LLM call) that builds `"<author> on <Platform>: \"<snippet>\""` from data the pipeline already has once distillation completes.
- In `borg/src/pipeline.rs`, inside the `is_thread` branch, after `distilled` is computed (after Gate-2, before the tuple is returned), override `title` via `thread::resolve_title(is_thread, title, &distilled, trace_id)`, which routes threads through `thread::thread_title` and passes every other kind through untouched. **Threads never fall back to the old `scraped_title`-derived `title` at all** (review-panel finding, see Resolved Decisions). The title goes to a generic `"<Platform> thread"` (or `"Thread thread"` if even the platform is unknown), logged at `warn!`, in two cases: (1) the distillation is a **fallback**, detected via the typed `distilled.meta.validation.fallback_reason.is_some()` signal (`distillers/src/validate.rs:255` sets it on every `fallback_distilled`; the long-path reduce failures set it directly at `distillers/src/thread.rs:413,415`), or (2) the builder returns `None` because both author and snippet are empty. **Keying on `fallback_reason`, NOT on `kind_specific` absence, is load-bearing:** `ThreadDistiller::distill` calls `attach_platform` UNCONDITIONALLY at its shared exit (`distillers/src/thread.rs:127`), so a fallback still exits with `kind_specific = Some(Thread{platform, author:None})` and a `"[reason]\n\n<snippet>"` summary. Keying on payload absence would let that summary reach the builder and ship `X thread: "[fabric-timeout] ..."` -- the exact leak this design exists to prevent. The `fallback_reason` branch never reads `summary`, so the internal reason string can never become title material, and it still recovers the platform label from the attached payload.
- `hygiene::sanitize_filename` needs no change — it already truncates to 80 chars at a hyphen boundary (`vault/src/hygiene.rs:109-130`), so a long generated title self-truncates into a reasonable filename.

### Architecture

Thread title custody after this design:

```
distill_for_publish_thread -> Distilled { tldr, summary, kind_specific, meta.validation.fallback_reason, .. }
                                    |
   success path:   kind_specific = Some(Thread{author, platform}),      fallback_reason = None
   fallback path:  kind_specific = Some(Thread{author:None, platform}), fallback_reason = Some("[reason]")
                   (attach_platform runs UNCONDITIONALLY at distill's exit, distillers/src/thread.rs:127,
                    so kind_specific is Some even on fallback; the ONLY None is the outer dispatch-error)
                                    |
                                    v
                   thread::resolve_title(is_thread, article_title, &distilled, trace_id)
                                    |
              fallback_reason.is_some()? --yes--> generic "<Platform> thread", log::warn!  (summary NEVER read)
                                    | no
                                    v
                   thread::title_for_thread(platform, author, tldr, summary)
                                    |
                        Some(title) -----------------> overrides `title`
                                    |
                        None (author+snippet both absent)
                                    |
                                    v
                     generic "<Platform> thread" (or "Thread thread"), log::warn!
                                    |
                                    v
                     title -> hygiene::sanitize_filename(&title) -> filename (unchanged mechanism)
```

`scraped_title` and `extract_article_title`'s Strategy 3 output are never consulted for threads under this design — the fragile dependency this design exists to remove is fully severed, not merely guarded against. Everything downstream of `title` (frontmatter, `filename_stub` at `pipeline.rs:819`, `filename` at `pipeline.rs:905`) is unchanged.

### Data Model

No new types. Reuses existing fields:
- `vault::distilled::ThreadPayload { author: Option<String>, post_count: u32, platform: String }` (`vault/src/distilled.rs:293-299`) — already populated correctly by `ThreadDistiller`.
- `Distilled.tldr: Option<String>` and `Distilled.summary: String` (`vault/src/distilled.rs:16-24, ~16`) — already populated for threads.

### API Design

```rust
// borg/src/thread.rs -- new file, mirrors the shape of github::extract_repo_slugs
// (deterministic, pure, unit-testable, no network/LLM).

/// Platform identifier -> human-readable label for the title. Unknown platform
/// identifiers pass through capitalized (defensive; ThreadPayload::platform is
/// currently always one of "x" | "reddit" | "hn").
fn platform_label(platform: &str) -> String;

/// Longest usable text to quote in the title: `tldr` when present and
/// non-empty, else the first ~80 chars of `summary` at a word boundary, else
/// None. Collapses internal whitespace runs (including embedded newlines) to
/// a single space via a LOCAL `s.split_whitespace().collect::<Vec<_>>().join(" ")`
/// -- NOT `hygiene::normalize_text_input`, which also lowercases and would
/// mangle an author handle's or a proper noun's casing in the title (review-
/// panel finding: the doc's first draft cited `normalize_text_input` and its
/// own Phase 1 example contradicted it). The word-boundary truncation itself
/// has no existing shared helper in this codebase (`vault/src/hygiene.rs`'s
/// truncation is char-boundary-only, for filenames) -- `thread.rs` implements
/// its own nearest-preceding-space search.
fn title_snippet(tldr: Option<&str>, summary: &str) -> Option<String>;

/// Build a thread note title from data the pipeline already has. `author` is
/// used VERBATIM, whatever shape the LLM extracted -- an `@handle` or a
/// display name (`docs/design/2026-06-07-creator-field-across-ingest-kinds.md`
/// author extraction is documented to allow either, `borg/patterns/distill-
/// thread.md:77`; the live vault confirms both forms already coexist in
/// `cortex-thread-author`, e.g. `'@tom_doerr'` alongside `'Peter Steinberger'`).
/// This function does not normalize or choose between them -- see Resolved
/// Decisions.
///
/// Shapes, in priority order:
/// 1. author + snippet -> `"<author> on <Platform>: \"<snippet>\""`
/// 2. author only       -> `"<author> on <Platform>"`
/// 3. snippet only       -> `"<Platform> thread: \"<snippet>\""`
/// 4. neither            -> `None` (caller substitutes a generic platform-only title)
pub fn title_for_thread(platform: &str, author: Option<&str>, tldr: Option<&str>, summary: &str) -> Option<String>;
```

The seam is extracted into two pure, unit-testable functions in `borg/src/thread.rs` (so it can be exercised against synthetic `Distilled` values without a live pipeline run); `pipeline.rs` calls a single line, `let title = crate::thread::resolve_title(is_thread, title, &distilled, trace_id);`:

```rust
// borg/src/thread.rs
pub fn resolve_title(is_thread: bool, article_title: String, distilled: &Distilled, trace_id: &str) -> String {
    // Non-thread kinds (plain article, github owner/repo) keep their arriving
    // title byte-identically -- no behavior change outside the is_thread arm.
    if is_thread { thread_title(distilled, trace_id) } else { article_title }
}

pub fn thread_title(distilled: &Distilled, trace_id: &str) -> String {
    let payload = match &distilled.kind_specific {
        Some(KindPayload::Thread(t)) => Some(t),
        _ => None,
    };
    // A distiller fallback is detected via the TYPED signal, NOT via payload
    // absence: attach_platform runs unconditionally at distill's exit
    // (distillers/src/thread.rs:127), so a fallback still carries a
    // Some(Thread{platform, author:None}) payload plus a "[reason]\n\n..."
    // summary. fallback_reason.is_some() short-circuits BEFORE summary is ever
    // read, so the internal [reason] string can never leak into a title.
    let is_fallback = distilled.meta.validation.fallback_reason.is_some();
    let built = if is_fallback {
        None
    } else {
        payload.and_then(|t| {
            title_for_thread(&t.platform, t.author.as_deref(), distilled.tldr.as_deref(), &distilled.summary)
        })
    };
    built.unwrap_or_else(|| {
        // Reached on a fallback distillation OR when a success-path payload has
        // neither author nor snippet. Recovers the platform label from the
        // attached payload (so an x-platform fallback titles "X thread", not
        // "Thread thread"); never consults `scraped_title`/Strategy 3.
        let label = payload.map(|t| platform_label(&t.platform)).unwrap_or_else(|| "Thread".to_string());
        log::warn!("[{trace_id}] thread_title: no usable author/snippet (is_fallback={is_fallback}); using generic '{label} thread' title");
        format!("{label} thread")
    })
}
```

### Implementation Plan

Ship order: single repo (`second-brain`), no cross-repo blast radius. One daemon-host deploy after Phase 3, then the operator backfill (Phase 4).

#### Phase 1: `thread::title_for_thread` + `title_snippet` + `platform_label`
**Model:** sonnet
- Add `borg/src/thread.rs` with `platform_label`, `title_snippet`, `title_for_thread` per the API Design above. Declare `mod thread;` in `borg/src/lib.rs`. Snippet whitespace-collapse is a small local helper (`split_whitespace().join(" ")`), NOT `hygiene::normalize_text_input` (that lowercases; see API Design note).
- **Success criteria:** (a) `title_for_thread("x", Some("@tom_doerr"), Some("Fjall is a Rust KV store"), "...")` returns `Some("@tom_doerr on X: \"Fjall is a Rust KV store\"")` with casing preserved; (b) `title_for_thread("x", Some("Peter Steinberger"), Some("..."), "...")` preserves the display-name casing verbatim (proves no accidental lowercasing); (c) `title_for_thread("reddit", None, None, "")` returns `None`; (d) `title_for_thread("hn", Some("dang"), None, "")` returns `Some("dang on Hacker News")`; (e) a `tldr` containing embedded newlines produces a single-line title with internal whitespace collapsed, not lowercased.

#### Phase 2: Wire the override at the thread distill seam
**Model:** opus
- In `borg/src/pipeline.rs`, inside the `is_thread` branch (after `distilled` is bound from `distill_for_publish_thread`, before the tuple return at line 759), apply the seam wiring above. Confirm `distilled.kind_specific` is populated for all three thread platforms (x/reddit/hn) on the success path before relying on it — if any platform's distiller path leaves `kind_specific` as `None` on success (not fallback), that is a pre-existing gap to flag, not silently paper over.
- Scope note: this phase's success criteria test the SEAM against constructed/synthetic `Distilled` values (unit/integration level, in-process) — live-vault reingest of the 2 known-bad notes is Phase 4's job, not this phase's, so a code phase's success does not depend on a live vault mutation.
- **Success criteria:** (a) given a synthetic `Distilled` with `kind_specific = Some(Thread(ThreadPayload{author: Some("@tom_doerr".into()), platform: "x".into(), ..}))` and a populated `tldr`, the resulting `title` is non-numeric and matches `title_for_thread`'s output; (b) given a synthetic fallback-shaped `Distilled` (`kind_specific: None`, `summary` starting with `"[fabric-timeout]"`, as `distillers::validate::fallback_distilled` actually produces), the resulting `title` is `"X thread"` — non-numeric AND does not contain the leaked `[fabric-timeout]` string; (c) a plain article or github-repo reingest is byte-identical in its title-selection path (no behavior change outside `is_thread`).

#### Phase 3: Regression test
**Model:** sonnet
- Feed the pipeline's title-selection logic a synthetic header-less `BrowserUaFetcher`-shaped markdown body for an X status URL (no `Title:`, no `# ` heading) and assert the resulting title is NOT purely numeric. Break-the-code: with the Phase 2 override removed, assert the SAME fixture DOES degenerate to the numeric ID — locking in the regression case per the quality-bar rule ("tests must bite").
- **Success criteria:** the positive test asserts a non-numeric title; the negative (override reverted) test asserts the pre-fix numeric-ID output, proving the fixture reproduces the original bug; both green under `otto ci`.

#### Phase 4: Backfill the 2 live broken notes (operator step, not a code phase)
- After Phase 1-3 ship and deploy, reingest the 2 known-bad notes via the standard replay path: `sb borg replay --bootstrap-from-vault --note notes/2067473155988332909.md` and the same for `notes/2069342679251452268.md`. Verified safe for a filename change: `borg/src/pipeline.rs` writes the new note at its new path first, then deletes the old path only if it differs from the new one (`pipeline.rs:982-989`, confirmed by direct read) — so a title-driven filename change on reingest cannot leave two copies or lose the note.
- This touches the live `obsidian` vault, not `second-brain` source — call it out explicitly as a post-deploy operator action, not something a phase-implementer agent executes.
- **Success criteria:** both notes have non-numeric titles and filenames matching the new convention; `grep -rlP "^title: '?\d{6,}'?$" notes/*.md` in the vault returns zero results; both notes' `cortex-thread-author`/`trace`/tags survive the reingest unchanged (reingest preserves them via the existing `old_note_path` restore path, `pipeline.rs:565-583`).

## Acceptance Criteria

- [ ] No thread/social note in the vault has a purely-numeric `title` after Phase 4 backfill (`grep -rlP "^title: '?\d{6,}'?$" notes/*.md` returns empty).
- [ ] `title_for_thread` is deterministic and makes zero network/LLM calls (unit-proven).
- [ ] The Phase 3 regression test fails when the Phase 2 override is reverted (proves the fixture reproduces the original bug).
- [ ] Article and github-repo title-selection behavior is unchanged (their existing tests stay green).
- [ ] A thread note whose distillation falls back (Fabric/YAML failure) still publishes with a non-numeric, non-panicking, non-leaking title (`"<Platform> thread"`, not the internal fallback-reason string).

## Resolved Decisions

- **2026-07-08 — title uses `author` verbatim: handle OR display name, whichever the LLM extracted.** Correction from this doc's first draft, which claimed `ThreadPayload.author` is always an `@handle` — that claim does not survive contact with the code or the vault. `borg/patterns/distill-thread.md:77` explicitly permits the extractor to return "handle or display name"; `distillers/src/thread.rs` passes the LLM's `author` field through unmodified (no normalization step exists); and the live vault already has both forms coexisting in `cortex-thread-author` (`'@tom_doerr'` alongside `'Peter Steinberger'`, `'tobi lutke'`, `'Z.ai'`, `'Sumanth'`, `'Santiago'`). `title_for_thread` does not choose between or normalize these shapes — it uses whatever `author` contains, exactly as every other consumer of `cortex-thread-author` already does. Titles will read `@tom_doerr on X: "..."` for the two notes this design backfills (both happen to store `@handle`) and `Peter Steinberger on X: "..."` for others — both correct, no special-casing needed.
- **2026-07-08 — threads never fall back to the old `scraped_title`/Strategy-3 path; a fallback distillation (or a builder-`None`) goes straight to a generic platform label.** Original draft proposed keeping the old title as a last-resort fallback, guarded by an `is_purely_numeric` check. Review panel correctly identified this as backwards, and the standard fix is to bypass `scraped_title` entirely for threads. **Correction folded in during Phase 2 implementation (recorded here so the doc matches shipped reality):** an earlier revision of this decision claimed a distiller fallback leaves `distilled.kind_specific = None`, and wired fallback-detection off that. That premise is factually wrong. `ThreadDistiller::distill` calls `attach_platform` UNCONDITIONALLY at its shared exit (`distillers/src/thread.rs:127`), applied to whatever `distill_short`/`distill_long` returned — including their `fallback_distilled` values. So a Fabric-error / YAML-parse-error / missing-summary / reduce-failure fallback exits with `kind_specific = Some(Thread{platform, author:None})` and a `"[reason]\n\n<snippet>"` summary, NOT `kind_specific = None`. (The observation that the three inner `fallback_distilled` call sites don't themselves call `attach_platform` is true but irrelevant — `attach_platform` runs later, in the shared `distill` exit, over their return value. The only genuine `kind_specific = None` at the seam is the outer `dispatch-error` fallback in `distill_for_publish_thread`.) The shipped code therefore detects a fallback via the typed, authoritative `distilled.meta.validation.fallback_reason.is_some()` signal (`distillers/src/validate.rs:255` sets it on every `fallback_distilled`; the long-path reduce failures set it directly at `distillers/src/thread.rs:413,415`). Keying on `fallback_reason` (a) never reads `summary`, so the internal `"[fabric-timeout] ..."`/`"[yaml-parse-error] ..."` string can never leak into a title, and (b) still recovers the platform label from the attached payload, so an x-platform fallback titles as `"X thread"` rather than `"Thread thread"`. Going straight to `"<Platform> thread"` is simpler (no numeric-detection helper needed) and strictly safer (structurally severs the dependency on the fragile scraped title rather than merely guarding one failure shape of it). This also fully resolves Alternative 2 below (no longer needed for threads) and the "is the numeric guard fail-closed for reddit/hn too" concern (there is no numeric guard to be under- or over-scoped).
- **2026-07-08 — snippet source is `tldr`, falling back to `summary`.** `Distilled.tldr` is explicitly documented as a "one-sentence hook" for display (`vault/src/distilled.rs:16-24`) — the natural fit for a title snippet. Falls back to a word-boundary-truncated `summary` prefix only when `tldr` is absent (legacy `distilled.yml` without the field, or an extractor that didn't populate it). A fallback `Distilled` is excluded from this path entirely per the decision above, so `summary`'s `[reason]` prefix is never eligible as a snippet.
- **2026-07-08 — fix generalizes to all `is_thread_host` platforms (x/reddit/hn), tested against X only.** The builder already takes `platform` as a parameter, so restricting it to X would be an artificial narrowing with no implementation savings. Reddit/HN paths are structurally covered but have no live broken fixtures to test against today (all 14 current thread notes are `platform: x`, reverified directly against the vault via `grep -rl "distilled-extractor: distill-thread-v1"` both at `notes/*.md` and fully recursive — 14 both times); revisit with real fixtures if a reddit/hn numeric-title note ever surfaces.
- **2026-07-08 — backfill via standard reingest, no standalone one-shot tool.** Only 2 live notes are affected. `bin/strip-transcripts` (`docs/design/2026-07-07-distillation-output-restore.md`) earned a standalone one-shot tool because it swept hundreds of notes; 2 notes is squarely inside `sb borg replay`'s existing, already-tested reingest path. A bespoke tool for 2 rows would be unrequested scope.
- **2026-07-08 — 3 code phases stay, not collapsed to 2 (review-panel pushback, not adopted).** Reviewer suggested merging Phase 1 (builder) and Phase 3 (regression test) into fewer phases as "more ceremony than needed" for a small fix. Declined: each phase here is already small, independently committable, and otto-ci-green on its own — exactly the phasing bar this repo holds itself to (see `distillation-output-restore` and `video-links-restore`, both landed as 3+ small phases for comparably-scoped fixes). Phase 3 specifically exists to lock in the regression with a break-the-code test distinct from Phase 1's unit tests; folding it into Phase 1 would blur "the builder works" from "the seam wiring can't silently regress."

## Alternatives Considered

### Alternative 1: Make `BrowserUaFetcher` synthesize a `Title:` preamble
- **Description:** Have the browser-UA fallback fetcher construct a `Title:` line (e.g. from the page's `<title>` tag if present, or a generic placeholder) so `extract_article_title` Strategy 1 succeeds uniformly across fetchers.
- **Pros:** Fixes the symptom at the fetch layer; no thread-specific code needed.
- **Cons:** Still depends on the scraped page happening to have a usable `<title>` tag (X's own SPA shell may not render one server-side for the fallback fetcher); doesn't use the author/tldr data borg already has, which is strictly better signal than a scraped title for a thread note.
- **Why not chosen:** Treats the symptom (missing title text) rather than the root cause (using the wrong data source for thread titles at all).

### Alternative 2: Regex-detect and reject numeric IDs in `extract_article_title` Strategy 3
- **Description:** In the shared article-title extractor, skip Strategy 3 when the URL path segment is purely numeric, falling through to Strategy 4 (raw URL) instead.
- **Pros:** Minimal, localized change; fixes the specific symptom for every URL kind, not just threads.
- **Cons:** Falling through to the raw URL as a title is barely better than the numeric ID (`https://x.com/tom_doerr/status/2067473155988332909` as a title is still unreadable and slugifies just as badly); doesn't leverage the author/tldr data that's actually available for threads.
- **Why not chosen:** Rejected for threads outright, not merely parked — under the Resolved Decision above, threads never consult `extract_article_title`'s output at all when the thread-aware builder has nothing to work with, so hardening Strategy 3 would fix a path threads no longer walk. It remains a live, independent option for plain-article and github-repo URLs (where Strategy 3 is still consulted and could theoretically hit the same numeric-segment shape), but no live evidence of that happening exists today, and fixing it there is out of scope for this design — a separate, smaller design if it's ever observed.

### Alternative 3: Have `ThreadDistiller` generate the title via its own LLM call
- **Description:** Add a `title` field to `ThreadPayload`, populated by asking the same LLM call that extracts author/claims to also produce a human title.
- **Pros:** Could produce a more naturally-phrased title than a mechanical template.
- **Cons:** An LLM call for something fully derivable from data already extracted (author + tldr) is wasteful and nondeterministic — the same reasoning `docs/design/2026-07-07-video-links-restore.md` used to reject an LLM-based fix for video links in favor of a deterministic parse.
- **Why not chosen:** Deterministic template composition is cheaper, exact, and testable; mirrors the house preference for deterministic parses over LLM calls for structured, already-available data.

## Technical Considerations

### Dependencies
- No new crates. `title_snippet`'s whitespace-collapse is a small local helper in `borg/src/thread.rs` (deliberately not `hygiene::normalize_text_input`, which lowercases — see API Design).

### Performance
- Pure string composition over already-in-memory `Distilled` fields. Negligible.

### Security
- None new. No new external input crosses a trust boundary; the snippet text is already rendered into the note body today (via `## Summary`), just not previously used for the title.

### Testing Strategy
- Unit: `title_for_thread` across all four priority-order shapes, casing-preservation, and whitespace-collapse.
- Integration: Phase 2's synthetic-`Distilled` seam tests (success path and fallback-shaped path), Phase 3's header-less-fetch fixture (positive and negative, break-the-code).
- Backfill verification: the vault-wide numeric-title grep, before and after Phase 4.

### Rollout Plan
- Phases 1-3 ship as normal commits (`otto ci` green each), one daemon-host deploy after Phase 3. Phase 4 (backfill) runs manually against the live vault after deploy.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| A distiller fallback (Fabric/YAML failure) leaves `kind_specific` absent, so no author/snippet at all | Med (fallback is a known, logged failure mode) | Low | Goes straight to `"<Platform> thread"`, never the ID and never a leaked `[reason]` string; `log::warn!` fires so the degraded case is visible, not silent |
| Generated title exceeds a reasonable length, producing an ugly (though not broken) title | Med | Low | `hygiene::sanitize_filename` already truncates the filename at 80 chars on a hyphen boundary; the frontmatter `title:` field itself is unbounded but bounded in practice by `tldr`'s one-sentence design intent |
| Two thread notes with the same author and no distinguishing snippet collide on filename | Low | Low | Pre-existing risk for any title-derived filename scheme in this codebase (not introduced by this design); out of scope here |
| Reddit/HN title shape is wrong in practice (untested, no live fixtures) | Med | Low | Structural fix ships for all platforms at zero extra cost; revisit with real fixtures if it surfaces, per Resolved Decisions |
| Multiple distinct thread notes all hit the fallback path on the same day, all titled `"X thread"` | Low | Low | Same collision class as the row above, same disposition — pre-existing filename-collision handling in this codebase is unchanged by this design |

## Open Questions

*(empty)*

## References

- Surfaced by: manual audit of `~/repos/scottidler/obsidian` git status during the 2026-07-07 distillation-output-restore cleanup; 2 broken notes found (`notes/2067473155988332909.md`, `notes/2069342679251452268.md`), 2 more found and manually fixed on the spot (not in this design's scope).
- Root cause seam: `borg/src/pipeline/handlers.rs:12-71` (`extract_article_title`), `borg/src/pipeline.rs:694-697` (`title` binding), `borg/src/pipeline.rs:905` (filename derivation).
- Fetch-path context: `borg/src/jina.rs:18-28` (Jina -> BrowserUaFetcher fallback), `docs/design/2026-06-07-creator-field-across-ingest-kinds.md:449-454` (fetch-path shape differences, prior art).
- Fallback-path verification (informs the "detect fallback via `fallback_reason`, never fall back to `scraped_title`" decision): `distillers/src/thread.rs:127` (`attach_platform` runs UNCONDITIONALLY at `distill`'s shared exit, over the value the inner `fallback_distilled` sites returned — so `kind_specific` is `Some(Thread{..})` even on a fallback), `distillers/src/validate.rs:222-263` (`fallback_distilled` sets `validation.fallback_reason` and builds the `"[reason]\n\n<snippet>"` summary; the `set` is at `:255`), `distillers/src/thread.rs:413,415` (the long-path reduce failures set `fallback_reason` directly). The shipped seam keys on `distilled.meta.validation.fallback_reason.is_some()`, not on `kind_specific` absence (`borg/src/thread.rs:120-156`).
- Author-shape evidence (informs the "verbatim, handle or display name" decision): `borg/patterns/distill-thread.md:77`, live vault `cortex-thread-author` values.
- Precedent for a kind-specific title override at the same seam: `docs/design/2026-05-19-github-slug-from-api.md`.
- Reingest safety for a filename-changing backfill: `borg/src/pipeline.rs:982-989` (delete-old-after-write-new ordering), `borg/src/pipeline.rs:565-583` (frontmatter-preserving reingest restore).
- Sibling design (same "deterministic over LLM" reasoning): `docs/design/2026-07-07-video-links-restore.md`.
- Reviewed by: Architect + Staff Engineer panel, 2026-07-08. All MUST-FIX and CONVERGENCE findings folded in; one phasing pushback (3 vs 2 code phases) declined with rationale above; the vault population count (14 `distill-thread-v1` notes) was re-verified directly against the live vault and kept as originally drafted, against the panel's uncorroborated "22."
