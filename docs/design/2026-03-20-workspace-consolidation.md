# Design Document: second-brain Workspace Consolidation

**Author:** Scott Idler
**Date:** 2026-03-20
**Status:** Approved
**Review Passes Completed:** 5/5

## Summary

Consolidate obsidian-borg (ingestion daemon) and obsidian-cortex (vault governance) into a single Cargo workspace called `second-brain` with a shared `vault` library crate. Both tools operate on the same Obsidian vault with the same schema but define types independently, causing drift (e.g., borg adding a ledger column that cortex's schema doesn't know about). A shared crate eliminates this class of bug by making the schema Rust code, not config or markdown.

## Problem Statement

### Background

obsidian-borg and obsidian-cortex are companion Rust CLI tools for a personal Obsidian vault:

- **borg** (v0.4.11): ingestion daemon. Receives URLs via Telegram, Discord, ntfy, HTTP, clipboard, and CLI. Fetches content, summarizes via Fabric/LLM, renders markdown notes with YAML frontmatter, writes to vault, logs to a dedup ledger.
- **cortex** (v0.2.7): governance tool. Lints naming, frontmatter, tags, scope, quality, broken links, duplicates. Generates intelligence (daily/weekly digests). Watches vault for changes.

Both projects:
- Target the same vault (`~/repos/scottidler/obsidian/`)
- Use the same frontmatter schema (domain, type, origin, status, method, tags, etc.)
- Shell out to the same `fabric` binary for LLM operations
- Share concepts: vault scanning, frontmatter parsing, filename sanitization, logging setup

But they define these concepts independently, causing:
1. **Schema drift** - borg added a `Path` column to the ledger that cortex didn't know about. Borg defines valid domains in `hygiene.rs` constants; cortex defines them in YAML config lists. Neither is authoritative.
2. **Duplicated code** - frontmatter parsing, filename sanitization, fabric wrapper, logging setup, config loading, secret resolution all exist in both projects with slight differences.
3. **Inconsistent conventions** - borg uses `env_logger` + `log`; cortex uses `tracing` + `tracing-subscriber`. Different config shapes for the same concepts (LlmConfig, VaultConfig).

### Problem

There is no single source of truth for the vault schema. Schema knowledge is scattered across Rust constants, YAML config files, and vault markdown documentation. When either tool evolves the schema, the other breaks silently or diverges.

### Goals

- Single source of truth for vault schema as Rust enums with serde serialization
- Shared library crate (`vault`) for types, frontmatter, note scanning, ledger, hygiene, config, logging, fabric, trace
- Both binaries (`borg`, `cortex`) in one workspace, importing from `vault`
- Unified logging on `env_logger` + `log` (drop tracing from cortex)
- All existing tests pass after migration
- Binary names shorten to `borg` and `cortex` (no `obsidian-` prefix)

### Non-Goals

- Merging borg and cortex into a single binary
- Rewriting business logic (pipeline, telegram, discord, linking, quality, etc.)
- Changing the vault's file structure or frontmatter format
- Adding new features to either tool
- Migrating config file locations (those stay at `~/.config/obsidian-borg/` and `~/.config/obsidian-cortex/`)

## Proposed Solution

### Overview

A Cargo workspace with three crates:

```
second-brain/
  Cargo.toml          # workspace root
  vault/              # shared library crate
  borg/               # ingestion binary (from obsidian-borg)
  cortex/             # governance binary (from obsidian-cortex)
```

### Architecture

```
  vault (library crate)
    schema.rs      -- THE canonical enums: Domain, NoteType, Origin, Status, Method
    frontmatter.rs -- Frontmatter struct, parse, serialize, validate
    note.rs        -- Note struct, scan_vault(), parse_note()
    ledger.rs      -- LedgerEntry, LedgerStatus, append/query/dedup
    hygiene.rs     -- Filename/tag/domain sanitization (URL normalization stays in borg)
    config.rs      -- Shared config types (VaultConfig, LlmConfig, MigrationConfig)
    logging.rs     -- Unified env_logger + log setup
    fabric.rs      -- Fabric subprocess wrapper
    trace.rs       -- Trace ID generation

  borg (binary crate) depends on vault
    Borg-specific: pipeline, telegram, discord, ntfy, routes, youtube,
    transcription, ocr, assets, markdown, router, notify, etc.
    Uses vault::schema::Domain, vault::frontmatter::Frontmatter, etc.

  cortex (binary crate) depends on vault
    Cortex-specific: daemon, linking, duplicates, quality, naming, tags,
    scope, autotag, intel, report, state, etc.
    Uses vault::schema for validation instead of config-defined enum lists.
```

### Data Model

