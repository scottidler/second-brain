# CLI Shakedown Report: sb v0.19.2

Scope: the surface the CPU-tier inference backend changed (`docs/design/2026-10-27-cpu-tier-inference-backend.md`). Host: `ripr` (Threadripper 9970X, AVX2/FMA/AVX-512), after `otto deploy` of `v0.19.2`. Read-only except `--prefetch-model` (HF cache only).

## Summary

| Metric | Count |
|---|---|
| Commands tested | 13 |
| Passed | 13 |
| Failed | 0 |
| Skipped | 3 (`sb bootstrap`: mutates `~/.config/sb`, already run by the deploy; `sb oracle eval`: spends LLM judge calls, measured in Phase 5; cortex `embed.backend` on the CLI: no `--config` flag, covered by unit tests and the doctor) |
| Pipelines tested | 5 |
| Edge cases tested | 4 |

## Command Results

| Command | Exit | Result |
|---|---|---|
| `sb --version` | 0 | `sb v0.19.2` |
| `sb doctor` | 0 | `[cpu] cpu features: avx avx2 fma f16c avx512f avx512_vnni avx512_bf16 avx_vnni (onnx capable)`; `db pin: bge-small-en-v1.5 (onnx parity: yes)`; `embed backend: onnx (requested auto; ...)`; `oracle backend: onnx (requested auto; ...)`; embedding coverage 4802 / 4802 |
| `sb cortex embed --prefetch-model` | 0 | `Prefetched embedding model bge-small-en-v1.5.`; log `resolved=onnx`, `cache warmed for BAAI/bge-small-en-v1.5` (0.73 s, cached) |
| `sb oracle stats` | 0 | 4802 notes, 116 distinct tags |
| `sb oracle call knowledge_search` `mode=vector` | 0 | 3 hits, top `notes/async-rust-explained-in-20-minutes.md`; ONNX loaded `dim=384 threads=16 token_type_ids=true`; 0.99 s wall including model load |
| same, `mode=hybrid` | 0 | 3 hits, same top 3 |
| same, `mode=bm25` | 0 | 3 hits, no model load (0.74 s) |
| same, no mode | 0 | `"mode":"configured"`, 3 hits |

## Config validation (`sb oracle --config <copy>`)

| `inference:` value | Result |
|---|---|
| `backend: fastembed` | load fails: `inference: unknown inference backend "fastembed"; valid values are: auto, candle, onnx at line 31 column 3` |
| `backend: ''` | load fails |
| `backend: ONNX` | case-insensitive, `resolved=onnx` |
| `backend: Candle` | `resolved=candle`, vector query returns hits |
| `backend: onnx` | `resolved=onnx` |
| `threads: 4` | ONNX session logs `threads=4` |

## Pipeline Recipes

```bash
sb oracle call knowledge_search --json '{"query":"rust async runtime","mode":"vector","limit":3,"detail":"metadata"}' | jq -c '{mode, count, top: [.results[]?.path]}'
# {"mode":"vector","count":3,"top":["notes/async-rust-explained-in-20-minutes.md","notes/think-twice-before-using-async-rust-prime-reacts.md","notes/async-rust-demystified-next-level-performance-many-will-miss.md"]}

sb oracle call knowledge_search --json '{"query":"rust async runtime","limit":3,"detail":"metadata"}' | jq -c 'del(.results)'
# {"count":3,"mode":"configured"}

sb doctor | rg '\[cpu\]'
```

## Edge Cases

- Unknown and empty backend values fail config load with the valid set named.
- Mixed-case backend values parse.
- `sb cortex embed --kind bogus`: exit 1, `unknown embedding kind "bogus"; expected summary | transcript-chunk | claim`.

## Failures & Bugs

- None in the changed surface.
- Suggestion: `sb cortex embed --kind bogus` loads the model (`rss delta=212.84 MB`) before it rejects the kind. Validate `--kind` before the load.

## Observations

- Every oracle call logs one INFO `ORT Build Info` line per process; the per-session BFCArena noise is gone.
- The live `~/.config/sb/cortex.yml` still pins `embed.workers: 2`, so cortex ONNX runs on 2 threads until it is set to 0 (Rollout operator step).

## Deploy checks (`ripr`)

- Migration: `bge-small-en-v1.5-candle|23930` before, `bge-small-en-v1.5|23930` after, 0 rows with `produced_at` after the deploy (a rename, not a re-embed).
- `otto deploy`: `Stopped before install and started after: borg cortex`; both units `active`.
- cortex logs `inference backend: requested=auto resolved=onnx model=bge-small-en-v1.5`.

## Release Validation

- Tag `v0.19.2`: annotated (`git cat-file -t` = `tag`), points at `d5803c6`, which is `origin/main`.
- Release: published, not a draft; first build in `debian:trixie` (release run 40552446681, success).
- Assets: `sb-v0.19.2-linux-amd64.tar.gz` + `.sha256`. Linux amd64 is the only target the workflow builds; no arm64 or macOS assets.
- Binary: `sha256sum -c` OK; the extracted `sb --version` prints `sb v0.19.2`, matching the installed binary.
