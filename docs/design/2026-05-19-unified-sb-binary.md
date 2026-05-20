# Design Document: collapse borg/cortex/oracle into one `sb` binary

**Author:** Scott Idler
**Date:** 2026-05-19
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

The second-brain workspace today ships three binaries (`borg`, `cortex`, `oracle`) that share a `vault` library crate, a `distillers` library crate, a vault on disk, a configuration vocabulary, and a release cadence. The three-binary federation is historical, not architectural: the vault crate is the real architectural kernel, the three binaries are just interface surfaces over it. Collapse them into one binary `sb` with the shape `sb <subsystem> <verb>` (e.g., `sb borg ingest`, `sb cortex sweep`, `sb oracle serve`), plus a small set of cross-cutting root commands (`sb status`, `sb doctor`, `sb bootstrap`). Every existing borg/cortex/oracle subcommand becomes an `sb`-prefixed equivalent with the same flags and the same behavior. The three existing crates become library-only crates; a new `sb` bin crate is the only `[[bin]]` in the workspace.

## Problem Statement

### Background

The workspace today contains five crates: `vault` (shared schema/IO library), `distillers` (shared Stage-2 distiller library), `borg` (ingestion daemon + CLI), `cortex` (vault governance daemon + CLI), `oracle` (MCP server + index/stats CLI). Each of the latter three is a `[[bin]]` crate with its own `main.rs`, its own clap tree, its own config file, its own log file, its own systemd unit (for borg and cortex), its own install step.

This federation is a side effect of how the project grew, not a design decision. The original three projects became one workspace; the binaries kept their separate identities even though they now share a kernel.

### Problem

The cost of three binaries:

- **Three CLIs to maintain.** Each has its own `--help`, its own global flags (`--config`, `--log-level`, `--vault`), its own logger init, its own config-loading code. The same kind of fix has to land in three places.
- **Three names to remember.** "Was it `borg dlq list` or `cortex dlq list`? Is `stats` under borg or oracle? Where does `migrate` live?" The federation forces the user to memorize which subsystem owns which verb.
- **Two `migrate` subcommands** (one under borg, one under cortex). Two different code paths for "migrate the vault" because nobody ever consolidated them.
- **Three install steps.** CLAUDE.md's Install section runs `cargo install --path borg`, `cargo install --path cortex`, `cargo install --path oracle`. `otto deploy` now wraps this, but the version-drift vector exists: if any one install fails, the binaries are mismatched until the next bump+deploy cycle. The three binaries can be on three different commits at a worst-case point in time.
- **No coherent operator entry point.** There is no `--help` that says "this is your second-brain; here's everything it can do." The user has to discover the surface across three commands.
- **Cross-cutting commands have nowhere to live.** "What's the state of my second-brain?" needs to read borg's ledger, cortex's embedding coverage, oracle's index stats, and the systemd state of two daemons. Today this means a fourth tool that shells out, or wedging it into one of the three existing binaries and accepting the conceptual mismatch.

### Goals

- One binary `sb` is the user-facing entry point for all CLI operations against the second-brain workspace.
- Every existing borg/cortex/oracle subcommand is reachable as `sb <subsystem> <verb>` with unchanged flags and unchanged behavior.
- New root-level commands (`sb status`, `sb doctor`, `sb bootstrap`) exist to cover cross-cutting operations.
- The two long-running daemons (today borg's and cortex's) keep running as separate processes under separate systemd units, just invoked from the same binary.
- The MCP server (today `oracle serve`) keeps its stdio purity contract.
- Process isolation between the daemons is preserved.
- The architectural invariant "borg cannot write to the FTS5 index" is preserved (the mechanism changes; see Architecture / "What we lose").
- `main.rs` for the new bin crate is thin: clap parsing, logger init, dispatch to lib code. All real logic lives in library crates.

### Non-Goals

- Consolidating the two `migrate` subcommands (borg's and cortex's) into one. Real drift, but separate work; this design preserves both as `sb borg migrate` and `sb cortex migrate`.
- Reworking the daemons' internals, their pipelines, their distillers, their FTS5 schema, or their config formats.
- Designing the body of `sb status`, `sb doctor`, or `sb bootstrap`. Those land as stub commands in this change and get filled in by follow-up design docs; this doc establishes the home and the dispatch wiring.
- Reworking the shared `vault` and `distillers` crates. They stay as-is, just with one more dependent (the new `sb` bin crate, transitively through the three lib crates).
- Renaming external integration touch-points (HTTP port 8181, Telegram bot, ntfy, fabric pattern names, frontmatter keys). All of these stay.

## Proposed Solution

### Overview

Refactor in three coupled moves:

1. **Demote the existing three bin crates to lib-only.** `borg/`, `cortex/`, `oracle/` keep their crate names and source layout. Their `Cargo.toml` drops the `[[bin]]` table and keeps only the `[lib]` target. Their existing `main.rs` content is preserved as a record of what each subsystem's CLI did, then gets re-homed in step 2.
2. **Add a new `sb` bin crate** at the workspace root. It depends on `borg`, `cortex`, `oracle`, `vault`, `distillers` (all by `path =`). Its `main.rs` is a thin shell: parse args via clap, init the logger, dispatch to lib functions, map errors to exit codes. Every existing CLI verb gets a lib function in its owning subsystem crate that takes a typed `Config + Opts` and returns `Result<Output>`. The bin crate's job is to call those.
3. **Ship root-level cross-cutting commands** as new code inside the `sb` crate. `sb status`, `sb doctor`, `sb bootstrap` are not subsystem-owned; they read across all three subsystems and live as `sb/src/cli/status.rs`, `sb/src/cli/doctor.rs`, `sb/src/cli/bootstrap.rs` alongside the per-subsystem CLI files.

The two long-running daemons keep running as separate processes. Systemd unit files (`borg.service`, `cortex.service`) are written by `sb borg daemon --install` and `sb cortex daemon --install` into `~/.config/systemd/user/`; their `ExecStart` lines invoke the unified bin (`{current_exe} borg daemon --start` / `{current_exe} cortex daemon --start`). Two systemd units, two PIDs, two memory budgets, one binary on disk. Crash isolation is unchanged from today.

The MCP server is invoked by Claude Code via `.mcp.json` pointing at `/home/saidler/.cargo/bin/sb` with args `["oracle", "serve"]`. The `sb` binary's logger initializer notices the `oracle serve` subcommand and sets up tracing-to-stderr-only, preserving stdout for JSON-RPC.

### Architecture

