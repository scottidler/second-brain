# Implementation notes: Automated Note Quality Loop

Append-only, one section per phase. Design doc: `docs/design/2026-10-14-automated-note-quality-loop.md`.

## Phase 0: Spike the unproven pieces

Zero repo code. Everything ran on ripr (2026-10-17, repo at 39ead43) under the session scratchpad `S=/tmp/claude-1000/claude-1000/-home-saidler-repos-scottidler-second-brain/3d292b36-e1f1-49cd-9759-13240970fffb/scratchpad/phase0` (written `$S` below). No non-markdown file under `docs/`, no commit, no `otto ci`.

### Spike results

**The one trace, used by all three judges:** `cl-c89ab882` (kind `youtube-url`, 22,784-char source from the inline `transcript:` field of `~/.local/share/sb/borg/stages/cl-c89ab882/distilled.yml`, 10 claims). Exported by `$S/dump_trace.py` to `$S/trace.json`; `$S/claims-input.txt` is the `CLAIMS:`/`SOURCE:` input; `$S/note.txt` is the note text.

**1. Gemini through fabric, key on the child env only (PASS)**
- Scratch `HOME=$S/home` holds a copy of `~/.config/fabric/` (contexts, extensions, locales, patterns, sessions, unique_patterns.txt) with a hand-written keyless `.env` (`DEFAULT_VENDOR=Gemini`, `DEFAULT_MODEL=gemini-2.5-flash`). See Deviations.
- Pattern passed as a file path, as `vault::fabric::resolve_pattern` does: `PAT=~/.config/sb/patterns/judge-claims.md`.
- Fails without the key (same scratch HOME):
  - `env -u GEMINI_API_KEY HOME=$S/home timeout 90 fabric -V Gemini -m gemini-2.5-flash -p $PAT < $S/claims-input.txt > $S/gemini-neg.out 2> $S/gemini-neg.err`
  - exit code `1`; stderr `GEMINI_API_KEY=, is not valid`; stdout empty (0 bytes).
- Succeeds with the key only on the child:
  - `REAL_HOME=$HOME HOME=$S/home timeout 110 $S/with-secret.sh GEMINI_API_KEY gemini-api-key fabric -V Gemini -m gemini-2.5-flash -p $PAT < $S/claims-input.txt > $S/gemini-verdict.txt 2> $S/gemini-pos.err`
  - `with-secret.sh` runs `manifest age decrypt .../gemini-api-key.age`, extracts the value inside a `$(...)`, exports it, then `exec`s the command. The value never reached a terminal, a file or an argv.
  - exit code `0`; `gemini-pos.err` empty.
- Verdict file: `$S/gemini-verdict.txt`: claims 1-5 and 7-10 `supported`, claim 6 `unsupported`.
- A scan of the scratch outputs for key-shaped strings found none (one false hit inside a torch `RECORD` file).

**2. DeepEval `SummarizationMetric` on OpenAI (PASS)**
- `REAL_HOME=$HOME DEEPEVAL_TELEMETRY_OPT_OUT=YES $S/with-secret.sh OPENAI_API_KEY openai-api-key $S/proj/.venv/bin/python -I $S/deepeval_run.py $S/trace.json $S/deepeval-verdict.jsonl gpt-4o-mini`
- exit code `0`; `score 0.4 seconds 15.4`.
- Verdict file: `$S/deepeval-verdict.jsonl` (one record in the Phase 4 verdict shape: `tool: deepeval`, `model: gpt-4o-mini`, `metric: summarization`, `value: 0.4`, `detail` = the reason text).
- Version resolved by `uv add deepeval`: `4.3.1` (the doc cites 4.2.9).
- The reason text flags a contradiction about "release of function hooks" plus extra information not in the source. It is a note-level score with no per-claim breakdown.

