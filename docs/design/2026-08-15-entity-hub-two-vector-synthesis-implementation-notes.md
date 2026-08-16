# Implementation Notes: entity hub bodies, merging the two knowledge vectors

Design doc: `docs/design/2026-08-15-entity-hub-two-vector-synthesis.md`

Append-only. A later entry supersedes an earlier one; nothing is rewritten.

## Phase 0: run what is already built, and snapshot first

**Model:** sonnet (run inline by the orchestrator — zero production code, live
daemon-host operations)

Executed on `desk` (the daemon host) on 2026-08-16 with `sb v0.14.3`.

### Recorded observations

| step | result |
|---|---|
| vault git snapshot | clean at `89d95fa`, tagged `pre-hub-phase0` |
| oracle DB snapshot | `~/.local/share/oracle/oracle.db.pre-phase0` (233 MB; 3076 notes / 227272 edges / 858 entities) |
| `hub-synthesized:` carriers | 15, exactly the set the doc names |
| `hub-body:` carriers | 0 |
| refusal bodies | 134 |
| overlap (synthesized ∩ refusal) | 0 |
| `sb cortex hub` dry run | would create 46 hubs (852 existing), 31 of them repo hubs |
| malformed repo slug | 1, `/home/saidler/repos/scottidler/loopr-v5` — rejected as predicted |
| `sb cortex hub --apply` | `created=46 existing=852 entities_recorded=898` |
| `find entities/repos -name '*.md' \| wc -l` | **32** (>= 30 ✓) |
| `select count(*) from notes where path like 'entities/repos/%'` | **32** (>= 30 ✓) |
| `repo-member` edges after reindex | still 4 — expected; they land in Phase 1's `graph --backfill` |

### The editorial decision (required by the phase)

**All 15 `hub-synthesized: 2026-07-02` hubs are RELEASED. None frozen. Zero
`hub-body: manual` keys were set.** This is the doc's expected outcome
(round 5, M1).

Enumerated set: `agents`, `anthropic`, `automation`, `claude-code`, `claude`,
`embeddings`, `football`, `knowledge-graph`, `llm`, `mcp`, `obsidian`,
`offense`, `ollama`, `prompt-engineering`, `rag`.

Evidence re-verified independently of the doc: 15/15 carry Fabric `summarize`
boilerplate (`ONE SENTENCE SUMMARY:` / `MAIN POINTS:` / `TAKEAWAYS:`) — 11 as
markdown headings (`## ONE SENTENCE SUMMARY:`), 4 bare. They are generated
prose, not hand-written, so the deterministic render supersedes them.

The no-refusal-in-the-manual-set assertion holds trivially: the manual set is
empty, and the synthesized ∩ refusal intersection is independently 0.

### Design decisions

- **Snapshot the oracle DB with `sqlite3 .backup`, not `cp`** — `hub.rs:379`.
  The doc prescribes `cp ~/.local/share/oracle/oracle.db{,.pre-phase0}`, but
  the live DB is in WAL mode with a 7 MB `-wal` file and five concurrent
  readers/writers (`sb oracle serve` ×4 plus the cortex daemon). A bare `cp` of
  `oracle.db` alone would snapshot a torn database missing every committed-but-
  uncheckpointed transaction. `.backup` is the online-backup API and produces a
  consistent single file. Same intent, correct mechanism.
- **Tagged the vault snapshot rather than making an empty commit** — the vault
  working tree was already clean, so `entities/` was committed by definition.
  `git tag pre-hub-phase0` gives the recoverability anchor the phase actually
  wants without a no-op commit.
- **Did not stop the cortex daemon for Phase 0.** The doc requires it stopped
  only for Phase 1's `graph --backfill`. Phase 0's writes are hub-file creation
  plus an oracle reindex, neither of which the daemon's sweeps contend with
  destructively.

### Deviations

- **`sb oracle index` does not accept `--vault`.** The doc's chain writes
  `sb oracle index`; invoking it with `--vault` (as the cortex commands take)
  fails with `unexpected argument '--vault'`. Ran it bare; it resolves the
  vault from `~/.config/sb/oracle.yml`. No doc change needed — the doc's own
  text is already correct.