```
second-brain/
  Cargo.toml                         workspace manifest
  sb/                                NEW - the only bin crate in the workspace
    Cargo.toml                       depends on: borg, cortex, oracle, vault, distillers, clap, eyre, env_logger, tracing-subscriber
    src/
      main.rs                        thin shell: Cli::parse -> logger::init -> Cmd::run
      logger.rs                      pick env_logger vs tracing-stderr based on parsed subcommand
      cli.rs                         module entry (2018-style): top-level Cli + Cmd enum
      cli/                           ALL clap/CLI plumbing namespaced here
        borg.rs                      BorgCmd enum + impl BorgCmd::run -> calls borg lib crate
        cortex.rs                    CortexCmd enum + impl CortexCmd::run -> calls cortex lib crate
        oracle.rs                    OracleCmd enum + impl OracleCmd::run -> calls oracle lib crate
        status.rs                    sb status clap + dispatch (reads across all three libs)
        doctor.rs                    sb doctor clap + dispatch
        bootstrap.rs                 sb bootstrap clap + dispatch

  borg/                              lib-only (was bin+lib)
    Cargo.toml                       no rusqlite dep; was: had [[bin]] borg
    src/
      lib.rs                         pub fn ingest_url(...), run_audit(...), run_dlq_list(...), run_daemon(...), etc.
      opts.rs                        was cli.rs; clap derives stripped; pure-Rust *Opts consumed by lib internals
      pipeline.rs, github.rs, ...    unchanged

  cortex/                            lib-only (was bin+lib)
    Cargo.toml                       rusqlite dep stays
    src/
      lib.rs                         pub fn run_classify(...), run_embed(...), run_daemon(...), etc.
      opts.rs                        was cli.rs; clap derives stripped; absorbs ClassifyOpts from classify.rs
      embed.rs, scope.rs, ...        unchanged

  oracle/                            lib-only (was bin+lib)
    Cargo.toml                       rusqlite dep stays
    src/
      lib.rs                         pub fn serve_mcp(...), run_index(...), run_stats(...), run_call(...)
      server.rs, tools.rs, ...       unchanged

  vault/                             unchanged - shared lib
  distillers/                        unchanged - shared lib
  config/                            unchanged - shared canonical YAMLs
  config/templates/                  NEW - starter configs embedded into sb via include_str!,
                                     dropped into ~/.config/{borg,obsidian-cortex,oracle}/ by `sb bootstrap`
```

Systemd unit files are NOT shipped as static files in the repo. They are
written by `sb borg daemon --install` and `sb cortex daemon --install` into
`~/.config/systemd/user/{borg,cortex}.service` (and the cortex daily/weekly
intel timer units). Source of truth for unit content lives in
`borg::install_systemd` (`borg/src/lib.rs`) and `cortex::install_systemd_service`
(`cortex/src/daemon.rs`); both consult `current_exe()` so the resulting
`ExecStart` always points at the freshly-installed `sb` binary.

The `sb/src/main.rs` is the only `main.rs` in the workspace and is structured per the project's `rust-cli-coder` Shell/Core rule: thin shell, no business logic. Sketch:

```rust
use clap::Parser;
use eyre::Result;
use sb::{cli::Cli, logger};

fn main() -> Result<()> {
    let cli = Cli::parse();
    logger::init_for(&cli)?;
    cli.cmd.run()
}
```

Three lines of substance. Dispatch is **per-subsystem**, not centralized - cargo/kubectl/git all do this. Layout:

- `sb/src/cli.rs` - top-level `Cli` and `Cmd` enum only:
  ```rust
  enum Cmd {
      Borg(BorgCmd),
      Cortex(CortexCmd),
      Oracle(OracleCmd),
      Status(StatusArgs),
      Doctor(DoctorArgs),
      Bootstrap(BootstrapArgs),
  }
  impl Cmd {
      fn run(self) -> Result<()> {
          match self {
              Cmd::Borg(c) => c.run(),
              Cmd::Cortex(c) => c.run(),
              Cmd::Oracle(c) => c.run(),
              Cmd::Status(a) => status::run(a),
              Cmd::Doctor(a) => doctor::run(a),
              Cmd::Bootstrap(a) => bootstrap::run(a),
          }
      }
  }
  ```
- `sb/src/cli/borg.rs` - defines `BorgCmd` (clap enum of `Ingest`, `Note`, `Dlq`, `Intake`, `Audit`, `Daemon`, etc.) and `impl BorgCmd { fn run(self) -> Result<()> { ... } }`. This file is the ONLY thing in `sb` that knows about the borg lib crate's API. Same pattern for `cli/cortex.rs` and `cli/oracle.rs`.
- `sb/src/cli/{status,doctor,bootstrap}.rs` - each exposes one `pub fn run(args: ...) -> Result<()>`. These are the only files that touch multiple subsystem lib crates.
- `sb/src/logger.rs` - per-subcommand logger initializer.

The per-subsystem lib crates (`borg`, `cortex`, `oracle`) are where the actual work happens. The `sb` files are pure clap-to-lib glue.

#### Current → proposed map

Every meaningful path in the workspace today and where it lands after the refactor. "Unchanged" means the path stays exactly where it is; "moves" or "splits" calls out the structural delta.

**Workspace top level**

| Current path | Proposed path | Note |
|---|---|---|
| `Cargo.toml` | `Cargo.toml` | Adds `sb` to `[workspace.members]`; adds `[workspace.default-members = ["sb"]]`. |
| `borg/` | `borg/` | Stays; demoted to lib-only (see per-crate rows). |
| `cortex/` | `cortex/` | Stays; demoted to lib-only. |
| `oracle/` | `oracle/` | Stays; demoted to lib-only. |
| `vault/` | `vault/` | Unchanged. |
| `distillers/` | `distillers/` | Unchanged. |
| `config/` | `config/` | Unchanged (canonical YAMLs). |
| `config/` (no templates today) | `config/templates/{borg,cortex,oracle}.yml.example` | NEW: per-subsystem config exemplars used by `sb bootstrap`. |
| `bin/gen-bge-reference.py` | `bin/gen-bge-reference.py` | Unchanged. |
| `docs/`, `README.md`, `LICENSE`, `clippy.toml`, `.otto.yml`, `.mcp.json` | same | Unchanged on disk; `.mcp.json` content edits to point at `sb`; `.otto.yml` `deploy` task restarts borg/cortex daemons (unit files are owned by `daemon --install`, not the deploy task). |
| *(no `sb/` today)* | `sb/` | NEW bin crate; sole `[[bin]]` in the workspace. |
| systemd units (not in repo) | still not in repo; written by `sb borg/cortex daemon --install` | Source of truth for unit content lives in `borg::install_systemd` and `cortex::install_systemd_service`. |

**The new `sb` crate**

