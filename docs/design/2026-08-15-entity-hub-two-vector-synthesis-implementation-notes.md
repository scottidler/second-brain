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
