"""Verdict producers for `sb borg eval`.

Interface is files only. `sb borg eval --format json` writes one JSONL record per
trace; each tool here reads those records and writes verdict JSONL back; `sb borg
eval --ingest-verdicts` loads it into `eval_scores`. See `eval/AGENTS.md`.
"""
