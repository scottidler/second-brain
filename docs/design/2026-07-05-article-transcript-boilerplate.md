# Design Document: Clean Article Fetch (in-process readability) + Transcript Quality Gate

**Author:** Scott Idler
**Date:** 2026-07-05
**Status:** In Review
**Review Passes Completed:** 2/5 (draft + correctness; clarity/edge-cases/review-panel pending)

> **Supersedes the defuddle approach.** An earlier draft of this design shelled the
> external `defuddle` Node CLI (installed under mise) as the preferred extractor.
> That was rejected by the author: it adds a scripting-language runtime (Node via
> mise) to a Rust stack, and it created a daemon PATH-reachability problem. This
> revision uses an **in-process Rust readability crate** (`dom_smoothie`) instead.
> The defuddle rationale is preserved in Alternatives + Resolved Decisions so it is
> not re-litigated.

## Summary

Borg's article fetch returns the whole rendered page as markdown (subscribe forms,
country dropdowns, nav, footer), with the real article buried in the tail. Phase 7
of the distillation overhaul stores that raw markdown verbatim as the note's
`## Transcript`, producing notes that are ~90% boilerplate. This design adds
**in-process readability extraction** (`dom_smoothie` HTML→article + `html2md`
HTML→markdown, no external binary or runtime) as the preferred article extractor,
falling back to the existing fabric-u/Jina/browser-UA chain on failure/thin/block,
plus a **coarse transcript quality gate** at the borg layer that fails closed to
no-transcript across BOTH the success and fallback distill paths.

## Problem Statement

### Background

The distillation overhaul (`docs/design/2026-07-05-distillation-knowledge-extraction.md`,
shipped v0.9.0) added durable article content: Phase 7 populates
`Distilled.transcript` with the fetched article markdown and renders it as
`## Transcript` so the text survives 60-day staging retention. Article URLs are
fetched by `fabric -u` (primary) or Jina Reader (fallback), both returning markdown.

### Problem

For subscription-gated / chrome-heavy sources, the fetched markdown is
overwhelmingly page furniture. Verified on `tg-1a0305`
(`~/.local/share/sb/borg/stages/tg-1a0305/`): the `fabric-u` fetch is 52,512 bytes;
the real article begins ~84% through. The rest is subscribe forms, a full country
dropdown (`Afghanistan`, `Albania`, ...), nav, footer. The published note is 1948
lines, ~1735 boilerplate.

The same `article_md` string is used twice: as the distiller input (Phase 6
map-reduce wastes tokens digging the article out of 84% junk) and as the stored
transcript (the trainwreck). Root cause is fetch quality: `fabric -u` (the primary)
does no readability extraction. The only extractor in the existing chain that does
is Jina, and it is only the *fallback*.

### Goals

- Article `## Transcript` contains the article, not the page chrome.
- The distiller *input* is the clean article too (better extraction, fewer wasted tokens).
- A chrome-heavy, block-page, or failed fetch can never store boilerplate as a
  transcript, on ANY distill path (success or fallback).
- Preserve the durability feature (article text in-note); fix forward.
- No regression on sites the existing chain already handles.
- The fix actually runs on the production daemon.
- **No new language runtime.** Extraction is in-process Rust, not a shelled
  scripting-language CLI.

### Non-Goals

- **Video/voicenote/thread/image transcripts** - untouched; article-only.
- **A remote extraction API as the primary path** - keeping extraction local
  (privacy: article URLs are not sent to a third party by default). Jina remains
  only as an existing fallback.
- **The `distill.article-transcript` on/off toggle** (shipped on main,
  `pipeline.rs::gate_article_transcript`) - orthogonal; this design makes the
  transcript *clean* when stored, and the quality gate lands in the SAME helper.
- **Pre-clean (raw-HTML) forensic capture** - the readable path persists its
  markdown output through the existing seam, byte-symmetric with fabric-u/jina; no
  fetcher retains pre-flatten HTML.
- **A general HTML-cleaning framework** - two focused crates (`dom_smoothie`,
  `html2md`), in-process.

## Proposed Solution

### Overview

Three composing changes, article-only:

