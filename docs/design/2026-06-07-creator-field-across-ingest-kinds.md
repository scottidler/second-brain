# Design Document: Populate `creator` across youtube/github/blog ingestion + surface it in the ledger

**Author:** Scott Idler (with Claude)
**Date:** 2026-06-07
**Status:** Implemented
**Review Passes Completed:** 5/5 + Architect review (Gemini)

## Summary

The `creator` frontmatter field (the canonical "who made this" property) is
populated almost exclusively for YouTube notes (98%), but barely for GitHub (2%)
or blog/web (6%). The only code path that writes `creator` is the
`ContentType::YouTube` arm in `borg/src/markdown.rs`. This change populates
`creator` for GitHub (repo owner, already in hand at dispatch) and blog (byline
extraction) at ingest time, and adds an "Author" column to the
`borg-ledger.base` Obsidian view so it is meaningful across all three kinds.

## Problem Statement

### Background

borg classifies every ingested URL and routes by kind
(`pipeline.rs::process_url_inner`): `router::classify_url` produces a
`url_match.link_name`, then YouTube goes to `process_youtube()`, GitHub/social/
reddit/article each map to a `ContentType`, and the note is rendered by
`borg/src/markdown.rs`. The render step writes frontmatter via a
`match &note.content_type { ... }`, and **only the `YouTube` arm emits a
`creator:` line** (from `metadata.uploader`). A config `default_creator` exists
but is empty by default.

Two facts make `creator` the right field to invest in:

1. **It is already canonical.** `cortex/src/migrate.rs` carries a rename
   `author -> creator`. The vault has 918 `creator:` notes and only 2 stray
   `author:` notes. Introducing a parallel `author` field would fight this
   decision.
2. **It already powers the knowledge graph.** `cortex/src/hub.rs` treats
   `creator` as a first-class entity (`HubKind::Creator`, shared-creator edges,
   `creator_weight: 0.2`). Populating it for GitHub and blog enriches the graph
   (owner hubs, author hubs), not just the ledger column.

### Problem

Measured coverage of `creator` across ingested notes (notes with a `source:`):

| kind | notes | have `creator` | coverage |
|------|-------|----------------|----------|
| youtube | 884 | 868 | 98% |
| blog/web | 286 | 18 | 6% |
| github | 48 | 1 | 2% |

`creator` is effectively a YouTube-only field. A ledger "Author" column today
would be blank for almost every GitHub and blog row. The author data exists or
is cheaply derivable for both missing kinds; the ingestion code simply never
writes it.

### Goals

- Populate `creator` at ingest for **GitHub** notes (= repo owner).
- Populate `creator` at ingest for **blog/web** notes (= byline) on a
  best-effort basis with deterministic fallbacks.
- Keep YouTube `creator` working unchanged (or unify it through the same
  resolution point without regression).
- Add an "Author" column (property `creator`) to `borg-ledger.base` and its
  views.
- Backfill existing GitHub notes cheaply (owner is in the `source` URL).

### Non-Goals

- A new `author` frontmatter field. `creator` is canonical; reuse it.
- Social/Reddit/HN `creator` population (the thread distiller already carries a
  `ThreadPayload.author`; wiring that to `creator` is a follow-up, not this doc).
- Blog backfill of the existing 286 notes via re-fetch (network-heavy; see Open
  Questions). Go-forward blog population is in scope; mass blog re-fetch is not.
- Changing how YouTube derives its uploader (yt-dlp remains authoritative).

## Proposed Solution

### Overview

Thread the per-kind author into the note's `ContentType` (the variant already
carries kind-specific data, as `YouTube { uploader, .. }` does), then resolve
`creator` once at render time from a single `creator_for(&ContentType)` helper.
GitHub gets its owner for free at dispatch; blog gets a new byline extractor.
The ledger gains a `creator` column.

### Architecture

Confirmed ingest flow (`borg/src/pipeline.rs`):

```
classify_url(url) -> url_match.link_name
  ├─ youtube  -> process_youtube() -> ContentType::YouTube { uploader, duration_secs }
  ├─ github   -> ContentType::GitHub        + parse_repo_url(url) -> (owner, repo)
  ├─ social   -> ContentType::Social        (thread distiller)
  ├─ reddit   -> ContentType::Reddit        (thread distiller)
  └─ _        -> ContentType::Article       (fabric/jina fetch -> article_md)
```

Two changes to this flow:

1. **Carry the author on the variant.** `ContentType::GitHub { owner: String }`
   and `ContentType::Article { author: Option<String> }`. `YouTube` keeps
   `uploader`. The compiler then forces every construction site to supply (or
   explicitly omit) the author.
2. **Resolve `creator` in one place.** `markdown.rs` gains
   `fn creator_for(ct: &ContentType) -> Option<String>` returning the uploader /
   owner / article author. The render writes `creator:` once from this helper
   (replacing the YouTube-only write). `default_creator` config remains the
   fallback when the helper returns `None`.

   This also fixes a latent bug: today the render writes `creator:` from
   `default_creator` (markdown.rs:141) **and then again** in the `YouTube` arm
   (markdown.rs:154). A YouTube note with a non-empty `default_creator` would
   emit two `creator:` lines; it is masked only because `default_creator` is
   empty by default. Consolidating to a single write removes the double-write.

### Data Model

`borg/src/markdown.rs::ContentType`:

```rust
pub enum ContentType {
    YouTube { uploader: String, duration_secs: f64 },
    Article { author: Option<String> },   // was: Article
    GitHub  { owner: String },             // was: GitHub
    Social,
    Reddit,
}
```

`creator` stays a top-level `vault::frontmatter::Frontmatter` field (already
exists, already indexed in `vault::search` FTS, already read by
`cortex::hub`). No schema change to `Frontmatter`.

**Fetcher contract** (`borg/src/types.rs` `FetchResult` / `FetchMeta`): add an
optional byline carrier so the author travels with the body from the same fetch.
The minimal shape is an extracted author on the meta:

```rust
pub struct FetchMeta {
    // ...existing fields...
    pub author: Option<String>,   // populated by fetchers that can see it
}
```

`BrowserUaFetcher` sets it by running `byline::extract` on the `raw` HTML it
already holds (`fetcher.rs:242`) before markitdown; the Jina-JSON fetcher sets it
from the JSON `author`; `fabric -u` leaves it `None`. Carrying the *extracted*
author (not the full raw HTML) keeps the contract small and avoids passing large
HTML blobs up the stack. The article path then maps `meta.author` into
`ContentType::Article { author }`.

**GitHub `None` case:** when `github::parse_repo_url` returns `None` (deep paths,
`github.com/blog/...`, pseudo-owners), `ct` is `ContentType::Article { author }`,
**not** `GitHub` - this matches the existing distiller routing, which already
sends `github_repo.is_none()` URLs to the article distiller (`pipeline.rs:567`).
Only repo-root github URLs become `ContentType::GitHub { owner }`.

`borg-ledger.base` adds:

```yaml
properties:
  creator:
    displayName: Author
```

and `creator` is added to each of the three views' `order:` lists. In the
default "Ledger" view place it after `domain`; the "By Method" and "By Domain"
views omit their grouped column from `order`, so just append `creator` among
their remaining columns. Position is cosmetic - the only requirement is that the
column appears in every view.

### API Design

- `borg::markdown::creator_for(&ContentType) -> Option<String>` — pure, testable.
- `borg::byline::extract(html: &str) -> Option<String>` — new module; pure
  parser over raw HTML (see byline strategy below). No network in the function
  itself; the caller (a fetcher that already holds HTML) supplies it. Must handle
  JSON-LD `author` as string | object-with-`.name` | array-of-those.
- GitHub owner needs no new API: `github::parse_repo_url` already returns
  `(owner, repo)` and is already called at `pipeline.rs:562` to build the title.

### Byline extraction strategy (blog)

**The fetchers do not expose a byline today, and a separate fetch to get one is
rejected.** Verified state of the fetch layer:

- `process_article_fabric` -> `fabric::fetch_article` (`fabric -u`) returns
  markdown only (`pipeline.rs:1086`). `fabric -u` is the **default** path
  (`use_fabric`), and it is opaque - there is no raw HTML to recover.
- `BrowserUaFetcher` fetches the raw HTML (`stages/fetcher.rs:242`,
  `let raw = response.bytes()`) but pipes it through `markitdown` and returns
  only the markdown (`:246-263`); the HTML is discarded.
- `jina.rs` requests `Accept: text/markdown` and returns markdown.

A standalone `GET` of the URL to recover a byline is **rejected**, not a
fallback: because `fabric -u` is the default and yields no HTML, such a `GET`
would fire on *every* blog ingest, on the hot path; and a bare `GET` lacks the
browser-UA / headless capability that `fabric -u` and `BrowserUaFetcher` use to
clear bot-walls, so it would frequently land on a Cloudflare/challenge page and
yield `None` anyway - pure latency, no byline.

