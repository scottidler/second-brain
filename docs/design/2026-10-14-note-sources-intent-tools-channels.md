# Design Document: Durable Sources, Capture Intent, Tools & Commands, YouTube Channels

**Author:** Scott Idler (drafted with Claude)
**Date:** 2026-10-14
**Status:** Implemented
**Review Passes Completed:** 5/5 (draft; correctness: Phase 7 pattern count and the `extension sign` verb checked against source; clarity: one archive-count command; edge cases: query encoding, archive write failure, popup auto-capture loss; excellence: voice lint). Review panel round 1 folded in (synthesis `~/.cache/review-panel/runs/0806kuTj/synthesis.md`).

## Summary

Borg notes work as a pointer plus a summary, and they lose value in four places:
- The verbatim source expires.
- Nothing records why a source was saved.
- Actionable tools and commands are buried or dropped.
- There is no way to list or search the YouTube channels Scott follows.

This doc:
- Keeps every distilled source in a durable archive outside staging.
- Threads a capture note from every URL ingest path into the distiller.
- Adds a `## Tools & Commands` body section.
- Gives oracle a YouTube-only creator list and a channel search.

Each part names how it gets measured: an `sb borg eval` (v0.16.0) metric or a literal command.

## Problem Statement

### Background

- Scott asked (2026-10-14): "are we getting the most value out of these transcripts, blogs, githubs etc?" Two read-only sweeps answered: one over 2,849 vault notes, one over past sessions.
- He picked suggestions 1-3 from that answer ("write the design doc for 1-3"), then asked for the YouTuber query as Part 4 ("A, fold it in as part 4").
- Settled decisions this doc keeps:
  - `## Transcript` stays out of Video/Article/Repo note bodies (`docs/design/2026-07-07-distillation-output-restore.md`, `distillers/src/render.rs:12-30`).
  - Tag proposals read staging only; the retention window is their stated limit, "no shadow accumulator" (`docs/design/2026-09-21-staged-tag-proposals.md:414`).
  - No separate Best Quotes section; `Claim.quote` carries quotes (2026-07-07).
- Settled decision this doc **supersedes**: 07-07 Resolved Decision "post-60-day custody: accept re-embed loss ... A durable transcript store is unrequested scope today; revisit condition: the first model bump". The new evidence:
  - Scott now requests it.
  - 5 of about 12 note-reading sessions fell back to the source.
  - Retention dropped to 14 days.
  - Session d60baa92 found its source expired.

### Problem

**1. The source expires.**
- 5 of about 12 sessions that read notes fell back to the transcript, every time for exact wording (sessions c889cd0c, 53127aa1, 1366e3fd, d60baa92, e533ae12).
- In d60baa92 (2026-09-30), "the stored copy had expired", so the transcript was re-fetched from YouTube.
- Retention dropped from 60/90 days to 14/21 days in commit 1cf6a4d (2026-08-19) (`borg/src/config.rs:883-884`).
- `borg::retention::sweep` (`borg/src/retention.rs:67-115`) deletes the whole trace dir. Nothing schedules it today: the only caller is `sb borg retention sweep` (`sb/src/cli/borg.rs:222`).
- Measured on ripr: `--dry-run` would delete 48,512 traces, including 969 of the 1,361 `distilled.yml`.
- 1,201 of 2,259 vault traces already lack a staged `distilled.yml`; 1,123 of those were ingested before 2026-08 on another host. Only a re-fetch brings them back.
- Where each kind's source lives today. Of the 1,361 staged `distilled.yml`, 620 carry `transcript` and 741 do not:

| Kind | Source location | Notes |
|---|---|---|
| video | `distilled.yml` `transcript` | 394 of 394 |
| article | `distilled.yml` `transcript` | 182 of 196 |
| repo | `transcript.md` or the GitHub envelope in `fetched.html` | `distillers/src/repo.rs:224` sets `transcript: None` |
| thread | the note body | Thread keeps `## Transcript` |
| session | `body.txt` | clyde keeps a durable staged copy of every session |

- Oracle's `within-window` (`oracle/src/server.rs:277-307`) compares today against `trace-expires`, so it reports `false` for sources still on disk.
- YouTube transcripts still carry rolling auto-caption duplication: "I believe that Vit Plus is going to be I believe that Vit Plus is going to be" (trace ht-2694026d).
  - Cause: `rolling_dedupe_action` (`borg/src/youtube.rs:60-66`) only handles prefix growth.
  - 330 of 394 staged video transcripts show it.
  - It roughly doubles stored text and pushes videos over the 12K-token map-reduce threshold.

**2. Nothing records why Scott saved a source.**
- `sb borg ingest` has no `--note` (`sb/src/cli/borg/args.rs:33-49`). The client body sends url/tags/force/method only (`borg/src/client.rs:421-426`).
- The Firefox popup sends `{url}` on open (`borg/clients/extension/popup.js:39-60`).
- Those two paths produced 952 of 1,404 YouTube notes, and 0 YouTube notes carry a capture note.
- The 58 capture notes that exist all came from Telegram:
  - 49 are Android share-sheet boilerplate (`Source: XDA`).
  - Most of the rest are page titles.
  - About 1 states intent.
