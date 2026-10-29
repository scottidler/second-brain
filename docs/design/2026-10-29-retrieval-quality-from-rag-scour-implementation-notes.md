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
