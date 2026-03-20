# second-brain - Claude Code Instructions

## Project Overview

Cargo workspace consolidating obsidian-borg (ingestion daemon) and obsidian-cortex (vault governance) with a shared `vault` library crate. Both tools operate on the same Obsidian vault with the same schema.

- **Repo:** `~/repos/scottidler/second-brain/`
- **Vault:** `~/repos/scottidler/obsidian/`
- **Design doc:** `docs/design/2026-03-20-workspace-consolidation.md`

## Architecture

```
second-brain/
  vault/    -- shared library crate (schema, frontmatter, note, ledger, hygiene, config, logging, fabric, trace)
  borg/     -- ingestion binary (Telegram, Discord, ntfy, HTTP, clipboard, CLI)
  cortex/   -- governance binary (lint, link, intel, daemon, migrate)
```

## Key Conventions

- **Edition:** 2024
- **Logging:** env_logger + log (unified; no tracing)
- **Schema:** vault::schema is THE single source of truth for Domain, NoteType, Origin, Status, Method
- **Config:** borg reads ~/.config/obsidian-borg/obsidian-borg.yml; cortex reads ~/.config/obsidian-cortex/obsidian-cortex.yml
- **Binary names:** `borg` and `cortex` (no obsidian- prefix)

## Testing

```
cargo test --workspace
```

## Building

```
otto ci          # full CI pipeline
otto install     # build and install binaries
```
