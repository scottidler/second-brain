# Implementation Notes: cortex association sweep

Design doc: `docs/design/2026-07-24-cortex-association-sweep.md`

## Phase 1: Config (fail-closed) + grouping + shared sim primitives

### Design decisions

- **Loader fail-closed** — `cortex/src/config.rs::Config::load_inner` — a
  PRESENT-but-unparseable primary config (`~/.config/sb/cortex.yml`) now
  propagates `load_from_file`'s error via `.context(...)` instead of
  `log::warn!`-and-fall-back-to-defaults. Only a MISSING file still defaults.
  The explicit `--config <path>` branch already hard-errored; unchanged.
- **`AssociationConfig` + `SimilaritySource`** — `cortex/src/config.rs` — new
  struct under `actions.association`, `#[serde(deny_unknown_fields)]`, mirrors
  `DuplicatesConfig`'s shape (`threshold: f64` default 0.85, `exclude:
  Vec<String>`) plus `similarity_source: SimilaritySource` (enum
  `Embedding | Claim | Both`, default `Both`) and `min_quiescence_secs: u64`
  default 600. Wired into `ActionsConfig` as `pub association:
  AssociationConfig` (auto-derives `Default` since every field implements it).
- **`group_by_slug`** — `cortex/src/association.rs` — new module, pure
  function `group_by_slug(notes: &[Note]) -> Vec<Vec<usize>>`. Keys on
  `frontmatter.extra["slug"]`, scoped to `note_type == "session"`
  (`NoteType::Session.as_str()`, schema-is-law), skips notes carrying a
  `superseded-by` extra key (tombstones) and notes with no `slug`, drops
  groups with fewer than 2 members. Uses a `BTreeMap<String, Vec<usize>>`
  keyed on the slug string so output group order is deterministic
  (slug-sorted), independent of `notes`' scan order.
- **`AssociateOpts`** — `cortex/src/opts.rs` — `{ apply: bool }`, matching the
  `HubOpts`/`GraphOpts` shape (dry-run default, `--apply` to write). No CLI
  wiring yet (Phase 5).
- **Promoted `duplicates::{tokenize, cosine_similarity}`** —
  `cortex/src/duplicates.rs` — changed from private `fn` to `pub(crate) fn`
  so `association`'s future claim-text similarity fallback (Phase 2) can call
  them without duplicating the TF-IDF logic. No behavior change.
- **`SearchIndex::cosine_between`** — `vault/src/search/vector.rs` — new
  method: exact pairwise cosine similarity between two notes' `kind=summary`
  embeddings for the active model. Reads exactly the two named rows (via a
  new private helper `read_summary_embedding_bytes`) and dot-products them
  directly — no top-k, so it cannot be crowded out by unrelated high-
  similarity notes elsewhere in the vault, unlike `semantic_neighbors`.
  Returns `Ok(None)` when either note lacks a summary embedding, or when the
  two stored vectors have mismatched byte lengths (logged as a warning,
  treated as uncomputable rather than erroring, matching the design's
  fail-safe "uncomputable -> below-threshold" contract for Phase 2).

### Deviations

- **`cosine_between` signature: `Result<Option<f32>>`, not the doc's literal
  `Option<f32>`** (same effect, correct seam). Every other embedding reader in
  `vault::search` (including `semantic_neighbors`, which this primitive is
  modeled on) returns `Result<...>` because reading `note_embeddings` is a
  fallible SQLite call (`active_embedding_model()` alone can error if the
  `embedding_config` seed row is missing). Collapsing that to a bare
  `Option<f32>` would mean swallowing a real DB error as "uncomputable,"
  which is the wrong signal for a caller (a broken index should not silently
  degrade every pair to cross-link). Only embedding-*presence* collapses to
  `Option`; the fallible-I/O layer stays `Result`-wrapped, consistent with
  every sibling reader in the file. Phase 2's `decide()` (which calls this)
  should propagate the `Result` and treat `Ok(None)` as the "uncomputable"
  case the design's fail-safe rule describes.
- **`AssociationConfig` field/enum placement: `cortex/src/config.rs`, not a
  separate module.** The doc doesn't specify where `SimilaritySource` lives;
  every other per-action config struct (`DuplicatesConfig`, `GraphConfig`,
  `EntitiesConfig`, etc.) lives inline in `config.rs`, so `AssociationConfig`
  and its `SimilaritySource` enum follow that precedent rather than
  introducing a new location.
- **Promotion via `pub(crate) fn` in place, not a new `cortex::sim` module.**
  The doc offered both as acceptable ("or extract `cortex::sim`"); `pub(crate)`
  is the smaller diff and keeps the TF-IDF primitives next to their only other
  caller (`duplicates.rs`'s own fuzzy-match pass), consistent with "copy the
  proven pattern, minimal churn."

### Tradeoffs

- **`group_by_slug` returns index groups (`Vec<Vec<usize>>`) into the caller's
  `notes` slice, not cloned `Note`s or `PathBuf`s.** Matches the doc's stated
  API signature exactly and avoids cloning potentially-large `Note` structs
  (body + raw content) for every same-slug pair; Phase 2's `decide()` and
  Phase 3's executor index back into the same slice the caller already owns.
- **`AssociationConfig::default()` hand-written rather than derived**, even
  though every field's zero value would be "valid" Rust (unlike the
  interval-secs-zero-panics class this repo's rules warn about) — kept
  hand-written to match every sibling config struct's existing convention
  (`DuplicatesConfig`, `GraphConfig`) rather than introduce a mixed style.
- **Loader fail-closed test drives the primary (XDG) path via
  `XDG_CONFIG_HOME` + the shared `crate::testutil::ENV_LOCK`**, rather than
  only exercising the explicit `--config` branch (which was already
  hard-erroring pre-Phase-1 and is easier to test in isolation). Both paths
  are covered so a future refactor of `load_inner` cannot silently reintroduce
  the fail-open bug on either branch.

### Open questions

- None. Phase 1's scope (config, grouping, shared sim primitives) has no
  outstanding decisions — all resolved in the design doc's "Resolved
  Decisions" section before this phase started.
