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

## Phase 2: Similarity decision core (transitive clustering)

### Design decisions

- **`AssociationOutcome` enum** — `cortex/src/association.rs` — the doc's Data
  Model variants (`Merge { survivor, absorbed, session_ids }` / `CrossLink
  { notes }`), derives `PartialEq, Eq` so tests can assert whole outcomes by
  value. Only `AssociationOutcome` is defined; `AssociationReport`
  (`WouldAssociate`/`Associated`) is deferred to Phase 5 (CLI/daemon) — Phase
  2's `decide` returns the bare outcome vector it needs and nothing more.
- **`decide(group, ctx)`** — `cortex/src/association.rs` — pure transitive
  clustering. Visits every pair in sorted `(i, j)` order, computes similarity
  per source, unions any pair `>= threshold` via `UnionFind`, then maps each
  resulting cluster to a `Merge` (>= 2 members) or a cross-link representative
  (singleton). One group-level `CrossLink` of all cluster representatives is
  emitted when the group resolves to >= 2 clusters.
- **`EmbeddingCosine` port + `DecideCtx<'a, E>`** — `cortex/src/association.rs`
  — the embedding signal is a trait (generic DI, no `dyn`, per the repo Rust
  rules); `vault::search::SearchIndex` gets the production impl (delegates to
  the Phase 1 `cosine_between`), and tests inject a deterministic
  `FakeEmbeddings`. This is what keeps `decide` a pure, index-free unit under
  test while the real path stays wired to the pairwise-cosine primitive.
- **`UnionFind` with union-by-min** — `cortex/src/association.rs` — a cluster's
  root is pinned to its smallest member index, so cluster ids are stable and
  the `BTreeMap<root, members>` iterates in a deterministic, union-order-
  independent sequence. Path compression on `find` keeps it near-constant.
- **Survivor computed in `decide`, not left as a Phase 3 placeholder** —
  `resolve_merge` / `survivor_key` (`cortex/src/association.rs`) — the doc's
  survivor rule (earliest `date`, ties by smallest primary session id) is
  deterministic and the `CrossLink` representative of a merge cluster IS its
  survivor, so `decide` must know it anyway. Computing it here (rather than a
  placeholder) means the `Merge` fields are final and Phase 3's executor
  consumes them as-is; a missing/unparseable `date` sorts LAST
  (`NaiveDate::MAX`) so a dated note always wins survivorship, and `path` is
  the final total-order tiebreak so no run can differ.
