# vault — Shared Schema & Primitives

> Read before touching `vault/`. This is the workspace's source of truth. Hybrid-search engine: `src/search/AGENTS.md`. Parent: `../CLAUDE.md`.

## Purpose

The single source of truth for the Obsidian-vault note schema (NoteType / Origin / Status / Method; `Domain` was deleted 2026-09-20 and `tags` is the sole classification facet), YAML frontmatter, note parsing, path resolution, the search index, embeddings, and the L2 `Distilled` contract. Consumed by borg, cortex, oracle, distillers, and sb. If a primitive is shared across crates, it lives here — not in a consumer.

## Entry Points

- `vault::schema` — the enums (`.as_str()`, `.all()`, `FromStr`), with feature-gated `schemars::JsonSchema` derives for MCP tool schemas.
- `vault::note::scan_vault(vault_root, scan_config) -> Vec<Note>` (rayon `par_iter`, path-sorted, deterministic); `parse_note(vault_root, path)`.
- `vault::frontmatter::Frontmatter` — known fields extracted, unknown fields preserved in an extras map (distillers / `cortex-*` keys).
- `vault::paths::{expand_tilde, deserialize_tilde_pathbuf, config_root, resolve_vault_root}`.
- `vault::ledger` — borg's dedup log helpers; `vault::receipts::FailureStage` (the seven terminal stages).
- `vault::embedding::{EmbeddingModel, load_active_model, embed_query}` (Candle / fastembed, feature-gated).
- `vault::distilled::Distilled { summary, tldr, slug, enumeration, key_ideas, claims, tags, links, kind_specific, meta, transcript }`.
- `vault::watcher::VaultWatcher::start(vault_root, config, applying_flag)` — debounced change stream.
- `vault::process::run(cmd, stdin, timeout, label) -> Outcome { Exited | TimedOut }`: the one subprocess primitive. Owns all three pipes (stdin `/dev/null` when `None`, never inherited), drains on threads, spawns in its own process group and SIGKILLs the group at the deadline. `kill_registered()` / `install_interrupt_handler()` cover Ctrl-C for interactive sb commands.
- `vault::wikilink::{parse, stem, WikiLink, Resolver}`: the one wikilink parser. `parse(body)` yields `WikiLink { target, heading, block, alias, embed, span }` for every link Obsidian renders, skipping code (backtick/tilde fences, inline backtick spans, 4-space/tab-indented lines). `Resolver::new(paths)` indexes every component-boundary path suffix once, so `resolve(target)` is a hash lookup: `dir/x` matches `dir/x.md` and `a/dir/x.md`, never `otherdir/x.md`; a bare target matches every note with that stem. Ungated (borg does not enable `search`).
- `vault::canonical::CanonicalSet { all, no_segment, max_per_note }`: the one loaded vocabulary snapshot, built by `CanonicalTagsFile::canonical_set()` and taken by `match_to_canonical` / `filter_and_cap`. Absorbed borg's private `CanonicalState` shape.

## Contracts & Invariants

- **Schema is law.** `vault::schema` enums are THE source of truth — never hardcode `"ai"`/`"article"`/`"authored"` strings in consumer crates; import the enums.
- **L2 Distilled contract** `{summary, tldr, slug, enumeration, key_ideas, claims, tags, links, kind_specific, meta, transcript}` is the finalized extractor output consumed by renderers (`distillers`) and borg's publish stage.
- **Tilde expansion at the boundary.** Any user-supplied path MUST pass through `expand_tilde` / `deserialize_tilde_pathbuf` before a filesystem call. For `PathBuf` config fields use `#[serde(deserialize_with = "vault::paths::deserialize_tilde_pathbuf")]`.
- **Vault-root precedence:** CLI override > config (`vault.root-path`) > marker-gated CWD (a `.obsidian/` dir must exist). No silent CWD fallback.
- **`embedding_config` pins `active_model` + `active_dim`** (384 for bge-small-en-v1.5); both cortex and oracle read these on dispatch so they never drift.
- **The segment guard is honored in both matchers.** `no-segment-match` in `canonical-tags.yml` lists tags that tier-2 segment splitting may never mint (`work-life-balance` must not produce `work` and `life`). `match_to_canonical` and `filter_and_cap` each carry their own tier-2 loop; a new guard goes in both or it does not hold.
- **Removing a tag from the vocabulary means deleting its self-map.** `match_to_canonical` checks `tag-mapping.yml` *before* canonical membership, so a leftover `x: x` self-map keeps minting a tag the vocabulary no longer contains.
- **`scan_vault` is deterministic** (par_iter + sort by path). **Pinned** is strict `Some(true)` — typos/nulls parse as not-pinned, never a parse error.

