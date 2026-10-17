# Handoff: claim judging over the full source (the 24k window)

Branch: main

## Next action

Write a design doc with `/create-design-doc`: **judge each claim against the part of the source it came from, instead of the first ~24k chars.** Goal: make Claude's `judge-claims` measure trusted (paired catch >= 0.80 on >= 30 applicable traces), which unlocks the judge-gated remediation and prompt candidates that v0.18.0 ships switched off. Scott asked for this doc on 2026-10-17; he has not yet reviewed a draft.

## Read first, in order

1. `docs/design/2026-10-14-automated-note-quality-loop.md`: the loop this extends. Resolved Decisions (bottom) has the trust floor, the phase 5 kappa numbers, and "no LLM judge check is trusted".
2. `docs/design/2026-10-14-automated-note-quality-loop-implementation-notes.md`: per-phase deviations; Phase 3 (trust rule), Phase 5 (kappa), Phase 8 (GEPA no-winner), audit fixes.
3. `borg/src/eval/judge.rs:103-123`: `JUDGE_INPUT_MAX_CHARS = 24_000`, `distillation_input` (note first, then source), `source_budget`.
4. `borg/src/eval.rs:558-570`: `judged_input` cuts the source to the budget (`truncate_source`). Both judges get this cut source.
5. `vault/src/fabric.rs:162-171,247`: fabric's stdin head-truncation, the reason the budget exists.
6. `borg/src/eval/trust.rs` (`judge_check_trust`) and `borg/src/config/eval.rs` (`TrustConfig`: `min-paired-catch-rate` 0.80, `min-samples` 30).
7. `borg/src/eval/inject.rs`: the planted-error run that measures trust.

## What we found (2026-10-17, ripr, v0.18.0 data in `~/.local/share/sb/borg/eval-cache.db`)

**Root cause of the low trust: truncation, not judge quality.** The distiller chunks long sources (chunk/reduce patterns), so claims come from the whole source. The judge sees only the head (~24k chars minus the note).

- 30 traces judged by both `gemini-2.5-pro` and `claude-sonnet-5` (`judge-claims`, baseline rows from `--inject` run `inject-20261017T150659Z`).
- Claims both judges marked not-supported:
  - Sources <= 24k chars: 23 traces, 173 claims, **2 flagged (1.2%)**.
  - Sources > 24k chars: 7 traces (33,753 to 614,737 chars), 66 claims, **36 flagged (55%)**.
- Agentic re-check of all 38 jointly flagged claims against the FULL source (Sonnet agents, each verdict cites a verbatim excerpt with its offset):
  - **29 supported, 9 partial, 0 unsupported, 0 contradicted.**
  - 9 flagged claims carry a `quote` found verbatim in the source past the judge's window.
  - Example: `ht-a40d79d8` claim 10 ("about 430 lines, two hours") is stated at the video's end; both judges called it contradicted because the intro says "about 500 lines".
  - The 9 partials are real distiller overstatements (wrong count, invented name "Vehagen", conflated attribution). Across 239 claims that is ~4% overstated detail, 0 fabricated.
- Paired catch rate on number-swap (`judge-claims`), recomputed from `eval_scores` (reproduces the official 0.58 / 0.69 exactly):

  | judge | all (n=26) | source <= 24k (n=20) | source > 24k (n=6) |
  |---|---|---|---|
  | claude-sonnet-5 | 0.69 | **0.90** | 0.00 |
  | gemini-2.5-pro | 0.58 | 0.75 | 0.00 |

  Originals flagged: Claude 0.10 on fitting sources vs 1.00 on oversized; Gemini 0.20 vs 1.00.
- `judge-distillation` is weak for a different reason: its rubric has no claim-accuracy axis (paired 0.07-0.65). Out of scope unless the doc chooses to fix it.

## What the doc must decide

- **How a claim finds its evidence in a long source.** Candidates: the claim's own `quote` located verbatim; retrieval of the best-matching chunk per claim (BM25 or the existing `bge-small` embeddings in `vault::search`); judging per chunk. The distiller's own chunk boundaries are not persisted today (check `borg/src/stages/` before assuming).
- **Interim rule:** a trace whose source exceeds the window is "not judgeable", not "failing", until the new path lands (affects `--unscored` coverage, `--remediate`, `--weekly`, `judge_trust`).
- **Re-measure trust:** `--inject` with >= 30 applicable traces per class (number-swap had 26; who-change 17). `--sample N` does not mean N per class today.
- **Rubric versions bump** (`borg/src/eval/cache.rs` `CLAIMS_RUBRIC_VERSION`, currently `v2`) so old trust rows stop counting.
- Whether the oracle relevance judge and answerability (`sb oracle eval --target answerability`, 297 of 579 sources hit its 22k cap) get the same treatment.

## State at handoff

- main HEAD `c2699e3` (Bump version to v0.18.0), tagged `v0.18.0`, pushed. Deployed on ripr: `sb --version` and borg `/health` both report v0.18.0.
- Commits that matter: `0ea0147`..`e51f34a` (Phases 1-9), `272f0c8` (promptfoo report-only), `1abca1c` (GEPA refuses while untrusted), `0e05aba` (eval.limit 100), `96f73be` (audit fixes: empty judge model fails closed, frozen remediation before-score).
- Timers enabled: `sb-eval.timer` (05:00 daily, 100 traces), `sb-eval-report.timer` (06:00 Sunday). Note: `otto deploy` runs bootstrap with `--skip-systemd`; `sb bootstrap` installs timers.
- **Session-scoped, may be gone:** a manual `sb-eval.service` run started 2026-10-17 14:42 PDT; its rows land in `eval_scores` only after each judge model finishes all 100 traces. Check: `sqlite3 ~/.local/share/sb/borg/eval-cache.db "select judge_model, check_name, count(*), sum(error is not null) from eval_scores where run_kind='baseline' and scored_at > '2026-10-17T21:42' group by 1,2"`.
- **Session-scoped evidence files** (on the SSD scratchpad, not in the repo; may be cleaned): `/media/saidler/samsung-4tb-ssd/claude-tmp/claude-1000/-home-saidler-repos-scottidler-second-brain/3d292b36-e1f1-49cd-9759-13240970fffb/scratchpad/claims/` holds `batch-{A,B,C,D}-verdicts.json` (the 38 cited verdicts), `extract.py`, `window.py`, `paired.py`. The numbers above are the durable record.

## Probes (re-run before trusting any claim here)

- Trust state today: `sb borg eval --remediate` (dry run; prints each gate reason) or `sb doctor` (eval line).
- Source lengths of judged traces: the `source` of a staged trace is `distilled.yml`'s `transcript`, else `transcript.md` / `fetched.html` / `body.txt` under `~/.local/share/sb/borg/stages/<trace>/` (`borg/src/eval/fixtures.rs` `staged_source`).
- Verdicts per claim: `eval_scores.metrics_json` (`{"verdicts":[...]}`) for `check_name='judge-claims'`.

## Suggested skills

- `create-design-doc` (then `review-panel`, then `how-to-execute-a-plan`).
- `session-recall` if the 2026-10-14 design session is needed (Scott's original ask is quoted in the loop doc's Background).
