# Implementation Notes: Graph-Augmented Memory

Running, append-only record of how the implementation interprets or diverges
from `2026-06-05-graph-augmented-memory.md`. One section per phase.

## Phase 1: Materialized edge graph + graph-expansion retrieval

### Design decisions
- **Semantic incremental trigger = a single `produced_at` watermark, not
  per-note tracking** — `cortex/src/graph.rs` (`build`). The doc requires
  semantic edges be keyed on `note_embeddings.produced_at` (NOT
  `notes.modified_at`) to avoid the stranding race. A single
  `semantic_watermark` = max(produced_at) seen, queried with `produced_at >=
  watermark`, satisfies that correctness requirement: `produced_at` IS bumped
  by `cortex embed` (unlike `modified_at`), so a note whose embedding lands
  after it was skipped is picked up on the next pass. Reprocessing the
  boundary note is idempotent (delete-by-src then insert), so `>=` is safe.
  Simpler than a per-note `built_at` table and equivalent in correctness.
- **Every edge is owned by its `src`** — graph pass writes only `(src,
  target)` rows; undirectedness is handled at *read* time by `expand_graph`'s
  `src IN seeds OR dst IN seeds` query. So no reverse rows are written, and
  delete-by-src fully refreshes a note's edges with no stale-reverse-edge
  problem. Matches the doc's "delete-then-insert by src".
- **Graph pass reads note data from the index DB, not a fresh `scan_vault`** —
  all deterministic signals (body for wikilinks, tags, creator/source/domain)
  already live in the `notes` table; the doc says edges are "derived from data
  already in the index". Avoids a second filesystem walk and keeps the pass
  consistent with the embeddings it reads.
- **Calibration values are module consts + a `GraphConfig`** —
  `SEMANTIC_K`, `MIN_COSINE`, `SHARED_TAG_FANOUT_CAP`, weights, `decay`,
  `graph_interval_secs`. Per the no-magic-numbers rule and so the Phase-1
  "calibrate against the labeled query set" open questions are tunable.
- **RRF generalized to N lists** — `reciprocal_rank_fusion` took exactly two
  `&[String]`; `graph-hybrid` fuses three (bm25 ⊕ vector ⊕ graph). Widened to
  `&[&[String]]`; the existing hybrid caller (oracle `server.rs`) updated to
  the new shape. Output for the 2-list case is identical.

- **Incremental triggers via a per-note `edge_build_state` table, not a global
  watermark** — `vault/src/search/graph.rs` (`content_edge_targets`,
  `record_edge_build`) + `vector.rs` (`semantic_edge_targets`,
  `note_summary_produced_at`). Supersedes the earlier global-watermark idea in
  the first draft of these notes. A `(note_path, content_built_at,
  semantic_built_at)` row per note (FK-cascaded) lets an incremental pass touch
  *exactly* the notes that changed: content edges when `modified_at >
  content_built_at`, semantic edges when the newest embedding `produced_at >
  semantic_built_at`. This is the per-row staleness pattern the design's
  `stale_embedding_targets` reference points at; it is both stranding-safe
  (semantic keyed on `produced_at`) and reprocess-free (no boundary churn the
  global watermark would cause).
- **`graph_interval_secs` lives on `GraphConfig`, not `DaemonConfig`** — the
  doc said "new `DaemonConfig` field"; grouping all graph knobs (k, cosine,
  weights, cap, interval) under one `GraphConfig` is more cohesive and keeps the
  calibration values together. The daemon reads `config.graph.graph_interval_secs`.
