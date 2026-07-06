# Implementation Notes: Clean Article Fetch (in-process readability) + Transcript Quality Gate

Running record of how the implementation diverges from or interprets
`docs/design/2026-07-05-article-transcript-boilerplate.md`. One section per phase.

> **Pivot recorded (2026-07-05):** the branch was first implemented against the
> `defuddle` Node CLI (4 commits: primitive, daemon-reachability doctor probe,
> pipeline wiring, quality gate). The author rejected adding a Node/mise runtime to
> the Rust stack. A POC (readability 0.3 vs dom_smoothie 0.18 vs defuddle vs Jina on
> 5 real ingested URLs) showed `dom_smoothie` matches defuddle in-process, including
> the arxiv abstract where readability 0.3 whiffed. The branch was reshaped onto
> in-process `dom_smoothie` + `html2md`; the defuddle primitive, its config fields
> (`defuddle-path` / `defuddle-timeout-secs`), and the entire daemon-reachability
> phase + `sb doctor` probe were removed. These notes describe the shipped
> (dom_smoothie) design.

## Phase 1: In-process readability primitive

### Design decisions
- `borg/src/readability.rs`: `fetch_article_readable(url, timeout_secs)` does an
  async reqwest browser-UA fetch, then runs `extract_markdown` in `spawn_blocking`
  (DOM parse + markdown render are CPU-bound and sync; keeping them off the async
  runtime per `rules/rust.md`).
- `extract_markdown` is a private sync fn (`dom_smoothie` -> article HTML ->
  `html2md::parse_html`), separated so it is unit-testable over a static HTML
  fixture without network.
- Typed `ReadableError` (`Fetch` / `Status` / `Extract` / `Empty`) so the caller
  matches variants, never string-sniffs.
- Reuses `stages::fetcher::BROWSER_UA`'s UA string and reqwest config shape rather
  than routing through `BrowserUaFetcher` (that fetcher converts to markdown via
  markitdown; the readable path needs the raw HTML dom_smoothie parses).
