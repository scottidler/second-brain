# Implementation Notes: CPU-Tier Inference Backend

Design doc: `docs/design/2026-10-27-cpu-tier-inference-backend.md`

## Phase 0: Prove the dual binary loads on `talos` and ORT-in-Rust matches the spike

Scratch branch `cpu-tier-phase0-spike`, commit `d361970`, worktree `/home/saidler/repos/scottidler/second-brain-phase0`. Never merged, never pushed. Numbers are in the design doc under "Measured (Phase 0, Rust ORT dual binary, 2026-10-27)".

### Design decisions
- Built on `ripr` and copied to `talos` with `scp`, not built on `talos`. Why: acceptance criterion 4 is "the binary that runs ONNX on `ripr` also runs on `talos`", so one artifact, one sha256 (`5a91bbb4...`) on both hosts.
- Spike code lives in `vault/src/embedding/onnx.rs` (`build_session`, `embed_batch`, `time_threads`) behind a new `vec-onnx` feature, called from a hidden `sb onnx-spike` (`sb/src/cli/spike.rs`). Why: the ORT archive only reaches the link if `sb` calls into it, so a vault-only test would prove nothing about the `sb` binary.
- One process-wide ORT environment in a `OnceLock` (`onnx::build_session`). Why: ort rc.15 refuses a second environment per process ("only one environment is allowed per process"). Phase 2's `OnnxEmbedder` and Phase 5's `OnnxCrossEncoder` must share one too.
- `talos` commands ran with `XDG_DATA_HOME=/home/saidler/sb-phase0-data`, a `cp -a` copy of `~/.local/share/sb`. Why: talos's installed `sb` is v0.15.18 and the scratch binary is main (v0.19.1), so opening the live `oracle.db` could have applied 0.15 -> 0.19 schema migrations under the installed oracle.
- Each timed pass covers tokenize + run + CLS pool + L2, after one warmup batch, 5 passes, median and best reported. Why: that is what `embed_batch` costs a cortex tick, not just `Session::run`.
- Corpus: the first 64 `docs/design/*.md` files (sorted) whose encoding hits the 512-token truncation. The Python spike's exact 64 texts were not kept, so the corpus differs; every chunk is exactly 512 tokens, which is the cost-relevant property.

### Deviations
- `ort` is 2.0.0-rc.15 / ORT 1.31.0, not rc.12 / ORT 1.24.2 as the doc's first Measured section says. `cargo add` picks the latest; rc.12 was only ever fastembed's `=` pin.
- Dropped the `fastembed` dependency on the scratch branch (Phase 1 work). rc.15 and fastembed's `ort = "=2.0.0-rc.12"` cannot co-resolve. `vec-fastembed` stays as an empty stub feature so the existing `cfg` gates compile.
- `sb doctor` on `talos` exited 1, not 0. The two error findings are host state (user@ `TimeoutStopUSec` 5s, 4 fabric patterns missing), not the binary; the installed v0.15.18 doctor exits 1 on the same host (5 errors). I did not fix them: one is `/etc/systemd` under sudo, the other is `sb bootstrap` writing `~/.config/sb`, both outside a scratch spike.
- Added a fourth `talos` command the plan did not list: `sb onnx-spike --probe`, to locate the fault the Architecture section says Phase 0 re-proves ("fires at session init, not process load").

### Tradeoffs
- Static link (default `download-binaries`) vs `load-dynamic`: kept static. The fault is at model parse, after any CPU gate, so the static archive is safe to carry on `talos`.
- `ort` default features (`tls-native`, `copy-dylibs`, `tracing`, `ndarray`) kept on the spike. Phase 2 should trim to what it uses.