| Proposed path | Origin |
|---|---|
| `sb/Cargo.toml` | NEW. Path deps on `borg`, `cortex`, `oracle`, `vault`, `distillers` + `clap`, `eyre`, `env_logger`, `tracing-subscriber`. |
| `sb/src/main.rs` | NEW. Three-line shell (`Cli::parse` → `logger::init_for` → `cli.cmd.run()`). |
| `sb/src/logger.rs` | NEW. Per-subcommand init: `tracing-subscriber` stderr-only for `oracle serve`, `env_logger` to file for everything else. Replaces the three separate `logging.rs` initializers that today live in each bin's `main.rs`. |
| `sb/src/cli.rs` | NEW. Top-level `Cli` Parser + `Cmd` enum + `Cmd::run` dispatch. |
| `sb/src/cli/borg.rs` | Clap derive structs extracted from `borg/src/main.rs` + `borg/src/cli.rs`. Defines `BorgCmd` enum + `impl BorgCmd::run`. |
| `sb/src/cli/cortex.rs` | Clap derive structs extracted from `cortex/src/main.rs` + `cortex/src/cli.rs`. |
| `sb/src/cli/oracle.rs` | Clap derive structs extracted from `oracle/src/main.rs` + `oracle/src/cli.rs`. |
| `sb/src/cli/status.rs` | NEW. Reads across all three lib crates. |
| `sb/src/cli/doctor.rs` | NEW. Same checks as `status`, each tagged with `Finding { severity, message, suggested_fix }`. |
| `sb/src/cli/bootstrap.rs` | NEW. Drops `config/templates/*.yml.example` into `~/.config/sb/`, registers shipped systemd units, calls `cortex::prefetch_embedding_model`. |

**`borg/` crate**

| Current path | Proposed path | Note |
|---|---|---|
| `borg/Cargo.toml` | unchanged structurally; `src/main.rs` deletion demotes to lib-only | borg's `Cargo.toml` has no explicit `[[bin]]` table — cargo auto-discovers `src/main.rs` as a bin. Removing the file (Phase 1) demotes the crate to lib-only automatically. No manifest edit required. |
| `borg/src/main.rs` | deleted; logic split | Clap derives → `sb/src/cli/borg.rs`. Subcommand handler bodies → `pub fn` per-verb in `borg/src/lib.rs`. Logger init → `sb/src/logger.rs`. |
| `borg/src/cli.rs` | split: clap layer → `sb/src/cli/borg.rs`; pure Opts → `borg/src/opts.rs` (rename) | The `Command` enum's inline-struct variants (`Ingest` line 37, `Note` line 54, `Migrate` line 69, `Audit` line 80, `Reingest` line 98) get extracted into standalone `pub struct IngestOpts/NoteOpts/MigrateOpts/AuditOpts/ReingestOpts`. `HotkeyOpts` (line 279) and `DaemonOpts` (line 302) are already standalone and move as-is. All Opts get their `#[derive(Parser/Subcommand/Args)]` and clap attributes stripped. Lib-internal callers (`borg/src/lib.rs:768,784`, `borg/src/lib.rs:785`) update `use crate::cli::*` → `use crate::opts::*`. New clap-derived `*Args` structs live in `sb/src/cli/borg.rs` with `impl From<*Args> for borg::opts::*Opts` in that same file (sb owns the translation; lib cannot depend on sb). |
| `borg/src/lib.rs` | `borg/src/lib.rs` | Stays; gains one `pub fn` per existing CLI verb (`ingest_url`, `run_audit`, `run_dlq_list`, `run_daemon`, …) wrapping today's `main.rs` handler bodies. Returns typed `Result<T>`, never `println!`s. |
| `borg/src/logging.rs` | deleted | Subsumed by `sb/src/logger.rs`. Verified safe: only `borg/src/main.rs` imports it. |
| `borg/src/{pipeline,github,backfill,blocklist,replay,retention,slides,stages,startup,transcription,triage,watchdog}/` (subdirectories) and their `.rs` peers (`pipeline.rs`, `github.rs`, `backfill.rs`, `blocklist.rs`, `replay.rs`, `retention.rs`, `slides.rs`, `stages.rs`, `startup.rs`, `transcription.rs`, `triage.rs`, `watchdog.rs` — 2018-style module entries), plus flat modules (`assets`, `audit`, `backoff`, `config`, `dashboard`, `description`, `discord`, `error`, `extraction`, `fabric`, `health`, `hygiene`, `intake`, `jina`, `ledger`, `markdown`, `migrate`, `notify`, `ntfy`, `ocr`, `quality`, `router`, `routes`, `telegram`, `trace`, `types`, `youtube`) | Unchanged | Pipeline/IO modules stay put. |
| `borg/patterns/*.md` | `borg/patterns/*.md` | Unchanged; install path under `~/.config/borg/patterns/` also unchanged (sync logic in `otto deploy` stays). |

**`cortex/` crate**

| Current path | Proposed path | Note |
|---|---|---|
| `cortex/Cargo.toml` | unchanged structurally; `src/main.rs` deletion demotes to lib-only | Same as borg — no explicit `[[bin]]` table today; bin lives via cargo's `src/main.rs` auto-discovery. |
| `cortex/src/main.rs` | deleted; logic split | Clap derives → `sb/src/cli/cortex.rs`. Handler bodies → `pub fn` in `cortex/src/lib.rs`. |
| `cortex/src/cli.rs` | split: clap layer → `sb/src/cli/cortex.rs`; pure Opts → `cortex/src/opts.rs` (rename) | The 9 existing `*Opts` structs (`LintOpts` line 88, `LinkOpts` line 107, `IntelOpts` line 118, `StateOpts` line 133, `DaemonOpts` line 144, `MigrateOpts` line 167, `SweepOpts` line 178, `EmbedOpts` line 200, `SummarizeOpts` line 236) keep their fields; their `#[derive(Parser/Subcommand/Args)]` and clap attributes are stripped. `ClassifyOpts` (currently at `cortex/src/classify.rs:220`) is moved into `opts.rs` so the file is a complete 10-struct manifest of the subsystem's input types. Lib-internal callers update `use crate::cli::*` → `use crate::opts::*`: `cortex/src/lib.rs:32`, `cortex/src/daemon.rs` (9 sites: Daemon/Intel/Lint/Link/State Opts construction at lines 10, 185, 204, 336, 377, 383, 400, 486, 496), `cortex/src/embed.rs:44`, `cortex/src/summarize.rs:27`, `cortex/src/intel.rs:5`, `cortex/src/summarize/tests.rs:2`. New clap-derived `*Args` structs live in `sb/src/cli/cortex.rs` with `impl From<*Args> for cortex::opts::*Opts` in that same file. |
| `cortex/src/lib.rs` | `cortex/src/lib.rs` | Gains `pub fn` per verb (`run_classify`, `run_lint`, `run_link`, `run_intel`, `run_state`, `run_migrate`, `run_sweep`, `run_summarize`, `run_embed`, `run_daemon`, `prefetch_embedding_model`). |
| `cortex/src/logging.rs` | deleted | Subsumed by `sb/src/logger.rs`. Verified safe: only `cortex/src/main.rs` imports it. |
| `cortex/src/classify.rs` | `cortex/src/classify.rs` (minus `ClassifyOpts`) | `ClassifyOpts` definition (line 220) extracted to `cortex/src/opts.rs`; `classify.rs` becomes a `use crate::opts::ClassifyOpts` consumer. Rest of the file unchanged. |
| `cortex/src/{embed,summarize,sweep}/` (subdirectories) and their `.rs` peers (`embed.rs`, `summarize.rs`, `sweep.rs` — 2018-style module entries), plus flat modules (`autotag`, `config`, `daemon`, `duplicates`, `fabric`, `frontmatter`, `intel`, `linking`, `links`, `llm`, `migrate`, `naming`, `quality`, `report`, `scope`, `state`, `tags`, `testutil`, `vault`) | Unchanged | The `.rs` files import `crate::opts::*` instead of `crate::cli::*`, but their bodies are unchanged. |