- `router::extract_capture_note` (`borg/src/router.rs:88`) takes share text as intent. That junk already reaches `## Why Captured`, the summary embedding (`cortex/src/embed.rs:708`), and single-call distills via `compose_capture_input` (`distillers/src/parse.rs:815`).
- The map-reduce path drops the note: `compose_capture_input` runs only on single-call paths (`article.rs:91-97`, `video.rs:117-123`, `thread.rs:101-109`, `session.rs:178-184`). This was disclosed as deferred in `docs/design/2026-07-05-distillation-knowledge-extraction.md:311`.
- No `borg/patterns/distill-*.md` says what to do with the note.
- Replay drops it (`borg/src/replay.rs:252-257`). The intake sidecar records URL bytes only (`borg/src/routes.rs:110-117`), so a failed trace has no note to replay.
- `--force` reingest drops it:
  - `capture-note` is not a preserved key.
  - The preserve reader is line-based (`borg/src/pipeline/publish.rs:225-260`).
  - The preserve writer filters on `CORTEX_PRESERVE_KEYS` and writes `{key}: {v}` raw (`borg/src/pipeline/atomic.rs:359-366`). A multiline note would become invalid YAML there.

**3. Actionable content is buried.**
- The repo install command renders only to frontmatter `cortex-repo-install`, and is dropped when over 500 chars (`render.rs:123-129`).
- 0 of 136 GitHub notes have a code block in the body.
- FTS indexes title/body/tags/summary/claims (`vault/src/search/schema.rs:497-536`), so a frontmatter-only install command is unsearchable.
- Tools named in videos (bacon, nextest, cargo-deny) get no URL and no install command. The only tool extraction today is `owner/repo` slugs from the description into `VideoPayload.repos` (`borg/src/stages/distill.rs:718-722`).