- **`entities` table created in Phase 1's `ensure_graph_schema`** — harmless to
  create early (it's empty until Phase 3 populates it) and keeps all
  graph-family DDL in one place.
- **`ensure_graph_schema` lives in `graph.rs`, not inline in `search.rs`** —
  `search.rs` is already at the bloat ceiling (`.otto.yml` raises the 1500
  convention to 3600 pending a search.rs split); adding the DDL inline tipped it
  over, so it moved to the `graph` submodule.

### Deviations
- **`w_seed = 1/(seed_rank+1)`, not the doc's `1/(K_RRF_INPUT − seed_rank)`** —
  `oracle/src/server.rs` (`graph_dispatch`). The doc's literal form gives the
  TOP seed (rank 0) the *smallest* weight, which is backwards for "the seed's
  own standing." Since the expansion score only feeds RRF as an *ordering*
  (continuous scores never enter RRF — the doc says so), the magnitude is
  immaterial; only monotonicity matters. Used the natural monotone-decreasing
  `1/(rank+1)` so better seeds contribute more.
- **rust-1.96 clippy `unnecessary_sort_by` drive-by** — the toolchain on this
  host flags pre-existing reverse-sort closures (`b.x.cmp(&a.x)`) across
  borg/cortex/sb/vault as errors under `-D warnings`. Rewrote them to
  `sort_by_key(|b| Reverse(...))` so the Phase-1 CI is green. Mechanical, not
  part of the graph feature.

### Tradeoffs
- **Rebuild-all-kinds per target vs. per-kind partial rebuild** — when a note is
  selected by *either* trigger, the pass rebuilds *all* its edge kinds
  (delete-by-src then re-derive). A content-only change therefore recomputes
  semantic neighbors too. Chose this for correctness simplicity: delete-by-src
  is all-or-nothing, and per-kind partial deletes would risk wiping a note's
  semantic edges while only rebuilding its content edges. At ~15-20 changed
  notes/day the extra cosine scans are negligible.

### Open questions
- `SEMANTIC_K` / `MIN_COSINE` / per-kind weights / `decay` ship at the doc's
  suggested defaults; calibration against a labeled query set is deferred to
  operational tuning (the consts/config make it a config edit, not a code
  change).

## Phase 2: Comprehensive cortex linking

### Design decisions
- **`glossary.yml` holds the vocabulary, not the knobs** — `config/glossary.yml`
  (`concepts` slug list + `aliases` surface→slug map), shipped by `sb bootstrap`
  to `~/.config/sb/glossary.yml` (`vault::paths::glossary()`), loaded by
  `cortex::linking::load_glossary` and folded into the in-memory `LinkingConfig`
  at `cortex::link` run time. Mirrors `canonical-tags.yml` exactly: data file in
  config, governance knobs (`min-word-length`, `scan-for`, target filters) in
  `cortex.yml`.
- **`insert_first_wikilink` matches the *surface* form, not the target slug** —
  `cortex/src/linking.rs`. The pre-existing code searched the body for `target`
  again, which silently failed whenever the matched title/alias differed from
  the slug (e.g. body says "Python Guide" but target is "python-guide"). Now
  `find_mention` returns the matched surface text and threads it through, so the
  apply step wraps the text that actually appears. This also fixes the latent
  title≠stem apply bug, not just the new alias path.
- **Plain `[[slug]]` only on exact (case-sensitive) match; otherwise
  `[[slug|Surface]]`** — so the link target is always the canonical lowercase
  slug (the hub-note name in Phase 3) while the prose case/wording is preserved
  as display text. `LangChain` → `[[langchain|LangChain]]`, `python` →
  `[[python]]`.

### Deviations
- **`--scan metadata` is a deliberate no-op for the linker** — the doc says add
  the `ScanScope::Metadata` variant, but also "no metadata linking into
  frontmatter; there is nothing to inject." So the variant exists (CLI accepts
  `--scan metadata`) but `lint_linking` has no `metadata` branch: creator/
  source/domain relationships are materialized as graph `edges` by `sb cortex
  graph` (Phase 1), never as wikilinks. Documented in `ScanScope::as_config_scan_for`.

### Tradeoffs
- **Glossary aliases use the same `min-word-length` gate as everything else** —
  short acronyms (`RAG`, len 3) below the gate won't auto-link; only their long
  surface forms (`Retrieval-Augmented Generation`) do. Deliberate: avoids
  over-linking 3-letter tokens that collide with common words.

### Open questions
- Whether the glossary's long-term source of truth should migrate INTO the vault
  (hub notes declaring their own aliases in frontmatter) rather than a config
  file. Deferred for discussion with the user after the full build; Phase 3
  already materializes concepts as in-vault hub notes, so both forms coexist.

