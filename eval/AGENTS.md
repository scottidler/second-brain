# eval - Python verdict producers

> Read this before touching `eval/`. The Rust side of the contract is `borg/src/eval/verdicts.rs` and `borg/src/eval/scored.rs`.

## Purpose

A `uv` project beside the Rust code that runs third-party judges (DeepEval, MiniCheck) over `sb borg eval` records and writes verdicts back. The interface is files only: no Python calls Rust and Rust never imports Python. A tool that earns it gets ported into `sb`; one that adds nothing is removed (design: `docs/design/2026-10-14-automated-note-quality-loop.md`, "The `uv` project").

## Contract

- In: `sb borg eval --scored --limit N --format json` writes one `TraceRecord` JSONL line per trace. This side reads `trace`, `run_kind`, `run_id`, `rubric_version`, `content_hash`, `source`, `note_text`, `claims[{id, text, quote?, who?}]`.
- Out: one verdict line per `{trace, run_kind, run_id, rubric_version, content_hash, tool, tool_version, model, claim_id?, metric, value, detail?}`. The five identity fields are copied from the record judged. `verdict.contract.VERDICT_KEYS` is the full key set; `sb` rejects any other key.
- Load: `sb borg eval --ingest-verdicts <file>` rebuilds the `eval_scores` key with `check_name = tool`, `judge_model = model`, folds one tool's verdicts per key into one row (note-level metrics under `metrics`, per-claim under `claims.<id>`), and drops a verdict whose `content_hash` is stale, naming the trace, then exits non-zero.

## Layout

- `verdict/contract.py`: read records, build and write verdicts.
- `verdict/tools/deepeval.py`: `SummarizationMetric` on an OpenAI model (`OPENAI_API_KEY` in the environment).
- `verdict/tools/minicheck.py`: MiniCheck `roberta-large` on CPU, one probability per claim. Needs `NLTK_DATA` holding `tokenizers/punkt_tab` (`bin/eval-nltk` provides it).
- `verdict/__main__.py`: `python -m verdict --records in --out out`; a tool failure on one trace is a stderr line and a non-zero exit, never a silent skip.
- `tests/`: pytest with fakes for the scorers and the metric; no model, no network.

## Running

- `otto eval-py`: export records, judge, write verdict JSONL, ingest. Pulls torch and calls OpenAI, so it is not part of `otto ci`.
- `otto eval-test`: the pytest suite, through `uv run`.

## Invariants

- Dependencies come from `uv add`; `uv.lock` pins them. torch is the CPU wheel from the `pytorch-cpu` explicit index.
- MiniCheck is the Liyan06 GitHub repo pinned by `rev`, not the PyPI name.
- Model weights and NLTK data live in `~/.cache/sb/eval`, never in the repo.
- Never print or write an API key.
