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