1. **In-process readability extraction, preferred for PLAIN article URLs only**
   (after the github/thread/youtube URL-kind predicates are hoisted above the
   fetch), falling through to the existing `fabric -u` -> Jina -> browser-UA chain
   on fetch failure, non-2xx, too-thin output, or a detected block/consent page.
   Extraction fetches the HTML itself (browser UA), so it is
   preferred-with-fallback, never a replacement.
2. **Block-page validation of the extracted output** via the existing
   `detect_block_page` / Gate-1 check before acceptance; a detected block page is
   treated as failure -> fallthrough.
3. **Source gate + coarse quality gate at the borg layer**, applied to the FINAL
   `Distilled.transcript` for articles (covering both the success and
   `fallback_distilled` paths), co-located with the existing toggle gate. The
   **source gate** is load-bearing: a transcript is stored ONLY when the in-process
   readable extractor produced it; any fallthrough (fabric-u/Jina/browser-UA) is raw
   non-readability markdown and its transcript is dropped. The **coarse quality
   gate** (min-length OR generous chrome ratio) then runs as defense-in-depth
   against a readable mis-extraction. Below the bar -> transcript cleared (no
   `## Transcript`), the origin URL remaining the recoverable archive.

### Architecture

```
article URL
  pipeline.rs::process_url_inner: URL-kind predicates HOISTED above the fetch
    github root?  -> repo path (no readable extraction)
    thread (X/Reddit/HN)? -> thread path (no readable extraction)
    youtube? -> youtube path (dispatched earlier)
    else (plain article):
      process_article_readable   [preferred]
        readability::fetch_article_readable(url, jina_timeout_secs)
          reqwest browser-UA fetch -> HTML
          spawn_blocking(dom_smoothie extract -> html2md)  [CPU-bound off the runtime]
        accept_readable_output: thin (< READABLE_MIN_CHARS) OR detect_block_page -> Err
        Err/thin/block -> log::warn! reason + fall through to:
      -> process_article_fabric -> process_article_jina -> browser-UA  [existing]
    persist_fetched_if_staging(extractor="readable"): raw.rs
      -> writes returned markdown to <trace>/fetched.html + fetched.yml (same seam)
  article_md feeds distill_for_publish_article
    -> distiller input (Phase 6 map-reduce; now clean)
    -> ONE final Distilled (success: article.rs; fallback: validate.rs)
  BORG LAYER, article-only (gate_article_transcript, pipeline.rs)  [finding #1]
    clear distilled.transcript when:
        distill.article-transcript == false      (existing toggle)
      OR clean_source == false                   (SOURCE gate: not from readable extractor)
      OR transcript_quality_ok() == false        (coarse defense-in-depth on clean source)
    -> covers success AND fallback: borg gates the ONE final Distilled
    -> the SOURCE gate (not the coarse ratio) is what guarantees no fallback chrome
  distillers render: ## Transcript emitted only when transcript is Some
```

House invariants preserved: borg owns fetch/extraction and the transcript decision
(the quality gate is borg-layer, distillers stay unaware and LLM-free); article-only.

### Data Model

- **No new config fields.** The readable fetch reuses `pipeline.jina-timeout-secs`
  (same HTTP-scrape timeout class). There is no `defuddle-path` / subprocess timeout
  because there is no subprocess (contrast the rejected defuddle design).
- **`borg/src/readability.rs` `ReadableError`** (typed enum): `Fetch`, `Status`,
  `Extract`, `Empty` - the caller matches variants, never string-sniffs.
- **Stage-dir artifact.** The readable handler writes through the EXISTING seam
  `persist_fetched_if_staging`: the returned markdown lands in `<trace>/fetched.html`
  plus a `fetched.yml` sidecar recording `extractor: readable`. Byte-symmetric with
  how fabric-u/jina persist.
- The quality gate is a pure function over the final transcript markdown; no new
  persisted state.

### API Design