#### vault::schema - The canonical schema

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Domain {
    Ai, Tech, Football, Work, Writing, Music, Spanish, Knowledge, Resources, System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NoteType {
    Youtube, Article, Github, Social, Reddit, Image, Pdf, Audio, Note, Vocab,
    Document, Code, Book, Video, Research, Daily, Meeting, Moc, Link, Poem, System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin { Authored, Assisted, Generated }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status { Unread, Reading, Reviewed, Starred }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Method { Telegram, Discord, Http, Clipboard, Cli, Ntfy, Manual }
```

Each enum gets: `Display`, `FromStr`, `as_str() -> &'static str`, `all() -> &[Self]`. Serde serializes as lowercase strings matching YAML frontmatter values.

**Migration mapping:**
- Borg's `types::IngestMethod` -> `vault::schema::Method` (same variants minus `Manual` which is new for cortex/manual edits)
- Borg's `hygiene::VALID_DOMAINS` constant -> `Domain::all()`
- Borg's `hygiene::DOMAIN_ALIASES` -> `vault::schema::domain_aliases()` or `normalize_domain()`
- Borg's `markdown::ContentType` enum -> stays in borg (rendering-specific, not schema)
- Borg's `types::ContentKind`, `IngestRequest`, `IngestResult`, `IngestStatus` -> stay in borg (ingestion-specific)
- Cortex's `config::SchemaConfig` (domains, types, origins, statuses, methods lists) -> replaced by `vault::schema::*::all()`

#### vault::frontmatter - Shared Frontmatter struct

Ported from cortex's `vault.rs`. Known fields extracted; unknown fields preserved in `extra: HashMap<String, serde_yaml::Value>`. Canonical field ordering in `to_yaml()`: title, date, type, domain, origin, tags, status, source, creator, then extras alphabetically.

#### vault::note - Note struct + vault scanning

Ported from cortex's `vault.rs`. Uses `walkdir` to scan vault, parses frontmatter from each `.md` file. Both borg (for migrate/audit) and cortex (for all commands) use this.

#### vault::ledger - Borg Ledger operations

Ported from borg's `ledger.rs`. Shared so cortex can read the ledger for validation. Uses `fs2` file locking.

#### vault::hygiene - Filename, tag, and domain normalization

Ported selectively from borg's `hygiene.rs`. Includes `sanitize_filename()`, `sanitize_tag()`, `normalize_domain()`, `normalize_text_input()`, domain alias mappings, and valid domain constants (replaced by `Domain::all()`).

**Not included in vault:** `clean_url()`, `canonicalize_url()`, `normalize_url()`. These depend on `CanonicalRule` which is a borg-specific config-driven regex system. URL normalization stays in borg's `hygiene.rs`; vault only gets the functions both tools need.

#### vault::config - Shared config types

Two separate VaultConfig concepts exist:
- Borg: `root_path`, `inbox_path`, `vault_name` (ingestion-specific)
- Cortex: `root_path`, `ignore`, `exclude`, `include` (scanning-specific)

The shared vault crate defines `ScanConfig` with `ignore` patterns (for `scan_vault()`), and both binaries map their config into it. Borg-specific fields (`inbox_path`, `vault_name`) stay in borg's config. Cortex-specific fields (`exclude`, `include`) stay in cortex's config (these are enforcement-level, not scan-level).

Also shared: `LlmConfig` (provider, model, api_key), `resolve_secret()`.

### Implementation Plan

#### Phase 1: Scaffold workspace + vault crate
- Root `Cargo.toml` with workspace members and shared deps
- `.otto.yml` from git-tools template (workspace-aware: `--workspace` flags)
- `clippy.toml` (too-many-arguments-threshold = 12)
- `CLAUDE.md` for project instructions
- `vault/Cargo.toml` as library crate
- `vault/src/lib.rs` with module declarations
- All vault source files: schema.rs, frontmatter.rs, note.rs, ledger.rs, hygiene.rs, config.rs, logging.rs, fabric.rs, trace.rs
- `cargo check -p vault` passes

#### Phase 2: Port borg into workspace
- `borg/Cargo.toml` with borg-specific deps, depends on `vault`
- Copy all borg `src/` files, `build.rs`
- Replace internal types with `use vault::*` imports
- Remove borg's local copies of extracted code
- Copy non-src files: `clients/`, `deploy/`, `docs/design/`, `fabric/`, example config
- `cargo check -p borg` passes

#### Phase 3: Port cortex into workspace
- `cortex/Cargo.toml` with cortex-specific deps, depends on `vault`
- Copy all cortex `src/` files, `build.rs`
- Replace `tracing::info!()` etc. with `log::info!()` etc. throughout
- Remove all `#[instrument(...)]` proc-macro attributes (tracing-specific)
- Remove `use tracing::instrument;` imports
- Drop tracing dependencies (tracing, tracing-appender, tracing-subscriber)
- Replace cortex's SchemaConfig enum lists with vault::schema
- Replace cortex's Frontmatter/Note with vault's versions
- Copy non-src files: `docs/design/`, example config
- `cargo check -p cortex` passes

#### Phase 4: Wire up CI + verify
- `.otto.yml` tasks: lint, check, test, cov, ci, build, install, deploy
- `cargo test --workspace` - all tests pass
- `cargo install --path borg` and `cargo install --path cortex` produce binaries
- Smoke test: `borg ingest --help`, `cortex lint --help`

## Alternatives Considered

### Alternative 1: Git submodule for shared types
- **Description:** Extract shared types into a third repo, include via git submodule or path dependency
- **Pros:** No monorepo; repos stay independent
- **Cons:** Submodule friction (version pinning, update ceremony). Three repos to maintain. CI complexity.
- **Why not chosen:** A workspace is simpler, keeps everything in sync, and matches the git-tools pattern already used.

### Alternative 2: Shared types via published crate
- **Description:** Publish `vault` to crates.io, depend from both projects
- **Pros:** True decoupling; versioned releases
- **Cons:** Publish ceremony for every schema change. Both tools must update in lockstep anyway.
- **Why not chosen:** These tools always evolve together against the same vault. Publishing adds friction with no benefit.

### Alternative 3: Schema as YAML/JSON config (current cortex approach)
- **Description:** Keep schema definitions in config files, validate at runtime
- **Pros:** No recompile needed to add a domain or type
- **Cons:** No compile-time safety. Two config files can define different lists. The borg-ledger column mismatch was caused by exactly this gap.
- **Why not chosen:** The whole point of this consolidation is to make schema drift impossible.

### Alternative 4: Keep separate repos, add a shared crate as git dependency
- **Description:** `vault = { git = "https://github.com/scottidler/vault" }`
- **Pros:** Independent repos, shared code
- **Cons:** Git dependency pinning, no workspace-level `cargo test --workspace`, harder to develop across crates
- **Why not chosen:** Workspace is strictly better for tightly coupled tools.

## Technical Considerations

### Dependencies

**Workspace-level shared deps:**
- chrono (0.4, serde feature), clap (4.6, derive), colored (3.1), dirs (6.0), env_logger (0.11), eyre (0.6), fs2 (0.4), log (0.4), regex (1), serde (1.0, derive), serde_json (1.0), serde_yaml (0.9), shellexpand (3.1), tokio (1, full)

**vault crate deps:** chrono, eyre, fs2, log, regex, serde, serde_json, serde_yaml, shellexpand, dirs, walkdir, env_logger

**borg-only deps:** axum, chrono-tz, reqwest, sha2, thiserror, teloxide, tower, tower-http, url, urlencoding, arboard, notify-rust, tokio-stream, tokio-util, hostname, base64, serenity

**cortex-only deps:** croner, glob, notify (file watcher), ureq, which

**Dropped from cortex:** tracing, tracing-appender, tracing-subscriber (replaced by env_logger + log from vault)

### Performance

No performance impact expected. The shared crate is a compile-time reorganization. No runtime behavior changes.

### Testing Strategy

- All existing unit tests from both projects are preserved in their respective crates
- Tests in `vault/` cover the extracted shared code (schema enum round-trips, frontmatter parsing, note scanning, ledger operations, hygiene functions)
- `cargo test --workspace` runs everything
- Smoke tests: `borg ingest --help` and `cortex lint --help` produce expected output

### Rollout Plan

1. Build and verify in `second-brain` repo
2. `cargo install --path borg` and `cargo install --path cortex` on both machines (desk.lan, ltl-7007.lan)
3. Update systemd service to point to new binary paths
4. Verify daemon starts, Telegram bot responds, cortex lint runs clean
5. Archive `obsidian-borg` and `obsidian-cortex` repos (do not delete)

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Borg tests break due to import changes | Medium | Low | Fix during port phase; tests are well-isolated |
| Cortex tracing->log migration misses instrumentation | Medium | Low | grep for remaining tracing:: and #[instrument] references; log macros are drop-in but #[instrument] attributes must be removed entirely |
| Schema enum doesn't cover all values in existing vault notes | Low | Medium | Audit vault frontmatter before finalizing enum variants |
| Config file paths change unexpectedly | Low | High | Config loading code stays per-binary; paths don't change |
| build.rs git-describe breaks in workspace | Low | Low | Each binary has its own build.rs; git describe works from workspace root |
| serde_yaml 0.9 is unmaintained | Low | Low | Both projects already depend on it; not a migration concern; track for future |
| Borg's string-concat YAML rendering diverges from vault Frontmatter | Low | Medium | Borg's render_note() stays as-is; vault Frontmatter is for read/validate, not borg's write path |
| Deploy paths change in workspace context | Medium | Low | Update .otto.yml deploy to use `cargo install --path borg` and `cargo install --path cortex` |

## Open Questions

- [x] Binary names: `borg` and `cortex` (confirmed, no obsidian- prefix)
- [x] Edition: 2024 (confirmed, both projects already use it)
- [x] Starting version: 0.5.0
- [ ] Should `borg` and `cortex` config files eventually be renamed from `obsidian-borg.yml` / `obsidian-cortex.yml` to `borg.yml` / `cortex.yml`? (deferred - not part of this migration)
- [ ] Should the vault markdown schema files (`system/schemas/*.md`) be generated from `vault::schema` or kept as manual documentation? (deferred)

## References

- obsidian-borg repo: `~/repos/scottidler/obsidian-borg/`
- obsidian-cortex repo: `~/repos/scottidler/obsidian-cortex/`
- git-tools workspace pattern: `~/repos/scottidler/git-tools/`
- Vault schema docs: `~/repos/scottidler/obsidian/system/schemas/`
- Borg Ledger: `~/repos/scottidler/obsidian/system/borg-ledger.md`