### Open questions
- ORT's own log lines arrive at INFO through `tracing` (hundreds of `ort::logging: Reserving memory in BFCArena ...` per session). Phase 2 should set the ORT log level to warning or filter the `ort` target, or cortex and oracle logs fill with them.
- `talos` scratch artifacts left in place for re-runs: `~/sb-phase0`, `~/sb-phase0-data/` (1.4G copy), `~/sb-phase0-bge-reference.json`, `~/sb-phase0-query.out`, `~/sb-phase0-query.err`, `~/sb-phase0-probe.out`. BAAI `onnx/model.onnx` is now also in talos's HF cache. Remove the scratch files once Phase 0 is accepted.
- Rust ORT does not reproduce the Python 64-thread cliff (64 is 4% slower than 32; Python was 2x). 16 is still the knee on cost per thread, but Phase 2's re-measure should decide between 16 and 32.

## Phase 0 (orchestrator)

### Design decisions
- Phase 2 builds on `ort` 2.0.0-rc.15 / ORT 1.31.0: `general.md` requires `cargo add` for versions, and rc.15 is what it resolves. The Background rc.12 line is marked superseded.

### Deviations
- Phase 0 criterion 1 amended from "every `talos` command exits 0" to "no command faults; `--version` and the vector query exit 0; doctor runs every section". Doc defect: `sb doctor` exits 1 on `talos` from host setup (user@ `TimeoutStopUSec`, missing fabric patterns), and the installed v0.15.18 binary fails it too, so the original wording measured `talos` config, not the ORT load.

### Tradeoffs
- None.

### Open questions
- None. The worker's ORT log-level, single-environment and 16-vs-32 thread items are routed to Phase 2 (and Phase 5 for the environment).

## Phase 1: Remove fastembed, fix the `vec` gates
### Design decisions
- `vec-onnx = ["search"]` (not empty); `vault/Cargo.toml`; the new gate `any(vec-candle, vec-onnx)` guards the SQL schema and vector module, which need `rusqlite` via `search`; an empty feature would enable the gate without its dependency.
- Test-only gates (`cfg(all(test, feature = "vec"))`, test `cfg(feature = "vec")` blocks) left on `vec`; the doc scopes the change to non-test gates, and `vec` still implies `vec-candle`.
- `embedding.rs` gates collapse to `cfg(feature = "vec-candle")`; no ONNX code exists until Phase 2, so "some backend" in that module is Candle only.
- Doctor's `has_any_subdir` helper deleted; it had no remaining caller once the fastembed probe went; doctor message now reads "embedding model cache".
- `vault/tests/regression/hybrid/retrieval.rs` fixture word "fastembed" renamed "embedder" consistently in corpus and queries; needed for the zero-match criterion; BM25 behavior is unchanged since the token is replaced at all sites.

### Deviations
- None.

### Tradeoffs
- Reworded `rerank.rs` comment to "the ONNX runtime aborts there" vs deleting the sentence; keeps the AVX-only host rationale the Phase 5 work depends on.

### Open questions
- None.