- **Claim-text fallback = pairwise TF cosine over the `## Claims` section** —
  `claim_similarity` / `claim_text` (`cortex/src/association.rs`) — reuses the
  Phase-1-promoted `duplicates::{tokenize, cosine_similarity}` exactly as the
  Phase 1 `promoted_sim_fns_are_callable_from_association` test demonstrated
  (raw term counts as the vector, not corpus-IDF-weighted). Term-count TF is
  genuinely pairwise (a third group member can't shift an A–B claim score),
  which matches the design's "real pairwise, not affected by others" principle
  that motivated `cosine_between` on the embedding side.

### Deviations

- **`decide` returns `Result<Vec<AssociationOutcome>>`, not the doc's bare
  `Vec<AssociationOutcome>`** (same effect, correct seam). The embedding
  signal is a fallible SQLite read (`cosine_between -> Result<Option<f32>>`);
  `decide` propagates that `Result` and treats `Ok(None)` as the "uncomputable"
  case. This is exactly what the Phase 1 notes recommended, and a
  `embedding_db_error_propagates_not_swallowed` test pins that a real DB error
  surfaces as `Err` rather than silently degrading every pair to cross-link on
  a broken index.
- **`decide` takes `group: &[&Note]` (the resolved members of one group), not
  the doc's unspecified `group` shape.** `group_by_slug` returns
  `Vec<Vec<usize>>` indices into the caller's `notes`; the Phase 5 caller maps
  one group's indices to `&Note` refs and hands them here. Keeps `decide`
  self-contained and index-free for unit testing, and avoids cloning `Note`s.
- **Uncomputable vs computed-zero are distinguished internally but both route
  to cross-link.** `claim_similarity` returns `None` (uncomputable) only when
  at least one note has NO claim tokens; two notes that both have claims but
  share no terms return `Some(0.0)` — a real below-threshold measurement.
  Either way the pair is never unioned, so the fail-safe (uncomputable never
  merges) holds; the distinction is preserved so `similarity-source` semantics
  and the tests read honestly.

### Tradeoffs

- **One group-level `CrossLink` of cluster representatives, vs per-pair
  cross-links.** For a 3-member group resolving to Merge{A,B} + singleton{C},
  `decide` emits `CrossLink { notes: [survivor(A,B), C] }` — the merge
  cluster is represented by its survivor (absorbed notes become tombstones
  that redirect to it, so linking them is pointless), and all distinct
  clusters in the group cross-reference each other through one outcome. The
  doc's success-criterion shorthand "Merge{A,B} + CrossLink{C}" is read as
  "C is cross-linked (to the merged note)"; the executor (Phase 4) inserts
  reciprocal wikilinks among the named representatives.
- **Term-count TF cosine for claims instead of corpus-IDF TF-IDF** (the
  duplicates fuzzy path builds IDF over its corpus). IDF would couple a pair's
  score to which other members are in the group, breaking pairwise
  independence; the flat TF cosine keeps each pair's similarity a function of
  only those two notes, and matches the primitive-usage pattern Phase 1 already
  established and tested.

### Open questions

- None. The 3-member `CrossLink` representative shape (survivor + singleton,
  not the literal `{C}`) is the one place the doc's shorthand needed
  interpretation; it is resolved in-line above and covered by
  `three_member_group_merges_close_pair_cross_links_distant_third`. Phases 3–5
  (executors + CLI/daemon) consume `AssociationOutcome` unchanged.

## Phase 3: Merge executor

### Design decisions
- **`execute_merge(vault_root, survivor, absorbed, session_ids, writer)` returns
  the changed vault-relative paths** — `cortex/src/association.rs::execute_merge`
  — mirrors `scope::apply_scope`/`duplicates::apply_duplicates` so Phase 5's
  `apply` and the daemon oscillation fingerprint draw only from real writes; a
  byte-identical survivor is not rewritten (skip-if-unchanged).
- **Idempotent union operates on raw section bullet lines, keyed per section** —
  `association.rs::append_bullets` + `claim_key`/`session_detail_key` — claims
  dedup by trimmed bullet text (design wording), session-details dedup by the
  `clyde://<id>` id (stable even if a rendered title/repo column differs).
  `bullet_blocks` carries each claim's `> "..."` quote-continuation lines along
  with it, matching `vault::search::parse_body_claims`.
- **Tombstone via existing frontmatter helpers + a body swap** —
  `association.rs::tombstone_content` composes `scope::remove_frontmatter_fields`
  (drop `slug:`), `scope::insert_frontmatter_fields` (add `superseded-by:`), and
  a new local `swap_body` (replace body with the `Merged into [[stem]].`
  redirect). NO `status:` change — `superseded-by:` is the marker (schema-is-law).
- **Survivor-stem, not a rename** — `association.rs::survivor_stem` uses the
  survivor's own `file_stem`; the survivor keeps its filename (design OQ4).
- **Explicit tombstone exclusion from embed via a `superseded_by` notes column**
  — `vault/src/search/schema.rs` (CREATE TABLE + `ensure_superseded_by_column`
  migration), `vault/src/search/index.rs::index_one` (populate from
  `superseded-by` frontmatter), `vault/src/search/vector.rs::stale_embedding_targets`
  (`AND (n.superseded_by IS NULL OR n.superseded_by = '')` on all three kinds).
  A session tombstone would already be skipped incidentally (empty summary/claims,
  Session is not transcript-eligible), but the explicit predicate is fail-closed
  and lets the test bite with a tombstone that STILL carries a non-empty summary.

### Deviations
- **A `NoteWriter` port (`AtomicWriter` in prod) instead of calling
  `vault::note::write_atomic` directly** like the sibling `apply_*` functions —
  same effect, correct seam. Earned by Phase 3's mandate to test a mid-cluster
  tombstone-write failure and prove self-heal deterministically (a `FailWriter`
  fails one path); a read-only-dir filesystem trick would be non-portable and
  root-fragile. Phase 5's `apply` threads `&AtomicWriter`.
- **Added the `superseded_by` column + migration in the vault crate**, which the
  Phase 3 bullet ("tombstones excluded from ... embed") implies but does not
  spell out as a schema change. It is the correct, testable seam for the
  "a tombstone gets no new embedding row" success criterion.

### Tradeoffs
- **Skip-if-unchanged on the survivor, always-write on a retire** vs writing
  both unconditionally — keeps the normal second run a true byte-level no-op and
  lets a self-heal re-absorption report only the newly-retired tombstone
  (survivor already unioned), rather than churning the survivor's mtime.
- **Dedup session-details by clyde id, claims by trimmed text** vs one uniform
  key — the design specifies different identities for the two sections, and the
  id is the stable identity for a session bullet whose display columns can drift.

### Open questions
- None. Cross-repo/system-mutating steps: none in this phase. The `superseded_by`
  column is additive (`DEFAULT ''`, idempotent `ALTER`), so it needs no
  coordinated migration — existing DBs gain it on next open via
  `ensure_superseded_by_column`.
