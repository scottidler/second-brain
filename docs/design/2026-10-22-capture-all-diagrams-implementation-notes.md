## Phase 0: Measure the article filter
### Design decisions
- The draft `FIGURE_TAXONOMY_PROMPT` reuses the `TAXONOMY_PROMPT` category set and output schema verbatim and rewords only the framing (a figure on a web page, not a video frame): design doc "Phase 0 Results": so the same `SlideCategory` parser fits and Phase 5 copies the text unchanged.
- `app-ui` and `webpage` both cover website screenshots, and `b-roll` is widened to cover logos, icons, mascots, memes, and product photos: same prompt: the sample's dominant false-keep risks were XDA screenshots and decorative art, and none of those categories are in `keep`.
- Labels were fixed before the classifier ran and were not edited after: hand-labeling method: so the precision/recall are not tuned to the prompt.
- The prompt passed on its first draft, so there was no revision loop: Phase 0 gate: nothing was tuned.

### Deviations
- The classify call was made from a Python harness posting the same request body to the Messages API, not through Rust `ocr::vision_classify`: the phase said zero repo code and the Rust fn is not drivable without a harness crate; the endpoint, model (`claude-sonnet-5`), `max_tokens` (64), and body shape are identical. Same effect, different caller.
- Images over 3.5 MB or 2048 px were downscaled to a 2048 px jpeg before classify: the API's 5 MB per-image limit; 10 of 143 survivors exceed 2 MiB, max 3.2 MB, so this touched only the large-pixel-count images. Borg's real path will downscale at `max-bytes` (Phase 1).
- The prefilter was reimplemented in the harness (format, URL tokens, `min-side-px`, per-note sha256): the Rust prefilter is Phase 4; the harness mirrors the design's rules and does not claim to match that code.
- 40 sample notes are the 40 most recent readable stages with image refs (by mtime), not a random draw; the 60 labeled survivors are a seeded random draw from those.

### Tradeoffs
- 60 labels from 17 notes vs a draw across more notes: the 40-note sample yields 143 survivors from 27 notes, concentrated in a few image-heavy articles (XDA, stateof.ai, a jev post), so the labeled set is clustered. Recorded; the precision margin (one image) is flagged in the doc.
- Borderline labels (#30 Obsidian canvas screenshot, #41 portrait slide, #56 graph view) were called by eye and left as first labeled, against re-labeling to fit: the gate is only honest if labels are not adjusted.

### Open questions
- Is a precision margin of one image (0.909 vs the 0.9 gate) enough to proceed to Phase 5, or should the labeled set be widened with a second draw first? The criterion as written is met.
- Phase 4 SVG rasterization: the `resvg` CLI draws no text without a loaded font and skips CSS class styling on some SVGs (2 of 10 rendered black). Should Phase 4 load a `fontdb` fallback? The doc's Phase 4 does not mention fonts; this is recorded in Phase 0 Results.
