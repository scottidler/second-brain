# Implementation Notes: GitHub Repos from YouTube Video Descriptions

Running, append-only record of how the implementation interprets or diverges
from `docs/design/2026-06-08-github-repos-from-video-description.md`.

## Phase 1: Extractor + data model

### Design decisions
- Left-boundary regex instead of host parsing — `borg/src/github.rs::REPO_SLUG_RE`
  — the `regex` crate has no lookbehind, so the host-exclusion rules (rule 2)
  are enforced by requiring the char before `github.com` to be start-of-text or
  a non-host-label char `[^a-z0-9._-]`. This rejects `gist.github.com`,
  `docs.github.com`, `notgithub.com`, and `github.com.evil.com` in one mechanism
  rather than enumerating subdomains. `raw.githubusercontent.com` never matches
  the literal `github.com/` token at all.
- Path capture stops at brackets/quotes/angles `[^\s()\[\]{}<>"']+`
  (`borg/src/github.rs::REPO_SLUG_RE`) so adjacent prose-glued URLs split
  cleanly; remaining trailing `.`/`,`/`/` are removed by `TRAILING_NOISE`
  trimming in `slug_from_path`. This satisfies rule 4 without a second regex.
- `.git` strip is sandwiched between two `TRAILING_NOISE` trims in
  `slug_from_path` so `repo.git).` -> `repo` works (trim closers, strip `.git`,
  trim again).
- `RESERVED_OWNERS` is a `const &[&str]` (rule 5 denylist verbatim from the
  doc), checked lowercased. Kept as a flat slice — the per-entry unit test
  iterates it directly so the test cannot drift from the list.

### Deviations
- None. The extractor rules, struct fields, and `#[serde(default)]` on
  `VideoPayload.repos` match the doc exactly.

### Tradeoffs
- Boundary-consuming regex vs. tokenize-then-parse — chose the single regex with
  a consumed boundary group over splitting the description into whitespace
  tokens and parsing each. The regex handles deep paths, query/fragment, and
  prose punctuation in one pass and is unit-tested per rule; tokenizing would
  re-implement URL-span detection.
- `repos: Vec<String>` (not `Option<Vec<String>>`) mirrors `RepoPayload.topics`
  — empty vec is the natural "none found" and renders no key, so an `Option`
  wrapper buys nothing.

### Open questions
- None for this phase. (Doc-level open questions on slug-vs-URL form, backfill,
  the double-fetch refactor, and the `github` field name remain as recorded in
  the design doc; none block implementation.)

## Phase 2: Wiring + render

### Design decisions
- The seam assignment uses `&metadata.description` directly
  (`borg/src/stages/distill.rs::distill_for_publish_video`). `description` is a
  non-optional `String` on `borg::youtube::VideoMetadata` (defaults to `""`),
  so no `Option` handling is needed — an empty description yields an empty slug
  list and `attach_payload`'s guard keeps the payload absent.

### Deviations
- None. Seam assignment, the `attach_payload` guard extension
  (`&& m.repos.is_empty()`), and the `github:` render block in `render.rs`
  match the doc's snippets verbatim. `video_metadata_from_yt_dlp` remains a
  pure mapper.

### Tradeoffs
- None beyond Phase 1's. The wiring is the one-seam change the doc describes.

### Open questions
- None.

## Phase 3: Integration tests + cleanup

### Design decisions
- `attach_payload` is tested directly (not only through `distill`) — it is
  private but reachable via `use super::*` in `distillers/src/video/tests.rs`.
  A `crate::fallback_distilled(...)` provides a `Distilled` with
  `kind_specific: None` to mutate, so the test asserts both the repos-only
  attach path and the all-empty skip path without standing up a FakeFabric run.

### Deviations
- None. The render-level test (two repos -> `github:` sequence; empty -> no
  key) and the `attach_payload` repos-only test match the Phase 3 spec. Added a
  complementary `attach_payload_skips_when_all_fields_empty` test to pin the
  guard's negative case.

### Tradeoffs
- None.

### Open questions
- None. The four doc-level open questions (slug vs URL, backfill, double-fetch
  refactor, field name) were all resolved in the doc's recommendations and are
  implemented as specified; none surfaced new questions during implementation.