**3. MiniCheck CPU (PASS)**
- Repo: `git clone git@github.com:Liyan06/MiniCheck.git` into `$S/mc/repo` at `b58b9fa69acbd1015ec970fa65dd752413a053d2`. Own venv `$S/mc/venv` (uv, Python 3.11). Model `lytang/MiniCheck-RoBERTa-Large` (the repo's `roberta-large`).
- `NLTK_DATA=$S/mc/nltk HF_HOME=$S/mc/hf timeout 550 $S/mc/venv/bin/python -I $S/minicheck_run.py $S/trace.json $S/minicheck-verdict.jsonl $S/mc/hf roberta-large`
- exit code `0`.
- Verdict file: `$S/minicheck-verdict.jsonl` (10 records, one per claim: `value` = support probability, `detail.label`, `detail.seconds`).
- **Per-claim CPU latency, measured over 10 claims (one claim per call, one 22,784-char source, warm model, a one-claim warm-up excluded):**
  - per claim, seconds: 2.37, 2.42, 2.68, 2.60, 2.58, 2.62, 2.65, 2.70, 2.56, 2.53
  - mean 2.57 s per claim
  - one batched call over all 10 claims: 25.8 s total (batching does not help on CPU)
  - model load: 0.9 s (weights already cached by an earlier failed run)
  - torch default threads 32 on a 64-thread CPU, `cuda.is_available() == False`.
- Labels (1 = supported): `[0, 1, 0, 1, 0, 0, 0, 1, 0, 0]`. MiniCheck-RoBERTa marks 7 of 10 claims unsupported where Gemini marked 1 of 10. Only claim 6 is unsupported in both. One trace is no verdict, but it is a large disagreement for Phase 5 to measure. `Inferencer` chunks the source at 400 tokens, so a long source is a plausible cause.
- Two undeclared prerequisites tripped the first runs:
  - `accelerate` is not in MiniCheck's `pyproject.toml` dependencies, and `from_pretrained(device_map="auto")` raises `ValueError ... requires accelerate` without it. Fixed with `uv pip install accelerate` (1.15.0).
  - NLTK `punkt_tab` is needed at runtime and is not shipped. `nltk.download` refused twice: the proxied-fetch SSRF guard, and the world-writable `/tmp` ancestor. Fixed by fetching `punkt_tab.zip` from `raw.githubusercontent.com/nltk/nltk_data` into `$S/mc/nltk/tokenizers/` and pointing `NLTK_DATA` at it.
- `uv pip install torch --index-url https://download.pytorch.org/whl/cpu` was followed by the repo install, which left `torch 2.14.1+cu130` in the venv. It still runs CPU-only here (no GPU).

**4. clyde export `scope` (PASS)**
- Commands: `clyde session export --since 14d --limit 200 --no-reindex > $S/clyde-export.json` (bulk), then `clyde session export --id <id> --no-reindex > $S/one-<id>.json` for 10 sessions (5 `personal`, 5 `work`, ids in `$S/ids.txt`). All exit `0`. The export JSON is `{schema-version: 2, host, cursor, scope-version: 5, sessions: [...]}`, so `scope` is a field of each element of `sessions`.
- Scope values on the 10 sessions: `personal` x5, `work` x5, nothing else.
- Same field across the full catalog (`clyde session export --no-reindex`, 3,188 sessions, `$S/clyde-all.json`): `personal` 994, `work` 2,194. The 200-row page: `personal` 148, `work` 52. No missing, null or unknown value anywhere. The expected set `{work, personal}` is confirmed.
- Scope is not derived from the repo path. All 5 `work` sessions have a `repo` under `scottidler/` (`claude` x3, `keep`, `dotfiles`), and 3 of the 5 `personal` sessions have `repo` `None`.

**Background processes:** none left running (`ps` for fabric, minicheck, deepeval after the last run). One early foreground call (`fabric -V Gemini -L`, which blocked on the then-missing `.env`) was backgrounded by the harness and killed by pattern; it held no key.

### Design decisions
- Two venvs: `uv` project `$S/proj` for DeepEval (`uv init --bare` + `uv add pyyaml deepeval`), and `$S/mc/venv` for MiniCheck, so the untrusted repo and its torch stack never share an environment with the DeepEval project. Phase 4's `eval/` has to decide whether both live in one lock file (torch is heavy).
- `with-secret.sh` (decrypt one keep secret, `exec` a command with it exported) is the spike's only key path. It is the shape `vault::fabric` already uses: key resolved in the parent, set on the child only.
- Gemini judge input is the existing `judge-claims` contract (`CLAIMS:` then `SOURCE:`), so Phase 1 sends something comparable.
- The pattern goes in as a file path (`-p ~/.config/sb/patterns/judge-claims.md`), matching `resolve_pattern`. The scratch fabric HOME needs no sb patterns.

### Deviations
- The scratch fabric `.env` is hand-written (keyless, two non-secret defaults) instead of copied minus key lines. The environment's secret-file guard blocks any command that names `~/.config/fabric/.env`, including `grep -v` and `rsync --exclude`. Same effect: the child holds no key from a file, and the `env -u GEMINI_API_KEY` run above shows it.
- fabric needs a `.env` to exist at all: with none under the scratch HOME it exits 1 with `error loading .env file ... no such file or directory`. A keyless stub satisfies it. Any test or CI scratch HOME for fabric needs the stub.
- The doc cites DeepEval 4.2.9; `uv add` resolved 4.3.1. Not pinned back. Phase 4's `uv.lock` pins whatever it resolves then.
- The trace is `cl-c89ab882` (22.8k-char transcript, 10 claims), not the newest staged trace, because the newest (`ht-a2a14748`) has a 1.5k-char source, too small for a latency measurement.
- The MiniCheck clone first landed in the repo working tree (the harness resets cwd between calls). I moved it, not deleted it, to `$S/mc/repo` before anything else ran. `git status` shows only the design doc untracked.

### Tradeoffs
- `gpt-4o-mini` for DeepEval vs a larger OpenAI model: cheaper, and enough to prove the path. Whether DeepEval's catch rate is useful is a Phase 5 question.
- `gemini-2.5-flash` for the Gemini spike vs `gemini-2.5-pro` (the model in the doc's observed-on-main line): flash proves the key path at lower cost. Phase 5 picks the default judge on measured numbers.
- MiniCheck-RoBERTa-Large vs DeBERTa-v3-Large (2048-token window against RoBERTa's 512): RoBERTa because the doc names it first and it is the cheaper CPU figure. DeBERTa's longer window may fit a 22k-char source better. Not measured.

### Open questions
- Phase 4/5 input, not a blocker: MiniCheck-RoBERTa marked 7 of 10 claims unsupported where Gemini marked 1 of 10. Should Phase 4 also spike `deberta-v3-large` before choosing which MiniCheck model enters `eval/`?
- Phase 6 input: clyde `scope` is a content classification, not a repo-path rule (a `scottidler/claude` session can be `work`). The doc's `scope: personal` filter is right as written, but the count of mineable queries depends on the classifier, so check the >= 50 criterion against `scope`, not cwd.

## Phase 1: Keys, judge input, JSON, error capture

Ran on ripr 2026-10-17, on top of 39ead43. Live checks used `./target/debug/sb` from `cargo build -p sb` (no `otto install`, no `otto deploy`).

### Observed criterion output
- **Key map unit test (PASS):** `vault::fabric::tests::command_exports_every_mapped_vendor_key_under_fabrics_name` parses `anthropic/gemini/openai` api-keys naming three env vars and asserts the child `Command` carries `ANTHROPIC_API_KEY`, `GEMINI_API_KEY` and `OPENAI_API_KEY` set from them. Bites: limiting `command` to the Anthropic entry fails it.
- **Construction tests (PASS):**
  - borg: `eval::judge::tests::both_judges_export_the_configured_key_map_on_their_fabric_child` parses a `borg.yml` with `llm.api-keys.{gemini,openai}`, builds both judges through `fabric_judges`, and reads `GEMINI_API_KEY`/`OPENAI_API_KEY` off each judge's own `command()`. Bites: building `FabricClaimJudge` with an empty map fails it.
  - oracle: `eval::tests::fabric_judge_exports_the_configured_key_map_on_its_fabric_child` does the same through `oracle::eval::fabric_judge(config, opts)`, the constructor `run` uses. Bites: an empty map fails it.
  - sb: `cli::borg::tests::eval_opts_carry_borg_yml_llm_api_keys` proves `sb borg eval` hands `config.llm.api_keys` to `EvalOpts`.
- **Oracle eval (PASS, with a scratch config, see Deviations):**
  - Baseline, deployed `oracle.yml` (no `llm` block): `env -u ANTHROPIC_API_KEY ./target/debug/sb oracle eval --k 1 --rebuild-cache` -> exit 0, `judgments: 0 total, 0 from truncated bodies (low-confidence), 27 judge errors (pairs skipped)`; each warn reads `could not find vendor` (fabric's `.env` holds no Anthropic key).
  - With the map: `env -u ANTHROPIC_API_KEY ./target/debug/sb oracle --config $S/oracle-keys.yml eval --k 1 --rebuild-cache` (the deployed file plus `llm: {api-keys: {anthropic: ESCOTE_ANTHROPIC_API_KEY}}`) -> exit 0, `judgments: 27 total, 0 from truncated bodies (low-confidence), 0 judge errors (pairs skipped)`, calibration `6 hand-labeled pairs ... -> TRUSTWORTHY`.
- **30,000-char source (PASS):** `eval::tests::a_30k_source_leaves_the_full_note_in_the_judge_input` runs a 30,000-char source through `evaluate`, rebuilds the input the judge sends, applies fabric's head truncation at 24,000 chars, and finds the full note (with its `quote:` line) inside. Bites: dropping the source budget (truncating the source to 24,000 as before) fails it. Note order is pinned separately by `distillation_input_puts_the_note_before_the_source`.
- **JSON count (PASS):** `./target/debug/sb borg eval --staged --no-judge` -> `distillation eval  (4290 fixtures, judge-model=<fabric default>)`; `./target/debug/sb borg eval --staged --no-judge --format json | jq -s length` -> `4290`.
- **Error capture, live:** `./target/debug/sb borg eval --staged --sample 1 --judge-model no-such-model-xyz --format json` -> exit 1, `Error: every judged trace errored (1 of 1); see the per-trace errors`, and the record for `cl-df686457` carries two errors (`judge-distillation`, `judge-claims`, both `404 Not Found`). A judged run, `./target/debug/sb borg eval --staged --sample 2 --rebuild-cache` -> exit 0, `judge errors: 0 of 2 traces`, 18 claims 100% supported. Unit tests: `a_judge_error_is_recorded_on_its_trace_and_the_run_continues`, `every_judged_trace_erroring_is_the_one_failing_run`, `a_short_claim_verdict_list_is_an_error_row_not_an_abort` (the first two bite when the error arm returns `Err`).

### Design decisions
- Vendor map type (`vault/src/fabric/keys.rs:ApiKeys`, `Vendor`): a closed enum (`anthropic`, `gemini`, `openai`) so an unknown vendor in yml is a load error; values are var names or files, resolved by `resolve_secret` only when the child is built.
- One child builder (`vault::fabric::command` (pub; was the private `build_fabric_command`) plus `vault::fabric::run_command`): each judge builds its command through its own `command()` method and runs exactly that, so the construction tests read the env of the command production spawns, not a parallel helper.
- `api-key` is the Anthropic entry (`vault::config::LlmConfig` (`TryFrom<RawLlmConfig>`)): after load `api_keys` always carries `anthropic -> api_key`; `api-keys.anthropic` alone sets `api_key`; naming two different vars is a load error rather than a silent pick.
- Source budget (`borg::eval::judge::source_budget` / `distillation_input`): `24,000 - chars(framing + full note)`; the eval truncates the source to it before hashing, so the distill hash covers exactly what the judge receives and fabric's head truncation never fires on a judged input.
- Per-trace records (`borg::eval::report::TraceRecord`): every fixture yields one record (also under `--no-judge`); judge failures land on `errors` as `{check, error}` and are never cached, so the next run retries them.
- Exit rule (`EvalReport::all_judged_errored`, checked in `sb/src/cli/borg.rs` after printing): non-zero only when judges ran and every trace carries an error; a trace counts as errored when any judge check on it failed.
- oracle `llm` block (`oracle/src/config.rs:LlmConfig`): `api-keys` only, `deny_unknown_fields`, empty by default (fabric `.env` fallback, the prior behavior).
- Rubric bump (`borg/src/eval/cache.rs`): `RUBRIC_VERSION` and `CLAIMS_RUBRIC_VERSION` to `v2`, with a one-line note of what changed.

### Deviations
- The source budget subtracts the input framing and the note, not "the pattern". `vault::fabric::truncate_input` truncates only the stdin input; the pattern goes to fabric as a `-p` file and never counts against the 24,000 chars. Same effect (the note always survives), correct seam.
- borg's own duplicate `LlmConfig` (`borg/src/config.rs`) is removed and `borg::config::LlmConfig` re-exports `vault::config::LlmConfig`, which was dead code. The key reconciliation lives in one place. Cortex keeps its own `LlmConfig`; it has no judge site.
- `vault::fabric::run_pattern(.., api_key_env: &str, ..)` keeps its signature as an Anthropic-only wrapper (`ApiKeys::anthropic`), so distillers, cortex and oracle's query transform are unchanged. Only the three judge sites (plus `--synth-intents`, which runs under `--judge-model`) carry the map.
- The JSONL record carries three fields beyond the doc's contract: `fixture` (`<kind>/<trace>`, the cache id), `judge_model`, and `errors`. Keys are snake_case as the contract writes them. `rubric_version` is the distillation rubric's (`content_hash` is the distill hash).
- `--format` takes `text | json` with `text` the default, not the house TTY-detected yaml/json: the text report is the human view and `--format json` is the JSONL contract. Case-insensitive; conflicts with `--synth-intents`, `--regenerate`, `--emit-calibration`.
- `sb oracle eval`'s report now prints `N judge errors (pairs skipped)` on the judgments line. Without it the "zero judge errors" criterion is not observable from the report.
- The oracle criterion ran against `./target/debug/sb` with `--config` pointing at a scratch copy of the deployed `oracle.yml` plus the `llm` block, not the doc's bare `~/.cargo/bin/sb oracle eval`. The deployed `~/.config/sb/oracle.yml` has no `llm` block, and editing it is a deploy-side change (see Open questions).
- The claims judge gets the same note-budgeted source as the distillation judge (shorter than its own claims-first input strictly needs), so both judges grade the same text.

### Tradeoffs
- Load error on a conflicting `api-key`/`api-keys.anthropic` vs letting one win: a silent pick reads one config line and ignores the other; the error names both vars.
- Error rows uncached vs cached: an error is a transient vendor or parse failure, so caching it would pin the failure; the cost is one retry per run per errored trace.
- `TraceRecord` kept in memory for the text report too (about 4,290 records with sources up to 24,000 chars) vs building records only under `--format json`: one code path, and Phase 2's `eval_scores` writes need the records either way.

### Open questions
- The doc's exact oracle command (`env -u ANTHROPIC_API_KEY ~/.cargo/bin/sb oracle eval --k 1 --rebuild-cache`) passes only once the deployed `~/.config/sb/oracle.yml` carries `llm: {api-keys: {anthropic: ESCOTE_ANTHROPIC_API_KEY}}`. `sb bootstrap` never rewrites an existing `oracle.yml`. Who adds it, and when (before Phase 2's timer)?
- The deployed `~/.config/sb/borg.yml` has no `llm.api-keys` yet. Gemini judging works today only through fabric's own `.env`. Phase 2's `sb-eval.service` env file needs `GEMINI_API_KEY`/`OPENAI_API_KEY` and borg.yml needs the matching entries.
- oracle's query-transform fabric call (`oracle/src/transform.rs`) still passes `""`. It is not a judge site, so it is out of scope here; flag if it should take the map too.

## Phase 2: Score table, `--unscored`, timer

Ran on ripr 2026-10-17 on top of 0ea0147. Live checks used `./target/debug/sb` with `--config <scratch borg.yml>` (the deployed `~/.config/sb/borg.yml` plus an `eval:` block, in the session scratchpad). No `otto deploy`, `otto install`, `sb bootstrap` or `systemctl` was run. The real `~/.local/share/sb/borg/eval-cache.db` was written (additive: the `eval_scores` table and 21 rows).

### Observed criterion output
- **Baseline row count after a fire: PASS against the debug binary, POST-DEPLOY for the real timer.**
  - Equivalent of the service's ExecStart: `./target/debug/sb borg --config $S/p2/borg.yml eval --unscored` (config `eval.limit: 3`, `judge-models: [gemini-2.5-pro]`) -> exit 0, `unscored: 3 traces selected, 21 score rows written, 1148 succeeded receipts with no staged source skipped`, 3m04s.
  - `sqlite3 ~/.local/share/sb/borg/eval-cache.db "select count(*) from eval_scores where run_kind='baseline' and julianday(scored_at) > julianday('now','-1 day')"` -> `21` (before: `.tables` -> `claim_judgments distill_judgments`). Per trace: 5 deterministic rows (`judge_model ''`, rubric `v1`) + `judge-distillation` and `judge-claims` for `gemini-2.5-pro` (rubric `v2`), `error` null on all 21. `julianday()` accepts the `...Z` RFC 3339 stamps.
  - Backlog before/after: `eval --unscored --count` -> `4284`, then `4281`. stderr: `1453 succeeded receipts have no staged source and are not counted` (1148 on the run path, since the run stops at `--limit`).
  - Post-deploy, after the timer has fired once: the same sqlite count query, expected > 0.
- **`systemctl --user is-enabled sb-eval.timer` -> `enabled`: POST-DEPLOY.** Needs `otto deploy` (runs `sb bootstrap --force`, which writes the units, then `systemctl --user daemon-reload` and `enable --now sb-eval.timer`). Post-deploy commands: `systemctl --user is-enabled sb-eval.timer`, `systemctl --user list-timers sb-eval.timer`, `systemctl --user start sb-eval.service` (a manual fire), `journalctl --user -u sb-eval.service -n 50`. Rendered unit text, from `eval::timer::render_units` over the scratch config with the real home and binary paths:
  ```
  [Unit]
  Description=sb borg eval --unscored - score unscored ingests into eval_scores (second-brain)
  After=default.target

  [Service]
  Type=oneshot
  ExecStartPre=/bin/sh -c '/home/saidler/.cargo/bin/manifest -C /home/saidler/repos/scottidler/keep/manifest.yml secrets env -f env > /run/user/1000/sb-eval.env'
  EnvironmentFile=-/run/user/1000/sb-eval.env
  Environment="PATH=/home/saidler/.local/share/mise/shims:...:/bin"
  ExecStart=/home/saidler/.cargo/bin/sb borg --config /home/saidler/.config/sb/borg.yml eval --unscored
  SuccessExitStatus=130
  WorkingDirectory=/home/saidler
  NoNewPrivileges=true
  PrivateTmp=true
  ```
  (comment lines omitted.) The timer is its own unit: `Description=Daily sb borg eval timer (second-brain)`, `OnCalendar=*-*-* 05:00:00`, `Persistent=true`, `WantedBy=timers.target`. The `enable --now` argv is pinned by `eval::timer::tests::bootstrap_enables_the_timer_now_after_a_daemon_reload`.
- **Unit test, baseline + candidate + two identical candidates persist: PASS.** `eval::scores::tests::baseline_and_candidate_rows_for_the_same_trace_and_judge_both_persist`, `two_candidates_with_identical_output_do_not_share_a_row` (two `run_id`s at one content hash, plus a baseline at that same hash: 3 rows). Bites: dropping `run_id` from the primary key fails the second one.
- **Unit test, `--unscored` lists error-only, other-judge-only and stale-hash traces: PASS.** `eval::backlog::tests::a_trace_whose_only_current_row_for_a_required_check_is_an_error_is_listed`, `a_trace_scored_by_a_judge_outside_the_required_set_is_listed`, `a_trace_with_an_older_content_hash_row_but_none_for_its_current_hash_is_listed` (plus `a_fully_covered_trace_is_not_in_the_backlog_and_re_distilling_it_puts_it_back` and the `scores::tests` twins). Each break below was made, the tests failed, then it was restored:
  - `is_covered` ignoring `error IS NULL`: 3 tests fail.
  - `is_covered` ignoring `content_hash`: 3 tests fail.
  - `is_covered` ignoring `judge_model`: 2 tests fail.
  - `put` without the error-never-replaces-success guard: 1 test fails.
  - `render_units` without the keys-need-bootstrap refusal: 1 test fails.
  - `find_backlog` counting unstaged receipts as owed: 1 test fails.
- `otto ci` -> exit 0, `[ci] All CI checks passed!` (the first run failed only `cargo fmt --check`; `cargo fmt --all` fixed it).

### Design decisions
- `eval_scores` lives in `eval-cache.db` beside the judgment cache, opened by its own connection (`borg/src/eval/scores.rs:ScoreStore`), not through `JudgmentCache`: the cache is keyed per judge call, this table per check, and only this one carries error rows and lineage. `the_score_table_shares_a_file_with_the_judgment_cache` pins the three tables in one file. `judge_trust` is Phase 3's and is not created here.
- Scores are written from the `TraceRecord`s `evaluate` already returns (`scores::rows_from_record`, called in `backlog::run_unscored`), so `evaluate`'s signature and its callers are untouched. Only `--unscored` writes `eval_scores`; `--staged` stays a read-only dev run.
- Every trace gets a row for each of the five deterministic checks (`quote-grounding`, `tools-grounding`, `placeholder-artifacts`, `listicle-survival`, `note-size`; rubric `v1`), `{"applicable":false}` when the note has nothing to measure, so a note with no quote can still be covered.
- Required set (`scores::required_checks`): the five deterministic checks plus `judge-distillation` and `judge-claims` for each model in the new `eval.judge-models` (`borg/src/config/eval.rs`). No default model: an empty list is an error in `run_unscored` and `--count`, because a silently chosen judge is a silently chosen definition of "covered". Each configured model scores every trace in one run (one `evaluate` per model).
- `put` is an upsert guarded so an error row never replaces a success (a transient failure on a `--rebuild-cache` re-run must not un-score a trace); a success replaces an error.
- The backlog is receipts-driven (`backlog::find_backlog`): `succeeded` receipts, oldest `COALESCE(terminal_at, received_at)` first, whose trace loads from staging and is not covered. `--limit` stops the walk early, so the rest carry over. `fixtures::load_staged_trace` (also Phase 6's reader) was split out of `load_staged` through a shared `load_staged_dir`; `eval::judged_input` was extracted from `evaluate` so the selector and the scorer compute the same distill hash from one function.
- Env bootstrap driven by the borg.yml map: `llm.api-keys` names the vars the judges need, and two checks derive from it. (1) `eval::timer::render_units` refuses to render when `llm.api-keys` is non-empty and `eval.env-bootstrap` is unset (the error names the vars). (2) `backlog::preflight_keys` runs before every `--unscored` judging run and fails naming each vendor and var that does not resolve in the service's environment (never a value), so a missing var is one loud failure, not 4,000 error rows. `eval.env-bootstrap` has the harvest shape (`command`, own `env-file`, `/run/user/1000/sb-eval.env` in the template).
- `sb bootstrap` (`register_systemd_units`) writes the eval units last, then runs `systemctl --user daemon-reload` and `enable --now sb-eval.timer` (`run_systemctl`, fails with systemctl's stderr).

### Deviations
- The service runs `sb borg eval --unscored` in the default text format, not the diagram's `--format json`: the JSONL carries every trace's full source and note into the journal, and the rows already land in `eval_scores`. `--unscored --format json` works (records on stdout, summary on stderr).
- `--unscored` conflicts with `--staged`, `--sample`, `--staging-root`, `--judge-model`, `--no-judge`, `--rebuild-cache` and the other mode flags, and takes `--limit` or `--count` (mutually exclusive). The doc's API lines list only `--limit` and `--count`; the conflicts keep `eval_scores` a record of the configured judges only.
- The backlog excludes succeeded receipts with no staged source (swept, or an unscored kind), reported on stderr as `N succeeded receipts have no staged source and are not counted`. The doc's rule reads "traces of successful receipts that are not yet covered"; counting the unscorable would pin `--count` above zero forever (1,148 of 5,737 today), and the doc's Non-Goals say scoring reads staging.
- Unparseable staged traces are listed on stderr and skipped, not fatal (`load_staged`, the dev command, still aborts on one).

### Tradeoffs
- Fail the whole `sb bootstrap` on an unrenderable eval unit vs warn and skip: failing is fail-closed, and the eval units are written last so every other unit has landed. Cost: `otto deploy` exits non-zero until borg.yml carries the `eval:` block (rollout step 1).
- Re-loading each selected fixture after `find_backlog` vs holding every uncovered fixture in memory during the walk: `--count` walks about 4,300 traces with sources up to 24k chars each, so the walk keeps only trace ids.
- One `evaluate` per judge model over the same fixtures vs a combined multi-judge pass: reuses `evaluate` and the judgment cache unchanged; deterministic rows are rewritten per model (idempotent).

### Open questions
- Rollout (operator-owned `~/.config/sb/borg.yml`; not edited). Before the finalization deploy, add:
  1. An `eval:` block: `judge-models: [<model>]` (the live check used `gemini-2.5-pro`), `env-bootstrap: {command: <the manifest command harvest uses>, env-file: /run/user/1000/sb-eval.env}`, optional `schedule` (default `*-*-* 05:00:00`) and `limit`. Without it `otto deploy` fails at the eval-unit step, and `--unscored` errors on empty `judge-models`.
  2. `llm.api-keys` entries for the other vendors so the map drives the vars: `gemini: GEMINI_API_KEY`, `openai: OPENAI_API_KEY` (the keep manifest's `secrets.env` already lists `gemini-api-key` and `openai-api-key`, and the harvest command's env-file carries every `secrets.env` entry). Today Gemini judging works only through fabric's own `.env`; the preflight guards only vars the map names.
  3. Phase 1's open item, same shape for oracle: `~/.config/sb/oracle.yml` needs `llm: {api-keys: {anthropic: ESCOTE_ANTHROPIC_API_KEY}}` (a dotfiles symlink; not touched).
- The first fire scores the whole 4,281-trace backlog unless `eval.limit` is set (3 traces took 3 minutes on `gemini-2.5-pro`, so one unlimited run is a long one). Is a limit of about 100 a day the intent, or one long catch-up run?
- Adding the Anthropic second judge to `eval.judge-models` doubles judge calls per trace; that is the Phase 5 config change the doc names.

## Phase 3: Planted-error injection

Ran on ripr 2026-10-17 on top of 021083d. Live run: `./target/debug/sb` (snapshotted into the scratchpad as `$S/p3/sb`) with `--config $S/p3/borg.yml`, i.e. the deployed `~/.config/sb/borg.yml` plus `eval: {judge-models: [gemini-2.5-pro], inject-workers: 4}`. No `otto install`, `otto deploy`, `sb bootstrap` or `systemctl`. The real `eval-cache.db` was written (additive: the `judge_trust` table, 6 trust rows, 917 score rows).

### Observed criterion output
- **Quote grounding catches 100% of fabricated quotes, zero LLM calls (PASS).** `eval::inject::tests::quote_grounding_catches_every_fabricated_quote_with_zero_llm_calls` plants a fabricated quote into every golden fixture plus five synthetic ones (>= 8 mutants), runs them through `evaluate` with `no_judge`, asserts `MockJudge::calls() == 0` and `MockClaimJudge::calls() == 0`, asserts every quote word is absent from its source, and asserts `quote-grounding` flags every mutant. Bites: building the quote from the source's own words fails it (and two sibling tests). Live: `quote-grounding fabricated-quote n 30, raw 1.00`.
- **`--inject --sample 30` (PASS).** `$S/p3/sb borg --config $S/p3/borg.yml eval --inject --sample 30 --report $S/p3/inject-report.txt` -> exit 0, 19m00s wall. Output, verbatim:
  ```
  planted-error injection  run=inject-20261017T135831Z  traces=30  judges=gemini-2.5-pro

  What this measures: catch rates for the four planted classes (number-swap,
  fabricated-quote, dropped-top-claim, who-change), and consistency between judges.
  What this misses:
    - omissions outside the planted classes
    - errors that every vendor shares
    - failures specific to long documents
    - style bias
    - the base rate of errors in production
  Kappa is agreement, not correctness. The flag rate is measured on unmutated
  production notes, which are not known-clean: a false-alarm proxy, not a
  true-negative rate. paired = flags the mutant AND passes its original.

  check                judge              class                 n  paired wilson-lb    raw   flag errors
  judge-distillation   gemini-2.5-pro     number-swap          26    0.27      0.15   0.46   0.27      0
  judge-distillation   gemini-2.5-pro     fabricated-quote     30    0.50      0.36   0.77   0.30      0
  judge-distillation   gemini-2.5-pro     dropped-top-claim    28    0.18      0.09   0.32   0.32      0
  judge-distillation   gemini-2.5-pro     who-change           17    0.65      0.45   0.88   0.29      0
  judge-claims         gemini-2.5-pro     number-swap          26    0.58      0.42   0.96   0.38      0
  judge-claims         gemini-2.5-pro     fabricated-quote    n/a     n/a       n/a    n/a    n/a    n/a
  judge-claims         gemini-2.5-pro     dropped-top-claim   n/a     n/a       n/a    n/a    n/a    n/a
  judge-claims         gemini-2.5-pro     who-change          n/a     n/a       n/a    n/a    n/a    n/a
  quote-grounding      (deterministic)    number-swap         n/a     n/a       n/a    n/a    n/a    n/a
  quote-grounding      (deterministic)    fabricated-quote     30    0.97      0.86   1.00   0.03      0
  quote-grounding      (deterministic)    dropped-top-claim   n/a     n/a       n/a    n/a    n/a    n/a
  quote-grounding      (deterministic)    who-change          n/a     n/a       n/a    n/a    n/a    n/a

  trust (paired catch >= 0.80 over >= 30 applicable traces, on every class the check can see, current rubric):
    judge-distillation   gemini-2.5-pro     untrusted: number-swap n 26 < min-samples 30; number-swap paired catch 0.27 < 0.80; fabricated-quote paired catch 0.50 < 0.80; dropped-top-claim n 28 < min-samples 30; dropped-top-claim paired catch 0.18 < 0.80; who-change n 17 < min-samples 30; who-change paired catch 0.65 < 0.80
    judge-claims         gemini-2.5-pro     untrusted: number-swap n 26 < min-samples 30; number-swap paired catch 0.58 < 0.80
    quote-grounding      (deterministic)    ungated (no model call)

  917 eval_scores rows and 6 judge_trust rows written
  ```
  - `judge_trust` rows per judge/check/class (`select judge_model, check_name, planted_class, rubric_version, paired, raw, flag, wilson_lower, sample_size from judge_trust`), one each, 6 total:
    - `'' | quote-grounding | fabricated-quote | v1 | 0.967 | 1.0 | 0.033 | 0.864 | 30`
    - `gemini-2.5-pro | judge-claims | number-swap | v2 | 0.577 | 0.962 | 0.385 | 0.418 | 26`
    - `gemini-2.5-pro | judge-distillation | dropped-top-claim | v2 | 0.179 | 0.321 | 0.321 | 0.09 | 28`
    - `gemini-2.5-pro | judge-distillation | fabricated-quote | v2 | 0.5 | 0.767 | 0.3 | 0.356 | 30`
    - `gemini-2.5-pro | judge-distillation | number-swap | v2 | 0.269 | 0.462 | 0.269 | 0.153 | 26`
    - `gemini-2.5-pro | judge-distillation | who-change | v2 | 0.647 | 0.882 | 0.294 | 0.449 | 17`
  - `eval_scores` rows from the run: `baseline 210` (30 originals x 7 checks), `inject 707` (mutants).
  - Outcome: both Gemini judge checks are untrusted on this sample. `judge-claims` catches 96% of number swaps raw, but it also flags 38% of the untouched originals, so its paired rate is 0.58. `judge-distillation` has no axis for claim accuracy and misses most single-claim mutations. Only 17 of 30 sampled notes carry a `who`, so with `--sample 30` the who-change class cannot reach the 30-sample floor. Quote grounding's one paired miss is an original that already had an ungrounded quote.
  - The first live attempt died after 21m51s with `inserting claim judgment: database is locked` (see Deviations: the shared cache connection). The successful run is the second.
- **Trust-rule unit tests (PASS), each bitten:**
  - `trust::tests::a_0_79_paired_catch_rate_on_one_applicable_class_is_untrusted`, `a_0_80_paired_catch_rate_on_every_applicable_class_with_30_samples_is_trusted` (also 24/30 through `ClassTally`). Bite: `<` -> `<=` on the rate fails the 0.80 test.
  - `one_of_one_caught_is_untrusted_below_min_samples`. Bite: dropping the sample check fails it.
  - `a_check_that_flags_every_note_scores_paired_zero_and_is_untrusted` (plus `inject::tests::a_judge_that_flags_everything_scores_paired_zero_and_is_untrusted` end to end). Bite: paired = raw fails both.
  - `a_row_for_an_older_rubric_version_is_untrusted`. Bite: ignoring the rubric fails it.
  - `judge_claims_is_never_untrusted_for_missing_a_who_change_or_fabricated_quote` (rows at 0.0 for who-change and fabricated-quote, none for dropped-top-claim). Bite: checking every class instead of the applicable ones fails it and three others.
- `otto ci` -> exit 0, `[ci] ✅ All CI checks passed!` (the first run failed clippy `type_complexity` on the worker result type; fixed with a type alias).

### Design decisions
- Module split: `borg/src/eval/inject.rs` (classes, `plant`, the matrix `applies`, `flags`, `ClassTally`, `run_inject`, `InjectReport::render`) and `borg/src/eval/trust.rs` (`judge_trust` table `TrustStore`, `judge_check_trust`, `TrustState`). Trust is its own module because Phases 7-9 read it without touching injection.
- Mutations (`inject::plant`), all deterministic over the full staged source:
  - number-swap: the first standalone integer (`\b\d+\b`, so `H100`/`x86` are skipped) in any claim becomes the smallest `n+k` the source never states.
  - fabricated-quote: the top claim's quote becomes 6 words from a fixed list of real words (`marmalade`, `trombone`, ...) absent from the source; fewer than 3 absent -> not applicable.
  - dropped-top-claim: needs >= 2 claims, so the mutant still has a claim list.
  - who-change: the first claim with a `who` gets a stand-in name whose words the source never uses.
- What "flags" means (`inject::flags`): `judge-distillation` flags any axis below 3 (a perfect note scores 3/3/3), `judge-claims` any verdict not `supported`, `quote-grounding` any quote not grounded (no quote passes). A missing judge result is an error, never a pass (`ClassTally::add`). This is the one definition; Phase 7's "failure" should reuse it.
- Sample (`inject::sample_fixtures`): all staged traces, filtered to those at least one class applies to, then the existing stratified sample (`fixtures::stratified_sample`, pulled out of `load_staged` unchanged). `--sample` defaults to `eval.trust.min-samples`.
- Judges: every `eval.judge-models` model, the same list `--unscored` uses; empty is an error. Originals and mutants go through the unchanged `evaluate`, so caching, error capture and deterministic metrics are shared.
- Rows: originals are written as `baseline` rows (they are the published note at its current distill hash), mutants as `inject` rows with `run_id = inject-<UTC stamp>:<class>`. `judge_trust` is `INSERT OR REPLACE` on its four-part key, so the latest measurement per rubric is current and older rubric rows stay as history.
- Concurrency: `eval.inject-workers` (default 4) scoped threads, each building its own judges; a trace's original and mutants stay on one worker, original first, so mutants whose claim list is unchanged get cache hits on the original's claim verdicts.
- Config: `eval.trust.min-paired-catch-rate` (0.80), `eval.trust.min-samples` (30), `eval.inject-workers` (4), validated at load in `Config::validate` (`EvalConfig::validate`); annotated in `config/templates/borg.yml.example`.
- Wilson bound: `calc::wilson_lower_bound`, one-sided 95% (z = 1.645); 24/30 -> 0.6575, matching the doc's "near 0.66".

### Deviations
- `JudgmentCache` now holds its connection behind a `Mutex` and is shared by the inject workers; it also sets a 5 s busy timeout. The first live run gave each worker its own connection and died at 21m51s with `database is locked`. In rollback-journal mode, SQLite returns BUSY without calling the busy handler when concurrent writers could deadlock (one holds SHARED and wants RESERVED, the other holds RESERVED and wants EXCLUSIVE), so the busy timeout alone could not prevent it. With one connection the workers never contend for the file lock, and `cache::tests::one_cache_shared_by_many_threads_writes_without_lock_errors` pins it (8 threads x 100 put/get on a file cache).
- `sample_size` counts traces where the mutation applied AND both sides were scored; a trace with a failed judge call is reported in the `errors` column, not in `n`. The doc says "distinct traces where the mutation was actually generated and applies"; an errored pair carries no evidence either way.
- Flag rate is computed per class over that class's applicable originals, not once per check, so each `judge_trust` row is self-contained.
- `--inject` writes `baseline` rows for the originals, which can cover those traces for `--unscored` (same judge, hash and rubric). The doc only says the inject report reads `inject` rows; the report is computed in memory and never reads them back.
- The `dropped-top-claim` mutant is also sent to `judge-claims` (one wasted call per trace, its row stored as `inject` but reported `n/a`), because `run_inject` reuses `evaluate`, which always runs both judges.
- `--inject` conflicts with `--format` (text report only), `--staged`, `--unscored`, `--judge-model`, `--no-judge`, `--staging-root`; it takes `--sample`, `--report`, `--patterns-dir`, `--rebuild-cache`. `--sample` now requires `--staged` or `--inject` (an `ArgGroup`).
- The doc says `eval/inject.rs`; the file is `borg/src/eval/inject.rs` (same module, crate-relative path).

### Tradeoffs
- Reusing `evaluate` per trace group vs a purpose-built judge loop that calls only applicable checks: one code path for caching and error rows, at the cost of the one wasted claims call above.
- Threads with a shared mutex-guarded cache vs sequential: the live run is about 7 judge calls per trace on gemini-2.5-pro; sequential would multiply wall time by the worker count. The mutex is held for single statements only, never across a judge call.
- Absolute flag threshold (any axis < 3) for `judge-distillation` vs a relative one (mutant scored lower than original): the doc defines flag rate on unmutated notes as an absolute property, and remediation will need an absolute pass/fail.

### Open questions
- With `--sample 30`, who-change applied to 17 traces and number-swap to 26, so no judge can reach `min-samples 30` on those classes. Should `--sample` mean "N applicable traces per class" (draw until each class reaches N), or does the operator run `--sample 60`?
- `judge-distillation` has no axis for claim accuracy, and it scores 0.18-0.65 paired. Is it meant to be gated on number-swap and dropped-top-claim at all, or should the matrix narrow it (a doc change, like the round 2 `judge-claims` narrowing)?
- `judge-claims` flags 38% of unmutated originals. That is either real production error or judge noise, and Phase 5's cross-vendor kappa is the first place to tell which.

## Phase 4: `uv` project and verdict ingest

Ran on ripr 2026-10-17 on top of 522296d. Live runs used a snapshot of the debug build (`$S/p4/sb`, `$S` = the session scratchpad) with `--config $S/p3/borg.yml` (the deployed `borg.yml` plus `eval: {judge-models: [gemini-2.5-pro], inject-workers: 4}`). No `otto install`, `otto deploy`, `sb bootstrap` or `systemctl`. The real `~/.local/share/sb/borg/eval-cache.db` took 4 additive rows (the two `otto eval-py` traces); the round-trip and stale checks ran against a scratch copy of that db (`XDG_DATA_HOME=$S/p4/xdg`, staging symlinked).

### Observed criterion output
- **`otto eval-py` writes verdict JSONL for the latest scored traces: PASS.** `REAL_HOME=$HOME SB=$S/p4/sb SB_BORG_CONFIG=$S/p3/borg.yml EVAL_PY_LIMIT=2 EVAL_PY_OUT=$S/p4/out $S/phase0/with-secret.sh OPENAI_API_KEY openai-api-key otto eval-py` -> `[eval-py] finished successfully  1m19s`, exit 0. Key lines:
  ```
  scored: 2 traces selected, 0 scored traces no longer staged, 0 re-distilled since scoring
  20 verdicts for 2 records written to .../p4/out/verdicts-20261017T142800Z.jsonl
  verdicts: 20 read, 20 loaded into 4 eval_scores rows, 0 groups rejected as stale
  ```
  The two traces are `cl-0d9fcb5b` (video, 10 claims) and `cl-0f243a01` (article, 8 claims): 2 deepeval verdicts plus 18 minicheck verdicts. The OpenAI key rode the child env only (`with-secret.sh`); no value was printed or written.
- **After `--ingest-verdicts`, `eval_scores` rows carry `deepeval` and `minicheck` metrics: PASS.** `select trace, run_kind, run_id, judge_model, check_name, rubric_version, content_hash, error from eval_scores where check_name in ('deepeval','minicheck')` on the real db:
  ```
  cl-0d9fcb5b|baseline||gpt-4o-mini|deepeval|v2|1113fdfb1755fe31|
  cl-0d9fcb5b|baseline||roberta-large|minicheck|v2|1113fdfb1755fe31|
  cl-0f243a01|baseline||gpt-4o-mini|deepeval|v2|ccb532bdd9134f3f|
  cl-0f243a01|baseline||roberta-large|minicheck|v2|ccb532bdd9134f3f|
  ```
  `error` is null on all four. DeepEval scored 0.09 and 0.38; the minicheck rows hold `claims.<id>.supported.{value,detail.label}` per claim.
- **Stale `content_hash` rejected, naming the trace: PASS.** The otto run's verdict file with `cl-0d9fcb5b`'s hash rewritten to `00000stale000000`, `$S/p4/sb borg --config ... eval --ingest-verdicts $S/p4/stale-verdicts.jsonl` (scratch db):
  ```
  rejected 11 verdict(s) for trace cl-0d9fcb5b (baseline run '', content_hash 00000stale000000): stale content_hash: the trace's current distill hash is 1113fdfb1755fe31
  verdicts: 20 read, 9 loaded into 2 eval_scores rows, 1 groups rejected as stale
  Error: 1 verdict group(s) rejected as stale for trace(s): cl-0d9fcb5b
  ```
  exit 1. The 9 verdicts for the other trace loaded; none of the stale ones did.
- **Round trip, baseline + candidate + injection, one tool, three distinct rows: PASS.** Trace `cl-0f243a01` on the scratch db. The baseline is the real staged note (`ccb532bdd9134f3f`); the injection is the real Phase 3 row (`inject-20261017T135831Z:number-swap` at `be85ac2189dc07a8`); no candidate run exists yet (Phase 8), so I inserted one `candidate | distill-article-feedface` judge row at the baseline hash into the scratch copy. Three records (the baseline record re-labelled per run) went through `python -m verdict --tool minicheck` (24 verdicts), then `--ingest-verdicts`:
  ```
  verdicts: 24 read, 24 loaded into 3 eval_scores rows, 0 groups rejected as stale
  cl-0f243a01|baseline||roberta-large|minicheck|ccb532bdd9134f3f||609
  cl-0f243a01|candidate|distill-article-feedface|roberta-large|minicheck|ccb532bdd9134f3f||609
  cl-0f243a01|inject|inject-20261017T135831Z:number-swap|roberta-large|minicheck|be85ac2189dc07a8||609
  ```
  The trace's 6 `judge-distillation` rows are unchanged (6 before and after). The three records carry the same note text (the injection's mutated note is not exported anywhere yet), so the three rows hold equal values; what the check proves is key separation, not scores.
- **Unit tests.** Rust (`cargo test`, 20 new in borg, 2 in sb) and Python (`otto eval-test`, 14 tests, 0.03 s). Each bite below was made, the named tests failed, then it was restored:
  - `judge_run` accepting any baseline hash: `a_stale_baseline_hash_is_rejected_naming_the_trace_while_current_verdicts_still_load` and `re_distilling_a_trace_after_the_verdicts_were_written_makes_them_stale` fail.
  - ingest writing every row with `run_id = ''`: `baseline_candidate_and_inject_verdicts_from_one_tool_land_in_three_distinct_rows` fails.
  - `minicheck.run` dropping `claim_id`: the claim-id test fails. `contract.verdict` not copying `content_hash`: 2 fail. `run_tools` catching `KeyError` instead of any error: the failing-tool test fails.
  - `bin/eval-nltk` with a wrong pinned sha256: exits 1 naming both hashes and leaves no dest dir.
- **MiniCheck model choice: `roberta-large`.** Same trace `cl-c89ab882` (22,784-char source, 10 claims), same venv (CPU torch 2.14.1, transformers 5.17.0), warm model, one claim per call, a one-claim warm-up excluded. Spike scripts: `$S/p4/spike_models.py`; labels are 1 = supported:

  | | per-claim CPU s (mean) | model load | labels | agreement with Gemini (9 of 10 supported, claim 6 unsupported) |
  |---|---|---|---|---|
  | roberta-large | 2.87 (2.80-3.00) | 1.0 s | `[0,1,0,1,0,0,0,1,0,0]` (3 supported) | 4 of 10 |
  | deberta-v3-large | 6.98 (6.62-7.30) | 26.3 s | `[0,1,0,1,1,0,0,0,1,0]` (4 supported) | 5 of 10 |
  | Gemini (Phase 0) | | | `[1,1,1,1,1,0,1,1,1,1]` | |

  deberta costs 2.4x the latency and 26x the load for one more agreeing claim on a single trace, which is noise. Its 2048-token window is also not used: `MiniCheck.__init__` sets `chunk_size = 500 if flan-t5-large else 400` (`minicheck/minicheck.py:148`), so both models score 400-token chunks of the source. Both under-support heavily against Gemini, so Phase 5's kappa is where that gets measured, not here. roberta stays the default (`--minicheck-model deberta-v3-large` switches it).
- **Phase 0 findings applied.** `accelerate` is a direct dependency. `punkt_tab` comes from `bin/eval-nltk` into a private `NLTK_DATA` that `otto eval-py` exports (pinned URL, sha256-checked, idempotent; `minicheck.require_nltk_data` fails loudly if it is missing). torch is the CPU wheel (`torch 2.14.1+cpu`) from an explicit `pytorch-cpu` uv index, clean with no workaround. MiniCheck is the GitHub repo pinned to `b58b9fa` by `rev`. DeepEval resolved to 4.3.1.
- `otto ci` -> exit 0, `[ci] ✅ All CI checks passed!` (the first run failed only `agents-map`, see Deviations). `eval-py` and `eval-test` are not in `ci`: `uv sync` pulls torch and `eval-py` calls OpenAI.

### Design decisions
- Package layout: project `sb-eval` (`eval/pyproject.toml`), package `verdict` with `contract.py` (the JSONL contract), `tools/deepeval.py`, `tools/minicheck.py`, `__main__.py`. The tools take their scorer or metric as an argument so the tests run with fakes and no model.
- One row per `eval_scores` key (`verdicts.rs:metrics_of`): a tool's per-claim verdicts and note-level metrics fold into one row's `metrics_json` (`{tool_version, metrics: {...}, claims: {"<id>": {...}}}`). The doc's key has no claim or metric part, so a row per verdict would collide.
- What "stale" means (`verdicts.rs:judge_run`): a baseline verdict must carry the trace's CURRENT distill hash, recomputed from staging through `judged_input` (the scorer's own function); a candidate or inject verdict must match a run that already has a row at that hash. A baseline for a trace swept from staging is rejected too (the hash cannot be confirmed). The rule needs no baseline row to exist, so `--staged` records ingest as well.
- Fail closed on the contract: `Verdict` is `deny_unknown_fields`; an empty `model` is rejected (`''` means a deterministic check); a tool named like a built-in check (`judge-claims`, `quote-grounding`, ...) is rejected so a tool cannot write a Rust check's rows. All rows are built before the first write, so a malformed group fails the whole file.
- Valid verdicts load even when others are stale, the stale ones are named on stderr, and the command then exits non-zero (`IngestOutcome::check`). Loading the good half is the doc's "loads only when it matches"; the non-zero exit is the "loudly".
- `--scored --limit N` (new, see Deviations) is the export selector; `--limit` is required with it, so an unbounded run cannot send every scored trace to a paid judge.
- `otto eval-py` ingests after writing, and ingests even when a tool failed on some trace, then exits with the tool's status, so a partial verdict file is loaded but never reads as a clean run.

### Deviations
- Added `--scored --limit N` (`borg/src/eval/scored.rs`, `sb/src/cli/borg.rs:run_eval_scored`). The doc's API lists no way to export "the latest scored traces", and the `--staged --sample` selector is not tied to the score table. It reuses `evaluate` with `no_judge` and the same JSONL renderer.
- `otto eval-py` also ingests the verdicts. The phase text says "writing verdict JSONL"; the doc's loop and the "run by `sb-eval.service`" line need the ingest to close, and it is one command.
- `bin/agents-map` now skips an `AGENTS.md` whose directory has no `src/`. `eval/AGENTS.md` is not a Rust crate, and the script's reverse check died on `find eval/src`. (`otto ci` red until fixed; Rust crates are checked exactly as before.)
- The round-trip candidate row was inserted by hand into a scratch db copy, because no candidate run can exist before the GEPA phase. The injection and baseline rows are real.
- `eval/` is wired into neither `sb-eval.service` nor `ci`. The service would need `uv`, torch and the OpenAI key in its env bootstrap, which is deploy-side (see Open questions).

### Tradeoffs
- One `uv` project and one lock for DeepEval and MiniCheck vs two venvs (Phase 0 used two): one `uv.lock` pins everything and `otto eval-py` is one `uv run`. Cost: DeepEval and the untrusted MiniCheck repo share an environment; MiniCheck is pinned by commit.
- MiniCheck by git `rev` over an unpinned branch: reproducible, at the price of a manual bump.
- A batched `scorer(docs, claims)` call per record vs one call per claim: Phase 0 measured no CPU gain from batching, but one call per record is simpler and gives the same labels.
- `roberta-large` over `deberta-v3-large`: numbers above.

### Open questions
- `nltk` still tries `nltk.download('punkt')` when MiniCheck imports, is refused (`Security Violation [pathsec.urlopen]`, plus a warning that the `~/.cache/sb/eval/nltk` ancestor is group-writable), and carries on using `punkt_tab`. It is noise, not a failure. I did not set `NLTK_ALLOW_PROXIED_URLOPEN`, which would switch an SSRF guard off. Is the noise acceptable, or should `otto eval-py` filter it?
- Wiring `eval-py` into the unattended path: `sb-eval.service` runs `--unscored` only. Running DeepEval and MiniCheck daily needs `uv` and the OpenAI key on the service's env-bootstrap and a model cache. Phase 5 decides which tools survive; should the service step wait for that?
- The injected mutant's note text is not exported (`--scored` exports baselines only), so injection verdicts for a tool need a mutant exporter. Phase 5's agreement table is the first consumer; it will need one.

## Phase 5: Cross-vendor agreement and judge default

Ran on ripr 2026-10-17 on top of 11b67df. Live runs used a snapshot of the debug build (`$S/p5/sb`, `$S` = the session scratchpad) with `--config $S/p5/borg.yml`: the deployed `borg.yml` plus `llm.api-keys: {anthropic: ESCOTE_ANTHROPIC_API_KEY, gemini: GEMINI_API_KEY}` and `eval: {inject-workers: 4}`, with NO `judge-models`, so the run used the new default list. Keys were decrypted onto the child env only (`$S/phase0/with-secret.sh`, nested for two keys, plus `openai-api-key` for `otto eval-py`); no value was printed or written. No `otto install`, `otto deploy`, `sb bootstrap` or `systemctl`. The real `eval-cache.db` was written (additive: 1,834 score rows and 11 trust rows per inject run, 60 tool rows).

### Observed criterion output
- **The report prints pairwise kappa over >= 30 traces: PASS.** Sequence:
  1. `$S/p5/sb borg --config $S/p5/borg.yml eval --inject --sample 30` (run `inject-20261017T144305Z`) -> exit 0, 11m17s. Gemini came entirely from the judgment cache: the sample is the same 30 traces as Phase 3's `inject-20261017T135831Z` (30 of 30 shared) and every Gemini number matches Phase 3. Claude made 3 failed calls (`fabric -p judge-claims failed: empty response` x2 on `hv-e01f3e96`, `judge-distillation` x1 on `hv-cbddbd03`), recorded as errors.
  2. `EVAL_PY_LIMIT=30 SB=$S/p5/sb SB_BORG_CONFIG=$S/p5/borg.yml EVAL_PY_OUT=$S/p5/out otto eval-py` -> exit 0, 12m09s. `scored: 30 traces selected, 0 scored traces no longer staged, 0 re-distilled since scoring`; `verdicts: 269 read, 269 loaded into 60 eval_scores rows, 0 groups rejected as stale`. The 30 most recently scored traces are exactly the inject run's originals.
  3. Re-ran the same `--inject --sample 30` (run `inject-20261017T150659Z`) -> exit 0, 1m01s: every judge call cached except the 3 errored ones, which retried and succeeded, so both judges have 30 notes. Report: `$S/p5/inject2-report.txt`.
- Planted-error table, run 3, verbatim:
  ```
  check                judge              class                 n  paired wilson-lb    raw   flag errors
  judge-distillation   gemini-2.5-pro     number-swap          26    0.27      0.15   0.46   0.27      0
  judge-distillation   gemini-2.5-pro     fabricated-quote     30    0.50      0.36   0.77   0.30      0
  judge-distillation   gemini-2.5-pro     dropped-top-claim    28    0.18      0.09   0.32   0.32      0
  judge-distillation   gemini-2.5-pro     who-change           17    0.65      0.45   0.88   0.29      0
  judge-claims         gemini-2.5-pro     number-swap          26    0.58      0.42   0.96   0.38      0
  judge-claims         gemini-2.5-pro     fabricated-quote    n/a     n/a       n/a    n/a    n/a    n/a
  judge-claims         gemini-2.5-pro     dropped-top-claim   n/a     n/a       n/a    n/a    n/a    n/a
  judge-claims         gemini-2.5-pro     who-change          n/a     n/a       n/a    n/a    n/a    n/a
  judge-distillation   claude-sonnet-5    number-swap          26    0.27      0.15   0.58   0.46      0
  judge-distillation   claude-sonnet-5    fabricated-quote     30    0.50      0.36   1.00   0.50      0
  judge-distillation   claude-sonnet-5    dropped-top-claim    28    0.07      0.02   0.50   0.54      0
  judge-distillation   claude-sonnet-5    who-change           17    0.53      0.34   0.94   0.47      0
  judge-claims         claude-sonnet-5    number-swap          26    0.69      0.53   1.00   0.31      0
  judge-claims         claude-sonnet-5    fabricated-quote    n/a     n/a       n/a    n/a    n/a    n/a
  judge-claims         claude-sonnet-5    dropped-top-claim   n/a     n/a       n/a    n/a    n/a    n/a
  judge-claims         claude-sonnet-5    who-change          n/a     n/a       n/a    n/a    n/a    n/a
  quote-grounding      (deterministic)    number-swap         n/a     n/a       n/a    n/a    n/a    n/a
  quote-grounding      (deterministic)    fabricated-quote     30    0.97      0.86   1.00   0.03      0
  quote-grounding      (deterministic)    dropped-top-claim   n/a     n/a       n/a    n/a    n/a    n/a
  quote-grounding      (deterministic)    who-change          n/a     n/a       n/a    n/a    n/a    n/a
  ```
- Kappa table, run 3, verbatim (printed between the planted-error table and the trust lines):
  ```
  pairwise judge agreement on the 30 unmutated originals (Cohen's kappa over current baseline rows; kappa undefined (n/a) when both raters gave every item one label)
  raters (notes labelled):
    deepeval (gpt-4o-mini)                       30
    judge-claims (claude-sonnet-5)               30
    judge-claims (gemini-2.5-pro)                30
    judge-distillation (claude-sonnet-5)         30
    judge-distillation (gemini-2.5-pro)          30
    minicheck (roberta-large)                    30

  trace level (+ = flags the note):
  rater a                                      rater b                                          n   ++   +-   -+   -- a-flag b-flag  agree  kappa
  deepeval (gpt-4o-mini)                       judge-claims (claude-sonnet-5)                  30    8   11    1   10   0.63   0.30   0.60   0.28
  deepeval (gpt-4o-mini)                       judge-claims (gemini-2.5-pro)                   30    8   11    3    8   0.63   0.37   0.53   0.13
  deepeval (gpt-4o-mini)                       judge-distillation (claude-sonnet-5)            30   10    9    5    6   0.63   0.50   0.53   0.07
  deepeval (gpt-4o-mini)                       judge-distillation (gemini-2.5-pro)             30    7   12    2    9   0.63   0.30   0.53   0.16
  deepeval (gpt-4o-mini)                       minicheck (roberta-large)                       30   18    1    8    3   0.63   0.87   0.70   0.25
  judge-claims (claude-sonnet-5)               judge-claims (gemini-2.5-pro)                   30    8    1    3   18   0.30   0.37   0.87   0.70
  judge-claims (claude-sonnet-5)               judge-distillation (claude-sonnet-5)            30    7    2    8   13   0.30   0.50   0.67   0.33
  judge-claims (claude-sonnet-5)               judge-distillation (gemini-2.5-pro)             30    6    3    3   18   0.30   0.30   0.80   0.52
  judge-claims (claude-sonnet-5)               minicheck (roberta-large)                       30    9    0   17    4   0.30   0.87   0.43   0.12
  judge-claims (gemini-2.5-pro)                judge-distillation (claude-sonnet-5)            30    8    3    7   12   0.37   0.50   0.67   0.33
  judge-claims (gemini-2.5-pro)                judge-distillation (gemini-2.5-pro)             30    7    4    2   17   0.37   0.30   0.80   0.55
  judge-claims (gemini-2.5-pro)                minicheck (roberta-large)                       30   11    0   15    4   0.37   0.87   0.50   0.16
  judge-distillation (claude-sonnet-5)         judge-distillation (gemini-2.5-pro)             30    6    9    3   12   0.50   0.30   0.60   0.20
  judge-distillation (claude-sonnet-5)         minicheck (roberta-large)                       30   14    1   12    3   0.50   0.87   0.57   0.13
  judge-distillation (gemini-2.5-pro)          minicheck (roberta-large)                       30    8    1   18    3   0.30   0.87   0.37   0.02

  claim level (+ = claim supported):
  rater a                                      rater b                                          n   ++   +-   -+   -- a-supp b-supp  agree  kappa
  judge-claims (claude-sonnet-5)               judge-claims (gemini-2.5-pro)                  239  195    5    1   38   0.84   0.82   0.97   0.91
  judge-claims (claude-sonnet-5)               minicheck (roberta-large)                      239  115   85    4   35   0.84   0.50   0.63   0.26
  judge-claims (gemini-2.5-pro)                minicheck (roberta-large)                      239  112   84    7   36   0.82   0.50   0.62   0.24

  ```
  Trust after run 3: all four judge checks untrusted (`judge-claims claude-sonnet-5`: `number-swap n 26 < min-samples 30; number-swap paired catch 0.69 < 0.80`); `quote-grounding` ungated. `1834 eval_scores rows and 11 judge_trust rows written`.
- **Port/drop verdict per `uv` tool in Resolved Decisions, citing numbers: PASS.** Three entries dated "2026-10-17, phase 5" in the design doc: MiniCheck dropped, DeepEval dropped, Gemini default with Anthropic second. Summary:
  - MiniCheck: flags 26/30 originals, so its number-swap paired catch is at most 4/26 = 0.15 against `judge-claims` 0.58 | 0.69; claim kappa vs `judge-claims` 0.24 | 0.26 while the fabric vendors agree at 0.91. Drop.
  - DeepEval: fails 19/30 originals (mean 0.435), so paired catch is at most 0.37 (fabricated-quote) and 0.42 (number-swap), below the 0.80 floor on classes trust requires; trace kappa 0.07-0.28 vs the fabric raters. Drop.
- **Gemini default, Anthropic second (live):** `$S/p5/sb borg --config $S/p5/borg.yml eval --staged --sample 1` (no `--judge-model`) -> exit 0, header `distillation eval  (1 fixtures, judge-model=gemini-2.5-pro)`, `judge errors: 0 of 1`. `--judge-model claude-sonnet-5` on the same config -> exit 0, `judge errors: 0 of 1` (the Anthropic key from `llm.api-keys` reaches fabric). `--inject` with no `judge-models` configured printed `judges=gemini-2.5-pro, claude-sonnet-5`.
- **Unit tests, each bitten (break made, named tests failed, restored):**
  - `eval::agreement::tests`: `kappa_matches_the_textbook_two_by_two` (0.40), `perfect_agreement_on_mixed_labels_is_kappa_one`, `perfect_disagreement_on_balanced_labels_is_kappa_minus_one`, `chance_level_agreement_is_kappa_zero`, `all_agree_on_one_label_is_undefined_not_perfect` (both all-flag and all-pass), `one_rater_using_one_label_is_kappa_zero_not_undefined`, `no_shared_item_is_undefined`, `trace_flags_follow_each_raters_rule`, `only_current_error_free_baseline_judge_rows_are_raters`, `agreement_pairs_two_vendors_on_shared_notes_at_both_levels`, `a_rater_that_judged_an_older_distillation_is_not_paired`, `render_prints_the_two_by_two_and_na_for_an_undefined_kappa`.
    - Dropping the `p_e == 1` guard: `all_agree_on_one_label_is_undefined_not_perfect` and `render_prints_..._na_for_an_undefined_kappa` fail.
    - Pairing on trace only (ignoring `content_hash`): `a_rater_that_judged_an_older_distillation_is_not_paired` fails.
    - Accepting inject rows as raters: 2 tests fail.
  - `eval::inject::tests::the_report_prints_pairwise_kappa_between_both_judges_and_any_ingested_tool` (two mock judges, kappa section between the planted table and trust, a tool row ingested at an original's hash joins the next report). Not rendering the agreement: fails.
  - `config::tests::eval_judge_models_default_to_gemini_then_anthropic_and_an_explicit_list_wins`. Default back to `Vec::new()`: fails.
  - `cli::borg::tests::the_default_judge_is_gemini_and_a_flag_or_config_overrides_it`. `eval_opts` not reading the default judge: fails.
  - Inverted by name: `inject::tests::run_inject_refuses_an_empty_judge_model_list` built its empty list from `EvalConfig::default()`, which now names two judges; it now sets `judge_models: Vec::new()` explicitly (an explicit empty list still errors).
- `otto ci` -> exit 0, `[ci] ✅ All CI checks passed!` (first run failed only `cargo fmt --check`; `cargo fmt --all` fixed it).

### Design decisions
- Agreement module (`borg/src/eval/agreement.rs`): a rater is `(check, judge_model)`; notes key on `(trace, content_hash)`; only current, error-free `baseline` rows with a model are raters (`is_current_rater_row`: judge checks on their current rubric, tool rows on the distillation rubric their record carried). Pairs are every unordered pair of raters with a shared item, not only the same check across models, so fabric judges compare with the tools and with each other's other check.
- Two levels: trace (`trace_flag`: `inject::flags`' rule for the judge checks, any MiniCheck label 0, DeepEval summarization < 0.5, the threshold the tool constructs the metric with) and claim (`claim_support`: `judge-claims` verdict i -> claim id i+1, MiniCheck `claims.<id>.supported.detail.label`).
- Kappa is `None` (printed `n/a`) with no shared item or when chance agreement is 1 (both raters gave every item one label: 0/0). One rater constant and the other mixed is kappa 0, defined. The 2x2 cells and each rater's positive rate print beside kappa so a degenerate case is readable.
- The inject report reads the agreement back from `eval_scores` after its own writes (`ScoreStore::baseline_rows_at(trace, content_hash)` per sampled original), so it covers this run's judges plus any tool verdicts already ingested at the same hash. Tools join by running `otto eval-py` over the sample and re-running `--inject`, which is all judgment-cache hits (1m01s here).
- Default judge (`borg/src/config/eval.rs`): `DEFAULT_JUDGE_MODEL = gemini-2.5-pro`, `SECOND_JUDGE_MODEL = claude-sonnet-5` (the distiller's `llm.model`, a name fabric already resolves), default `judge-models` is both in that order, `EvalConfig::default_judge_model` is the first entry. `--judge-model` became `Option<String>`; `sb/src/cli/borg.rs:eval_opts` resolves `None` to the default judge on a judged run, and to `""` (no model) on `--no-judge` and `--scored`, so an unjudged record never names a judge that did not run.
- Phase 4 open question 3 (mutant export): not needed. The doc's kappa is over traces' current baseline rows, and the port rule's catch-rate arm is settled from baselines alone because paired catch rate <= the share of originals the check passes. No mutant exporter was built.

### Deviations
- Required set: the doc says the required checks are the deterministic ones plus the default judge's, and adding the second judge is a config change. Phase 2 made the required set every model in `eval.judge-models` (one list), so with Anthropic in the default list it is required by default. Kept that way: kappa needs both judges on the same notes, and a second judge that is not required would never retry an error row. Removing it from the list is the config change.
- The kappa section pairs every rater, not only the same `check_name` across judge models as the doc's pairing key reads; same-check pairs are a subset of the table.
- Tools did not join the first inject run: they score rows that run writes. The kappa table above is from the cached re-run.
- No code was removed from `eval/`. The doc says a tool that adds nothing is removed; the verdict is written, the removal is left as an open question because it touches Phase 8's coverage floor.
- `oracle`'s relevance judge keeps fabric's default model (it has hand-label calibration and is not part of this loop's judge default).

### Tradeoffs
- Bounding the tools' paired catch rate from baselines vs building a mutant exporter and running the tools on mutants: the bound is exact math and settles both tools (MiniCheck <= 0.15, DeepEval <= 0.37/0.42 on two classes), at zero added code. A tool with a low original flag rate would have needed the exporter.
- `claude-sonnet-5` (same model as the distiller) vs a different Anthropic model for the second judge: the doc asks for the vendor, and this name is proven through fabric in production. Same-model self-preference would push Claude toward `supported`, which makes its agreement with Gemini on unsupported claims more telling, not less.
- Reading agreement back from the table vs computing it from in-memory records: one extra query per trace, and tool verdicts are included without a second code path.

### Open questions
- Phase 3's question, answered as far as agreement can: Gemini `judge-claims` flags 11/30 originals, Claude 9/30, 8 both; trace kappa 0.70; claim kappa 0.91 over 239 claims with 38 unsupported by both and 6 split. The flags are a cross-vendor signal, not Gemini noise. Whether those 38 claims are production errors or an error both vendors share is not something kappa can tell.
- Removing DeepEval and MiniCheck from `eval/` (and torch from `uv.lock`), and rewriting Phase 8's coverage floor without "DeepEval coverage": do it now as its own commit, or with Phase 8?
- Phase 4 open question 2 (wire `eval-py` into `sb-eval.service`): with both tools dropped there is nothing to wire; the service stays `--unscored` only.
- Deploy: the timer now judges with two vendors by default, so `sb-eval.service`'s env file needs the Anthropic AND Gemini keys, and `borg.yml` needs `llm.api-keys.gemini` for the preflight to guard it. Every trace costs about 4 judge calls instead of 2.
- Claude's `judge-claims` beats Gemini's on number-swap paired (0.69 vs 0.58) and its `judge-distillation` flags 0.50 of originals. The default stays Gemini on the vendor-bias argument; flag it if catch rate should decide the default instead.
- With `--sample 30` only fabricated-quote reaches `min-samples 30` (number-swap 26, dropped-top-claim 28, who-change 17), and every judge check can see number-swap, so no judge can be trusted at this sample size whatever its rate (Phase 3's open question, still open).

### Background processes
None left running.

## Phase 6: Mined queries and answerability

Ran on ripr 2026-10-17 on top of 62865e4. Live runs used the debug build (`./target/debug/sb`, snapshotted as `$S/p6/sb`), not `~/.cargo/bin/sb`. No `otto install`, `otto deploy`, `sb bootstrap` or `systemctl`. The answerability run used `--config $S/oracle-keys.yml` (the deployed `oracle.yml` plus `llm: {api-keys: {anthropic: ESCOTE_ANTHROPIC_API_KEY}}`, Phase 1's scratch copy) under `env -u ANTHROPIC_API_KEY`; the deployed `oracle.yml` was not touched. Query file: `$S/p6/q.yml`, not `/tmp/q.yml`.

### Observed criterion output
- **`sb oracle eval --mine-queries <path>` writes >= 50 queries, all personal-scope: PASS.** `./target/debug/sb oracle eval --mine-queries $S/p6/q.yml` -> exit 0, 7.1 s wall:
  ```
  mined 237 distinct queries (267 knowledge_search calls) from 1701 personal-scope sessions -> .../scratchpad/p6/q.yml
  sessions: 4896 total, 1701 personal, 3195 work (dropped), 0 missing or unknown scope (dropped), 58 personal without a transcript
  ```
  - Independent check (scratch script over `clyde session export --include-archived --no-reindex`, hosts `desk` 4,726 + `ripr` 170): all 237 mined queries occur in a personal-scope transcript; 0 are work-only; 134 distinct work-only queries were excluded.
- **`sb oracle eval --queries <mined> --target answerability` prints the loss count, eligible and compared pairs, and per-outcome counts: PASS (at `--k 3`, see Deviations).** `env -u ANTHROPIC_API_KEY $S/p6/sb oracle --config $S/oracle-keys.yml eval --queries $S/p6/q.yml --target answerability --k 3 --report $S/p6/answerability-report.txt` -> exit 0, 23m28s, 0 judge warnings. Verbatim, losses list trimmed to its first 5 of 37 lines:
  ```
  answerability eval @ K=3  (237 queries, judge-model=<fabric default>, retrieval=configured)

  What this measures: for each query's top hits, whether the note answers the query
  as well as its own source does (source >= 2 and note <= 1 is a distillation loss).
  What it cannot see:
    - only notes oracle already retrieved enter the comparison; a distillation that
      dropped the very terms a query needs never surfaces as a hit, so that loss is
      not measured here
    - sources come only from borg's staging window (14 days by default), so the
      score skews to recent notes

  pairs: 711 eligible (every retrieved query-hit pair), 579 compared
  outcomes:
    compared           579
    no-trace           71
    source-expired     51
    source-unreadable  10
    judge-error        0

  distillation loss: 37 of 579 compared pairs (6.4%)
    source answers (>= 2): 227   note answers (>= 2): 222   sources truncated to 22000 chars: 297

  losses (source score / note score):
    2/1  notes/git-worktrees-the-secret-sauce-to-multi-agent-workflows.md  [cl-4790677f]  "git worktree bare repo clone directory layout workflow"
    2/1  notes/claude-code-karpathy-autoresearch-the-new-meta.md  [cl-1ed09284]  "Karpathy CLAUDE.md repo viral downloads agent instructions"
    2/1  notes/i-made-opus-4-8-and-fable-5-build-the-same-app-raw-results.md  [cl-9ae094cf]  "Fable pricing cheap discount week"
    2/1  notes/why-i-stopped-building-ai-agents-and-started-using-claude-cowork.md  [cl-6c468303]  "humanizer skill"
    2/1  notes/github-trending-today-43-kakehashi-qm-gander-simpleenglish-skill-recorder.md  [cl-b52741d9]  "humanizer skill"
    ...
  ```
  Full report: `$S/p6/answerability-report.txt`. 28 of the 37 losses are 2/1 (source "good", note "marginal"); 8 are 2/0; 1 is 3/1.
- **Unit test, zero compared pairs is `inconclusive`: PASS.** `eval::answerability::tests::zero_compared_pairs_is_inconclusive_not_a_loss_count_of_zero` (no-trace, expired and unreadable hits, zero judge calls; render says `INCONCLUSIVE`, never `0 of`), plus `every_judge_call_failing_is_inconclusive_too`. Bite: making `verdict()` return `Measured` at 0 compared fails both.
- **Unit test, a `work` session and a no-scope session contribute no queries: PASS.** `eval::mine::tests::a_work_session_and_a_session_with_no_scope_contribute_no_queries` (personal, work, absent scope, and an out-of-vocabulary `home` scope; only the personal query survives) and `the_scope_key_absent_from_the_export_json_drops_the_session` (the key missing from the JSON itself). Bites: mapping an unknown scope to personal fails both; mapping `work` to personal fails the first.
- Other new tests: `mine::tests` (dedup, subagent and staged-copy transcripts, only `knowledge_search` tool calls, the rendered set loads through `Queries::load` with stable ids, a clyde failure names the command), `answerability::tests` (one outcome per hit, the no-loss cases, distinct cache keys for note and source, the source cap), borg `eval::fixtures::tests::probe_staged_trace_*`, sb `cli::oracle::tests` (target parse, `--mine-queries` conflicts, `StagedSourceReader` mapping).
- `otto ci` -> exit 0, `[ci] ✅ All CI checks passed!` (first run failed only `cargo fmt --check`; `cargo fmt --all` fixed it).

### Design decisions
- Mining reads tool calls from the transcript files, not the export body (`oracle/src/eval/mine.rs:mine`): clyde's `--with-body` carries message text only, no `tool_use` inputs (checked on a session with known calls). The export supplies the session list, `scope`, `transcript-path`, `staged-path` and `project-dir`; the miner resolves live-then-staged and adds `subagents/*.jsonl`, the same layout rule as clyde's `transcript_layout_parts`.
- Scope filter (`ExportSession::scope_verdict`): `personal` only. `work`, absent and any other value drop the session before any file is opened, and the counts are printed. Never cwd or repo.
- `clyde session export --include-archived` with clyde's default lazy reindex (`mine::export_from_clyde`): archived sessions can still have a staged transcript, and a fresh catalog is the point.
- Dedup on lowercased, whitespace-collapsed text, first seen wins; ids are `mined-<fnv of the dedup key>` so a query keeps its id and its cached judgments across re-mines (`mine::render_yaml`). Tags from the original call are not carried: the query text is the question, and old calls may carry retired filters.
- Source port (`oracle::eval::answerability::SourceReader`, implemented in `sb` as `StagedSourceReader` over the new `borg::eval::probe_staged_trace`): oracle does not depend on borg, and the doc puts the cross-crate join in `sb`.
- `probe_staged_trace` (`borg/src/eval/fixtures.rs`) shares `load_staged_dir` with `load_staged_trace` and returns `Loaded | Absent | Unreadable(why)`. No directory -> `source-expired`; a directory with missing artifacts, an unscored kind, no recoverable source, or a parse error -> `source-unreadable`.
- Retrieval is the live configured pipeline (`answerability::retrieve` via `run_configured_pipeline`): the doc's "top hits" are what `knowledge_search` returns with no `mode`.
- The judged note is the full flattened body capped at 8,000 chars, not the summary the lift target judges (`answerability::retrieve`): the comparison is note vs source, and summary-only would manufacture losses for answers that live in Claims.
- The source is capped at 22,000 chars and the judge's fabric input budget raised to 24,000 (`answerability::fabric_judge`): a source cut at the lift target's 8,000 would hide losses.
- Note and source judgments share oracle's judgment cache. The source's key uses `source:<trace>` as its item and a hash over title + source text, so the two never alias (`source_and_note_judgments_are_cached_under_distinct_keys`). Both use the note's title, so the only variable is the body.
- `--target lift | answerability` (`sb/src/cli/oracle.rs:EvalTarget`, case-insensitive, default `lift`) instead of a new subcommand. `--mine-queries` conflicts with `--target`, `--emit-calibration`, `--report`, `--rebuild-cache`.

### Deviations
- The live answerability run used `--k 3`, not the default 10. At K=10, 237 queries would mean roughly 3,000+ fabric calls; K=3 was 1,158 calls and 23.5 minutes. The criterion's outputs do not depend on K.
- A fifth outcome, `judge-error`: a hit with a readable source whose note or source judge call failed. The doc's four outcomes leave no slot for it, and counting it as `compared` or as a source outcome would lie. It is printed and not scored.
- "Eligible" is every retrieved `(query, hit)` pair, the sum of the outcomes. The doc does not define it.
- Debug binary and a scratch `--config`, not `~/.cargo/bin/sb` with the deployed `oracle.yml` (no `llm` block; Phase 1's open question).
- `load_staged_trace` is unchanged; the read path is its new sibling `probe_staged_trace` over the same reader, because answerability must tell swept from unreadable and `load_staged_trace` folds both into `None`. Same reader, correct seam.

### Tradeoffs
- Reading transcripts directly vs adding a tool-call export to clyde: clyde is another repo, and the export already names the files.
- One judge with a 24,000-char budget for note and source vs two judges: the note is capped at 8,000 before it reaches the judge, so one budget serves both.
- Sequential judging vs workers: matches the lift target and its single-connection judgment cache; the cost is the 23-minute run.

### Open questions
- 297 of 579 compared sources hit the 22,000-char cap, so long sources are judged on their head only, which can hide a loss whose answer sits late in the source. Raise the cap, or chunk the source?
- 28 of the 37 losses are 2/1. Is "good source, marginal note" the loss Scott cares about, or should the bar be source 3 / note <= 1 (1 pair here)?
- 71 of 711 hits carry no `trace` (pre-staging or non-borg notes) and can never be compared. Should "eligible" exclude them?

### Background processes
None left running.

## Phase 7: Remediation with lineage

Ran on ripr 2026-10-17 on top of a44e1f3 (Phases 1-6 plus the two off-plan fixes c3534be and a44e1f3). Debug build only (`cargo build -p sb`). No `otto install`, `otto deploy`, `sb bootstrap`, `systemctl`, and no `--apply` against anything live. The live dry run used `XDG_DATA_HOME=$S/p7/xdg` (`$S` = the session scratchpad) holding a `sqlite3 .backup` copy of the deployed `~/.local/share/sb/borg/receipts.db` plus symlinks to the deployed `eval-cache.db` and `stages/`, so the debug binary's receipts v6 migration landed on the copy only. Afterwards the deployed `receipts.db` still reads `schema_version` 5, no `replay_origin` column, 0 `replay_of` rows.

### Observed criterion output
- **Unit test: two concurrent `--remediate` runs on the same failing source post exactly once: PASS.** `eval::remediate::tests::two_concurrent_remediate_runs_on_the_same_failing_source_post_exactly_once`: 12 rounds, each on fresh file DBs; two threads open their own receipts and score connections, plan, meet at a `Barrier`, then run `remediate::run` with `--apply` semantics against one shared counting poster. Every round: 1 post, one `Posted`, one `AlreadyAttempted { prior: 1 }`, 1 claim row. Bites (break made, test failed, restored):
  - claim skipping the count (`if false && prior > 0`): the concurrency test and 3 sequential tests fail (the second claim hits the `source_url` primary key).
  - `TransactionBehavior::Deferred` instead of `Immediate`: the concurrency test fails 3 of 3 runs (`database is locked` on the racing upgrade from SHARED).
  - Siblings: `a_remediated_source_that_fails_again_is_never_posted_a_second_time` (the fail -> remediate -> fail loop), `an_auto_remediation_intake_row_with_no_claim_still_counts_as_the_attempt` (an intake row with `replay_origin = 'auto-remediation'` counts; a lineage row with no origin does not), `a_post_the_daemon_never_got_releases_the_claim_and_an_unknown_one_keeps_it`.
- **Unit test: a failure scored only by an untrusted judge check triggers no remediation, and the listing names the judge, check and class: PASS.** `a_failure_only_an_untrusted_judge_reports_triggers_nothing_and_names_judge_check_and_class` (the measured Gemini `judge-claims` trust row: paired 0.58 over 26 number swaps): outcome `Blocked`, 0 posts, 0 claim rows, listing line `gate: judge gemini-2.5-pro check judge-claims class number-swap untrusted (n 26 < min-samples 30; paired catch 0.58 < 0.80)`. Plus `a_never_measured_judge_is_untrusted_too` (no trust row) and `a_trusted_judge_failure_triggers_a_remediation` (0.80 over 30 posts). Bite: `trigger()` accepting any failure: both untrusted tests fail.
- **Unit test: a deterministic-check failure triggers a remediation even when no judge check is trusted: PASS.** `a_deterministic_check_failure_triggers_a_remediation_even_when_no_judge_check_is_trusted`: quote grounding 0 of 1 plus a failing untrusted `judge-claims`, no `judge_trust` rows at all: 1 post, the trigger is `quote-grounding` (`judge_model ''`), the claim row freezes that row's key (check, rubric, the trace's current distill hash), and the body carries `replay-of`, `replay-origin: auto-remediation`, the original `method`, `force: true`. Bite: `trigger()` acting only on `Trusted` (deterministic gated like a judge): 8 tests fail.
- **Lineage end to end (in-process daemon): PASS.** `an_applied_remediation_lands_lineage_on_the_daemons_receipts_row` drives `remediate::run` through the production `build_router` `/ingest` route against a sandboxed `XDG_DATA_HOME` receipts DB: the daemon's new row carries `replay_of = tr-failing`, `replay_origin = auto-remediation`, the original method, and `select count(*) from receipts where replay_of is not null` is 1; the claim names the queued trace. `server::tests::ingest_writes_replay_of_at_intake_and_refuses_an_origin_without_it` covers the route alone (and that `replay-origin` without `replay-of` is refused before intake, no row). Bite: intake passing no lineage (`lineage.as_ref().filter(|_| false)`): both fail.
- **`select count(*) from receipts where replay_of is not null` > 0 after one `--remediate --apply`: POST-DEPLOY.** The running daemon is the installed v0.17.7, which drops `replay-of`; `DaemonPoster::connect` refuses it: `curl -s http://127.0.0.1:8181/health` -> `{"status":"ok","service":"obsidian-borg","version":"v0.17.7"}`, no `features`. `--apply` was not run against it, so the refusal is shown by `health::tests::health_advertises_replay_of_and_an_older_body_supports_nothing` on that body shape, not live. After `otto deploy` (the daemon restarted on this build), on ripr:
  ```
  sb borg eval --remediate                 # dry run: expect the two would-post entries below, or their successors
  sb borg eval --remediate --apply
  sqlite3 ~/.local/share/sb/borg/receipts.db "select count(*) from receipts where replay_of is not null"
  sqlite3 ~/.local/share/sb/borg/receipts.db "select trace_id, replay_of, replay_origin, status from receipts where replay_origin = 'auto-remediation'"
  sqlite3 ~/.local/share/sb/borg/receipts.db "select source_url, original_trace, remediation_trace, trigger_check, post_error from remediations"
  sb borg eval --remediate                 # again: each posted source now reads "already had its one remediation"
  ```
- **Live dry run against the deployed `eval-cache.db`:** `XDG_DATA_HOME=$S/p7/xdg ./target/debug/sb borg eval --remediate` -> exit 0, 1.8 s. Summary, verbatim:
  ```
  remediation (dry run; --apply posts)  scored traces 33  failing 23  actionable 2
  not read: 0 swept from staging, 0 with no succeeded receipt here, 0 unreadable
  ...
  posted 0  would-post 2  blocked (untrusted judge only) 21  already attempted 0  not remediable 0  not delivered 0  unknown 0
  ```
  The two it would post, both on deterministic failures (every judge check is untrusted, as expected):
  ```
  cl-0d9fcb5b  https://www.youtube.com/watch?v=hJzqEAf2U4I  [url via cli]
    FAIL tools-grounding (deterministic, ungated): 1 of 3 tool names, 0 of 0 commands grounded
    -> would post {"force":true,"method":"cli","replay-of":"cl-0d9fcb5b","replay-origin":"auto-remediation","tags":[],"url":"https://www.youtube.com/watch?v=hJzqEAf2U4I"}

  ht-47d1c203  https://www.youtube.com/watch?v=miBG-a3FuhU  [url via http]
    FAIL quote-grounding (deterministic, ungated): 0 of 1 quotes grounded in the source
    FAIL judge-claims claude-sonnet-5 (untrusted judge: recorded, never acts): 4 of 9 claims not supported
         gate: judge claude-sonnet-5 check judge-claims class number-swap untrusted (n 26 < min-samples 30; paired catch 0.69 < 0.80)
    ...
    -> would post {"force":true,"method":"http","replay-of":"ht-47d1c203","replay-origin":"auto-remediation","tags":[],"url":"https://www.youtube.com/watch?v=miBG-a3FuhU"}
  ```
  A blocked one:
  ```
  cl-46bb93f6  https://dev.to/kasuboski/talos-cluster-3pi6  [url via cli]
    FAIL judge-claims gemini-2.5-pro (untrusted judge: recorded, never acts): 1 of 9 claims not supported
         gate: judge gemini-2.5-pro check judge-claims class number-swap untrusted (n 26 < min-samples 30; paired catch 0.58 < 0.80)
    -> not posted: only an untrusted judge check fails it
  ```
  The scratch receipts copy holds 0 `remediations` rows after the dry run.
- `otto ci` -> exit 0, `[ci] ✅ All CI checks passed!` (the first run failed `source-lint` on a "Phase 3" comment in a test, the second `cargo fmt --check`; both fixed).

### Design decisions
- Remediation method: the re-POST carries the failing trace's ORIGINAL receipts `method` (as `sb borg replay` does), not `cli`. A URL reingest re-renders `method:` frontmatter, so `cli` would rewrite how the note was captured. Notifications are suppressed by origin instead: `dispatch::pings_operator(method, replay_origin)` is false for `ReplayOrigin::AutoRemediation` whatever the method, and `routes::ingest` applies it before handing its sinks to `dispatch_ingest`. Lineage does not depend on the method: `replay-of` and `replay-origin` ride the body, and the attempt count reads `replay_origin`, never `method`, so no method choice can dodge it. The remediation still joins `/queue` batches and the `--unscored` backlog like any ingest.
- Where the claim lives: a `remediations` table in `receipts.db` (receipts schema v6, beside a new `receipts.replay_origin` column). The doc's single transaction has to count intake rows AND insert the marker, so both must sit in one SQLite file; the receipts DB is the one holding intake rows. `source_url` is the primary key, a second guard under the count.
- The claim (`remediate::claim`): `BEGIN IMMEDIATE`; `prior_attempts` = claims for the URL + `replay_origin = 'auto-remediation'` intake rows no claim accounts for; insert only at 0. The trigger row's key (`judge_model`, `check`, `rubric`, `content_hash`; `run_kind` baseline, `run_id ''`) is frozen on the claim as the "before" score for Phase 9.
- After the POST: `Queued` with a trace -> the claim records `remediation_trace`. A POST that provably never reached intake (connect refused, 401, a 4xx, or a `Failed` answer, which the route returns only when it refused or failed to record intake) releases the claim, because no attempt exists. Anything ambiguous (timeout, 5xx, unreadable 200, no trace id) keeps the claim with `post_error`, because an intake row may exist. Either failure makes the run exit non-zero after printing.
- Failure rule (`remediate::row_failure`): the two judge checks through `agreement::trace_flag` (the `inject::flags` rule over a stored row); the deterministic checks as the text report reads them: quote grounding `grounded < quoted`, tools grounding any name or command ungrounded, any placeholder reason, listicle survival `< 1.0`, note size over the ceiling. `{"applicable":false}` passes; an unreadable current row is an error, never a pass.
- Only current rows act: `baseline`, `run_id ''`, at the trace's current distill hash (recomputed from staging), error-free, and in the required set (`eval.judge-models`). Tool rows (deepeval, minicheck), other judges, error rows and stale hashes never trigger (`error_rows_other_judges_and_stale_hashes_never_trigger`). Deterministic failures sort first, so the trigger is the ungated one when there is one.
- Scan scope: traces with an error-free baseline row (`ScoreStore::latest_scored_traces`), not every receipt; a trace with no scores has no failure to read.
- Capture note: the body carries the note `replay::capture_note_for` resolves (envelope, else the published note's `capture-note:`), the same rule `sb borg replay` uses, so the re-distill sees the stated intent.
- Fail closed on an old daemon (`DaemonPoster::connect`): `--apply` first reads `/health`, which now lists `features: ["replay-of"]` (`health::DAEMON_FEATURES`); a daemon without it would queue the ingest and silently drop the lineage, so the run stops before any claim.
- `/ingest` validation (`routes::replay_lineage`): `replay-origin` without `replay-of`, or a blank `replay-of`, is refused before intake with `Failed` + `intake-rejected` and no row, like a 401. An unknown `replay-origin` value is a JSON parse error.
- `record_replay` is `#[cfg(test)]` now, per the doc ("stays for tests only"); production lineage is `receipts::record_received_with_lineage`, reached through `intake::record_replay_with_sidecar`.
- `sb borg log --trace` prints `replay_origin` beside `replay_of`.

### Deviations
- The doc's `origin = auto-remediation` is the receipts column `replay_origin` and the body field `replay-origin`, not a column named `origin`. `origin:` is already the note frontmatter facet (`vault::schema::Origin`: authored | assisted | generated); a receipts `origin` meaning something else would be two names for different things.
- The doc names one body field (`replay-of`). A second additive optional field, `replay-origin`, carries `auto-remediation`, because the count rule needs the origin and `replay-of` alone cannot tell a remediation from a manual replay.
- `/health` gained an additive `features` list. Not in the doc; it is what lets `--apply` refuse the v0.17.7 daemon that is running now instead of posting a lineage-less remediation.
- The claim is released when the POST provably never reached intake. The doc says an attempt counts once its intake row exists; a refused or unconnected POST has none, so holding its claim would block the source forever with no attempt made.
- The listing groups a judge check's untrusted reasons per class on one line (`class number-swap untrusted (n 26 < min-samples 30; paired catch 0.58 < 0.80)`).

### Tradeoffs
- Original method plus origin-gated pings vs `method: cli` (the a44e1f3 precedent): `cli` needs no dispatch change, but rewrites the remediated note's `method:` and the trace prefix to claim a CLI capture that never happened.
- Claim table in `receipts.db` vs `eval-cache.db`: the eval cache is where the trigger row lives, but the count must see intake rows inside the same transaction. Cost: an eval-side table in the daemon's DB (additive; the v0.17.7 daemon's migrations ignore it).
- A `/health` capability check vs echoing `replay-of` in the `IngestResult`: the echo would detect an old daemon only after it had already queued one unlineaged remediation.
- No per-run cap on posts: today's dry run finds 2 actionable of 33 scored. If the deterministic failure rate holds as the 100-a-day timer covers the backlog, that is a few a day; a cap is deferred until it is a problem.

### Open questions
- Nothing runs `--remediate --apply` unattended yet. `sb-eval.service` runs `--unscored` only, and no phase of the plan wires remediation into the timer, but the Goals say "a failing note gets one automatic re-distill". Add `ExecStart=sb borg eval --remediate --apply` after the `--unscored` line (a oneshot unit runs them in order), and in which phase?
- Is a `tools-grounding` failure (1 of 3 tool names grounded on `cl-0d9fcb5b`) worth a full re-fetch and re-distill? The doc makes every deterministic check actionable; listicle survival and note size are in the same position. Narrowing the actionable set would be a doc change.
- `--remediate` must run on the daemon host: the claim lives in that host's `receipts.db`. Fine on ripr, where both the timer and the daemon run.

### Background processes
None left running.

## Phase 8: GEPA candidates and the promptfoo check

Ran on ripr 2026-10-17 on top of 414f929. Debug build only. Live runs used a snapshot of `./target/debug/sb` (`$S/p8/sb`, `$S` = the session scratchpad) with `--config $S/p8/borg.yml`, a byte copy of the deployed `~/.config/sb/borg.yml`; the deployed file was not edited. Keys were already in the shell env (`ESCOTE_ANTHROPIC_API_KEY`, `GEMINI_API_KEY`, checked by presence only). No `otto install`, `otto deploy`, `sb bootstrap`, `systemctl`. The real `eval-cache.db` was written (additive: the `candidate_distills` table with 50 distills, and 378 `candidate` rows over 17 run ids).

Disk: the first `otto ci` died with `mold: failed to write to an output file. Disk full?`. `/` was at 100% (791M free) because `target/debug` held 419G (368G of it stale `deps/`). I ran `cargo clean --profile dev` (regenerable build cache, 427.4GiB removed, `target/release` untouched); `/` went to 52%.

### Observed criterion output
- **One GEPA run writes a candidate file with before/after metrics on two disjoint samples: FAIL (no run produced a winner, so no file was written).** The search, scoring, rows and file writer all ran live; the file is only written for a winner, and none of 14 live rewrites beat the current pattern. Sample sizes: 3 train + 3 holdout (`--candidate-split 3 --pattern distill-video` -> `eligible 1781`, train `ht-47d1c203 cl-0d9fcb5b ht-45c6a8ab`, holdout `cl-fdb02a2d cl-d642cec2 ht-6447ad43`). Four live runs, `SB=$S/p8/sb SB_BORG_CONFIG=$S/p8/borg.yml uv run --directory eval python -m propose --pattern distill-video --sample 3 --budget N --work-dir ... --out $S/p8/candidates`:
  1. budget 15, 17:09-17:14Z (332s): every reflection failed with `AttributeError: 'PatternAdapter' object has no attribute 'propose_new_texts'`. GEPA swallows a failed reflection and carries on, so the run exited 0 printing "no candidate beat". Fixed: `PatternAdapter.propose_new_texts = None` (GEPA reads the attribute to choose the custom proposer); `test_gepa_runs_the_custom_proposer_through_the_adapter_end_to_end` runs a real `gepa.optimize` over a fake sb and fails without it; `__main__` now exits 1 when no rewrite was ever proposed (`FabricProposer.proposed`).
  2. budget 15, 17:15-17:23Z (509s): 2 rewrites proposed, minibatch sums 1.70 and 1.50 vs the seed's 2.089. No winner.
  3. budget 45, 17:24-17:54Z (1807s): 7 rewrites, sums 1.5, 1.2, 1.3, 1.37, 1.7, 1.2, 0.67 vs 2.089. Hand-scoring one rejected rewrite showed the per-trace floor was the cause: one note went 10 -> 8 claims and scored 0 while the sample average fell only 9.67 -> 9.00 (-6.9%), and the sample objective said `candidate score 0.7037` vs `baseline score 0.6897`, "the candidate beats the baseline". The doc's floor is "claims per note" over the sample, so the per-trace zero was stricter than the doc. Fixed: a trace's search score is its factor product, and the floor is applied over the sample (a breach zeroes every trace); a note-level cut is feedback (`this note: ...`). Test `one_note_losing_claims_is_feedback_when_the_sample_average_holds_the_floor` (bites when per-note zeroing is restored).
  4. budget 45, 17:57-18:32Z (2057s): 7 rewrites, sums 0.0, 0.0, 0.0, 0.0, 2.0, 1.95, 1.975 vs 2.089. Output: `metric calls: 45  rewrites proposed: 7`, `none of 7 rewrites beat the current pattern on the train sample; nothing written`, exit 0.
  - Root cause of the exact 0.0 scores, by hand through the same path (`$S/p8/sb borg --config $S/p8/borg.yml eval --candidate-pattern $S/p8/work4/distill-video-13f21fc3adf9.md --pattern distill-video --traces ht-47d1c203 cl-0d9fcb5b ht-45c6a8ab --patterns-dir borg/patterns`, exit 0, all cache hits):
    ```
    ht-47d1c203          0.500     0.000        10         7         4         4
        floor: claims per note 10.00 -> 7.00 (-30%) cuts more than 10%
    cl-0d9fcb5b          0.889     0.000         9         7         0         0
    ht-45c6a8ab          0.700     0.000        10         7         0         0
    baseline  score 0.6897 = quote grounding 1.000 x claim support 0.690 x listicle survival 1.000; claims/note 9.67, enumeration items/note 1.33
    candidate score 0.0000 = quote grounding 1.000 x claim support 0.667 x listicle survival 1.000; claims/note 7.00, enumeration items/note 1.33; FLOOR: claims per note 9.67 -> 7.00 (-28%) cuts more than 10%
    ```
    The coverage floor zeroes it, as designed. The rewrites react to `judge-claims` flags by tightening the claims rules (`diff` of the four zeroed rewrites against the seed: "do not extract multiple claims that state the same core idea", "a claim is an argument or a key takeaway, not a description of events", "Narration: do not report what the speaker is doing"), and the distiller then writes 7 claims per note instead of ~10. The non-zero rewrites (run 4's last three) kept the count and still lost on claim support. So the scorer is doing what the doc asks; the search found no rewrite that improves claim support without cutting claims, on a 3-trace sample.
  - Doc vs code on "no winner": the doc's Output line ("`<report-dir>/candidates/<pattern>-<date>.md` with the diff and the before/after metrics on both samples") describes the winner's file, and the criterion assumes a winner exists. The code writes nothing when the best candidate is the current pattern (`CandidateFile::build` refuses a candidate equal to its baseline: there is no diff). I read that as a doc defect (the criterion is unconditional, the output contract is not), not a code gap; see Open questions.
- **Unit test: a candidate with 20% fewer claims per note scores 0: PASS.** `eval::candidate::tests::a_candidate_with_20_percent_fewer_claims_per_note_scores_zero` (5 -> 4 claims per note, every factor 1.0, score 0.0, breach `claims per note 5.00 -> 4.00 (-20%)`; the same notes uncut score 1.0). Siblings: `a_cut_of_exactly_the_allowed_share_is_not_a_breach` (10 -> 9), `a_candidate_with_fewer_enumeration_items_scores_zero_too`, `dropping_every_quote_cannot_win`, `a_candidate_that_cuts_claims_loses_on_the_floor_and_says_so` (end to end through `score`). Bite: `if floor_breaches.is_empty() || true` fails 3 of them.
- **`otto ci` runs the promptfoo task when `borg/patterns/distill-video.md` differs from `origin/main`, and skips it otherwise: PASS.**
  - Unchanged tree: `otto ci` -> exit 0, `[promptfoo] promptfoo: skipped: no borg/patterns/*.md differs from origin/main`, `[ci] ✅ All CI checks passed!`.
  - With one line added to `distill-video.md` (`Keep each claim to one sentence.` under `# OUTPUT`, reverted afterwards with `git checkout`): `otto ci` 18:32-19:09Z, `[promptfoo] promptfoo: running: differs from origin/main: borg/patterns/distill-video.md`, `promptfoo: 36 regenerated notes`, then promptfoo `✓ 10 passed (27.78%)  ✗ 26 failed (72.22%)  0 errors`, `Duration: 13m 2s`, 945,969 grading tokens; the task took 37m23s and failed (so did `check`, on `cargo fmt` only).
  - Most failure reasons read like "the source transcript ends at [00:03:07], making it impossible to verify the majority of the claims". Cause: the tests' `context` was the record's `source`, which is cut to the judge budget (24,000 chars minus the note), while the note was distilled from the whole fixture source (`video/game-over-neovim/source.md` is 95,635 chars). Fixed: `bin/promptfoo-check` now reads each fixture's full `config/eval/distill-fixtures/<fixture>/source.md` as `context` (checked on that fixture: context length 95,635). The pass rate after the fix is UNVERIFIED: I did not repeat the 37-minute, ~1M-token grading.
  - Unit tests, `eval/tests/test_promptfoo_check.py` against throwaway git repos with an `origin/main` ref: unchanged skips; an edited `distill-video.md` runs; a committed change ahead of origin runs; a new untracked pattern runs; a change outside `borg/patterns` skips; a missing base exits 2. Bites: pathspec pointed at `borg/patterns/none/*.md` fails 2; forcing the skip branch fails 3.
- **`--apply-candidate` refuses an untrusted judge (the doc's "No untrusted judge triggers ... an applied candidate"): PASS by unit test.** `eval::candidate::file::tests::apply_refuses_an_untrusted_claim_judge_naming_it_and_touches_nothing` uses the measured Gemini `judge-claims` trust row (paired 0.58, n 26): the error names `claim-support`, `judge-claims`, `gemini-2.5-pro`, `number-swap: n 26 < min-samples 30`, `paired catch 0.58 < 0.80`; zero distills run; `borg/patterns/distill-article.md` unchanged. Also `apply_refuses_a_claim_judge_that_was_never_measured`, and the trusted path `a_trusted_claim_judge_and_a_confirmed_holdout_write_the_pattern` (0.80 over 30: fresh holdout re-distill of both arms, then the file is written). Bite: `let _ = gate(...)` fails both refusal tests. Not run live: no candidate file exists to apply.
- `candidate` rows in `eval_scores`, live: `select count(distinct run_id), count(*) from eval_scores where run_kind='candidate'` -> `17|378`; the seed's run id `distill-video:154445582a1bb83f` has rows for all seven checks (five deterministic, two `gemini-2.5-pro`).
- `otto ci` (final, unchanged tree) -> exit 0, `[ci] ✅ All CI checks passed!`, `[promptfoo] promptfoo: skipped: ...`, `[eval-test] 15 passed`.

### Removed from `eval/` (Phase 5's drop verdicts)
- DeepEval and MiniCheck (`eval/verdict/`, its tests), `torch`, `accelerate`, the `pytorch-cpu` uv index, `bin/eval-nltk`, and the `otto eval-py` task. `uv remove` dropped the lock to `gepa` 0.1.4 (zero dependencies) plus pytest.
- `--ingest-verdicts` (`borg/src/eval/verdicts.rs`) and `--scored` (`borg/src/eval/scored.rs`): nothing produces verdicts any more, and `--scored` existed only to feed the tools. Both flags now fail to parse (`the_dropped_tool_flags_scored_and_ingest_verdicts_are_gone`).
- The DeepEval/MiniCheck rater arms in `eval/agreement.rs`. Their rows (60 in the real db) remain; a row under an unknown check is not a rater (`is_current_rater_row` requires a known rubric). Inverted by name: `the_report_prints_pairwise_kappa_between_both_judges_and_never_a_dropped_tool` (was `..._and_any_ingested_tool`), and `trace_flags_follow_each_raters_rule` now asserts the tool rows have no flag and no claim label.
- `eval/` itself stays: it is the GEPA search (`propose/`) and the promptfoo project (`promptfoo/`). `eval/AGENTS.md` and the root `CLAUDE.md` entry were rewritten. `~/.cache/sb/eval/{models,nltk}` (MiniCheck weights, punkt) are left on disk; nothing reads them now.
- `otto eval-test` (pytest, now light) joined `ci`.

### Design decisions
- One objective, in Rust (`borg/src/eval/candidate.rs:objective`): quote grounding x claim support x listicle survival; claims per note and enumeration items per note may each fall at most `eval.candidate.max-coverage-cut` (0.10) below the baseline. Quote grounding with no quote scores 1 only when the baseline quoted nothing either; claim support with nothing judged is 0; listicle survival with no declared count is 1.
- The search proposes, sb scores (`eval/propose/` + `sb borg eval --candidate-pattern`): GEPA (the `gepa` library) calls sb for every evaluation, so the floor, the rows and the candidate file have one implementation. Per-trace search scores are factor products zeroed by the sample floor, so GEPA's minibatch sum sees the same floor as the sample objective.
- Pattern swap at the fabric seam (`candidate.rs:CandidateFabric`): replaces only the named pattern with a file and resolves every other pattern as `--patterns-dir` does; it counts the swaps, and `FabricPatternDistiller` fails a distill that never called the pattern. `--candidate-split` only takes traces that reach the single-call path (`approx_tokens(len) <= SINGLE_CALL_TOKEN_THRESHOLD`, repo always), so a long video never "scores" a short-path pattern.
- Distill cache (`candidate.rs:DistillCache`, table `candidate_distills` in `eval-cache.db`, key run id + trace + source hash + distill model): the seed is scored on every GEPA call; without it each call re-distills the baseline. Fallback distills are not cached. A candidate identical to the baseline is distilled once (`a_candidate_identical_to_the_baseline_is_distilled_once`; before the fix the seed had two distill hashes per trace).
- `run_id` (`candidate::run_id`): pattern name plus the hash of the pattern TEXT (the doc says "the candidate file's hash"; the pattern text is what the run measured, and the candidate .md does not exist yet during the search).
- Candidate file (`candidate/file.rs:CandidateFile`): a YAML frontmatter record (`deny_unknown_fields`: pattern, both run ids, base hash, candidate hash, judge, distiller, both samples' objectives and trace lists, trust at write, full pattern text) over the human page (metrics table, trust per factor, apply line, status, diff in a four-backtick fence because the patterns carry ``` fences). Never overwritten (`-2`, `-3` suffixes). `parse` refuses a pattern text that no longer hashes to `candidate-hash`.
- `--apply-candidate` order (`file.rs:apply`): trust gate (before keys, before any distill), base-hash check, holdout traces still staged, fresh holdout re-distill must beat the baseline with the floor intact, then an atomic write into `borg/patterns/<P>.md` (or `--patterns-dir`).
- Reflection (`eval/propose/reflect.py:FabricProposer`): GEPA's `custom_candidate_proposer`, one `fabric -m gemini-2.5-pro` call, the pattern returned between `<pattern>` tags (GEPA's default extractor takes first-to-last ``` and the patterns contain ```yaml blocks).
- promptfoo (`bin/promptfoo-check` + `eval/promptfoo/promptfoo.yml`): `git diff --name-only origin/main -- 'borg/patterns/*.md'` plus untracked patterns decides; when it runs: `cargo build -p sb`, `target/debug/sb borg eval --regenerate --patterns-dir borg/patterns --no-judge --format json`, one promptfoo test per regenerated note (`echo` provider, `llm-rubric` + `context-faithfulness` graded by `google:gemini-2.5-pro`, context = the fixture's full source). promptfoo 0.124.1 from `npm install --save-exact` (`npm view promptfoo version` -> `0.124.1`), lockfile committed, `node_modules` (1.4G) gitignored and installed by `npm ci` on first run. promptfoo state goes to `~/.cache/sb/eval/promptfoo`, telemetry off.
- `--regenerate --format json` (`regenerate.rs:RegenerateReport::records`): both arms' records as `run_kind candidate`, `run_id regenerate:without-note|with-note`.
- Config (`eval.candidate.{max-coverage-cut, distill-workers}`, defaults 0.10 and 4): validated at load, annotated in the template. Reports dir `vault::paths::borg_eval_reports_dir()` (`~/.local/share/sb/borg/eval-reports`), candidates under `candidates/`.

### Deviations
- A GEPA run that finds nothing better writes no candidate file (no diff exists), while the criterion expects a file from every run. See Open questions.
- GEPA runs while every judge check is untrusted. The doc says "GEPA and `--apply-candidate` refuse to run when any factor of the metric comes from an untrusted check"; the Resolved Decision names only `--apply-candidate`, and this phase's criterion needs a live GEPA run. The search records (scores are always recorded) and the file states each factor's trust and `BLOCKED`; `--apply-candidate` is the act, and it refuses. The objective is never silently changed.
- `gepa` (the library `dspy.GEPA` wraps) instead of DSPy: the program under optimization is sb's distill pipeline, not a DSPy module, and `gepa.optimize` with a custom adapter is the seam for an external system. `gepa` has no dependencies; DSPy would pull litellm and friends for nothing.
- `--apply-candidate` confirms on the holdout BEFORE writing (the doc says write, then re-run to confirm), so a failed confirmation leaves the repo untouched. The confirm is a fresh distill of `--candidate-pattern` over the staged holdout, not `--regenerate` (which re-distills the golden fixtures): same effect, the sample the candidate was measured on.
- The promptfoo check regenerates every capture-note fixture (18 x 2 arms), not only the kinds whose pattern changed, and gates on promptfoo's own exit (any failing note fails the task).
- `Eval(Box<EvalArgs>)`: clippy `large_enum_variant` after the new flags.
- Design doc amended in place (untracked, not committed): the coverage floor in "Prompt improvement" and in Phase 8 is claims per note and enumeration items per note; "DeepEval coverage" left with DeepEval.

### Tradeoffs
- Floor on the sample average vs on each note: the doc's measure is "claims per note", an average; per-note zeroing rejected a rewrite that won on the sample (run 3). Cost: one note can lose claims inside a passing sample, which the feedback names.
- Sample of 3 + 3 vs larger: 3 kept one budget-45 run near 30 minutes (judging is sequential inside `evaluate`). It is small for a 10% floor and a claim-support rate, so a winner on 3 traces would still be weak evidence.
- Reusing `evaluate` (both judges) for candidate grading vs a claims-only path: one code path and complete `candidate` rows, at one wasted `judge-distillation` call per note.
- An absolute promptfoo gate vs a comparison against `origin/main`'s patterns: absolute is what the doc describes and costs one regenerate; a comparison would double it.

### Open questions
- No winner, no file: should a no-winner GEPA run write a record anyway (the best rejected rewrite with its train/holdout metrics, status BLOCKED), or is the criterion amended to "writes a candidate file when a rewrite beats the pattern"? I read the doc as defective here and did not build the record.
- Every rewrite that lifted claim support did it by cutting claims (4 of run 4's 7 were zeroed at 7 claims per note). Should the reflection prompt carry each note's claim count and the floor in numbers, or is a larger sample the next step? Not run: the orchestrator stopped further live runs.
- promptfoo gate: 26 of 36 regenerated notes failed before the full-source fix (most of them on the truncation). After the fix the rate is unknown. Is "any failing note fails `otto ci`" the intended bar, or a pass-rate floor (promptfoo's `PROMPTFOO_PASS_RATE_THRESHOLD`), or a comparison against `origin/main`'s patterns? Note an untrusted judge (llm-rubric on Gemini) now gates `otto ci` whenever a pattern changes.
- GEPA vs the trust rule (Deviations): should `python -m propose` refuse while `judge-claims` is untrusted, as the doc's architecture bullet says?
- The doc still describes the `uv` verdict contract, `--ingest-verdicts`, `otto eval-py` and DeepEval/MiniCheck in the Overview diagram, "The `uv` project" and API Design. Amend those sections too?
- `~/.cache/sb/eval/models` and `nltk` (MiniCheck weights, punkt) are orphaned on ripr; delete with `rkvr rmrf`?

### Background processes
None left running.

## Phase 9: Weekly failure clusters

Ran on ripr 2026-10-17 on top of 22baefe. Live checks used `./target/debug/sb` with `--config <scratch borg.yml>` (the Phase 8 scratch config) and `BORG_DISABLE_NOTIFICATIONS=1`. No `otto deploy`, `otto install`, `sb bootstrap` or `systemctl` was run, and no Telegram message was sent.

### Observed criterion output
- **One run writes `eval-reports/<date>.md` with a category, a count, example traces, a Remediated section and each judge's trust state: PASS (debug binary), POST-DEPLOY for the timer.**
  - `BORG_DISABLE_NOTIFICATIONS=1 ./target/debug/sb borg --config $S/p9/borg.yml eval --weekly --patterns-dir borg/patterns --report $S/p9/report-copy.md` -> exit 0, one fabric call (the log stamps 30 s between config load and the notify line). The command writes the real path by design: `~/.local/share/sb/borg/eval-reports/2026-10-17.md` (2227 bytes, additive; the dir did not exist before).
  - Report body: `scored traces: 33`, `failing traces: 23 (2 actionable, 21 flagged only by untrusted judge checks)`, `scored traces no longer in staging: 0; with no succeeded receipt here: 0; unreadable: 0`.
  - Failure categories: `1. Poor Summary (21 traces)` examples `cl-4598b194, cl-74cea19d, cl-8255b4af`; `2. Unsupported Claims (15 traces)` examples `cl-46bb93f6, cl-74cea19d, cl-8255b4af`; `3. Grounding Failure (2 traces)` examples `cl-0d9fcb5b, ht-47d1c203`. A trace can sit in two categories, so the counts do not sum to 23.
  - Judge trust, all four judge checks `UNTRUSTED`, each with why: gemini `judge-distillation` (number-swap n 26 < 30 and paired catch 0.27; fabricated-quote 0.50; dropped-top-claim n 28 and 0.18; who-change n 17 and 0.65), gemini `judge-claims` (number-swap n 26, paired 0.58), claude-sonnet-5 `judge-distillation` (0.27 | 0.50 | 0.07 | 0.53) and `judge-claims` (paired 0.69). These are the Phase 3 and 5 `judge_trust` rows, read back.
  - `## Remediated` -> `No source has been remediated.` (the `remediations` table is empty; the debug binary's receipts open created it on the real receipts.db, additive). `## Candidates` -> `No measured candidate patterns.`
  - Post-deploy: the timer's first fire writes the same file; the live Telegram send is unproven (see Open questions).
- **`sb doctor` prints the eval line: PASS.** `./target/debug/sb doctor`:
  - `[borg] eval: 33 traces scored, 23 failing (2 actionable), 0 of 4 judge checks trusted, 0 remediated (0 still failing), last weekly report 2026-10-17`
  - Warn: `[borg] eval: 4 of 4 judge checks are untrusted, so no remediation or candidate acts on them: judge-distillation (gemini-2.5-pro), judge-claims (gemini-2.5-pro), judge-distillation (claude-sonnet-5), judge-claims (claude-sonnet-5)` with fix `sb borg eval --inject re-measures them ...`.
  - doctor exits 1 in this run only because of `[patterns] 1 of 30 patterns missing` (`eval-cluster-failures.md` is in the binary's pattern list and not yet in `~/.config/sb/patterns/`). `otto deploy` runs `sb bootstrap --force`, which writes it. That Error clears post-deploy, and any new pattern raises it.
- **Tests bite.** Each mutation compiled and failed the named tests, then was reverted:
  - skip the `Unclustered` category -> `a_failing_trace_the_model_left_out_lands_in_unclustered` and `untrusted_only_failures_are_counted_separately_...`.
  - disable the `announce_unless` gate -> `announce_sends_nothing_while_notifications_are_disabled` and `the_process_gate_is_closed_under_test_...`.
  - `>` to `>=` on the stale-report check -> `a_missing_report_is_a_warn_and_a_stale_one_is_a_warn_at_the_boundary_only`.
  - `failed` receipts no longer `attempt-failed` -> `outcome_attempt_failed_when_the_remediation_receipt_failed` and the lost-response test.
  - render an untrusted check as `trusted` -> `every_judge_check_prints_its_trust_state` and `a_measured_but_weak_check_says_why_it_is_untrusted`.
  - stop counting model-invented trace ids -> `counts_come_from_validated_trace_lists_not_from_the_model` and the sort test.
  - drop the `still_failing` Warn -> `a_remediated_note_that_still_fails_is_a_warn`.
- `cargo test --workspace --lib`: 1782 + 696 + 303 + 175 + 143 + 11 + 618 passed, 0 failed (borg, cortex, distillers, oracle, sb, strip_transcripts, vault).
- **`otto ci`: NOT CONFIRMED GREEN.** The first run's `lint`, `bloat`, `agents-map`, `source-lint`, `eval-test` (15 passed) and `test` (`finished successfully  39s`) tasks passed. `promptfoo` was still running when the harness killed the run (see Open questions). `check` (clippy and fmt) has no finish line in the log I could read.

### Design decisions
- One gather, two readers - `borg/src/eval/weekly.rs:gather` - the report and the `sb doctor` line read the same `remediate::plan`, `judge_check_trust`, remediation and candidate reads, so the report and the doctor cannot disagree about "failing" or "untrusted". `health` is `gather` without the model call.
- The model names categories and assigns traces; Rust owns every number - `weekly/cluster.rs:assemble` - a count is the length of the validated trace list, invented trace ids are dropped and printed ("named N trace ids that were not in its input"), and a failing trace the model left out goes to `Unclustered`, so nothing vanishes. A reply that is not the `{"categories":[...]}` object is an error, never an empty clustering.
- A failed clustering call fails the run - `weekly::build` - no report is better than one with categories no model made. `sb doctor` warns when the newest report is older than `eval.report.stale-after-days` (8), so a dead timer is visible.
- Clustering input is built from the current failing rows only, with the rejected claim texts a `judge-claims` row marked not supported (`weekly.rs:rejected_claims`, verdict `i` is `distilled.claims[i]`), capped at `eval.report.max-failures` (200) in trace-id order, and the report says how many were left out. Failures from an untrusted judge are tagged `[untrusted judge]` in the prompt.
- One message, by construction - `notify::MessageSink` + `announce_unless` - `announce` takes one `&str`, so a per-category or per-trace send is not expressible. The disable gate is a parameter of `announce_unless` so a test can open it (the process gate is always closed under `cfg!(test)`); `announce` passes `real_notifications_disabled()`. The send is plain text, not HTML, through the sink trait so tests record instead of sending.
- A missing or failing Telegram sink is said on stderr and does not fail the run - `sb/src/cli/borg/weekly.rs:notify` - the report is already on disk; a Telegram outage must not turn the timer red.
- The Remediated outcome reads the remediation receipt, not the POST - `weekly/remediated.rs:classify` - a claim with no trace is resolved through `receipts.replay_of` + `replay_origin = 'auto-remediation'`; no trace and no intake row is `pending`, or `attempt-failed` when the claim carries a `post_error`. The "before" is the claim's frozen trigger key, read from `eval_scores`.
- The weekly report is a second timer pair - `eval/timer.rs` - `sb-eval-report.service` (`sb borg eval --weekly`) and `sb-eval-report.timer` (`eval.report.schedule`, default `Sun *-*-* 06:00:00`), rendered by the same private `render` the daily pair uses (one `Spec` for what differs), with the same env-bootstrap and the same refuse-to-render rule. `install` renders both pairs before writing any file; `enable_commands` enables both timers in the one `systemctl --user enable --now` call, and `sb bootstrap` prints both.
- Doctor Warn conditions - `sb/src/cli/checks/eval.rs:eval_findings` - an untrusted judge check (the "not a silent downgrade" rule), a remediated note still failing after its one re-distill, no weekly report or one older than `stale-after-days`. Failing traces alone are not a Warn: with every judge untrusted and a 38% flag rate the loop would warn forever. A fresh install (nothing scored) is one Info line.
- `eval.report.model` defaults to the default judge, so the clustering call goes to a vendor `llm.api-keys` already carries a key for, and the timer's env-bootstrap already holds it.
- The clustering prompt is a fabric pattern, `borg/patterns/eval-cluster-failures.md`, listed in `sb bootstrap`'s embedded set, with `--patterns-dir` resolving it like every other eval pattern. CLAUDE.md's pattern counts (thirteen support patterns, 30 files) follow.

### Deviations
- Doctor flags the weekly report as stale after 8 days and warns when there is none - the doc says only "a `sb doctor` Warn line" - a weekly timer that stops firing is otherwise invisible; both are config (`eval.report.stale-after-days`).
- `--weekly` is a new flag on `sb borg eval` and the timer runs it - the doc's API Design lists no flag for the weekly run - the doc leaves the entry point open, and the daily `--unscored` pattern (one flag, one service) is the precedent.
- The report also lists measured candidate files with their holdout result and the `--apply-candidate` command - Phase 9's bullet does not name it, the Failure analysis and Prompt improvement sections do ("The weekly report lists each candidate with its deltas and the exact command"), and nothing before Phase 9 could.
- The report path is the real `~/.local/share/sb/borg/eval-reports/<date>.md`; `--report` writes an additional copy - the brief allowed the real path when the command writes it by design.

### Tradeoffs
- `sb doctor` runs the full `remediate::plan` scan (a staged-source read per scored trace) vs a summary the report leaves behind - the scan keeps the doctor line live and correct between reports. Cost: it grows with the scored-trace count; staged sources are swept after 14 days, so most old traces are a cheap `not_staged` miss. Revisit if doctor gets slow.
- Plain-text Telegram summary vs the HTML the ingest replies use - no escaping surface, and the message carries a path and counts only.
- Failing traces beyond `max-failures` are counted and named in the report but not clustered vs chunking the call - one call is what the doc specifies.
- A trace may appear in several categories, so category counts do not sum to the failing count - a trace that fails two ways is a fact about the trace; forcing one bucket would hide the second failure.

### Open questions
- `otto ci` was not completed. The `promptfoo` task runs for ANY new or changed `borg/patterns/*.md` (`bin/promptfoo-check` diffs `borg/patterns/*.md` against origin/main, untracked included), so adding `eval-cluster-failures.md` triggered the full regenerate-and-grade run over the fixtures (LLM calls, longer than the 600 s tool cap), and Phase 8's own open question says an untrusted-judge `llm-rubric` gate failed 26 of 36 regenerated notes. A non-distill pattern cannot change a distilled note, so the gate has nothing to measure for this file. Options: (a) narrow `bin/promptfoo-check` and its pytest to `borg/patterns/distill-*.md` (matches the Phase 8 criterion, which names `distill-video.md`); (b) keep the gate as written and run it to completion with `GEMINI_API_KEY` set; (c) move the clustering prompt out of `borg/patterns/` (inline text written to a temp file for fabric). Which?
- This implementer's Bash tool died mid-`otto ci` ("task output swap refused (tasks dir moved or linked)": the session tasks dir was replaced), so the commit was not made. The tree holds all Phase 9 changes uncommitted.
- Should `sb borg eval --weekly` also run `--inject` or `--unscored` first? It reads whatever the last runs left, which is what the doc describes.
- Telegram send is untested live (by instruction). Post-deploy: after `otto deploy` and `sb bootstrap`, run `sb borg eval --weekly` once with notifications on and confirm exactly one message arrives.

### Background processes
None left running by this phase. (The harness reported the `otto ci` background task as killed; with Bash unavailable I could not confirm no `otto` process remains.)

## Orchestrator: after Phase 9

### Design decisions
- Phase 9 committed by the orchestrator as e51f34a after the worker's shell died mid `otto ci` (another session replaced /tmp/claude-1000 with a symlink; Claude Code refused the moved tasks dir until restart). `otto ci` green before the commit.
- Scott delegated the open questions. Decisions taken and recorded in Resolved Decisions: promptfoo distill-only and report-only (272f0c8); GEPA search gated on judge trust at `--candidate-split` (1abca1c); remediation stays off the timer; Phase 8 criterion 1 amended to the winner case.

### Deviations
- `eval.limit` default changed None -> 100 (0e05aba). Resolved Decisions already claimed the code default was 100 while only the deployed borg.yml set it; the code now matches the doc.

### Tradeoffs
- promptfoo report-only vs gating: the grader is an untrusted LLM judge, so a gate would block `otto ci` on noise; it prints failures and a count instead.

### Open questions
None.

## Implementation audit round 1 fixes

Source: the round 1 Mode 2 audit synthesis (findings 1-7; 8-11 deferred by the synthesis). Every claim was checked against the code before changing it; none was refuted. Code commit 96f73be.

### Design decisions
- Finding 1: `Ungated` is decided by the check, never the model string - `borg/src/eval/trust.rs:judge_check_trust` - only a check in `scores::DETERMINISTIC_CHECKS` is ungated; an LLM check with an empty or blank model is `Untrusted` on every applicable class ("fabric's default model is never trusted"), so every caller (remediation, weekly, `--inject` report, candidate split/apply gates) fails closed with no change of its own. Test: `eval::trust::tests::an_llm_check_with_an_empty_judge_model_is_untrusted` (perfect `""` and `"  "` rows still untrusted).
- Finding 1: a blank model never reaches a judge - `borg/src/config/eval.rs:EvalConfig::validate` rejects a blank `eval.judge-models` entry at load; `sb/src/cli/borg.rs:eval_opts` refuses `--judge-model ''`; `borg/src/eval.rs:fabric_judges`, the one constructor of every LLM judge (main eval, `--regenerate`, `--unscored`, `--inject`, candidate score/apply), refuses a blank model unless `--no-judge`. An explicitly emptied `judge-models: []` now makes a judged run fail loudly instead of judging with fabric's default. Tests: `config::eval::tests::a_blank_judge_model_is_a_load_error`, `cli::borg::tests::a_blank_judge_model_flag_is_refused`, `eval::judge::tests::fabric_judges_refuses_a_blank_model_for_a_judged_run`, and the inverted tail of `the_default_judge_is_gemini_and_a_flag_or_config_overrides_it` (it pinned "emptied list falls back to fabric's default").
- Finding 2: the claim freezes the trigger row's metrics - `borg/src/eval/remediate.rs:claim` writes `Failure::metrics_json` into the new `remediations.trigger_metrics_json` (schema.sql, receipts schema v7, additive `has_column` migration) - `weekly/remediated.rs:before_text` rebuilds the row from the claim alone and never reads `eval_scores`. A pre-column claim (NULL) says "the trigger row was not frozen", never a live read. Tests: `eval::weekly::tests::the_before_score_survives_an_overwrite_of_the_trigger_row` (overwrites the key with a passing row, "before" still says `0 of 1 quotes grounded`; fails against HEAD's `remediated.rs`), `a_claim_without_frozen_metrics_says_it_was_not_frozen`, the frozen-metrics assertion in `eval::remediate::tests::a_deterministic_check_failure_triggers_a_remediation_even_when_no_judge_check_is_trusted`, `receipts::tests::v7_migration_adds_the_frozen_trigger_metrics_to_a_v6_claim_table`.
- Finding 3: `config.staging.root` threaded through `run_eval_unscored`, `run_eval_inject`, `run_eval_remediate` (`sb/src/cli/borg.rs`), `run_split`/`run_score`/`run_apply` (`candidate.rs`), `weekly::run`, the doctor eval line (`checks/eval.rs:read_health`), `--staged`'s default (`eval_opts`; `EvalOpts::from` no longer picks a root), and `sb oracle eval --target answerability`, which now loads borg.yml for it. Test: `cli::borg::tests::staged_reads_the_configured_staging_root_unless_a_flag_overrides_it`.
- Finding 4: `RegenerateReport::all_judged_errored` (both arms) and `errored_of_total` - `borg/src/eval/regenerate.rs` - the `--regenerate` branch bails on it exactly like the main path. Test: `eval::regenerate::tests::regenerate_reports_all_judged_errored_only_when_every_trace_errored`.
- Finding 5: `--candidate-split` JSON gains `fabric-env` (fabric var -> the `llm.api-keys` reference: env var name or file path) - `sb/src/cli/borg/candidate.rs:split_json` - and `eval/propose/reflect.py:child_env` resolves each reference (existing file read and trimmed, else the named env var) onto the fabric child only, warning by reference name and leaving fabric's var alone when it does not resolve, as `vault::fabric::command` does. Tests: `cli::borg::tests::candidate_split_json_carries_the_api_key_references_under_fabric_names`, pytest `test_the_proposer_exports_the_configured_vendor_keys_under_fabric_names`, `test_an_unresolvable_key_reference_leaves_fabric_on_its_own_env`.
- Finding 6: restored, not required: `--synth-intents` with no `--judge-model` runs fabric's own default model again (its v0.17.7 behavior); `--judge-model` still names one. `eval_opts` treats synth as a non-judged mode for the default. Test: `cli::borg::tests::synth_intents_keeps_fabric_default_model_unless_a_flag_names_one`.
- Finding 7: one bullet in the design doc's Usefulness prerequisite: oracle's BM25 arm treats every query as natural language, so `OR`/`NOT`/`NEAR`/phrase quotes are literals, with the probe numbers.
- Each new test was proved to bite with a compiling break (condition forced false, frozen value replaced by `None`, configured root replaced by the hardcoded default, `env=None` on the Python child), each failing on its assertion, then restored.

### Deviations
- Finding 5 at a different seam than "read borg.yml in Python": sb hands the reflection step the key references through the split JSON it already parses. Same effect, correct seam: borg.yml keeps one loader and one normalize chain (`vault::config::load_config`), and no Python YAML dependency was added. The contract line in `eval/AGENTS.md` names the field.
- Finding 3 also moved `--staged`'s default root (pre-existing, not new in this loop) to `staging.root`, same class. `sb/src/cli/checks.rs:data_dir_findings` (pre-existing at v0.17.7, the synthesis's precedent) still reports `borg_stages_dir()`: it is a disk-footprint line with no config in hand, outside the eval consumers the finding names.
- Receipts `SCHEMA_VERSION` 6 -> 7 for the additive column; no rebuild, older binaries ignore it.

### Tradeoffs
- Frozen metrics copy on the claim vs making the trigger `eval_scores` row immutable - the key is legitimately rewritten (`--inject` baseline rows, `--rebuild-cache`), and freezing it would block rescoring; the copy freezes only what the report needs.
- Refusal at `fabric_judges` plus early flag/config checks vs refusal in `eval_opts` alone - `eval_opts` cannot know which modes judge (`--candidate-split`, `--remediate`, `--weekly` never call a judge with `opts.judge_model`), so the single constructor every judge passes through is the narrowest chokepoint that also covers future judged modes.
- Python resolves the reference itself vs sb printing values - key values never appear on any stdout or file; they exist only in the child's environment.

### Open questions
- `eval.report.model: ""` still runs the weekly clustering call on fabric's default model. It is not a judge and gates nothing; reject it at load too for symmetry?
- The eval readers assume the per-trace staging layout (`<root>/<trace>/`). `staging.layout: per-stage` would put traces under `<root>/raw/<trace>/` and every eval reader would find none. Pre-existing; worth a load-time refusal or layout-aware reader?
- Findings 8-11 (judge-pattern identity in trust keys, GEPA objective wording, 24k-char judge truncation, CLI ingest client toast) stay deferred per the synthesis.