- **The reindex reported `Inserted: 0 / Unchanged: 3122`.** Not a failure: the
  four live `sb oracle serve` processes run a VaultWatcher that had already
  ingested the 46 new files before the explicit `index` ran. Verified directly
  against the DB — 904 `entities/%` rows in `notes`, 32 under
  `entities/repos/%` — so the phase's success criterion is met by state, not by
  the command's delta counters.

### Tradeoffs

- **`.backup` (~0.9 s, 233 MB) vs. quiescing every writer and `cp`-ing all
  three WAL files.** Chose `.backup`: it is correct under concurrency and needs
  no service interruption on Scott's live machine.

### Open questions

- **The running cortex daemon rewrites hub frontmatter within seconds of a
  hub being written** — it added `cortex-quality` / `cortex-quality-issues` to
  all 46 new stubs immediately after the `--apply` commit (vault commit
  `b8a1bbc`). This is normal governance, but it directly threatens Phase 2's
  *"a second run with unchanged inputs writes zero bytes (vault
  byte-identical)"* acceptance criterion: the daemon, not the builder, will
  dirty the files between runs. **Phase 2's live verification must stop the
  cortex daemon first**, or the byte-comparison measures the daemon instead of
  the builder. Flagging now so the Phase 2 verification is designed around it.

### Live-state deltas for later phases

- vault git: `89d95fa` (pre) → `7973c85` (46 stubs) → `b8a1bbc` (daemon
  frontmatter)
- oracle: 858 → 904 entity rows; 3076 → 3122 notes
- `repo-member` edges: 4 (unchanged; Phase 1 wires the rest)

## Phase 1: membership primitives for creator/source, and stop the false one

**Model:** opus

Code only. No live-vault or live-DB command was run in this phase; the
`graph --backfill` and its success criteria that need the live index are the
orchestrator's step on the daemon host.

### Design decisions

- **`vault::search::extract_host` made `pub`** — `vault/src/search.rs:436`. It
  already had the exact semantics and tests the two cortex copies were
  approximating. Now it is the single host implementation in the workspace.
- **`cortex::hub::source_host` is now a one-line delegation to
  `extract_host`** — `cortex/src/hub.rs`. Its own parser (identical logic,
  independently maintained) is deleted. `collect_stubs` is unchanged and reads
  the host through it.
- **`cortex::hub::source_hub_path(&str) -> Option<String>`** — the single place
  that turns a `source:` value into a hub path, mirroring `repo_hub_path`.
  Used by the `source-member` edge builder.
- **`cortex::graph::source_host` renamed `source_bucket_key`** —
  `cortex/src/graph.rs`. The function returns the raw lowercased value on
  schemeless input, which is not a host: the name was the lie that let the
  divergence live. Its host extraction now delegates to `extract_host`; the
  URL-shaped-vs-schemeless SPLIT stays local because it is this layer's policy
  (the bucket layer must keep schemeless values so co-provenance markers group;
  the hub layer must skip them). `shared-source` behavior is unchanged, pinned
  by `schemeless_sources_still_share_a_shared_source_bucket`.
- **One `MEMBER_WEIGHT` constant (1.0) for all three `*-member` kinds**,
  replacing `REPO_MEMBER_WEIGHT`. Same value, one name; membership is a strong
  deterministic signal.
- **Stopword matched with `eq_ignore_ascii_case` on the raw `[[target]]`**
  before `resolve_note_path`, per the doc. The `[[a|b]]` / `[[a#h]]` forms
  already arrive stripped to `a` from `extract_wikilinks`' capture group.
- **Template ships the stopwords UNCOMMENTED** —
  `config/templates/cortex.yml.example`. The Rust default is the empty list the
  doc specifies (code never silently suppresses a link), so the two measured
  offenders have to come from the starter config or a fresh `sb bootstrap`
  would ship the false-membership behavior. Pinned by
  `cortex_template_seeds_the_wikilink_stopwords`.

### Deviations

- **The shared source seam is TWO public functions, not one.** The doc (and the
  task) name a single `hub::source_hub_path` used by both the stub side and the
  edge side. The stub side cannot use a path-returning function: `HubStub`
  needs the host as its `title` (the human-facing hub name) and as the basis of
  its `slug`, and recovering those by string-parsing a formatted path back
  apart would be strictly worse than what is there today. Implemented at the
  correct seam instead: `hub::source_host` (the one host read, shared by both
  sides) with `hub::source_hub_path` derived from it for the edge side. Same
  effect — one host implementation, no fourth copy, and the two divergent
  copies are gone — and the property the doc actually wants is asserted
  directly by `source_hub_path_matches_stub_hub_path`, which pins the edge
  `dst` byte-identical to `HubStub::hub_path()` across `www.`, query strings,
  uppercase, ports, and deep paths.
