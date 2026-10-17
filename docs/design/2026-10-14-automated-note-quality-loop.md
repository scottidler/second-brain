# Design Document: Automated Note Quality Loop

**Author:** Scott Idler (drafted with Claude)
**Date:** 2026-10-14
**Status:** Implemented (2026-10-17; phases 0-9 on main, implementation audit pending)
**Review Passes Completed:** 5/5 (draft; correctness: every path:line and measured number re-read from the research brief run on ripr at d4e44d3; clarity: phases reordered so plumbing lands before judges; edge cases: replay loops, judge-vendor bias, staging expiry, key threading; excellence: voice lint). Review panel round 1 folded in (synthesis `~/.cache/review-panel/runs/gNfmuD6i/synthesis.md`), including findings 1, 15, 16 and 18 on 2026-10-17. Round 2 (Scott-ordered, scoped to those folds; same synthesis file, round 2 section) folded the same day.
**Evidence re-run:** 2026-10-17 on ripr at v0.17.7 (`e0b7135`). Every path:line, count and Observed-on-main line below is from that run.

## Summary

Scott treats the vault as a store that agents query. He will not read, label, review or write notes. This doc turns `sb borg eval` into a loop that runs unattended:
- Score every new ingest.
- Measure the judges with planted errors and with agreement across vendors, instead of human labels.
- Measure whether notes answer the questions Scott has asked oracle in past sessions.
- Re-distill a failing note once.
- Hand Scott a measured prompt change he applies with one command.

Third-party tools (DeepEval, DSPy GEPA, MiniCheck, promptfoo) run in a `uv` project beside the Rust code. Any that prove useful get ported into `sb`.

## Problem Statement

### Background

- Scott, 2026-10-14:
  - "regularly i treat them as a data store to RAG and query and research on. whatever our answer is I dont want to manually review notes or create notes I want a skill/automated/framework/whatever to create the system improtve the quality etc"
  - "lets download and use any of these tools even the python ones to improve our things"
  - "if the python code proves very useful, we will port it to rust in sb"
- `sb borg eval` v0.16.0 (commit e1db1ec):
  - Golden fixtures or `--staged [--sample N]`.
  - Deterministic quote grounding, listicle survival, note size, placeholder artifacts.
  - `judge-distillation` (three 0-3 axes) and `judge-claims` (a verdict per claim).
  - SQLite judgment cache.
- The 2026-10-14 scour found the same consensus in industry and in Scott's own prior research (session df5a3954):
  - Cheap source-grounding checks first.
  - Narrow yes/no judges per failure mode.
  - Judges measured before they are trusted.
  - Coverage checked separately, because judges miss omissions (arXiv 2608.31016).
- Companion doc: `docs/design/2026-10-14-note-sources-intent-tools-channels.md`.
  - It is `Status: Implemented`. Its Phase 9 shipped `sb borg eval --regenerate` and `--patterns-dir` in 69509ea, so both flags exist on main.
  - This doc depends on its Phase 6 (`--note` through replay) and Phase 9 (`--regenerate`). Both have landed.
  - It does not change that doc's rule that `eval --staged` reads staging.

### Problem

`sb borg eval` is a developer command. Measured on ripr at v0.17.7 (`e0b7135`), it cannot run as a loop:

1. **Judge keys come from the wrong place, and oracle's judge has none.**
   - Since 69509ea, borg's `FabricJudge` and `FabricClaimJudge` get `llm.api-key` from `borg.yml`, normalized into `fabric.api_key` (`borg/src/config.rs:27`, read at `sb/src/cli/borg.rs:370`, passed at `borg/src/eval/judge.rs:155,341`). So `sb borg eval --staged --sample 3 --rebuild-cache` now exits 0 with `ANTHROPIC_API_KEY` unset in the shell.
   - Oracle's relevance judge still passes `""` (`oracle/src/eval/judge.rs:107-111`). It falls back to fabric's `.env`, whose `DEFAULT_VENDOR` is Anthropic and which holds no `ANTHROPIC_API_KEY`.
   - `vault/src/fabric.rs:90-95` maps the one configured key onto `ANTHROPIC_API_KEY` only. A Gemini or OpenAI judge works today only because `~/.config/fabric/.env` happens to hold `GEMINI_API_KEY` and `OPENAI_API_KEY`, outside keep and outside the timer's secret bootstrap.
2. **No machine-readable output.** `EvalReport::render()` (`borg/src/eval/report.rs:244`) is the only output: no JSON, no per-trace rows. `--format json` is rejected by clap.
3. **No way to select unscored ingests.** `load_staged` samples by trace-id hash and has no time or receipts filter (`borg/src/eval/fixtures.rs:181`).
4. **One judge error aborts the run** (`with_context(..)?` + `bail!` on the judge calls in `borg/src/eval.rs:330-350`).
5. **The judges see a lossy note.**
   - `judge_note_text` (`borg/src/eval/fixtures.rs:272`) renders no `quote` and no `who`.
   - The input puts the source before the note (`borg/src/eval/judge.rs:150`). A source at the 24,000-char cap (`borg/src/eval.rs:44`) leaves fabric's 24,000-char head budget no room for the note.
6. **The judges are unmeasured.** `config/eval/distill-calibration.yml` does not exist, and human labels are off the table.
7. **The judge shares a vendor with the distiller (Anthropic).** Self-preference is a documented judge bias (vault note `llm-as-a-judge-scaling-ai-evaluation-strategies.md`).
8. **Nothing measures what Scott uses notes for.**
   - `sb oracle eval` runs 9 hand-written queries (`config/eval/queries.yml`).
   - Scott's past oracle queries on ripr: 324 `knowledge_search` calls, 271 distinct, by a regex over raw lines of the `~/.claude/projects` transcripts (87 files matched, subagent transcripts included; a JSON parse finds the same calls in 81 files).
   - `sb oracle eval` exits 1 on main before any judge runs: its BM25 arm passes the raw query to FTS5 `MATCH` (`vault/src/search/query.rs:249-256`), and `24/7` in a hand-written query is an FTS5 syntax error. Search has failed closed on a malformed `MATCH` since f97718f (`query.rs:31-40`), which is right. The bug is that oracle hands a natural-language query to `search` unquoted (`oracle/src/server/pipeline.rs:46,155`). Mined queries carry punctuation like this routinely.
9. **Nothing acts on a bad score, and replay cannot carry lineage.**
   - `receipts::record_replay` (`borg/src/receipts.rs:473`) is `INSERT OR IGNORE` (`:489`). `/ingest` writes the receipt before it returns, so a post-POST call leaves `replay_of` NULL.
   - It is called only from tests today, and `replay_of` is null on every row.
   - A URL replay is a fresh `force: true` POST with a new trace (`borg/src/replay.rs:267`), so nothing stops fail -> replay -> fail.
