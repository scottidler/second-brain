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
