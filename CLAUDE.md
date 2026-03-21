# second-brain - Claude Code Instructions

## Project Overview

Cargo workspace consolidating obsidian-borg (ingestion daemon), obsidian-cortex (vault governance), and oracle (knowledge retrieval MCP) with a shared `vault` library crate. All tools operate on the same Obsidian vault with the same schema.

- **Repo:** `~/repos/scottidler/second-brain/`
- **Vault:** `~/repos/scottidler/obsidian/`
- **Design docs:** `docs/design/2026-03-20-workspace-consolidation.md`, `docs/design/2026-03-21-oracle-mcp.md`

## Architecture

```
second-brain/
  vault/    -- shared library crate (schema, frontmatter, note, ledger, hygiene, config, logging, fabric, trace)
  borg/     -- ingestion binary (Telegram, Discord, ntfy, HTTP, clipboard, CLI)
  cortex/   -- governance binary (lint, link, intel, daemon, migrate)
  oracle/   -- knowledge retrieval MCP server (search, browse, domain briefs, ledger queries)
```

## Key Conventions

- **Edition:** 2024
- **Logging:** env_logger + log (unified; no tracing) for borg/cortex; tracing for oracle (rmcp compatibility)
- **Schema:** vault::schema is THE single source of truth for Domain, NoteType, Origin, Status, Method. vault enums have feature-gated `schemars::JsonSchema` derives for MCP tool schemas.
- **Config:** borg reads ~/.config/obsidian-borg/obsidian-borg.yml; cortex reads ~/.config/obsidian-cortex/obsidian-cortex.yml; oracle reads ~/.config/oracle/oracle.yml
- **Binary names:** `borg`, `cortex`, and `oracle` (no obsidian- prefix)

## Testing

```
cargo test --workspace
```

## Building

```
otto ci          # full CI pipeline
otto install     # build and install binaries
```