10. **Nothing improves the prompts.** Pattern edits are hand-written. `--regenerate` and `--patterns-dir` now exist (69509ea), but nothing proposes a candidate pattern or measures one against a coverage floor.

### Goals

- Every successful ingest gets scored within a day, unattended, with per-trace results in a table.
- Judge quality is measured without human labels: catch rate on planted errors, and agreement across vendors, with their limits stated.
- A usefulness score built from Scott's own past queries.
- A failing note gets one automatic re-distill, never a loop.
- Prompt changes arrive as a measured candidate Scott applies with one command. He never reviews notes.
- The Python tools prove themselves against the Rust metrics, and any that earn it get ported.

### Non-Goals

- **Any step where Scott reads, labels, reviews or writes notes.** Excluded by his direction. Running `--apply-candidate` on a candidate the loop already measured is the one approval left.
- **Hiding low-scoring notes from oracle.** Excluded: Scott uses the vault for recall, so a flagged note stays searchable.
- **Publishing reports to marquee.** Excluded: marquee is the Okta-gated work surface, and second-brain is a home repo.
- **Ragas.** Excluded for now:
  - A bare install of 0.4.3 is broken (`langchain_community...vertexai` missing).
  - Its Faithfulness and SummaryScore duplicate DeepEval's.
  - Revisit if DeepEval disappoints in Phase 0.
- **Scoring from the companion's archive.** Parked: scoring reads staging, as the companion doc states. Revisit if the unscored backlog ever outlives the 14-day staging window.
- **Running the promptfoo check in GitHub CI.** Excluded: it needs judge API keys, which the GitHub workflow does not hold. It runs locally in `otto ci`, the gate before `bump release`.

## Proposed Solution

### Overview

```
successful receipts without a score
          │
          v
sb-eval.timer (daily) ──> sb borg eval --unscored --format json
                                │
          ┌─────────────────────┼──────────────────────────┐
          v                     v                          v
  deterministic checks   judges (Gemini default,     eval/ uv project
  (quote grounding ...)   Anthropic second)           (DeepEval on OpenAI, MiniCheck)
          └─────────────────────┼──────────────────────────┘
                                v
                     eval_scores table (per trace)
                                │
     ┌──────────────┬───────────┼──────────────┬──────────────────┐
     v              v           v              v                  v
 one re-distill  weekly      judge quality   answerability     GEPA candidate
 per source      failure     (planted errors, (mined queries)   (regenerate +
 (lineage)       clusters    vendor kappa)                      coverage floor)
```

### Architecture

**Run and store**
- `sb borg eval` gains:
  - `--format json`
  - `--unscored`: the traces of successful receipts that are not yet covered (the coverage rule is under the `eval_scores` bullet below), oldest first.
  - `--count`: with `--unscored`, print the size of that backlog and score nothing.
  - Per-trace error capture.
- `--unscored` takes an optional `--limit` from config. The backlog carries over to the next run, so every ingest gets scored eventually, not just a sample.
- A failed judge call writes an error row and the run continues. Exit is non-zero only if every judged trace errored. An error row never triggers remediation.
- Judge keys, per vendor:
  - `llm.api-keys` in `borg.yml` and `oracle.yml` maps each vendor to its key var (`anthropic: ESCOTE_ANTHROPIC_API_KEY`, `gemini: GEMINI_API_KEY`, `openai: OPENAI_API_KEY`).
  - `vault::fabric` exports each mapped var under fabric's expected name on the child process.
  - All three judge sites use it: borg's two judges and oracle's relevance judge.
  - `llm.api-key` keeps working as the Anthropic entry. borg normalizes it into `fabric.api_key` today (`borg/src/config.rs:27`).
  - Each of the three judge sites gets a construction test showing the configured map reaches it, not only a test of the shared command builder.
- Judge input:
  - `judge_note_text` renders `quote` and `who` under each claim.
  - The note goes first, then the source, which gets the remaining budget: max chars minus pattern minus note.
  - `RUBRIC_VERSION` and `CLAIMS_RUBRIC_VERSION` bump to `v2`.
- A new `eval_scores` table in `eval-cache.db`, keyed on `(trace, run_kind, run_id, content_hash, rubric_version, judge_model, check_name)`.
  - `run_kind` is `baseline` (the staged note as published), `candidate` (a `--regenerate` run under a candidate pattern dir) or `inject` (a planted-error mutation).
  - `run_id` is `''` for baseline. For a candidate it is the pattern name plus the candidate file's hash. For an injection it is the inject run's id plus the planted class. So two candidates, or a candidate whose output happens to equal baseline, never share a row.
  - `check_name` names the judge check or deterministic check that produced the row (`judge-distillation`, `judge-claims`, `quote-grounding`, ...).
  - `content_hash` is the **distill hash**: `stable_hash(kind, budgeted source, judge_note_text)` (`borg/src/eval.rs:300`), the hash over exactly what the distillation judge sees. The claims cache's separate hash over claim text (`:327`) stays internal to that cache and never appears in `eval_scores`.
  - **Required checks:** the deterministic checks plus every check of the default judge model. The set comes from config, so adding the second judge to it is a config change.
  - **Current row:** for a trace and a required check, the `baseline` row whose `content_hash` is the trace's current distill hash and whose `rubric_version` is current. Older rows stay as history and nothing reads them.
  - **Covered:** a trace is covered when every required check has a current row with `error IS NULL`. An error row, or another judge's row, does not cover it, so the next run retries it. `--unscored` lists the traces that are not covered.
  - Remediation and the weekly report read current rows only. Kappa pairs current rows on `(trace, content_hash, rubric_version, check_name)` across judge models. GEPA reads `candidate` rows for its own `run_id`. The `--inject` report reads `inject` rows for its own run.
- `sb-eval.timer` + `sb-eval.service` are cloned from the harvest timer (`borg/src/harvest/timer.rs:43,122`, rendered through `vault::systemd`).
  - The service has its own `ExecStartPre` secret bootstrap and env file carrying all three vendor keys.
  - `sb bootstrap` writes the units and runs `systemctl --user enable --now sb-eval.timer`. Today's harvest install only prints that step.

**Judge quality without labels**
- Planted errors (`borg/src/eval/inject.rs`): deterministic mutations of a cloned `Distilled`.
  - Swap a number in a claim.
  - Fabricate a quote using words that are not in the source.
  - Drop the top claim.
  - Change a claim's `who`.
- Not every check can see every class. Trust is measured only where a check can see the mutation. The **applicability matrix**:

  | Check | number-swap | fabricated-quote | dropped-top-claim | who-change |
  |---|---|---|---|---|
  | `judge-distillation` (sees `judge_note_text`, which renders quote and who after Phase 1) | yes | yes | yes | yes |
  | `judge-claims` (input and hash are claim text only, `borg/src/eval.rs:326-327`) | yes | no | no | no |
  | `quote-grounding` (deterministic) | no | yes | no | no |

  - `judge-claims` stays on claim text: its job is per-claim support, and dropping a claim leaves the remaining claims supported. The cells marked no are reported as `n/a`, never as a miss.
  - Adding a column to the matrix means adding a check that can see that class.
