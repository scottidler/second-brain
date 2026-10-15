# Implementation notes: note sources, intent, tools, channels

## Phase 1: VTT overlap dedupe
### Design decisions
- Overlap check is a separate `overlap_remainder` helper, run only when `rolling_dedupe_action` returns Push (borg/src/youtube.rs, `parse_vtt_segments`). Today's prefix-growth rule is untouched and the new check runs in addition to it.
- A matching cue is pushed as a new segment carrying only the non-overlapping remainder, with its own start time; a fully covered cue is dropped. This keeps timestamps accurate for the new words.
- Overlap must be whole words and at least `MIN_OVERLAP_WORDS` (3), so a legitimately repeated short phrase is not eaten.
### Deviations
- None.
### Tradeoffs
- Fixed 3-word minimum vs. no minimum: avoids false merges at the cost of missing 1-2 word overlaps.
- `clean_vtt` (plain-text path) left unchanged vs. also fixing it: the phase names only `parse_vtt_segments`.
### Open questions
- None.

## Phase 2: Source archive write, archive-first reads, durability on `/trace`
### Design decisions
- New `borg::archive` module (`SourceArchive::write`, `is_durable`, `trace_dir`) owns the archive layout: `<root>/<trace>/distilled.yml`, plus `transcript.md` for repo (rendered README transcript) and thread (thread markdown). Video and article write `distilled.yml` alone, since its `transcript` is the source. `transcript.md` is written first and `distilled.yml` last, each temp-then-rename, so `distilled.yml` presence means a complete archive.
- The archive root is config, not a hardcoded path: borg `archive.root` and cortex top-level `archive-root`, both defaulting to `vault::paths::borg_sources_dir()`, mirroring the `staging.root` / `staging-root` pair. Both example configs document it.
- The write sits in one seam, `pipeline::archive_then_publish` (borg/src/pipeline/publish.rs), called by `process_url_inner` in place of the bare `atomic::publish_unique`. It writes the archive, then the note; `PublishError::ArchiveWrite` (and `NoteWrite`) both map to `FailureStage::PublishFailed` through `PublishError::failure_stage`, and `publish_failure_result` builds the terminal `IngestResult`. This made the "read-only archive dir fails the trace, no note" criterion testable without a live fetch.
- `distill_for_publish_repo` now returns `RepoDistillation { distilled, readme_transcript }` so the README the distiller saw reaches the archive whether or not staging is enabled (borg/src/stages/distill.rs).
- `vault::trace::dir(root, trace)` is the one per-trace path join, refusing anything but a single plain component. borg's archive writer, the `/trace` route and cortex's reader all go through it, so a `..` trace id from the route or from frontmatter never resolves outside the root.
- `vault::distilled::FILENAME` is the shared `distilled.yml` name; borg's `DISTILLED_FILENAME` now aliases it and cortex imports it.
- Cortex: `embed::source` (new file, split out to stay under the 1,500-line bloat limit) holds `SourceRoots { archive, staging }`, `read_source_transcript` (archive first, then staging) and `revive_archived_sentinels`. `process_batch` takes `SourceRoots` in place of `staging_root`.
- Sentinel retry: `revive_archived_sentinels` clears the transcript-chunk examined row of a staging-sourced note whose archived `distilled.yml` mtime is newer than the sentinel watermark (new `SearchIndex::sentinelled_notes` / `clear_embedding_examined_batch`). A re-sentinel raises the watermark to the archive mtime (`sentinel_watermark`), so an archive that exists but has no transcript cannot revive the same note every tick (the oscillation the sentinel exists to stop; pinned by `archive_without_transcript_does_not_revive_the_sentinel_every_tick`).
- `/trace/{id}` computes `source-durable` from `archive::is_durable` for both the found and the 404 response.
- Oracle's `trace_block` stays filesystem-free; its doc comment and the MCP server instructions now say `within-window` reflects staging retention only and point at the daemon's `/trace` `source-durable` for durability (the Architecture bullet on the trace block description).
### Deviations
- `PublishError` did not exist; it was created in `borg/src/pipeline/publish.rs` with two variants (`ArchiveWrite`, `NoteWrite`) rather than only the one the doc names, so the note-write failure also flows through the typed seam. Same `publish-failed` stage as before for the note write.
- The archive is written immediately before the note's atomic write, after the slide-aware path has already copied slide JPEGs into the vault. On an archive failure those attachments remain, exactly as they already do when the note-size gate fails at the same point. The note itself is never written, which is what the doc requires.
- Archiving covers the URL kinds the per-kind Resolved Decision names (video, article, repo, thread). Image, voicenote, idea/text, document and session paths are not archived: session is skipped by decision, and the others keep their verbatim text in the note body (`## Transcript`), so the decision does not list them.
- `TraceStateResponse` keeps its existing snake_case keys (`trace_id`, `failure_stage`) and adds `source-durable` (kebab) as the doc's API section spells it, with `#[serde(default)]` so a client reading an older daemon still parses.
### Tradeoffs
- Sentinel revival keyed on the archive file's mtime vs. a new "source appeared" column: the mtime needs no schema change and is in the same unit as `notes.modified_at` (unix seconds). Cost: an archive written in the same second as the sentinel does not revive it; the next archive rewrite or note edit does.
- Revival runs at the top of every transcript batch (one indexed query plus a stat per sentinelled note) vs. once per daemon tick: simpler wiring, and the sentinel set is small (~hundreds).
- Archive-first falls back to staging when the archived `distilled.yml` has no transcript, rather than treating an archive hit as final: the staged copy is the same bytes when present, and the fallback costs one read.
### Open questions
- None.
