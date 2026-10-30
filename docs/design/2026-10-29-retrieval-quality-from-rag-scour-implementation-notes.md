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

## Phase 1: Operator query set

### Design decisions
- The curated set is `~/repos/scottidler/obsidian/system/eval/oracle-queries.yml`, written from the Phase-0 `mined-queries.yml` (331 queries) with the `mined-<fnv>` ids untouched, so every cached judgment for a kept query carries over. Same schema as `config/eval/queries.yml`. Nothing mined entered the second-brain repo; the vault git repo was not touched (Syncthing carries it).
- Curation, 331 -> 249: 53 dropped as one-word (fewer than 2 whitespace tokens), 29 dropped as session-specific or personal (Slack thread ids, `CLAUDE_PLUGIN_ROOT`/`CLAUDE_SKILL_DIR` tool internals, orchestration-session artifacts, contact-list/phone-number/youth-football roster lookups, internal host/network and personal-project queries), 0 dropped as duplicates (the miner already dedupes case- and whitespace-insensitively).
- The 20 `calibration: {}` queries are evenly spaced across the curated file (index `floor(i * 249 / 20)`), so the panel spans topics instead of clustering on the first themes mined.
- `oracle/AGENTS.md` now names the vault path as the operator set (path only, no counts or line numbers).

### Deviations
- `sb oracle eval --emit-calibration` judges every query in the set (all 249), not just the 20 calibration queries, because the sheet rows are emitted inside the per-query judging loop and the early return comes after it. So the first emit is the full LLM judging bill; it fills the judgment cache, which makes the criterion-1 full run mostly cache hits. Same effect as the doc, a different cost profile than the doc implies.
- The emitted sheet carries the LLM judge score in each row (`judge`) next to the empty `human` slot. Scott sees the judge's score while labelling, which weakens the "blind 0-3" property the committed 4 calibration queries had. Not changed here (code); see open questions.

### Tradeoffs
- Dropped by explicit substring list rather than a classifier: the list is auditable and the drop counts are reproducible, at the cost of a hand-picked list that fits this mined set only.
- Kept near-duplicate rephrasings (e.g. several "Karpathy autoresearch ..." variants): they are distinct queries with distinct ids and the eval wants ranking behavior on paraphrase; deduping them would also invalidate cached ids.