- The `--inject` report prints, per check and applicable class:
  - **Paired catch rate:** the share of applicable traces where the check flags the mutant AND passes that trace's unmutated original. A check that flags everything scores 0 here, not 1.
  - **Raw catch rate** (true-positive rate on planted errors).
  - **Flag rate on unmutated notes.** This is a false-alarm proxy, not a true-negative rate, because production notes are not known-clean.
  - **Applicable sample size:** distinct traces where the mutation was actually generated and applies (a note with no number cannot take a number swap).
- Cross-vendor agreement: the same traces judged by Gemini and Anthropic through fabric, plus DeepEval running on an OpenAI model and MiniCheck from the `uv` project. That makes three model vendors plus one small local checker. The report prints pairwise kappa.
- What this measures and what it misses (stated in every report header):
  - It measures catch rates for the four planted classes, and consistency between judges.
  - It misses:
    - Omissions outside the planted classes.
    - Errors that every vendor shares.
    - Failures specific to long documents.
    - Style bias.
    - The base rate of errors in production.
  - Kappa is agreement, not correctness.
- Default judge: Gemini (via fabric), because the distiller is Anthropic. Anthropic runs as the second judge.
- **What an untrusted judge is barred from** (panel round 1 finding 1):
  - Every `--inject` run writes a `judge_trust` row per judge model, check and applicable class: paired catch rate, raw catch rate, flag rate, applicable sample size, rubric version, measured time.
  - A judge check is trusted when, on every class the matrix marks applicable to it:
    - its paired catch rate is at least `eval.trust.min-paired-catch-rate` (default 0.80), and
    - its applicable sample size is at least `eval.trust.min-samples` (default 30), and
    - the row is for the current rubric version.
  - Both defaults are policy floors, not proof. 24 of 30 caught has a one-sided Wilson lower bound near 0.66, so a trusted check is "caught at least this often on a small synthetic sample", nothing stronger. The report prints the lower bound next to each rate.
  - Its scores are always recorded, trusted or not.
  - `--remediate` acts on a failure from a trusted judge check, or from a deterministic check. A deterministic failure stays actionable even when no judge is trusted. A failure that only an untrusted judge check reports is listed with the judge, check and class that failed the gate, and nothing is posted for it.
  - GEPA and `--apply-candidate` refuse to run when any factor of the metric comes from an untrusted check. Dropping that factor would silently change the objective, so they name the check and stop.
  - The weekly report and `sb doctor` print each judge check's trust state, so an untrusted judge is a Warn, not a silent downgrade.
  - Deterministic checks are not gated: they make no model call. The same `--inject` run measures them, and the report shows their rates.

**Usefulness from Scott's own queries**
- `sb oracle eval --mine-queries <out.yml>` reads `knowledge_search` arguments from clyde's session export across hosts and dedups them.
  - Only sessions whose clyde export `scope` is `personal` qualify. The field is always `work` or `personal`, resolved as `scope_override -> stored scope -> classifier fallback` (`tatari-tv/clyde/sessions/src/export.rs:149-155`).
  - A missing or unknown scope drops the session (fail closed).
- The `answerability` target: for each mined query's top hits, `judge-relevance` (`oracle/src/eval/judge.rs:21`) grades the note, and grades the note's source.
  - "Source >= 2, note <= 1" is a distillation-loss signal that needs no labels.
  - **Source read path** (panel round 1 finding 16): `sb` reads the hit note's `trace`, then loads that one trace from borg's staging root with the same reader `load_staged` uses (`borg/src/eval/fixtures.rs:181`), exposed as a single-trace `load_staged_trace`. The fixture's `source` is the judged text.
  - **Every hit gets one outcome**, and only `compared` is scored:
    - `compared`: source and note both judged.
    - `no-trace`: the note's `trace` is empty (`vault/src/search.rs:488` allows `""`).
    - `source-expired`: the trace has been swept from staging.
    - `source-unreadable`: missing artifacts, an unsupported kind, or a parse error from the staged reader.
  - The report prints eligible and compared pair counts. Zero compared pairs is `inconclusive`, never "no loss".
  - **What it cannot see:** only notes oracle already retrieved enter the comparison. A distillation that dropped the very terms a query needs never surfaces as a hit, so that loss is not measured here. Sources come only from the 14-day staging window, so the score skews to recent notes. The report header says both. Measuring the first means retrieving over sources, which this doc does not build. Fixing the second is the parked archive-backed scoring (Non-Goals).
- **Prerequisite, landed before Phase 1 as its own targeted fix:** oracle's BM25 calls quote natural-language queries for FTS5. Without it `sb oracle eval` dies on the first query carrying `/` (Problem 8), and mined queries carry punctuation routinely.
  - The quoting goes at oracle's natural-language boundary: the BM25 arm of `run_search_mode` (`oracle/src/server/pipeline.rs:155`) and the hybrid helper `bm25_paths` (`:46`).
  - It does NOT go in `search_filter` (`vault/src/search/query.rs:249-256`). `find_similar` (`query.rs:46-58`) already sends a constructed `"a" OR "b"` expression through that path, and quoting it again turns the OR into a literal (a probe in round 2 measured `rust OR python` drop from 3 matches to 1).
  - The fix carries regression tests for punctuation, embedded quotes, and `find_similar`'s constructed query.
- The cross-crate join (borg eval scores x oracle retrieval) lives in `sb`, because borg does not depend on oracle.

**Remediation**
- The `/ingest` body gains an additive `replay-of` field (the `IngestRequest` additive rule holds).
  - Intake writes it in the same transaction that creates the receipt row.
  - `record_replay` stays for tests only.
- What counts as an attempt: a remediation is counted when its intake row exists with `replay-of` set and `origin = auto-remediation`, whatever happens after (fetch failure, lost response, crash).
- Before posting, `--remediate` runs the check and the claim in one SQLite transaction. It counts auto-remediation rows for the source URL, and inserts a pending marker only when the count is 0. So concurrent runs cannot both post.
- A note that still fails stays searchable. It is listed in the weekly report and in `sb doctor` as a Warn.
- **Remediation trail** (panel round 1 finding 18): the weekly report has a Remediated section. Each source gets its original and remediation trace (joined on `replay_of`), the before and after scores, and one outcome:
  - `pending`: the remediation receipt is still `received` or in flight.
  - `passed`: the remediated trace is covered and passes every required check.
  - `still-failing`: covered, and some required check still fails.
  - `unscored`: the remediation ingest succeeded but the trace is not yet covered.
  - `eval-errored`: the latest attempt to score it left only error rows.
  - `attempt-failed`: the remediation receipt ended `failed` or `crashed`.
  - The outcome comes from the remediation receipt's status, not from the POST response. Intake returns `Queued` and runs the ingest detached, so a lost response is `pending` until the receipt settles.
  - The "before" score is frozen: the pending marker stores the key of the current row that triggered the remediation, and the report reads that row. A later rescore of the original trace never rewrites it.
