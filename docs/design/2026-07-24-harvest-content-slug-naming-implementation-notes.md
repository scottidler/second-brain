# Implementation notes: content-derived slug naming for harvest notes

Companion to `2026-07-24-harvest-content-slug-naming-handoff.md`. Append-only.

## Phase 1: slug in the contract + pattern

### Design decisions
- `vault::distilled::Distilled.slug: Option<String>` — `#[serde(default, skip_serializing_if = "Option::is_none")]` so legacy staged `distilled.yml` (no `slug:` key) still deserializes and a `None` slug never serializes.
- `distillers::session::clean_slug` — trims + lowercases only; filename-safety (illegal chars, length) is deferred to the publish path's `hygiene::sanitize_filename` so the raw subject stays intact in the contract.
- Slug added to both `PatternYaml` (single-call) and `ReduceYaml` (reduce) parse leaves; the chunk pattern does NOT get a slug (the reduce pass names the whole).

### Deviations
- None.

### Tradeoffs
- Slug on the shared `Distilled` (per the handoff) vs a session-only field — chose the handoff's contract field. Cost: ~27 exhaustive `Distilled { .. }` literals across every kind + tests needed `slug: None` (applied compiler-driven). Benefit: one contract, uniform render/parse.

### Open questions
- None.

## Phase 2: publish path uses the slug

### Design decisions
- `borg::pipeline::session::harvest_slug_stem(slug, title) -> (stem, used_title_fallback)` — pure, testable; caller WARNs on the title-slug fallback. Both branches pass through `hygiene::sanitize_filename`.
- The chosen stem is persisted as frontmatter `slug:` for cross-harvest stability and to give the Phase 3 association check something to match on.

### Deviations
- None.

### Tradeoffs
- Persist the sanitized stem (matches the filename) vs the raw LLM slug — chose the sanitized stem so frontmatter `slug:` and the filename never diverge (a field that says one thing and means another is the "cognitive dissonance" this repo forbids).

### Open questions
- None.

## Phase 3: collision = association

### Design decisions
- **Architecture: cortex owns the association; borg only names deterministically.**
  The handoff sketched modifying `resolve_publish_path` in borg, but Scott's two
  decisions — "per-similarity" and "threshold tunable in `cortex.yml`" — put the
  merge/cross-link decision in cortex, which is the vault governor and the only
  holder of embeddings (so "embedding OR claim overlap" can be computed there).
  cortex already has `cortex::duplicates` (TF-IDF cosine + `threshold`), so the
  association is an extension of existing machinery, not a new subsystem.
- **borg (this commit): `harvest_publish_path`** replaces the shared
  `atomic::resolve_publish_path` `-N` suffixer FOR HARVEST ONLY (other content
  kinds keep `-N`). On a content-slug collision it uses a deterministic suffix
  derived from the primary session id (`{slug}--{first-8-of-uuid}.md`), never the
  order-dependent `-N`. `force` overwrites the bare-slug note in place.
- The residual "which of two same-slug sessions gets the bare slug" is left to
  cortex's association sweep; the filename is display/addressing only, the
  identity anchor stays the session id + receipts DB.

### Deviations
- **Config home: `cortex.yml` (honored) but the ACTOR is cortex, not borg.** The
  handoff located the mechanism in borg's publish path; this implementation moves
  the association to cortex to honor Scott's `cortex.yml` steer and match cortex's
  governance role. borg's role shrinks to deterministic collision-free naming.

### Tradeoffs
- Deterministic session-suffix on collision vs always-suffixing every harvest
  note with a hash — chose bare-slug-when-free so the common (no-collision) case
  keeps a clean subject filename; only genuine collisions get the suffix.
- Claim/TF-IDF similarity (cortex `duplicates`) as the first association signal
  vs embedding cosine — cortex has both; the existing `duplicates` TF-IDF path is
  the cheapest correct start and is already threshold-driven. Embedding-based
  refinement can ride the same threshold later.

### Open questions
- Cortex association sweep (group same-base-slug notes, merge-vs-cross-link by
  similarity, union claims + `cortex-session-ids` surgery) is the larger second
  half of Phase 3 and is NOT in this commit — it is the next focused piece.
- Confirm `cortex.yml` (vs `borg.yml`) as the threshold home now that the actor
  is cortex — this implementation assumes `cortex.yml`, consistent with the
  existing `duplicates.threshold`.

## Phase 4: regenerate the existing notes

### Design decisions
- Deferred: the ~121 existing notes have NO `slug` frontmatter (distilled before
  this feature), so a pure rename-only migration cannot produce slugs. They
  require re-distillation, which is an LLM-cost, live-vault mutation and must run
  only AFTER `otto deploy` ships the new patterns. This is a deploy + irreversible
  live-vault action, gated behind the finalization approval checkpoint.

### Deviations
- None yet.

### Tradeoffs
- None yet.

### Open questions
- Full 60d re-distill (`sb borg harvest --since 60d --force`) vs a targeted
  slug-only backfill — pending the deploy/approval checkpoint.