Instead, **surface the byline from the same fetch that already has it**, via a
fetcher-contract change (see Data Model): the fetchers that hold the source data
attach it to `FetchResult`, and `byline::extract` runs on that:

- `BrowserUaFetcher` already has `raw` HTML in hand - attach it (or the extracted
  author) to the result before markitdown discards it. `byline::extract(html)`
  then runs the deterministic ladder, reusing the *same* bot-wall-cleared fetch.
- Jina's JSON mode (`Accept: application/json` / `X-Respond-With`) returns
  `author` + `publishedTime` from the identical request - attach `author`.
- `fabric -u` (the default success path) yields no HTML and no author, so those
  notes resolve to `author: None`. We accept this rather than re-fetch.

`byline::extract` deterministic ladder (over the HTML a fetcher hands up):

1. `<meta name="author" content="...">`
2. JSON-LD `"author"` - handle all shapes: a string, an object with `.name`, or
   an **array** of either; take the first resolvable name.
3. `<meta property="article:author" ...>` / `og:article:author`
4. `<link rel="author">` / `<a rel="author">` text
5. Fallback: `None` (leave `creator` unset; never fabricate)

A Fabric-pattern byline is an even later fallback, not attempted here (see
Alternatives).

**Coverage reality (set expectations honestly):** because `fabric -u` succeeds
for most articles and exposes no HTML, blog byline coverage will **not** approach
the YouTube 98%. It is limited to (a) Jina-JSON hits and (b) articles that fall
through to `BrowserUaFetcher` (the ones `fabric -u` blocked on). This is
best-effort by design - the goal is "populate when cheaply and correctly
knowable," not "every blog gets an author." Notes ingested via a successful
`fabric -u` keep `creator` empty until reingested through a path that carries
HTML. Raising fabric-path coverage would mean reordering or augmenting the
article fetcher, which is out of scope here (see Open Questions).

**Multiple authors:** when a page lists several (co-bylines, JSON-LD `author`
arrays), take the **first** and stop. `creator` is a single scalar and the
graph/ledger key on one value; joining names would create junk hub entities.

### Implementation Plan

#### Phase 1: Carry author on ContentType + single `creator_for` render (github free)
**Model:** opus
- Change `ContentType::GitHub` -> `GitHub { owner: String }` and
  `ContentType::Article` -> `Article { author: Option<String> }` in
  `borg/src/markdown.rs`; fix the `content_type_str` match and all constructors.
- **Move the `ct` construction.** Today `ct` is built at `pipeline.rs:536`,
  *before* `parse_repo_url` (562) and the article fetch, so owner/author are not
  yet known. Relocate the construction so `ct` is assembled once its data is
  available: for a github **repo root** (`parse_repo_url` yields `Some(owner)`)
  -> `ContentType::GitHub { owner }`; when `parse_repo_url` is `None` (deep
  paths, `github.com/blog/...`) -> `ContentType::Article { author: None }`, which
  matches the distiller already routing those to the article path
  (`pipeline.rs:567`); all other non-youtube kinds ->
  `ContentType::Article { author: None }` this phase (Phase 2 fills `author`).
  Social/Reddit/YouTube unchanged.
- Add `fn creator_for(&ContentType) -> Option<String>` and rewrite the render so
  `creator:` is written **exactly once**: use `creator_for(ct)`, and when it
  returns `None` fall back to `default_creator` only if that config string is
  non-empty (it is a `String`, not an `Option`). Remove both the YouTube-arm
  write and the standalone `default_creator` write so neither can fire twice.
  **YouTube's `creator` now flows through `creator_for` too** (it returns the
  `uploader`); the YouTube match arm keeps writing `duration:` - only the
  `creator:` line moves out of it.
- Unit tests: `creator_for` returns uploader / owner / article-author / None per
  variant; render emits exactly one `creator:` line (including the
  `default_creator`-set + YouTube case that double-wrote before); github note
  renders `creator: <owner>`.
- `cargo test -p borg`; `otto ci`.

#### Phase 2: Blog byline via the fetcher contract (no extra fetch)
**Model:** opus
- Add `borg/src/byline.rs` with `extract(html: &str) -> Option<String>` and the
  ladder above; table-driven unit tests covering each rung **and** the JSON-LD
  shapes (string, object-with-`.name`, array) plus a no-author page -> `None`.