**`oracle/` crate**

| Current path | Proposed path | Note |
|---|---|---|
| `oracle/Cargo.toml` `[[bin]] oracle` table (line 14) | removed | Unlike borg/cortex, oracle has an explicit `[[bin]]` table. Both the table AND `src/main.rs` are removed in Phase 1 to demote the crate to lib-only. |
| `oracle/src/main.rs` | deleted; logic split | Clap derives → `sb/src/cli/oracle.rs`. Handler bodies → `pub fn` in `oracle/src/lib.rs`. Logger init (currently inline in `main.rs`) → `sb/src/logger.rs`. |
| `oracle/src/cli.rs` | merged wholesale into `sb/src/cli/oracle.rs` | Unlike borg/cortex, oracle has no lib-internal coupling to its own `cli.rs` — only `oracle/src/main.rs` imports from it. The whole file moves; no opts.rs needed. |
| `oracle/src/lib.rs` | `oracle/src/lib.rs` | Gains `pub fn serve_mcp(...)`, `run_index(...)`, `run_stats(...)`, `run_call(...)`. |
| `oracle/src/{server,tools,config}.rs` | Unchanged | Search functionality lives in `vault::search`, not in oracle (oracle is a thin MCP consumer of `vault::search::SearchIndex`). |

**Shared crates (no changes)**

| Path | Note |
|---|---|
| `vault/src/**` | Unchanged. `vault::search` keeps its `search` feature gate; `borg` continues to depend on `vault` without that feature, with the grep-lint backstop for the cargo-feature-unification case described below. |
| `distillers/src/**` | Unchanged. |
| `config/{canonical-tags,tag-mapping,tag-proposals}.yml` | Unchanged. |

**Install + runtime artifacts (outside the repo)**

| Current | Proposed | Note |
|---|---|---|
| `~/.cargo/bin/{borg,cortex,oracle}` | `~/.cargo/bin/sb` (the old three are `rkvr rmrf`-ed in Phase 4) | One binary on disk. |
| `~/.config/borg/borg.yml`, `~/.config/obsidian-cortex/obsidian-cortex.yml`, `~/.config/oracle/oracle.yml` | unchanged paths | Each subsystem lib still loads its own config from its own dir. `sb bootstrap` populates these from `config/templates/*.yml.example` if absent. |
| `~/.config/borg/patterns/*.md` | unchanged | |
| `~/.config/second-brain/{canonical-tags,tag-mapping,tag-proposals}.yml` | unchanged | |
| `~/.config/systemd/user/borg.service`, `cortex.service` | rewritten in place by `sb borg daemon --install` / `sb cortex daemon --install` (template lives in `borg::install_systemd` / `cortex::install_systemd_service`, ExecStart=`{current_exe} borg daemon --start`) | Same unit names; same PIDs/restart budgets. |
| `~/.config/systemd/user/borg.service.d/*.conf` | n/a after this refactor | The new in-binary install writes the full unit content (secrets ExecStartPre, EnvironmentFile, PATH, hardening) so per-machine drop-ins are no longer needed on the borg side. If a drop-in exists from before, systemd still merges it on top — but the base unit alone is now sufficient. |
| `.mcp.json` `command: oracle, args: ["serve"]` | `command: sb`, `args: ["oracle", "serve"]` | One-line edit (paired with sb install). |
| `~/.local/share/{borg,cortex,oracle}/logs/*.log` | `~/.local/share/sb/{borg,cortex,oracle,status,doctor,bootstrap}.log` | One shared directory; one file per subsystem or root verb. See "Logger discipline". |

#### Concrete extraction example: `borg ingest`

Today's `borg/src/main.rs` has something like (paraphrased):

```rust
Commands::Ingest { url, force, .. } => {
    let config = load_config(&cli.config)?;
    let trace_id = trace::generate(IngestMethod::Cli);
    let outcome = pipeline::ingest_url(&url, &config, &trace_id, force).await?;
    println!("{}", outcome.summary());
}
```

After refactor, `borg/src/lib.rs` exposes the lib function:

```rust
pub async fn ingest_url(
    config: &BorgConfig,
    opts: IngestOpts,
) -> Result<IngestOutcome> {
    let trace_id = trace::generate(IngestMethod::Cli);
    pipeline::ingest_url(&opts.url, config, &trace_id, opts.force).await
}
```

And `sb/src/cli/borg.rs` calls it (per-subsystem dispatch, not a centralized match):

```rust
impl BorgCmd {
    pub async fn run(self) -> Result<()> {
        let config = borg::Config::load()?;
        match self {
            BorgCmd::Ingest { url, force } => {
                let outcome = borg::ingest_url(&config, IngestOpts { url, force }).await?;
                println!("{}", outcome.summary());
                Ok(())
            }
            // ... one arm per BorgCmd variant
        }
    }
}
```

The `println!` (and any other I/O) lives in `sb/src/cli/borg.rs`. The lib returns typed data. The same pattern applies to `cli/cortex.rs`, `cli/oracle.rs` for their respective subsystems.

### What we lose, and what replaces it

The current Cargo-level enforcement of "borg cannot reach SQLite" depends on a happy accident: today the three binaries compile as three independent dep trees, so borg's manifest (which declares `vault = { path = "../vault" }` *without* the `search` feature) actually produces a `vault` library that doesn't contain `rusqlite` types. Under one bin crate, cargo's feature unification compiles `vault` ONCE with the union of features any dependent wants. Since `cortex` and `oracle` both pull in `vault` with `features = ["search"]`, the unified `vault` artifact has `search` enabled, and `borg`'s code can `use vault::search::*` and compile. The compile-time wall comes down.

We accept this and replace it with two thinner walls:

1. **Lint-level enforcement.** Add a check to `otto ci`'s lint task that greps `borg/src/` for `rusqlite` imports and for `vault::search` usages. Either presence is a CI failure. This is mechanically reliable and catches the "borg developer accidentally reaches for SQLite" case as well as the cargo invariant ever did.
2. **Architectural documentation.** A `docs/architecture.md` (or equivalent) explicitly states: "borg writes only to the filesystem. Oracle owns SQLite. Borg's lib must not import rusqlite or vault::search." This is the convention layer that the lint enforces.

The compile-time mechanism was the strongest form of enforcement. The grep-based lint is weaker but still effective: a violation is a one-line PR comment, not a runtime bug. Net: we trade a structurally-guaranteed invariant for a CI-enforced one. Acceptable cost for the unified-CLI win.

### Data Model

