## Phase 1: Config surface + taxonomy

### Design decisions
- `SlideCategory` custom `Deserialize` impl using `to_lowercase()` match — `borg/src/config.rs:SlideCategory::from_str_case_insensitive` — serde has no built-in `ignore_case`; a custom impl is the only path that accepts `"Code"`, `"CODE"`, and `"code"` as equivalent without a proc-macro dependency
- `ContentFilterConfig` uses `#[serde(default)]` at the struct level plus per-field `#[serde(default = "fn")]` — `borg/src/config.rs:ContentFilterConfig` — struct-level default allows the entire block to be omitted from YAML; field-level defaults fill in individual missing keys
- `enabled: false` in `ContentFilterConfig::default()` — `borg/src/config.rs:ContentFilterConfig::default` — design doc mandates the feature is off until Phase 5 validation; the hard default prevents any accidental live classification
- Separate `default_keep`, `default_max_vision_concurrency`, `default_min_confidence` free functions — `borg/src/config.rs` — serde's `default = "..."` attribute requires a callable path, not a closure or const; three short functions are the idiomatic Rust form
- `vision_per_slide: bool` removed from `YoutubeSlidesConfig` — `borg/src/config.rs:YoutubeSlidesConfig` — the field was declared and defaulted but never read anywhere in the codebase (confirmed by grep); removing it makes the struct consistent with its actual usage and eliminates dead code
- Constants `DEFAULT_MAX_VISION_CONCURRENCY` and `DEFAULT_MIN_CONFIDENCE` — `borg/src/config.rs` — numeric defaults pinned as named consts per the "no magic numbers" rule; tests reference the const to stay in sync if values change

### Deviations
- None.

### Tradeoffs
- Custom `Deserialize` vs `strum`/`serde_enum_str` for case-insensitive parsing — chose custom impl — avoids adding a new proc-macro dependency for a single enum; the match arm list is short and the compile-time cost is zero
- Hard error on unknown taxonomy string vs silently dropping it — chose hard error — the design doc explicitly specifies this; a mistyped category in `keep:` that silently becomes empty would produce confusing "no slides kept" behavior with no feedback

### Open questions
- None.

## Phase 2: Capture stage

### Design decisions
- Introduced `Run { start, end }` as a thin time-window type, not a member-frame container - `borg/src/slides.rs:Run` - the design Data Model explicitly leaves `Cluster` unchanged and has `best_frame` re-select from the caller's `frames` slice by the run's `[start,end]` window, so a run only needs to carry the window it spans
- `collapse_runs` merges adjacent clusters on a time-gap test (`gap <= RUN_MERGE_MAX_GAP_SECS`) rather than re-hashing canonical frames - `borg/src/slides.rs:collapse_runs` - the helper must be pure (no I/O, no decode) and the `Cluster` type stores no pHash; a small-gap merge stitches a continuously-drawn diagram's abutting growth fragments while a presenter pause (gap above threshold) keeps two distinct decks separate, which is the structural signal the doc names ("small inter-cluster gap"). The "pHash superset / monotonic growth" half of the doc's description is realized downstream by `best_frame` picking the largest (most-complete) frame in the merged window
- `RUN_MERGE_MAX_GAP_SECS = 2.0` as a named const - `borg/src/slides.rs` - the doc lists the stitch gap as an open question ("seconds-scale; tune empirically") and added no config field for it in Phase 1; a named const honors the no-magic-numbers rule and is the single tuning point
- `best_frame` stats file sizes with `fs::metadata` (no decode) and uses `is_none_or` to track the running max - `borg/src/slides.rs:best_frame` - max-JPEG-byte-size is the completeness proxy from the doc; a stat is cheaper than an image decode and the doc characterizes reading a handful of local JPEG sizes as negligible pure-prefix work
- Window membership is inclusive on both ends (`>= start && <= end`) - `borg/src/slides.rs:best_frame` - a run's terminal frame sits exactly at `end`, so an exclusive upper bound would drop the most-complete frame this helper exists to pick
- Logging: `collapse_runs`/`best_frame` log entry/exit at DEBUG; the per-cluster merge decision and per-frame stat are TRACE (tight loops) per the logging rule; `shape_from_kept_count` is a trivial three-arm match and carries no log
- `shape_from_kept_count` is a direct `match` on count - `borg/src/slides.rs:shape_from_kept_count` - 0 -> TextOnly, 1 -> Hero, >=2 -> SlideSection, exactly the doc's mapping

### Deviations
- The doc's `collapse_runs` prose mentions a "pHash superset" merge condition; the implemented merge condition is purely the time-gap test (no re-hash), with completeness handled by `best_frame`'s max-byte-size selection. This keeps `collapse_runs` a pure, decode-free helper as the "Pure helpers in slides.rs (no I/O, no network)" constraint requires, and keeps `Cluster` unchanged as the Data Model mandates. The gap threshold is the tunable knob the doc's open question anticipates.

### Tradeoffs
- Gap-only merge vs re-hashing canonical frames to verify a pHash superset - chose gap-only - re-hashing would make the helper do decode I/O (violating the pure-helper constraint) and would require threading hashes through or re-reading files; the gap test plus downstream best-frame selection covers the same intent (stitch a growing diagram, separate distinct decks) without that cost. Risk of over-merging two genuinely back-to-back diagrams with no pause is accepted and noted below
- `Run` as a bare window vs carrying its constituent cluster indices - chose bare window - nothing in Phase 2 or the documented Phase 3/4 consumers needs the member clusters once the window is known (classification and best-frame both key off the window), so the smaller type is the right surface