- Add `author: Option<String>` to `FetchMeta` (`borg/src/types.rs`).
- `BrowserUaFetcher` (`stages/fetcher.rs`): run `byline::extract` on the `raw`
  HTML it already holds (`:242`) before markitdown discards it, and set
  `meta.author`. `fabric -u` leaves it `None`.
- Jina fetcher: switch to JSON mode (or add a JSON variant) so the response
  carries `author`; set `meta.author` from it. **Spike first** to confirm Jina's
  exact JSON field name(s) before committing (see Open Questions); if the field
  is unreliable, fall back to running `byline::extract` over Jina's HTML if it
  can return HTML, else leave `None`.
- Article path: finalize `ContentType::Article { author: meta.author }` *after*
  the fetch (filling the Phase-1 `None` placeholder). **No standalone `GET`.**
- Tests: byline unit tests as above; a fetcher test asserting `BrowserUaFetcher`
  populates `meta.author` from HTML; the fabric path leaves `creator` empty.
- `cargo test -p borg`; `otto ci`.

#### Phase 3: Ledger "Author" column
**Model:** sonnet
- Edit `~/repos/scottidler/obsidian/system/views/borg-ledger.base`: add
  `creator` to `properties` (`displayName: Author`) and to each view's `order:`.
- Verify in Obsidian that the column renders and is YouTube-rich immediately.

#### Phase 4: Backfill existing GitHub notes
**Model:** sonnet
- Add a new `borg::audit::FindingKind` (e.g. `GithubCreatorMissing`): a note whose
  `source` is a github.com repo root and whose `creator` is empty. Implement the
  fix in `apply_fixes` to set `creator = parse_repo_url(source).owner`. Surfaced
  via the existing `sb borg audit --fix github-creator-missing`. This is cleanup
  on a stable schema (no format change), so Rust is appropriate per the
  no-Rust-migration rule's carve-out (the same carve-out that already covers
  `audit --fix`). No network.
- Run once on the daemon host; verify by re-measuring github coverage.
- Blog backfill is deferred (Open Questions).

## Alternatives Considered

### Alternative 1: Introduce a new `author` frontmatter field
- **Description:** Add `author` to `Frontmatter`, populate per kind, add an
  `author` ledger column.
- **Pros:** The word "author" reads more naturally than "creator" for blogs.
- **Cons:** Forks the data: `cortex/src/migrate.rs` already renamed
  `author -> creator`, `cortex::hub` keys on `creator`, search FTS indexes
  `creator`. Two fields means two graph entities and a split ledger.
- **Why not chosen:** `creator` is canonical by prior decision; label the column
  "Author" via `displayName` instead.

### Alternative 2: Resolve `creator` from the `Distilled` payload
- **Description:** Add owner/author to `RepoPayload`/an article payload and have
  the distilled->frontmatter render set `creator`.
- **Pros:** Centralizes per-kind metadata in one contract; backfillable via
  `cortex summarize --backfill`.
- **Cons:** The rendered `creator` currently comes from `ContentType` in
  `markdown.rs`, not from `Distilled`. Routing it through Distilled for some
  kinds and ContentType for YouTube splits the source of truth.
- **Why not chosen:** Keep one render path (`creator_for(&ContentType)`).
  Revisit if/when YouTube's `uploader` is unified into `VideoPayload.channel`.

### Alternative 3: Standalone `GET` to recover the byline
- **Description:** When the fetcher hands up only markdown, do a separate HTTP
  `GET` of the URL and run `byline::extract` on that HTML.
- **Pros:** Uniform - works regardless of which fetcher produced the body.
- **Cons:** `fabric -u` is the default and exposes no HTML, so this `GET` would
  fire on essentially every blog ingest (hot-path latency); and a bare `GET`
  lacks the browser-UA / headless that the real fetchers use, so it lands on
  bot-walls and returns `None` - latency with no byline. It also risks a body/
  byline mismatch when the `GET` page differs from the distilled one.
- **Why not chosen:** Surfacing `meta.author` from the same fetch is free,
  divergence-proof, and bot-wall-consistent. The `GET` is rejected outright.

### Alternative 4: LLM/Fabric byline extraction first
- **Description:** Ask a Fabric pattern to extract the author from article text.
- **Pros:** Handles bylines buried in prose with no meta tags.
- **Cons:** Costs an LLM call, is non-deterministic, and is overkill when most
  sites expose `<meta name="author">` / JSON-LD.
- **Why not chosen:** Deterministic meta parsing first; Fabric byline is a
  documented later fallback only.

