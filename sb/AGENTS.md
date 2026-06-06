# sb — Unified CLI (Composition Root)

> Read before touching `sb/`. Parent: `../CLAUDE.md`.

## Purpose

`sb` is the only binary in the workspace. It unifies the lib-only crates (borg, cortex, oracle) under one CLI and threads versions into them. Subcommands dispatch to library entry points; `status` / `doctor` / `bootstrap` are standalone. It owns the workspace's only `build.rs` (emits `GIT_DESCRIBE`). **The CLI is the only place `println!`/`eprintln!` are allowed** — libraries return typed data; `sb` formats and prints.

## Entry Points

- `main.rs::main()` — eyre hook → `Cli` parse → logger init → dispatch.
- `cli.rs`: `Cli` (clap root, `version = env!("GIT_DESCRIBE")`), `Cmd` (subcommand enum: Borg, Cortex, Oracle, Status, Doctor, Bootstrap), `Cmd::run()` (async dispatch).
- `build.rs` — `GIT_DESCRIBE` from `git describe --tags --always`.

## Command Modules (`src/cli/`)

- `borg.rs` (+`cli/borg/`) — Daemon, Ingest, Note, Hotkey, Extension, Migrate, Audit, Log, Reingest, … → borg lib.
- `cortex.rs` — Classify, Lint, Link, Intel, State, Daemon, Migrate, Sweep, Summarize, Embed, … → cortex lib (after-help checks `fabric` availability).
- `oracle.rs` — Serve, Index, Stats, Call (`--list`, `--json`); width-aware tool-list formatting; exit codes via `outcome_is_failure`.
- `bootstrap.rs`, `status.rs`, `doctor.rs`, `checks.rs` — setup + health.

## Contracts & Invariants

- **Version threading:** `env!("GIT_DESCRIBE")` / `env!("CARGO_PKG_VERSION")` are passed into libs at init (e.g. `borg::serve_init(config, env!("GIT_DESCRIBE"))`, `extension::stage(…, env!("CARGO_PKG_VERSION"), …)`). Libraries never call `env!` for these.
- **Errors:** eyre hook installed before parse; libs return `eyre::Result`; `sb` formats.
- **Stdio belongs to `sb`** — libs must not print directly.

## Patterns

- **Add a subcommand:** add a `Cmd` variant → create `src/cli/<name>.rs` with a `<Name>Cli`/`<Name>Args` struct + `pub async fn run(self) -> Result<()>` → add a dispatch arm in `Cmd::run()` → `pub mod` declare it.

## Module Map

- `main.rs`, `lib.rs`, `cli.rs`, `error.rs` (+`error/`), `logger.rs`, `build.rs`.
- `cli/`: `borg.rs`, `cortex.rs`, `oracle.rs`, `bootstrap.rs`, `status.rs`, `doctor.rs`, `checks.rs`.