- Depends on companion Phase 6 (landed), so a remediated note keeps its capture note.

**Prompt improvement**
- Builds on companion Phase 9's `sb borg eval --regenerate`, which re-distills fixture sources through the current patterns.
- `--patterns-dir <DIR>` already exists on main (69509ea): it resolves every pattern the eval distills or judges through from DIR. A candidate pattern runs from a temp dir without touching `~/.config/sb/patterns/`.
- The `uv` project runs DSPy GEPA over one distill pattern at a time.
  - The metric is quote grounding x claim support rate x listicle survival, on a frozen staged sample.
  - Coverage floor: a candidate that cuts claims per note or enumeration items per note by more than 10% vs baseline scores 0. A pattern cannot win by saying less. Both measures are Rust. (Amended in phase 8: the floor named "DeepEval coverage" too, and DeepEval was dropped in phase 5; see Resolved Decisions.)
  - The winner is re-scored on a disjoint sample before it is written.
  - Output: `<report-dir>/candidates/<pattern>-<date>.md` with the diff and the before/after metrics on both samples.
- The weekly report lists each candidate with its deltas and the exact command. `sb borg eval --apply-candidate <file>` writes the pattern into `borg/patterns/`, re-runs `--regenerate` on the disjoint sample to confirm, and leaves the change for Scott's normal ship (commit, `bump release`, `otto deploy`).
- The repo ships direct to main (789 commits, 0 merges), so there is no PR.
- promptfoo runs llm-rubric and context-faithfulness checks over `config/eval/distill-fixtures/` (regenerated) inside `otto ci`, only when `borg/patterns/*.md` changed against `origin/main`.
- The GEPA baseline is frozen after companion Phases 9-11 land, so the companion's prompt changes are not mistaken for GEPA gains.

**Failure analysis**
- Weekly, an LLM groups the flagged claims and low axes from `eval_scores` into failure categories, with counts and three example traces each.
- Output goes to `~/.local/share/sb/borg/eval-reports/<date>.md`, a `sb doctor` Warn line, and the existing Telegram notification sink.
- Nothing is written to the vault.

**The `uv` project**
- Location: `eval/` at the repo root, with `pyproject.toml` + `uv.lock`, dependencies added with `uv add`.
- Interface is files only. Contract:
  - `sb borg eval --format json` writes one JSONL record per trace: `{trace, run_kind, run_id, rubric_version, kind, content_hash, source, source_truncated, note_text, claims: [{id, text, quote?, who?}], metrics}`. `content_hash` is the distill hash.
  - Each tool writes verdict JSONL back: `{trace, run_kind, run_id, rubric_version, content_hash, tool, tool_version, model, claim_id?, metric, value, detail?}`. The tool copies the first five fields from the record it judged.
  - `sb borg eval --ingest-verdicts <file>` rebuilds the full `eval_scores` key from those fields, with `check_name = tool` and `judge_model = model`. It loads a verdict only when its `content_hash` still matches that row's trace and run, and drops stale ones loudly, naming the trace.
- Run by `otto eval-py` and by `sb-eval.service`.
- Port rule: a tool earns a Rust port into `sb` when, on the Phase 5 measurements:
  - its planted-error catch rate beats the Rust check it would replace, or
  - it adds a signal nothing in Rust has.
  A tool that adds nothing is removed from `eval/`.

### Data Model

```sql
CREATE TABLE IF NOT EXISTS eval_scores (
    trace            TEXT NOT NULL,
    run_kind         TEXT NOT NULL,     -- 'baseline' | 'candidate' | 'inject'
    run_id           TEXT NOT NULL,     -- '' for baseline; candidate pattern+file hash; inject run id+class
    kind             TEXT NOT NULL,
    judge_model      TEXT NOT NULL,     -- '' for a deterministic check
    check_name       TEXT NOT NULL,     -- 'judge-distillation' | 'judge-claims' | 'quote-grounding' | tool name
    rubric_version   TEXT NOT NULL,
    content_hash     TEXT NOT NULL,     -- the distill hash (borg/src/eval.rs:300)
    scored_at        TEXT NOT NULL,     -- RFC 3339; compare with julianday()
    metrics_json     TEXT NOT NULL,
    error            TEXT,              -- non-null when a judge call failed; never covers a trace
    PRIMARY KEY (trace, run_kind, run_id, content_hash, rubric_version, judge_model, check_name)
);

CREATE TABLE IF NOT EXISTS judge_trust (
    judge_model         TEXT NOT NULL,  -- '' for a deterministic check
    check_name          TEXT NOT NULL,  -- 'judge-distillation' | 'judge-claims' | 'quote-grounding' | ...
    planted_class       TEXT NOT NULL,  -- applicable classes only (the applicability matrix)
    rubric_version      TEXT NOT NULL,
    paired_catch_rate   REAL NOT NULL,  -- flags the mutant AND passes its original; the trust metric
    raw_catch_rate      REAL NOT NULL,
    flag_rate           REAL NOT NULL,  -- on unmutated notes; a false-alarm proxy
    wilson_lower        REAL NOT NULL,  -- one-sided lower bound on paired_catch_rate
    sample_size         INTEGER NOT NULL, -- distinct traces where the mutation applied
    measured_at         TEXT NOT NULL,
    PRIMARY KEY (judge_model, check_name, planted_class, rubric_version)
);
```

### API Design

```
sb borg eval --staged [--sample N] [--format json] [--judge-model M] [--no-judge]
sb borg eval --unscored [--limit N] [--format json]
sb borg eval --unscored --count             # backlog size only; scores nothing
sb borg eval --inject [--sample N]
sb borg eval --ingest-verdicts <file.jsonl>
sb borg eval --apply-candidate <candidate.md>
sb borg eval --remediate [--apply]          # one re-distill per failing source; dry-run default
sb oracle eval --mine-queries <out.yml>
sb oracle eval --queries <mined.yml> --target answerability
otto eval-py
```

### Implementation Plan

Companion-doc dependencies: Phase 7 here needs companion Phase 6, and Phase 8 needs companion Phases 9-11. The companion is `Status: Implemented`, so nothing here waits on it.

Prerequisite: the FTS5 quoting fix (Usefulness section) lands first as its own targeted fix, with its own commit and tests. Phase 1 starts after it.