- **`GraphStats` gained THREE fields, not the two new kinds only.**
  `repo_member` is included because the doc's own bullet says `tally`'s
  `_ => {}` already hid `repo-member` from the run report, and the phase's
  success criterion asks the report to show all three.
- **No test asserts the live counts** (`repo-member` > 200,
  `entities/every.md` at zero surviving wikilink edges after a real backfill).
  Those are live-index criteria and this phase was run under an explicit
  no-live-command constraint. Each is covered by an equivalent temp-index test
  on the same code path; the live numbers are the orchestrator's to observe.

### Tradeoffs

- **Renaming `source_host` -> `source_bucket_key` in `graph.rs` vs leaving the
  name alone.** Renaming touches a private function and three call sites and
  makes the diff slightly larger; leaving it would keep a name that describes
  something the function does not return. Took the rename.
- **Empty-list code default + seeded template vs seeding the default in Rust.**
  The doc pins the empty default, and it is the fail-loud choice (suppression
  is visible in config, never implicit in code). Cost: an EXISTING install does
  not get the stopwords from an upgrade — see the open question below.
- **`is_wikilink_stopword` does a linear scan over the config list** rather
  than building a `HashSet` per pass. The list is 2 entries and the scan runs
  once per wikilink; a set would be more machinery than the data justifies.

### Open questions

- **The live `~/.config/sb/cortex.yml` must be edited before the backfill.**
  The code default is empty and the seeded values live only in the starter
  template, which `sb bootstrap` does not re-apply over an existing config. So
  the daemon host's `cortex.yml` needs

  ```yaml
  graph:
    wikilink-stopwords:
      - every
      - brief
  ```

  added under its existing `graph:` block, or `graph --backfill` reinstates all
  569 false `entities/every.md` wikilink edges and the phase's stopword
  criterion cannot pass. (Its current `graph:` block holds only
  `fact-max-per-run` and `fact-interval-secs`; both are valid keys, so the new
  `deny_unknown_fields` will not reject it.)
- **`deny_unknown_fields` is now live on `GraphConfig` and `EntitiesConfig`.**
  The daemon host's config was inspected and is clean, but any OTHER machine's
  `cortex.yml` with a stray key under `graph:`/`entities:` turns from silently
  tolerated into a hard load failure on upgrade. That is the intended behavior
  (Rollout section names it); flagging so a laptop failure after `otto deploy`
  is read as the feature, not a regression.

## Phase 2: deterministic hub bodies

**Model:** opus

Code only. No live-vault or live-DB command was run: every test drives a temp
vault and an in-memory index. The first full builder run against
`~/repos/scottidler/obsidian/` (and the live criteria that depend on it) is the
orchestrator's step on the daemon host.

### Design decisions

- **`SearchIndex::hub_members_deliberate`** — `vault/src/search/graph.rs`. Both
  filters (deliberate kinds; `src NOT LIKE 'entities/%'`) live in the SQL, so no
  consumer can forget one. `hub_members` stays kind-agnostic for the graph tests
  that use it as a generic inbound probe; production synthesis no longer calls
  it (it now has zero production callers).
- **The renderer is a separate pure module** — `cortex/src/hub/render.rs`, with
  its tests in `cortex/src/hub/render/tests.rs`. No injection seam anywhere on
  the body path: every renderer test feeds real `Claim` values and asserts on
  emitted text. That is the structural fix for how rev 1 shipped broken (doubles
  whose `_members` argument was ignored).
- **`Vector::of` classifies through `vault::schema::NoteType`**, not a local
  list of type strings (CLAUDE.md: schema is law). An unparseable/absent type is
  `Other` and renders under neither section.
- **One composition seam, `compose_hub_content(fm_block, title, body)`** —
  `cortex/src/hub.rs`. Stub creation, a rendered body, and a stub reset all pass
  through it, and `render_hub` is now built from it plus `stub_body(stub)`. That
  is what makes "reset to the stub" byte-identical to "freshly stubbed": the two
  cannot drift into an infinite rewrite loop over a newline. The stub-file bytes
  are unchanged from before this phase (pinned by the pre-existing
  `write_stubs_*` tests).
