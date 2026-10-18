# Implementation Notes: Claim Judging Over the Full Source

Design doc: `docs/design/2026-10-17-claim-judge-full-source.md`

## Phase 1: Full-source claims judging + not judgeable

### Design decisions
- One gate, `judge_claims_gated` (`borg/src/eval.rs`), called from `evaluate`. Every caller (`--staged`, `--unscored`, `--inject`, candidate scoring, the regenerate grader) reaches `judge-claims` through `evaluate`, so no path keeps the 24k trim. It compares `fx.source.chars().count()` to `EvalOpts.claims_source_max_chars`. Over the cap: no cache read, no judge call, `ClaimsOutcome::NotJudgeable { source_chars, cap }`. Under it: the judge gets `fx.source` (the whole staged source), never `JudgedInput.source` (the distillation judge's budgeted one).
- Claims cache key: `stable_hash(kind \n full source \n claims)` (`judge_claims_gated`). A v2 entry keyed on the budgeted source can never answer a v3 call.
- `ClaimsOutcome` (`borg/src/eval/report.rs`) serializes through a private untagged `ClaimsWire` with `serde(try_from, into)`: `Judged` -> the verdict array, `NotJudgeable` -> `{"applicable":false,"reason":"source-over-cap","source_chars":N,"cap":C}`, `NotRun` -> `null`. Deserialize refuses an object with `applicable: true` or any other `reason`, so a malformed row never reads as `NotRun`.
- `rows_from_record` (`borg/src/eval/scores.rs`) writes the not-judgeable row with `serde_json::to_value(&ClaimsOutcome)`: the `eval_scores` shape and the JSONL shape have one definition. The string lives once, `REASON_SOURCE_OVER_CAP`.
- `FabricClaimJudge` (`borg/src/eval/judge.rs`) fails closed: a source over its `source_max_chars` is an error before fabric runs. It always passes `max_chars = 0` to `vault::fabric::run_command`. Confirmed in `vault/src/fabric.rs::truncate_input`: `max_chars == 0` is no limit.
- `claims_input(kind, claims, source)` is a pub fn now; its format is byte-identical to the old inline one.
- `inject::not_judgeable(check, metrics)` plus `ClassTally::exclude()` and a `not_judgeable` counter. `tally` checks exclusion before `flags`, so `ClassTally::add(original, mutant)` keeps its signature for `trust/tests.rs`. `flags` still returns `None` for a not-judgeable side: a caller that skips the exclusion counts an error, never a pass.
- Weekly `Outcome::NotJudgeable` (`borg/src/eval/weekly/remediated.rs::scored`): no required row fails AND at least one required row carries `reason: source-over-cap` -> `not-judgeable`, text `judge-claims (<model>) not judgeable: source N chars > cap C`. Never `passed`. A failing row still wins (`still-failing`).
- Candidate filter at the top of `candidate::score`: fixtures over the cap are partitioned out before any distill-cache read or distill call, and `req` is rebound to the filtered slice. Both arms share a source, so both drop together; `complete` covers the scored traces only.
- `EvalConfig::validate` refuses `claims-source-max-chars: 0` (every source would be not judgeable) and `claims-timeout-secs: 0`.
- `EvalOpts::default()` and `From<&EvalArgs>` take `borg::config::DEFAULT_CLAIMS_*`, so there is one default; `eval_opts` (`sb/src/cli/borg.rs`) sets both from `config.eval`.
- `claims-timeout-secs` default 90: max(90, 2 x p99) = max(90, 83.8) = 90 on gemini's 41.9s p99, already a multiple of 30 and above Phase 0's 55.3s max.

### Deviations
- **`is_covered` predicate.** The doc's `AND NOT (json_extract(metrics_json,'$.reason') = 'source-over-cap' AND json_extract(metrics_json,'$.source_chars') <= ?4)` drops every ordinary row: no `reason` -> `=` is NULL -> `NULL AND NULL` is NULL -> `NOT NULL` is NULL -> row filtered. No trace would ever be covered. Shipped: `AND NOT (json_extract(metrics_json,'$.reason') IS ?5 AND COALESCE(json_extract(metrics_json,'$.source_chars') <= ?4, 1))`. Same intent, two-valued. An over-cap row with no `source_chars` never covers (fail closed). Break #5 below shows the `=` form failing an existing coverage test.
- **`weekly::remediated::collect` takes its cap from `inputs.eval.claims_source_max_chars`**, the `EvalConfig` that `weekly::Inputs` already carries, not `EvalOpts`. The weekly path builds no `EvalOpts`, and `EvalOpts.claims_source_max_chars` is filled from that same field. Same effect, correct seam. `backlog::find_backlog` takes it from `EvalOpts` as written (`run_unscored`, and `run_eval_unscored --count` in sb).
- **The `ClassTally` not-judgeable path is pulled forward from Phase 2.** Once `TraceMetrics.judge_claims` has three states, `inject::tally` must do something with `NotJudgeable` in Phase 1, and API Design says excluded; the parent ordered it for this phase. Phase 1 adds the counter, `exclude()` and a test (`a_not_judgeable_judge_claims_pair_is_in_neither_n_nor_errors`). The inject report does not print the count yet: the bucketed `fits | over-window | over-cap` report is Phase 2.
- **`FabricClaimJudge.max_chars` -> `source_max_chars`.** The old field was fabric's truncation budget, which is now always 0; keeping the name would describe a knob that no longer exists. The new field is the cap the judge enforces.
- **`SampleScore.dropped_over_cap` is `Vec<String>` (trace ids), not a bare count.** The render prints count and ids, so a frozen sample says which traces it lost.
- **Added, not in the doc:** `EvalReport.not_judgeable_claims` and one render line (`claims not judgeable: N traces have a source over the claims cap ...`), printed only when N > 0, so the skip is visible on `--staged` and `--unscored` runs.
- **`ClaimVerdict` gained `Deserialize`**, which the JSONL round-trip criterion needs.

### Tradeoffs
- Untagged `ClaimsWire` + `try_from`/`into` vs hand-written `Serialize`/`Deserialize`: the derive keeps the wire shape declarative and the `TryFrom` holds the one validation rule.
- A `not_judgeable()` predicate checked before `flags()` vs a three-state return from `flags`: leaves `flags`' signature and its tests untouched, and the `None` fallback keeps the failure mode closed.
- Partition inside `candidate::score` vs in `load_traces` or the sb call site: `score` is the one seam both `--candidate-pattern` and `--apply-candidate` cross, which is where the doc puts it.
- The weekly rule reads the stored row's `reason` and does not re-measure the source: `is_covered` has already rejected stale over-cap rows (source now under the cap) before `scored` reads them, so any `source-over-cap` row reaching the rule is current.

### Break-and-restore evidence
Each guard removed, its test run (`cargo test -p borg -p sb <name>`), file restored byte-for-byte. Observed:
1. Gate removed (`if false && source_chars > cap` in `judge_claims_gated`) -> `eval::tests::judge_claims_over_cap_is_not_judgeable`: `assertion left == right failed: an over-cap source never reaches the claims judge  left: 1  right: 0`.
2. Trim re-introduced (claims judge handed `judged_input(fx).source`) -> `eval::tests::judge_claims_reads_the_whole_source_under_the_cap`: `left: [23930]  right: [300000]`.
3. Fabric trim re-introduced (`run_command(.., JUDGE_INPUT_MAX_CHARS, ..)`) -> `eval::judge::tests::fabric_claim_judge_sends_the_whole_source_to_fabric`: `left: 24000  right: 300047`.
4. Cap predicate removed from `is_covered` -> `judge_claims_over_cap_is_not_judgeable` panicked at `borg/src/eval/tests.rs:870` (the "raising the cap past the source re-opens the trace" assert).
5. `IS` swapped back to the doc's `=` -> `eval::scores::tests::a_trace_with_every_required_check_current_and_clean_is_covered`: `assertion failed: store.is_covered("tr-1", "h1", &required, ...)`.
6. `claim_judge.source_max_chars = opts.claims_source_max_chars` dropped from `fabric_judges` -> `cli::borg::tests::eval_claims_cap_and_timeout_flow_from_borg_yml_to_the_claims_judge`: `left: (400000, 45)  right: (1000, 45)`.
7. `claims_timeout_secs: config.eval.claims_timeout_secs` dropped from `eval_opts` -> same test: `left: (1000, 90)  right: (1000, 45)`.
8. Weekly NotJudgeable branch disabled -> `eval::weekly::tests::outcome_not_judgeable_when_judge_claims_could_not_read_the_source`: `left: Passed  right: NotJudgeable`.
9. Candidate filter disabled (`partition(|_| true)`) -> `eval::candidate::tests::an_over_cap_trace_is_dropped_before_distill_and_the_sample_stays_complete`: `assertion left == right failed: 2 judgeable traces x 2 arms  left: 6  right: 4`.
10. JSONL shape broken (`applicable: true`) -> `eval::report::tests::claims_outcome_round_trips_all_three_jsonl_shapes`: `left: Object {"applicable": Bool(true), ...}  right: Object {"applicable": Bool(false), ...}`.
11. Inject exclusion disabled -> `eval::inject::tests::a_not_judgeable_judge_claims_pair_is_in_neither_n_nor_errors`: `assertion left == right failed: only t2 is over the cap  left: 0  right: 1`.
12. `FabricClaimJudge` refusal disabled -> `eval::judge::tests::fabric_claim_judge_refuses_a_source_over_its_cap` panicked at `borg/src/eval/judge/tests.rs:250` (the `unwrap_err` on an over-cap source).

### Open questions
- The deployed `~/.config/sb/patterns/judge-claims.md` still carries the cut-off rule until `otto deploy` syncs patterns (Phase 3). `otto deploy` installs the binary and syncs patterns together, so this bites only if the new `sb` is installed without a deploy: its rows would be stamped `v3` but judged by the old text. Confirm Phase 3 ships through `otto deploy`, not a bare `cargo install`.

## Phase 2: Per-class sampling + bucketed inject report

### Design decisions
- `sample_fixtures(staged_root, sample, claims_cap) -> InjectSample { fixtures, short }` (`borg/src/eval/inject.rs`). It takes the first `sample` of the kind-stratified order (so the result is always a superset of the old sample and its judgment cache still hits), then keeps adding the next trace that helps at least one short (check, class) pair, and stops when every pair holds `sample` or the pool ends. A trace that helps no short pair is skipped, so the sample does not bloat with traces the shortfall does not need.
- The pairs are `INJECT_CHECKS` x `applicable_classes`, so `judge-distillation` (all four classes), `judge-claims` number-swap and `quote-grounding` fabricated-quote each get a floor. "Judgeable" is per check: only `judge-claims` has a cap (`judgeable(check, chars, cap)`); the others read over-cap traces.
- `fixtures::stratified_order` is the round-robin order extracted from `stratified_sample`, which is now its prefix plus the id sort. One definition of the order, no behavior change for `--staged`.
- `ShortPair { check, class, have, want }` implements `Display` (`judge-claims number-swap has 2 of 5`); `sb` prints one `warning:` line per short pair to stderr. A short pool is a warning, not an error: trust already fails closed on `sample_size < min-samples`.
- `SourceBuckets { fits, over_window, over_cap }` of `ClassTally`, on `InjectCell.buckets` (`Some` exactly when `tally` is). The bucket is chosen by the original's source chars: `fits` is `<= JUDGE_INPUT_MAX_CHARS` (the constant `FabricJudge` uses, no new literal), `over_window` is over that and `<= EvalOpts.claims_source_max_chars`, `over_cap` is over the cap. `InjectReport.claims_source_max_chars` carries the cap for the header. `judge_trust` rows still read the whole-sample `tally`; the buckets are report lines only.
- The printed pair count is `ClassTally::pairs()` = `n + errors + not_judgeable`, so the over-cap count for `judge-claims` is exactly the Phase 1 `not_judgeable` count; paired catch prints `n/a` for an empty bucket and `0.83 (n=6)` otherwise.
- Help text: `--sample` now describes the per-class floor (`sb/src/cli/borg/args.rs`). `borg/AGENTS.md` updated for both.

### Deviations
- **The bucket line is printed for every cell with a tally**, not only `judge-claims`. The doc says "per (model, check, class)"; for `judge-distillation` the over-cap bucket carries ordinary scored pairs (it has no cap), which the report shows honestly.
- **`sample_fixtures` returns a struct, not `Vec<Fixture>`**, so the caller can name the short class (the doc requires "says which class fell short"). Same effect, correct seam.
- **Which traces count toward the floor also depends on the check** (see above), where the doc says "plantable, judgeable traces" without saying judgeable is only a `judge-claims` notion.

### Tradeoffs
- Skip traces that help no short pair vs append the next N in order: keeps `--sample 30` close to 30 plus only what the sparse class needs, at the cost of the sample no longer being a pure prefix of the stratified order.
- Short pool as a stderr warning vs a hard error: the run is still useful for the classes that filled, and trust gating already refuses an undersized row.

### Break-and-restore evidence
Each guard removed in `borg/src/eval/inject.rs`, run `cargo test --workspace inject::tests`, file restored from a copy and `touch`ed (cargo's mtime trap). Observed:
1. Growth removed (`helps_short_pair` forced false) -> `inject::tests::the_sample_grows_until_a_sparse_class_reaches_the_floor`: panicked at `borg/src/eval/inject/tests.rs:773` (the `number-swap pairs >= 30` assert).
2. Short report removed (`.filter(|_| false)` on `short`) -> `a_pool_too_small_reports_the_short_class_by_name` panicked at `tests.rs:809` (`expect("judge-claims number-swap fell short")`) and `an_over_cap_trace_does_not_count_toward_the_claims_floor`: `left: None  right: Some(1)`.
3. Cap ignored in the floor (`(judgeable(..) || true)`) -> `an_over_cap_trace_does_not_count_toward_the_claims_floor`: `left: None  right: Some(1)`.
4. Bucketing collapsed (`if true { &mut self.fits`) -> `the_report_buckets_pairs_by_source_length`: `left: (4, 0, 0)  right: (2, 1, 1)`.
5. Not-judgeable exclusion disabled (`if false && ...` in `tally`) -> `a_not_judgeable_judge_claims_pair_is_in_neither_n_nor_errors`: `assertion left == right failed: only t2 is over the cap  left: 0  right: 1`, and `the_report_buckets_pairs_by_source_length` fails on `judge-claims makes no call over the cap`.
The rendered-line assertion (`pairs fits | over-window | over-cap: 2 | 1 | 1` for judge-claims number-swap) lives in test 4's body after the bucket asserts, so it runs only when bucketing is right; it was not broken separately.

### Open questions
- None.

## Implementation audit, round 1 (2026-10-18)

Synthesis: `~/.cache/review-panel/runs/fq6ZL3LN/synthesis.md`. Architect seat: compliant, but 5 of its reads were denied by gemini's policy guard and it never opened `eval/propose/`. Staff seat: not signed off, two verified findings. Both folded here.

### Must-fix, folded
- `candidate::score` dropped over-cap traces (Phase 1) but `candidate::split` did not apply the cap, so the GEPA adapter (`eval/propose/adapter.py:51-54`) could be handed a trace that came back with no score row and raise `sb returned no score`. The staff seat reproduced it in-process. Fix: `split` and `load_split` take `claims_cap` and filter on it; `run_split` threads `opts.claims_source_max_chars`. Test `the_split_skips_traces_over_the_claims_cap`. Same commit: when every trace is dropped, the CLI said "every trace errored on an arm"; it now says how many were over the cap and names them.

### Cheap wins, folded
- `SourceBuckets::of_mut` checked the 24k budget before the cap, so a cap below 24k put over-cap pairs in `fits`. The cap is checked first now. Test `a_cap_below_the_judge_budget_still_buckets_over_cap_pairs_as_over_cap`.
- Doc Overview named a "`--remediate` before-score" call that does not exist. Corrected in the doc; nothing bypassed the gate.

### Deferred, with reason
- A cell whose every pair is not judgeable writes a `judge_trust` row with `sample_size` 0. It fails closed (`n 0 < min-samples 30`), so it is correct as a gate; a clearer reason string is not worth a change the data has not asked for.

### Open questions
- How many staged repo traces exceed the 400k cap is unmeasured; the `--inject` run in Phase 3 prints the over-cap bucket and answers it.