## Phase 3: Entity hub notes

### Design decisions
- **Hub directory is `entities/` (flat), resolving the doc's open question** —
  `cortex/src/hub.rs` (`HUB_DIR`). All hubs (concept/creator/source/tag) live at
  `entities/<slug>.md`. It's on neither `ScanConfig`'s nor `WatcherConfig`'s
  ignore list (both are denylists), so hubs are scanned, indexed, and watched by
  default — no defaults change was needed (the cross-cutting concern only bites
  when adding an *excluded* dir). A concept's `[[langchain]]` resolves to
  `entities/langchain.md` by stem.
- **Added `NoteType::Entity` to `vault::schema`** — the doc specifies `type:
  entity` frontmatter; schema is law, so the variant was added (`as_str`/`all`/
  `FromStr`), auto-covered by the existing roundtrip tests.
- **Slugify creators/sources, pass concept/tag slugs through** — creator
  "Andrej Karpathy" → `andrej-karpathy`, source host `youtube.com` →
  `youtube-com`; concepts/tags are already kebab.
- **Entity/hub edges are just Phase-1 wikilink edges** — once a hub note exists
  and Phase-2 links `[[concept]]` in bodies, Phase-1's wikilink-edge builder
  emits the edge, and resolve-`dst`-or-skip already protects against a
  deleted/stale hub. So Phase 3 adds no new edge kind for concept hubs; the
  out-of-band-deletion test confirms a vanished hub never aborts the pass.
- **Over-cap tag routing emits one `shared-tag` edge per note to the tag hub** —
  `cortex/src/graph.rs`. When a tag's bucket exceeds `fanout_cap`, instead of
  skipping, the pass emits a single `src → entities/<tag>.md` edge (weight =
  the same `1/ln(1+df)` rarity weight) if the hub exists, replacing the df²
  pairwise explosion.

### Deviations
- **`entities` table populated by `cortex hub`, not the graph pass** — the doc
  says "populate the entities table in the cortex graph pass." `cortex hub`
  owns hub-note creation and therefore knows each hub's `hub_path`, so it is the
  natural owner of the catalogue; populating it from the graph pass would
  duplicate the glossary load + creator/source scan. The graph pass still
  *consumes* hubs (tag routing) but does not write `entities`.

### Tradeoffs
- **`cortex hub` never deletes, only creates** — idempotent: an existing hub is
  left untouched (no destructive refresh), so the `rkvr` safety rule never
  triggers here. A future content-refreshing variant would write (not delete),
  also safe.

### Open questions
- Whether entity hub notes should be excluded from other cortex governance
  (classify/quality/auto-tag) so a stub isn't flagged low-quality or
  reclassified. Out of Phase-3 scope; `type: entity` gives a clean filter hook
  if it becomes noisy.

## Phase 4: LLM entity discovery

### Design decisions
- **`EntityExtractor` trait (DI), `FabricExtractor` in prod** —
  `cortex/src/entities.rs`. The discovery logic takes an injected extractor so
  every test runs against a deterministic `MockExtractor` (no live LLM), per the
  rust-cli DI convention. `FabricExtractor` runs the `extract-entities` Fabric
  pattern and splits output one-entity-per-line; an LLM/subprocess failure
  yields no entities for that note (logged), never aborting the pass.
- **Bounded concurrency = sequential + `max_per_run`** — extraction runs one
  note at a time (concurrency 1) and the pass processes at most `max_per_run`
  ingested notes per run (default 50). This is the strictest reading of the
  no-unbounded-fanout rule: a backlog can never fan out parallel LLM calls.
- **Shipped `extract-entities` Fabric pattern** — `borg/patterns/` + bootstrap
  PATTERNS (now 15), so the default `fabric_pattern` works after `sb bootstrap`.
- **`entity-proposals.yml` merges, never clobbers** — a re-run appends only
  *new* slugs; existing proposals (which a human may be mid-review on) keep their
  recorded frequency. Mirrors `tag-proposals.yml` and never auto-promotes.
