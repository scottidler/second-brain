# Design Document: CPU-Tier Inference Backend (Candle stays, ONNX preferred when the CPU can run it)

**Author:** Scott Idler
**Date:** 2026-10-27
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

`sb` embeds and reranks on Candle with no AVX2 floor, a choice made for the Sandy Bridge-era `desk` (now `talos`). The daemon host is now `ripr`, a Threadripper 9970X with AVX2, FMA and AVX-512. This doc compiles a second backend, ONNX Runtime called directly through `ort`, into the same binary. At startup the process detects the CPU and prefers ONNX when it can run it, and falls back to Candle when it can't. The vectors are identical (measured), so the switch is a pin rename, not a re-embed.

## Problem Statement

### Background

- `docs/design/2026-05-17-candle-embedding-backend.md` picked Candle because pyke's bundled ONNX Runtime SIGILLs on a CPU without AVX2. That doc's tiers (native target, MKL, fastembed) were meant to be compile-time upgrades.
- The fastembed tier has been unreachable from the shipped binary since that doc landed:
  - cortex, oracle and sb all depend on `vault/vec`, and `vec = ["vec-candle"]` (`vault/Cargo.toml`)
  - enabling `vec-fastembed` alongside it hits the `compile_error!` in `vault/src/embedding.rs`
- The daemons moved to `ripr`. `talos` still runs oracle locally through `.mcp.json`, so the old path is live:

| host | CPU | avx2 | fma | avx512f | role |
|---|---|---|---|---|---|
| `ripr` | Threadripper 9970X, 64 threads | yes | yes | yes | borg + cortex daemons, oracle |
| `talos` | Sandy Bridge era, 32 threads | no | no | no | oracle (local, on demand) |
| `lappy` | Ryzen 7 PRO 7840U, 16 threads | yes | yes | yes | oracle (local, on demand) |

### Problem

- Embedding on `ripr` runs Candle on 2 threads of 64:
  - live `~/.config/sb/cortex.yml` pins `embed.workers: 2`, a value justified by desk timings
  - the 2026-10-27 00:30 embed tick took 166 s for 498 chunks, ~0.33 s per chunk
  - gemm already picks its FMA kernel at runtime (`gemm-common` `gemm.rs`), so the instruction set is not the main limit today; the thread count and the backend are
- The rerank stage is off (`retrieval.rerank.enabled: false`) because Candle costs ~150-300 ms per pair on an AVX-only CPU. The stage was built and has never been evaluated on a CPU that can run it.
- Backend choice is compile-time and exclusive. A binary that runs on `talos` can never use the faster path on `ripr`.
- The backend is baked into the model identity (`bge-small-en-v1.5-candle`), so any backend switch triggers a full re-embed of ~24K rows.

### Measured (Phase 0 spike, already run, `ripr`, 2026-10-27)

Python `onnxruntime` 1.31.1 (the shipped binary links pyke ORT 1.24.2, pinned by `ort-sys` 2.0.0-rc.12 `dist.txt`; superseded: `cargo add` resolves rc.15 / ORT 1.31.0, see the Phase 0 subsection below) on the same HF repo the Candle backend downloads (`BAAI/bge-small-en-v1.5`, file `onnx/model.onnx`). CLS pool + L2 norm, max length 512, 64 chunks of 512 tokens, batch 16.

| model file | max cosine distance vs `bge-reference.json` | s/chunk @2 threads | @8 | @16 | @32 | @64 |
|---|---|---|---|---|---|---|
| `BAAI/.../onnx/model.onnx` (fp32) | 1.0e-8 | 0.0804 | 0.0380 | 0.0237 | 0.0215 | 0.0488 |
| `Xenova/.../onnx/model.onnx` (fp32) | 1.0e-8 | 0.0802 | 0.0345 | 0.0233 | 0.0213 | 0.0361 |
| `Xenova/.../onnx/model_quantized.onnx` (int8) | 3.1e-3 | 0.0495 | 0.0254 | 0.0208 | 0.0188 | 0.0335 |
| `Xenova/.../onnx/model_int8.onnx` (int8) | 2.4e-2 | 0.0535 | 0.0270 | 0.0200 | 0.0172 | 0.0316 |

What this settles:
- fp32 ONNX matches the `sentence-transformers` reference to 1e-8. The existing Candle parity test requires < 1e-3. Same weights, same tokenizer, same pooling: the vectors are interchangeable.
- ONNX at 16 threads is ~14x the current tick rate (0.024 vs 0.33 s/chunk).
- 64 threads is 2x slower than 32 (SMT oversubscription). Inference threads must be bounded. This rules out fastembed, which hardcodes `with_intra_threads(available_parallelism())`.
- int8 buys 10-20% at 16-32 threads and breaks parity (3e-3 to 2.4e-2). Rejected; see Addendum.
- `ms-marco-MiniLM-L6-v2` (rerank), `bge-small-en-v1.5` and `bge-base-en-v1.5` all ship `onnx/model.onnx` and `tokenizer.json` in the same HF repos the Candle backend already uses (HF API, 2026-10-27). The code's older repo id `cross-encoder/ms-marco-MiniLM-L-6-v2` redirects to `cross-encoder/ms-marco-MiniLM-L6-v2`.

### Measured (Phase 0, Rust ORT dual binary, 2026-10-27)

Scratch branch `cpu-tier-phase0-spike` (commit `d361970`, never merged). Spike code: `vault/src/embedding/onnx.rs` plus a hidden `sb onnx-spike` subcommand.

