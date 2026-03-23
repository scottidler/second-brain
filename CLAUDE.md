# second-brain - Claude Code Instructions

## Project Overview

Cargo workspace consolidating obsidian-borg (ingestion daemon), obsidian-cortex (vault governance), and oracle (knowledge retrieval MCP) with a shared `vault` library crate. All tools operate on the same Obsidian vault with the same schema.

- **Repo:** `~/repos/scottidler/second-brain/`
- **Vault:** `~/repos/scottidler/obsidian/`
- **Design docs:** `docs/design/` (workspace consolidation, oracle MCP, classify pipeline, tag sweeper, etc.)

## Architecture

```
second-brain/
  vault/    -- shared library crate (schema, frontmatter, note, ledger, hygiene, canonical, config, logging, fabric, trace)
  borg/     -- ingestion binary (Telegram, Discord, ntfy, HTTP, clipboard, CLI)
  cortex/   -- governance binary (lint, link, intel, sweep, daemon, migrate)
  oracle/   -- knowledge retrieval MCP server (search, browse, domain briefs, ledger queries)
  config/   -- shared config source of truth (canonical-tags.yml, tag-mapping.yml, tag-proposals.yml)
```

## Key Conventions

- **Edition:** 2024
- **Logging:** env_logger + log (unified; no tracing) for borg/cortex; tracing for oracle (rmcp compatibility)
- **Schema:** vault::schema is THE single source of truth for Domain, NoteType, Origin, Status, Method. vault enums have feature-gated `schemars::JsonSchema` derives for MCP tool schemas.
- **Config:** borg reads ~/.config/borg/borg.yml; cortex reads ~/.config/obsidian-cortex/obsidian-cortex.yml; oracle reads ~/.config/oracle/oracle.yml
- **Shared config:** ~/.config/second-brain/ has canonical-tags.yml, tag-mapping.yml, tag-proposals.yml (source of truth in `config/`). Both borg and cortex read from this shared directory.
- **Patterns:** borg's Fabric patterns live at `~/.config/borg/patterns/` (source of truth in `borg/patterns/`)
- **Tags:** 110 canonical tags, max 7 per note. Borg post-filters Fabric output through the canonical vocabulary. Cortex `sweep` command migrates and governs tags.
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

## Install (for /shipit)

```bash
cargo install --path borg && systemctl --user restart borg
cargo install --path cortex && systemctl --user restart cortex
cargo install --path oracle
cp borg/patterns/*.md ~/.config/borg/patterns/
mkdir -p ~/.config/second-brain && cp config/canonical-tags.yml config/tag-mapping.yml config/tag-proposals.yml ~/.config/second-brain/
```

borg and cortex run as systemd user daemons and must be restarted after install.
oracle is an MCP server launched on demand, no restart needed.