- **`plan_hub_body` is pure and returns the bytes** —
  `HubPlan { outcome, content, previously_rendered }`. The four write branches
  are decided without touching the filesystem, which is what lets
  `build_hub_bodies` compute EVERY outcome before writing ANY file (the
  run-level backstop) and makes each branch directly testable.
- **`previously_rendered` = the body's first H2 is `## Summary`** —
  `hub::body_is_rendered`. Only the renderer emits that heading, so stub bodies,
  the 134 refusal bodies, and the 15 Fabric `## ONE SENTENCE SUMMARY:` bodies
  are all excluded from the backstop count, exactly as the doc requires (the
  first run's ~124 refusal resets are expected, not a regression signal).
- **A dry run does not open the oracle index at all** — `hub::run` returns
  before `SearchIndex::open`. Skipping only the `upsert_entity` calls would
  still have created the DB file and run `ensure_schema` on a fresh host; "a dry
  run writes nothing anywhere" is the whole point of the gate.
- **The digest's per-vector budget is one forward pass per vector**
  (`render::vector_line`). An empty line (nothing fit, not even a truncated
  first claim) costs 0 bytes, so the whole session budget cedes to sources.
  Ceding is one-directional and computed from the emitted line's byte length,
  so the arithmetic is exactly the doc's.
- **Structural source-level tests** for the two invariants that are absences
  rather than behaviors: no `fabric::` / `run_pattern` / `HubSynthesizer`
  reference and no `fs::write(` anywhere in `hub.rs` / `hub/render.rs`
  (`no_fabric_call_is_reachable_from_cortex_hub`). A future edit cannot quietly
  reintroduce either.
- **The retrieval contract has a negative control**
  (`an_unbudgeted_digest_would_starve_the_second_vector`): with the byte budget
  raised to 1 MB the same fixture's source vector IS truncated out of the
  512-token window. Without it, the passing assertion could not distinguish
  "the budget works" from "the fixture is small".
- **Tokenizer fixture committed at
  `cortex/tests/fixtures/bge-small-en-v1.5-tokenizer.json`** (711 KB, copied
  from the local hf-hub cache) with `tokenizers = "0.23.1"` as a cortex
  dev-dependency (the same version `vault::embedding::candle` loads). Offline,
  no weights, runs in `otto ci`.

### Deviations

- **`render_hub_body` takes the hub TITLE as its first argument.** The doc's API
  section writes `render_hub_body(members, caps)`, but the digest's definition
  sentence is `<Title>: hub of N sources and M sessions.` - the title is an
  input, not something recoverable from the members. Same effect, correct seam.
- **`HubMember` carries `date`**, beyond the doc's
  `{ path, title, note_type, claims }`. The doc's own member ordering (`date:`
  descending, path tiebreak, undated last) cannot be implemented without it.
- **A member bullet's wikilink is `([[<path minus .md>|<title>]])`**, not the
  doc's literal `([[member]])`. A bare basename is ambiguous across two
  same-named notes and does not resolve into a nested directory; the full
  vault-relative path is a literal-path match that resolves unconditionally
  (the same reasoning `repo_hub_wikilink_target` already carries), and the alias
  keeps the render readable. A title containing `|`, `[`, or `]` drops the alias
  rather than emitting broken markup.
- **`synthesize_hub` is gone, not kept.** The doc says to keep "the file-handling
  fail-safe in `synthesize_hub`"; its semantics are kept in full (frontmatter
  preserved verbatim, a failure never overwrites, the same file is rewritten and
  never re-slugged or deleted) but they now live in the pure `plan_hub_body`,
  because the old function's shape was `(&impl HubSynthesizer)` and the trait is
  deleted. The carried acceptance test is carried too, as
  `a_run_where_every_member_is_unreadable_preserves_every_body`.
- **`SynthOutcome` kept its name and grew to six variants** (`Manual`,
  `Preserved`, `Rendered`, `Unchanged`, `Reset`, `StubKept`). Two variants
  cannot express four branches, and the write/no-write split inside branches 3
  and 4 is what the run report counts.
- **`HubReport.synthesized` / `synth_preserved` are renamed** to the seven
  per-branch counters the doc's run report requires
  (`bodies_written` / `bodies_unchanged` / `bodies_reset` / `stubs_kept` /
  `bodies_manual` / `bodies_preserved` / `members_skipped`), printed by
  `sb cortex hub --apply --synthesize`.