**4. No way to list or search Scott's YouTubers.**
- `creator_browse` (`oracle/src/server.rs:936-975`) returns only name and count, with no type filter. Hacker News (20), Michael Larabel (13) and Throw Deep Publishing (11) sit among the YouTubers.
- YouTube notes store only the channel name. `fetch_metadata` (`borg/src/youtube.rs:91-128`) runs `yt-dlp --dump-json` and discards `channel_id`, `channel_url`, `uploader_id` and `upload_date`.
- WebSearch returns no youtube.com URLs for channel queries (the 2026-10-14 scour's creator lane), so an agent cannot search a channel today.

### Goals

- Every distilled source written from now on survives the staging sweep, without bloating the vault.
- A capture note can come from every URL ingest path, survives replay and reingest, and shapes the distill on short and long inputs.
- Tools and commands land in a searchable body section, repo install included.
- Oracle lists YouTube creators with their handles, and searches a followed channel.
- Each part has a measurement: an eval metric, a test, or a literal command.

### Non-Goals

- **Recovering the 1,201 sources already gone.** Excluded: only a re-fetch brings them back, and that is a bulk ingest Scott orders explicitly.
- **Automating the retention sweep.** Parked: revisit after Phase 3 lands and the archive is backfilled.
- **Changing what tag proposals read.** Excluded: settled in the 09-21 doc.
- **Putting `## Transcript` back in note bodies.** Excluded: settled 2026-07-07.
- **Capture notes on voicenote, image and idea ingests.** Excluded: the operator's own words are the content there.
- **Adding `## Tools & Commands` to notes already distilled.** Excluded: `cortex summarize --backfill` skips distilled notes (`cortex/src/summarize.rs:293`). A `--force` reingest adds it note by note.
- **Anthropic Citations or MiniCheck as a grounding gate.** Parked in the Addendum.
- **The other template issues from the same sweep** (duplicate tldr/Key Ideas, promo links, low-information tags, session note dates, failed distills published as notes). Parked: Scott picked 1-3 plus channels.

## Proposed Solution

### Overview

| Part | Change | Owner crate(s) |
|---|---|---|
| 1 | VTT overlap dedupe; a durable per-trace source archive, written before the vault note; archive-first reads; backfill verb; durability reported by the daemon | borg, vault, cortex |
| 2 | Share-boilerplate filter; `--note` on ingest; the note survives replay and reingest; the note reaches map-reduce; prompts say how to use it; popup note field | borg, distillers, sb, extension |
| 3 | `tools` field on `Distilled`; prompts extract it; `## Tools & Commands` section; repo install moves into it; Links never repeat a tool URL; eval metric | vault, distillers, borg |
| 4 | `type` filter and handle on `creator_browse`; persist channel id/handle/url/published date; backfill; `channel_search` | vault, oracle, borg, sb |

### Architecture

**Part 1: source archive**
- New path `vault::paths::borg_sources_dir()` -> `~/.local/share/sb/borg/sources/<trace>/`.
- Archive contents per kind: `distilled.yml` plus the source the distiller saw (see Resolved Decisions). Sessions are skipped.
- Borg is the sole writer.
  - It writes the archive before the vault write, independent of `staging.enabled`.
  - A failed archive write returns a typed `PublishError::ArchiveWrite`, which classifies as `FailureStage::PublishFailed` (`vault/src/receipts.rs:33`). The vault note is not written.
  - Today's staged writes keep their WARN-and-continue behavior (`borg/src/stages/distill.rs:306-308`). Only the archive seam fails closed.
- A reingest gets a new trace id and a new archive dir. The archive grows without pruning by design.
- `cortex::embed::read_staged_transcript` resolves the archive first, then staging (`cortex/src/embed.rs:1339-1380`).
  - A note sentinelled as "missing source" (`embed.rs:1343-1346`) is retried when its archive dir exists, so the backfill revives skipped notes.
- `cortex::proposals` keeps scanning staging only.
- `sb borg eval --staged` keeps reading staging.
- Durability: the daemon's `GET /trace/{trace_id}` (`borg/src/server.rs:59`, `routes::trace_state`) adds `source-durable: true|false`.
  - Oracle's `trace_block` stays a pure, filesystem-free function (`oracle/src/server.rs:267-269`), because oracle runs on hosts that hold no archive.
  - The trace block description points callers at `/trace` for durability.
- The archive lives outside the vault, following the precedent that moved sidecars out because they bloated the vault and its git (`borg/src/retention.rs:11-13`).

**Part 2: capture intent**
- Filter at two seams:
  - `router::extract_capture_note` drops `Source: <site>` lines. It splits on lines before collapsing whitespace, so a line rule can see them.
  - Publish drops a note equal to the fetched title, since the title only exists after the fetch.
  - An empty result is `None`.
- `sb borg ingest --note "<text>"` -> `borg::client` body `note` -> the existing `IngestRequest.note` (`borg/src/types.rs:317-322`). The daemon already accepts it.
- Intake records the note with the URL, so replay of a failed trace keeps it. Replay of a published trace reads `capture-note:` from the note.
- A borg-owned preserve list (named for what it is, not `CORTEX_PRESERVE_KEYS`) carries `capture-note` across `--force` reingest. Both halves change:
  - The reader (`publish.rs:225-260`) parses block scalars.
  - The writer (`atomic.rs:359-366`) emits through `yaml_scalar` (`markdown.rs:355`), not a raw `format!`.
- `build_reduce_input` (`distillers/src/parse.rs:635-640`) and `build_thread_reduce_input` take `note: Option<&str>`. Every long-path caller in `article.rs`, `video.rs`, `thread.rs` and `session.rs` passes it.
- Patterns `distill-{video,article,thread,session}` and their `-reduce` siblings, plus `distill-repo`, get one instruction block for the note:
  - Weight tldr and claim selection toward the stated interest.
  - Never invent relevance the source does not support.
  - Ignore the note when it is empty.
- Popup: a textarea plus a Capture button. Enter submits, and an empty note is a bare capture.

**Part 3: Tools & Commands**
- `Distilled.tools: Vec<Tool>`, `#[serde(default)]`, bounded in `validate.rs` the way `enumeration` is (`distillers/src/validate.rs:87,116-130`).
- Video, article and repo patterns (and the chunk/reduce triples) extract tools.
- `render.rs` emits `## Tools & Commands` after Claims and before Links.
  - Each tool is one bullet: name, URL if present, then the command in a code span.
  - A multiline command goes in a fenced block under the bullet.
- `RepoPayload.install` renders in that section. Frontmatter `cortex-repo-install` stays for compatibility, still capped at 500.
- Dedup happens in `render`, for every kind: a `## Links` entry whose URL a tool already carries is dropped. `merge_description_links` is unchanged.

**Part 4: YouTube channels**
- `creator_stats` and `notes_by_creator` (`vault/src/search/stats.rs:374-383`) take an optional note type. `notes.note_type` is already indexed (`idx_notes_note_type`, `schema.rs:72`).
- Discovery chain: yt-dlp `--dump-json` -> `VideoPayload` -> frontmatter -> the oracle index -> `creator_browse`.
  - `VideoPayload` gains `channel_id`, `channel_handle` (yt-dlp `uploader_id`, e.g. `@letsgetrusty`), `channel_url` and `published_at` (from `upload_date`).
  - Render writes `channel-id`, `channel-handle` and `channel-url` to frontmatter.
  - `published_at` stops being hardcoded `None` (`borg/src/stages/distill.rs:46-70`).
  - The index stores `channel_handle`, and `creator_browse` with `type: youtube` returns `handle` per creator.
- Backfill: `sb borg channels --backfill [--apply]`.
  - Dry-run by default. Never clobbers.
  - Writes through the existing atomic note writer (`borg/src/pipeline/atomic.rs`).
  - Modeled on the creator backfill (`borg/src/audit.rs:501-560`).
  - One `yt-dlp --dump-json` per note, because staging holds no channel id.
- Search: `borg::youtube::search_channel(handle, query, limit) -> Result<Vec<ChannelHit>>`.
  - It runs `yt-dlp --flat-playlist -J --playlist-end N "https://www.youtube.com/@<handle>/search?query=<q>"` with an 8 s timeout, below the 10 s daemon-client timeout (`vault/src/daemon.rs:20`).
  - It keeps `watch?v=` entries only, because the endpoint also returns playlists.
  - It is exposed as the `sb borg channel-search` verb and a bearer-gated daemon route `GET /channels/search`.
  - A yt-dlp failure or timeout is a 502 with the yt-dlp stderr tail.
  - Oracle's `channel_search` tool calls the route through `DaemonClient`, the way `ingest_queue` calls `GET /queue` (`oracle/src/queue.rs:1,50-56`). It is registered in the MCP router and in `sb oracle call` dispatch.
  - Each hit carries `in-vault: <path>` when the vault already has that video.

### Data Model

```rust
// vault/src/distilled.rs
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Tool {
    pub name: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub command: Option<String>,   // install or usage, verbatim from the source
}

// Distilled
#[serde(default)]
pub tools: Vec<Tool>,

// VideoPayload additions, all #[serde(default)]
pub channel_id: Option<String>,
pub channel_handle: Option<String>,
pub channel_url: Option<String>,
pub published_at: Option<String>,   // existing field, now populated
```

- All additions are `#[serde(default)]`, so all 1,361 staged `distilled.yml` files still deserialize.
- Bounds: `tools` is capped at 20 entries, and `command` at 500 chars, the same ceiling as `cortex-repo-install`. Overflow is counted in `ValidationMeta.bounds_truncations`.

### API Design

```
sb borg ingest <URL> [--note <TEXT>] [-t TAGS...] [--force]
sb borg retention archive [--apply]          # backfill sources/ from staging; dry-run default
sb borg channels --backfill [--apply]        # channel id/handle/url on youtube notes
sb borg channel-search <@HANDLE> <QUERY> [--limit N] [--format yaml|json]

GET /trace/{trace_id}                          (existing; adds "source-durable")
GET /channels/search?handle=@x&q=...&limit=N   (new, bearer-gated)

oracle creator_browse { type?: "youtube", creator?, tags?, tags_mode?, detail?, limit? }
                      -> with type youtube, each creator carries "handle"
oracle channel_search { handle: "@x", query: "...", limit?: 10 }
```

### Implementation Plan

Within each part, deterministic work comes first and prompt work last. The eval harness lands before the prompts it measures. The parts ship independently.

#### Phase 1: VTT overlap dedupe
**Model:** sonnet
- In `parse_vtt_segments` (`borg/src/youtube.rs`), collapse a cue whose text starts with the previous cue's suffix. These are ASR "previous line + new line" cues, and the check runs in addition to today's prefix-growth rule.
- **Success criteria:**
  - A fixture VTT with two-line rolling cues renders each sentence exactly once (new unit test).
  - A fixture with a deliberate non-adjacent repeat keeps both copies.

#### Phase 2: Source archive write, archive-first reads, durability on `/trace`
**Model:** opus
- Add `vault::paths::borg_sources_dir()`.
- Archive contents per the per-kind Resolved Decision.
- `PublishError::ArchiveWrite` maps to `PublishFailed`. The archive is written before the vault write, independent of `staging.enabled`.
- Cortex embed resolves the archive first and retries the missing-source sentinel when an archive dir exists.
- `GET /trace/{id}` adds `source-durable`.
- **Success criteria:**
  - Unit test: delete the staging trace dir, keep the archive, and the embed still resolves the transcript (mirrors `cortex/src/embed/tests.rs:318-332`).
  - Unit test: an archive write into a read-only dir fails the trace with `failure_stage = publish-failed`, and no vault note is written.

#### Phase 3: Archive backfill verb
**Model:** sonnet
- `sb borg retention archive [--apply]` copies every staged trace's source artifacts that have no archive dir yet. Dry-run by default. Never overwrites.
- Existing video transcripts are archived as staged, duplicated text included. Phase 1 fixes new ingests only.
- Operator step (Scott, on the daemon host ripr): run it with `--apply` before anyone runs `sb borg retention sweep`.
- **Success criteria:**
  - After `--apply` on ripr, `fd -t d . ~/.local/share/sb/borg/sources --max-depth 1 | wc -l` is >= the dry run's `to_copy` count. Amended at the implementation audit: the first draft said >= 1,361, but that count includes session traces, which the archive skips by design. The dry run on 2026-10-15 reported scanned=1367, to_copy=718, session=649.
  - A second run reports 0 to copy.

#### Phase 4: Capture-note boilerplate filter
**Model:** sonnet
- The router drops `Source: <site>` lines, splitting on lines before it collapses whitespace.
- Publish drops a note equal to the fetched title.
- **Success criteria:**
  - Input `"Source: XDA\nhttps://..."` yields `None`.
  - Input `"Great overview of always-on agents. (bookmark it) https://..."` keeps its note text.
  - Applied to the 58 existing vault values, between 1 and 10 survive.

#### Phase 5: Borg-owned preserve key, reader and writer
**Model:** opus
- A borg-owned preserve list carries `capture-note`.
  - The reader (`publish.rs:225-260`) parses block scalars.
  - The writer (`atomic.rs:359-366`) emits through `yaml_scalar`.
- **Success criteria:**
  - A `--force` reingest of a note with a multiline capture note yields the same decoded `capture-note` value (unit test round-trip through reader, writer and YAML parse).
  - The written frontmatter parses as YAML.

#### Phase 6: `--note` through CLI, client, intake, replay
**Model:** sonnet
- `sb borg ingest --note`. The `borg::client` body carries `note`. Intake records the note.
- Replay sends the note: from intake for failed traces, from `capture-note:` for published ones.
- **Success criteria:**
  - A replay of a failed trace ingested with `--note` sends the note (unit test on the replay body).
  - `borg/tests/extension_body_matches_ingest_request.rs` still passes (the field is additive).

#### Phase 7: `tools` contract
**Model:** sonnet
- `Tool`, `Distilled.tools`, and validate bounds.
- **Success criteria:**
  - Every staged `distilled.yml` deserializes: `sb borg eval --staged --no-judge` exits 0 and prints `note-size: N/N`.
  - Round-trip test for a `Distilled` with tools.

#### Phase 8: Tools render section, repo install in body, Links dedup
**Model:** sonnet
- `render.rs` emits `## Tools & Commands` with `RepoPayload.install` in it, and drops `## Links` entries whose URL a tool carries, for every kind.
- **Success criteria:**
  - A repo fixture with an install line renders the command in the body.
  - `sb oracle call knowledge_search --json '{"query":"<that command>","mode":"bm25"}'` finds the note after reindex.
  - A URL present in `tools` appears once in the rendered body.

#### Phase 9: Eval harness for Parts 2 and 3
**Model:** opus
- A fixture may carry a `capture-note.txt`. A one-time `sb borg eval --synth-intents` writes one per fixture: an LLM states a plausible reason to save that source, and the file is committed. No note is hand-written.
- `sb borg eval --regenerate` re-distills each fixture's `source.md` through the current patterns, once with the note and once without, then grades both.
- A deterministic `tools-grounding` metric:
  - A tool name is grounded by the shingle rule (`borg/src/eval/calc.rs`).
  - A command is grounded only by an exact, case-sensitive substring of the source after whitespace collapse.
- `judge_note_text` adds the tools section.
- **Success criteria:**
  - The report prints a `tools-grounding:` line.
  - Removing a fixture's tools changes that line.
  - A fixture command `cargo test --release` against a source holding `cargo test release` scores ungrounded (unit test).

#### Phase 10: Capture note into map-reduce, plus note pattern guidance
**Model:** opus
- `build_reduce_input` and `build_thread_reduce_input` take `note`, and every long-path caller passes it.
- The distill patterns get the note instruction block.
- **Success criteria:**
  - Unit test: a long-path video distill with a note includes the note text in the reduce input.
  - `rg -l "capture note" borg/patterns/distill-*.md | wc -l` >= 9 (video, article, thread, session, their 4 reduce patterns, and repo).
  - `sb borg eval --regenerate` on the capture-note fixtures shows no more `unsupported` claim verdicts with the note than without.

#### Phase 11: Tools extraction prompts
**Model:** opus
- Video/article/repo patterns and their chunk/reduce siblings extract `tools`.
- **Success criteria:**
  - `sb borg eval --regenerate` on the repo and video fixtures prints `tools-grounding:` at or above 90% for commands.

#### Phase 12: Popup note field
**Model:** sonnet
- Textarea plus Capture button in `popup.html`/`popup.js`. The body sends `{url, note}`.
- **Success criteria:**
  - Live check: `sb borg extension stage --to ./dev-ext`, load it in `about:debugging`, capture a page with a note, and the resulting vault note carries that `capture-note:`.
  - Operator step after that: `sb borg extension sign`, then `otto deploy`.

#### Phase 13: `creator_browse` type filter
**Model:** sonnet
- `creator_stats(note_type)`, `notes_by_creator(.., note_type)`, `CreatorBrowseRequest.type`.
- **Success criteria:**
  - `sb oracle call creator_browse --json '{"type":"youtube","limit":25}'` contains neither "Hacker News" nor "Michael Larabel".

#### Phase 14: Persist channel metadata
**Model:** sonnet
- Keep `channel_id`, `uploader_id`, `channel_url` and `upload_date` from `yt-dlp --dump-json` in `VideoPayload` and frontmatter.
- **Success criteria:**
  - A re-ingest of `https://www.youtube.com/watch?v=53XYcpCgQWE` writes `channel-handle: '@letsgetrusty'`.

#### Phase 15: Channel backfill
**Model:** sonnet
- `sb borg channels --backfill [--apply]`, one `yt-dlp --dump-json` per YouTube note, atomic writes, never clobbers.
- Operator step: Scott runs `--apply` on ripr. It is a network loop over about 1,400 notes.
- **Success criteria:**
  - After `--apply`, `rg -l '^channel-handle:' ~/repos/scottidler/obsidian/notes | wc -l` is >= 1,300 (some videos are deleted or private).

#### Phase 16: Channel search in borg (lib, CLI, daemon route)
**Model:** opus
- `borg::youtube::search_channel`, the `sb borg channel-search` verb, and `GET /channels/search` with the 8 s bound and the 502 error shape.
- **Success criteria:**
  - `sb borg channel-search @letsgetrusty tokio --limit 10` prints >= 1 `watch?v=` hit and no playlist entries.
  - A handle failing `^@[A-Za-z0-9._-]+$` is rejected before yt-dlp runs (unit test).

#### Phase 17: Oracle channel search and creator handles
**Model:** sonnet
- The index stores `channel_handle`, and `creator_browse` with `type: youtube` returns `handle`.
- The oracle `channel_search` tool calls `GET /channels/search` and is registered in the MCP router and in `sb oracle call`.
- **Success criteria:**
  - `sb oracle call creator_browse --json '{"type":"youtube","limit":5}'` shows a `handle` on each entry after Phase 15's backfill.
  - With the daemon stopped, `sb oracle call channel_search --json '{"handle":"@letsgetrusty","query":"tokio"}'` errors with the daemon address in the message.

## Acceptance Criteria

- [ ] `fd -t d . ~/.local/share/sb/borg/sources --max-depth 1 | wc -l` >= the `sb borg retention archive` dry run's `to_copy` count on ripr after Phase 3, and a trace whose staging dir is gone still yields its transcript to cortex embed.
  - Amended at the implementation audit: the first draft said >= 1,361, which counts session traces that the archive skips by design. Dry run on 2026-10-15: scanned=1367, to_copy=718, session=649.
  - Observed on main: `ls: cannot access '/home/saidler/.local/share/sb/borg/sources': No such file or directory`.
- [ ] `sb borg ingest` takes `--note`, and a `--force` reingest keeps the decoded `capture-note` value.
  - Observed on main: `sb/src/cli/borg/args.rs:33-49` declares `url`, `--clipboard`, `--file`, `--tags`, `--force` and no `--note`.
  - Running `sb borg ingest --help` is denied by the bulk-ingest guard (`intent-guard.sh`), which reads it as an ingest with a non-literal URL. The criterion is checked from source and a unit test.
- [ ] `rg -l '^## Tools & Commands' ~/repos/scottidler/obsidian/notes | wc -l` > 0 after the first new repo or video ingest, and `sb borg eval --staged --no-judge` prints a `tools-grounding:` line.
  - Observed on main: `0` notes. The eval prints no `tools-grounding` line.
- [ ] `sb oracle call creator_browse --json '{"type":"youtube","limit":25}'` succeeds without Hacker News.
  - Observed on main: `Error: creator_browse: unknown field 'type', expected one of 'creator', 'tags', 'tags_mode', 'detail', 'limit'`.
- [ ] `sb borg channel-search @letsgetrusty tokio --limit 10` returns >= 1 `watch?v=` hit.
  - Observed on main: the verb does not exist yet (Phase 16).
  - The underlying `yt-dlp --flat-playlist -J --playlist-end 10 "https://www.youtube.com/@letsgetrusty/search?query=tokio"` returned 10 entries in 0.82 s on ripr (yt-dlp 2026.08.19). Neither review seat reproduced this, because the seats' sandbox had no DNS.

## Resolved Decisions

- **2026-10-14: The archive holds `distilled.yml` plus the source the distiller saw, per kind.**
  - Video and article: the `transcript` inside `distilled.yml`.
  - Repo: `transcript.md`, else the README decoded from the Stage-0 envelope (`borg::github::readme_from_stage0`).
  - Thread: `transcript.md` when staged.
  - Session: skipped, because clyde keeps a durable staged copy.
  - Grounds: Scott uses the vault as a store that agents query, and when a note falls short, the exact source is what they fall back to ("i treat them as a data store to RAG and query and research on"). Panel round 1 measured 741 of 1,361 staged `distilled.yml` without a transcript.
- **2026-10-14: No step requires Scott to read, label, review or write notes.** Fixtures, intents and judge validation are generated. `--note` and the popup note stay optional input. Scott's direction: "I dont want to manually review notes or create notes I want a skill/automated/framework/whatever".
- **2026-10-14: Archive outside staging, not an in-place sweep exemption.** An exemption would make tag proposals' window unbounded, which reopens 09-21 `:414`.
- **2026-10-14: This doc supersedes 07-07's post-60-day custody decision.** Grounds: Scott requested it, 5 of about 12 note-reading sessions fell back to the source, and retention is now 14 days. Panel round 1 caught that the first draft reopened it silently.
- **2026-10-14: The archive write fails the trace through a typed error, before the vault write, independent of `staging.enabled`.** Staged writes keep their WARN behavior. Panel round 1 found the first draft's "same as receipts" analogy wrong: terminal receipt writes are best-effort (`borg/src/pipeline.rs:395-398`).
- **2026-10-14: The VTT dedupe fixes new ingests only.** The backfill copies existing text as staged. The first draft's "dedupe before backfill" ordering bought nothing (panel round 1).
- **2026-10-14: The capture-note filter ships before the note prompts.** 49 of 58 existing notes are share boilerplate, and single-call paths already feed the note to the distiller.
- **2026-10-14: The preserve key is borg-owned, and both the reader and the writer change.** Adding `capture-note` to `CORTEX_PRESERVE_KEYS` would claim cortex owns a borg field. The writer at `atomic.rs:359-366` drops non-cortex keys (panel round 1).
- **2026-10-14: Durability is reported by the daemon's `/trace`, not by oracle's local trace block.** Oracle runs off-host (lappy) and its trace block is filesystem-free by contract (`oracle/src/server.rs:267-269`) (panel round 1).
- **2026-10-14: Links dedup happens in `render` for every kind.** `merge_description_links` only gates description URLs on videos (panel round 1).
- **2026-10-14: The channel search network call lives in borg, behind a daemon route, and oracle calls that route.** This follows the `ingest_queue` -> `GET /queue` precedent (`oracle/src/queue.rs:1,50-56`). The architect seat called the precedent invented; the staff seat and the code refute that.
- **2026-10-14: Persist `channel_handle`, not only `channel_id`.** The `/channel/UC.../search` form returned 0 entries in a test on ripr, while `/@handle/search` works.
- **2026-10-14: The popup uses a textarea plus a Capture button, and an empty note is a bare capture.** The popup needs a submit step once it takes input.
- **2026-10-14: `Tool` drops the `kind` field.** No consumer reads it (panel round 1 defer item; dropping it is cheaper than deferring).
- **2026-10-14: The eval harness (Phase 9) lands before the prompt phases (10, 11)**, so the prompts are measured from their first commit (panel round 1).

## Alternatives Considered

### Exempt `distilled.yml` from the sweep in place
- **Description:** `sweep` skips `distilled.yml` and deletes the rest.
- **Pros:** No new path, no reader changes.
- **Cons:** Tag proposals walk every staging dir, so their window grows without bound. That reopens 09-21 `:414`.
- **Why not chosen:** It breaks a settled decision.

### Restore the 60-day retention
- **Description:** Revert 1cf6a4d's defaults.
- **Pros:** One-line change.
- **Cons:** It only moves the cliff. d60baa92's lookup came after 60+ days.
- **Why not chosen:** It does not make the source durable.

### Store the source in the vault
- **Description:** A hidden folder or sidecar per note.
- **Cons:** It reverses the move that took sidecars out of the vault for bloat and git noise, and Syncthing carries it to every host.
- **Why not chosen:** Settled precedent.

### Free-text "Actionable" section instead of a typed `tools` field
- **Description:** The prompt writes a prose section.
- **Cons:** No validation bound, no grounding metric, no Links dedup.
- **Why not chosen:** A typed field matches the `enumeration` precedent and is measurable.

### Oracle calls yt-dlp directly
- **Description:** The `channel_search` tool spawns yt-dlp in oracle.
- **Cons:** It gives oracle its first direct network call, it fails on hosts without yt-dlp, and it duplicates borg's yt-dlp wrapper.
- **Why not chosen:** The `/queue` precedent already gives oracle a daemon-backed tool.

### Adopt an eval framework (DeepEval, Ragas, promptfoo, Inspect)
- **Description:** Measure Parts 2-3 with an off-the-shelf summarization metric.
- **Pros:** DeepEval `SummarizationMetric` and Ragas `SummaryScore` score coverage with generated yes/no questions (deepeval.com/docs/metrics-summarization, docs.ragas.io summarization_score).
- **Cons:**
  - DeepEval, Ragas and Inspect are Python-only.
  - promptfoo is YAML-native but has no claim decomposition.
  - `sb borg eval` already does claim verdicts, quote grounding and calibration in Rust.
- **Why not chosen:** It would add a second eval stack next to the one that already covers the need. Past sessions show no prior adoption either.

## Technical Considerations

### Dependencies
- yt-dlp on the daemon host (ripr: 2026.08.19). It is already required for YouTube ingest.
- Firefox extension re-sign for Phase 12 (AMO). It is an operator step.
- No cross-repo changes. `tatari-tv` and other `scottidler` repos are untouched.

### Performance
- Archive: `distilled.yml` alone is 24.5 MB today and grows about 390 files a month. Repo READMEs and thread transcripts add about 6.5 MB on top (`fetched.html` 5.0 MB plus `transcript.md` 1.5 MB, measured 2026-10-14).
- Channel backfill: about 1.6 s per `yt-dlp --dump-json`, over about 1,400 notes. It is a one-time operator run.
- Channel search: 0.82 s per query measured, bounded at 8 s.

### Security
- `--note` and the popup note are operator text. The patterns already treat instructions in the input as content, and the note block repeats that rule.
- `GET /channels/search` is bearer-gated like `/trace` and `/queue`.
- The handle and query reach yt-dlp as a single URL argument, never through a shell. The handle must match `^@[A-Za-z0-9._-]+$`, and the query is percent-encoded into the URL.

### Testing Strategy
- Unit tests named in each phase's criteria.
- `sb borg eval --staged --no-judge` before and after Phases 1, 7 and 8. Quote grounding (98.0% on 2026-10-14) must not drop.
- `sb borg eval --regenerate` on the capture-note and tools fixtures for Phases 10 and 11.

### Rollout Plan
- One commit per phase, `otto ci` green each time. `bump release`, then `otto deploy`, per part.
- Operator steps, in order:
  1. After Phase 3 ships: `sb borg retention archive --apply` on ripr.
  2. After Phase 12 ships: `sb borg extension sign`, then `otto deploy`.
  3. After Phase 15 ships: `sb borg channels --backfill --apply` on ripr.
- `sb borg retention sweep` stays manual. Nothing here schedules it.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| The sweep runs before the backfill and deletes 969 sources | Low | High | The sweep is manual today, and rollout step 1 orders the backfill first |
| An archive disk error blocks ingest | Low | Med | Fail closed is intended; the receipt shows `publish-failed` and `sb borg replay` retries |
| VTT dedupe drops a legitimately repeated phrase | Med | Low | Only an exact suffix/prefix overlap between adjacent cues collapses; a fixture covers a deliberate repeat |
| A capture note biases claims away from the source | Med | Med | The pattern rule says never invent relevance; Phase 10 checks with-note claims are no less supported |
| The tools prompt invents install commands | Med | Med | `tools-grounding` requires an exact substring for commands |
| yt-dlp channel search breaks on a YouTube change | Med | Low | The route returns a 502 with stderr; yt-dlp updates ride the existing ingest dependency |
| Creator names collide in the channel backfill | Low | Low | The backfill keys on each note's own video URL, not the creator name |
| The popup loses one-click capture on open | High | Low | Enter submits with focus in the empty textarea, so a bare capture is one keypress |

## Open Questions

None.

## References

- Sweeps 2026-10-14: note template audit (2,849 notes) and usage history (sessions c889cd0c, 53127aa1, 1366e3fd, d60baa92, e533ae12, b785f1cf, e60ccbd8)
- Review panel round 1: `~/.cache/review-panel/runs/0806kuTj/synthesis.md`
- `docs/design/2026-07-05-distillation-knowledge-extraction.md` (capture note, deferred reduce seam `:311`)
- `docs/design/2026-07-07-distillation-output-restore.md` (no `## Transcript`; the custody decision superseded here)
- `docs/design/2026-09-21-staged-tag-proposals.md` (`:414`)
- `sb borg eval` v0.16.0 (commit e1db1ec): `--staged`, quote grounding, `judge-claims`
- Query-focused summarization supports conditioning on intent: https://arxiv.org/pdf/2411.06524
- Hamel Husain on judges (binary, per failure mode, TPR/TNR vs human labels): https://hamel.dev/blog/posts/llm-judge/

## Addendum: parked options

- **Anthropic Citations for quotes.**
  - Citations return `cited_text` with guaranteed source pointers.
  - They cannot be combined with structured outputs: `output_config.format` returns 400 (docs.claude.com/en/docs/build-with-claude/citations, /structured-outputs).
  - Borg's distill uses Fabric and YAML, so adopting Citations means a two-call pipeline. Quote grounding already measures 98.0%.
  - Revisit if grounding drops below 95%.
- **MiniCheck as a cheap per-claim grounding gate.** https://github.com/Liyan06/MiniCheck (Ollama `bespoke-minicheck`; the 7B is non-commercial without a license). Revisit if `judge-claims` cost becomes a problem.
- **Sweep automation.** Revisit after Phase 3 lands.
- **Panel round 1 defer bundle:**
  - International handles vs the ASCII regex. Revisit on the first rejected handle.
  - Stale handles after a channel rename. Revisit when a search returns 0 for a followed channel.
  - Per-trace archive integrity verification. Revisit if an archive read fails.