**Build.** On `ripr`, baseline x86-64 (no `target-cpu` anywhere), rustc/cargo 1.99.1, `cargo build --release -p sb@0.19.1`, then `scp` to `talos:~/sb-phase0`. Binary sha256 `5a91bbb46d0a4f811f55a472c9f91bffc869e243b69ecd2a9f7f1cdfad81c8b8` on both hosts.
- `cargo add ort --optional` resolved `ort` / `ort-sys` **2.0.0-rc.15**, not rc.12. rc.15 bundles **ORT 1.31.0** (`ort::info()`: `ORT Build Info: git-branch=rel-1.31.0, git-commit-id=402979a, fp8-kv-cache=1, build type=Release`; `ort-sys` `build/download/dist.tsv` points at `ms@1.31.0`). The 1.24.2 pin above belongs to rc.12, which fastembed pins with `ort = "=2.0.0-rc.12"`.
- rc.15 and fastembed's `=rc.12` cannot co-resolve (`cargo add` fails: "failed to select a version for `ort`"), so the scratch branch dropped the `fastembed` dependency. Phase 1 deletes it for real, so Phase 2 lands on rc.15 / ORT 1.31.0.
- `vec = ["vec-candle", "vec-onnx"]` with the `compile_error!` removed. ORT is statically linked: `ldd target/release/sb` lists only `libstdc++`, `libgcc_s`, `libm`, `libc`.