- `READABLE_MIN_CHARS = 200` (thin floor, in `handlers.rs`, matching the transcript
  gate's floor for a consistent "not a real article" bar).

### Deviations
- None from the (reshaped) design.

### Tradeoffs
- `dom_smoothie` yields cleaned article HTML; converted to markdown with `html2md`
  (an in-process Rust crate) rather than storing plain `text_content` - preserves
  links/structure in the transcript and the distiller input, at the cost of one more
  Rust dep (no runtime). `text_content` was the lower-fidelity alternative.
- reqwest client is built per call (simple, mirrors `jina.rs`); a shared client is a
  possible later optimization, not needed for correctness.

### Open questions
- None.

## Phase 2: Wire readable preferred, with URL-kind hoist, block-check, fallthrough

### Design decisions
- `process_article_readable` (`handlers.rs`) mirrors `process_article_fabric`'s
  `(title, article_md, byline=None)` shape; reuses `pipeline.jina-timeout-secs`
  (same HTTP-scrape timeout class - no new config).
- `accept_readable_output(url, md)` (pure, unit-tested) rejects thin
  (`< READABLE_MIN_CHARS`) or `detect_block_page`-positive output as `Err` so the
  caller falls through.
- URL-kind predicates hoisted above the fetch in `process_url_inner`:
  `is_plain_article = github_repo.is_none() && !is_thread`. Readable extraction is
  tried FIRST only for plain articles; github roots / threads keep the fabric-u ->
  Jina chain; the distiller dispatch reuses the hoisted `is_thread`.
- Happy-path persist via `persist_fetched_if_staging(extractor="readable")`.
- No heavy permit (a light in-process fetch, like `jina`; only fabric takes one).

### Deviations
- Full `process_url_inner` network-injection integration test not built: the repo
  has no such harness (fabric/Jina fallthrough hits the network). The new logic is
  unit-tested at its seams (`accept_readable_output`, the `is_plain_article` gate)
  plus `extract_markdown` in Phase 1.

### Tradeoffs
- Unit-testing the seams vs. a network-injection harness: chose the seams; the
  harness is a large out-of-scope refactor and the seams capture the new logic.

### Open questions
- `READABLE_MIN_CHARS = 200` is a first cut; may want tuning against live fixtures
  in Phase 4 (Risk table).

## Phase 3: Borg-layer transcript quality gate (success + fallback)

### Design decisions
- Extended `gate_article_transcript`: toggle-off clears unconditionally; toggle-on
  additionally clears when `transcript_quality_ok` fails. One borg-layer gate on the
  single FINAL `Distilled` covers BOTH success and `fallback_distilled` (finding #1).
- Coarse heuristic: drop when `chars < MIN_TRANSCRIPT_CHARS (200)` OR the fraction of
  non-blank lines shorter than `MIN_PROSE_LINE_CHARS (40)` exceeds
  `MAX_CHROME_RATIO (0.9)`.

### Deviations
- The design phrases the ratio as "link-line-to-prose-line." Implemented as a
  SHORT-line ratio (line length `< 40`), NOT literal markdown-link detection -
  counting links would flag the link-heavy-but-legit HN roundup (the exact false
  positive criterion (c) guards against). Length distinguishes country-dropdown /
  form chrome from titled link lines. The link-heavy-legit fixture asserts this.
- Updated the existing toggle test `gate_article_transcript_on_keeps_transcript_section`
  to a clean, >200-char fixture: with the quality gate, toggle-on no longer keeps a
  pathologically-thin transcript.

### Tradeoffs
- Short-line ratio vs. markdown-link parsing: chose line length - passes
  link-dense-but-legit content while catching country-dropdown / nav walls, and is
  simpler and more robust.

### Open questions
- Thresholds (200 / 40 / 0.9) are first cuts; the Phase 4 live replay may tune them.

## Phase 4: Measure (operator/live)

Operator step, not code: `otto deploy` -> `sb borg replay tg-1a0305` -> `sb borg
eval`. No daemon config change (in-process; no `defuddle-path`). Verifies acceptance
criteria 1 and 6 (the live eval). Pending.

## Review-panel audit response (post-reshape)

Ran a cross-model implementation audit (Architect/Gemini + Staff/Codex). Three
findings, all dispositioned:

### Finding #1 (must-fix): coarse gate kept the real trainwreck on the fallback path
- Codex measured the actual `tg-1a0305/fetched.html`: 52384 chars, 1176 non-blank
  lines, 718 short -> ratio 0.611, below the 0.90 threshold, so the coarse gate KEPT
  it. AC #3 + the pipeline comment overclaimed ("never publish chrome").
- A focused second panel unanimously confirmed the fix: **source-gate the
  transcript** (option b) over reword-only (a) or an orthogonal ratio signal (c,
  rejected as re-introducing the brittleness the design avoided).
- Implemented: `gate_article_transcript(distilled, enabled, clean_source)` with
  `clean_source = readable_triple.is_some()` from the dispatch. Non-clean fetch ->
  transcript dropped. The coarse `transcript_quality_ok` is retained as
  defense-in-depth on the clean-source path only.
- **Jina deliberately NOT counted as a clean source** (panel-verified): its handler
  returns clean r.jina.ai markdown OR a raw browser-UA fallback from the same return
  type - the caller cannot distinguish them, and Jina leaked chrome on arxiv in the
  POC. Fail-closed. A provenance enum that could keep genuinely-clean Jina output is
  deferred (needs evidence dropped-fallback transcripts are a real loss).
- Test added: `gate_clears_transcript_from_non_clean_source` (clean-looking text +
  `clean_source=false` -> dropped).

### Finding #2 (fixed): `html2md` is GPL-3.0+ in an MIT repo
- Confirmed the license conflict. Swapped to `htmd` (Apache-2.0), a drop-in
  `convert(&str) -> Result<String>` that keeps markdown output. dom_smoothie is MIT.
  (Zero-dep alternative - dom_smoothie's plain `text_content` - was available but
  markdown fidelity for the transcript + distiller input won.)

### Finding #3 (fixed): Phase-1 test under-asserted
- `extract_markdown_keeps_article_and_drops_chrome` now asserts every chrome region
  in the fixture (nav Subscribe/Log in, header newsletter/cookie, footer) is
  dropped, not just the cookie banner.

### Not-a-defect (noted)
- A pre-existing flaky test `stages::alert::tests::first_alert_fires_then_is_suppressed`
  failed once during this work: `should_alert` uses a process-wide
  `static OnceLock<Mutex<CooldownMap>>` and the alert tests are not serialized, so a
  parallel test hitting the `xda-developers.com` block-page path pollutes the shared
  map. Unrelated to this change (passes on re-run); flagged for separate cleanup
  (serialize the alert tests or use per-test domains).