#### Phase 0: Spike the unproven pieces
**Model:** sonnet
- Zero repo code. Scratchpad only:
  - fabric with a Gemini `-m`, with `GEMINI_API_KEY` exported on the child only and fabric's `.env` key hidden, so the child env is proven to be the key source. fabric has no config-dir flag (`fabric --help` shows only `--config=`, a YAML file), so the child runs with `HOME` pointed at a scratch dir holding a copy of `~/.config/fabric/` minus the key lines.
  - DeepEval `SummarizationMetric` on an OpenAI model, over one exported staged trace.
  - MiniCheck's small CPU model (roberta or deberta, from the Liyan06 GitHub repo, not the PyPI name collision) on the same trace.
  - The clyde export `scope` field on 10 sessions.
- (The pattern-dir spike is gone: `--patterns-dir` shipped in 69509ea.)
- **Success criteria:**
  - Each of the three judges writes a verdict file for the same trace.
  - The spike notes record:
    - that the Gemini call succeeds under the scratch `HOME` with the key only on the child env, and fails under the same `HOME` with `env -u GEMINI_API_KEY`
    - MiniCheck's per-claim CPU latency
    - the clyde scope values seen (expected `work` and `personal` only)

#### Phase 1: Keys, judge input, JSON, error capture
**Model:** opus
- `llm.api-keys` per vendor through `vault::fabric`, at all three judge sites.
- `judge_note_text` renders quote and who; note first, budgeted source; rubric versions bump.
- `--format json` and per-trace error capture.
- Requires the FTS5 quoting prerequisite (Usefulness section) to have landed.
- **Success criteria:**
  - Unit test: with `llm.api-keys.gemini` naming a var, the fabric child command carries `GEMINI_API_KEY` set from it, and the same for `openai` and `anthropic`.
    - This replaces the earlier live check, which already passes on main because fabric's `.env` holds the Gemini key. Observed on main: `env -u ANTHROPIC_API_KEY -u GEMINI_API_KEY ~/.cargo/bin/sb borg eval --staged --sample 1 --rebuild-cache --judge-model gemini-2.5-pro` exits 0.
  - Construction tests: borg's `FabricJudge`, borg's `FabricClaimJudge` and oracle's relevance judge each receive the configured key map.
  - `env -u ANTHROPIC_API_KEY ~/.cargo/bin/sb oracle eval --k 1 --rebuild-cache` reports a judged pair count above 0 and zero judge errors. Exit 0 alone proves nothing: oracle's eval skips a failed judge with a warn and still exits 0 (`oracle/src/eval.rs:300-303`, `sb/src/cli/oracle.rs:130-142`).
    - Observed on main: exits 1, `fts5 search failed for query "how to run an AI coding agent 24/7 securely"`.
  - Unit test: a 30,000-char source still leaves the full note text in the judge input.
  - `~/.cargo/bin/sb borg eval --staged --no-judge --format json | jq -s length` equals the fixture count in the text report.
    - Observed on main: clap rejects `--format`.

#### Phase 2: Score table, `--unscored`, timer
**Model:** sonnet
- `eval_scores` (the full seven-part key), the required-check set, the current-row and covered rules, `--unscored [--limit]`, `--unscored --count`, and the `sb-eval` units with `enable --now` in `sb bootstrap`.
- **Success criteria:**
  - After one fire, `sqlite3 ~/.local/share/sb/borg/eval-cache.db "select count(*) from eval_scores where run_kind='baseline' and julianday(scored_at) > julianday('now','-1 day')"` > 0.
    - Observed on main: `.tables` -> `claim_judgments distill_judgments`; no `eval_scores`.
  - `systemctl --user is-enabled sb-eval.timer` prints `enabled`.
    - Observed on main: `systemctl --user list-unit-files 'sb-eval*'` -> `0 unit files listed.`
  - Unit test: a baseline row and a candidate row for the same trace and judge both persist, and so do two candidates whose output is identical.
  - Unit test: a trace whose only current row for a required check is an error row is listed by `--unscored`. So is a trace scored by a judge outside the required set. A trace with an older content hash's row but none for its current hash is listed.

#### Phase 3: Planted-error injection
**Model:** opus
- `eval/inject.rs` with the four classes, the applicability matrix, the `--inject` report (paired catch rate, raw catch rate, flag rate, Wilson lower bound, applicable sample size), and the `judge_trust` rows it writes.
- **Success criteria:**
  - Quote grounding catches 100% of fabricated quotes built from words absent from the source, with zero LLM calls (unit test).
  - `--inject --sample 30` prints those columns per check and applicable class, prints `n/a` for inapplicable cells, carries the "what this misses" header, and writes one `judge_trust` row per judge, check and applicable class.
  - Unit tests on the trust rule:
    - 0.79 paired catch rate on one applicable class: untrusted. 0.80 on every applicable class with 30 samples: trusted.
    - 1 of 1 caught: untrusted (below `min-samples`).
    - A check that flags every note, mutant and original alike: paired catch rate 0, untrusted.
    - A row for an older rubric version: untrusted.
    - `judge-claims` is never marked untrusted for missing a who-change or fabricated quote.

#### Phase 4: `uv` project and verdict ingest
**Model:** sonnet
- `eval/` with `pyproject.toml`, DeepEval and MiniCheck, the JSONL contract, `--ingest-verdicts`, and `otto eval-py`.
- **Success criteria:**
  - `otto eval-py` writes a verdict JSONL for the latest scored traces.
  - After `--ingest-verdicts`, `eval_scores` rows carry `deepeval` and `minicheck` metrics.
  - A verdict with a stale `content_hash` is rejected with an error naming the trace.
  - Round trip: verdicts for one trace's baseline, one candidate and one injection, all with the same tool, land in three distinct rows, and none overwrites another.

#### Phase 5: Cross-vendor agreement and judge default
**Model:** opus
- Gemini becomes the default judge model, with Anthropic second.
- The report prints pairwise kappa next to the planted-error table.
- **Success criteria:**
  - The report prints pairwise kappa over >= 30 traces.
  - The port/drop verdict for each `uv` tool is written into this doc's Resolved Decisions, citing the numbers.

#### Phase 6: Mined queries and answerability
**Model:** opus
- `--mine-queries` through clyde, with the `personal` scope filter.
- The `answerability` target, with `load_staged_trace` as the source read path and the "what it cannot see" header.
- **Success criteria:**
  - `~/.cargo/bin/sb oracle eval --mine-queries /tmp/q.yml` writes >= 50 queries, all from `personal`-scope sessions.
    - Observed on main: no `--mine-queries` flag (`sb oracle eval --help`). Clyde's `scope` is `work | personal` (`export.rs:149-155`).
  - `~/.cargo/bin/sb oracle eval --queries /tmp/q.yml --target answerability` prints a distillation-loss count, the eligible and compared pair counts, and a count per outcome (`no-trace`, `source-expired`, `source-unreadable`).
  - Unit test: zero compared pairs reports `inconclusive`, not a loss count of 0.
  - Unit test: a session with `scope: work`, and one with no scope, contribute no queries.

