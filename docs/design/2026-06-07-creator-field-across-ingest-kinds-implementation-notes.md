# Implementation Notes: `creator` across youtube/github/blog ingestion

Running, append-only record of how the implementation interprets or diverges
from `2026-06-07-creator-field-across-ingest-kinds.md`. One section per phase.

## Phase 1: Carry author on ContentType + single `creator_for` render

### Design decisions
- `creator_for` (`borg/src/markdown.rs`) treats an empty/whitespace carried
  value as `None` (`raw.filter(|s| !s.trim().is_empty())`), so an empty YouTube
  uploader or GitHub owner falls back to `default_creator` rather than emitting
  a `creator: ""` line. The spec said "never fabricate"; this enforces it on the
  carrier side too.
- The relocated `ct` construction (`pipeline.rs::process_url_inner`) keys on
  `github_repo.is_some()` rather than on `link_name == "github"`. This mirrors
  the existing distiller dispatch one block below (which already routes by
  `github_repo.is_some()`), so a deep github path (`link_name == "github"` but
  `parse_repo_url == None`) becomes `Article { author: None }` exactly as the
  design's Data Model section requires - and the two routing decisions can never
  disagree about what is a "repo root".

### Deviations
- None. The variant shapes, the single-write consolidation, and the YouTube arm
  retaining only `duration:` all match the spec.

### Tradeoffs
- `creator_for` returns `Option<String>` (owned) rather than `Option<&str>`.
  Owned is simpler at the single call site and the values are short; borrowing
  would force a lifetime on the helper for no measurable gain.

### Open questions
- None for Phase 1.

## Phase 2: Blog byline via the fetcher contract

### Design decisions
- `byline::extract` uses `regex` + `serde_json` (both already in `borg`'s
  dependency tree) rather than adding an HTML-parser crate. The design's
  Dependencies note explicitly preferred an existing workspace dep; there is no
  HTML parser already pulled in, but the deterministic meta/JSON-LD/link ladder
  is small enough that targeted regex + a hand-rolled `attr` scanner is correct
  and avoids a new dependency.
- The live article path discards `FetchMeta` entirely (`jina::fetch_article_markdown`
  returned only `String`; `process_article_*` returned only `(title, md, ContentType)`
  with the `ContentType` thrown away at the call site). To make the byline reach
  a note, the third tuple element of `process_article_fabric`/`process_article_jina`
  was repurposed from the already-discarded `ContentType` to `Option<String>`
  (the byline), and `jina::fetch_article_markdown` now returns `(String, Option<String>)`.
  Over-long meta values are rejected (not truncated) - a truncated paragraph is
  not a name.

### Deviations
- **Dropped the design's "switch Jina fetcher to JSON mode" step (Phase 2,
  bullet 4).** That step targets `stages::fetcher::JinaFetcher`, which is part of
  `MultiFetcher` - confirmed *not* on the live `process_url_inner`/reingest path
  this design serves (live Jina is `jina.rs::jina_fetch` with
  `Accept: text/markdown`). Switching the trait-chain fetcher to JSON would be
  dead work for this design and is exactly the open question a parallel session
  owns. Live jina-markdown therefore yields `author: None`; the only live blog
  path that currently carries a byline is the browser-UA fallback inside
  `jina::fetch_article_markdown` (it runs `byline::extract` on raw HTML). The
  Jina-JSON author, when that workstream lands, composes as
  `json_author.or(browser_byline)` in `jina.rs` - the seam is documented in the
  function's doc comment.

### Tradeoffs
- Repurposing the discarded third tuple element vs. adding a 4th element: chose
  to repurpose, since the `ContentType` it replaced was already dead (every call
  site bound it to `_`). One fewer field to thread, and the compiler confirmed
  no live reader of the old value existed.
- `byline::extract` coverage is the pure-function unit tests (each ladder rung +
  JSON-LD string/object/array/@graph + entity decode + length cap + negative).
  Per the advisor, no networked `BrowserUaFetcher` test was added - the wiring
  (`extract(&raw)` -> `meta.author`) is a thin one-liner and the extraction is
  what carries the risk.