- `borg/src/readability.rs` (new):
  - `fetch_article_readable(url, timeout_secs) -> Result<String, ReadableError>` -
    reqwest browser-UA fetch, then `spawn_blocking(extract_markdown)` (DOM parsing +
    markdown render are CPU-bound and sync; never on the async runtime). Typed `Err`
    on non-2xx, extraction failure, or empty output.
  - `extract_markdown(html, url) -> Result<String, ReadableError>` (sync, private) -
    `dom_smoothie::Readability::new(html, Some(url), None)?.parse()?` -> article HTML
    -> `html2md::parse_html`. Separated so it is unit-testable over a static HTML
    fixture without network.
- `borg/src/pipeline/handlers.rs`:
  - `process_article_readable(...)` mirroring `process_article_fabric`'s
    `(title, article_md, byline=None)` shape; block-page + thin checks via
    `accept_readable_output`; persists via `persist_fetched_if_staging(extractor="readable")`.
  - `accept_readable_output(url, md) -> Result<String>` - too-thin
    (`< READABLE_MIN_CHARS`) or `detect_block_page` -> `Err` (fall through). Pure,
    unit-tested.
- `borg/src/pipeline.rs::process_url_inner`: URL-kind predicates hoisted above the
  fetch; `is_plain_article = github_repo.is_none() && !is_thread`; readable dispatched
  only for plain articles; fallthrough on err/thin/block with a `log::warn!`.
- `borg/src/pipeline.rs::gate_article_transcript` (borg-layer helper, on `main`):
  extended so the article transcript is also cleared when `transcript_quality_ok`
  returns false. Runs on the FINAL Distilled, covering the fallback path too.

### POC Evidence (Phase 0)

5 real ingested URLs, extracted-content bytes; ✓ = clean article, ✗ = failed:

| Source | raw | `readability` 0.3 | **`dom_smoothie` 0.18** | defuddle (Node) | Jina (remote) |
|---|--:|--:|--:|--:|--:|
| arxiv abs | 42 KB | 70 B ✗ (title only) | 1954 B ✓ | 1982 B ✓ | 9301 B + chrome |
| arstechnica | 161 KB | 2585 B ✓ | 10982 B ✓ | 10858 B ✓ | 13064 B ✓ |
| cloudflare blog | 287 KB | 8157 B ✓ | 10213 B ✓ | 9978 B ✓ | 12347 B ✓ |
| substack | 326 KB | 19465 B ✓ | 19473 B ✓ | 26752 B ✓ | 24807 B ✓ |
| jetbrains blog | 143 KB | 10285 B ✓ | 12278 B ✓ | 14403 B ✓ | 12398 B ✓ |

`dom_smoothie` matches defuddle on all five including arxiv (where `readability` 0.3
whiffed and Jina leaked chrome). It is the in-process, no-runtime winner.

### Implementation Plan

Ordering: prove a Rust crate matches defuddle (done) -> in-process primitive ->
wire preferred+fallback+block-check -> borg-layer quality gate -> measure.

#### Phase 0: Prove an in-process Rust readability crate matches defuddle
**Model:** opus
- DONE. Built a throwaway harness comparing `readability` 0.3, `dom_smoothie` 0.18,
  defuddle, and Jina on 5 real ingested URLs (POC Evidence table).