### Open questions
- Should `--emit-calibration` omit the judge score from the sheet so labelling is blind (the committed set's labels were blind)? That is a code change in `oracle/src/eval.rs` and belongs to Phase 2, where the calibration panel is re-run on the Phase-1 labels.
- `cortex::daemon::tests::test_duration_until_weekly_all_days` is wall-clock dependent: at 12:10 PDT on Thu 2026-10-29 it failed `otto ci` (the next `Thu 12:00` is 7 days + 1 hour minus the elapsed 10 minutes because DST ends 2026-11-01, which exceeds the test's 7-day bound). It passed on the rerun an hour later, and `otto ci` was green at commit. Untouched here; should the test be made clock-independent?

### Evidence (Phase 1, run on ripr 2026-10-29)
- Criterion 1: `sb oracle eval --queries ~/repos/scottidler/obsidian/system/eval/oracle-queries.yml` exit 0, `relevance eval @ K=10  (249 queries ...)`. Baseline on the operator set: vector nDCG@10 0.7633, hybrid 0.7798, configured 0.7634, graph-hybrid 0.5534, bm25 0.2731; `judgments: 4317 total, 0 from truncated bodies, 1 judge errors (pairs skipped)`. That 1 judge error is exactly the silent-low-score class the design names in problem 6.
- Calibration sheet: `~/repos/scottidler/obsidian/system/eval/oracle-calibration-sheet.yml`, 326 rows over 20 queries, every `human: null`. Criterion 2 (trust gate on > 67 labelled pairs) is PENDING-SCOTT: 326 > 67, so the pair count is sufficient once labelled.
- The first `--emit-calibration` run took about 80 minutes of fabric judging (the full pool of all 249 queries); the follow-up full run had only cache hits.
- There is no copy-back subcommand: the sheet's `human` values must be transcribed into each calibration query's `calibration:` map (note-path -> 0..3) in `oracle-queries.yml`. The gate is then read from the `calibration:` line of `sb oracle eval --queries <set>`.

## Phase 2: Paired A/B eval

### Design decisions
- Arm B is `--compare-config` (else arm A's `oracle.yml`) over `--compare-db` (else arm A's DB) (`oracle/src/eval.rs:open_compare_arm`). Both flags must name an existing file: `Config::load` silently falls back to defaults on a missing path and `SearchIndex::open` creates an empty DB, and either would compare against the wrong arm without a word.
- Judge text is attached after both arms retrieve (`eval.rs:attach_texts`). Each pooled note gets ONE text, from arm A's index first and arm B's only for a note A does not hold, so a note scores the same whichever arm surfaced it.
- Paired pool per (query, target) = that target's arm-A top-K ∪ arm-B top-K; IDCG from that pool only (`eval/paired.rs:paired_ndcg`). The legacy single-arm table is arm A only, scored on arm A's all-target pool (`evaluate` restricts the judgments to `legacy_pool`), so a compare run's legacy table matches a plain run's exactly.
- Split: `fnv1a(id)` even is tune, odd is confirm (`paired.rs:Split::of`). `cache::fnv1a` is the number behind `stable_hash`, so the split hash and the cache/mined-id hash are the same function.
- CI: percentile bootstrap, 10,000 resamples, seed `0x20261029`, an in-house SplitMix64 PRNG (`paired.rs:bootstrap_ci`). Lower/upper are the 250th and 9,750th sorted resampled means.
- MDD: `2.8 * sd / sqrt(n)` with the sample sd (n-1) (`paired.rs:mdd`). Zero variance is tested on the samples (all deltas equal), not on `sd == 0`: three deltas of 0.2 gave sd 5.5e-17 from rounding and would have printed an MDD instead of `insufficient`. Caught by `mdd_is_insufficient_for_n_le_one_or_zero_variance`.
- Verdict per target and split, same rule on both splits (`paired.rs:verdict`). The rendered table says the ship decision reads the confirm rows; tune rows are for picking settings.
- Latency budget is an input, `--latency-budget-ms <MS>`: set, `ship` also needs arm B's p95 for that target and split <= the budget; unset, p95 A and B are printed and the header says `latency: reported, NOT gated`. Given without a compare flag, `sb` errors before doing any work.
- Fail on missing judgment applies to EVERY run, single-arm too (the decision rule says any pooled note). `evaluate` keeps judging the whole pool after a failure, caching every success, then fails listing up to 10 failures, so a rerun retries only those. A pooled note with no judge text (in neither arm's index) is a missing judgment as well. `EvalReport.judge_errors` is gone: it could only ever be 0.
- Judge text (`eval/calc.rs:prepare_note_text`): summary, then `"\n\n"`, then one `- <claim>` line per claim, all flattened; a claim that does not fit whole is dropped with every later one. Claims come from `vault::search::parse_body_claims(note.body)`, the same parser that fills `notes.claims` at index time (`NoteRow` carries no claims field). The `truncated` flag still marks only the body fallback.
- The judge's fabric input budget is now `max_chars` of note text plus the query/title framing (`eval/judge.rs:judge_input_budget`). Found on the live run: `FabricJudge` capped the WHOLE input at 8000, so `vault::fabric` cut the tail of every note text near the cap (`Truncating input from 8119 to 8000 chars` in `oracle.log`). With summary+claims filling to 8000 that would split the last claim the boundary rule kept whole.
- The paired table reports each arm's `configured` rerank tally (`configured rerank: arm A off, arm B ran 248/248`) and warns when an arm's enabled stage did not run on every query, the same guard the single-arm table already had. Without it a latched-off arm B would read as a rerank-on result.
- `--compare-db`/`--compare-config` conflict with `--emit-calibration` and `--mine-queries` (clap), and with `--target answerability` (an error in `sb`).

### Deviations
- `--latency-budget-ms` is not in the doc's API block. The doc puts the budget in each phase, so it is an input rather than a constant; unset means reported, not gated.
- `judge_input_budget` is not in the doc. Without it the doc's "claims truncated at a claim boundary" is false on the wire for any note near the cap.
- Criterion 1 reads "delta 0.000 and CI [0.000, 0.000]"; the table prints 4 decimals (`+0.0000`, `[+0.0000, +0.0000]`). The test asserts exact `0.0` and `(0.0, 0.0)`, which is stronger. Same-arm rows are `insufficient` (zero variance), as the decision rule says.
- The rerank first-use run on the full operator set FAILS (exit 1), by design: one pooled pair has no judgment (root cause below). The verdict table below comes from a scratch copy of the set with that one query removed (248 queries, `.../scratchpad/oracle-queries-minus-refused.yml`, diff is exactly the two lines of `mined-736d308b04a79ca1`). The vault set is untouched.
- Calibration panel "before and after": PENDING-SCOTT. Every `calibration:` map in the operator set is still empty (Scott is labelling the blind sheet). Commands once the labels are copied into `oracle-queries.yml`:
  - before (summary-only judge text): the v0.20.0 binary, pinned at `.../scratchpad/sb-v0.20.0-installed` and still the installed `sb` until a deploy: `sb oracle eval --queries ~/repos/scottidler/obsidian/system/eval/oracle-queries.yml`, read the `calibration:` line. After a deploy, build it from `f28710b` (`git worktree add <dir> f28710b && cargo build --release -p sb`).
  - after (summary + claims): the same command with the Phase-2 binary (`target/release/sb` from this commit, or `sb` after deploy).
  - Both are cache hits for every judgment already made; the cache keys on content hash, so the two judge texts never collide. Both runs fail on the refused pair until it is resolved (open question 1).
- Phase 0's open question (a DB-path log line in `cortex::embed::run`) stays with Phase 3; this phase does not touch cortex.

### Tradeoffs
- Arm A retrieves every query, then arm B, in one process: vs interleaving per query. Simpler, and it is one run, which is what the cpu-tier doc requires. The cost is that load drift over the ~7 minutes between the two passes lands in the latency columns; the twin runs below agree within 50 ms on the one p95 that matters.
- SplitMix64 written out (12 lines) vs the `rand` crate. No new dependency, and the bootstrap stays bit-reproducible across crate upgrades.
- Collect every missing judgment, then fail: vs failing on the first. One run surfaces the whole failure set and caches every success, so a 90-minute judging pass is never thrown away for one bad pair.
- Judge text from arm A's index first: vs from whichever arm surfaced the note. Arms are cut from one snapshot, so the text is the same either way; picking A first makes it deterministic when an index arm's notes table has drifted.

### Open questions
- **The refused pair.** `mined-736d308b04a79ca1` ("Pluto Security function hooks credentials") x `notes/mini-shai-hulud-worm-compromises-tanstack-mistral-ai-guardrails-ai-more-packages.md`. Root cause, measured: fabric's default model is `claude-sonnet-5` (`fabric --dry-run`: `Model: claude-sonnet-5`); a direct Messages API call with the same system prompt and input returns `stop_reason: "refusal"`, `stop_details.category: "cyber"`, `content: []`. fabric reports that as `empty response`. It is deterministic (3/3 via fabric, both Phase-1 runs, this phase's run) and pair-specific: the same note with the query "npm supply chain attack TanStack" scores 3, and the same query with an unrelated note scores 0. Phase 1 counted it as a skipped pair, which silently scored the note 0 on every list. Options: drop the query from the operator set (the calibrated judge cannot grade its pool); or have `FabricJudge` retry a refusal on a configured fallback model (mixes judges inside one pool); or Anthropic's Cyber Verification Program for the judge key. Recommendation: drop the query; it is Scott's vault file, so it is his call.
- The calibration sheet question from Phase 1 is settled in practice: `oracle-calibration-sheet-blind.yml` exists beside the original.

### Evidence (Phase 2, ripr 2026-10-29)
- Tests named in the doc, all in `oracle/src/eval/tests.rs`, MockJudge:
  - `compare_same_arm_zero_delta`: every target, both splits, delta 0.0 and CI (0.0, 0.0), verdict `insufficient`.
  - `compare_target_set_does_not_rebase`: adding a `graph` target that surfaces a relevant note leaves `vector` and `configured` paired (nDCG A, nDCG B, delta, CI, n) identical on both splits, AND drops both legacy nDCG means (re-based).
  - `compare_missing_judgment_fails`: a MockJudge that drops one note only arm B surfaces makes `evaluate` return `Err` naming `p1 / notes/z.md`.
- Shown failing with the change reverted (sources backed up, mutated, tested, restored byte-identical):
  - paired table computed but dropped (`.ok().filter(|_| false)`) -> `compare_same_arm_zero_delta` FAILED ("a paired run carries the paired table").
  - zero-variance `insufficient` check removed from `verdict` -> `compare_same_arm_zero_delta` FAILED at the verdict assert.
  - `paired_ndcg` scoring on all judgments instead of the A∪B pool -> `compare_target_set_does_not_rebase` FAILED ("vector/tune paired numbers moved when a target was added").
  - the fail-on-missing `bail!` disabled (old skip behaviour) -> `compare_missing_judgment_fails` FAILED.
- The old `judge_errors_are_counted_and_reported` is inverted by name as `judge_errors_fail_the_run`; `pooled_note_without_judge_text_fails_the_run` covers the no-text case.
- Full operator set (249 queries), `sb oracle eval --queries ~/repos/scottidler/obsidian/system/eval/oracle-queries.yml --compare-config <scratchpad>/arm-rerank-oracle.yml --latency-budget-ms 1500`: exit 1 after 5129 s, `Error: 1 pooled note(s) have no judgment; the run fails rather than score them 0. ... mined-736d308b04a79ca1 / notes/mini-shai-hulud-...md: fabric -p judge-relevance failed: empty response`. This was the one-time re-judge under the new judge text (eval cache 6,513 -> 10,038 rows). Arm B = live `oracle.yml` + `retrieval: {rerank: {enabled: true}}`.
- 248-query scratch set, same flags, run twice (731 s and 751 s, all cache hits), identical nDCG and CI both times; `configured rerank: arm A off, arm B ran 248/248`. This is Phase 7's baseline. No shipped default changed.

  | target | split | nDCG A | nDCG B | delta | 95% CI | n | MDD | p95 A ms | p95 B ms | verdict |
  |---|---|---|---|---|---|---|---|---|---|---|
  | configured | tune | 0.6388 | 0.7801 | +0.1413 | [+0.0889, +0.1948] | 122 | 0.0749 | 36.5 | 767.1 | ship |
  | configured | confirm | 0.6371 | 0.7881 | +0.1509 | [+0.0873, +0.2149] | 99 | 0.0913 | 37.6 | 767.1 | ship |
  | vector | tune / confirm | 0.8167 / 0.8202 | same | +0.0000 | [0, 0] | 117 / 95 | insufficient | 36.1 / 36.4 | 38.3 / 37.8 | insufficient |
  | hybrid | tune / confirm | 0.8076 / 0.8208 | same | +0.0000 | [0, 0] | 119 / 96 | insufficient | 36.1 / 37.3 | 37.5 / 38.1 | insufficient |
  | bm25 | tune / confirm | 0.9111 / 0.8903 | same | +0.0000 | [0, 0] | 47 / 45 | insufficient | 1.1 / 1.1 | 1.4 / 1.4 | insufficient |
  | graph | tune / confirm | 0.5883 / 0.5864 | same | +0.0000 | [0, 0] | 113 / 93 | insufficient | 425.9 / 425.9 | 435.6 / 441.0 | insufficient |
  | graph-hybrid | tune / confirm | 0.6896 / 0.7081 | same | +0.0000 | [0, 0] | 113 / 93 | insufficient | 430.6 / 432.0 | 429.5 / 434.5 | insufficient |
  | graph-hybrid (no fact) | tune / confirm | 0.6896 / 0.7081 | same | +0.0000 | [0, 0] | 113 / 93 | insufficient | 494.2 / 496.6 | 492.9 / 511.0 | insufficient |

  - Rerank clears the pre-registered rule on confirm: CI lower bound +0.0873 > 0, arm B p95 767 ms <= 1500. The other targets do not read the config knob, so their arms are identical and `insufficient` is the correct verdict, not a defect.
  - The confirm MDD (0.0913) is above the doc's 0.02 planning threshold for a target that moves. Rerank's effect is large enough to clear it; smaller changes (Phases 4-6, 8) will likely not be detectable on 99 confirm queries. The doc's risk table says to grow the set before Phase 4 when MDD > 0.02.
  - Paired n is smaller than the legacy n (215 per mode) because a paired pool holds only that target's two lists: a query whose own A∪B pool has no relevant note is excluded. bm25's n of 47/45 is that effect at its strongest.
- Single-arm table (arm A, all-target pool, summary + claims judge text, 248 queries): vector 0.7561, hybrid 0.7751, configured 0.7563, graph-hybrid 0.5439, bm25 0.2678; `judgments: 5662 total, 0 from truncated bodies`. Phase 1's summary-only numbers on 249 queries were vector 0.7633, hybrid 0.7798, configured 0.7634; the absolute shift is the judge-text change the risk table predicts, and decisions use paired deltas only.
- `otto ci` green at commit. Arm B config, both reports and the run logs stay in the session scratchpad (`rerank3-report.txt` is the final report); nothing but `.md` under `docs/`.

## Phase 2 follow-up: judge refusal

### Design decisions
- Scott chose (2026-10-29) to drop query `mined-736d308b04a79ca1` ("Pluto Security function hooks credentials") from the vault operator set, over a fallback judge model. `claude-sonnet-5` refuses the pair with that query and `notes/mini-shai-hulud-worm-compromises-tanstack-mistral-ai-guardrails-ai-more-packages.md` (`stop_reason: refusal`, `category: cyber`), 3 of 3 attempts. The set is now 248 queries. A future refusal fails the run loudly, by design.

### Deviations
- None.

### Tradeoffs
- Dropping one query vs a fallback judge: a fallback mixes two judge models into one calibration.

### Open questions
- None.

## Phase 3: Recipe versioning

### Design decisions
- `cortex/src/embed.rs` went from 1449 to ~630 lines before any recipe work: one file per kind (`embed/summary.rs`, `embed/transcript.rs`, `embed/claim.rs`), plus `embed/allowance.rs` (`ChunkAllowance`, `cap_work_by_chunks`, `TranscriptWork`), `embed/inference.rs` (the dedicated pool, `embed_in_sub_batches`) and `embed/lock.rs`. The cap and allowance tests moved to the sibling `embed/allowance/tests.rs`, which brought `embed/tests.rs` from 1499 back to ~1390 lines. Pure move: the embed test count was 58 before and after the split.
- The recipe constants live in vault: `vault::search::{SUMMARY_RECIPE, TRANSCRIPT_CHUNK_RECIPE, CLAIM_RECIPE}`, all 1, read through `EmbeddingKind::recipe()`. Every write path stamps `kind.recipe()` itself (`upsert_embedding`, `upsert_embeddings_batch` and `swap_embeddings_batch` through `insert_batch_item`, `swap_kind_chunks`, `mark_nothing_to_embed_batch`), and `stale_embedding_targets` / `sentinelled_notes` compare against the same call. No caller passes a recipe, so a writer and the stale query cannot disagree.
- Stale clause `OR e.recipe < ?4` and sentinel clause `OR x.recipe < ?4` on all three arms; `SELECT DISTINCT` on the note columns before `ORDER BY ... LIMIT` (`vault/src/search/vector.rs:stale_embedding_targets`). `sentinelled_notes` only reports sentinels at the current recipe, so the archive revival never clears a sentinel the stale query already ignores.
- Schema: `recipe INTEGER NOT NULL DEFAULT 1` in both `CREATE TABLE`s, plus `ensure_recipe_columns` (`vault/src/search/schema.rs`) for existing DBs, the same `PRAGMA table_info` + `ALTER ADD COLUMN` pattern as the notes-column migrations. It runs after the claim-kind rebuild, whose copy lists the pre-recipe columns.
- Per-note summary swap: new `swap_embeddings_batch` (`vector.rs`), one `BEGIN IMMEDIATE` transaction, DELETE each note's rows of the item's kind and model, then INSERT. `process_summary_batch` writes through it. The 200 ms transaction test now times this writer, since it is the one production uses.
- Scott's option A: `mark_embedding_examined_batch` is renamed `mark_nothing_to_embed_batch` and deletes the note's rows of that kind and model in the sentinel's transaction, logging INFO with the count when it clears any. The old name described half of what it now does.
- `--drop-kind`: `SearchIndex::delete_embeddings_of_kind` is renamed `drop_embedding_kind` and deletes the kind's rows and its `embedding_examined` sentinels (all models) in one transaction, returning `DroppedKind { rows, sentinels }`. Sentinels are kind-keyed (`PRIMARY KEY (note_path, kind, model_version)`), so only that kind's go. `sb cortex embed --drop-kind` prints both counts.
- `cortex::embed::run` logs `cortex::embed::run: oracle db=<path>` at DEBUG; `drop_kind` logs the same at DEBUG. Phase 0 had to prove arm isolation from `/proc/<pid>/fd` because the path was never logged; it is now in the `-v` output (evidence below).
- New index `idx_note_embeddings_note_kind_model ON note_embeddings(note_path, kind, model_version)`, created by `ensure_recipe_columns`. It is the root cause fix for Phase 0's 18 s batch boundary (see Tradeoffs and Evidence).

### Deviations
- Recipe constants sit in `vault::search`, not `cortex::embed` (the doc's Architecture names both crates). Same effect, correct seam: the four write sites and the stale query are vault code, so the constant they compare against lives beside them. Phase 4's chunker constants are also vault constants per the doc. A change to `cortex::embed::summary_embed_text` must bump `SUMMARY_RECIPE` in vault; the constant's doc comment names that function.
- The stale-join index is not in the doc. Without it the DISTINCT query is as slow as the old one (21-22 s vs 23-24 s on the snapshot); with it both are ~20 ms. Adding a column without fixing the query it feeds would ship the doc's DISTINCT with the same 18-24 s boundary Phase 0 measured.
- `swap_embeddings_batch` deletes only the item's `model_version`, while `swap_kind_chunks` deletes every model's chunk rows. The summary path never deleted another model's row (it was `INSERT OR REPLACE`), and keeping it that way leaves an A/B model flip's summaries alone. `upsert_embeddings_batch` stays (stamped) for the bench and regression fixtures; no production path calls it.
- Two renames the doc does not name: `mark_embedding_examined_batch` -> `mark_nothing_to_embed_batch`, `delete_embeddings_of_kind` -> `drop_embedding_kind`. Both functions changed behavior; the old names would have lied.
- A recipe bump in tests is simulated by restamping stored rows and sentinels one below the constant (test-util `SearchIndex::set_test_recipe`), because the constants are compile-time. The stale query sees exactly that state after a real bump.
- Stamping cannot be observed in a test while every constant equals the column default (1): an unstamped write lands at 1 either way. `recipe_reembed_is_not_stale_next_tick` asserts every row carries `kind.recipe()`, so it fails on an unstamped write site the moment Phase 4 sets transcript-chunk or claim to 2. The sentinel stamp is observable now (its `ON CONFLICT DO UPDATE` keeps the old value if unstamped) and is mutation-tested.

### Tradeoffs
- `SELECT DISTINCT` (the doc's form) vs `EXISTS` subqueries. Measured on the snapshot without the index: old 24.0 s, DISTINCT 21.4 s, EXISTS 19.5 s (11.8 s bumped). With the index all three are 10-30 ms, so the doc's form stays.
- One transaction for the summary swap (DELETE + INSERT per note) vs one `swap_kind_chunks` call per note. One transaction keeps the summary batch's single short write, which the 200 ms test pins.
- Deleting no-source rows scoped to the sentinel's model vs every model. Scoped: the sentinel itself is per model, and another model's rows are not served under this pin.

### Open questions
- Superseded notes are outside `stale_embedding_targets` (`superseded_by` filter), so their rows never re-embed. The snapshot has exactly 1 such row (a summary). A future `SUMMARY_RECIPE` bump would leave it at the old recipe and break "exactly one recipe per kind" for summary. Phases 4-5 bump only transcript-chunk and claim, so the acceptance criterion is unaffected. `search_vector` does not filter on `superseded_by`, so those rows are still served. Should a superseded note's rows be deleted?
- The 17 Phase-0 notes on live: their sentinels are current today, so nothing touches them until the Phase-5 recipe-2 drain, when the old-recipe sentinel stops masking them, the read finds no source, and `mark_nothing_to_embed_batch` deletes their 163 rows. That is the intended path; it means the live index keeps serving those 163 vectors until Phase 5 deploys.
- The live daemon on ripr pays roughly 10 s (summary) + 23 s (transcript-chunk) of stale query per embed batch until a binary with the new index is deployed. No deploy here; Phase 5 is the deploy. Worth shipping earlier?

### Evidence (Phase 3, ripr 2026-10-29)
- Tests, all green in `otto ci`:
  - `vault/src/search/vector/tests/recipe.rs`: `recipe_bump_marks_only_that_kind_stale`, `stale_targets_distinct`, `nothing_to_embed_clears_that_kinds_rows_only`, `drop_kind_clears_that_kinds_sentinels`, `summary_swap_replaces_the_notes_rows_for_that_model_only`, `sentinelled_notes_skip_an_old_recipe_sentinel`
  - `cortex/src/embed/tests/recipe.rs`: `recipe_reembed_is_not_stale_next_tick` (summary and transcript-chunk, a no-source note included), `empty_source_clears_old_rows`
  - `vault/src/search/schema/tests.rs`: `recipe_migration_adds_columns_reading_one_and_is_idempotent`
  - `drop_embedding_kind_removes_only_that_kind` is the renamed `delete_embeddings_of_kind_removes_only_that_kind`, asserting `dropped.rows`.
- Shown failing with the change reverted (vector.rs / schema.rs backed up, mutated, tested, restored, `cmp` clean):

  | mutation | fails |
  |---|---|
  | A: drop `OR e.recipe < ?4` | `recipe_bump_marks_only_that_kind_stale`, `recipe_reembed_is_not_stale_next_tick` |
  | B: drop `OR x.recipe < ?4` | `recipe_bump_marks_only_that_kind_stale`, `recipe_reembed_is_not_stale_next_tick` |
  | C: drop `DISTINCT` | `stale_targets_distinct` (+ 3 others that count targets) |
  | D: no row DELETE in the sentinel write | `empty_source_clears_old_rows`, `nothing_to_embed_clears_that_kinds_rows_only` |
  | E: no sentinel DELETE in the drop | `drop_kind_clears_that_kinds_sentinels` |
  | F: sentinel upsert keeps the old recipe | `recipe_reembed_is_not_stale_next_tick` |
  | G: summary swap without its DELETE | `summary_swap_replaces_the_notes_rows_for_that_model_only` |
  | H: `sentinelled_notes` without the recipe filter | `sentinelled_notes_skip_an_old_recipe_sentinel` |
  | I: no stale-join index | `recipe_migration_adds_columns_reading_one_and_is_idempotent` |

- Migration on a copy of the Phase-0 snapshot (`scratchpad/phase3/arm3/sb/oracle/oracle.db`), real binary, same bwrap shape as Phase 0 (live oracle dir read-only): `sb cortex embed -v --drop-kind claim` exit 0, 0.7 s wall including the migration, log `adding note_embeddings.recipe (existing rows read as recipe 1)` and the same for `embedding_examined`. Census after: `summary|1|4922`, `transcript-chunk|1|19663`, sentinels `transcript-chunk|1|1598`. Live `oracle.db` mtime unchanged across both runs (1793318879).
- DB path at `-v`: `sb cortex embed -v --kind summary` on the same arm logged `[DEBUG] cortex::embed: cortex::embed::run: oracle db=.../scratchpad/phase3/arm3/sb/oracle/oracle.db`, exit 0, `scanned=0`, 0.9 s total.
- Stale query timing, sqlite3 `.timer`, LIMIT 64, live model id; old SQL on the pre-migration copy vs new SQL on the migrated copy:

  | kind | old | new, recipe current | new, recipe bumped (64 distinct of 64) |
  |---|---|---|---|
  | summary | 9.913 s | 0.014 s | 0.001 s |
  | transcript-chunk | 23.066 s | 0.023 s | 0.032 s |
  | claim | 0.001 s | 0.002 s | 0.001 s |

  Plan before: `SEARCH e USING INDEX idx_note_embeddings_kind_model (kind=? AND model_version=?)`, i.e. every chunk row of the model visited once per candidate note. After: `SEARCH e USING INDEX idx_note_embeddings_note_kind_model (note_path=? AND kind=? AND model_version=?)`. `search_vector`'s scan plan is unchanged (`SCAN e`, 22-23 ms full-row read on both copies).

## Pre-Phase 4: eval size ruling

### Design decisions
- Scott ruled (2026-10-29) to proceed at 248 queries rather than grow the set. Phase 2's rerank arm measured confirm MDD 0.0913 at n=99, so sd(delta) is about 0.32 and a 0.02 MDD needs about 2,060 confirm queries; mining yields 331. The risk row and Resolved Decisions are amended. The ship rule (CI lower bound > 0 on confirm) is unchanged.

### Deviations
- The risk-table mitigation "grow the set before Phase 4 if MDD > 0.02" is replaced by the ruling above.

### Tradeoffs
- LLM-synthesized queries vs a smaller honest set: synthesized queries derived from notes favor those notes.

### Open questions
- None.

## Phase 4: Token-accurate chunker

### Design decisions
- The chunker is `vault::embedding::chunk` (`chunk.rs`): `windows(segmenter, max_input_tokens, prefix, text) -> Result<Vec<Window>>` does the work and `chunk(...)` returns only the texts. It runs over a `Segmenter` port (`token_spans`: byte span per token, specials excluded; `count_tokens`: encoded length with `[CLS]`/`[SEP]`, truncation off), so one algorithm serves the real tokenizer (`TokenChunker`) and the mock (`WhitespaceSegmenter`, one token per word, test-util only).
- Window capacity = `max_input_tokens - SPECIAL_TOKENS - tokens(prefix)`, after the prefix is cut at a token boundary to `PREFIX_MAX_TOKENS`. A capacity at or below `CHUNK_OVERLAP_TOKENS` is an error. Each window is the verbatim byte slice from its first token's start to its last token's end; the final `prefix + "\n\n" + slice` is re-encoded and the window shrinks one token at a time until it fits. The next window starts `CHUNK_OVERLAP_TOKENS` before the previous end (never at or before the previous start).
- `CHUNK_OVERLAP_TOKENS = 64`, `PREFIX_MAX_TOKENS = 64` and `SPECIAL_TOKENS = 2` are vault constants re-exported from `vault::embedding`; `TRANSCRIPT_CHUNK_RECIPE`'s doc comment names them. `CHUNK_MAX_TOKENS` is gone, and so is the word chunker `vault::embedding::chunk_transcript` and its tests, and cortex's `CLAIM_GROUP_MAX_WORDS`.
- `EmbeddingModel` gains `max_input_tokens`, `count_tokens` and `chunk`, all required (no default), so no implementor can silently fall back to a word count. Candle and ONNX return their `MAX_SEQ_LEN` (512) and delegate to a `TokenChunker` loaded from the same `tokenizer.json` with truncation and padding off (`candle.rs` / `onnx.rs` `load_version`). `MockEmbedder` defaults to a 512 window (`MOCK_MAX_INPUT_TOKENS`) with `with_max_input_tokens` for tests.
- `cortex::embed::transcript` calls `model.chunk("", &transcript)`; the prefix is Phase 5. On a tokenizer error it logs ERROR naming the note, counts it `failed`, and writes no rows and no sentinel, so the old rows keep serving and the note is retried next tick.
- `cortex::embed::claim::group_claims(model, text) -> Result<Vec<String>>` packs claims in order and accepts a group only after re-encoding the exact `\n`-joined string; a single claim over the window is windowed by `model.chunk` into its own groups instead of being truncated. Same error handling as transcripts.
- `cortex::embed::summary`: every summary text is counted; over the window logs one WARN per embed naming the note path, token count and window (`summary text of <path> is <n> tokens, over the 512-token window; only its head is embedded`). A tokenizer error there skips the note the same way.
- `TRANSCRIPT_CHUNK_RECIPE` 1 -> 2 and `CLAIM_RECIPE` 1 -> 2 (`vault/src/search/vector.rs`), each with a comment saying what recipe 1 and 2 are. `recipe_reembed_is_not_stale_next_tick` now observes stamping for transcript chunks (rows at 2, column default 1) and passes.
- `vault/AGENTS.md` names the chunk module, the trait methods and the recipe rule.

### Deviations
- The trait gets a third method, `count_tokens`, beyond the doc's two. `group_claims` has to measure each candidate group and the summary WARN has to measure the summary; neither is a windowing call. Same effect, correct seam.
- The backends do not reuse their inference tokenizer for chunking: it carries truncation 512 and batch padding. Each holds a second `Tokenizer` from the same file with both off.
- The doc says a tokenizer error skips the note for chunking; the summary count applies the same rule (an un-tokenizable summary would not embed either).
- An oversize single claim is windowed rather than embedded alone and truncated (the old behavior). "Never truncated" applies to claims too.
- The two named tests are a cortex integration test, `cortex/tests/chunker.rs`, because the committed fixture lives in `cortex/tests/fixtures/`; they drive vault's real `TokenChunker` and check every window with an independent fixture tokenizer.
- Phase 3's `backend_free_migration_renames_the_pin_and_rows_without_a_reembed` seeded recipe-less rows and asserted nothing is stale; with two kinds at recipe 2 those rows are stale for the recipe, which is correct. The test now stamps the current recipe before its staleness assertion so it still checks the rename and only the rename.
- Arm A is the snapshot copy opened by the new binary, not re-embedded. The open runs the Phase-3 migration (two `recipe` columns and the stale-join index); `search_vector` reads neither, so retrieval is the snapshot's. Arm A's legacy single-arm numbers match Phase 2's to the fourth decimal (vector 0.7561, hybrid 0.7751, configured 0.7563), which is the check.
- The eval cache lives beside arm A's DB (`eval.rs:eval_cache_path`), so arm A carries a `.backup` copy of the live `eval-cache.db` (10,038 rows); the 845 new judgments went into that copy, not the live cache.

### Tradeoffs
- Shrinking one token at a time (the doc's rule) vs a binary search: overflow after a mid-word cut is one or two tokens, so a linear shrink re-encodes one or two extra times per window.
- A tokenizer failure leaves the note unsentinelled (retried every tick, loud every tick) vs sentinelling it (quiet, but a transient error would hide the note until it is edited). Loud wins; the drain loop already halts on a zero-progress batch.
- Generic `Segmenter` port vs a tokenizer-only chunker: the port lets `MockEmbedder` run the same windowing code in every cortex test, so the batch tests exercise the real algorithm.

### Open questions
- The paired verdict is `hold` with every non-bm25 delta negative; tune is significantly negative for vector, hybrid and configured. Measured: chunk rows went 19,679 -> 25,802 and rows per chunked note 9.0 -> 11.9 (2,178 -> 2,161 notes). Hypothesis, not verified: more chunk vectors per transcript note give it more chances to win the min-distance pooling (problem 3), crowding out summary-matched notes. Evidence that would confirm it: per-query top-10 kind attribution (which row kind won each pooled note) on arms A and B. Phase 5 measures against this arm, and Phase 6's kind offsets are the lever the doc already plans; whether Phase 5 should still ship 4+5 together if 4+5 vs live is also `hold` is Scott's call under the decision rule.
- 1,324 transcript-chunk sentinels remain at recipe 1 in arm B, all on notes that now have recipe-2 rows; a second `--kind transcript-chunk` pass scanned 0, so they are inert. They clear only when those notes go sourceless again. Worth a cleanup in the sentinel write?

### Evidence (Phase 4, ripr 2026-10-29)
- Tests, all green in `otto ci`:
  - `cortex/tests/chunker.rs`: `chunk_fits_model_window` (fixture tokenizer, truncation off; a 20,441-token input with a no-whitespace URL run (400 path segments plus a 300-char query) every fourth round, long German and English compounds, accented and CJK text, code punctuation; prefixes empty, a sentence, and 128 words cut to 64 tokens; every chunk <= 512 with `[CLS]`/`[SEP]` and the longest > 496) and `chunk_overlap_has_no_gaps` (first window at token 0, last at the final token, consecutive windows advance and share 1..=64 source tokens, every window equals its verbatim source slice).
  - `vault/src/embedding/chunk/tests.rs`: blank input, verbatim single window, exact-capacity boundary, stride 446, prefix capacity, prefix cut at `PREFIX_MAX_TOKENS`, capacity-below-overlap error, multibyte slicing, shrink-on-inflated-re-encode, tokenizer error returned.
  - `cortex/src/embed/tests/chunking.rs`: `tokenizer_error_skips_the_note_without_rows_or_sentinel` (transcript, claim and summary paths) and `oversize_summary_warns_with_path_and_token_count`. `group_claims_fits_the_token_window` replaces `group_claims_splits_by_word_budget` (the oversize-claim behavior changed by design).
- Shown failing with the change reverted (`vault/src/embedding/chunk.rs` backed up, mutated, tested, restored, `cmp` clean):

  | mutation | fails |
  |---|---|
  | M1: accept every window without the re-encode shrink | `chunk_fits_model_window`: `chunk 10 (prefix 0 bytes) encodes to 513 tokens with [CLS]/[SEP], over 512` |
  | M2: `chunk` replaced by the recipe-1 400-word / 350-stride window | `chunk_fits_model_window`: `chunk 0 (prefix 0 bytes) encodes to 13932 tokens` |
  | M3: next window starts at the previous end (no overlap) | `chunk_overlap_has_no_gaps`: `consecutive windows share at least one source token: 0..510 then 510..1020` |
  | M4: next window starts one past the previous end (a gap) | `chunk_overlap_has_no_gaps`: `0..510 then 511..1021` |

  M1 is the WordPiece re-tokenization risk the doc names, observed on the fixture: a cut slice re-encoded one token longer than its span.
- Index arm, same shape as Phase 0 (`$P4` = `.../scratchpad/p4`; `bwrap --dev-bind / / --ro-bind ~/.local/share/sb/oracle ~/.local/share/sb/oracle`; the `touch` probe returned `Read-only file system`; `cortex.yml` has neither `archive-root` nor `staging-root`):
  - Binary: `target/release/sb` built from this commit's tree (`sb v0.20.0-7-gc6e0f86`), pinned as `$P4/sb-phase4`.
  - Snapshot `2026-10-30T00:32:09Z`: `sqlite3 ~/.local/share/sb/oracle/oracle.db ".backup $P4/snapshot.db"` -> `summary|4924|4924`, `transcript-chunk|19679|2178` (rows, notes), no `recipe` column. Arm A = `$P4/armA/sb/oracle/oracle.db`, arm B = `$P4/armB/sb/oracle/oracle.db`, both byte-copies of the snapshot (same sha256), each with `sb/borg` symlinked to the live borg dir.
  - Arm B: `XDG_DATA_HOME=$P4/armB sb cortex embed -v --kind transcript-chunk --backfill`, 17:36:33 -> 17:47:59 PDT: migration logged, `oracle db=$P4/armB/...` at DEBUG, `embed complete: scanned=3498 embedded=25802 skipped_empty=274 failed=0`, exit 0, 0 ERROR lines (the WARNs are missing or transcript-less sources). A second `--kind transcript-chunk` pass: `scanned=0`.
  - Arm B census: `summary|1|4924`, `transcript-chunk|2|25802` over 2,161 notes; no transcript-chunk row at any other recipe; `min(produced_at)` after the run start. The snapshot's masked set (stale chunk rows behind a current sentinel) is again 17 notes / 163 rows; arm B holds 0 rows for them, and they are exactly the 17 snapshot notes with no chunk rows in arm B (`mark_nothing_to_embed_batch ... cleared N old row(s)` in the log).
  - Live DB untouched: still no `recipe` column after both runs.
- Token-count command (Acceptance Criteria) against arm B: `rows 25802 over512 0`. Same script with a distribution: snapshot `rows 19679 median 650 p90 746 max 20703 over512 17849 tokens_not_embedded 3066549`; arm B `rows 25802 median 512 p90 512 max 512 over512 0 tokens_not_embedded 0`.
- Paired eval, 17:48:59 -> 18:15:20 PDT, exit 0, no judge refusal: `XDG_DATA_HOME=$P4/armA sb oracle eval --queries ~/repos/scottidler/obsidian/system/eval/oracle-queries.yml --compare-db $P4/armB/sb/oracle/oracle.db` inside the same bwrap (248 queries, live `oracle.yml` on both arms, latency reported not gated: Phase 4 names no budget). Judgments 5,249; arm A's cache copy 10,038 -> 10,883.

  | target | split | nDCG A | nDCG B | delta | 95% CI | n | MDD | p95 A ms | p95 B ms | verdict |
  |---|---|---|---|---|---|---|---|---|---|---|
  | vector | tune | 0.7565 | 0.7168 | -0.0397 | [-0.0741, -0.0066] | 119 | 0.0482 | 47.0 | 49.8 | hold |
  | vector | confirm | 0.7603 | 0.7293 | -0.0310 | [-0.0720, +0.0076] | 96 | 0.0573 | 43.8 | 49.2 | hold |
  | configured | tune | 0.7566 | 0.7169 | -0.0397 | [-0.0741, -0.0066] | 119 | 0.0482 | 43.7 | 43.1 | hold |
  | configured | confirm | 0.7603 | 0.7293 | -0.0310 | [-0.0720, +0.0076] | 96 | 0.0573 | 44.2 | 43.5 | hold |
  | hybrid | tune | 0.7683 | 0.7387 | -0.0296 | [-0.0568, -0.0036] | 119 | 0.0375 | 40.7 | 44.0 | hold |
  | hybrid | confirm | 0.7643 | 0.7410 | -0.0232 | [-0.0580, +0.0097] | 97 | 0.0484 | 42.0 | 43.9 | hold |
  | graph | tune | 0.5349 | 0.5191 | -0.0158 | [-0.0387, +0.0067] | 116 | 0.0325 | 470.4 | 448.8 | hold |
  | graph | confirm | 0.5291 | 0.5210 | -0.0081 | [-0.0376, +0.0208] | 96 | 0.0418 | 468.0 | 464.2 | hold |
  | graph-hybrid | tune | 0.6305 | 0.6109 | -0.0196 | [-0.0444, +0.0041] | 116 | 0.0352 | 470.6 | 445.5 | hold |
  | graph-hybrid | confirm | 0.6408 | 0.6212 | -0.0197 | [-0.0514, +0.0122] | 96 | 0.0449 | 476.3 | 458.4 | hold |
  | graph-hybrid (no fact) | tune / confirm | 0.6305 / 0.6408 | 0.6109 / 0.6212 | -0.0196 / -0.0197 | [-0.0444, +0.0041] / [-0.0514, +0.0122] | 116 / 96 | 0.0352 / 0.0449 | 535.6 / 549.5 | 513.2 / 536.3 | hold |
  | bm25 | tune / confirm | 0.9111 / 0.8903 | same | +0.0000 | [0, 0] | 47 / 45 | insufficient | 11.9 / 11.5 | 11.4 / 11.2 | insufficient |

  - Verdict on the decision rule: `hold` for every vector-reading target. No confirm CI lower bound is above 0; every confirm delta is negative, and the tune split is significantly negative (CI upper bound < 0) for vector, configured and hybrid. bm25 does not read vectors, so its arms are identical.
  - Kept for Phase 5: arm B `$P4/armB` (recipe-2 chunks, no prefix), arm A `$P4/armA` with the grown judgment cache, the snapshot `$P4/snapshot.db`, the report `$P4/eval-report.txt`, logs `$P4/armB-embed.log` and `$P4/eval.log`. `$P4` is `/media/saidler/4tb/claude-tmp/claude-1000/-home-saidler-repos-scottidler-second-brain/821cbe7f-1f30-43e6-8fa8-6acb8d69303f/scratchpad/p4`.
- No deploy, no live re-embed: Phase 5 ships 4+5 as one drain.
