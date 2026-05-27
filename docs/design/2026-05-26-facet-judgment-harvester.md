# Design Document: sb facet - judgment-moment harvester for Claude Code JSONL transcripts

**Author:** Scott Idler
**Date:** 2026-05-26
**Status:** Implemented
**Review Passes Completed:** 5/5 (draft, correctness, clarity, edge cases, excellence) + Architect round 1 absorbed

## Summary

Add a fourth sb subsystem - `sb facet` - alongside borg/cortex/oracle. It scans Claude Code's per-session JSONL transcripts under `~/.claude/projects/`, collates turns into stable cross-session work-items via LLM clustering, mines each work-item for moments of *senior judgment in operation* (framing, iterating, rejecting, pushing for, sequencing, naming-the-failure), and writes one evolving markdown note per work-item into the obsidian vault. facet owns its JSONL parser, scanner, and repo-slug resolver outright (the patterns are borrowed from `tatari-tv/claude-report` with source-comment attribution, but cr's public surface only carries token/metadata; facet needs full turn text and cannot consume cr as a library). Vault output is private-first; Scott is the publish gate.

## TL;DR

- **Closes the apprenticeship gap for one user.** The motivating insight, lifted from the Shopify CEO note already in the vault: AI knowledge gets trapped in private chats; making senior judgment visible turns individual practice into shared taste. `sb facet` does that for Scott's own JSONL corpus.
- **One vault note per work-item.** Work-items are problems-being-attacked, not sessions. A single session can spawn multiple work-items; multiple sessions across days can collapse into one. The LLM does the cross-session collation; a SQLite ledger keeps work-item identity stable across re-runs.
- **Open judgment vocabulary.** Scaffolded by frame / iterate / reject (Shopify's "shared taste development" triple) plus push-for / sequence / name-the-failure. The LLM mines moments in their full variety with quoted-exchange evidence; it is *not* picking from a closed enum.
- **Mirrors borg/cortex/oracle exactly.** `facet/` lib crate in the workspace, `sb facet <verb>` subcommand surface, daemon mode installable via `sb facet daemon --install`, config at `~/.config/sb/facet.yml`, state at `~/.local/share/sb/facet/state.db`, frontmatter prefix `facet-*`, vault output under `notes/facet/`.
- **facet owns the JSONL parser, scanner, and repo resolver.** Patterns are borrowed from `tatari-tv/claude-report` with attribution in source comments, but facet does not depend on cr as a library: cr's public types (`SessionSummary`, `AssistantEntry`, `TokenTotals`) carry only metadata for cost calculation and drop the actual turn text. Architect round 1 ruled the dep would have bought ~300 lines of cross-org friction in exchange for code that fundamentally cannot answer facet's needs.
- **Default scope excludes work repos.** `tatari-tv/*` is excluded by default; `scottidler/*` and other personal cwds are included. Configurable via include/exclude lists. Honours the home/work persona separation in `~/.claude/refs/personas.md`.
- **Three-tier LLM use, via fabric subprocess.** Haiku for cheap clustering and turn classification; Sonnet for judgment extraction; Opus for cross-work-item portrait rollups (run less frequently). LLM calls go through `fabric -p <pattern> -m <model>` matching the existing borg/distillers convention; pattern files live at `facet/patterns/` and sync to `~/.config/sb/patterns/` via `otto deploy`. Per-tick and per-day budget caps in config.
- **Per-session extract windows, never raw cross-session concatenation.** A work-item's "new turns" are extracted per source session, not by stitching multiple sessions into one prompt. Cross-session synthesis happens at portrait-rollup time over already-extracted moments, not raw transcripts. This bounds extract-stage token budgets to one session's new turns and eliminates the context-blowout failure mode.
- **Per-stage ledger cursors.** `sessions.last_cluster_offset` advances on cluster success; per-(session, workitem) `last_extract_turn_uuid` advances on extract success. Cluster outputs are persisted; extract retries pull from persisted cluster state, not from a fresh LLM cluster call. Closes the split-brain / cost-loop hole the Architect identified.
- **Fencepost-merged vault rendering, not overwrite.** Managed sections are wrapped in `<!-- facet:auto:begin {section-id} -->` / `<!-- facet:auto:end {section-id} -->` fenceposts. The renderer touches only content inside fenceposts; anything outside is operator-owned and preserved across re-renders. Manual notes are a first-class part of v1, not an Open Question.

## Problem Statement

### Background

Every Claude Code session writes a JSONL transcript to `~/.claude/projects/<encoded-cwd>/<session-uuid>.jsonl`. The corpus today is ~1,471 files across ~472 MB and grows daily. Each line is a typed event - user turn, assistant turn, tool call, tool result, subagent dispatch - with full text, timestamps, model, tokens, cache stats, sessionId, parentUuid, cwd, gitBranch, and agentId fields. Nothing about how Scott actually works with AI is missing from these files; it is all on disk, and almost none of it is read after the session ends.

The Shopify CEO note already in the vault (`notes/shopify-ceo-reveals-their-secret-ai-developer.md`) names this exactly:

> "Teams build collective judgment about AI quality by seeing the full context of how experienced users frame problems, iterate on outputs, and reject plausible-but-wrong answers."

Senior judgment in AI use is the highest-leverage thing to share, and the hardest to extract from private transcripts. The same note frames the failure mode: *"Everyone is alone with their model, which means everyone has to rediscover the same lessons from scratch."*

`tatari-tv/claude-report` (`cr`) already harvests the *quantitative* layer from this corpus: per-session token usage, model mix, repo detection, subagent rollup, LLM-generated session titles, monthly cost reports with LLM-rendered narrative tracks. It does not harvest the qualitative layer: what Scott actually said, what he pushed back on, how he reframed, what he overrode.

### Problem

Three concrete gaps make today's transcripts wasted institutional learning:

- **The judgment is invisible.** A reader scrolling a JSONL file sees raw turn content with no summary of the *moves* in the conversation. Even Scott himself cannot retrieve "what did I say no to last week" without grepping by hand.
- **The work-item is fragmented.** Sessions are sliced by Claude Code's own session lifecycle (start, /clear, /exit, fresh session), not by the problem being worked. A single architectural question can span 4 sessions across 3 days; a single session can cover 6 unrelated work-items. The natural unit for retrieval and sharing is the work-item, and nothing today produces that unit.
- **No cadence, no surface.** `cr` runs monthly and writes a one-off markdown report. There is no daily/hourly rhythm, no per-work-item permalink, no continuous surface that can be linked, quoted, or shared the day after the work happened.

### Goals

- **One evolving vault note per work-item.** Each work-item gets a stable slug, a stable note path (`notes/facet/work-items/<slug>.md`), and a body that grows as new sessions touch the same work-item. Re-running the harvester updates the note idempotently.
- **Mine moments of senior judgment with quoted-exchange evidence.** Every extracted moment carries: the AI move that triggered it, Scott's move, the judgment mode it expresses (open vocabulary), and the verbatim quote(s) from the transcript so a reader can see the move land.
- **Cross-session work-item identity.** A SQLite ledger maps sessions to work-items; the same work-item ID persists across daemon ticks. Re-runs do not invent new work-items for already-tracked work.
- **Daily cadence by default, hourly opt-in.** The daemon harvests on a configurable interval. `sb facet harvest` is the one-shot CLI equivalent. Both run the same code path.
- **Work-repo hygiene.** `tatari-tv/*` is excluded from the default scan to keep work transcripts out of the personal vault. The exclude is a config list, not a hard-coded check, so the user can adjust without recompile.
- **Mirror the existing subsystem shape.** Every operator surface that exists for borg/cortex/oracle exists for facet: `sb facet status`, `sb facet doctor` integration, `sb bootstrap` drops a starter config, `sb facet daemon --install` writes a systemd unit. Zero new operator concepts.
- **Borrow patterns, do not depend.** `claude-report` is treated as prior art, not as a library. facet owns its own `scan`, `repo`, and `jsonl` modules; the implementations carry source-comment attribution where they mirror cr's approach (parent/subagent grouping by stem; git-remote-URL slug parsing). cr's pricing-oriented types do not flow through facet at all.

### Non-Goals

- **No automated public redaction in v1.** Output lives in the private obsidian vault. Scott is the publish gate for any external share. A `sb facet publish --redact` verb is a v2 concern and is not specified here; mentioning it now would be the "two-stage capture-vs-publish" frame the brainstorm explicitly rejected.
- **Not a replacement for `cr`.** `claude-report` continues to own monthly cost/usage reports. facet's harvest output is qualitative and per-work-item; cr's report output is quantitative and per-month. The two share *patterns* (file enumeration shape, slug parsing) but no code.
- **No closed pattern enum.** Frame/iterate/reject/push-for/sequence/name-the-failure are *scaffolding* the extractor LLM is told to start from. The extractor is allowed to surface judgment modes outside that list when warranted. The frontmatter `facet-modes` field is a list of strings, not an enum.
- **No retry semantics on LLM failures.** Per-tick failures are terminal; the ledger records the failure with a stage and an error. `sb facet retry <trace>` is a manual verb. (Same posture as the receipts-log design's "no retry semantics" rule.)
- **No vault-resident SQLite.** The ledger stays under `~/.local/share/sb/facet/`. The vault holds only markdown and attachments. Borg's receipts-log boundary is preserved: each subsystem owns its own SQLite file; nothing in facet opens borg's or oracle's databases.
- **No new ingest sources in v1.** facet reads `~/.claude/projects/` only. Codex transcripts, gemini-cli transcripts, IDE plugin transcripts are out of scope. The architecture allows them later as additional `scan::Source` implementations.

## Proposed Solution

### Overview

Three layers of state, scope-split:

```
Layer 1 (durable, daemon-internal):
  ~/.local/share/sb/facet/state.db        -- SQLite, ledger of sessions seen, work-items, judgment moments
  ~/.config/sb/facet.yml                  -- runtime config

Layer 2 (vault, user-facing, idempotently re-rendered):
  <vault>/notes/facet/work-items/<slug>.md -- one note per work-item
  <vault>/notes/facet/portraits/<mode>.md  -- optional cross-work-item rollups, lower cadence

Layer 3 (source of truth, never written by facet):
  ~/.claude/projects/<encoded-cwd>/<sid>.jsonl  -- Claude Code's own transcript files
```

End-to-end on each daemon tick:

1. **scan** - enumerate JSONL files; for each session, parse new turns from `sessions.last_cluster_offset` via `facet::jsonl::parse_session_file`; filter by include/exclude repo lists; produce a list of FacetSession with the new-turn slice.
2. **cluster** - for each new turn-range, ask Haiku "does this belong to an existing work-item the ledger knows about, or is it a new one?" The work-item is identified by its slug + title; the LLM is given the existing work-items' slugs and one-line summaries for the affected repos.
3. **extract** - for each touched work-item, ask Sonnet to mine the new turns for judgment moments. The prompt scaffolds frame/iterate/reject/push-for/sequence/name-the-failure but allows free-form modes. Output is a list of `JudgmentMoment` records with mode, quote, ai_move, scott_move, and why-it-matters.
4. **render** - for each touched work-item, materialize the vault note from the ledger state. Idempotent: same inputs produce same bytes. Frontmatter holds the metadata; body is organized by judgment-mode sections with quoted-exchange evidence.
5. **ledger update** - write the new offsets, new work-items, new judgment moments. Mark work-items dormant if their last contribution is older than the dormancy threshold (default 14 days).
6. **(less frequently) portrait rollup** - once per portrait-cadence (default weekly), run Opus over the recent judgment moments grouped by mode to produce a cross-work-item portrait note per mode.

### Architecture

#### Subsystem placement

Mirrors borg/cortex/oracle:

```
second-brain/
  vault/        -- shared library (existing)
  distillers/   -- per-kind L2 distillers (existing)
  borg/         -- ingestion library (existing)
  cortex/       -- governance library (existing)
  oracle/       -- knowledge retrieval library (existing)
  facet/        -- judgment-moment harvester library (NEW)
  sb/           -- unified CLI binary (gains sb facet subcommand)
  config/       -- shared config (gains facet.yml template + facet-tags.yml if needed)
```

`facet/` is a lib crate (edition 2024). `sb/Cargo.toml` adds `facet = { path = "../facet" }`. The crate exposes a public API for the sb subcommand:

```rust
pub mod jsonl;    // typed Turn parser over ~/.claude/projects/<cwd>/<sid>.jsonl
pub mod scan;     // file enumeration + parent/subagent grouping; emits FacetSession
pub mod repo;     // cwd -> owner/repo slug via git-remote-URL parsing
pub mod fabric;   // FabricCaller wrapper + FakeFabric mock (mirrors distillers)
pub mod workitem; // cross-session clustering, slug derivation, ledger writes
pub mod extract;  // per-session LLM judgment-moment extraction
pub mod render;   // fencepost-merging vault note rendering
pub mod daemon;   // cadence loop, install_systemd_service
pub mod config;   // FacetConfig from ~/.config/sb/facet.yml
pub mod ledger;   // sqlx connection pool, query helpers
pub mod notify;   // optional notification sinks (reuses borg::notify pattern)
```

#### Modules facet owns (cr is prior art, not a dep)

The Architect's round-1 verification showed that `claude-report`'s `pub` surface exposes only the metadata path (`SessionSummary`, `AssistantEntry`, `TokenTotals`) that `claude_pricing::parse_jsonl_file` produces; the actual `message.content` text is dropped during pricing extraction. facet needs full turn text, so cr's `Session*` types cannot be consumed. Adding cr as a git dep would buy facet roughly two modules' worth of small code (file enumeration in `scan.rs`, slug parsing in `repo.rs`) while costing cross-org version coupling.

Resolution: facet owns the parser end-to-end. cr is referenced in source comments as prior art / inspiration where the implementation parallels cr (parent+subagent grouping by stem; git-remote URL parsing).

- `facet::jsonl` - typed line parser over JSONL. Exposes `Turn { uuid, parent_uuid, timestamp, role: Role::User | Role::Assistant, content: Vec<ContentBlock> }` where `ContentBlock` is `Text { text }` | `ToolUse { name, input }` | `ToolResult { tool_use_id, content }`. Errors on schema drift are recorded as `JsonlError::UnknownLineShape` and the line is skipped with a logged warning (forward-compatible; new Claude Code versions adding fields do not panic).
- `facet::scan` - enumerates JSONL files under the configured projects root, groups parent + subagent files by stem (mirrors cr's `SessionFile { group_id, kind: Parent | Subagent }` shape), filters by include/exclude cwd lists.
- `facet::repo` - resolves a cwd to an `owner/repo` slug via `git -C <cwd> remote get-url origin` + URL parsing. Patterns ssh, https, git, ssh:// URLs (parallel to cr's `repo::parse_slug`).
- `facet::fabric` - wraps `distillers::FabricCaller` (the real trait name; the doc previously said `Fabric`). `FakeFabric` for tests is the same shape as distillers' mock.

No cr dependency in `Cargo.toml`. No upstream PR.

#### sb subcommand surface

```
sb facet harvest [--since <when>] [--repo <pattern>] [--dry-run]
sb facet daemon  [--install | --uninstall | --status]
sb facet list    [--repo <slug>] [--mode <mode>] [--status active|dormant|archived]
sb facet show    <work-item-slug>
sb facet render  <work-item-slug>   # force re-render of one work-item
sb facet retry   <session-uuid|workitem-slug>
sb facet archive <work-item-slug>   # marks archived in ledger; moves note via rkvr rmrf semantics
sb facet status                     # last harvest, counts, budget consumed
sb facet doctor                     # config sanity, LLM key, ledger reachable, vault writable
```

`sb status` and `sb doctor` (the top-level commands) gain a `facet` section the same way they already have `borg`/`cortex`/`oracle` sections.

#### Daemon mode and systemd install

Same pattern as borg/cortex. `facet::install_systemd_service` (in `facet/src/daemon.rs`) writes `~/.config/systemd/user/sb-facet.service` with the `ExecStart=` path resolved at install time from the current `sb` binary's `std::env::current_exe()` (mirrors `borg::install_systemd` and `cortex::install_systemd_service`):

```
[Unit]
Description=sb facet harvest daemon
After=network-online.target

[Service]
Type=simple
ExecStart=<resolved sb binary path> facet daemon
Restart=on-failure
RestartSec=30s

[Install]
WantedBy=default.target
```

Unit content lives in Rust (per `borg::install_systemd` / `cortex::install_systemd_service`), not in the repo as a static file. `sb bootstrap` does NOT install the unit; the user runs `sb facet daemon --install` once per machine.

#### Config file layout

`~/.config/sb/facet.yml`:

```yaml
# Source of JSONL transcripts.
claude-projects-root: ~/.claude/projects

# Cadence.
harvest-interval-secs: 86400          # daily; set to 3600 for hourly
portrait-interval-secs: 604800        # weekly; set to 0 to disable

# Scope.
include-cwds:
  - ~/repos/scottidler        # all personal repos
exclude-cwds:
  - ~/repos/tatari-tv         # work-identity hygiene; never harvested
  - ~/repos/scottidler/obsidian  # vault itself; not productive to mine
# An entry matches if cwd starts-with the expanded path.
# Precedence: exclude wins on overlap. A cwd under an excluded prefix is skipped
# even if it also sits under an included prefix.

# LLM tiering.
llm:
  cluster-model: claude-haiku-4-5
  extract-model: claude-sonnet-4-6
  portrait-model: claude-opus-4-7
  per-tick-budget-usd: 5.00
  per-day-budget-usd: 20.00

# Concurrency caps (per 2026-05-12 borg pipeline concurrency-cap incident).
concurrency:
  max-sessions-per-tick: 16
  max-llm-inflight: 4
  parse-rayon-threads: 4

# Work-item lifecycle.
dormancy:
  inactive-days: 14    # mark dormant after this many days with no new contribution

# Output.
vault:
  workitems-dir: notes/facet/work-items
  portraits-dir: notes/facet/portraits

# Notifications (optional; defaults off).
notify:
  on-new-workitem: false
  on-budget-exhausted: true
```

All path fields pass through `vault::paths::deserialize_tilde_pathbuf` (or `expand_tilde` for `String`) at config-load time. No fabricated `~/...` fallbacks in code.

#### State directory

```
~/.local/share/sb/facet/
  state.db         -- the ledger (see Data Model)
  fixtures/        -- optional: cached LLM responses keyed by content hash, for test reproducibility
```

The state directory is created by `sb bootstrap` if missing. `dirs::data_local_dir().expect("dirs::data_local_dir() returned None ...")` per the CLAUDE.md guidance; no fabricated path fallback.

### Data Model

#### JSONL ingestion shape

facet owns the parser. `facet::jsonl::parse_session_file(path: &Path, start_byte_offset: u64) -> Result<ParsedSlice>` returns the new turns since the byte offset plus the new end-of-file offset. `ParsedSlice` carries:

```rust
pub struct ParsedSlice {
    pub session_uuid: String,
    pub turns: Vec<Turn>,                 // only NEW turns since start_byte_offset
    pub end_byte_offset: u64,             // after the last fully-parsed line
    pub schema_drift_lines: u32,          // lines skipped due to unknown shape; logged
}

pub struct Turn {
    pub uuid: String,                     // for ledger keying
    pub parent_uuid: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub role: Role,                       // User | Assistant
    pub content: Vec<ContentBlock>,
    pub model: Option<String>,            // for assistant turns
}

pub enum ContentBlock {
    Text { text: String },
    ToolUse { id: String, name: String, input: serde_json::Value },
    ToolResult { tool_use_id: String, content: String, is_error: bool },
}
```

The byte-offset cursor is exact: parsing resumes after the last newline-terminated line of the previous tick. A partial trailing line (file mid-write by Claude Code) is left untouched and re-read on the next tick.

`facet::scan::enumerate(config: &FacetConfig, ledger: &Ledger) -> Vec<FacetSession>` produces:

```rust
pub struct FacetSession {
    pub session_uuid: String,
    pub cwd: PathBuf,
    pub repo_slug: Option<String>,        // resolved via facet::repo
    pub parsed: ParsedSlice,              // only the new turns this tick
    pub subagent_session_uuids: Vec<String>, // sibling subagent JSONL files grouped by stem
}
```

#### Work-item schema

```rust
pub struct WorkItem {
    pub id: i64,                      // SQLite rowid
    pub slug: String,                 // kebab-case, stable, used in vault path
    pub title: String,                // human-readable, LLM-generated, can be updated
    pub repos: Vec<String>,           // every repo_slug a contributing session touched
    pub status: WorkItemStatus,       // Active | Dormant | Archived
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub dormant_since: Option<DateTime<Utc>>,
    pub sessions_count: u32,
    pub modes_present: Vec<String>,   // open vocabulary; e.g. ["frame", "reject", "name-the-failure"]
}
```

Slug derivation: kebab-case of the LLM-generated title, deduplicated against the ledger. The slug is **frozen on work-item creation**; the title can update later (richer context produces better titles), but the slug, vault path, and ledger row identity never change. This guarantees that hand-edited Obsidian links to `[[work-items/<slug>]]` survive title regenerations.

#### Judgment-moment schema

```rust
pub struct JudgmentMoment {
    pub id: i64,
    pub workitem_id: i64,
    pub session_uuid: String,
    pub turn_uuid: String,            // points at a specific parentUuid in JSONL
    pub mode: String,                 // open vocabulary
    pub ai_move: String,              // short description of what the AI did that triggered this
    pub scott_move: String,           // short description of Scott's move
    pub quote_excerpt: String,        // verbatim quote, length-capped
    pub why_it_matters: String,       // LLM-generated one-line significance
    pub extracted_at: DateTime<Utc>,
    pub extractor_model: String,      // e.g. claude-sonnet-4-6
}
```

#### SQLite tables

```sql
CREATE TABLE sessions (
    session_uuid TEXT PRIMARY KEY,
    cwd TEXT NOT NULL,
    repo_slug TEXT,
    first_seen_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    last_cluster_offset INTEGER NOT NULL DEFAULT 0,  -- byte offset; advances on cluster success
    last_cluster_turn_uuid TEXT,                     -- last clustered turn; for diagnostics
    failure_count INTEGER NOT NULL DEFAULT 0,
    last_failure_reason TEXT,
    last_failure_stage TEXT                          -- 'scan' | 'cluster' | 'extract' | 'render'
);

CREATE TABLE work_items (
    id INTEGER PRIMARY KEY,
    slug TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('active','dormant','archived')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    dormant_since TEXT
);

CREATE TABLE work_item_repos (
    workitem_id INTEGER NOT NULL REFERENCES work_items(id),
    repo_slug TEXT NOT NULL,
    PRIMARY KEY (workitem_id, repo_slug)
);

CREATE TABLE session_workitem (
    session_uuid TEXT NOT NULL REFERENCES sessions(session_uuid),
    workitem_id INTEGER NOT NULL REFERENCES work_items(id),
    first_contribution_at TEXT NOT NULL,
    last_contribution_at TEXT NOT NULL,
    last_extract_turn_uuid TEXT,         -- per (session, workitem) extract cursor; advances on extract success
    PRIMARY KEY (session_uuid, workitem_id)
);

-- Persisted cluster output so extract retries do not re-call the cluster LLM.
-- A row exists for each (session, turn range) that the cluster stage has resolved.
-- The extract stage consumes these rows; on transient extract failure the rows
-- remain valid and the next tick resumes extract without re-clustering.
CREATE TABLE cluster_assignments (
    id INTEGER PRIMARY KEY,
    session_uuid TEXT NOT NULL REFERENCES sessions(session_uuid),
    workitem_id INTEGER NOT NULL REFERENCES work_items(id),
    first_turn_uuid TEXT NOT NULL,       -- inclusive
    last_turn_uuid TEXT NOT NULL,        -- inclusive
    clustered_at TEXT NOT NULL,
    cluster_model TEXT NOT NULL,
    extracted INTEGER NOT NULL DEFAULT 0 CHECK (extracted IN (0, 1)),
    UNIQUE (session_uuid, first_turn_uuid, last_turn_uuid)
);

CREATE INDEX idx_cluster_pending ON cluster_assignments(extracted) WHERE extracted = 0;

CREATE TABLE judgment_moments (
    id INTEGER PRIMARY KEY,
    workitem_id INTEGER NOT NULL REFERENCES work_items(id),
    session_uuid TEXT NOT NULL,
    turn_uuid TEXT NOT NULL,
    mode TEXT NOT NULL,
    ai_move TEXT NOT NULL,
    scott_move TEXT NOT NULL,
    quote_excerpt TEXT NOT NULL,
    why_it_matters TEXT NOT NULL,
    extractor_model TEXT NOT NULL,
    extracted_at TEXT NOT NULL,
    UNIQUE (workitem_id, turn_uuid, mode)   -- idempotent re-extraction
);

CREATE INDEX idx_moments_mode ON judgment_moments(mode);
CREATE INDEX idx_moments_workitem ON judgment_moments(workitem_id);
CREATE INDEX idx_sessions_repo ON sessions(repo_slug);

CREATE TABLE ledger_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- e.g. last-harvest-tick, last-portrait-tick, schema-version, current-budget-tick-usd
```

#### Vault frontmatter (per work-item note)

```yaml
---
title: "Loopr v5 stage-eight wiring capstone"
date: 2026-05-26
type: facet-workitem
origin: assisted
method: facet
trace: ft-3a7c19
status: active
domain: ai
facet-workitem-id: 142
facet-slug: loopr-v5-stage-eight-wiring-capstone
facet-status: active
facet-sessions-count: 7
facet-repos:
  - scottidler/loopr
facet-modes:
  - frame
  - iterate
  - push-for
  - reject
  - name-the-failure
facet-first-seen: 2026-05-18
facet-last-seen: 2026-05-26
facet-extractor: facet-v1
tags:
  - facet
  - judgment
  - loopr
---
```

`type: facet-workitem` and `type: facet-portrait` are added to `vault::schema::NoteType` as part of Phase 1. `method: facet` is added to `vault::schema::Method`. Frontmatter pre-existing fields (`date`, `origin`, `trace`, `status`, `domain`, `tags`) reuse the standard vault schema; the new `facet-*` fields are facet-managed and not touched by other subsystems (matches the borg-* / cortex-* convention). The schema additions are exposed through `schemars::JsonSchema` derives so oracle's MCP tool schemas pick them up automatically.

`tags` are derived: always includes `facet` and `judgment`; adds one tag per repo_slug's terminal segment (e.g. `loopr`); is post-filtered through the canonical-tags vocabulary same as borg's output.

### Data Flow

#### Per-tick sequence

```
sb facet daemon (or one-shot sb facet harvest)
  |
  v
[scan]
  - enumerate JSONL files under claude-projects-root via facet::scan
  - filter by include-cwds / exclude-cwds (path prefix match on expanded paths; exclude wins)
  - for each session_uuid in JSONL:
      look up ledger.sessions
      parse new turns from byte offset = sessions.last_cluster_offset (or 0 for new sessions)
      via facet::jsonl::parse_session_file; trailing partial line is left for next tick
  - drop sessions with no new turns
  |
  v
[cluster]     [Haiku, batched per session, pattern: facet-cluster]
  - for each session-with-new-turns, invoke `fabric -p facet-cluster -m claude-haiku-4-5`
    with input: existing active work-items for affected repos (slug -> one-line title) + compact
    digest of the new turns (role + first 200 chars per turn; tool_use/tool_result names only)
  - pattern body lives in facet/patterns/facet-cluster.md (synced to
    ~/.config/sb/patterns/facet-cluster.md by otto deploy)
  - output: JSON list of (first_turn_uuid, last_turn_uuid, assignment) where assignment is
    either `{ kind: "existing", slug }` or `{ kind: "new", title }`
  - in one transaction per session:
      insert any new work_items + session_workitem rows
      insert one cluster_assignments row per assignment (extracted=0)
      sessions.last_cluster_offset := parsed.end_byte_offset
      sessions.last_cluster_turn_uuid := last turn uuid in the slice
  - on cluster LLM failure: NO writes; sessions.last_cluster_offset unchanged; retry next tick
  |
  v
[extract]     [Sonnet, per (session, cluster_assignment), pattern: facet-extract]
  - select cluster_assignments WHERE extracted = 0, batched by session
  - for each row: extract the turn range [first_turn_uuid, last_turn_uuid] from JSONL
    (re-parse the bounded slice; bounded by uuid not byte-offset, so safe across re-renders)
  - invoke `fabric -p facet-extract -m claude-sonnet-4-6` per cluster_assignments row
    (NOT per work-item; the extract window is one session's contiguous slice, never a
    cross-session stitch)
  - pattern body at facet/patterns/facet-extract.md, scaffolds frame/iterate/reject/push-for/
    sequence/name-the-failure with open vocabulary explicitly allowed; output schema
    (mode, ai_move, scott_move, quote_excerpt verbatim <= extract.quote-max-chars,
    why_it_matters one sentence) as a JSON list
  - in one transaction per row:
      insert judgment_moments (UNIQUE on (workitem_id, turn_uuid, mode) makes it idempotent)
      cluster_assignments.extracted := 1
      session_workitem.last_extract_turn_uuid := last_turn_uuid
  - on extract LLM failure: that row stays extracted=0; retry next tick; OTHER work-items'
    extracts proceed unaffected
  |
  v
[render]      [pure, deterministic, no LLM, fencepost-merging]
  - for each work-item touched in this tick (by cluster or by extract):
      load all judgment_moments for that work-item
      group by mode
      if notes/facet/work-items/<slug>.md exists:
          read existing file; identify operator-owned content outside facet:auto fenceposts
          regenerate auto sections in place; preserve operator content verbatim
      else:
          render fresh from template (auto sections only; no operator content yet)
      write via tempfile + atomic rename
  |
  v
[ledger update]
  - work_items.updated_at = now
  - work_items where now - last_contribution > inactive-days  ->  status = 'dormant'
  - ledger_meta last-harvest-tick = now
  - ledger_meta current-budget-tick-usd reset / updated
  |
  v
(if portrait-interval elapsed)
  v
[portrait rollup]   [Opus, per mode, less frequent]
  - input: already-extracted judgment_moments (NOT raw transcripts) for the mode within the
    portrait window; up to portrait.max-moments-per-mode (default 80)
  - invoke `fabric -p facet-portrait -m claude-opus-4-7` once per mode
  - render notes/facet/portraits/<mode>.md with fencepost-merging too
```

#### Failure handling per stage

- **scan failure** (cannot read JSONL, malformed line): record `sessions.failure_count += 1`, `last_failure_reason = "..."`, `last_failure_stage = 'scan'`; skip the session for this tick; surface in `sb facet doctor`. Per-line schema-drift errors do not fail the session; the line is skipped and `parsed.schema_drift_lines` is logged.
- **cluster failure** (LLM API error): Failure is terminal for this tick. Set `last_failure_stage = 'cluster'` and skip this session. `sessions.last_cluster_offset` is NOT advanced. The same turns are re-clustered next tick. Because cluster output for already-clustered ranges is persisted in `cluster_assignments`, a failure here does not cause re-clustering of *previously* clustered ranges. This matches the Non-Goals rule ("no retry semantics on LLM failures") and the receipts-log discipline; the prior version of this paragraph proposed in-tick backoff and was a stale draft artifact (Architect round-2 consensus).
- **extract failure**: per (session, cluster_assignment) granularity. The failing row stays `extracted=0`; the next tick's extract stage picks it up. Other rows for other work-items in the same session, and other sessions, proceed unaffected. `last_failure_stage = 'extract'` on the session row.
- **render failure** (vault write): logged as error, retry next tick. Render is fully deterministic from the ledger, so a failed tick can re-render on the next tick with no LLM cost. Vault writes are tempfile + atomic rename.
- **budget exhaustion mid-tick**: the tick stops cleanly at the next *stage* boundary (between cluster rows or between extract rows). Ledger is consistent because every row write is its own transaction. A `budget-exhausted` notification fires if enabled. Already-clustered-but-not-yet-extracted rows survive in `cluster_assignments` and are picked up on the next tick without re-spending the cluster cost.

### Vault Output

#### Per-work-item note structure

```
---
{frontmatter as specified above}
---

<!-- facet:auto:begin header -->
# {title}

> [!tldr]
> {LLM-generated one-paragraph summary of what the work-item is about,
>  regenerated on each render from the current judgment moments and titles}

## Context

- **Repos:** {repo_slug list}
- **Sessions:** {N sessions}, first {date}, last {date}
- **Status:** active | dormant | archived
- **Judgment modes present:** {comma-separated}
<!-- facet:auto:end header -->

<!-- facet:auto:begin section:frame -->
## Frame

> {ai_move: "what the AI did or proposed"}
>
> ```text
> {Scott's response, verbatim, length-capped, wikilinks neutered by code fence}
> ```
>
> *Why it matters: {why_it_matters}*
>
> - `{session_uuid_short}` at `{timestamp}`

{ ... repeated for each moment in this mode ... }
<!-- facet:auto:end section:frame -->

<!-- facet:auto:begin section:iterate -->
## Iterate
{ ... }
<!-- facet:auto:end section:iterate -->

<!-- facet:auto:begin section:reject -->
## Reject
{ ... }
<!-- facet:auto:end section:reject -->

<!-- facet:auto:begin section:push-for -->
## Push for
{ ... }
<!-- facet:auto:end section:push-for -->

<!-- facet:auto:begin section:sequence -->
## Sequence
{ ... }
<!-- facet:auto:end section:sequence -->

<!-- facet:auto:begin section:name-the-failure -->
## Name the failure
{ ... }
<!-- facet:auto:end section:name-the-failure -->

<!-- facet:auto:begin section:other -->
{ ... open-vocabulary modes appear here, each in its own ## subsection,
      alphabetical by mode name; this entire block is one fencepost ... }
<!-- facet:auto:end section:other -->

<!-- Operator notes below this line are preserved across re-renders.
     Anything you write outside facet:auto fenceposts (including in entirely new sections,
     callouts, or appended paragraphs) survives untouched. -->

<!-- facet:auto:begin footer -->
---

*This note was synthesized by `sb facet`. Source transcripts: {N JSONL files under ~/.claude/projects/}. To re-render: `sb facet render {slug}`.*
<!-- facet:auto:end footer -->
```

The H2 sections appear only if the work-item has moments in that mode (the fencepost block exists but is empty when there are no moments). The order of standard sections is fixed (frame, iterate, reject, push-for, sequence, name-the-failure); open-vocabulary modes appear inside the `section:other` fencepost in alphabetical order.

#### Fencepost-merge rendering contract

The renderer is a structured merge, not a replace. Algorithm:

1. If the target file does not exist: render from template with all fenceposts populated; write atomically. Done.
2. If the target file exists: parse it into a sequence of `Block { kind: Auto { id } | Operator, body: String }` segments by walking `<!-- facet:auto:begin {id} -->` ... `<!-- facet:auto:end {id} -->` pairs.
3. For each existing `Auto { id }` block: replace its body with the freshly-rendered body for that section id.
4. For each `Operator` block: preserve byte-for-byte.
5. If the rendered template defines a new section id not present in the existing file (e.g. a new mode emerged): insert the new auto block at its template position; surrounding operator blocks are not perturbed.
6. If an existing auto block's section id is no longer present in the template (e.g. all moments of a mode were archived): the auto block is rewritten as empty (`<!-- facet:auto:begin section:X --><!-- facet:auto:end section:X -->`) but not removed, so operator-added content nearby is not relocated.
7. Frontmatter is treated as a single `Auto { id: "frontmatter" }` block; operator-added frontmatter keys (not in the facet-managed set) are preserved (see "Frontmatter merge" below).

Fencepost markers are HTML comments, invisible to Obsidian's preview pane. An operator with markdown source open sees them and knows where the autopopulated regions are. Mistakenly deleted markers are detected on the next render: if a section's begin/end pair cannot be located, the renderer falls back to "append the section anew at the end of the file" rather than overwriting, and a warning is logged so the operator can fix the markers manually.

#### Frontmatter merge

`facet-*` keys are facet-owned; the renderer overwrites them. All other frontmatter keys (including `tags`) are operator-extensible: the renderer reads the existing file's frontmatter, takes the union of operator-set keys with facet-managed keys, lets facet-managed keys win on the keys it owns, and writes the result. Operator-added tags survive; facet-managed tags (`facet`, `judgment`, per-repo terminal segment) are always present.

#### Portrait rollup note structure (one per mode, weekly cadence)

```
---
title: "How Scott rejects plausible-but-wrong answers"
date: 2026-05-26
type: facet-portrait
origin: assisted
method: facet
facet-mode: reject
facet-moments-included: 47
facet-workitems-spanned: 12
facet-window-days: 30
facet-extractor: facet-v1
tags:
  - facet
  - portrait
  - reject
---

# How Scott rejects plausible-but-wrong answers

{Opus-synthesized narrative: 3-5 short paragraphs naming the recurring shapes
 of Scott's rejection moves, with linked quoted examples drawn from across
 the recent work-items. Each example links back to [[work-item-slug]].}

## Representative moments

- [[loopr-v5-stage-eight-wiring-capstone]] - rejected a premature abstraction
- [[receipts-log-migration]] - rejected a Rust-side migration in favor of bash
- ...
```

#### Linking discipline

- Work-item notes link to portrait notes via `[[portraits/reject]]` when a mode section is present.
- Portrait notes link to contributing work-item notes via `[[work-items/<slug>]]`.
- Neither links to the underlying JSONL paths (those are operator-surface only, via `sb facet show`).

### Operator Surface

#### sb facet verbs

| Verb | Behavior |
|---|---|
| `sb facet harvest` | One-shot. Runs the full per-tick sequence. Honors `--since`, `--repo`, `--dry-run`. |
| `sb facet daemon` | Long-running. Re-runs harvest every `harvest-interval-secs`. Re-runs portrait rollup every `portrait-interval-secs`. |
| `sb facet daemon --install` | Writes the systemd unit and enables it. Idempotent. |
| `sb facet daemon --uninstall` | Disables and removes the unit. |
| `sb facet list` | Lists work-items from the ledger. `--repo`, `--mode`, `--status` filters. Default: active only. |
| `sb facet show <slug>` | Prints the work-item note path and key ledger facts (session count, modes, last update). |
| `sb facet render <slug>` | Forces re-render of one work-item from the current ledger state (no LLM call). |
| `sb facet retry <session-uuid \| slug>` | For a session: rewinds `sessions.last_cluster_offset` to a point before the failure so the next tick re-clusters. For a work-item slug: marks any `cluster_assignments` rows for that slug as `extracted=0` so the next tick re-extracts. Useful after fixing a transient LLM error or after rolling a pattern file. |
| `sb facet archive <slug>` | Marks `status='archived'`. Moves the note to `notes/facet/archive/<slug>.md` via `rkvr rmrf` semantics (so it stays recoverable). |
| `sb facet status` | Last harvest tick, last portrait tick, work-item counts by status, budget consumed this tick / today. |
| `sb facet doctor` | Config validity, LLM key reachability, vault writable, ledger reachable, claude-projects-root present, persona excludes applied. |

#### Integration with top-level verbs

- `sb status` gains a `facet:` section showing last harvest, active/dormant counts, budget.
- `sb doctor` calls `facet::doctor()` alongside borg/cortex/oracle doctors.
- `sb bootstrap` drops `config/templates/facet.yml` into `~/.config/sb/facet.yml` if not present; creates `~/.local/share/sb/facet/` if not present.

#### Log / debug surface

- Crate uses `env_logger + log` (matches borg/cortex; oracle uses tracing for rmcp).
- Every non-trivial function emits a DEBUG entry log with named parameters, per `~/repos/.claude/rules/log.md`.
- Tight loops (per-turn iteration in extract) log at TRACE.
- Sensitive payloads (full LLM prompts, full transcript turns) log as length summaries + first-N-char previews, never inline.

#### Inspecting the ledger

`~/.local/share/sb/facet/state.db` is opened by sb facet only. Read-only inspection via `sqlite3` is supported (no exclusive lock during reads). `sb facet status --json` emits a machine-readable status dump for piping into `jq`.

### Error Handling and Concurrency

#### Concurrency caps

Per the 2026-05-12 borg pipeline concurrency-cap incident:

- Parse / scan: rayon `par_iter` bounded by `concurrency.parse-rayon-threads` (default 4). CPU-bound; no LLM calls.
- LLM dispatch: tokio `JoinSet` with a semaphore bounded by `concurrency.max-llm-inflight` (default 4). No unbounded fanout.
- Per-tick session cap: `concurrency.max-sessions-per-tick` (default 16). If more than 16 sessions have new turns in a single tick, the remainder is deferred to the next tick. Ledger remains consistent.

When the daemon's sync stages run alongside the async LLM stages, they wrap CPU-bound passes in `tokio::task::block_in_place` (same pattern as cortex's daemon).

#### Budget enforcement

`llm.per-tick-budget-usd` is checked before each LLM call:

```
estimated_call_cost_usd = estimate_from_model_and_token_count(...)
if budget_consumed_this_tick + estimated_call_cost_usd > per_tick_budget_usd:
    record reason in ledger; skip this call; do not advance offsets for affected work-items
```

`llm.per-day-budget-usd` is enforced at the ledger level: rolling 24-hour window summed across ticks. Daemon goes idle (logs "budget exhausted, sleeping until midnight") when exhausted.

#### Failure classification

The ledger's `sessions.last_failure_reason` is a free-form string. For machine triage, `judgment_moments` is the success surface and `sessions.failure_count > 0` is the failure surface. There is no DLQ table; failures are session-level (the smallest unit that can fail-and-retry cleanly).

#### Atomicity

- All cluster + extract writes within one work-item's per-tick processing happen in a single SQLite transaction. Either the work-item is fully clustered + extracted + ready-to-render, or nothing changes for it.
- Vault writes use tempfile + `fs::rename` (atomic on the same filesystem). No partial notes appear.
- The ledger is written *after* the vault note is written successfully. If the daemon dies between a successful vault write and the ledger update, the next tick rewrites the same note (idempotent) and then updates the ledger.

### Testing

#### Unit tests (no LLM, fast)

- `scan::filter_by_include_exclude` - fixture cwd lists, expanded paths, edge cases (~ in include but not exclude, etc.).
- `workitem::derive_slug` - title to slug; collision resolution; idempotence of repeated calls.
- `ledger::*` - schema migration, transaction rollback on error, idempotent inserts on `(workitem_id, turn_uuid, mode)`.
- `render::vault_note` - golden frontmatter and body for a hand-crafted set of judgment moments.

#### Integration tests (FakeFabric)

A `FakeFabric` implementation of the `FabricCaller` trait pins canned outputs by pattern name + input hash. Mirrors `distillers::FakeFabric` exactly. Fixtures live at `facet/tests/fixtures/`. An end-to-end test:

1. Lays down a synthetic `~/.claude/projects/` directory tree under `tempdir`.
2. Constructs a `FacetConfig` with vault root pointing to a second tempdir.
3. Runs `facet::daemon::harvest_once(&config, &FakeFabric::default())`.
4. Asserts the vault now contains the expected work-item notes with the expected frontmatter and section structure.
5. Re-runs harvest; asserts no-op (idempotent re-render, no new ledger rows, no fresh fabric calls).

#### Real-fabric smoke test

Gated behind `FACET_REAL_FABRIC=1` env var. Runs against a tiny synthetic transcript (a few dozen turns) with a known judgment moment. Asserts that the extractor returns at least one moment in a sensible mode. Not run in CI by default; the user opts in manually after verifying their fabric install can talk to the Anthropic API.

#### Borg/cortex test borrow-list

facet imports the existing `vault::test_support` helpers for vault tempdir setup and frontmatter parsing. No reinvention.

## Implementation Plan

The doc lists every piece that ships in v1; no soak/burn-in/phase-gating per `feedback-no-phase-gating`. Sequencing is for ordering, not for value-gating between phases.

### Phase 1: Crate scaffolding, ledger, vault schema extensions
**Model:** sonnet

- Add `facet/` crate to workspace `Cargo.toml`. Wire into `sb/Cargo.toml`.
- Stub modules: `scan`, `workitem`, `extract`, `render`, `daemon`, `config`, `ledger`, `notify`.
- Extend `vault::schema::NoteType` with `FacetWorkitem` and `FacetPortrait` variants (kebab-case serde renames).
- Extend `vault::schema::Method` with `Facet` variant.
- Implement `FacetConfig` deserialize from YAML with `vault::paths::deserialize_tilde_pathbuf` on every path field.
- Implement SQLite schema creation + migrations via sqlx (schema-version pinned in `ledger_meta`).
- Implement `ledger::Ledger` with insert/query helpers for sessions, work_items, judgment_moments.
- Drop config template at `config/templates/facet.yml`; wire `sb bootstrap` to install it.
- Unit tests on ledger, config, and schema round-trip.

### Phase 2: JSONL parser, scan, repo (facet owns these)
**Model:** sonnet

- Implement `facet::jsonl` typed parser for `~/.claude/projects/<encoded-cwd>/<sid>.jsonl`. `Turn`, `ContentBlock`, `Role`, `parse_session_file(path, start_byte_offset) -> ParsedSlice`. Newline-terminated parsing with partial-line tolerance.
- Implement schema-drift tolerance: unrecognized line shapes are logged at WARN and skipped; `parsed.schema_drift_lines` carries the count.
- Implement `facet::scan::enumerate(config: &FacetConfig, ledger: &Ledger) -> Vec<FacetSession>`. Parent/subagent grouping by stem. Include/exclude filtering with tilde-expanded path-prefix matching; exclude wins on overlap.
- Implement `facet::repo::resolve_slug(cwd: &Path) -> Option<String>` via `git -C <cwd> remote get-url origin` + URL parsing for ssh, https, git, ssh:// shapes (parallels `claude_report::repo::parse_slug` with source-comment attribution).
- Implement byte-offset cursor handling: `parse_session_file` resumes after the last newline-terminated line of the previous tick; partial trailing lines wait.
- Unit tests with synthetic JSONL fixtures covering: empty file, single-turn file, mid-stream parse, subagent grouping, schema-drift line in the middle, partial trailing line, malformed JSON line.

### Phase 3: Work-item clustering with persisted cluster state
**Model:** opus

- Author `facet/patterns/facet-cluster.md` (Fabric pattern). Iterate against real recent JSONL data manually until cluster output is sensible across the four golden scenarios below.
- Implement `facet::fabric` wrapping `distillers::FabricCaller` (the actual trait name); reuse `distillers::FakeFabric` shape for tests.
- Implement `workitem::cluster_new_turns(session: &FacetSession, ledger: &Ledger, fabric: &dyn FabricCaller) -> Vec<Assignment>`.
- Per-session transactional writes: new work_items + session_workitem rows + cluster_assignments rows + sessions.last_cluster_offset advance, all in one tx. On LLM failure: no writes, offset unchanged.
- Slug derivation (frozen-on-creation), collision handling.
- Mock-fabric unit tests with golden assignments for several scenarios:
  - Brand-new session, brand-new work-item.
  - Existing session, continuing work-item.
  - Same work-item, second session, different day.
  - Two work-items emerge from one session.
  - Cluster LLM transient failure: state unchanged, retry next tick succeeds, no duplicate work-items created.

### Phase 4: Judgment extraction (per cluster_assignment, never cross-session)
**Model:** opus

- Author `facet/patterns/facet-extract.md` (Fabric pattern). Iterate against real recent JSONL data manually. Validate that the LLM produces moments in the scaffolding modes (frame/iterate/reject/push-for/sequence/name-the-failure) and surfaces open-vocabulary modes when warranted.
- Implement `extract::mine_moments(assignment: &ClusterAssignment, turns: &[Turn], fabric: &dyn FabricCaller) -> Vec<JudgmentMoment>` where `turns` is the bounded slice from `[first_turn_uuid, last_turn_uuid]` for *one session*. Cross-session synthesis happens at portrait time, never here.
- Per-row transactional writes: insert judgment_moments (idempotent on `(workitem_id, turn_uuid, mode)`), flip `cluster_assignments.extracted` to 1, advance `session_workitem.last_extract_turn_uuid`, all in one tx.
- Quote-excerpt length cap (config: `extract.quote-max-chars`, default 800), ai_move/scott_move shape, why_it_matters one-liner.
- Mock-fabric unit tests covering: each scaffolding mode, at least one open-vocabulary mode, an extract failure that leaves cluster_assignments.extracted=0 with no judgment_moments rows.

### Phase 5: Fencepost-merging renderer
**Model:** sonnet

- Implement the `Block { Auto { id }, Operator }` parser that scans an existing file for `<!-- facet:auto:begin {id} -->` ... `<!-- facet:auto:end {id} -->` pairs. Preserve everything outside auto blocks.
- Implement `render::work_item_note(workitem: &WorkItem, moments: &[JudgmentMoment], existing: Option<&str>, ledger: &Ledger) -> String`. From-scratch render when `existing` is `None`; structured merge when `Some`.
- Frontmatter merge: facet-managed keys win on the keys they own; operator-added keys preserved. `tags` is union (facet-managed tags always present; operator-added tags survive).
- Atomic write via tempfile + rename.
- Golden tests covering:
  - Fresh render (no prior file).
  - Re-render with no operator content: byte-identical when ledger unchanged.
  - Re-render with operator content between auto blocks: operator content preserved verbatim.
  - Re-render adds a new mode section: existing operator content not relocated.
  - Re-render where a mode emptied out: auto block becomes empty but stays present.
  - Operator deleted a fencepost marker: detection + warning + append-anew fallback.
  - Operator added `tags: [my-tag]` and an operator-only frontmatter key: both survive.

### Phase 6: Daemon, sb subcommand, systemd install
**Model:** sonnet

- Implement `daemon::run_loop(config: FacetConfig) -> Result<()>`.
- Tokio main loop, concurrency caps, budget enforcement.
- `install_systemd_service` and `uninstall_systemd_service` matching borg's shape.
- Wire up all `sb facet *` verbs in `sb/src/cli/facet.rs` (matches the existing per-subsystem module pattern under `sb/src/cli/`).
- Integration with `sb status`, `sb doctor`, `sb bootstrap`.

### Phase 7: Portrait rollups
**Model:** opus

- Author `facet/patterns/facet-portrait.md` (Fabric pattern) for cross-work-item synthesis per mode.
- Implement `extract::portrait_for_mode(mode: &str, window_days: u32, ledger: &Ledger, fabric: &dyn FabricCaller) -> String`. Input is already-extracted `judgment_moments` rows for the mode, never raw transcripts.
- Render portrait notes with frontmatter as specified.
- Daemon dispatch on `portrait-interval-secs`.

### Phase 8: End-to-end tests and shakedown
**Model:** sonnet

- FakeFabric integration test driving harvest_once end-to-end over a synthetic JSONL tree.
- Real-fabric smoke test gated by `FACET_REAL_FABRIC=1`.
- `sb facet doctor` sanity over a fresh install.
- Update `CLAUDE.md` to describe the new subsystem (one paragraph + structural entry under "Architecture").
- Add `facet` to the build/install matrix; ensure `otto ci` covers the new crate.

## Alternatives Considered

### Alternative 1: Extend `cr` with vault-emitting subcommand
- **Description:** Add `cr emit --to-vault` and `cr daemon` to claude-report. No new sb subsystem.
- **Pros:** No new crate; reuses all of cr's existing infrastructure directly.
- **Cons:** cr lives in `tatari-tv/*`; harvest output is personal-vault material. Mixing personal vault paths and frontmatter conventions into a work-org repo couples them backwards. cr does not get `vault::*` integration for free without depending on second-brain. The judgment-harvester wants to live where `vault::*` lives.
- **Why not chosen:** Wrong direction of coupling. The new subsystem belongs in second-brain, not in claude-report.

### Alternative 2: Depend on `claude-report` as a library
- **Description:** Add `claude-report` as a git dependency (pinned to a commit). Import its `scan`, `session`, `title`, `repo` modules.
- **Pros:** Apparent code reuse; no parser to write in facet.
- **Cons:** Architect round 1 verified empirically that cr's `pub` surface exposes only metadata: `SessionSummary`, `TokenTotals`, `AssistantEntry`. `claude_pricing::parse_jsonl_file` drops `message.content` text during pricing extraction. cr's `title::extract_prefix` re-reads JSONL from disk to get the first user/assistant turns because the metadata path does not retain them. There is no `Session { turns, ... }` type for facet to consume. Adding the dep would require an upstream PR to expose new APIs cr does not currently have, in exchange for ~300 lines of code (scan.rs + repo.rs) that facet can implement directly.
- **Why not chosen:** The dep would not deliver what the original spec assumed. The cost (cross-org pin, upstream PR coordination, version drift) outweighs the value of two small modules. facet owns its parser, scanner, and repo resolver; cr is treated as prior art in source comments where the implementation parallels it.

### Alternative 3: Refactor `cr` into a shared lib + new subsystem
- **Description:** Extract a fresh JSONL-with-full-text parser into a shared crate (`claude-jsonl-core` or similar). Both `cr` and facet depend on it. cr would also need to be reworked to consume the new typed parser instead of its current pricing-only path.
- **Pros:** Cleanest decoupling. Future-proof if more consumers emerge.
- **Cons:** Premature today. cr's pricing-only metadata extraction is intentional (its monthly cost reports do not need turn text). Forcing cr to adopt a fatter parser is a cross-repo refactor for ~300 lines of saved code. There is exactly one new consumer.
- **Why not chosen:** YAGNI. facet owns its parser. If a third consumer ever needs the same shape, the extraction can happen then with two real consumers driving the API.

### Alternative 4: Per-cadence digest output instead of per-work-item notes
- **Description:** Each harvest tick produces one note (e.g. `notes/facet/digests/2026-05-26.md`) listing all work-items active that tick. Work-items are not first-class durable artifacts.
- **Pros:** Simpler ledger; no slug stability problem; no idempotent re-render.
- **Cons:** A single work-item's story is scattered across N daily notes. Cannot share "the loopr v5 story" as one document. Loses the cross-session collation value the user explicitly named as the point.
- **Why not chosen:** The brainstorm settled this: the durable unit is the work-item, not the cadence tick.

### Alternative 5: Closed pattern enum (fixed list of judgment modes)
- **Description:** Define `enum JudgmentMode { Frame, Iterate, Reject, PushFor, Sequence, NameTheFailure }` and constrain the extractor to pick from it.
- **Pros:** Tighter ledger schema; easier to query and aggregate.
- **Cons:** Misses real judgment shapes outside the enum. The Shopify framing is "shared taste development" - taste does not fit in six buckets. Brainstorm explicitly rejected this.
- **Why not chosen:** Open vocabulary is the user's stated requirement. `facet-modes` stays a list-of-strings.

## Technical Considerations

### Dependencies

**Internal (this workspace):**
- `vault` - paths, schema, note rendering helpers, config tilde-expansion.
- `sb` - the binary that exposes `sb facet`.

**External:**
- `sqlx` with sqlite feature (already used by oracle / borg).
- `tokio`, `rayon` (workspace standards).
- `env_logger + log` (workspace standard for non-oracle subsystems).
- `serde`, `serde_yaml`, `serde_json`.
- `chrono` for timestamps (workspace standard).
- No `claude-report` library dependency. See Alternative 2 in Alternatives Considered for the rationale; the parser lives in `facet::jsonl`.
- No direct LLM SDK dependency. LLM calls are subprocesses to the `fabric` CLI, matching the borg/distillers pattern. The `FabricCaller` trait in `distillers/src/fabric.rs` (with `FakeFabric` for tests) is the prior art facet adopts. The Anthropic API key is consumed by `fabric` itself, not by facet.

### Performance

- Per-tick scan touches all JSONL files but reads only the new bytes per session (offset-tracked). The full corpus is ~472 MB today; a daily tick reads only the day's growth in the common case.
- LLM call volume is bounded by `max-sessions-per-tick` and `max-llm-inflight`. Worst-case tick at default settings: 16 cluster calls (Haiku) + up to 16 extract calls (Sonnet) ≈ a few dollars.
- SQLite write volume is small (a few hundred rows per tick at most). The receipts-log doc's "transaction stays under 200ms" pattern applies.

### Security

- API key from env var `ANTHROPIC_API_KEY` (workspace standard). No key in config files.
- No network reads other than the LLM endpoint. JSONL files are local-filesystem only.
- Vault output is local-filesystem only. No external publishing in v1.
- The exclude-cwds list is the only guard against work-transcript leakage. The default ships with `~/repos/tatari-tv` excluded; the user is responsible for adding any additional sensitive paths.

### Testing Strategy

(See Testing section above.) Summary: FakeFabric fixtures for unit/integration tests; real-fabric gated smoke test; golden tests for rendering; ledger schema migrations covered by sqlx test helpers.

### Rollout Plan

- v1 ships behind no feature flag. The subsystem is opt-in by virtue of `sb facet daemon --install` being a manual operator action; nothing happens until the user installs the unit.
- `sb bootstrap` drops the config template but does NOT enable the daemon. The first harvest the user sees is the one they explicitly run via `sb facet harvest`.
- The first real harvest will produce a large initial burst of work-items (the whole corpus is "new"). The `max-sessions-per-tick` cap prevents this from being a single multi-hour LLM run; the daemon will catch up over many ticks. Operators can prime the corpus faster via `sb facet harvest --since <date>` in a loop.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| LLM clusters two unrelated threads into one work-item | Med | Med | `sb facet split <slug>` verb (v2; tracked in Open Questions). For v1, the operator can manually edit the work-item note and the ledger does not lock the title; re-render preserves the operator's edits if added to a `# Manual notes` section. |
| Open-vocabulary mode explosion (LLM invents 50 mode names) | Med | Low | Render groups any mode that appears in <3 moments under a single `## Other` section. `sb facet doctor` flags modes that have appeared only once across the entire ledger. |
| Quote excerpts leak secrets (API keys pasted into a session by the user) | Low | High | The extractor prompt includes "never include API keys, tokens, or other secrets in quote_excerpt; truncate to a placeholder." Real-LLM smoke test asserts this. Long-term: a redaction pass before vault write (out of scope for v1; called out as Open Question). |
| LLM budget consumed by a single runaway tick (e.g. catching up after a long gap) | Med | Low | Per-tick budget caps the spend; the daemon pauses cleanly at the next work-item boundary. The operator can raise the per-day budget temporarily for catch-up. |
| Vault note collisions with hand-written notes at the same slug | Low | High | Vault layout under `notes/facet/` is a dedicated subtree owned exclusively by facet. Vault writes refuse to overwrite a file not present in the ledger as a known facet artifact. |
| JSONL schema drift in Claude Code releases breaks the parser | Med | Med | `facet::jsonl` is forward-compatible: unknown line shapes are logged at WARN and skipped, not panicked on. `parsed.schema_drift_lines` is summed across the tick and reported in `sb facet doctor`. A non-zero drift count is a signal to inspect a sample line and extend the parser. The unit-test fixtures include forward-compatibility regression cases (an unknown line shape in the middle of a session). |
| One session's new turns exceed the extract model's context window | Med | Med | Per-session windows bound the input to one session's *new* turns since `last_extract_turn_uuid` (typically a tick's worth, not a full session history). If a single tick's worth still exceeds the model context (long catch-up after an outage), the cluster stage already breaks the session into multiple `cluster_assignments` rows by topic boundary; the extract LLM sees one row's range at a time. As a backstop, `extract::mine_moments` splits any single range exceeding `extract.max-input-tokens` (default 60_000) at a turn boundary and emits two extract calls. |
| The user installs facet on a machine that already has borg/cortex running, and the systemd unit conflicts | Low | Low | Unit is named `sb-facet.service`; no conflict with `sb-borg.service` or `sb-cortex.service`. `sb status` includes facet alongside the others. |
| Ledger schema migration during an in-flight harvest | Low | Med | Schema version pinned in `ledger_meta`. Daemon refuses to run if the on-disk schema is newer than the binary expects. `sb facet doctor` reports the mismatch. |
| Mid-write JSONL line truncation (scanned mid-append by Claude Code) | Med | Low | `facet::jsonl::parse_session_file` drops any final line not terminated by a newline. `sessions.last_cluster_offset` advances only past complete lines. The truncated suffix is read again on the next tick. |
| User deletes / moves `~/.claude/projects/` files between ticks | Low | Low | Ledger rows for the missing sessions remain. `sb facet doctor --reconcile` lists orphans; `--reconcile --prune` archives them. The harvester never panics on a missing file; missing-on-read is logged and skipped. |
| Vault root path changes between ticks (CLI override or config edit) | Low | Med | `vault::paths::resolve_vault_root` is re-evaluated each tick. Old notes at the previous root are NOT migrated automatically; an operator-run `sb facet relocate --from <old> --to <new>` verb is a v2 concern (Open Question). The new root becomes authoritative; previously-rendered notes at the old root become stale. |
| `llm.per-day-budget-usd < llm.per-tick-budget-usd` misconfiguration | Low | Low | `sb facet doctor` flags this on startup. The effective cap per tick is `min(per-tick, per-day-remaining)`. |
| Quote excerpts include `[[wikilinks]]` that resolve to other vault notes Obsidian doesn't expect | Med | Low | Quote excerpts are wrapped in code fences or blockquotes in the rendered note so the wikilink syntax is rendered literally and not resolved by Obsidian. |

## Open Questions

- **Redaction posture for shared output.** v1 is private-vault-only. When a v2 publish verb is added (likely a `sb facet publish --to <path>` flow), what redaction is automated and what stays manual? Specifically: client names, repo names that map to confidential projects, secrets that slipped into transcripts.
- **Work-item splitting and merging.** v1 has neither verb. When the LLM merges two unrelated threads into one work-item, the operator's only fix is to edit the ledger directly or archive the bad work-item. A clean `sb facet split <slug> --at <turn>` and `sb facet merge <slug> <slug>` interface is worth specifying before v2.
- **Codex / Gemini-CLI transcript ingestion.** The architecture supports additional `scan::Source` implementations. v2 question: do those go into the same work-items as Claude Code (cross-tool work-items) or into separate per-tool work-items?
- **Extractor pattern iteration.** The first version of `facet/patterns/facet-extract.md` will be naive. v1 ships with the pattern that produces acceptable golden outputs on a hand-curated set of recent JSONL turns; ongoing iteration of the pattern is expected. The pattern lives alongside borg's Fabric patterns under `facet/patterns/` and syncs to `~/.config/sb/patterns/` via `otto deploy`. A separate `facet/patterns/facet-extract-vN.md` versioning convention may be needed if breaking changes to the JSON output shape require ledger migrations.
- **Cluster prompt cost amortization for active sessions.** Today every tick that touches a session re-runs the cluster LLM over the session's new turns. For an actively-typed session that grows by a few turns every minute, this is N small Haiku calls instead of one larger call. A "wait until the session has been idle for X minutes before clustering" gate may be worth specifying; without it, hourly cadence on a long session can produce hourly cluster calls of diminishing marginal value.
- **What if the LLM proposes a work-item slug that collides with an archived one?** The current spec says slugs are frozen on creation and deduplicated against the ledger. An archived work-item's slug is taken; does the cluster prompt know about archived slugs, or does the dedup logic auto-suffix (`-2`)? Decision needed before Phase 3.

## References

- `notes/shopify-ceo-reveals-their-secret-ai-developer.md` - motivating frame: apprenticeship gap, shared taste development.
- `docs/design/2026-05-19-unified-sb-binary.md` - the sb subsystem pattern facet conforms to.
- `docs/design/2026-05-20-receipts-log.md` - the ledger-instead-of-markdown pattern facet adopts (each subsystem owns its own SQLite).
- `docs/design/2026-05-12-borg-pipeline-concurrency-caps.md` - the concurrency-cap incident facet's defaults are sized against.
- `~/repos/tatari-tv/claude-report` - prior art for JSONL scanning patterns (parent/subagent grouping by stem; git-remote URL parsing). NOT a runtime dependency; facet owns its parser, scanner, and repo resolver. See Alternatives Considered #2.
- `~/repos/scottidler/claude/HOME/repos/.claude/rules/log.md` - function-level DEBUG logging rule.
- `~/.claude/refs/personas.md` - home/work identity separation; basis for the default `tatari-tv/*` exclude.