- **Success criteria:** MET - `dom_smoothie` extracts the arxiv abstract (1954 B,
  matching defuddle's 1982 B) where `readability` 0.3 returned title-only (70 B), and
  is clean + comparable to defuddle on the other four.

#### Phase 1: In-process readability primitive
**Model:** opus
- `borg/src/readability.rs`: `fetch_article_readable` (async reqwest browser-UA
  fetch + `spawn_blocking` extraction) + `extract_markdown` (dom_smoothie ->
  html2md) + typed `ReadableError`. Add `dom_smoothie` + `html2md` to borg deps.
- **Success criteria:** (a) `extract_markdown` over a chrome-wrapped HTML fixture
  keeps the article body and drops nav/header/footer chrome (asserted); (b) output
  is smaller than the raw page; (c) contentless input errors or returns empty.

#### Phase 2: Wire readable preferred, with URL-kind hoist, block-check, fallthrough
**Model:** opus
- Hoist github/thread/youtube URL-kind predicates ABOVE the fetch; dispatch
  `process_article_readable` FIRST only for plain article URLs; on `Err`, output
  below `READABLE_MIN_CHARS`, or a `detect_block_page`-positive result, `log::warn!`
  the reason and fall through to `fabric -u` -> Jina -> browser-UA. Persist via the
  existing seam (`extractor: readable`).
- **Success criteria:** (a) `accept_readable_output` accepts a clean article and
  rejects thin + block-page inputs (fall through); (b) a GitHub root URL and an
  X/Reddit/HN thread URL NEVER invoke readable extraction (asserted on the exact
  `is_plain_article` gate).

#### Phase 3: Borg-layer transcript source gate + coarse quality gate (success + fallback)
**Model:** opus
- Extend `gate_article_transcript(distilled, enabled, clean_source)`. Order:
  toggle-off drops unconditionally; **`clean_source == false` drops** (the SOURCE
  gate - only a transcript the in-process readable extractor produced is stored;
  any fallthrough is dropped); otherwise run the coarse `transcript_quality_ok` as
  **defense-in-depth**. `clean_source = readable_triple.is_some()` from the
  dispatch. **Keep the coarse gate COARSE** - a pathological-input circuit breaker
  (min-length OR a generous short-line ratio; length not link-detection, so a legit
  link-dense roundup passes) - but it is NOT what guarantees "no fallback chrome";
  the source gate is (the coarse ratio alone keeps the 0.61-short-line trainwreck).
  Borg-layer placement on the final Distilled covers both distill paths.
- **Success criteria:** (a) a clean-looking transcript from a NON-clean source ->
  cleared (source gate); (b) clean readable extraction -> kept; (c) link-heavy-but-
  legit clean extraction -> KEPT (false-positive guard); (d) a chrome/too-short
  clean-source transcript -> cleared (coarse gate); (e) toggle off -> cleared.

#### Phase 4: Measure article extraction quality
**Model:** opus
- **Operator step:** `otto deploy`, then a live re-distill + `sb borg eval`.
  Re-clean the trainwreck with `sb borg replay tg-1a0305`. No daemon config change
  needed (in-process; no `defuddle-path` to set).
- **Success criteria:** article coverage holds-or-beats the recorded 1.200 baseline
  across the fixture replay; no increase in fetch failures.

## Acceptance Criteria

- [ ] Ingesting the `tg-1a0305` URL produces a note whose `## Transcript` is small,
  contains the known article sentence, and contains NO country-dropdown/`Afghanistan`
  boilerplate (Phases 1-3).
- [ ] A URL where readable extraction errors, returns `< READABLE_MIN_CHARS`, or
  returns a detected block page still publishes via the existing
  fabric-u/Jina/browser-UA chain, with the fallthrough reason logged (Phases 1-2).
- [ ] A `## Transcript` is stored ONLY when the in-process readable extractor
  produced it (source gate); a transcript from ANY fallthrough (fabric-u/Jina/
  browser-UA) is dropped on BOTH the success and `fallback_distilled` paths, so a
  chrome-heavy fallback page is never published; the note still carries
  Summary/Claims. A clean readable extraction is kept unless it trips the coarse
  defense-in-depth gate; a link-heavy-but-legit clean extraction is KEPT (Phase 3).
- [ ] A GitHub root URL and an X/Reddit/HN thread URL never invoke readable
  extraction (article-only preserved) (Phase 2).
- [ ] No new language runtime is introduced; extraction is in-process Rust
  (`dom_smoothie` + `html2md`), no subprocess, no `sb doctor` reachability probe.
- [ ] `sb borg eval` article coverage holds-or-beats the 1.200 baseline (Phase 4).
- [ ] Video/voicenote/thread/image transcripts are byte-identical to pre-change
  output (article-path-scoped changes).

## Resolved Decisions

- **2026-07-05 - In-process Rust extraction, not a shelled Node CLI (defuddle).**
  defuddle adds a scripting-language runtime (Node via mise) to a Rust stack and a
  daemon PATH-reachability problem (mise dir absent from the systemd PATH). Rejected
  by the author. An in-process Rust crate has no runtime, no subprocess, and no PATH
  problem - it also deletes the daemon-reachability phase entirely. (Author.)
- **2026-07-05 - `dom_smoothie` over `readability` 0.3.** POC: `readability` 0.3
  extracted only the title on arxiv (70 B, missing the abstract); `dom_smoothie`
  handled it (1954 B), matching defuddle, and matched defuddle on the other four
  sources. (Author + POC.)
- **2026-07-05 - Not Jina-primary.** Jina is remote (every article URL would be sent
  to r.jina.ai) - a privacy regression for a personal second-brain - AND in the POC
  it leaked the most chrome on arxiv. Kept only as the existing fallback. (Author + POC.)
- **2026-07-05 - Not a compiled standalone binary either.** A runtime-free Go/Rust
  binary still means a subprocess + PATH + the daemon-reachability problem. An
  in-process crate is strictly better for the no-runtime constraint. (Author.)
- **2026-07-05 - Quality gate at the borg layer on the FINAL Distilled** (finding
  #1): the `fallback_distilled` path bypasses the distiller's own transcript path, so
  gating in `gate_article_transcript` covers success AND fallback. (Author, per prior panel.)
- **2026-07-05 - Transcript is SOURCE-gated; the coarse ratio is defense-in-depth
  only.** Post-implementation audit measured the real `tg-1a0305` fallback page at a
  0.61 short-line ratio - below the generous 0.90 threshold - so the coarse gate
  alone KEEPS it, contradicting "never publish chrome." Fix: store a transcript only
  when the in-process readable extractor produced it (`clean_source =
  readable_triple.is_some()`); drop it on any fallthrough. Jina does NOT count as a
  clean source - its handler returns clean r.jina.ai markdown or a raw browser-UA
  fallback from the same return type, indistinguishable to the caller (and Jina
  leaked chrome on arxiv in the POC). The coarse `transcript_quality_ok` is retained
  as defense-in-depth against a readable mis-extraction, not as the chrome guarantee.
  (Author, per unanimous review-panel consensus on the reshape audit.)
- **2026-07-05 - Extracted output validated with `detect_block_page`** before
  acceptance (finding #4): a bot-wall/consent body must fall through, not publish. (Author, per prior panel.)
- **2026-07-05 - URL-kind predicates hoisted above the fetch** (finding #5): readable
  extraction is gated to plain articles; github/thread keep the existing chain. (Author, per prior panel.)
- **2026-07-05 - Coarse gate is a circuit breaker, not a chrome classifier.**
  Min-length OR a generous short-line ratio; a link-heavy-legit fixture is a required
  false-positive guard. (Author, per prior panel.)

## Alternatives Considered

### Alternative 1: Shell the `defuddle` Node CLI (the original design)
- **Cons:** adds Node/mise runtime to a Rust stack; daemon PATH-reachability problem
  requiring a `defuddle-path` config + `sb doctor` probe.
- **Why not chosen:** author rejected the runtime dependency; `dom_smoothie` matches
  its quality in-process (POC).

### Alternative 2: Make Jina the primary fetcher (no new extractor)
- **Cons:** remote API for every article (privacy); dependency on a third-party
  service being up / within rate limits; leaked the most chrome on arxiv in the POC.
- **Why not chosen:** privacy regression + a quality regression on a named source.

### Alternative 3: `readability` 0.3 crate
- **Why not chosen:** arxiv blind spot (title-only, missed the abstract).

### Alternative 4: Post-fetch markdown cleaning / revert Phase 7 / quality-gate-only
- **Why not chosen:** chrome is flattened into the fetched markdown (readability needs
  HTML); reverting abandons the durability feature; the gate alone drops rather than
  cleans and does nothing for distiller-input quality. The gate is kept as one layer.

## Technical Considerations

### Dependencies
- `dom_smoothie` (pure-Rust Mozilla-Readability port) + `html2md` (HTML->markdown).
  Both are in-process Rust crates (`cargo add`); zero new language runtimes, zero
  subprocesses. Jina/fabric/markitdown fallbacks unchanged.

### Performance
- One extra in-process extraction on the article happy path: an HTTP fetch (bounded
  by `jina-timeout-secs`) plus CPU-bound DOM parse + markdown render on a blocking
  thread. On success it REPLACES the fabric-u fetch and shrinks the distiller input
  (~52 KB -> ~4-20 KB measured), reducing Phase 6 cost. Fallthrough adds latency
  only on a miss.

### Security
- The readable fetch uses reqwest with an explicit timeout, a browser UA, and a
  bounded redirect policy. Extraction is in-process (no subprocess, no shell). It
  fetches an operator-supplied URL (same trust boundary as the existing fetch).

### Testing Strategy
- Unit: `extract_markdown` over a chrome-wrapped HTML fixture (article kept, chrome
  dropped); `accept_readable_output` ok/thin/block; the `is_plain_article` gate for
  github/thread/plain URLs; the borg-layer quality gate across clean / chrome / link-
  heavy-legit / short / toggle-off.
- Integration: full `process_url_inner` network-injection is out of scope (no harness
  exists); the new logic is covered at its decision seams.
- Eval: `sb borg eval` article coverage before/after (Phase 4 gate).

### Rollout Plan
- Single repo, on `fix-article-transcript-boilerplate`; phases land as individual
  commits. The design doc + implementation notes land WITH the code (author
  preference). Phase 4 requires `otto deploy` then re-distill + eval and
  `sb borg replay tg-1a0305` - **no daemon config change** (in-process).

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| dom_smoothie extracts poorly on some site class | Med | Med | preferred-with-fallback: thin/block/err falls through to the existing chain; POC covered arxiv/news/blog/substack |
| Extracted body is a bot-wall/consent page | Med | Med | `detect_block_page` on the output + borg-layer quality gate |
| Quality gate false-positive eats a link-heavy-legit article | Med | Med | generous short-line ratio; link-heavy-legit fixture is a required assert |
| Too-thin threshold drops real short articles | Med | Low | tune `READABLE_MIN_CHARS` on fixtures; fallthrough still fetches |
| Blocking DOM parse starves the async runtime | Low | Med | extraction runs in `spawn_blocking`, off the runtime |
| html2md renders an ugly transcript | Low | Low | dom_smoothie yields clean article HTML; html2md over clean HTML is well-behaved; tune or swap crate if needed |

## Open Questions

None. The runtime-dependency objection is resolved by the in-process crate; the
extractor choice is settled by the POC; the bot-wall and fallback concerns are closed
by the block-page check + fallthrough + quality gate.

## References

- `docs/design/2026-07-05-distillation-knowledge-extraction.md` - parent (Phase 7)
- `~/.local/share/sb/borg/stages/tg-1a0305/` - the trainwreck trace
- `borg/src/readability.rs` (in-process extractor), `borg/src/pipeline/handlers.rs`
  (`process_article_readable`, `accept_readable_output`), `borg/src/pipeline.rs`
  (URL-kind hoist, `gate_article_transcript`, `transcript_quality_ok`)
- `borg/src/stages/fetcher.rs` (`BrowserUaFetcher`, `BROWSER_UA`),
  `borg/src/stages/classify.rs` (`detect_block_page`),
  `borg/src/stages/raw.rs` (`persist_fetched_if_staging`, `is_thread_url`)
- POC harness: `readability` 0.3 vs `dom_smoothie` 0.18 vs defuddle vs Jina (5 URLs)
- `rules/rust.md` (subprocess/async hygiene, in-process preference), `rules/taste.md`
  (fix forward; copy in-house precedent; no scripting runtime)

## Addendum

### Rejected drafts / deferred options
- Shell the `defuddle` Node CLI (original design) - rejected; Node/mise runtime +
  daemon PATH problem. All defuddle-specific machinery (the `defuddle-path` /
  `defuddle-timeout-secs` config and the `sb doctor` reachability probe) was removed
  in the reshape.
- Jina-primary - rejected; remote/privacy + arxiv chrome leak.
- `readability` 0.3 crate - rejected; arxiv blind spot.
- Compiled standalone binary - rejected; subprocess + PATH + daemon-reachability, all
  avoided by an in-process crate.