No data-model changes. The vault crate's schema, frontmatter, ledger, intake, dlq, and distilled types are unchanged. The three subsystem lib crates expose typed `Config` and per-command `Opts` structs the same way they do today; they just become library-public instead of bin-private.

### API Design

#### CLI surface

The complete CLI tree mirrors the existing three CLIs prefixed with their subsystem, plus three new root commands. See [the full subcommand inventory in the conversation that produced this doc] - reproduced here in condensed form:

```
sb borg ingest <url>
sb borg note <text>
sb borg hotkey {install,uninstall}
sb borg sign
sb borg migrate
sb borg audit [--invariant]
sb borg intake {list,show}
sb borg dlq {list,show,archive,replay}
sb borg reingest
sb borg reingest-failed
sb borg replay
sb borg retention
sb borg blocklist {list,remove,clear}
sb borg backfill-ingested
sb borg dashboard
sb borg daemon --start

sb cortex classify
sb cortex lint
sb cortex link
sb cortex intel
sb cortex state
sb cortex migrate
sb cortex sweep
sb cortex summarize [--backfill]
sb cortex embed [--backfill | --prefetch-model]
sb cortex daemon --start

sb oracle serve
sb oracle index
sb oracle stats
sb oracle call <tool> [args...]

sb status
sb doctor
sb bootstrap
```

#### Lib API surface

Each existing crate's `lib.rs` exposes one public function per subcommand. The signature pattern:

```rust
// borg/src/lib.rs (sketch)
pub fn ingest_url(config: &BorgConfig, opts: IngestOpts) -> Result<IngestOutcome>;
pub fn run_audit(config: &BorgConfig, opts: AuditOpts) -> Result<AuditReport>;
pub fn run_dlq_list(config: &BorgConfig, opts: DlqListOpts) -> Result<Vec<DlqRow>>;
pub fn run_daemon(config: &BorgConfig) -> Result<()>;  // long-running
// ... one per subcommand
```

Each subsystem CLI file (`sb/src/cli/borg.rs`, etc.) owns its own clap derives AND its dispatch. The top-level `cli.rs` just routes by `Cmd` variant to the right subsystem's `run()` method.

Existing `main.rs` content from each crate gets translated into:
- Its clap derive structs move into `sb/src/cli/<subsystem>.rs` as the subsystem's `Cmd` enum.
- Its dispatch logic moves into the same file as `impl <Subsystem>Cmd::run`.
- Its actual work moves into the lib crate as a public function returning typed data.

#### Args vs. Opts: two-layer split (borg, cortex only)

`borg/src/cli.rs` and `cortex/src/cli.rs` cannot move wholesale to `sb/` because their `*Opts` types are imported by lib-internal modules (verified: `borg/src/lib.rs:768,784`; `cortex/src/lib.rs:32`; `cortex/src/daemon.rs` at 9 construction sites; `cortex/src/{embed,summarize,intel}.rs`). The lib needs those types to remain inside its own crate. The clap derives, however, drag a `clap` dependency through the lib and tie its public surface to a CLI parser.

Resolution — a two-layer naming convention:

| Layer | Type name | Location | What it is |
|---|---|---|---|
| Lib API (callable, pure Rust) | `<Verb>Opts` | `<subsystem>/src/opts.rs` | Plain `pub struct` with the same fields the current `*Opts` types have today, *minus* `#[derive(Parser/Subcommand/Args)]` and `#[arg(...)]`/`#[command(...)]` attributes. Consumed by lib internals and by the lib's `pub fn`s. |
| CLI parse target (clap-derived) | `<Verb>Args` | `sb/src/cli/<subsystem>.rs` | clap-derived struct that mirrors the lib's `*Opts` fields with the clap attributes attached. Built by `Cli::parse()`. |
| Translation | `impl From<<Verb>Args> for <subsystem>::opts::<Verb>Opts` | `sb/src/cli/<subsystem>.rs` | Lives in `sb` because the lib cannot depend on `sb` (wrong direction in the dep DAG). Trivial field-by-field move. |

Concrete sketch:

```rust
// cortex/src/opts.rs (renamed from cli.rs; clap derives stripped)
pub struct EmbedOpts {
    pub backfill: bool,
    pub prefetch_model: bool,
    pub max_batch: Option<usize>,
}

// sb/src/cli/cortex.rs (new)
#[derive(clap::Args)]
pub struct EmbedArgs {
    #[arg(long)]            pub backfill: bool,
    #[arg(long)]            pub prefetch_model: bool,
    #[arg(long)]            pub max_batch: Option<usize>,
}
impl From<EmbedArgs> for cortex::opts::EmbedOpts {
    fn from(a: EmbedArgs) -> Self {
        Self { backfill: a.backfill, prefetch_model: a.prefetch_model, max_batch: a.max_batch }
    }
}
```

This keeps the lib free of `clap`, keeps the lib's existing internal API names (`*Opts`) unchanged so the call sites in `daemon.rs` etc. only need a one-token import update, and makes the clap-vs-lib boundary self-documenting via the `*Args`/`*Opts` distinction.

**Nested subcommand enums (borg-specific).** `borg/src/cli.rs` contains five nested `clap::Subcommand`-derived enums in addition to the `*Opts` structs: `DashboardAction` (line 152), `IntakeAction` (line 165), `DlqAction` (line 189), `BlocklistAction` (line 228), `RetentionAction` (line 268). These follow the same two-layer treatment, but the `From` impls for them are not pure field moves — they are `match` blocks mapping `*ActionArgs` variants (clap, in `sb/src/cli/borg.rs`) onto `*Action` variants (pure Rust, in `borg/src/opts.rs`). Sketch:

```rust
// borg/src/opts.rs
pub enum DlqAction {
    List(DlqListOpts),
    Show(DlqShowOpts),
    Archive(DlqArchiveOpts),
    Replay(DlqReplayOpts),
}

// sb/src/cli/borg.rs
#[derive(clap::Subcommand)]
pub enum DlqActionArgs {
    List(DlqListArgs),
    Show(DlqShowArgs),
    Archive(DlqArchiveArgs),
    Replay(DlqReplayArgs),
}
impl From<DlqActionArgs> for borg::opts::DlqAction {
    fn from(a: DlqActionArgs) -> Self {
        match a {
            DlqActionArgs::List(x) => Self::List(x.into()),
            DlqActionArgs::Show(x) => Self::Show(x.into()),
            DlqActionArgs::Archive(x) => Self::Archive(x.into()),
            DlqActionArgs::Replay(x) => Self::Replay(x.into()),
        }
    }
}
```

Mechanically rote, but worth calling out so Phase 1's per-verb translation work doesn't surprise the implementer with five extra match blocks. Cortex has no nested action enums; its 9 `*Opts` structs are all flat.

Oracle does NOT need this split (no lib-internal coupling to `oracle/src/cli.rs`), so oracle's `cli.rs` moves wholesale to `sb/src/cli/oracle.rs` and the clap-derived structs there can keep using `*Opts` names directly without ambiguity.

#### Logger discipline