- **The stub-creation write also moved to `write_atomic`.** The doc names only
  `hub.rs:522` (the body write), but the success criterion says *every* hub
  write, and `write_stubs` had the identical torn-write exposure on a
  Syncthing'd vault.
- **`cortex::embed::summary_embed_text` extracted** (from the inline block in
  `process_summary_batch`) and made public, so the retrieval-contract test
  asserts against the REAL embed-text composition instead of a copy of it. The
  byte-identical invariant it carries is now pinned by its own test.
- **No live criteria are asserted here**: `grep -rl "don't have access…"
  entities/ -> 0`, the ~160-hub written-body band, and the live second-run
  zero-byte check all need the daemon host. Each has an equivalent temp-vault
  test on the same code path.

### Tradeoffs

- **Source-level grep tests vs a type-level guarantee** for "zero Fabric calls"
  and "no `fs::write`". A type-level guarantee would mean a capability wrapper
  around the filesystem, which is far more machinery than this invariant is
  worth; the grep is cheap, exact, and fails loudly on the next edit.
- **Wikilink alias (`|<title>`) vs bare path target.** The alias adds bytes to
  the body (never to the digest, which carries claims only) and can be dropped
  by adversarial titles. Chose it because the body is read by a human in
  Obsidian, where a wall of `[[knowledge/tech/2026-04-…]]` targets is unreadable.
- **Six-variant `SynthOutcome` vs a `(branch, wrote: bool)` pair.** The flat
  enum makes the run report a single match with no impossible combinations, at
  the cost of two variants that differ only in whether bytes moved.
- **`build_hub_bodies` holds all plans in memory before writing.** That is the
  backstop's precondition, and it costs one `String` per changed hub (hundreds,
  each a few KB) - trivial next to the file reads it already does.

### Open questions

- **The live verification must stop the cortex daemon first.** Carried forward
  from Phase 0's open question and still load-bearing: the daemon rewrote
  `cortex-quality` frontmatter onto all 46 new stubs within seconds of Phase 0's
  `--apply`. The builder preserves frontmatter verbatim, so a daemon edit does
  not fight the builder - but it DOES dirty the files between two builder runs,
  and the "a second run writes zero bytes" criterion is a byte-compare of the
  whole vault. Stop `cortex.service` for the verification, or compare hub files
  only.
- **The first live run's reset count will exceed the default backstop of 20 if
  any of the resets land on previously-RENDERED bodies.** Today none can (no hub
  carries a renderer-produced body yet), so the first run is safe at the
  default. From the SECOND run on, a membership change that empties several
  large hubs at once could legitimately exceed 20 and abort the run; the abort
  message names the hubs and the fix is a config bump. Flagging so an abort is
  read as the backstop working, not as a bug.
- **`entities.render.*` is absent from the live `~/.config/sb/cortex.yml`.** The
  Rust defaults are the designed values, so no config edit is required to run
  Phase 2 - the template entry is commented-out documentation. Only a deliberate
  tuning (e.g. raising `max-render-resets-per-run`) needs an edit.

## Phase 3: asymmetry report

**Model:** sonnet

Code only. No live-vault or live-DB command was run: every test drives a temp
vault and an in-memory `SearchIndex`. `entities/claude.md` reporting `both`
against the live corpus is the one criterion the orchestrator verifies on the
daemon host.

### Design decisions

- **New submodule `cortex/src/hub/asymmetry.rs`**, mirroring how Phase 2 put
  the renderer in its own pure module (`cortex/src/hub/render.rs`). `hub.rs`
  re-exports `AsymmetryBucket`, `AsymmetryReport`, `AsymmetryRow`,
  `AsymmetryTotals`, `build_asymmetry_report` the same way it re-exports the
  render types.
- **`build_asymmetry_report` reuses `SearchIndex::hub_members_deliberate` and
  `hub::load_hub_member` verbatim** - the exact membership query and member
  loader Phase 2's body builder reads from. No second membership query: the
  design explicitly calls this out, and a second query could silently drift
  from the builder's view of reality.