**`talos`** (Genuine Intel 3.10 GHz, 32 threads, `avx` yes, `avx2`/`fma`/`avx512f` no). Run with `XDG_DATA_HOME=/home/saidler/sb-phase0-data`, a `cp -a` copy of `~/.local/share/sb`, so the scratch binary (main is v0.19.1; talos's installed `sb` is v0.15.18) never opened the live `oracle.db`.

| command | exit | result |
|---|---|---|
| `~/sb-phase0 --version` | 0 | `sb v0.19.1-8-g23ab974` |
| `~/sb-phase0 doctor` | 1 | every section ran to completion, no fault; 2 error findings, both host state: user@ `TimeoutStopUSec` 5s < 210s, and "4 of 30 patterns missing". Installed `sb doctor` (v0.15.18) on the same host also exits 1 (5 errors, its parser rejects the live `jev` classifier) |
| `~/sb-phase0 oracle call knowledge_search --json '{"query":"rust async runtime","mode":"vector","limit":5,"detail":"metadata"}'` | 0 | `"mode": "vector"`, `"count": 5`; top hit `notes/async-rust-explained-in-20-minutes.md`; log: `CandleBertModel::load_version: loaded BAAI/bge-small-en-v1.5 version=bge-small-en-v1.5-candle dim=384 workers=8` |
| `~/sb-phase0 -l warn onnx-spike --probe --reference ~/sb-phase0-bge-reference.json` | 132 (SIGILL) | ort printed `WARNING: This CPU does not support AVX2, which is required by ort's prebuilt ONNX Runtime binaries.`, then faulted inside session build |

- Kernel: `traps: sb-phase0[3719496] trap invalid opcode ip:611f0f5f027a ... in sb-phase0[594d27a,...]`. File offset `0x594d27a` = vaddr `0x594e27a` = `vpbroadcastq %xmm0,%ymm0` (AVX2) in `onnx::NodeProto::NodeProto(google::protobuf::Arena*)`, ORT's protobuf model parse during `commit_from_file`.
- So the static archive does not fault at process load. It faults the first time ORT parses a model, which is after any `CpuFeatures` gate would run. The structural guard in `OnnxEmbedder::load` is sufficient; `load-dynamic` is not needed.

**`ripr`** (Threadripper 9970X, 64 threads, load average 7 to 14 from the live daemons during the run).

```
./target/release/sb -l warn onnx-spike --corpus docs/design \
  --reference vault/tests/fixtures/bge-reference.json --candle \
  --threads 2 8 16 32 64 --repeats 5
```

`BAAI/bge-small-en-v1.5` `onnx/model.onnx` (fp32), CLS pool + L2 norm, truncation 512. Corpus: the first 64 `docs/design/*.md` files (sorted) that tokenize to the full 512 tokens, batch 16. One session per thread count, `intra_threads = N`, `inter_threads = 1`, `GraphOptimizationLevel::Level3`; one warmup batch, then 5 timed passes over all 64 chunks (tokenize + run + pool).

| threads | session init | 5 passes (s, 64 chunks) | median s/chunk | best s/chunk | Python spike s/chunk |
|---|---|---|---|---|---|
| 2 | 0.075 s | 5.256 5.406 5.298 5.286 5.295 | 0.0827 | 0.0821 | 0.0804 |
| 8 | 0.076 s | 2.374 2.277 2.210 2.280 2.264 | 0.0356 | 0.0345 | 0.0380 |
| 16 | 0.075 s | 1.640 1.619 1.674 1.735 1.607 | 0.0256 | 0.0251 | 0.0237 |
| 32 | 0.075 s | 1.431 1.413 1.412 1.423 1.410 | 0.0221 | 0.0220 | 0.0215 |
| 64 | 0.076 s | 1.478 1.470 1.468 1.460 1.457 | 0.0229 | 0.0228 | 0.0488 |

- Parity vs `bge-reference.json`: max cosine distance `2.749e-13`, norms `[0.99999994, 1.0, 0.99999994]`.
- ONNX vs Candle on the same 64 x 512-token chunks: max cosine distance `3.612e-13` (Candle at 8 workers: 0.1235 s/chunk).
- 16 threads: Rust 0.0256 vs Python 0.0237 = 1.08x, inside the 1.5x gate.
- 64 threads in Rust is 4% slower than 32, not 2x. The Python oversubscription cliff does not reproduce here; 16 -> 32 still buys 14%. Phase 2 owns the default thread count.

### Measured (Phase 2, `OnnxEmbedder` default threads, `ripr`, 2026-10-27)

`OnnxEmbedder::load_version("bge-small-en-v1.5", N, &CpuFeatures::detect())` through `EmbeddingModel::embed_batch`, release build, a throwaway `#[ignore]` harness (deleted after the run). Corpus: the first 64 `docs/design/*.md` files (sorted) longer than 6000 bytes, each truncated to 512 tokens; batch 16; one warmup batch, then 5 timed passes over all 64 chunks (tokenize + run + CLS pool + L2). The live daemons held load average 3 to 5 before each run. Two runs back to back:

| threads | run 1 median s/chunk | run 1 best | run 2 median s/chunk | run 2 best |
|---|---|---|---|---|
| 2 | 0.0800 | 0.0793 | 0.0785 | 0.0784 |
| 8 | 0.0386 | 0.0380 | 0.0344 | 0.0341 |
| 16 | 0.0270 | 0.0264 | 0.0272 | 0.0254 |
| 24 | 0.0244 | 0.0242 | 0.0247 | 0.0235 |
| 32 | 0.0235 | 0.0229 | 0.0234 | 0.0226 |
| 48 | 0.0216 | 0.0214 | 0.0228 | 0.0217 |
| 64 | 0.0230 | 0.0226 | 0.0238 | 0.0228 |

- The knee rule: the largest thread count whose doubling from N/2 still bought at least 20%. 8 -> 16 bought 21-30%; 16 -> 32 bought 13-14%; 32 -> 64 lost 2%. Past 16, each doubling costs twice the threads for a seventh of the time.
- `DEFAULT_ONNX_THREADS = 16`, unchanged from the plan. 32 would buy ~13% per tick and break the thread budget: cortex governance 12 + cortex ONNX 32 + oracle ONNX 32 = 76 of 64 when a query lands during a tick. `lappy` has 16 logical CPUs, so the default is also capped at available parallelism.
- Same set, ONNX vs Candle (`embedding::onnx::tests::onnx_matches_candle_real_model`, 6 mixed-length texts including one past 512 tokens): max cosine distance `4.8e-7`.
- ORT log volume: with the forwarder dropping INFO/VERBOSE, a full run logs one `vault::onnx` INFO line (the ORT build string) and one `OnnxEmbedder::load_version` INFO line per session, and nothing under the `ort` target.

### Measured (Phase 5, rerank eval, `ripr`, 2026-10-27)

`sb oracle eval` (lift target, 9 queries in `config/eval/queries.yml`, K=10, fabric default judge), release build of main at `7ac4e2e` plus the Phase 5 tree (`sb v0.19.1-12-g7ac4e2e`). Run against a `sqlite3 .backup` copy of the live `oracle.db` and `eval-cache.db` under `XDG_DATA_HOME=<scratch>`, inside `bwrap --tmpfs ~/.local/share/sb` so the live data dir was invisible to the process; `strace` showed the only `oracle.db` it opened was the copy. The live `oracle.yml` was copied twice: unchanged (rerank off) and with `retrieval.rerank.enabled: true` (rerank on). No `inference:` block, so both resolved `auto` -> ONNX at 16 intra-op threads for query embedding and, in the rerank-on run, for the cross-encoder (`inference backend: requested=auto resolved=onnx model=ms-marco-MiniLM-L6-v2`, `OnnxCrossEncoder::load: ... threads=16`). Latency is per-query wall clock of retrieval only, nearest-rank, after one untimed warmup query; budget `latency-budget-ms` = 1500 (default).

Rerank off:

| target | nDCG@10 | Recall@10 | MRR | p50 ms | p95 ms | rerank |
|---|---|---|---|---|---|---|
| bm25 | 0.4416 | 0.3338 | 0.7222 | 1.2 | 2.2 | - |
| vector | 0.7928 | 0.7507 | 0.8333 | 32.0 | 35.9 | - |
| hybrid | 0.7585 | 0.6843 | 0.8889 | 33.3 | 36.7 | - |
| graph | 0.3889 | 0.3678 | 0.4444 | 326.6 | 409.3 | - |
| graph-hybrid | 0.5948 | 0.4625 | 0.8426 | 326.6 | 407.9 | - |
| graph-hybrid (no fact) | 0.5948 | 0.4625 | 0.8426 | 376.4 | 433.0 | - |
| configured | 0.7928 | 0.7507 | 0.8333 | 32.8 | 34.3 | off |

Rerank on:

| target | nDCG@10 | Recall@10 | MRR | p50 ms | p95 ms | rerank |
|---|---|---|---|---|---|---|
| bm25 | 0.4174 | 0.2427 | 0.7222 | 1.6 | 2.7 | - |
| vector | 0.6982 | 0.4728 | 0.8333 | 34.7 | 36.8 | - |
| hybrid | 0.6701 | 0.4314 | 0.8889 | 34.5 | 37.3 | - |
| graph | 0.3484 | 0.2339 | 0.4444 | 349.1 | 399.1 | - |
| graph-hybrid | 0.5438 | 0.3058 | 0.8426 | 355.0 | 402.9 | - |
| graph-hybrid (no fact) | 0.5438 | 0.3058 | 0.8426 | 379.2 | 465.0 | - |
| configured | 0.8482 | 0.6016 | 1.0000 | 400.1 | 721.4 | ran 9/9 |

- Rerank ran: yes, on 9 of 9 queries (no WARN, the probe never latched it off).
- The judged pool is the union of every target's top 10, so turning rerank on adds notes to the pool (187 -> 236 judgments) and re-bases every row's Recall and ideal DCG. Compare within one run, not across runs: the off-run `vector` and `configured` rows are identical, because the shipped pipeline is vector-only plus the stub exclude.
- Same pool (rerank-on run): configured with rerank 0.8482 nDCG vs vector 0.6982 (+0.150); Recall 0.6016 vs 0.4728 (+0.129); MRR 1.0000 vs 0.8333.
- Cost: configured p50 400 ms, p95 721 ms with rerank vs 33 / 34 ms without (input-k 50 pairs, one batched forward). p95 is 48% of the 1500 ms budget.
- With 9 queries, nearest-rank p95 is the slowest query.
- Judge calibration held in both runs: boundary P/R 0.83/0.91 (off), 0.80/0.92 (on), both TRUSTWORTHY.
- Defaults unchanged: `rerank.enabled` stays `false` in the template. Turning it on in `ripr`'s live `oracle.yml` is Scott's call.

### Goals

- Keep the Candle path, unchanged in behavior, for any CPU without AVX2+FMA (Scott: "maintain the old path").
- One `sb` binary carries both backends. The process detects the CPU at runtime and prefers ONNX when it can run it (Scott: "detect and prefer the new path").
- The ONNX path uses every instruction set the CPU has (Scott: "ALL of the modern instruction sets"). ONNX Runtime's MLAS layer picks its kernels at runtime (SSE, AVX2, FMA3, AVX-512F, AVX-512 Core, AVX-512 VNNI, AVX-VNNI kernels are all present in the pyke static archive), so `sb` does no ISA dispatch of its own.
- Use the cores (Scott: "power of the threadripper"): inference thread count is configured, bounded, and defaults to the measured knee, not to 2.
- Switching backends never mixes incompatible vectors and never forces a re-embed when the vectors are proven identical.
- The rerank stage gets an ONNX implementation and eval numbers (quality and latency) that Scott can enable it on. Building and evaluating it is in scope; enabling it is his call.

### Non-Goals

- **Excluded:** `-C target-cpu=native` or any per-host compile target. Scott asked for runtime detection; a native binary SIGILLs on `talos`.
- **Excluded:** speeding up Candle itself (gemm `x86-v4`, MKL). Candle is the old path and stays as it is.
- **Excluded:** int8 / quantized models. Measured: small gain, parity broken.
- **Parked:** local Whisper in `bin/meeting` / `bin/dictate` (both call Groq "because desk is AVX-only"). Revisit when Scott wants transcription off Groq; that is a cost and privacy decision, not a CPU one. This doc only fixes their stale comments.
- **Parked:** ONNX for `bge-base-en-v1.5`. It has no reference fixture, so no parity proof. It stays Candle-only until someone adds a fixture.
- **Excluded:** GPU execution providers. No GPU in the inference path.

## Proposed Solution

### Overview

- Replace the dead `vec-fastembed` backend with `vec-onnx`: `ort` called directly, plus the `tokenizers` and `hf-hub` crates the Candle backend already uses. Same HF repos, same tokenizer, same CLS pool.
- `vec = ["vec-candle", "vec-onnx"]`. Drop the `compile_error!`. Both backends compile into every `sb`.
- A pure function resolves the backend from config plus detected CPU features: `auto | candle | onnx`.
- Model identity loses the backend suffix. A backend may serve a model id only if a parity test proves it within 1e-3 cosine of that model's reference fixture.
- A one-shot migration renames `bge-small-en-v1.5-candle` -> `bge-small-en-v1.5` in the pin and the rows.
- ONNX gets a cross-encoder behind the existing `Reranker` port. Its default is set by `sb oracle eval`.
- `sb doctor` gains a `cpu` section: detected features, resolved backend, DB pin, mismatch warnings.

### Architecture

```
            cortex.yml embed.backend / oracle.yml inference.backend
                               |
CpuFeatures::detect() --> resolve_backend(requested, features) --> Backend::{Candle, Onnx}
                               |                                        |
                               v                                        v
          vault::embedding registry: Arc<dyn EmbeddingModel>   vault::search::rerank: Box<dyn Reranker>
               |                     |                              |                    |
          CandleEmbedder       OnnxEmbedder                CandleCrossEncoder     OnnxCrossEncoder
          (unchanged)          (ort Session,               (unchanged)            (ort Session,
                                intra_threads = N)                                 intra_threads = N)
```

- **CPU probe:** `vault::cpu::CpuFeatures { avx, avx2, fma, f16c, avx512f, avx512_vnni, avx512_bf16, avx_vnni }`, filled by `is_x86_feature_detected!`. `onnx_capable()` is `avx2 && fma`. The other flags are reported by doctor only.
- **Resolution** (`vault::embedding::resolve_backend(requested: BackendChoice, cpu: &CpuFeatures, model: &str) -> Result<Backend>`), pure and table-tested:

| requested | `onnx_capable()` | model has an ONNX parity proof | result |
|---|---|---|---|
| `auto` | yes | yes | `Onnx` |
| `auto` | yes | no | `Candle`, INFO naming the model |
| `auto` | no | any | `Candle`, INFO naming the missing features |
| `candle` | any | any | `Candle` |
| `onnx` | no | any | **error**: names the missing features; no fallback |
| `onnx` | yes | no | **error**: names the model; no fallback |
| `onnx` | yes | yes | `Onnx` |

- **The SIGILL guard is structural:** `OnnxEmbedder::load` and `OnnxCrossEncoder::load` take a `&CpuFeatures` and return `Err` unless `onnx_capable()`. That runs before any `ort` call. The 2026-05-17 incident shows the fault fires at session init, not process load; Phase 0 re-proves that for the dual binary on `talos`.
- **Threads:** ONNX sessions are built with `intra_threads = N`, `inter_threads = 1`, and graph optimization `Level3`. ORT's thread pool lives outside rayon and N bounds it. Tokenization (`tokenizers::encode_batch`) does run on rayon, so ONNX calls also go through `embed::in_inference_pool`, exactly like Candle; that keeps cortex's governance isolation (`cortex/AGENTS.md`) whole. One `Session` per loaded model behind a `Mutex`: `ort` 2.0.0-rc.12 `Session::run` takes `&mut self`, and one session with N intra threads is the parallelism anyway.
- **Load failure under `auto`:** not a silent fall back to Candle. It degrades exactly as a Candle load failure does today: `cortex/src/daemon.rs` disables embed for the process and logs it, and `sb doctor` reports the backend that actually loaded, not just the one resolved.
- **Model files:** `hf-hub` fetches `onnx/model.onnx` + `tokenizer.json` into the same HF cache Candle uses (`~/.cache/huggingface/hub`). No relative cache path (the fastembed `.fastembed_cache` default goes away with fastembed).

### Data Model

- `model_version` strings become backend-free: `bge-small-en-v1.5`, `bge-base-en-v1.5`.
- Each registry entry carries `{ id, dim, hf_repo, onnx_parity: bool }`. `onnx_parity` is `true` only for a model whose parity test exists and runs in `otto ignored`. Today: `bge-small-en-v1.5` only.
- Why reverse the 2026-05-17 rule ("`model_version` encodes backend family so a backend swap does not silently mix incompatible vectors"): that rule protected against an unmeasured difference. The same doc already let kernel variants share an id because their drift sat inside tolerance. The fp32 ONNX drift is 1e-8, far inside the 1e-3 the Candle parity test enforces. The protection moves from the name to the `onnx_parity` gate plus its test, so a backend without proof still cannot serve an id.
- **Migration** (`vault::search::schema`, on open, idempotent, in one transaction). It must survive an old cortex writing `-candle` rows after the rename (the deploy race below), so it dedups first:
  - in `note_embeddings`, where both a `-candle` row and a backend-free row exist for the same `(note_path, kind, chunk_index)`, keep the one with the larger `produced_at` and delete the other (the table's `UNIQUE (note_path, kind, chunk_index, model_version)` would otherwise abort the rename)
  - in `embedding_examined`, keep the larger `examined_at` the same way
  - `UPDATE embedding_config SET value = 'bge-small-en-v1.5' WHERE key = 'active_model' AND value = 'bge-small-en-v1.5-candle'`
  - `UPDATE note_embeddings SET model_version = 'bge-small-en-v1.5' WHERE model_version = 'bge-small-en-v1.5-candle'`
  - `UPDATE embedding_examined SET model_version = 'bge-small-en-v1.5' WHERE model_version = 'bge-small-en-v1.5-candle'` (safe after the dedup)
  - the same set for `bge-base-en-v1.5-candle`
  - any other table or index holding `model_version`: Phase 3 enumerates them from `vault/src/search/schema.rs` and migrates each
  - `-fastembed` rows: never written by a shipped binary; the migration leaves them stale, so the existing stale path re-embeds them
- **The deploy race (why the dedup exists):** today `otto deploy` installs the new binary, then restarts cortex. In that window a new-binary process (`sb bootstrap --force` in the deploy itself, or an oracle MCP spawn) can migrate the DB while the old cortex keeps writing `active_model = -candle` each tick (`cortex/src/embed.rs` -> `SearchIndex::set_active_embedding`). The old cortex then sees every row as stale and re-embeds under `-candle`. Two fixes, both in Phase 3:
  - `otto deploy` stops the daemons before installing and starts them after (today it only restarts after)
  - the dedup above, so a race that slips through anyway converges on the next open instead of failing
- **Old readers:** an oracle MCP process started before the deploy holds the old binary and the old pin. Oracle never writes embeddings, so it only fails loud (unknown pin) until its Claude session restarts. Rollout says so.
- **Rollback:** on the new binary, `backend: candle` is the recovery path; it serves the same backend-free id. Downgrading to a pre-migration binary is unsupported: it would need the reverse UPDATE, and nothing in the plan writes one.

### API Design

Config (both annotated in `config/templates/*.yml.example`, kebab-case):

```yaml
# cortex.yml
embed:
  backend: auto     # auto | candle | onnx. auto = onnx when the CPU has AVX2+FMA, else candle
  workers: 0        # threads for inference. 0 = backend default (candle 2, onnx 16)

# oracle.yml
inference:
  backend: auto     # same values and meaning as cortex embed.backend
  threads: 0        # 0 = backend default (candle 2, onnx 16); used by query embed and rerank
```

- `backend` is case-insensitive, unknown values fail config load (house enum shape, copied from `youtube.engine` in borg config).
- No `fallback` key: `auto` is the fallback policy and `candle | onnx` are explicit pins. A second key would encode the same meaning twice.
- Default `onnx` threads = 16: the measured knee (16 -> 32 buys 9%, 64 loses 2x). Phase 2 re-measures in Rust before the constant lands.
- `embed.workers` keeps its name and its "0 = default" meaning; the default becomes backend-dependent.

Rust surface:

```rust
pub enum BackendChoice { Auto, Candle, Onnx }        // serde, kebab-case
pub enum Backend { Candle, Onnx }
pub struct CpuFeatures { pub avx: bool, pub avx2: bool, pub fma: bool, /* ... */ }
impl CpuFeatures { pub fn detect() -> Self; pub fn onnx_capable(&self) -> bool; }
pub fn resolve_backend(req: BackendChoice, cpu: &CpuFeatures, model: &str) -> eyre::Result<Backend>;
```

- The registry changes from `Vec<(String, Arc<ActiveModel>)>` (one concrete type) to `Vec<(String, Arc<dyn EmbeddingModel>)>`. `ActiveModel` goes away.
- Every load path (`load_active_model`, `load_model_version`, `embed_query`, `get_or_load_model`, `prefetch_*`) takes the resolved `Backend` and threads count.
- `sb cortex embed --prefetch-model` prefetches the files for the backend `auto` resolves to on this host.

### Implementation Plan

#### Phase 0: Prove the dual binary loads on `talos` and ORT-in-Rust matches the spike
**Model:** opus
- Scratch branch, never merged. Remove the `compile_error!`, add `ort` with `cargo add`, enough code to build a session and run one batch.
- On `talos`: `sb --version`, `sb doctor`, one `knowledge_search` with `mode=vector`. Confirms static-linked ORT does not fault at load on a non-AVX2 CPU.
- On `ripr`: time the spike's 64 chunks through Rust ORT at 2/8/16/32 threads.
- Paste both results into this doc under "Measured".
- **Success criteria:**
  - no `talos` command faults; `sb --version` and the vector query exit 0 and the query returns hits; `sb doctor` runs every section (amended at Phase 0: `sb doctor` exits 1 on `talos` from host setup the installed v0.15.18 binary also fails on, so "exits 0" tested `talos` config, not the ORT load)
    - Observed on main: cannot run yet; it needs the Phase 0 scratch build
    - Observed at Phase 0 (scratch `d361970`): `--version` exit 0; `knowledge_search mode=vector` exit 0, 5 hits; `doctor` exit 1 on user@ `TimeoutStopUSec` and 4 missing fabric patterns, no fault; installed v0.15.18 `doctor` exits 1 with 5 errors
  - Rust ORT s/chunk at 16 threads is within 1.5x of the spike's 0.0237
  - if the `talos` load faults: stop. No later phase starts. The doc returns to review with `ort` `load-dynamic` as the named option.

#### Phase 1: Remove fastembed, fix the `vec` gates
**Model:** sonnet
- Delete `vault/src/embedding/fastembed.rs`, the `vec-fastembed` feature and the `fastembed` dependency.
- Change every non-test `cfg(feature = "vec")` gate on the schema and vector module (`vault/src/search.rs`, `vault/src/search/schema.rs`) to `cfg(any(feature = "vec-candle", feature = "vec-onnx"))` so the gate means "some backend". `vec-onnx` is declared empty here and filled in Phase 2.
- Remove the doctor's dead `~/.cache/fastembed` probe in `embedding_findings` (`sb/src/cli/checks.rs`).
- Drop the `.fastembed_cache/` lines from `.gitignore` and the stale compile_error comments from `.otto.yml`.
- Reword every user-facing "fastembed" string (`cortex/src/opts.rs` `--prefetch-model` help, `sb/src/cli/bootstrap.rs`, `oracle/src/tools.rs`, `cortex/src/daemon.rs`, `CLAUDE.md`, `vault/AGENTS.md`, `cortex/AGENTS.md`) to name the embedding model, not a backend.
- **Success criteria:**
  - `rg -n -i --hidden --glob '!.git/**' --glob '!docs/**' --glob '!config/**' --glob '!Cargo.lock' fastembed` returns no matches (`config/` holds generated glossary and eval fixture text, not code; `--hidden` catches `.otto.yml` and `.gitignore`)
    - Observed on main: 83 matching lines
    - Observed at Phase 1 (`3bf5ef7`): no matches (rg exit 1)
    - Amended at the implementation audit: the zero-match check holds at Phase 1 only. Phases 3 and 4 add 8 lines that name the removed backend as a negative case, by design: the migration leaves `-fastembed` rows alone (`vault/src/search/schema.rs`, `schema/tests.rs`) and `backend: fastembed` must fail config load (`embedding/backend/tests.rs`, `cortex/src/config/tests.rs`). No production path names fastembed.
  - `otto ci` exits 0

#### Phase 2: `OnnxEmbedder`
**Model:** opus
- New `vec-onnx` feature (`ort` + existing `tokenizers`, `hf-hub`). `vec = ["vec-candle", "vec-onnx"]`.
- `vault/src/embedding/onnx.rs`: load from the model's `hf_repo`, `intra_threads = N`, CLS pool, L2 norm, max length 512, `token_type_ids` when the graph declares it.
- `vault/src/cpu.rs`: `CpuFeatures`. `OnnxEmbedder::load` refuses unless `onnx_capable()`.
- Parity: `vault/tests/regression/onnx/parity.rs`, a sibling of the Candle test against the same `bge-reference.json`. It asserts `|norm - 1| < 1e-4` per vector and absolute cosine distance `< 1e-3` (the Candle test scores `1 - dot` with no norm check, so a doubled vector would pass; tighten it the same way).
- An ONNX sibling of `embedding::candle::tests::pool_batch_matches_one_at_a_time_real_model` covering a mixed-length batch and a > 512-token text, plus a direct Candle-vs-ONNX check on that same set.
- All three are added by exact name to the `otto ignored` task, and its comment's expected count is updated.
- Re-measure the default thread count in Rust and set `DEFAULT_ONNX_THREADS` from it.
- **Success criteria:**
  - `otto ignored` exits 0 and its per-binary `test result:` lines sum to `6 passed; 0 failed` (3 today + 3 new), so a misspelled `--exact` name that runs zero tests cannot pass
    - Observed on main: exit 0, per-binary lines sum to `3 passed; 0 failed` (most binaries print `0 passed`, so the sum is the check, not any one line)
  - `OnnxEmbedder::load` returns `Err` for a `CpuFeatures` lacking `avx2` and for one lacking `fma` (unit tests, no model download)

#### Phase 3: Backend-free model identity and the pin migration
**Model:** opus
- Registry entries become `{ id, dim, hf_repo, onnx_parity }` with backend-free ids. `onnx_parity = true` for `bge-small-en-v1.5` only.
- Schema migration as in Data Model, dedup first. Enumerate every table holding `model_version` from the schema; migrate each.
- `otto deploy`: stop the installed daemons before `install`, start them after.
- **Success criteria:**
  - a test opens a DB seeded with `-candle` rows and pin, and afterward finds the backend-free id, the same row count, and zero stale targets
  - a test seeded with BOTH a `-candle` and a backend-free row for one chunk keeps only the newer `produced_at` row and the open succeeds
  - a second open of the migrated DB changes nothing (idempotence test)

#### Phase 4: Runtime selection and config
**Model:** opus
- `BackendChoice`, `Backend`, `resolve_backend` with the table above as a table test, using injected `CpuFeatures`.
- Registry -> `Arc<dyn EmbeddingModel>`; remove `ActiveModel`.
- cortex: `embed.backend`; `embed.workers` default becomes backend-dependent. Inference still runs inside `embed::in_inference_pool` for Candle. ONNX sessions get `workers` as `intra_threads`.
- oracle: `inference.backend`, `inference.threads` for query embedding.
- Log the resolution at INFO once per process: requested, resolved, missing features if any.
- Update both annotated templates.
- **Success criteria:**
  - the `resolve_backend` table test covers all seven rows and passes
  - an unknown `backend` value fails config load (test)
  - a test calls cortex's embed loader with `backend: auto` and injected AVX2+FMA `CpuFeatures` and gets `Backend::Onnx`; with injected AVX-only features it gets `Backend::Candle`

#### Phase 5: `OnnxCrossEncoder` and the rerank decision
**Model:** opus
- `vault/src/search/rerank/onnx.rs` behind the `Reranker` port: `cross-encoder/ms-marco-MiniLM-L6-v2` `onnx/model.onnx`, same `intra_threads`, same CPU gate. Selection through `resolve_backend`, with an `onnx_parity`-style gate proven by an ordering test.
- Ordering parity: an `#[ignore]` test scoring fixed `(query, doc)` pairs with both cross-encoders and asserting identical rank order and max score delta < 1e-3. Added to `otto ignored`.
- Fix the stale probe comment in `oracle/src/server/pipeline.rs` (says it projects over available parallelism; `rerank.rs` removed that).
- Latency instrument (none exists today in `oracle/src/eval.rs` or `oracle/src/eval/report.rs`): `sb oracle eval` records wall-clock per query and reports p50/p95 per target.
- The eval run must prove the rerank stage ran. `RERANK_DISABLED` in `oracle/src/server/pipeline.rs` switches rerank off for the whole process after a slow probe or a load failure, so the eval report states, per target, whether rerank ran, and a rerank-on run with the stage disabled is reported as such, never scored as "rerank on".
- Run `sb oracle eval` on `ripr` with rerank on and off. Record nDCG@10, p50/p95, and "rerank ran: yes" here, against the existing `latency-budget-ms` default of 1500 (`oracle/src/config.rs`).
- Defaults do not change. `rerank.enabled` stays `false` in the template; whether to turn it on in the live `ripr` `oracle.yml` is Scott's call with these numbers (templates are write-if-missing, so a template flip would not reach `ripr` anyway).
- **Success criteria:**
  - ordering parity test passes in `otto ignored`, and the per-binary sum is `7 passed; 0 failed`
    - Observed on main: `3 passed; 0 failed`
  - the eval report shows p50/p95 per target and "rerank ran" for the rerank-on target, and those numbers are recorded in this doc
    - Observed at Phase 5: `otto ignored` per-binary lines sum to `7 passed; 0 failed`; rerank-on eval shows `configured ... 400.1 721.4 ran 9/9`; numbers under "Measured (Phase 5, ...)"

#### Phase 6: `sb doctor` cpu section
**Model:** sonnet
- `sb/src/cli/checks/cpu.rs`, one `Section` named `cpu`, the house shape of `checks/stop_timeout.rs`.
- Findings: detected features (info), resolved backend for embed and for oracle (ok), DB pin (ok), and a Warn when `onnx` is pinned in config on a CPU that is not `onnx_capable()`.
- **Success criteria:**
  - `sb doctor` on `ripr` prints a `[cpu]` line naming `avx512f` and `backend: onnx`
    - Observed on main: no `[cpu]` line (rg exit 1)
  - the Warn case is covered by a unit test with injected `CpuFeatures`

#### Phase 7: Docs and comments sweep
**Model:** sonnet
- Remove the "desk is AVX-only" framing everywhere it is stated as the daemon host's CPU: `CLAUDE.md`, `cortex/AGENTS.md` (also its wrong "embed uses fastembed" line), `vault/AGENTS.md`, `vault/src/search/AGENTS.md` (its "backend in the model id" invariant becomes the `onnx_parity` gate), `vault/src/search/rerank.rs`, `cortex/src/embed.rs`, `cortex/src/config.rs`, `cortex/src/embed/tests.rs`, `oracle/src/server/pipeline.rs`, `vault/src/embedding/candle.rs`, `vault/Cargo.toml`, both templates, and the comments in `bin/meeting` and `bin/dictate`.
- The scar-tissue history (the 2026-08-16 two-day tick) stays; it is why the per-tick cap and pool isolation exist, and it is still true on `talos`.
- Document the backend selection in `CLAUDE.md` (one paragraph) and `vault/AGENTS.md`.
- **Success criteria:**
  - `rg -i -n 'avx-only' --glob '!docs/**' --glob '!config/eval/**'` returns only lines that name `talos` or describe the 2026-08-16 incident as history
    - Observed on main: 12 lines, every one asserting the daemon host is AVX-only
  - `otto ci` exits 0

## Acceptance Criteria

- [ ] On `ripr`, `sqlite3 ~/.local/share/sb/oracle/oracle.db "select model_version, count(*) from note_embeddings group by 1"` returns exactly one row, `bge-small-en-v1.5`, with a count >= the pre-deploy count. And `select count(*) from note_embeddings where produced_at > <deploy epoch>` is no larger than the chunks embedded by post-deploy ticks in `journalctl --user -u cortex | rg 'daemon embed tick'` (a rename, not a re-embed).
  - Observed on main: `bge-small-en-v1.5-candle|23930`; rows with `produced_at` in the last day: 142; post-tick journal lines in that day: `embedded=498`, `21`, `3`. (Today the row count can sit below the tick sum because re-embeds replace rows; after the rename the check is that the post-deploy row count does not jump toward 23930.)
- [ ] On `ripr`, a cortex embed tick of >= 100 chunks logs <= 0.05 s/chunk (`journalctl --user -u cortex | rg 'inference progress'`).
  - Observed on main: 498 chunks, last sub-batch at 166.7 s cumulative, ~0.33 s/chunk
- [ ] `sb doctor` on `ripr` prints a `[cpu]` finding naming `backend: onnx`; the same binary on `talos` prints `backend: candle`.
  - Observed on main: `sb doctor | rg '\[cpu\]|backend'` matches nothing (rg exit 1); the only embedding-backend line is `[embedding cache] candle cache: 2 model(s) under /home/saidler/.cache/huggingface/hub`
- [ ] On `talos`, `sb oracle` answers a `knowledge_search` with `mode=vector` using the same binary that runs ONNX on `ripr`.
  - Observed on main: cannot run yet; no dual binary exists until Phase 4 (Phase 0 proves the load on a scratch branch)
- [ ] `rg -i -n 'avx-only' --glob '!docs/**' --glob '!config/eval/**'` returns only `talos`-scoped or historical lines.
  - Observed on main: 12 matching lines across 11 files, all asserting the daemon host is AVX-only

## Resolved Decisions

- 2026-10-27: vectors are interchangeable across backends (spike, 1e-8). Model id drops the backend suffix; a per-model parity gate replaces the name-based guard.
- 2026-10-27: `ort` directly, not fastembed. The deciding reason: fastembed hardcodes intra threads to `available_parallelism()` (64 on `ripr`, measured 2x slower than 32) in both `text_embedding/impl.rs` and `reranking/impl.rs`, with no knob. Also: relative `.fastembed_cache` default. (Its reranker does accept user-defined models, so ms-marco is not a blocker.) Panel round 1: both seats agree.
- 2026-10-27, panel round 1: deferred findings, with reasons. Unpinned HF revision: the Candle path has the same exposure today; not introduced here. ORT spin-wait and thread total (12 governance + 16 cortex ONNX + 16 oracle ONNX = 44 of 64): measure only if governance latency shows up. "Same artifact on both hosts": the house deploy builds per host from one `Cargo.lock` with a hash-pinned ORT archive, and acceptance criterion 4 runs the final binary on `talos`. `-fastembed` pin: never written by a shipped binary.
- 2026-10-27, panel round 1, pushed back: "cortex's rayon threads queue on the session Mutex" is wrong; `embed_in_sub_batches` (`cortex/src/embed.rs`) makes one `embed_batch` call per sub-batch, sequentially.
- 2026-10-27: fp32 only; int8 rejected on parity.

## Alternatives Considered

### Alternative 1: `-C target-cpu=native` per host
- **Description:** each host compiles for its own CPU in `otto deploy`.
- **Pros:** one-line change; speeds up Candle's compile-time-gated helpers.
- **Cons:** no runtime detection; a binary copied to `talos` SIGILLs; Candle stays the backend on `ripr`.
- **Why not chosen:** Scott asked for runtime detection. It also leaves most of the 14x on the table.

### Alternative 2: fastembed as the ONNX backend
- **Description:** fix the feature wiring and ship the existing `fastembed.rs`.
- **Pros:** code already written.
- **Cons:** threads unbounded and not configurable (64 on `ripr`); relative cache dir; no ms-marco reranker; pulls `openssl-sys` and `image` by default.
- **Why not chosen:** the thread cap is the measured win, and fastembed blocks it.

### Alternative 3: keep the backend suffix and re-embed on switch
- **Description:** pin `bge-small-en-v1.5-onnx`, let the stale path re-embed ~24K rows.
- **Pros:** no migration code.
- **Cons:** a host-dependent id means `talos` and `ripr` can never share an index; every backend flip is a full re-embed; the name encodes a difference measurement says does not exist.
- **Why not chosen:** names tell the truth, and these vectors are the same.

### Alternative 4: speed up Candle (gemm `x86-v4`, MKL)
- **Description:** keep one backend, enable AVX-512 gemm and Intel MKL.
- **Pros:** no second runtime.
- **Cons:** MKL is Intel's library on an AMD part and is another static link that must stay safe on `talos`; candle's elementwise kernels stay compile-time gated.
- **Why not chosen:** Scott asked to keep the old path as is and prefer a new one.

## Technical Considerations

### Dependencies
- Add `ort` with `cargo add` (pyke static ORT, download-binaries; the archive is already cached on `ripr`). Remove `fastembed`.
- Every host's first build needs network to `cdn.pyke.io`, or `ORT_LIB_LOCATION` set.
- No cross-repo blast radius: everything lives in `second-brain`. Ship order is per host: `ripr` first (daemons), then `talos` and `lappy`.

### Performance
- Spike numbers above. Phase 0 and Phase 2 re-measure in Rust and paste results here.
- cortex `daemon.rayon-threads` (12) + ONNX 16 = 28 of 64 threads during an embed tick.

### Security
- New native code: the pyke ORT static archive, hash-pinned by `ort-sys`. Same supply chain the fastembed dependency already pulled.
- Models come from the same HF repos already trusted for Candle weights.

### Testing Strategy
- Unit, no network: `resolve_backend` table test, CPU gate refusal, config enum parse, migration + idempotence, doctor Warn.
- `otto ignored` (network, real models): ONNX embed parity vs fixture and vs Candle, cross-encoder order parity.
- Host shakedown: `talos` (Candle) and `ripr` (ONNX) each run `sb doctor` and one vector query.

### Rollout Plan
- Ship as one release after Phase 7.
- Restart any Claude session on `ripr` that has oracle loaded (its old `sb oracle serve` fails loud on the new pin until then).
- Operator steps on `ripr` (Scott): set `embed.workers: 0` (or delete the line) in `~/.config/sb/cortex.yml` so the backend default applies; `otto deploy`; confirm the INFO line and the acceptance criteria.
- `talos`, `lappy`: `otto deploy`, `sb doctor`.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Static ORT faults at load on `talos` | Low | High | Phase 0 gate; `load-dynamic` named as the fallback |
| Rust ORT slower than Python spike | Med | Med | Phase 0 measures; Phase 2 sets the default from Rust numbers |
| Migration misses a table keyed on `model_version` | Med | Med | Phase 3 enumerates from the schema; zero-stale-targets test |
| ONNX threads + rayon oversubscribe during governance | Low | Med | bounded `intra_threads`; doctor reports the total |
| Live `embed.workers: 2` pins ONNX at 2 threads after deploy | High | Low | operator step in Rollout; still 4x faster at 2 |

## Review Log

- Pass 1 (draft): from the design-research brief plus the ONNX spike run on `ripr`.
- Pass 2 (correctness): checked the schema columns (`embedding_config(key, value)`, `embedding_examined` PK includes `model_version`) and the `ort` `Session::run(&mut self)` signature; the Phase 1 `rg fastembed` criterion would have failed on 16 files, so Phase 1 now rewords them and the criterion excludes `config/`.
- Pass 3 (clarity): filled the `lappy` CPU (`ssh`), said where `vec-onnx` is first declared.
- Pass 4 (edge cases): `embedding_examined` PK collision on rename -> `UPDATE OR IGNORE` + cleanup; the live `embed.workers: 2` pin -> Rollout step; `-fastembed` rows -> left stale.
- Panel round 1 (Gemini + Codex): 2 must-fix (deploy race in the migration; no latency instrument for Phase 5) and 11 cheap wins folded in; 4 deferred with reasons in Resolved Decisions; 1 seat claim pushed back.
- Pass 5 (excellence): acceptance criterion 1 made checkable (no "no re-embed in between" clause that no command measures).

## Open Questions

- None.

## References

- `docs/design/2026-05-17-candle-embedding-backend.md`
- `docs/design/2026-06-06-configurable-retrieval-pipeline.md`
- `cortex/AGENTS.md` (inference pool isolation)
- fastembed 5.13.4 `src/text_embedding/impl.rs`, `src/reranking/impl.rs` (hardcoded intra threads)
- gemm-common 0.19.0 `src/gemm.rs` (runtime FMA dispatch)

## Addendum: rejected and parked

- **int8 bge-small:** 10-20% faster at 16-32 threads, 3e-3 to 2.4e-2 cosine drift. Would force a re-embed and break cross-host interchangeability for a small gain. Revisit only with a measured retrieval-quality result from `sb oracle eval`.
- **Local Whisper:** see Non-Goals.