`sb/src/logger.rs` examines the parsed `Cli` and picks one of two initialization paths. **All logs land under `~/.local/share/sb/<name>.log`** (one directory, one file per subsystem or root verb):

| Invocation | Path | Writer |
|---|---|---|
| `sb borg <verb>` | `~/.local/share/sb/borg.log` | env_logger |
| `sb cortex <verb>` | `~/.local/share/sb/cortex.log` | env_logger |
| `sb oracle serve` | `~/.local/share/sb/oracle.log` | tracing-subscriber (preserves stdout for MCP JSON-RPC) |
| `sb oracle index/stats/call` | `~/.local/share/sb/oracle.log` | env_logger |
| `sb status` | `~/.local/share/sb/status.log` | env_logger |
| `sb doctor` | `~/.local/share/sb/doctor.log` | env_logger |
| `sb bootstrap` | `~/.local/share/sb/bootstrap.log` | env_logger |

Single parent directory (`sb/`) so `ls ~/.local/share/sb/` is the whole story; per-subsystem leaf files so long-running daemon output doesn't interleave. The two oracle rows share a file path on disk — only the writer library differs.

`vault::logging::setup_logging` was refactored from `(app_name, level)` to `(log_file_path, level)` so the layout lives in `sb/src/logger.rs` and is not baked into the shared lib.

The lib crates keep their `log::debug!` / `log::info!` / `log::warn!` macros unchanged. The bin chooses the implementation that picks those up.

### Implementation Plan

The phases below are organizational; per [[feedback-no-phase-gating]] they ship back-to-back as one coherent change, one bump, one shipit. No soak time between phases.

#### Phase 1: Demote bins to libs, build the `sb` skeleton
**Model:** sonnet

- **Args/Opts split (borg + cortex only).** This must land *before* removing `main.rs`, otherwise lib internals stop compiling. Per crate:
  - `git mv <subsystem>/src/cli.rs <subsystem>/src/opts.rs`.
  - In `opts.rs`: strip every `#[derive(Parser/Subcommand/Args)]` and every `#[arg(...)]`/`#[command(...)]` attribute; keep struct/field definitions intact. Drop any `clap::Subcommand`-only `Command` enum (only kept its variants for their inline payloads — those become standalone `*Opts` structs).
  - **borg specifically:** the `Command` enum's inline-struct variants (`Ingest` line 37, `Note` line 54, `Migrate` line 69, `Audit` line 80, `Reingest` line 98 in the current `cli.rs`) get extracted into standalone `pub struct IngestOpts/NoteOpts/MigrateOpts/AuditOpts/ReingestOpts`. `HotkeyOpts` and `DaemonOpts` are already standalone — they move as-is.
  - **cortex specifically:** move `ClassifyOpts` from `cortex/src/classify.rs:220` into `cortex/src/opts.rs` so `opts.rs` is a complete manifest.
  - Update lib `mod cli;` → `mod opts;` in each `lib.rs`. Update all `use crate::cli::*` → `use crate::opts::*` (one find-replace per crate; sites enumerated in the map: cortex has 14 sites, borg has 4).
  - `cargo check -p borg --lib` and `cargo check -p cortex --lib` must pass after this step with no other changes. The `--lib` flag is required because `src/main.rs` still exists at this step and calls `Cli::parse()` against the old (now clap-less) types; the bin target will not compile until `main.rs` is removed in the next bullet. The `--lib` check verifies that lib internals (the actual contract being changed) still compile against the renamed `opts` module.
- For each of `borg/`, `cortex/`, `oracle/`: edit `Cargo.toml` to remove the `[[bin]]` section and ensure `[lib]` is present (or use the implicit `src/lib.rs` convention). Move `src/main.rs` content into a temporary holding spot or scratch file - it won't compile in the lib crate as-is.
- Create new `sb/` crate at the workspace root. Add to workspace members in `Cargo.toml`. Declare path dependencies on the three lib crates plus `vault`, `distillers`, plus `clap`, `eyre`, `env_logger`, `tracing-subscriber`.
- Write `sb/src/main.rs` as the three-line shell shown above.
- Write `sb/src/cli.rs` (module entry) with the top-level `Cli` and `Cmd` enum that delegates to each subsystem via `Cmd::run`.
- Write `sb/src/cli/borg.rs`, `cli/cortex.rs`, `cli/oracle.rs`. Each defines its subsystem's clap enum AND its `impl run`. For borg/cortex, this is where the clap-derived `*Args` structs live AND where `impl From<*Args> for <subsystem>::opts::*Opts` lives (the translation layer; lib cannot depend on `sb`). For oracle, the clap structs from `oracle/src/cli.rs` move wholesale and can keep their `*Opts` names — no translation needed.
- Write `sb/src/cli/status.rs`, `cli/doctor.rs`, `cli/bootstrap.rs` as stubs that bail with "not yet implemented" (filled in by Phase 2).
- For each lib function that doesn't exist yet (most of them), extract the body of the corresponding old `main.rs` subcommand handler into a `pub fn` in the lib crate. The function takes `&<Subsystem>Config + <Verb>Opts` and returns typed output; never `println!`. Output formatting happens in `sb/src/cli/<subsystem>.rs` after the lib call returns.
- Write `sb/src/logger.rs` with the per-subcommand branch logic described above.

#### Phase 2: Cross-cutting command bodies
**Model:** opus

- Implement `sb/src/cli/status.rs` with a `Report` struct that aggregates: systemd state of borg + cortex (shell out to `systemctl --user show -p ActiveState,MemoryCurrent,MainPID`), config presence and parse status, pattern-sync drift detection (compare `borg/patterns/*.md` mtimes/hashes against `~/.config/borg/patterns/*.md`), shared-config drift, embedding model presence, embedding coverage (call into cortex lib), recent intake/ledger/dlq counts (call into borg lib), oracle vault stats (call into oracle lib).
- Implement `sb/src/cli/doctor.rs`: same checks as status but each returns a `Finding { severity, message, suggested_fix }`. Output is a list of findings sorted by severity.
- Implement `sb/src/cli/bootstrap.rs`: first-time setup. Drops config templates from `config/templates/` into `~/.config/sb/{borg,cortex,oracle}.yml` if missing; registers the shipped systemd units; runs `cortex embed --prefetch-model` to warm the fastembed cache. Idempotent; existing files are not clobbered.

#### Phase 3: Repo-side artifacts
**Model:** sonnet

- Update `borg::install_systemd` and `cortex::install_systemd_service` (the existing in-lib unit writers, reached via `sb borg daemon --install` / `sb cortex daemon --install`) so they write the *correct* unit shape for the unified bin: unit names `borg.service` / `cortex.service` (no `obsidian-` prefix), `ExecStart={current_exe} borg|cortex … daemon --start`, full secrets-decryption ExecStartPre + EnvironmentFile + PATH + hardening for borg; PATH + hardening for cortex. Matching uninstall functions target the same names. Single source of truth for unit content lives in the binary, not in a separate `systemd/*.service` file.
- Create `config/templates/borg.yml.example`, `cortex.yml.example`, `oracle.yml.example` - exemplars with comments explaining each field.
- `otto deploy` (in `.otto.yml`) restarts any borg.service / cortex.service that already exist, but does NOT write or sync unit files. The first-time bootstrap path on a fresh machine is `sb borg daemon --install && sb cortex daemon --install`.