## Patterns

- **Add a schema variant:** add the enum arm + `.as_str()`/`FromStr`, add a roundtrip/serde test.
- **Add a frontmatter field:** add to `Frontmatter`, parse in `from_value` (extract or extras), emit in canonical order in `to_yaml`.
- **New path config field:** annotate with `deserialize_tilde_pathbuf` (or call `expand_tilde` at the boundary for `String`).

## Anti-patterns

- **Literal `~` directory bug:** `fs::create_dir_all("~/vault")` creates a literal `~` dir in CWD — always `expand_tilde` first.
- **Fabricated fallback path:** `dirs::*_dir().unwrap_or_else(|| PathBuf::from("~/.local/share"))` creates a literal `~`. Use `.expect("… set HOME/XDG_*")` — panic is correct when both are unset.
- **Spawning with `Command::output()` / `spawn()` + `try_wait` polling:** a child that writes more than the pipe buffer, or a grandchild holding the pipe, hangs the caller; an inherited stdin can hang a child that reads it. Go through `vault::process::run`.
- **A private wikilink regex:** hand-rolled `[[...]]` patterns drifted into three variants (missing `#heading`, `dir/x`, code). Parse with `vault::wikilink::parse` and compare through `Resolver`.
- **Schema duplication / hardcoded model string** in consumer crates — import the enum; read `active_model` from `embedding_config`.

## Module Map

- **Schema/notes:** `schema.rs`, `frontmatter.rs`, `note.rs`, `detail.rs`, `table.rs` (+`table/`), `wikilink.rs` (+`wikilink/`).
- **Paths:** `paths.rs` (+`paths/`).
- **Capture/ledger:** `ledger.rs`, `receipts.rs` (+`receipts/`), `intake.rs` (+`intake/`), `trace.rs`.
- **Embeddings/distilled:** `embedding.rs` (+`embedding/`), `distilled.rs` (+`distilled/`).
- **Search:** `search.rs` (+`search/`) — see `src/search/AGENTS.md`.
- **Tags/hygiene:** `canonical.rs`, `hygiene.rs`.
- **Daemon client / ingest queue:** `daemon.rs` (+`daemon/`: `HotkeyConfig` incl. `request-timeout`, the client-side borg daemon address, `client_auth_token`, the first-party bearer resolver, and under feature `http` `daemon::client::DaemonClient`, the ONE request path to the daemon: address + token + timeout resolved once, status checked before any body parse; shared by borg and oracle), `http.rs` (+`http/`; feature `http`: `client(timeout)` and `builder(Timeouts)` (`Timeouts::total` for request/response, `Timeouts::Stream { connect, read }` for ntfy), the one place a `reqwest::Client` is built; `clippy.toml` `disallowed-methods` bans `Client::new`/`Client::builder`/`ClientBuilder::new` everywhere else, tests included), `queue.rs` (+`queue/`: the `QueueSnapshot` wire contract for `GET /queue`).
- **Misc:** `watcher.rs`, `rss.rs` (+`rss/`), `logging.rs`, `config.rs` (incl. `load_config` + `Normalize`, the one borg.yml loading chain), `fabric.rs` (runs fabric through `process.rs`), `process.rs` (+`process/`), `text.rs` (char-accurate, panic-free string truncation), `tombstone.rs` (the shared soft-retire tombstone shape written by `cortex::association` and `borg::dedupe`), `identity.rs` (+`identity/`: `NoteIndex`, the note `trace:` back-edge and the `superseded-by` convergence that resolves a staged trace to the ONE note it produced; owned here because the `trace:` map is one-way and independent copies of this drift, and because cortex must not gain a borg dependency to reach `borg::harvest::identity`).