### Open questions
- [ ] (Owned by the parallel session) Jina JSON-mode author field name(s). Until
  resolved, live jina-markdown ingests leave `creator` empty for blogs.
- [ ] (Out of scope, per design) Raising blog coverage on the `fabric -u` success
  path - it exposes no HTML, so most blog ingests still resolve to no byline.

### Addendum (open questions resolved by the parallel spike)
- Jina JSON field is `data.metadata.author`, and it mirrors `<meta name="author">`
  **only** - it does NOT surface JSON-LD author (verified across WordPress, Ghost,
  Forem). `fabric -u` is itself a Jina markdown scrape (`fabric --help`: "Scrape
  website URL to markdown using Jina AI"), byte-identical to the jina.rs markdown
  path, so neither default path carries a byline. This makes the deferral correct
  on the merits: a Jina-JSON fold-in would only add `<meta>`-class coverage, which
  `byline::extract` over raw HTML on the browser-UA rung **already** captures -
  and that rung additionally recovers the JSON-LD population (the bot-walled CMS
  publications that fall through to it are exactly the JSON-LD-rich ones). The two
  sources are complementary; the byline split in the design was right.

## Phase 3: Ledger "Author" column

### Design decisions
- `creator` added to `properties` (`displayName: Author`) and to all three views'
  `order` lists (after `domain` in Ledger/By-Method; after `method` in By-Domain,
  which groups by domain). Position is cosmetic per the design; the column appears
  in every view. Validated as well-formed YAML with `creator` present in all views.

### Deviations
- No git commit for this phase. `system/views/borg-ledger.base` lives in the
  `scottidler/obsidian` vault repo and is **untracked** there; that vault
  propagates via Syncthing, not git (see the design's Rollout Plan and the
  borg-topology context). The on-disk edit is the deliverable.

### Tradeoffs
- None.

### Open questions
- None. (Obsidian render verification is a manual visual check the user can do;
  the YAML is structurally valid and YouTube notes are already `creator`-rich, so
  the column populates immediately.)

## Phase 4: Backfill existing GitHub notes via `audit --fix`

### Design decisions
- The scan iterates `build_note_index` (every vault note carrying a `source:`)
  rather than the completed-ledger entries, so the backfill covers all github
  repo-root notes regardless of ledger state. The duplicate check (section 4)
  already uses the same index, so this is consistent with the existing scanner.
- `set_creator_if_empty` inserts the `creator:` line directly after `type:` to
  echo the render's frontmatter ordering, and re-reads/re-checks emptiness at fix
  time (not just at scan time) so a value written between scan and fix is never
  clobbered. The finding also only fires when `creator` is empty, so there are
  two independent guards against overwrite.
- `FindingKind::GithubCreatorMissing` auto-surfaces on the CLI as
  `--fix github-creator-missing` because `sb`'s Audit command takes
  `Option<Vec<borg::audit::FindingKind>>` and the enum derives `ValueEnum` with
  `rename_all = "kebab-case"` - no bespoke CLI wiring.

### Deviations
- None. This matches the design's Phase 4 (new `FindingKind`, fix sets
  `creator = parse_repo_url(source).owner`, no network, surfaced via the existing
  `audit --fix` verb, only repo roots, never overwrites).

### Tradeoffs
- Tests live in audit.rs's existing inline `#[cfg(test)] mod tests` block rather
  than an extracted `audit/tests.rs`. The repo rule prefers extracted test files,
  but audit.rs already uses an inline block; extracting the whole module is a
  mechanical refactor out of scope for this phase, and mixing styles within one
  file is worse than matching what is there.

### Open questions
- None. Run `sb borg audit --fix github-creator-missing` once on the daemon host
  (the vault is Syncthing-propagated, so the rewritten notes fan out to other
  hosts); re-measure github `creator` coverage afterward. Blog backfill remains a
  non-goal (reingest is the per-note lever).