#### Phase 7: Remediation with lineage
**Model:** opus
- Needs companion Phase 6.
- `replay-of` in the `/ingest` body, written at intake. `--remediate` with the transactional single-attempt claim, gated on judge trust.
- **Success criteria:**
  - Unit test: two concurrent `--remediate` runs on the same failing source post exactly once.
  - Unit test: a failure scored only by an untrusted judge check triggers no remediation, and the listing names the judge, check and class.
  - Unit test: a deterministic-check failure triggers a remediation even when no judge check is trusted.
  - `sqlite3 ~/.local/share/sb/borg/receipts.db "select count(*) from receipts where replay_of is not null"` > 0 after one `--remediate --apply`.
    - Observed on main: `0`.

#### Phase 8: GEPA candidates and the promptfoo check
**Model:** opus
- Needs companion Phases 9-11.
- GEPA with the coverage floor over the existing `--regenerate --patterns-dir`, `candidate` rows in `eval_scores`, `--apply-candidate` gated on judge trust, and the `otto ci` promptfoo task.
- Coverage floor (amended in phase 8): claims per note and enumeration items per note, both Rust. "DeepEval coverage" left with DeepEval (phase 5).
- **Success criteria:**
  - One GEPA run that finds a rewrite beating the current pattern writes a candidate file with before/after metrics on two disjoint samples. A run with no winner writes nothing and says so. (Amended at phase 8: four live runs found no winner, so the original wording, which assumed every run has one, was a doc defect. See Resolved Decisions.)
  - Unit test: a candidate with 20% fewer claims per note scores 0.
  - `otto ci` runs the promptfoo task when `borg/patterns/distill-video.md` differs from `origin/main`, and skips it otherwise.

#### Phase 9: Weekly failure clusters
**Model:** sonnet
- Weekly clustering into `eval-reports/<date>.md`, the Remediated section, judge trust states, the `sb doctor` Warn, and the Telegram notification.
- **Success criteria:**
  - One run writes `~/.local/share/sb/borg/eval-reports/<date>.md` with at least one category, a count and example traces, a Remediated section (empty is fine), and each judge's trust state.
  - `sb doctor` prints the eval line.

## Acceptance Criteria

All observed lines re-run 2026-10-17 on ripr at v0.17.7 (`e0b7135`).

- [ ] Every successful ingest gets covered (a current, error-free row for every required check) with no human step. After a week of timer runs, `sb borg eval --unscored --count` prints 0, or a number below one day's ingest count. This measures the backlog left over, not what one run processed.
  - Observed on main: no `sb-eval.timer` (`systemctl --user list-unit-files 'sb-eval*'` -> `0 unit files listed.`); no `eval_scores` table (`sqlite3 ~/.local/share/sb/borg/eval-cache.db .tables` -> `claim_judgments distill_judgments`); clap rejects `--unscored` and `--format`.
  - Observed on main: `sb borg eval --staged --sample 3 --rebuild-cache` with `ANTHROPIC_API_KEY` unset in the shell now exits 0 (3 scored), because 69509ea threads `llm.api-key` (as `fabric.api_key`). The 2026-10-14 `could not find vendor` failure no longer reproduces.
- [ ] `sb borg eval --inject` reports catch rate per planted class for every judge, with no human labels anywhere, and writes `judge_trust` rows.
  - Observed on main: no `--inject` (`sb borg eval --help`); `config/eval/distill-calibration.yml` absent; the report prints `calibration: UNCALIBRATED - no hand labels; judge unvalidated.`
- [ ] No untrusted judge triggers a remediation or an applied candidate.
  - Observed on main: no `judge_trust` table, no `--remediate`, no `--apply-candidate`.
- [ ] `sb oracle eval --queries <mined.yml> --target answerability` runs over >= 50 of Scott's own past `personal`-scope queries.
  - Observed on main: `rg -c "  - id:" config/eval/queries.yml` -> `9`. ripr's transcripts hold 324 `knowledge_search` calls, 271 distinct queries, in 87 files. `env -u ANTHROPIC_API_KEY sb oracle eval --k 1 --rebuild-cache` exits 1 on the FTS5 `24/7` query.
- [ ] No source is auto-remediated twice, and remediation rows carry lineage.
  - Observed on main: `select count(*) from receipts where replay_of is not null` -> `0`.
- [ ] Each `uv` tool carries a port or drop verdict backed by Phase 5 numbers in Resolved Decisions.
  - Observed on main: no `eval/` directory at the repo root.

## Resolved Decisions