- **Classification needs only `note_type`, not claims** - `load_hub_member`
  still parses `## Claims` (it is the one existing, tested loader), but the
  asymmetry report counts raw deliberate membership per vector, not
  claim-bearing membership (Phase 2's claim-bearing filter is a DIFFERENT,
  narrower set used for body rendering). Classifying through
  `hub::Vector::of(&member.note_type)` keeps the vector definition in the ONE
  place the design pins it (schema is law).
- **`--asymmetry` is checked FIRST inside `hub::run`, before `write_stubs` and
  before any apply/synthesize branch, and returns early.** This is what makes
  the read-only contract hold even under a combined
  `--asymmetry --apply --synthesize` invocation: the flag short-circuits
  before any write path exists to fall into, rather than relying on every
  downstream branch individually declining to write.
- **`AsymmetryReport::render()` lives in cortex, not `sb`.** `sb/AGENTS.md`
  pins stdio to `sb`, but the "two runs produce byte-identical output"
  criterion needs a pure, directly-testable serialization - the same pattern
  `bridge::apply_bridge`'s `report.diff` field already uses (a library
  produces the string, `sb` only `print!`s it).
- **Structural read-only guard** (`asymmetry_report_is_read_only_by_construction`)
  mirrors `no_fabric_call_is_reachable_from_cortex_hub`: greps the module's own
  source for `fs::write(`, `write_atomic`, `INSERT INTO`, `upsert_entity`,
  `.execute(` and asserts none appear, so a future edit cannot quietly turn the
  report into a writer.
- **An unmaterialized stub (no hub file on disk) is excluded from the report
  entirely, not counted as `unlinked`** - mirrors the body builder's
  `abs.exists()` gate in `build_hub_bodies`, and keeps "the four buckets sum to
  the hub count" meaning the count of hubs that actually exist.

### Deviations

- **No test exercises `cortex::hub::run` with `opts.asymmetry = true`.**
  `Config::oracle_db_path()` is `vault::paths::oracle_db_path()` - a hardcoded
  `~/.local/share/oracle/oracle.db`, not overridable per test `Config`. Every
  existing hub test that needs an index (`build_hub_bodies`,
  `populate_entities`, ...) calls the lower-level function directly against
  `SearchIndex::open_memory()` for exactly this reason; the one existing test
  that calls `run()` at the top level (`dry_run_writes_no_vault_bytes_...`)
  only works because `opts.apply = false` returns before the index is ever
  opened. Testing `--asymmetry` at the `run()` level would open the REAL live
  oracle DB - forbidden under this phase's no-live-DB constraint and simply
  wrong to do from a unit test regardless. Tests instead drive
  `build_asymmetry_report` directly (same code path `run()` calls), which is
  the identical seam Phase 1 and Phase 2 already used for their own DB-backed
  assertions.

### Tradeoffs

- **Reusing `load_hub_member` (which parses claims) vs. a lighter
  type-only loader.** The asymmetry report only needs `note_type` per member,
  so parsing claims is unused work. Chose reuse over a second loader: the
  report is IO-bound already (one query + N file reads, same as the body
  builder), the extra parse cost is negligible next to the file read it
  already does, and a second, narrower loader would be a second place to keep
  in sync with `HubMember`'s shape.
- **Skip-and-log on an unreadable member vs. propagating the error.** The
  report never aborts on one bad file (matches the read-only/best-effort
  posture the design implies for a report command); a hub still lands in
  exactly one bucket from whatever membership DID load. Unlike Phase 2's body
  builder, there is no "Preserved" branch to fall back to here - there is
  nothing to preserve, since nothing is ever written.

### Open questions

None.

## Live verification (orchestrator, daemon host `desk`, 2026-08-16)

Run from `target/release/sb` built at each phase, deliberately NOT
`cargo install` — installing is a finalization action and the live runs did not
need it. The cortex daemon was stopped for the backfill and the builder runs
and restarted afterwards (`active`).

### Phase 1 — `graph --backfill` (01:29 → 02:31, ~62 min)

Run report: `full_rebuild=true notes=3123 semantic=30838 wikilink=14362
shared_tag=125798 metadata=33016 repo_member=261 creator_member=1130
source_member=1518 skipped=275` — all three membership kinds printed, so
nothing is swallowed by the old `_ => {}` arm.