## Technical Considerations

### Dependencies
No new crates for GitHub (owner is already parsed). Blog byline parsing uses
HTML/meta parsing; prefer an existing workspace dependency (e.g. the HTML parser
already pulled in by the fetch path) over adding one - confirm during Phase 2.

### Performance
- GitHub: zero added network (owner from the URL already in hand).
- Blog: **zero added network** - the byline rides the existing fetch
  (`BrowserUaFetcher` HTML / Jina-JSON). Local parse, no LLM, no extra `GET`.
- Backfill (github): pure string parse over `source`, no network.

### Security
Author strings are untrusted page content; escape as YAML (the render already
uses `escape_yaml_string`) and cap length to avoid pathological meta values.

### Testing Strategy
Pure unit tests for `creator_for` (per variant) and `byline::extract` (per
ladder rung + negative case). Render test asserting exactly one `creator:` line.
A github-coverage re-measure after the Phase 4 backfill.

### Rollout Plan
Ship in the normal `bump && otto install` flow; restart `borg`/`cortex` so the
daemon ingests with the new render. The ledger `.base` edit is a vault file
(Syncthing-propagated). Backfill runs once on the daemon host.

Note there are **two** backfill avenues for go-forward notes: the `audit --fix`
pass (Phase 4, github only, no network), and ordinary **reingest**. Reingest
overwrites the note at its original location with a freshly rendered body +
frontmatter (only the original `date` and write location are preserved -
`pipeline.rs:457`), so a re-sent note is re-rendered through the new
`creator_for` logic. Reingest is the only way a *blog* note picks up a byline
retroactively, since the byline needs the fetch; this is why mass blog backfill
is a non-goal rather than an `audit --fix` rule.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| ContentType variant change misses a constructor | Med | Low | Compiler enforces exhaustiveness; tests per variant |
| Blog byline absent on `fabric -u` (default) ingests | High | Low | Accepted by design: `fabric -u` exposes no HTML; coverage limited to Jina-JSON + browser-UA paths; `None` is fine, reingest is the lever |
| Blog byline wrong/junk from bad meta | Med | Low | Deterministic ladder + `None` fallback; never fabricate; length cap |
| Author string breaks YAML | Low | Med | `escape_yaml_string` + length cap |
| Backfill writes wrong owner for deep github URLs | Low | Med | `parse_repo_url` returns None for non-root URLs; only fix repo roots |
| `parse_repo_url` maps pseudo-paths (`/orgs/x`, `/sponsors/x`, `gist.`) to a bogus owner | Low | Low | Small denylist of non-owner first segments in `creator_for`/backfill, or accept (rare); the body distiller already tolerates these |
| Fetcher-contract change ripples across `Fetcher` impls | Med | Low | `meta.author` is additive + `Option`; existing impls default it to `None`; one new field, compiler-checked |
| Backfill clobbers an existing hand-set `creator` | Low | Med | Fix targets only notes with an **empty** `creator`; never overwrite |

## Open Questions

Resolved during review (incl. Architect review):
- **Byline source** - Confirmed no fetcher exposes a byline today (`fabric -u`
  and jina both return markdown; `BrowserUaFetcher` fetches raw HTML but discards
  it post-markitdown). **Resolution:** surface the author from the *same* fetch
  via an additive `FetchMeta.author` - `BrowserUaFetcher` runs `byline::extract`
  on the HTML it already has; Jina-JSON sets it from the JSON; `fabric -u` leaves
  it `None`. A standalone `GET` is **rejected** (it would fire on every default
  `fabric -u` ingest and hit bot-walls the real fetchers cleared).
- **Blog backfill of the existing 286 notes** - **Resolution: go-forward only.**
  No mass re-fetch rule. Reingest is the manual, per-note lever; a rate-limited
  bulk re-fetch is rejected (network cost, bot-wall divergence, no-Rust-migration
  ethos).
- **Unify YouTube's `uploader` into `creator_for`** - **Resolution: yes, now.**
  `creator_for` returns the uploader for the `YouTube` variant; one resolution
  path for all kinds; the YouTube arm retains only `duration:`, covered by a
  render regression test.

Resolved by live investigation (2026-06-07; probes against r.jina.ai JSON +
markdown modes, `fabric -u`, and raw browser-UA HTML across 13 URLs):

- **Jina JSON field path - confirmed `data.metadata.author`** (a flat string,
  *not* `data.author`). Live: jvns.ca -> `"Julia Evans"`, simonwillison.net ->
  `"Simon Willison"`. No spike needed - the field name is settled. The Phase-2
  Jina path reads `data.metadata.author` and accepts `None` when absent.