- **Known-set = glossary concepts + alias targets + canonical tags** —
  proposals exclude anything already in the controlled vocabularies, so the file
  only ever surfaces genuinely-new candidates.

### Deviations
- None.

### Tradeoffs
- **Sequential extraction over a worker pool** — slower wall-clock on a big
  backlog, but the pass is off-hot-path (daily daemon cadence) and the safety of
  never fanning unbounded LLM subprocess calls on desk.lan outweighs throughput.
  `max_per_run` caps each pass regardless.

### Open questions
- Did not ship an empty `entity-proposals.yml` starter via bootstrap (unlike
  `tag-proposals.yml`); the discovery pass creates it on first write. Trivial to
  add for symmetry if desired.

## Phase 5: MemGraphRAG

### Design decisions
- **`fact` edges connect entity hubs, carry provenance** — `cortex/src/memgraph.rs`.
  A triple `(subject, predicate, object)` becomes an edge `entities/<subj>.md
  --predicate--> entities/<obj>.md`, `kind = "fact"`, with the originating note
  in `src_note`. Both endpoints must already be hub notes; if either is absent
  the edge is skipped (resolve-endpoint-or-skip — `insert_edges` now checks BOTH
  src and dst, a safe generalization that is a no-op for deterministic edges
  whose src is always a real note).
- **`TripleExtractor` trait (DI), `FabricTripleExtractor` in prod** — parses
  `subject | predicate | object` lines; predicate slugified. Tested against a
  `MockTriples` extractor, no live LLM. Ships the `extract-triples` Fabric
  pattern (PATTERNS now 16).
- **`--backfill` triggers the factual+consolidation layer** — per the doc, `sb
  cortex graph --backfill` does the deterministic full rebuild AND (bounded,
  `fact_max_per_run`) triple extraction + the three consolidation agents. Plain
  `sb cortex graph` stays deterministic-only.
- **Contradiction policy = flag-only** (resolving the doc's open question):
  `detect_contradictions` records functional-predicate conflicts and logs WARN;
  it never deletes or overwrites an edge. Recency/source-confidence reconciliation
  is left for a future policy once the corpus actually produces conflicts.
- **edge_kinds matches predicate OR kind** — `expand_graph`'s filter now matches
  either column, so a graph-mode caller can target a relation (`uses`,
  `released-on`) directly. fact edges get `fact_weight` (default 0.5).
- **Cluster bridging targets fully-isolated notes** — `bridge_clusters` adds a
  `bridge` edge from each note with zero incident edges to its nearest semantic
  neighbor (cosine ≥ `bridge_min_cosine`), reusing `note_embeddings`. A note
  with no embedding or no qualifying neighbor stays isolated.

### Deviations
- **Ontology layer is the Phase-3 `ontotype` + undirected read, not a new
  structure** — the doc calls for "enforced two-way relationships
  (entity↔fact↔passage)." `ontotype` is set on every entity at hub-stub time;
  fact edges are stored directed but `expand_graph` traverses `src/dst IN seeds`,
  so entity↔fact↔passage is bidirectional at read with no extra reverse rows.
  No separate ontology table was added beyond `entities.ontotype`.

### Tradeoffs
- **Noise removal is a predicate stop-list, not a salience model** — deterministic
  and cheap; drops generic predicates (`is`/`has`/`relates-to`). A learned
  salience score is out of scope and, per the doc, low-value on this corpus.

### Open questions
- The labeled-query-set retrieval-lift measurement (Phase-1 baseline vs. typed
  edges) is an operational benchmark, not a unit test — per the doc's Testing
  Strategy it is "a measurement and regression guard, not a ship-gate." Left for
  operational calibration with the tunable `GraphConfig` weights.
- Predicate vocabulary is open extraction (LLM-chosen, slugified) rather than a
  controlled set; `functional_predicates`/`noise_predicates` config lists govern
  consolidation. A controlled predicate vocabulary (mirroring canonical tags) is
  the natural next step if open extraction proves noisy.
