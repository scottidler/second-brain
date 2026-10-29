# Implementation Notes: Retrieval Quality from the RAG Scour

Design doc: `docs/design/2026-10-29-retrieval-quality-from-rag-scour.md`

## Phase 0: Prove an index arm, zero code

### Design decisions
- The bwrap shape is `bwrap --dev-bind / / --ro-bind ~/.local/share/sb/oracle ~/.local/share/sb/oracle -- env XDG_DATA_HOME=<arm> sb cortex embed -v ...`. The whole FS stays as it is (model cache in `~/.cache/huggingface`, vault, config, the `sb/borg` symlink target) and only the live oracle dir goes read-only. A `touch` probe inside the same bind returned `Read-only file system` before either embed run.
- The arm is `/media/saidler/4tb/claude-tmp/claude-1000/-home-saidler-repos-scottidler-second-brain/821cbe7f-1f30-43e6-8fa8-6acb8d69303f/scratchpad/arm0`, WAL checkpointed after the backfill so `arm0/sb/oracle/oracle.db` is self-contained. Later phases can use it as the Phase-0 arm (today's recipe, freshly re-embedded, minus the 17 masked notes below). A pristine copy of the pre-spike snapshot sits beside it at `scratchpad/snapshot.db`.
- Everything the embed process creates under the data home landed in the arm: `arm0/sb/cortex.log`, `arm0/sb/cortex/embed.lock`, the DB and its `-wal`/`-shm`. The embed lock lives under `XDG_DATA_HOME`, so an arm run never contends with the live daemon's lock. That is correct for a separate DB and means two arm runs on the same arm dir still serialize.
- Mined queries went to `scratchpad/mined-queries.yml` (331 queries). Nothing but the two `.md` files touched the repo.

### Deviations
- Success criterion 1 asks for the `-v` log to name the arm DB path. `sb cortex embed` never logs the DB path at any level (`cortex::embed::run` opens `config.oracle_db_path()` without a log line), so the criterion cannot be met with zero code. Substitute proof: `/proc/<pid>/fd` of the running embed held only `arm0/sb/oracle/oracle.db{,-wal,-shm}`, the `-v` log names arm-derived paths for the log file, `archive_root` and `staging_root`, and the live DB's max `produced_at` (1793291372) predates the spike start (1793299214). Phase 2 or 3 can add one DEBUG line naming the DB path in `cortex::embed::run`; that is code, so it was not done here.
- Success criterion 2c (no note with a pre-spike chunk row lacks one after) fails: 17 notes, 163 rows. The root cause is recorded in the design doc's Phase 0 Observed bullets. In short: stale chunk rows plus a current `embedding_examined` sentinel on notes whose transcript source is now empty or gone; `--drop-kind` removes rows but not sentinels, so `--backfill` never re-selects those notes. The 1% row-count criterion still passes (-0.83%) only because 163 is small.

### Tradeoffs
- Ran the embed inside the Claude Code bash sandbox as well as bwrap. The sandbox did not block anything embed needed (the run logged 0 WARN/ERROR in `arm0/sb/cortex.log`), and bwrap's own bind was proven with the `touch` probe, so the outer sandbox adds safety without muddying the proof.
- The backfill ran beside the live cortex daemon on the same CPU: 19,500 chunks in 15m36s (0.048 s/chunk vs the doc's 0.024 s/chunk on an idle ripr). `scanned=2720` exceeds the 2159 embedded notes because the 512-chunk per-tick allowance defers the tail of each 64-note batch and the next batch re-reads it (`per-tick allowance 512 chunk(s): embedding 52 note(s)/489 chunk(s) now, deferring the rest of 592 total`). Each batch boundary also spent 18 s in the stale-target query on this DB. Neither changes the arm's contents; both bear on arm build time in Phases 4-6.

### Open questions
- The 17 masked notes: the live index serves 163 chunk vectors whose transcript the system can no longer produce (12 traces have no `sources/` or `stages/` dir, 5 have a `distilled.yml` with no transcript). Phase 3's recipe drain hits the same path: the stale clause selects them, the read finds no source, the sentinel is written, and the old recipe-1 rows stay forever, which breaks the acceptance criterion "exactly one recipe per kind". Should a no-source examination delete that note's existing rows of the kind (my recommendation: yes, in Phase 3, with a test), or should those rows be kept as the only surviving trace of the transcript?
- Should `--drop-kind` also clear the kind's `embedding_examined` sentinels? Today it leaves a state where a backfill silently skips notes. Phase 3 makes `--drop-kind` a rollback-only lever, but the sentinel mask still bites any operator who uses it.
- Criterion 1 needs a DB-path log line to be checkable as worded. Add it in the first code phase that touches `cortex::embed` (Phase 3), or reword the criterion to accept the `/proc` fd proof?