- **Jina's `metadata.author` mirrors `<meta name="author">` only; it does NOT
  surface JSON-LD `author`.** Three independent positive cases prove this: on
  css-tricks.com (WordPress), dev.to (Forem), and ghost.org (Ghost), the raw HTML
  carries a JSON-LD author (`"Geoff Graham"`, `"Jess Lee"`, `"Team Ghost"`
  respectively) while Jina's `metadata.author` returns **`None`** for all three.
  This is the load-bearing finding for the Phase-2 architecture (see next bullet).
- **The two Phase-2 byline sources have *different, complementary* coverage - the
  doc's split is well-founded, not redundant.**
  - Jina-JSON (`metadata.author`) catches the `<meta name="author">` rung only.
    It hits personal blogs that bother to set it (jvns, simonwillison) and
    **misses every JSON-LD-only site** (the entire WordPress/Yoast + Ghost +
    Forem + most-news population).
  - `BrowserUaFetcher` + `byline::extract` over raw HTML catches `<meta>` **and**
    the JSON-LD rung (rung 2) - i.e. it recovers exactly the Geoff-Graham /
    Jess-Lee / Team-Ghost authors Jina drops. So `byline::extract`'s JSON-LD rung
    is **not** redundant with Jina; it is the rung that covers mainstream CMS
    platforms. The path that served the page determines byline coverage, and the
    bot-walled big-publication URLs (the ones `fabric -u` blocks on and that fall
    through to `BrowserUaFetcher`) are precisely the JSON-LD-rich ones the raw-HTML
    path handles best. Keep both rungs as specced.
- **Neither markdown header carries an author line - confirmed.** Jina markdown
  mode (`Accept: text/markdown`, what `jina.rs` uses today) emits only
  `Title:` / `URL Source:` / `Markdown Content:`. And **`fabric -u` is itself a
  Jina markdown scrape** (`fabric --help`: `-u, --scrape_url= Scrape website URL
  to markdown using Jina AI`); its output is byte-identical to raw Jina markdown -
  no byline. The doc's "fabric -u exposes no author" premise holds.
- **Folding the fabric path in = a scraper swap, not an extra fetch - but it
  buys only the *weaker* coverage.** Because `fabric -u` just round-trips through
  Jina Reader, routing the default article scrape through a JSON-mode `jina.rs`
  call (which already owns the r.jina.ai HTTP) returns `data.content` (same
  markdown body) **and** `data.metadata.author` from one request, zero added
  network. But per the finding above, JSON mode only gets `<meta name="author">`
  coverage - it would **not** recover the JSON-LD authors. So this swap raises the
  default-path floor modestly without matching the raw-HTML path. Still **out of
  scope** for this doc; the framing is "swap the scraper for partial gain," not
  "an extra fetch is required."
- **Correction to Alternative 3 (standalone `GET`):** the rejection stands, but
  the rationale is hot-path latency on the `fabric -u` default + bot-walls -
  **not** "a GET would recover nothing." A bot-wall-clearing fetch that ran
  `byline::extract` *would* recover JSON-LD authors (that is exactly what the
  `BrowserUaFetcher` rung does). The `GET` is rejected on cost/bot-wall grounds,
  not capability.
- **Coverage ceiling is set by BOTH page and path.** Single-author personal blogs
  frequently encode *nothing* machine-readable (overreacted.io, martinfowler.com,
  joelonsoftware.com, kentcdodds.com returned no `<meta>`, `article:author`,
  JSON-LD, or `rel=author` in raw HTML) - no path helps those. CMS publications
  encode JSON-LD author that *only the raw-HTML path* recovers. Net: best-effort,
  below YouTube's 98%, holds - but the raw-HTML path materially out-covers
  Jina-JSON on the mainstream-publication slice.

## References
- `borg/src/pipeline.rs` - classify + per-kind dispatch (`process_url_inner`)
- `borg/src/markdown.rs` - `ContentType` + `creator:` render
- `borg/src/github.rs` - `parse_repo_url`, `RepoMetadata.owner`
- `vault/src/frontmatter.rs` - `creator` field
- `cortex/src/hub.rs` - `creator` as a graph entity
- `cortex/src/migrate.rs` - the `author -> creator` rename
- `~/repos/scottidler/obsidian/system/views/borg-ledger.base` - the ledger view
