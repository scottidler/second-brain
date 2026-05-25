# second-brain

Rust workspace that ships a single `sb` binary subsuming three subsystems
that share one Obsidian vault:

- **borg** (ingestion daemon) - Telegram, Signal, Discord, ntfy, HTTP,
  clipboard, Firefox capture extension. Distills each input via Fabric
  patterns and lands it as a markdown note.
- **cortex** (vault governance daemon) - lint, link discovery, tag sweeps,
  classification, daily/weekly intel, semantic embedding.
- **oracle** (MCP knowledge retrieval) - hybrid BM25 + vector search,
  domain briefs, ledger queries. Launched on demand via `.mcp.json`.

All three operate against `~/.config/sb/` for config and the vault
filesystem for content.

## Prerequisites

- **Rust toolchain** - `rustup` or your distro's package. Required for
  `cargo install`.
- **Go toolchain** - for `go install github.com/danielmiessler/fabric/cmd/fabric@latest`.
  Operators who would rather avoid Go can download a pre-compiled fabric
  binary from https://github.com/danielmiessler/fabric/releases instead.
- **Firefox** (optional) - only needed if you want the capture extension
  installed via `sb bootstrap --extension`.

## Install

```bash
# 1. Install sb (this repo's single binary)
cargo install --git https://github.com/scottidler/second-brain --bin sb

# 2. Install Daniel Miessler's fabric (external dep; provides
#    extract_wisdom, summarize, create_tags, etc.)
go install github.com/danielmiessler/fabric/cmd/fabric@latest
fabric -y --update-patterns

# 3. Provision sb's canonical assets, install systemd units, prefetch
#    the embedding model
sb bootstrap
```

Bootstrap drops config templates, shared vocabulary (`canonical-tags.yml`,
`tag-mapping.yml`, `tag-proposals.yml`), and 14 fabric distill patterns
into `~/.config/sb/`. It also installs the borg and cortex systemd
user units and prefetches the embedding model (~100 MB on first run).

Re-running `sb bootstrap` is idempotent: existing files are preserved.
Use `sb bootstrap --force` to refresh shared vocabulary and patterns from
the binary's embedded copies (per-host templates are still preserved).

## Configure and start daemons

Edit `~/.config/sb/borg.yml` to wire transports (Telegram, Signal, Discord,
desktop notifications). The template ships with every transport commented
out so you opt in to what you need:

```bash
$EDITOR ~/.config/sb/borg.yml
```

Start the daemons and verify:

```bash
systemctl --user start borg cortex
sb doctor   # every section should report green
```

`sb status` shows live state; `sb doctor` adds severity-tagged findings
with actionable fix commands (missing assets, missing external binaries,
drift between installed copies and the binary's embedded source-of-truth).

## Optional: Signal transport

Signal ingest is opt-in. Bootstrap does not install the `signal-rs` CLI
because it would mean pulling in a network-capable binary that operators
didn't ask for; `sb doctor` surfaces the absence with the exact install
command if you add a `signal:` block to `borg.yml`.

```bash
# 1. Install the signal-rs CLI binary (pinned version matches the dep
#    borg links against; mismatches are surfaced by sb doctor signal)
cargo install --git https://github.com/scottidler/signal-rs --bin signal-rs --tag v0.2.1

# 2. Stop borg so it doesn't race with the link handshake
systemctl --user stop borg

# 3. Link this machine as a linked device. Use the borg-owned state dir;
#    sb doctor expects exactly this path.
signal-rs link --name borg --state-dir ~/.local/share/sb/borg/signal-state/
# Scan the QR with the primary phone (Settings -> Linked Devices -> +).

# 4. Uncomment the signal: block in ~/.config/sb/borg.yml.
#    Set signal.host to the exact hostname of this machine - Signal-Server
#    fans Note-to-Self messages to every linked device, so unpinned ingest
#    would silently double-ingest on multi-machine setups.
$EDITOR ~/.config/sb/borg.yml

# 5. Restart and verify
systemctl --user start borg
sb doctor   # signal section should now show "linked"
```

See `docs/design/2026-05-24-signal-as-borg-transport.md` and
`docs/design/2026-05-24-signal-state-dir-internalization.md` for the
design rationale behind the Signal integration.

## Day-to-day operation

```bash
# Inspect live state
sb status
sb doctor

# Run cortex tasks on demand
sb cortex sweep --proposals
sb cortex intel --mode daily
sb cortex classify --apply

# Query the knowledge vault (via the MCP server in your client of choice)
# Configured via .mcp.json -> `sb oracle serve`
```

## References

- `CLAUDE.md` - architecture overview and per-subsystem invariants.
- `docs/design/` - design memos. The install pipeline behaviour
  documented here is specified in
  `docs/design/2026-05-24-install-pipeline.md`.