#### Phase 4: Cutover
**Model:** sonnet

- Update `.mcp.json` to point at `sb` with `args: ["oracle", "serve"]` (paired with `cargo install --path sb` so the bin is on PATH when Claude Code launches the MCP).
- `cargo install --path sb` (or `otto deploy`, which also restarts any existing borg/cortex daemons).
- Rewrite the systemd units: `sb borg daemon --install && sb cortex daemon --install`. These regenerate `~/.config/systemd/user/{borg,cortex}.service` (plus cortex's daily/weekly intel timers) with `ExecStart` pointing at the new `sb` binary, run `daemon-reload`, and `enable --now` the units.
- `rkvr rmrf ~/.cargo/bin/borg ~/.cargo/bin/cortex ~/.cargo/bin/oracle` after the new `sb` install lands. (`rkvr` archives these for recovery; per the safety rule we don't `rm`.)
- Update `CLAUDE.md`: replace the three-binary Install section with the single-binary version. Update the "Binary names" line in the architecture overview.
- Update memories that name the three binaries.
- `otto ci`, `bump`, `git push && git push --tags`, `otto deploy`.
- Smoke-test: `sb status` reports both daemons active on the new binary, MCP query through Claude Code returns results, fresh `sb borg ingest <url>` produces a note end-to-end.

## Alternatives Considered

### Alternative 1: Keep three bins, add a fourth `sb` for cross-cutting commands only

- **Description:** Don't refactor. Keep `borg`, `cortex`, `oracle` as three separate binaries with their existing CLIs. Add a new `sb` binary whose only job is `sb status`, `sb doctor`, `sb bootstrap`. The new binary shells out to the three existing ones for data.
- **Pros:** Zero breakage of muscle memory, scripts, systemd units, `.mcp.json`. Lower-risk refactor.
- **Cons:** This is the alternative Scott explicitly rejected. It preserves the federation; it accepts the three-CLI cognitive load; the version-drift vector between the three binaries remains; cross-cutting commands have to shell out and parse text instead of calling library functions; the four-binary footprint is *worse* than three.
- **Why not chosen:** Rejected by the primary user as not solving the "disjointed and incoherent" problem.

### Alternative 2: Single binary AND single process

- **Description:** Collapse to one binary AND run both daemons in one process - one thread for borg's ingestion loop, one for cortex's sweep loop, share memory and a single tokio runtime.
- **Pros:** Even simpler operational story.
- **Cons:** A memory leak in cortex's embed loop kills borg's ingestion (the 2026-05-19 cortex OOM would have taken everything down). Two separate restart budgets become one. Tokio runtime contention between an LLM-bound workload and a CPU-bound embedding workload. No way to upgrade cortex without dropping ingestion mid-flight.
- **Why not chosen:** Process isolation between the daemons is a real architectural property worth keeping. The cost of two systemd units calling one binary is essentially zero; the benefit (independent crash budgets, independent memory accounting, independent restart timing) is large.

### Alternative 3: Flat root subcommands (`sb ingest`, `sb sweep`, etc.)

- **Description:** Drop the borg/cortex/oracle layer. Every leaf verb lives at the root: `sb ingest`, `sb sweep`, `sb embed`, `sb classify`, `sb dlq list`, etc. git/cargo-style.
- **Pros:** Shortest commands possible. One less word to type. Matches the conventions of git and cargo most closely.
- **Cons:** Two verb collisions today (`borg migrate` and `cortex migrate` both exist), and more will appear over time. The subsystem context is meaningful documentation - knowing that "this verb belongs to ingestion" or "this verb belongs to vault governance" tells the user something real about what the command does. Loses the kubectl/aws affordance of `sb borg --help` showing exactly the ingestion-subsystem surface.
- **Why not chosen:** Scott chose `sb <subsystem> <verb>` explicitly during the design conversation. The subsystem names are real conceptual divisions worth preserving in the CLI.

## Technical Considerations

### Dependencies

No new external dependencies. `clap`, `eyre`, `env_logger`, `tracing-subscriber` are already in the workspace (one or more of the three bins uses each today). The `sb` crate pulls all of them in once.

**Update (2026-05-19, post-cutover):** `sb`'s `Cargo.toml` ended up depending on `rmcp = "1.3.0"` as well. This was added during Phase C of the cutover so `sb/src/cli/oracle.rs` can format the typed return of `oracle::call` (`rmcp::model::CallToolResult`) and the typed return of `oracle::tools` (`Vec<rmcp::model::Tool>`). `rmcp` was already an oracle dep; this just lifts it into `sb` so the CLI shell can pattern-match on its types. `serde_yaml` was added later (Phase 5 of `lib-api-cleanup`) so `sb/src/cli/checks.rs` can run a parse-status check on each subsystem's config file. See `docs/design/2026-05-19-lib-api-cleanup.md` for the full lib-API contract these deps support.

### Performance

`sb borg ingest <url>` has the same runtime profile as today's `borg ingest <url>`: arg parsing, config load, pipeline invocation. The added dispatch layer is one `match` on a clap enum, microseconds.

Compile time: the `sb` bin links the three lib crates plus vault plus distillers. The compiled binary size grows compared to any individual current binary, but is smaller than the sum of all three because the shared crates are linked once. The release-mode binary is plausibly 1.5-2x the size of `borg` today, well under any limit anyone cares about.

Daemon startup time: unchanged - clap dispatch is dwarfed by daemon initialization (config load, vault scan, embedding-model load for cortex).

### Security

No new attack surface. The `sb` binary inherits the same trust boundaries as the three binaries it replaces. Systemd unit hardening directives (`NoNewPrivileges`, `ProtectSystem`, `ProtectHome`, `ReadWritePaths`, `PrivateTmp`) carry over unchanged in the shipped unit files.

The borg-doesn't-write-SQLite invariant is preserved at the lib-crate Cargo.toml level: `borg/Cargo.toml` has no `rusqlite` dependency. The `sb` bin transitively links rusqlite through `cortex` and `oracle`, but borg's code path cannot reach those types because borg's manifest doesn't pull them in. Compile-time enforcement is preserved by drawing the boundary one layer lower.

### Testing Strategy

- Existing unit tests in each subsystem crate continue to work unchanged; they ran against the library code, not the bin code, and the library code is unchanged.
- New: integration test in the `sb` crate that exercises `Cli::parse()` against a representative input for each subcommand and asserts the resulting enum shape. This catches clap-tree regressions.
- New: integration test for the logger dispatch - parse a `sb oracle serve` invocation, assert the logger init function returns a tracing-subscriber configured for stderr-only.
- Smoke tests in `otto ci` for the bin: `sb --help`, `sb borg --help`, `sb cortex --help`, `sb oracle --help`, `sb status` (against an empty fake-vault tempdir).
- Manual: cutover validation per Phase 4.

### Rollout Plan

Single coherent change, shipped per `feedback-no-phase-gating`:

1. Phases 1-4 land as a sequence of commits on `main` (or one squashed commit if it's small enough; large refactor probably wants two or three commits for reviewability even though they ship together).
2. `otto ci` green throughout.
3. `bump -m` (this is a minor: breaking change for anything that scripted the old binary names, even though we're the only consumer).
4. `git push && git push --tags`.
5. `otto deploy` installs `sb`, drops the new systemd units, restarts daemons under the new binary.
6. `rkvr rmrf` the old binaries.
7. Smoke-test end-to-end.

If anything in the smoke test fails, the rollback path is: `git revert` the merge commit, `bump`, `otto deploy`, restore the old binaries from rkvr archive. No data is at risk because no on-disk format changes.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Library extraction surfaces hidden coupling between subsystem `main.rs` files and modules they shouldn't have used | Medium | Med | The extraction is mechanical: each `main.rs` subcommand handler becomes a `pub fn` with explicit args and return type. The compiler catches anything that was secretly reaching for a sibling-module private. `otto ci` after each subsystem's extraction. |
| MCP stdio purity breaks because some lib's `log::info!` ends up going to stdout in the `oracle serve` path | Medium | High | Logger init in `sb/src/logger.rs` matches on the parsed subcommand BEFORE any lib code runs. For `oracle serve`, tracing-subscriber with stderr-only writer; no env_logger. Add a smoke test that runs `sb oracle serve` for 100ms and asserts stdout contains no log lines. |
| `bump`'s workspace-version handling miscounts a lib-only crate | Low | Low | All crates already share `version.workspace = true` (verify in the refactor). bump increments the workspace version once; all crates pick it up. |
| Old binaries linger in `~/.cargo/bin/` after install because no step removes them | Low | Low | Phase 4 explicitly `rkvr rmrf`s them. If a user (or me) forgets, the old binaries are stale but harmless - they just won't see new code. Document in CLAUDE.md. |
| Systemd refuses to load shipped units due to placeholder paths needing per-machine substitution | Low | Med | Use `%h` (systemd's home-directory expansion) instead of hardcoded `/home/saidler/` in the shipped units: `ExecStart=%h/.cargo/bin/sb borg daemon --start`. systemd substitutes at unit-load time. |
| `.mcp.json` edit breaks Claude Code's connection to oracle | Low | Med | The edit is one-line: change `"command"` and `"args"`. Test in Claude Code immediately after the edit; fall back to old binary path if needed (still present until rkvr step). |
| Refactor takes longer than expected and intermediate states are broken on `main` | Medium | Low | Per the rollout plan, intermediate Phase-1 commits can leave the workspace in a state where individual subsystem CLIs no longer work as `borg`/`cortex`/`oracle` (because there's no bin) but the `sb` CLI doesn't yet work either. Mitigate by doing Phase 1 in a single commit that crosses the cutover atomically, or by keeping the old bins in their own `Cargo.toml` until the `sb` bin can subsume them entirely (longer but safer). |
| Cargo feature unification puts `rusqlite` into the unified `vault` artifact, so borg's lib code can now reach SQLite types via `vault::search` (previously impossible because borg's separate dep tree compiled `vault` without `search`) | Certain | Med | Replace compile-time enforcement with a CI lint that greps `borg/src/` for `rusqlite` and `vault::search` imports. Document the invariant in `docs/architecture.md`. See the "What we lose, and what replaces it" section. |
| Cortex's existing global args (`--vault`, `--config`, `--verbose`, `--log-level`) need to live under the `cortex` subcommand group, not at the root `sb` parser, so `sb cortex --vault PATH --config X classify` parses identically to today's `cortex --vault PATH --config X classify` | Medium | Low | clap's `#[command(subcommand)]` with per-group global args handles this cleanly via `args_conflicts_with_subcommands` + `Args` attached to the subsystem-level struct. Verify by writing the integration test that round-trips each existing CLI form. The same applies to borg's existing `-c/--config`, `-v/--verbose`, `-l/--log-level` and oracle's matching set. |
| `bump`'s workspace-version mode mis-detects the new layout (new bin crate, lib-only existing crates) | Low | Low | All crates already use `version.workspace = true`. Verify after the refactor that `bump` updates `[workspace.package].version` and all crates pick it up. Add a smoke test to `bump`-time CI: assert all crate versions match. |
| `cargo build` after the refactor defaults to building all workspace members and is slower than today's `cargo build -p borg` | Low | Low | Set `[workspace.default-members = ["sb"]]` in the root `Cargo.toml` so `cargo build` builds the bin by default. Library tests still run via `cargo test --workspace`. |
| `otto ci`'s existing `--features vec` flag needs to flow to the `sb` bin (which transitively wants vault's `search`, `watcher`, `vec` features through cortex/oracle) | Low | Med | The `sb/Cargo.toml` declares dependencies on cortex/oracle that pull in the needed vault features. `cargo check/test --workspace --features vec` resolves through. Verify by running otto ci against the new layout before phase-4 cutover. |

## Open Questions

- [ ] **Scope decision (real):** does Phase 2 (cross-cutting command bodies for status/doctor/bootstrap) ship in the same change as Phases 1/3/4, or as a follow-up after the bin-unification lands? Arguments either way: shipping together gets the full operator experience in one step; splitting reduces blast radius and lets the unification stabilize before the new commands' designs are settled. Default in the plan above is "together"; flag this to revisit before implementation starts.
- [ ] **Audit follow-up:** are there scripts or external integrations (besides `.mcp.json`, systemd units, Telegram bot HTTP calls, fabric/hotkey shims) that hardcode the binary names `borg`/`cortex`/`oracle`? A grep across `~/repos/scottidler/dotfiles/` and the Telegram bot config before cutover. Probably yields nothing - Scott is the only consumer.
- [ ] **Out of scope, tracked for follow-up:** consolidate `sb borg migrate` and `sb cortex migrate` into one `sb migrate` at the root. Two `migrate` subcommands is real drift but its own design decision.

## References

- Workspace manifest: `Cargo.toml`
- Today's bin crates: `borg/Cargo.toml`, `cortex/Cargo.toml`, `oracle/Cargo.toml`
- Shared libs: `vault/`, `distillers/`
- Systemd units currently in `~/.config/systemd/user/borg.service` and `cortex.service` (not in repo)
- `.mcp.json` at workspace root
- `.otto.yml`'s `deploy` task (current install logic)
- CLAUDE.md (workspace-level + `~/repos/scottidler/claude/HOME/repos/.claude/rules/rust.md` for the Shell/Core split convention this design follows)
- Memory: `feedback-design-doc-first`, `feedback-no-phase-gating`, `feedback-self-contained`, `feedback-no-full-paths-for-installed-bins`, `project-deploy-debt`
