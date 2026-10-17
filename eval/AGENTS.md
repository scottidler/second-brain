# eval - prompt-candidate search and the promptfoo check

> Read this before touching `eval/`. The Rust side is `borg/src/eval/candidate.rs` (split, scoring, coverage floor, `candidate` rows) and `borg/src/eval/candidate/file.rs` (the candidate file and `--apply-candidate`).

## Purpose

Two tools beside the Rust code. Neither decides anything on its own (design: `docs/design/2026-10-14-automated-note-quality-loop.md`, "Prompt improvement"):

- `propose/` (uv project `sb-eval`): a GEPA search (`gepa`, the library DSPy's `dspy.GEPA` wraps) over ONE distill pattern. It only proposes text. Every score comes from `sb borg eval --candidate-pattern`, so the metric and its coverage floor have one definition, in Rust.
- `promptfoo/` (npm, `promptfoo` pinned exactly in `package.json` + `package-lock.json`): the config `bin/promptfoo-check` runs from `otto ci` when a `borg/patterns/distill-*.md` differs from `origin/main`.

DeepEval and MiniCheck were dropped on their measured numbers (design doc, Resolved Decisions), along with torch, the NLTK fetch, `otto eval-py` and the verdict ingest.

## Contract with `sb`

- `sb borg eval --candidate-split N --pattern P`: JSON `{pattern, kind, eligible, train, holdout, fabric-env}`, two disjoint stable samples of staged traces whose distill reaches P's single-call path. `fabric-env` maps fabric's var (`GEMINI_API_KEY`) to borg.yml's `llm.api-keys` reference for it (an env var name or a file path, never a value); `reflect.child_env` resolves each one onto the reflection child only, as `vault::fabric::command` does for the Rust judges.
- `sb borg eval --candidate-pattern FILE --pattern P --traces ... --format json [--patterns-dir DIR]`: JSON `SampleScore` (kebab-case keys). Per trace: `score` (this note against its own baseline note, null when an arm errored), `feedback`, `errors`, `candidate-note`, `source-head`. Per sample: `baseline` and `candidate` objectives, present only when `complete`. Writes `candidate` rows to `eval_scores` and caches distills in `eval-cache.db`.
- Add `--holdout ... --write-candidate [DIR]` to re-score the winner on the holdout and write `<DIR>/<pattern>-<date>.md` (default `~/.local/share/sb/borg/eval-reports/candidates`).

## Layout

- `propose/__main__.py`: `python -m propose --pattern P [--sample N] [--budget N]`. Split, `gepa.optimize` (train is both trainset and valset), then the candidate file when the best is not the current pattern. Refuses at the split while the claim-support judge check is untrusted: `sb borg eval --candidate-split` carries the same trust gate as `--apply-candidate`.
- `propose/sb.py`: the three `sb` calls; a non-zero exit is an `SbError` carrying stderr.
- `propose/adapter.py`: the `GEPAAdapter` (one component, the pattern text); an errored trace scores 0 for the search.
- `propose/reflect.py`: the custom proposer. One `fabric -m <model>` call per rewrite, with the split's `fabric-env` keys exported on that child, the new pattern taken from `<pattern>...</pattern>`: the patterns carry their own ``` fences, which GEPA's fence extractor would cut.
- `promptfoo/promptfoo.yml`: the `echo` provider over each regenerated note, `llm-rubric` and `context-faithfulness` graded by `google:gemini-2.5-pro`.
- `tests/`: pytest with fake runners (no sb, no fabric, no network), plus the run/skip rule of `bin/promptfoo-check` against throwaway git repos.

## Running

- `otto gepa --pattern distill-video --sample 4 --budget 40`: calls the distiller and the judges, so it is not in `ci`. Env `SB`, `SB_BORG_CONFIG`.
- `otto eval-test`: the pytest suite, in `ci`.
- `otto promptfoo` (`bin/promptfoo-check`), in `ci`, report only: skips unless a `distill-*` pattern differs from `origin/main`; failing fixtures are printed, never fail `ci`, because the grader is an untrusted judge. Running it needs `GEMINI_API_KEY` and borg.yml's distiller key; without the Gemini key it prints NOT RUN.

## Invariants

- Python dependencies come from `uv add`; `uv.lock` pins them. The promptfoo version comes from `npm install --save-exact`.
- Nothing here writes `borg/patterns/`. That is `sb borg eval --apply-candidate`, gated on judge trust.
- Never print or write an API key.