### Open questions
- `RUN_MERGE_MAX_GAP_SECS` is fixed at 2.0s with no config surface. If empirical tuning (the doc's open question) shows it needs to be operator-tunable, it should become a `content-filter` config field in a follow-up; Phase 2 does not add config.
- Over-merge guard: two distinct diagrams shown truly back-to-back with a sub-2s transition would collapse into one run. The doc's risk table mitigates this with "monotonic pHash growth + small gap"; this implementation relies on the small-gap half only. Confirm against a labeled sample whether the gap test alone is sufficient or a pHash-superset check (which would require giving `collapse_runs` access to hashes) is warranted.

## Phase 3: Vision classifier
### Design decisions
- Factored a private `ocr::vision_call` shared core - `borg/src/ocr.rs:vision_call` - holding the API-key resolve, base64 encode, Messages-API body build, permit acquire, timeout, and response-text extraction in one place; `vision_extract` and `vision_classify` are thin wrappers over it (prompt + max-tokens differ). This is the "share the HTTP/auth/base64 core" the doc's API Design calls for, with no duplication.
- Process-wide vision permit pool implemented as `static VISION_PERMITS: OnceLock<Arc<Semaphore>>` in `borg/src/ocr.rs` (the doc's `OnceCell<Semaphore>`; the repo uses `std::sync::OnceLock`, matching the existing `pipeline::permits::PermitPool` pattern rather than adding the `once_cell` crate). `init_vision_permits(cap)` is `pub` for Phase 5/startup wiring; `acquire_vision_permit` is held across the HTTP call inside `vision_call`, gating BOTH `vision_classify` AND the existing image-ingest `vision_extract`.
- Uninitialized pool = ungated (returns `None` permit) - `borg/src/ocr.rs:acquire_vision_permit` - so the pre-pool `vision_extract` behaviour is preserved when startup never sized the pool (and unit tests that exercise only the parser never touch a semaphore).
- `SlideClass` placed in `borg/src/config.rs` alongside `SlideCategory` (Phase 1), per the doc's Data Model "config-owned" placement; `slides/classify.rs` and `slides.rs` import it.
- Taxonomy prompt is a single `const TAXONOMY_PROMPT` - `borg/src/slides/classify.rs` - forcing classification by dominant region (ignore webcam insets), a sharp diagram-vs-infographic/chart line, and a two-line `CATEGORY:`/`CONFIDENCE:` structured reply.
- Parser is fail-closed - `borg/src/slides/classify.rs:parse_classification` - returns `Err` on missing CATEGORY/CONFIDENCE, unknown category, non-numeric or out-of-range confidence, empty/garbage replies, and conflicting CATEGORY lines (ambiguous). An `Err` drops only that run downstream.
- `classify_slides` fans out over a `tokio::task::JoinSet` and reorders results back to input order by spawned-task index - `borg/src/slides/classify.rs:classify_slides` - so the returned vector aligns positionally with `best_frames`; concurrency is left to the process-wide permit pool, no second per-video gate. A join error (panic) maps to an `Err` so a panicked task drops only its run, never the batch.
- `Slide.caption: Option<String>` replaced by `Slide.class: Option<SlideClass>` - `borg/src/slides.rs:Slide` - both `skip_serializing_if`; per the Data Model the swap is schema-neutral for `slides.yml` because `caption` was always `None`.

### Deviations
- `ocr::vision_classify` takes an extra `prompt: &str` parameter not shown in the doc's API Design signature (which listed only `image_data, mime_type, vision_config, llm_config`). The doc's prose says classify.rs "calls `ocr::vision_classify` with a taxonomy prompt" and the prompt is classify.rs's concern, so passing it in (rather than hardcoding a taxonomy prompt inside `ocr`) keeps `ocr` taxonomy-agnostic and is the cleaner factoring. Functionally equivalent; signature is one parameter wider.
- Derived `Clone` on `config::LlmConfig` (`borg/src/config.rs`) - it was the only one of the three config types `classify_slides` needs to move into spawned `'static` tasks that lacked it (`ContentFilterConfig`/`VisionConfig` already derive `Clone`). Additive, harmless for a plain config struct.
- Removed the dead "Visual caption" rendering block in `slides.rs:render_pattern_input` (it read `slide.caption`, now gone). The block was dead because `caption` was always `None`; its corresponding test assertions were dropped. Classification-aware rendering, if any, belongs to Phase 4 and is out of Phase 3 scope.

### Tradeoffs
- `OnceLock<Arc<Semaphore>>` (stdlib, mirrors `pipeline::permits`) vs adding the `once_cell` crate the doc names - chose stdlib to avoid a new dependency and to match the established in-repo permit-pool pattern.
- `JoinSet` + clone-into-task vs a borrowing `join_all` from the `futures` crate - chose `JoinSet` to avoid adding `futures` (the doc mandates no new crate); the cost is cloning three small config values per run, negligible against a network call.
- Permit pool "uninitialized = ungated" vs "uninitialized = panic" - chose ungated to preserve the existing image-ingest `vision_extract` path's behaviour in any code path or test that issues a vision call before startup wiring; the trade is that a missed `init_vision_permits` call silently removes the cap rather than failing loudly (Phase 5 owns the startup wiring that guarantees init).

### Open questions
- The doc's API Design signature for `vision_classify` omits the `prompt` parameter this implementation adds; confirm the wider signature is acceptable (it is the cleaner layering) or whether the taxonomy prompt should instead live inside `ocr`.
- `init_vision_permits` is `pub` but not yet called anywhere - Phase 4/5 must wire it at startup from `content-filter.max-vision-concurrency`. Until then the pool is ungated (see the tradeoff above).