## Phase 2: `OnnxEmbedder`
### Design decisions
- Shared ORT plumbing in its own module, `vault::onnx` (`vault/src/onnx.rs`: `build_session`, `require_onnx_capable`, private `environment()`); rc.15 allows one environment per process, so Phase 5's `OnnxCrossEncoder` must build through the same `build_session`. The environment sits in a `OnceLock<Result<Environment, String>>`: race-free (the spike's `get`-then-build could lose a race to "only one environment"), and a failed build is cached because ORT would refuse a second attempt anyway.
- CPU gate lives in `build_session` itself, plus a first-line check in `OnnxEmbedder::load_version`; `vault/src/onnx.rs:build_session`; any future ONNX caller gets the SIGILL guard structurally; `load_version` checks first so an incapable CPU never even downloads the model.
- ORT logging: `ort` built with `default-features = false` (no `tracing`), a custom `with_logger` forwarding to `log` under target `ort`, warnings and errors only, then `Environment::set_log_level(Warning)`; `vault/src/onnx.rs:forward_ort_log`; with the `tracing` feature ort creates the env at VERBOSE and forwards everything; the threshold must also be enforced in the callback because the env build itself logs hardware discovery at INFO before `set_log_level` can run (seen in the first measurement run: 3 INFO lines; zero after the fix).
- `ort` features trimmed to `std`, `download-binaries`, `tls-rustls`, `api-31`; `vault/Cargo.toml`; drops native-tls/openssl, ndarray, copy-dylibs and tracing, which nothing uses.
- `Session::run` takes `&mut self` in rc.15 (verified in `ort-2.0.0-rc.15/src/session/run.rs`), so one `Mutex<Session>`; the tokenizer stays outside the lock (`encode_batch` is `&self`), so only the ORT run serializes.
- ONNX has its own registry, `SUPPORTED_MODELS = [("bge-small-en-v1.5", "BAAI/bge-small-en-v1.5", 384)]`, with the backend-free id; `vault/src/embedding/onnx.rs`; this phase does not touch the Candle registry or the DB pin; Phase 3 merges the two into `{ id, dim, hf_repo, onnx_parity }`. `bge-base` is refused ("no ONNX parity proof").
- `token_type_ids` is fed only when the graph declares it, decided once at load from `session.inputs()`; the load also fails loud if `input_ids` or `attention_mask` is missing.
- `config.json` is fetched too (as Candle does) to check `hidden_size == dim` and take `pad_token_id` for padding, so a drifted checkpoint fails at load instead of producing wrong-width vectors.
- Pooling is a pure `cls_pool_normalize(shape, data, batch, dim)` that checks rank, batch, dim and element count, so the shape guards are unit-tested without a model.
- Threads: `0` means `DEFAULT_ONNX_THREADS.min(available_parallelism)`; an explicit count is capped at available parallelism; `vault/src/embedding/onnx.rs:resolve_threads`; intra-op threads past the logical CPU count only contend.
- `DEFAULT_ONNX_THREADS = 16` from the re-measure (numbers in the doc's "Measured (Phase 2 ...)" section): 16 -> 32 buys 13-14%, and 32 would put cortex + oracle + governance at 76 threads on a 64-thread host.
- `CpuFeatures::missing_for_onnx()` returns the missing names so the error says "lacks avx2 and fma"; Phase 4's resolution table needs exactly that message.

### Deviations
- `OnnxEmbedder::load_version(version, threads, &CpuFeatures)` rather than a bare `load(&CpuFeatures)`; same effect, correct seam: it mirrors `CandleBertModel::load_version(version, workers)` so Phase 4's registry can thread version and threads through one call.
- The direct Candle-vs-ONNX check is a unit test in `vault/src/embedding/onnx/tests.rs` (`embedding::onnx::tests::onnx_matches_candle_real_model`, gated on `vec-candle`) rather than a third file; the plan named no location. Threshold 1e-4 (measured 4.8e-7), tighter than the 1e-3 parity bar.
- `vault/AGENTS.md` module map gained `cpu.rs` and `onnx.rs` lines (and an `OnnxEmbedder` key-API line): `otto ci`'s `agents-map` lint fails on an undocumented top-level module.
- The thread re-measure used a throwaway `#[ignore]` integration test (deleted after the runs, never committed) instead of the Phase 0 `sb onnx-spike` subcommand, so it timed the shipped `OnnxEmbedder` path.

### Tradeoffs
- Forwarding ORT logs to `log` vs letting ORT write its own stderr (no logger, env default ERROR): forwarding keeps ORT warnings in `cortex.log` / `oracle.log` and never on oracle's MCP stdio, at the cost of one small callback.
- One session with N intra threads vs Candle-style replicas: ORT's pool is the parallelism, and the replicas would multiply RSS for nothing.
- Capping explicit thread counts at available parallelism vs honoring any number: a config typo of 128 on a 16-thread laptop would only contend; capping is silent, so the INFO load line logs the effective count.

### Open questions
- None.

## Phase 3: Backend-free model identity and the pin migration
### Design decisions
- One registry, `vault::embedding::MODELS: &[ModelSpec { id, dim, hf_repo, onnx_parity }]` with `model_spec(id)`, plus `BGE_SMALL_MODEL_VERSION` / `BGE_BASE_MODEL_VERSION`: `vault/src/embedding.rs`: the Phase 2 ONNX list and the Candle list merge here. Candle's `supported_model` returns every entry (it is the reference backend); ONNX's filters on `onnx_parity`. Both now return `&'static ModelSpec` instead of a `(repo, dim)` tuple.
- The registry and `ACTIVE_MODEL_VERSION` are no longer gated on `vec-candle`: they are plain data the schema seed reads under `any(vec-candle, vec-onnx)`, so an ONNX-only build (`cargo check -p vault --features vec-onnx`) compiles too.
- `CANDLE_MODEL_VERSION`, candle's `MODEL_REPO`/`DIM`/`SUPPORTED_MODELS`/`BGE_BASE_MODEL_VERSION`, and onnx's `ONNX_MODEL_VERSION`/`SUPPORTED_MODELS` are deleted, not aliased: no compat shims; every caller moved to the registry constants.
- Migration `SearchIndex::migrate_backend_free_model_ids`: `vault/src/search/schema.rs`: runs inside `ensure_vec_schema` after the claim-kind rebuild and before the `active_model` seed. The legacy ids are an explicit list, `LEGACY_MODEL_IDS` (`-candle` small and base), not derived from the registry: they are historical facts about what shipped binaries wrote.
- Tables enumerated from `schema.rs` and cross-checked against the live `oracle.db` schema (`sqlite3 -readonly`): `embedding_config.active_model`, `note_embeddings.model_version`, `embedding_examined.model_version`. No index, trigger or other table (`edges`, `graph_state`, `edge_build_state`, `entities`, `note_tags`) stores a model id.
- A read-only probe (`legacy_model_ids_present`) runs first; with no legacy id anywhere the migration returns before `BEGIN IMMEDIATE`, so oracle and cortex opens after the rename take no write lock. A migration that does run is one transaction and logs per-id counts (renamed, dropped-older, pin) at INFO.
- Tie on `produced_at` / `examined_at` keeps the backend-free row: fewer writes, and either is correct.
- `otto deploy` now stops the borg/cortex units that exist before `install`, runs `sb bootstrap --force`, then starts them; an `EXIT` trap starts them again if anything between fails, so a failed deploy never leaves the daemons down: `.otto.yml` `deploy`. Verified by reading and `bash -n` on the extracted script; not executed (cortex is live on this host).
- Tests (`vault/src/search/schema/tests.rs`): rename with same row count and zero stale targets for all three kinds; collision dedup (newer-wins both ways, tie, multi-chunk, examined, re-pinned `-candle`); idempotence (byte-identical rows, sentinels, pin, and the third run reports no work); fresh-DB no-op; `-fastembed` and custom pins untouched. Mutation-checked: short-circuiting the migration fails 3 tests; removing the dedup DELETEs fails 2 with the UNIQUE abort.
- Two existing tests that pinned the old seed (`active_embedding_model_reads_the_default_seed`, `vec_schema_seeds_active_model_config`) inverted to expect `bge-small-en-v1.5`.

### Deviations
- Dedup is per `(note_path, kind)` set, not per `(note_path, kind, chunk_index)` row as the Data Model bullet says: the set with the newer `MAX(produced_at)` wins whole and the other set is deleted. Same result for one-chunk kinds (summary, which the success criterion tests); for transcript-chunk and claim sets it avoids keeping, beside a newer 2-chunk set, chunk 2 of an older 3-chunk generation, which `search_vector` would max-pool over until a stale re-embed replaced it.
- `embedding_examined` dedup keeps the newer `examined_at` exactly as specified (its primary key already makes it one row per set).

### Tradeoffs
- Probe on every open vs a one-shot marker row: the probe scans the `(kind, model_version)` index (a few ms at ~24K rows) but converges after a deploy race that a marker would have recorded as "done".
- Explicit legacy list vs `format!("{id}-candle")` from the registry: explicit, so a model added later never grows a phantom legacy id.

### Open questions
- `vault/src/search/AGENTS.md` still states "a backend swap changes the string and triggers re-embed". It is now false; the plan assigns that line to Phase 7, so I left it.
- The `.otto.yml` deploy change is code only; its first run is the Phase 7 rollout on `ripr`.

## Phase 4: Runtime selection and config
### Design decisions
- `BackendChoice`, `Backend`, `InferenceConfig`, `resolve_backend`, `resolution_reason` and `resolve_backend_logged` live in a new `vault::embedding::backend` (`vault/src/embedding/backend.rs`), re-exported from `vault::embedding`, and are ungated: plain data plus a pure function, so cortex and oracle config can deserialize `BackendChoice` in any build.
- `BackendChoice` has a hand-written `Deserialize` (ASCII-lowercase match, error names `auto, candle, onnx`). `youtube.engine` itself is a derived serde enum and so case-sensitive; the doc asks for case-insensitive, so this follows the `SlideCategory` deserializer in borg config, the house case-insensitive enum.
- `resolve_backend` is pure; the INFO line comes from `resolve_backend_logged` (requested, resolved, model, reason: missing features or the parity-less model). Callers log through it exactly once per model load: once per process in cortex (CLI run or daemon start), once per pinned model in oracle (first vector query).
- An unknown model under `auto` resolves to Candle (`has_onnx_parity` is false), so Candle's loader produces the existing "unknown model" error; under `onnx` it is the "no ONNX parity proof" error.
- Load paths in `vault/src/embedding.rs` (`load_active_model`, `load_model_version`, `prefetch_active_model`, `prefetch_model_version`, `is_supported_model_version`) take a `Backend`; loads also take threads and `&CpuFeatures` and return `Box<dyn EmbeddingModel>`. The registry is `Vec<(String, Arc<dyn EmbeddingModel>)>`; `ActiveModel` is deleted. Each match arm is `cfg`-gated per backend, so `vec-candle`-only, `vec-onnx`-only and no-backend builds still compile (all three checked).
- `embed_query(text, model_version, &InferenceConfig, &CpuFeatures)`: on a registry miss it checks the registry has the model, resolves and logs, checks the resolved backend can serve it, then loads with `inference.threads`.
- cortex: `embed.backend` on `EmbedConfig`; loading goes through `embed::load_embedder` (`cortex/src/embed/loader.rs`), which takes the actual loader as a parameter. Production passes `vault::embedding::load_model_version`; the success-criterion test passes a recording closure that returns a `MockEmbedder`, so the test runs the real resolve-and-load path with injected `CpuFeatures` and no download.
- `embed.workers` reaches the backend unchanged as its thread count: Candle reads it as before (replicas, 0 = its own default), ONNX as intra-op threads (0 = `DEFAULT_ONNX_THREADS` capped at available parallelism). The rayon inference pool keeps its sizing (`workers`, 0 = 2), so Candle behavior is byte-identical.
- oracle: `inference:` is `vault::embedding::InferenceConfig` directly (`deny_unknown_fields`); `OracleMcpServer` detects `CpuFeatures` once in `new` and passes it with `config.inference` to `embed_query`.
- `cortex::embed::prefetch` takes a `BackendChoice` and prefetches the files for what it resolves to on this CPU: `sb cortex embed --prefetch-model` passes `embed.backend`; `sb bootstrap` loads cortex.yml (defaults when absent) and passes the same.
- Templates: cortex.yml.example now sets `backend: auto` and `workers: 0` (was a commented `# workers: 2`); oracle.yml.example gains an `inference:` block. The bootstrap template-parse tests cover both.

### Deviations
- ONNX calls go through `embed::in_inference_pool` as well, not "for Candle" only as the Phase 4 bullet says: followed the Architecture section (tokenization runs on rayon; panel finding C6). No code change was needed: every `embed_batch` already funnels through `embed_in_sub_batches`.
- `embed_query` and oracle's lazy load take the `InferenceConfig` (requested backend + threads) and resolve on first load, rather than taking an already-resolved `Backend` as the API Design says. Same effect, correct seam: resolution depends on the pinned model (the parity column), and oracle only learns that from the DB at query time; resolving on the registry miss also makes the INFO line once per model per process.
- The cortex success criterion's "embed loader" is `load_embedder`, the function both `embed::run` and `load_daemon_model` call, with the model load injected; the tests do not call `load_daemon_model` itself, which opens the fixed `oracle_db_path()`.
- oracle `inference.threads: 0` on Candle keeps today's default (`min(8, available_parallelism)` replicas on the global rayon pool), not "candle 2" as the API Design comment reads. Candle is the unchanged path, and oracle has no dedicated pool; the oracle template and the `Config.inference` doc say 8.
- cortex `embed.workers: 0` on ONNX sizes the rayon pool at 2 (tokenization only) and ORT at 16; an explicit `workers: N` sizes both at N. Same "workers = inference threads" contract; the pool's sizing rule is untouched so Candle stays identical.
- `vault/AGENTS.md` key-API line rewritten and `cortex/AGENTS.md` gained a `load_embedder` invariant line: the old line named `load_active_model` as Candle-only, which is now false.

### Tradeoffs
- `resolve_backend_logged` wrapper vs logging inside `resolve_backend`: keeps the table function free of side effects, and the reason text (`resolution_reason`) is unit-tested on its own.
- Registry keyed by model id only vs `(model, backend)`: the backend is fixed per process (config + CPU), so a second key part could never differ.
- Bootstrap loads cortex.yml to honor a `candle` pin during prefetch vs always prefetching for `auto`: an operator who pinned Candle on a capable host should not get the ONNX files instead.

### Open questions
- `vault/src/search/AGENTS.md` still names `load_active_model()` in the re-embed flow line; left for the Phase 7 docs sweep with the rest of that file.
- The rollout step on `ripr` (set `embed.workers: 0` in the live cortex.yml) is unchanged and still needed: the live `workers: 2` would pin ONNX at 2 intra-op threads.

## Phase 5: `OnnxCrossEncoder` and the rerank decision
### Design decisions
- One cross-encoder registry, `CROSS_ENCODERS: &[CrossEncoderSpec { id, hf_repo, onnx_parity }]` with `cross_encoder_spec`: `vault/src/search/rerank.rs`: the Phase 3 embedding-registry shape for rerank models. Candle serves every entry (reference backend); ONNX only an entry with `onnx_parity`. Kept out of `embedding::MODELS` so `is_supported_model_version` never accepts a cross-encoder as an embedding pin.
- Selection goes through the embedding resolution table: `embedding::backend::resolve_with_parity` (the seven-row table with the parity column supplied by the caller) and `resolve_with_parity_logged`; `resolve_backend` and `resolve_backend_logged` now delegate to them with the embedding registry's bit. `rerank::resolve_rerank_backend` passes the cross-encoder registry's bit, so rerank and embedding cannot drift onto two tables.
- `get_or_load_reranker(model, &InferenceConfig, &CpuFeatures) -> Arc<dyn Reranker>` replaces the Candle-only registry; oracle's `maybe_rerank` passes `config.inference` and the server's detected CPU, so rerank uses the same `backend` and `threads` as query embedding (the API Design comment for `inference.threads`). The INFO resolution line fires once per rerank model per process.
- `OnnxCrossEncoder` (`vault/src/search/rerank/onnx.rs`) builds its session through `vault::onnx::build_session` (the one ORT environment, CPU gate, inter 1, Level3) and checks the CPU first itself so an incapable host never downloads the model. Threads reuse `embedding::onnx::resolve_threads` (made `pub(crate)`): 0 = 16 capped at available parallelism. Output must be `[batch, 1]` (`single_logits`), else a loud error.
- HF repo is the canonical `cross-encoder/ms-marco-MiniLM-L6-v2` for both backends (Candle used the redirecting `-L-6-` id): one repo, one tokenizer, so the parity proof compares like with like.
- Ordering parity test `search::rerank::onnx::tests::onnx_ranks_like_candle_real_model`: 8 fixed pairs of mixed length (one past 512 tokens), asserts identical rank order, max score delta < 1e-3, a fixture that separates relevance, and batch-vs-single agreement. Measured max delta 1.9e-6. Added to `otto ignored` by exact name; the task comment says `7 passed`.
- Rerank observability: `RerankRun { Off, Ran, FailedOpen, Disabled }` and `PipelineRun { rows, rerank }` (`oracle/src/server/pipeline.rs`), returned by `run_pipeline_traced` / `run_configured_pipeline_traced`; `run_pipeline` / `run_configured_pipeline` keep their signatures for `knowledge_search` and the answerability eval. A latched-off `RERANK_DISABLED` returns `Disabled`.
- Eval latency: `retrieve` times every target per query (`record` in `oracle/src/eval.rs`) and `ModeReport` carries `Latency { p50_ms, p95_ms, n }` (nearest rank) and `rerank: Option<RerankTally>` (only the `configured` target has a rerank stage). The table cell reads `off`, `ran N/T`, or `NOT RUN N/T`, and a partial or latched-off run prints a WARNING saying its scores are not a rerank-on result.
- One untimed warmup of every target on the first query before the timed pass, so model loads (embedder, cross-encoder) are not charged to whichever target runs first.
- oracle's `default_rerank_model` reads `vault::search::RERANK_MODEL_ID` instead of repeating the string.
- Fixed the stale probe comment in `maybe_rerank` / `rerank_within_budget` (said "over the available parallelism"; projection is linear).

### Deviations
- The eval ran with the live data dir hidden (`bwrap --dev-bind / / --tmpfs ~/.local/share/sb`) and `XDG_DATA_HOME` at a scratch copy, and the config passed with `sb oracle --config <copy>` rather than `XDG_CONFIG_HOME`: same isolation, and it leaves fabric's own config lookup (the judge) untouched. `strace` confirmed the only `oracle.db` opened was the copy; opening it ran the Phase 3 migration on the copy (23930 rows renamed).
- `vault/src/search/rerank.rs` module header: updated the pieces list and the "not exercised" sentence for the new test, but left the "desk is AVX-only" status paragraph for the Phase 7 sweep that names this file.
- `vault/src/search/AGENTS.md` gained a rerank entry-point line and `oracle/AGENTS.md` its `server.rs` line names the traced pipeline; nested files are not required by `agents-map`, but the old text no longer described the reranker.
- `CandleCrossEncoder`'s registry and `get_or_load_reranker` / `prefetch_reranker` moved up to `rerank.rs` (backend-aware); `CandleCrossEncoder::prefetch` / `OnnxCrossEncoder::prefetch` are the per-backend halves. `prefetch_reranker` still has no caller, as before.

### Tradeoffs
- Cross-encoder registry separate from `MODELS` vs one table with a kind column: separate, because every `MODELS` consumer (DB pin validation, stale-target scan) would otherwise need to filter the kind out.
- Warmup query vs counting model load in p95: warmup, because with 9 queries p95 is the max and would measure a one-time load, not steady-state per-query cost; a fresh oracle process does pay the load on its first rerank query.
- Comparing rerank within one run vs across runs: within, because the judged pool is the union of every target's top 10, so the rerank-on run re-bases every row's Recall and ideal DCG (vector 0.7928 off-run vs 0.6982 on-run with identical rankings).

### Open questions
- Enable rerank on `ripr`? Same pool: configured nDCG@10 0.8482 with rerank vs 0.6982 without (+0.150), Recall 0.6016 vs 0.4728, MRR 1.0 vs 0.833; cost p50 400 ms / p95 721 ms vs 33 / 34 ms, under the 1500 ms budget. 9 queries only. Scott's call; the template stays `enabled: false`.
- Scratch artifacts to remove once accepted: `/media/saidler/4tb/claude-tmp/claude-1000/-home-saidler-repos-scottidler-second-brain/bbc0a79e-fab3-40cb-b4a9-a67b2afef0a2/scratchpad/phase5-data/` (migrated copy of `oracle.db` plus `eval-cache.db` with 49 new judgments, and `sb/oracle.log`) and `phase5-config/`. The ms-marco ONNX and safetensors files are now in `~/.cache/huggingface/hub`.

## Phase 6: `sb doctor` cpu section
### Design decisions
- One `cpu` `Section` (`sb/src/cli/checks/cpu.rs`, wired in `all_sections` between `classifier` and `embedding cache`): an info line listing every detected feature with the ONNX verdict, an ok/info/error line for the DB pin (with its parity bit from `model_spec`), and one `embed backend:` and one `oracle backend:` line from `vault::embedding::resolve_backend` on the detected CPU and the loaded cortex/oracle config. Pure core `findings_for(cpu, cortex_choice, oracle_choice, pin)` takes injected `CpuFeatures`; `findings()` is the thin loader.
- The pin is read through `SearchIndex::peek_active_model` (`vault/src/search/vector.rs`): a read-only SQLite connection that neither creates the file nor runs the schema migration. The cpu section therefore never writes the DB.
- A consumer whose config does not load is skipped (the config section already reports it), the `stop_timeout` house shape.
- Severity: `onnx` pinned on a CPU that is not `onnx_capable()` is a Warn naming the config key; any other resolve failure (for example `onnx` pinned on a model without parity) is an Error; `auto` falling to Candle is Ok with the reason in the line.

### Deviations
- "Reports the backend that actually loaded" (panel C7): the check reports what the loaders WILL resolve (same table, same CPU, same config), not a load. Loading a model in a health check costs a multi-second, memory-heavy load, and a download on a cold cache; the running processes' own INFO line (`inference backend: requested=.. resolved=..`) is the record of what actually loaded. A doctor-side load would only prove the doctor process, not cortex or oracle.
- The Warn for an unmigrated legacy `-candle` pin is an addition: before the Phase 3 migration has run on the DB the pin is not in the registry, so the section warns and says the rename happens when cortex or oracle on this build opens the DB.

### Tradeoffs
- Read-only peek in vault vs `SearchIndex::open` in sb: open migrates and writes; a doctor must not change what it inspects, and under the live old cortex it would run the migration.
- Resolution report vs a model load: see Deviations.

### Open questions
- Criterion "`sb doctor` on `ripr` prints a `[cpu]` line naming `avx512f` and `backend: onnx`": proven here in isolation, not on `ripr`; still due after the deploy (it is in the Acceptance Criteria). Note the first doctor run on a not-yet-migrated live DB shows the legacy-pin Warn and `backend: candle` until cortex or oracle on the new build opens the DB.
- Scratch to remove once accepted: `/media/saidler/4tb/claude-tmp/claude-1000/-home-saidler-repos-scottidler-second-brain/bbc0a79e-fab3-40cb-b4a9-a67b2afef0a2/scratchpad/phase6-data/` (copy of `oracle.db`, migrated by the run, plus strace logs and doctor output).

## Phase 7: Docs and comments sweep
### Design decisions
- The AVX-only framing now names `talos` wherever it survives: `cortex/AGENTS.md`, `cortex/src/embed.rs`, `cortex/src/config.rs`, `cortex/src/embed/tests.rs`, both templates, `bin/meeting`, `bin/dictate`. The 2026-08-16 incident text stays as history, reattributed to `talos` (where it still holds).
- `vault/src/search/AGENTS.md`: the "backend swap changes the string and re-embeds" invariant became the `onnx_parity` gate on `embedding::MODELS`, proven by `onnx_matches_candle_real_model`; the re-embed flow line names `cortex::embed::load_embedder` and `vault::embedding::load_model_version` instead of `load_active_model()`.
- Backend selection documented in one `CLAUDE.md` paragraph (under Hybrid retrieval, beside the embeddings pin) and one `vault/AGENTS.md` bullet.
- `vault/src/search/rerank.rs` status paragraph retitled and scoped to a CPU without AVX2+FMA; `oracle/src/config.rs` `RerankConfig` doc no longer says "this host".

### Deviations
- None. The doc's list names `vault/AGENTS.md`, `oracle/src/server/pipeline.rs`, `vault/src/embedding/candle.rs` for the sweep; no "AVX-only" assertion about the daemon host remained in them (earlier phases already reworded them), so they needed no change beyond the new `vault/AGENTS.md` bullet. The "embed uses fastembed" line in `cortex/AGENTS.md` was already gone (Phase 1).

### Tradeoffs
- Reattributing the 2026-08-16 incident comments to `talos` vs deleting them: the doc says the history stays, and the per-tick cap and pool isolation still guard `talos`.

### Open questions
- None.