| criterion | observed |
|---|---|
| `repo-member` > 200 | **261** (was 4) ✓ |
| `wikilink` edges into `entities/every.md` | **0** (was 569) ✓ |
| `creator-member` into `entities/every.md` | **1** ✓ |
| `clyde://` source-member edges / dangling dsts | **0 / 0** ✓ |
| `entities/youtube-com.md` source-member | **1030** (fanout cap would have zeroed it) ✓ |
| unknown key under `graph:` / `entities:` | both fail load, naming the key ✓ |
| no landed note body modified by the run | **0 files** with an mtime inside the backfill window ✓ |

`fact` edges 359 → 468 rewritten by the pre-existing Fabric extraction
(`facts scanned=1000 triples=9445 written=468 skipped=8977`), the plan's only
LLM spend, as stated in-phase.

### Phase 2 — builder (`hub --apply --synthesize`)

Run 1: `written=522 unchanged=0 reset=303 stubs_kept=73 manual=0 preserved=0
members_skipped=0` (sums to 898).
Run 2: `written=0 unchanged=522 reset=0 stubs_kept=376` — `entities/` SHA-256
manifest **byte-identical** across the two runs.

| criterion | observed |
|---|---|
| refusal bodies | **134 → 0** ✓ |
| `## From sources` / `## From your sessions` / both | 490 / 59 / 27 ✓ |
| the 15 Fabric bodies rewritten | 0 `ONE SENTENCE SUMMARY` left ✓ |
| rendered body parses to zero claims | 0 `## Claims` headings ✓ |
| second run writes zero bytes | byte-identical ✓ |
| flagless run read-only | vault AND `entities` table unchanged ✓ |
| definition sentence | `Claude: hub of 345 sources and 63 sessions.` — the doc's measured 345/63 cohort ✓ |

**End-to-end retrieval (the original ask).** After `oracle index` + `cortex
embed` (825 hub embeddings refreshed), `knowledge_search` under the LIVE
configured vector-first pipeline, queried with a claim that appears in
`entities/claude.md` ONLY via its `## Summary` `Sessions:` line and its
`## From your sessions` section (never in `## From sources`, verified by
section-range grep), returns **`entities/claude.md` at rank 4 of 10**. Its
`tldr` renders as the definition sentence, which is what round 3's M2 fix
existed for. ✓

### Phase 3 — `hub --asymmetry`

`asymmetry: both=31 learned-not-applied=823 applied-not-read=28 unlinked=16
(hubs=898)` — buckets sum to the hub count. `entities/claude.md` reports
`both`. Two runs byte-identical; vault and `entities` table unchanged. ✓

### Finding: the written-body count landed at 522, above the doc's ~480-510 band

Phase 2's bullet says to check rather than shrug. Cause found: Phase 0's 46 new
hub files triggered the live cortex daemon's link action at 01:10:46
(`link: applied wikilink fixes to 1058 file(s)`), which inserted wikilinks
pointing at the new hubs across the corpus BEFORE the backfill ran. Those are
real `wikilink` membership edges, so more hubs cleared the claim-bearing bar
than the pre-Phase-0 disk simulation predicted. Not a defect: the extra bodies
are genuine membership, and the doc's own prediction was explicitly a
simulation of a state that the daemon then changed. Recorded because the band
is a stated tripwire.

### Note on two different member counts

`--asymmetry` reports `entities/claude.md` as `sources=436`, while its rendered
digest says `345 sources`. Both are correct and measure different things: the
report counts all deliberate members, the digest counts CLAIM-BEARING members,
which is what the doc pins `N`/`M` to.

### Acceptance criteria: no amendments

Every criterion was executed. None turned out to be a doc defect, so nothing in
the Acceptance Criteria section was rewritten. The one doc statement worth
re-checking (round 5's M1 evidence that all 15 `hub-synthesized:` bodies carry
Fabric boilerplate) was independently re-verified TRUE — an anchored grep
initially showed only 4/15, but the other 11 carry the same markers as markdown
headings (`## ONE SENTENCE SUMMARY:`), so the doc's conclusion stands and the
first grep was wrong.

### Vault git trail

`89d95fa` (tag `pre-hub-phase0`) → `7973c85` (46 stubs) → `b8a1bbc` (daemon
quality frontmatter) → `98dea63` (daemon link sweep, 1057 files) → `7f6fb95`
(522 deterministic hub bodies).

DB snapshots: `oracle.db.pre-phase0`, `oracle.db.pre-phase1-backfill`,
`oracle.db.pre-phase2`.