- **2026-10-14: No human review or labels anywhere.** Judge quality comes from planted errors and cross-vendor agreement, with their limits printed in every report. (Scott's direction; the limits were added in panel round 1.)
- **2026-10-14: Python tools run in a `uv` project at `eval/`, and talk to `sb` through a JSONL contract.** Proven ones get ported to Rust. ("if the python code proves very useful, we will port it to rust in sb")
- **2026-10-14: Gemini is the default judge and Anthropic the second, because the distiller is Anthropic.** DeepEval runs on an OpenAI model, so the agreement table spans three vendors. Panel round 1 caught that DeepEval on Anthropic is not an independent vendor.
- **2026-10-14: Keys are mapped per vendor (`llm.api-keys`), at all three judge sites.** Panel round 1 found the single-key mapping covers Anthropic only, and found oracle's judge as a third site.
- **2026-10-14: Lineage is written at intake through the `/ingest` body.** An attempt counts once its intake row exists. Panel round 1 found a post-POST `record_replay` is a no-op (`INSERT OR IGNORE` after the receipt exists).
- **2026-10-14: Scoring is driven by unscored successful receipts, not a time window.** A window plus a sample cap contradicts "every ingest" (panel round 1).
- **2026-10-14: Low-scoring notes stay searchable.** Scott uses the vault for recall, and the companion doc keeps the source as the fallback.
- **2026-10-14: Reports go to a local dir, `sb doctor` and Telegram.** Nothing goes into the vault (no machine spam) or to marquee (work surface).
- **2026-10-14: Prompt changes arrive as a measured candidate plus `--apply-candidate`, not a PR.** The repo ships direct to main. The candidate is measured on two disjoint samples with a coverage floor before Scott sees it, so applying it is an approval, not a note review.
- **2026-10-14: The promptfoo check runs in local `otto ci`, not GitHub CI**, because GitHub holds no judge keys (panel round 1).
- **2026-10-14: Mined queries filter on clyde's resolved `scope` and fail closed**, not on a cwd prefix (panel round 1).
- **2026-10-14: Scoring reads staging, matching the companion doc.** Archive-backed scoring is parked.
- **2026-10-17: An untrusted judge records but never acts** (panel round 1 finding 1, second half). Trust is a per-judge, per-class catch rate from `--inject`, at least 0.80 on every class the judge can see and current to the rubric version. `--remediate` and `--apply-candidate` refuse on untrusted signals and say why. Deterministic checks are ungated. (The metric, sample floor and applicability rule were tightened by round 2, below.)
- **2026-10-17: `eval_scores` keys on `run_kind` and `content_hash`** (finding 15). Baseline, candidate and injected runs of the same trace never collide, matching the existing judgment cache.
- **2026-10-17: Answerability reads sources through a single-trace staged loader, and states its blind spot** (finding 16). Expired traces are counted, not scored. Loss that keeps a note from being retrieved at all is out of scope: measuring it needs retrieval over sources, which nobody asked for.
- **2026-10-17: Remediated sources get a before/after section in the weekly report** (finding 18, folded instead of deferred).
- **2026-10-17: Mined queries filter on clyde `scope: personal`.** The doc said `home`, a value clyde never emits (`export.rs:149-155`). Found on the evidence re-run.
- **2026-10-17 (superseded the same day by round 2): The FTS5 quoting fix rides Phase 1.** `sb oracle eval` exits 1 on main on an unquoted `24/7`, so answerability cannot run without it.
- **2026-10-17: The Phase 1 key criterion is a unit test, not a live call.** The live command already passes on main through fabric's own `.env`, so it proved nothing.
- **2026-10-17: The pattern-dir spike is dropped.** `--patterns-dir` shipped in 69509ea.
- **2026-10-17, panel round 2: trust is a paired catch rate with a sample floor, per applicable class.** 1 of 1 passed the old bar, and a judge that flags everything had catch rate 1.0 (round 2 items 1, 7). The trust metric is now "flags the mutant AND passes its original", at least 0.80 over at least 30 applicable traces. The defaults are policy floors; the report prints the Wilson lower bound. The jev "same bar" justification is dropped: that was precision against gold labels, not synthetic recall.
- **2026-10-17, panel round 2: an applicability matrix replaces "every class is visible".** `judge-claims` sees claim text only (`borg/src/eval.rs:326-327`), so it is measured on number swaps alone. It stays that way because per-claim support is its job (item 5).
- **2026-10-17, panel round 2: coverage needs an error-free current row for every required check** (item 4). Error rows and other judges' rows no longer suppress a retry. One definition of `--unscored`, in one place.
- **2026-10-17, panel round 2: current-row rule, `run_id`, `check_name`, and the distill hash as `content_hash`** (items 3, 8). The JSONL contract carries `run_kind`, `run_id` and `rubric_version`, so `--ingest-verdicts` rebuilds the full key.
- **2026-10-17, panel round 2: the FTS5 fix is a separate targeted fix, landed first, quoted at oracle's call sites** (items 2, 6). Quoting in `search_filter` would break `find_similar`'s constructed OR query. Supersedes the same-day "rides Phase 1" entry.
- **2026-10-17, panel round 2 cheap wins folded:** deterministic failures stay actionable with no trusted judge, and an untrusted factor blocks GEPA rather than silently changing the objective (item 9). The oracle criterion checks judged pairs and zero errors, plus per-site construction tests (item 10). Answerability has four named outcomes and an `inconclusive` state (item 11). Remediated has six outcomes reconciled against receipt status, with a frozen "before" row (item 12). The overall criterion measures the leftover backlog via `--unscored --count` (item 13).
- **2026-10-17, panel round 2 pushback:** the architect's fail -> remediate -> fail loop is already closed. `--remediate` counts auto-remediation rows per source URL inside one transaction, so a remediated trace that fails again finds a count of 1 and is not posted. No change.
- **2026-10-17, panel round 2 defer, folded as one sentence:** answerability skews to notes inside the 14-day staging window. Stated in the report header; the fix is the parked archive-backed scoring.
- **2026-10-17, phase 5: MiniCheck is dropped.** Measured on ripr over the `--inject --sample 30` sample (run `inject-20261017T150659Z`), with MiniCheck `roberta-large` scoring the same 30 originals at the same distill hash:
  - It flags 26 of 30 originals (0.87). `judge-claims` flags 11 (Gemini) and 9 (Claude).
  - Port arm 1 (beats `judge-claims` on number-swap): a check's paired catch rate cannot exceed the share of originals it passes. MiniCheck passes 4 of 30, so its number-swap paired rate is at most 4/26 = 0.15, against `judge-claims` 0.58 (Gemini) and 0.69 (Claude). It cannot win, and no mutant run is needed to show it.
  - Port arm 2 (a signal nothing in Rust has): its signal is per-claim support, which `judge-claims` already produces. It marks 0.50 of 239 claims supported where the two fabric judges mark 0.82 and 0.84. Claim-level kappa against `judge-claims`: 0.26 (Claude), 0.24 (Gemini), while the two fabric vendors agree at 0.91. Trace-level kappa against the four fabric raters: 0.02-0.16.
  - Cost: 2.6-2.9 s per claim on CPU, plus torch in `uv.lock`.
- **2026-10-17, phase 5: DeepEval is dropped.** Same sample, `SummarizationMetric` on `gpt-4o-mini`, pass threshold 0.5:
  - Mean score 0.435. It fails 19 of 30 originals (0.63).
  - Port arm 1 (beats `judge-distillation`): it passes 11 of 30 originals, so its paired catch rate is at most 11/30 = 0.37 on fabricated-quote and 11/26 = 0.42 on number-swap. Both sit below the 0.80 trust floor, and trust needs every applicable class, so DeepEval can never become a check that acts. Its rate on mutants against `judge-distillation` (Gemini 0.27 | 0.50 | 0.18 | 0.65) is unmeasured; the floor settles it first.
  - Port arm 2: its distinct signal is question-based coverage, but the tool records only the combined score, and that score agrees with nothing: trace-level kappa 0.07-0.28 against the four fabric raters, 0.25 against MiniCheck.
  - Consequence for Phase 8: its coverage floor names "DeepEval coverage". With DeepEval dropped, the floor is claims per note and enumeration items, both Rust.
- **2026-10-17, phase 5: Gemini stays the default judge, Anthropic second, and both run by default.** `eval.judge-models` defaults to `[gemini-2.5-pro, claude-sonnet-5]`; the first entry is also `--judge-model`'s default. On the same 30 originals:
  - `judge-claims`: the vendors agree at kappa 0.70 (trace) and 0.91 (claim: 239 claims, 38 unsupported by both, 6 split). The Phase 3 38% flag rate is a signal both vendors share, not Gemini noise. Kappa is agreement, not correctness, so an error both vendors share is not ruled out.
  - `judge-distillation`: kappa 0.20. Claude flags 0.50 of originals, Gemini 0.30.
  - Claude's `judge-claims` number-swap paired rate is 0.69 against Gemini's 0.58. Neither is trusted.
- **2026-10-17, Scott: the `sb-eval` timer scores at most 100 traces a day.** `eval.limit` defaults to 100, not unbounded, so a missing config line cannot trigger a full-backlog run. The 4,281-trace backlog drains over the following weeks.
- **2026-10-17, phase 8: a GEPA run with no winner writes no candidate file.** Four live `distill-video` runs (3 train + 3 holdout traces, budgets 15 to 45) proposed 16 rewrites and none beat the seed. The rewrites tightened the claims rules and cut claims per note 9.67 -> 7.00 (-28%), so the coverage floor zeroed them, which is the floor doing its job. The Phase 8 criterion is amended to the winner case.
- **2026-10-17, decided for Scott (he delegated the call): the GEPA search refuses while its claim judge is untrusted.** `--candidate-split`, the search's first call, carries the same trust gate as `--apply-candidate` (`1abca1c`). Every judge check is untrusted today, so the search does not run until `--inject` measures one trusted.
- **2026-10-17, decided for Scott: the promptfoo check runs only on `borg/patterns/distill-*.md` and is report-only** (`272f0c8`). Other patterns cannot change a distilled note. Its grader is an LLM judge that has not passed the planted-error floor, so failing fixtures are printed and never fail `otto ci`.
- **2026-10-17, decided for Scott: `--remediate --apply` stays off the `sb-eval` timer until a judge check is trusted.** Today only deterministic checks can trigger it (2 of 23 failing traces). Wiring it onto the timer is the follow-up once a judge clears the floor.
- **2026-10-17, finding that bounds this doc: no LLM judge check is trusted.** Best paired catch rate is Claude `judge-claims` on number-swap at 0.69 against the 0.80 floor. Measurement, reporting and deterministic remediation work; judge-driven remediation and prompt candidates are gated off by design. Raising a judge over the floor is the next design doc.
- **2026-10-14: Architect seat pushback.**
  - "judge.rs has no api_key_env, so the citations are hallucinated" is wrong: `api_key_env` is `run_pattern`'s parameter name, and `judge.rs:154,336` pass `""` to it.
  - "Phase 6 is unrequested scope" is demoted: Scott's "data store to RAG and query" ask is what calls for a usefulness metric.

## Alternatives Considered

### Keep human calibration
- **Description:** The existing `--emit-calibration` hand-label flow.
- **Why not chosen:** Scott excluded manual review.

### Ragas instead of DeepEval
- **Why not chosen:** A bare install is broken at 0.4.3, and the metrics duplicate DeepEval's.

### gemini and codex CLIs as judges
- **Description:** Reuse the review-panel seats (`gemini -m gemini-3.1-pro-preview`, `codex exec`).
- **Cons:** They are agentic, heavy and wall-clock capped (`~/.claude/skills/architect/script.sh:224`), the wrong shape for per-claim verdicts.
- **Why not chosen:** fabric reaches the same vendors in one call.

### Hide failing notes via `exclude.stub`
- **Why not chosen:** It trades recall for precision in a store used for recall.

### Run the loop inside the borg daemon after each publish
- **Pros:** Scores within minutes.
- **Cons:** It adds judge latency and cost to the hot ingest path, and couples daemon uptime to judge vendors.
- **Why not chosen:** A daily timer matches the harvest precedent and keeps ingest fast.

### Auto-apply GEPA candidates
- **Description:** The timer writes the winning pattern into the deployed patterns dir.
- **Cons:**
  - `otto deploy` regenerates `~/.config/sb/patterns/` from the repo, so an auto-applied pattern is overwritten on the next deploy.
  - Committing to main from a timer bypasses `otto ci`.
- **Why not chosen:** One command from Scott keeps the repo as the source of truth.

## Technical Considerations

### Dependencies
- `uv` 0.7.21 and Python 3.11.17 on ripr. DeepEval 4.2.9, DSPy 3.4.1 (`dspy.GEPA`) and promptfoo 0.124.1 (npm, node v26.10.0) imported in throwaway probes.
- MiniCheck from the Liyan06 GitHub repo (torch). There is no GPU and no ollama on ripr; the CPU is a 64-thread Threadripper 9970X with 122 GB RAM.
- clyde export (with `scope`) for cross-host session logs. fabric for every judge call.
- Companion doc Phases 6 and 9-11 (see Implementation Plan).

### Performance
- Deterministic pass: 1,361 traces in 3.2 s.
- Judged pass: 16.7 s for 3 traces with 2 fabric calls each. About 40 ingests a day (292 in the 7 days to 2026-10-14) makes about 80 judge calls a day per judge model.

### Security
- Keys come from keep through the timer's secret bootstrap, and each is exported only on the fabric child process. Nothing logs a value.
- Mined queries come only from `personal`-scope sessions, filtered before any external call.
- The `uv` tools receive only staged home content.

### Testing Strategy
- Unit tests named per phase.
- `--inject` is itself the regression harness for every judge and check.

### Rollout Plan
- One commit per phase, `otto ci` green, `bump release`, `otto deploy`.
- `sb bootstrap` installs and enables the timer.
- Operator steps: none, beyond running `--apply-candidate` on a candidate the weekly report lists.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Judge cost grows with ingest volume | Med | Med | Content-hash cache; deterministic checks first; `--limit` carries the backlog over instead of skipping |
| Gemini and Anthropic disagree on most items | Med | Med | Planted-error catch rate decides which judge to trust; disagreement is reported, not averaged |
| A GEPA candidate overfits or games the metric | Med | Med | Coverage floor plus a re-score on a disjoint sample |
| Errors shared across vendors go unseen | Med | Med | Stated in every report header; planted classes grow as weekly clusters reveal new failure types |
| A remediation re-fetch fails on a dead URL | Med | Low | One attempt, counted at intake; then reported |
| A `uv` dependency breaks (as Ragas did) | Med | Low | `uv.lock` pins versions; a broken tool is dropped, not patched around |

## Open Questions

None.

## References

- Companion doc: `docs/design/2026-10-14-note-sources-intent-tools-channels.md`
- Review panel round 1: `~/.cache/review-panel/runs/gNfmuD6i/synthesis.md`
- `sb borg eval` v0.16.0, commit e1db1ec
- Scour 2026-10-14 (this session):
  - Hamel Husain on judges: https://hamel.dev/blog/posts/llm-judge/
  - Pointwise vs pairwise bias: arXiv 2406.12319, 2504.14716
  - Omissions: arXiv 2505.23654, 2608.31016
  - DeepEval summarization: https://deepeval.com/docs/metrics-summarization
  - DSPy GEPA: https://dspy.ai/learn/optimization/optimizers/
  - MiniCheck: https://github.com/Liyan06/MiniCheck
- Sessions df5a3954 (LLM-judge research synthesis) and 07598227 (jev applications)
