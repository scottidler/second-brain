# Design Document: sb v0.8.5 Shakedown Cleanup

**Author:** Scott (with claude)
**Date:** 2026-05-20
**Status:** Implemented
**Review Passes Completed:** 5/5 + Architect consultation
**Tracking:** `docs/shakedown-sb-v0.8.5.md`

## Summary

The v0.8.5 shakedown surfaced seven defects spanning config layout, CLI argument plumbing, response shape consistency, error reporting polish, and a residual memory leak in the cortex daemon. This document phases the fixes from the highest-blast-radius change (config unification across the workspace) down to mechanical bug fixes, and folds in the long-deferred Phase 2 of the embed memory bounding work.

## Problem Statement

### Background

`sb v0.8.5` consolidated the prior borg/cortex/oracle binaries into a single binary with subcommands. The unification covered the binary surface, the logging surface (all subsystems write to `~/.local/share/sb/<name>.log`), and the systemd unit story. It did NOT cover:

- The on-disk config layout. Each subsystem still has its own directory under `~/.config/`:
  - borg loader: `~/.config/borg/borg.yml`
  - cortex loader (actual): `~/.config/cortex/cortex.yml`
  - cortex docstring + `sb bootstrap` + `sb status`/`doctor`: `~/.config/obsidian-cortex/obsidian-cortex.yml` (three sites refer to a path the loader never reads)
  - oracle loader: `~/.config/oracle/oracle.yml`
  - shared catalogue (canonical-tags, tag-mapping, tag-proposals): `~/.config/second-brain/*.yml` - paths are baked as DEFAULTS in `borg/src/config.rs:781` and `cortex/src/config.rs:257-259`, but each user `borg.yml` / `cortex.yml` can override `canonical_path` / `mapping_path` / `proposals_path` to point anywhere. This means the migration touches two layers: code defaults and (potentially) user yaml values.
  - patterns directory: `~/.config/borg/patterns/` (Fabric patterns for L2 distillation).
- **Vault-root resolution drift across the three subsystems** (surfaced by the Architect review). Each subsystem's `VaultConfig::default()` returns a different value for the same conceptual field:
  - `borg/src/config.rs:800` -> `"~/obsidian-vault"` (a path that does not exist on Scott's machine; works only because his `borg.yml` overrides it)
  - `oracle/src/config.rs:93` -> `"~/repos/scottidler/obsidian"` (Scott's actual vault, hardcoded into the binary)
  - `cortex/src/config.rs:143` -> `None`, with `Config::vault_root()` at line 634 falling through to `std::env::current_dir()`
  Three different "where is the vault by default" answers in one binary. The cortex CWD fallback is the worst of the three because it silently treats any working directory as a vault target, including directories where `--apply` verbs would mutate arbitrary markdown files. Cortex.yml on Scott's machine has `root-path` commented out, so manual `sb cortex` invocations without `-r` hit the CWD fallback today.
- Several CLI-arg-to-business-logic seams (`--scan`, `--format` validation).
- The oracle MCP response shape - eight tools settle on `{count, results}`, three diverge to `{count, tags|sources|creators}`, one (`domain_brief`) uses `{recent_notes}`.
- Cortex's per-tick model load (Phase 1 of the embed-bounding doc shipped; Phase 2 still pending). Observed RSS climb during this shakedown: 1.2 GB -> 2.8 GB in ~50 minutes.

The shakedown also flagged the `eyre` `Location:` block leaking into every user-facing error; the codebase is already on eyre (86 files), so this is a Display-customization fix, not a migration.

### Problem

Eight items, all confirmed:

1. **Config-path drift.** `sb status`/`doctor` warn about a "missing" cortex config that the loader is not even configured to read.
2. **`sb cortex daemon --status` prints a hint, not status.** Inconsistent with `sb borg daemon --status` which embeds full systemctl + journal output.
3. **`sb cortex lint --format yaml` (or any unknown value) silently falls back to human format.** clap should reject the value at parse time.
4. **`sb cortex link --scan {people,projects,concepts,all}` is a dead flag.** `LinkArgs.scan` is parsed but never reaches `cortex::link()`; the linker always uses `config.actions.linking.scan_for`.
5. **Oracle MCP response shapes are inconsistent.** `tag_search` returns `{tags}`, `source_browse` returns `{sources}`, `creator_browse` returns `{creators}`, `domain_brief` exposes `recent_notes` instead of `results`. Generic jq pipelines built around `.results[]` silently return nothing for these tools.
6. **eyre's `Location: <file>:<line>:<col>` line is printed on every error.** It exposes internal source paths to users who hit a bad arg.
7. **Cortex daemon baseline RSS grows monotonically.** H2 (catastrophic OOM) was bounded in v0.8.1; H1 (per-tick model reload allocator churn) remains. The "no known leaks on main" rule requires this to land.
8. **Vault-root resolution is inconsistent across the three subsystems and unsafe in cortex.** Three different `VaultConfig::default()` values (see Background), and cortex's CWD fallback turns "run `sb cortex lint` from the wrong directory" into either a wasted run (read-only verbs) or a markdown-rewriter on the wrong tree (`--apply` verbs). The shakedown trip wire fired exactly once - on a read-only run. The `--apply` blast radius is the real risk.

### Goals

- Single config directory `~/.config/sb/` containing per-subsystem yaml files and the shared catalogue files, with a one-shot migration from the legacy locations.
- Single shared vault-root resolver consumed by all three subsystems. The unified binary has one answer to "where is the vault" - not three.
- No subsystem silently treats an arbitrary working directory as a vault. The CWD branch of vault resolution requires explicit proof (a `.obsidian/` marker directory) before accepting the path.
- All CLI flags either reach their destination or be removed - no silently dead flags.
- One consistent MCP/JSON response shape across all 18 oracle tools, with a deprecation alias for the existing keys so prior callers do not break in-flight.
- Errors that show users the message and the suggested fix without leaking source file paths.
- Cortex daemon RSS stays bounded across long-running operation - tunable, observable, regression-tested.

### Non-Goals

- Changing the binary surface (`sb borg/cortex/oracle/...` subcommand layout stays).
- Touching the systemd unit naming. `borg.service` and `cortex.service` stay; only their `ExecStart`/`Environment` paths change if config paths change.
- Migrating off the `dirs` crate. Configs continue to live under `dirs::config_dir()`, just under a single `sb/` subdirectory.
- Re-doing the L2 distillation contract, retention model, blocklist, or any other ingestion-pipeline surface.
- Phase 2 of bounding's "long-lived model" only ships if measurement (in this doc, Phase 7) confirms candle internals are bounded across the input distribution. If measurement says otherwise, Phase 7's deliverable is the measurement report and a different fix; we do not promise the lifecycle change up front.

## Proposed Solution

### Overview

Seven phases (Phase 1 has two sub-phases that ship together), ordered by blast radius (largest first so the smaller mechanical fixes land on a settled foundation). Each phase is independently shippable; nothing later depends structurally on anything earlier except for Phase 1 (config + vault-root) being the layer the rest assumes.

| # | Phase | Why this order | Touches |
|---|-------|----------------|---------|
| 1a | Config unification to `~/.config/sb/` | Touches the most files; every later phase reads config | all 5 crates + sb/cli + bootstrap + systemd templates |
| 1b | Vault-root resolution unification | Same crates as 1a; the two changes share a `vault::paths` module and migrate in one PR | borg/cortex/oracle config.rs + new vault::paths |
| 2 | Oracle response shape unification | API-shape change with deprecation aliases; independent of config | oracle/src/server.rs |
| 3 | Eyre error Display polish | Cross-cutting; small | sb/src/main.rs + eyre install hook |
| 4 | `--scan` flag wiring | Single-file fix once decided | cortex/src/lib.rs + linking config |
| 5 | `--format` enum validation | Trivial; clap derive change | sb/src/cli/cortex.rs |
| 6 | `sb cortex daemon --status` parity | Trivial; mirror borg | sb/src/cli/cortex.rs |
| 7 | Cortex embed H1 leak | Needs measurement; lands last so we ship on a clean base | cortex/src/embed.rs + vault/src/embedding/candle.rs |

### Architecture

#### Phase 1a: Config layout under `~/.config/sb/`

New layout:

```
~/.config/sb/
  borg.yml                 # was ~/.config/borg/borg.yml
  cortex.yml               # was ~/.config/cortex/cortex.yml
  oracle.yml               # was ~/.config/oracle/oracle.yml (often defaults-only)
  canonical-tags.yml       # was ~/.config/second-brain/canonical-tags.yml
  tag-mapping.yml          # was ~/.config/second-brain/tag-mapping.yml
  tag-proposals.yml        # was ~/.config/second-brain/tag-proposals.yml
  patterns/                # was ~/.config/borg/patterns/
    distill-article.md
    distill-repo.md
    ...
```

Rationale (mirroring `~/.local/share/sb/`):

- Logs already colocate under `dirs::data_local_dir()/sb/`. Configs colocating under `dirs::config_dir()/sb/` makes the install footprint trivially discoverable: `sb/` directories under both `~/.config` and `~/.local/share` and nothing else.
- A single root means `sb bootstrap` writes one directory; `sb doctor` checks one directory; uninstall is one `rkvr rmrf` of `~/.config/sb/` (plus the data dir).
- Shared catalogue files (canonical-tags, tag-mapping, tag-proposals, patterns/) move alongside the subsystem configs they serve - both borg and cortex read them today, and `~/.config/sb/` is the natural neutral root.

One shared module owns the path resolution:

```rust
// vault/src/paths.rs (new)
pub fn config_root() -> PathBuf {
    dirs::config_dir()
        .expect("dirs::config_dir() returned None (set XDG_CONFIG_HOME)")
        .join("sb")
}

pub fn borg_config() -> PathBuf       { config_root().join("borg.yml") }
pub fn cortex_config() -> PathBuf     { config_root().join("cortex.yml") }
pub fn oracle_config() -> PathBuf     { config_root().join("oracle.yml") }
pub fn canonical_tags() -> PathBuf    { config_root().join("canonical-tags.yml") }
pub fn tag_mapping() -> PathBuf       { config_root().join("tag-mapping.yml") }
pub fn tag_proposals() -> PathBuf     { config_root().join("tag-proposals.yml") }
pub fn patterns_dir() -> PathBuf      { config_root().join("patterns") }
```

Every loader, bootstrap site, doctor site, and systemd template reads from this module. Hardcoded paths anywhere else are a code-review reject.

**Migration:**

`sb bootstrap` gains a `--migrate` mode (and runs migration automatically on the first invocation that detects a legacy directory). The migration is a copy + rewrite-in-place with idempotence checks:

1. **Detect:** any of `~/.config/{borg,cortex,obsidian-cortex,oracle,second-brain}` containing a non-empty .yml, OR the patterns dir at `~/.config/borg/patterns/`.
2. **Copy:** for each legacy path -> new path:
   - If new path exists and contents differ: refuse and print a diff hint ("legacy file ~/.config/borg/borg.yml differs from ~/.config/sb/borg.yml; resolve manually before rerunning").
   - If new path does not exist: copy legacy -> new (preserve mtime).
   - If new path exists and contents match: noop.
   - Patterns directory: recursive copy if target doesn't exist; refuse if target exists with differing content.
3. **Rewrite paths inside copied yaml files.** The borg / cortex yaml files contain `canonical_path: "~/.config/second-brain/canonical-tags.yml"` style fields. After copying, parse the yaml and rewrite any value matching the legacy patterns to the new `~/.config/sb/` location. This handles users who relied on the in-yaml defaults; users who pointed these paths somewhere custom keep their custom path untouched (only literal legacy paths are rewritten).
4. **Marker:** leave a `.migrated-to-sb` marker file in each legacy directory so a second run does not re-copy stale files if the user edits the new copy.
5. **Never delete the legacy directory.** A future `sb bootstrap --prune-legacy-config` verb (out of scope here) is the cleanup. Until the user opts in, both layouts coexist on disk and the loader reads from `~/.config/sb/` only.

Update the in-code DEFAULTS in `borg/src/config.rs` and `cortex/src/config.rs` to point at the new shared paths as part of Phase 1a; for users on a fresh install (no legacy directories), the defaults give them the right layout with no migration needed.

**systemd implication:** borg.service and cortex.service today have `--config` flags in their `ExecStart`. After the path change, `sb borg daemon --install` / `sb cortex daemon --install` regenerates the unit file with the new path. `otto deploy` already restarts services after install; the existing flow picks up the new `ExecStart` automatically.

#### Phase 1b: Vault-root resolution unification

Single shared resolver in the `vault` crate, consumed by all three subsystems:

```rust
// vault/src/paths.rs (extends the Phase 1a module)
use std::path::{Path, PathBuf};
use eyre::{eyre, Result};

/// Resolve the vault root with explicit precedence: CLI > config > marker-gated CWD.
///
/// Returns an error rather than silently picking up an arbitrary working directory.
pub fn resolve_vault_root(
    cli_override: Option<&Path>,
    config_value: Option<&str>,
) -> Result<PathBuf> {
    if let Some(p) = cli_override {
        return Ok(p.to_path_buf());
    }
    if let Some(s) = config_value {
        let expanded = shellexpand::tilde(s);
        return Ok(PathBuf::from(expanded.as_ref()));
    }
    let cwd = std::env::current_dir()?;
    if cwd.join(".obsidian").is_dir() {
        return Ok(cwd);
    }
    Err(eyre!(
        "vault root not set: pass --vault <path>, set `vault.root-path` in your config, \
         or run from a directory that contains a `.obsidian/` directory.\n\
         (current directory: {})",
        cwd.display()
    ))
}
```

**Behavioral changes from today:**

| Before | After |
|--------|-------|
| `borg`'s `VaultConfig::default()` returns `root_path: "~/obsidian-vault"` (literal hardcoded path) | `root_path: None` |
| `oracle`'s `default_vault_root()` returns `"~/repos/scottidler/obsidian"` (literal hardcoded path) | `root_path: None` |
| `cortex`'s `Config::vault_root()` falls through to `std::env::current_dir()` unconditionally | The CWD branch requires `.obsidian/` to exist; otherwise hard-error |
| `cortex::Config::vault_root()` lives as a method on `cortex::Config` | Replaced by `vault::paths::resolve_vault_root`; the cortex method is deleted |

**Why `.obsidian/` as the marker.** Obsidian writes this directory the moment a vault is opened in the app. It is the universal "this is a vault" signal that every Obsidian-aware tool already reads. We are not inventing a new marker - we are checking for the one already there.

**Live-system impact on Scott's machine (verified):**

- `borg.yml` sets `vault.root-path: ~/repos/scottidler/obsidian/` -> tier 2 wins -> no change in behavior.
- `cortex.service` systemd unit invokes cortex with `--vault /home/saidler/repos/scottidler/obsidian` -> tier 1 wins -> no change in behavior.
- `cortex.yml` has `root-path` commented out today -> manual `sb cortex` invocations without `-r` from inside `~/repos/scottidler/second-brain` silently lint the code repo. After Phase 1b they error with the message above. That is the fix.
- `oracle.yml` is missing today (defaults used). After Phase 1b, oracle also requires `root-path` set or CWD-with-marker. `sb bootstrap` writes the new config template with `root-path` seeded from whichever subsystem config already has it.

**Migration assistance.** Phase 1a's yaml rewriter also seeds `root-path` into any subsystem config that lacks it, sourcing the value with precedence: existing borg.yml value > existing cortex.yml value > existing oracle.yml value > systemd unit `ExecStart` parsed for `--vault`. If no source has a value, the rewriter leaves `root-path: ~` commented out, and the resolver's marker-gated CWD or explicit `--vault` becomes the only path forward. This means existing users with one configured subsystem get all three pre-configured after migration.

#### Phase 2: Oracle response shape unification

Every tool that today returns `{count, sources|tags|creators}` adds a `results` alias (and we update the documentation/MCP descriptions to point at `results`). `domain_brief` adds `results` alongside its existing `recent_notes`. The old keys stay for one release as a transition; they are removed in the release after.

Pattern:

```rust
// Before (oracle/src/server.rs:722):
"sources": sources,

// After:
"results": sources.clone(),
"sources": sources, // deprecated alias; removed in a follow-up release
```

Tool inventory and target shape:

| Tool | Current | After Phase 2 |
|------|---------|---------------|
| `knowledge_search` | `{count, results}` | unchanged |
| `list_notes` | `{count, results}` | unchanged |
| `find_similar` | `{count, results}` | unchanged |
| `recent_activity` | `{count, days, results}` | unchanged |
| `tag_search` (with tag) | `{count, results, tag}` | unchanged |
| `tag_search` (no tag) | `{tags}` | `{count, results, tags}` (results aliases tags) |
| `source_browse` (no host) | `{count, sources}` | `{count, results, sources}` |
| `creator_browse` (no creator) | `{count, creators}` | `{count, results, creators}` |
| `domain_brief` | `{... recent_notes}` | `{... results, recent_notes}` |
| `note_read` / `vault_overview` / `schema_info` / `inbox_status` / `quality_report` / `find_links` / `classify_status` / `duplicate_groups` / `ingest_history` / `reindex` | per-tool object | unchanged - they are not "list of things" tools |

Also fixes the `domain_brief.unread_count: null` observed in shakedown - this is a missed `unwrap_or(0)` in the response builder; trivial.

#### Phase 3: Eyre error Display polish

eyre's default `Report` Display (via `Debug` impl, since `fn main() -> Result<()>` uses Debug to print) includes the `Location: <file>:<line>:<col>` block from the original `eyre::eyre!` / `wrap_err` site. We hide this in user-facing output by installing a custom `eyre::set_hook` in `sb/src/main.rs` before `Cli::parse()`. The hook prints:

- The root error message (top of the chain).
- Each `Caused by:` context, indented.
- Nothing else: no Location, no backtrace, unless `--verbose` was passed OR `RUST_BACKTRACE=1` is set in the environment.

Implementation can be hand-rolled (about 30 lines wrapping `eyre::EyreHandler`) or use `color-eyre` (which provides the same Location-pruning behavior plus terminal coloring honoring `NO_COLOR`). Hand-rolled is preferred because it's one fewer dep and we don't actually need the color output - we want fewer characters, not more colorful ones. Decide at implementation time based on which produces the smaller diff.

The hook installs before `Cli::parse()` so even parse-time errors get the cleaned format (note: clap errors do not flow through eyre, so they keep their existing clean format - this hook only affects eyre `Report` printing).

#### Phase 4: `--scan` flag wiring

`LinkArgs.scan: String` becomes `LinkArgs.scan: ScanScope` (enum derives `clap::ValueEnum`). Values: `People | Projects | Concepts | All` (default `All`).

In `cortex/src/lib.rs::link`:

```rust
pub fn link(vault_root: &Path, config: &Config, opts: &LinkOpts) -> Result<Report> {
    let scan_for = opts.scan.as_str_set();   // overrides config when not "all"
    let linking_config = if opts.scan != ScanScope::All {
        let mut c = config.actions.linking.clone();
        c.scan_for = scan_for;
        Cow::Owned(c)
    } else {
        Cow::Borrowed(&config.actions.linking)
    };
    // ... rest unchanged, uses linking_config.scan_for
}
```

CLI flag wins over config when explicitly set; config wins when the user does not pass `--scan` (or passes `--scan all`).

#### Phase 5: `--format` enum validation

```rust
// sb/src/cli/cortex.rs LintArgs
#[derive(Args)]
pub struct LintArgs {
    // ...
    #[arg(long, value_enum, default_value_t = LintFormat::Human)]
    pub format: LintFormat,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum LintFormat { Human, Json }
```

Bad values (`--format yaml`) error at parse time with clap's standard "invalid value 'yaml' for '--format <FORMAT>': possible values: human, json".

#### Phase 6: `sb cortex daemon --status` parity

Match borg's pattern. Replace the current "Service file: ... \n Check status: systemctl ..." print with a `systemctl --user status cortex --no-pager` exec, mirroring `borg/src/lib.rs` (or wherever borg's status verb lives) line by line.

#### Phase 7: Cortex embed H1 leak

Two sub-steps, gated on measurement:

**7a (always ships): instrument and observe.** Add per-tick RSS sampling to the cortex daemon. Before each `run_embed` invocation, before `load_active_model`, after the model drop, log the daemon's RSS (read from `/proc/self/status` `VmRSS`). With this in place, we have a trace of:
- Steady-state baseline before a tick.
- Peak during inference.
- Post-drop baseline.
- Delta per tick across hours of idle ticks.

This is the measurement the bounding doc deferred. It is small (one helper, one log line on entry, one on exit) and is valuable independent of what we do next.

**7b (ships if measurement supports it): one-shot model load at daemon start.** If the per-tick delta with the current per-tick load pattern shows monotonic growth (it does in the shakedown evidence; 1.2 -> 2.8 GB in 50 min), AND a synthetic test confirms candle's per-instance scratch state is bounded across varying input distributions, THEN move `load_active_model` from `run_embed` to `Daemon::start`. The model lives in the daemon struct; `run_embed` borrows it.

The synthetic test (deferred per the bounding doc):

```rust
// vault/tests/candle-bounded.rs
#[test]
#[ignore] // costly; run with `cargo test --release -p vault -- --ignored`
fn candle_bert_rss_plateaus_across_1000_calls() {
    let m = CandleBertModel::load_with_workers(1).unwrap();
    let baseline = read_rss();
    for batch_idx in 0..1000 {
        let texts = vary_lengths(batch_idx);  // mix short summaries + 512-tok chunks
        let _ = m.embed_batch(&texts).unwrap();
        if batch_idx % 100 == 0 {
            let now = read_rss();
            eprintln!("after {} batches: rss={} MB (delta={})", batch_idx, now, now - baseline);
        }
    }
    let final_rss = read_rss();
    // Acceptance: RSS does not grow more than 200 MB over baseline after warmup.
    assert!(final_rss - baseline < 200 * 1024 * 1024,
            "candle leaked: baseline={} final={}", baseline, final_rss);
}
```

If the test fails, 7b does not ship. Phase 7 in that case becomes "ship 7a, file the candle leak as a candle-internals issue with the measurement attached, and revisit in a follow-up release."

### Data Model

No persistent data-model changes. Config files keep their existing yaml schema; only their on-disk path changes. SQLite schema (oracle index) is untouched.

### API Design

**Path module (new):**

```rust
// vault/src/paths.rs
pub fn config_root() -> PathBuf;
pub fn borg_config() -> PathBuf;
pub fn cortex_config() -> PathBuf;
pub fn oracle_config() -> PathBuf;
pub fn canonical_tags() -> PathBuf;
pub fn tag_mapping() -> PathBuf;
pub fn tag_proposals() -> PathBuf;
pub fn patterns_dir() -> PathBuf;
```

**Eyre hook (new):**

```rust
// sb/src/main.rs
fn install_error_hook(verbose: bool) -> Result<()>;  // hides Location unless verbose
```

**CLI type changes (Phase 4 + 5):**

```rust
pub struct LinkArgs {
    pub apply: bool,
    pub scan: ScanScope,   // was String
}

pub enum ScanScope { People, Projects, Concepts, All }

pub struct LintArgs {
    pub apply: bool,
    pub format: LintFormat,   // was String
    // ...
}

pub enum LintFormat { Human, Json }
```

**Oracle response (Phase 2):** adds `results` as a transitional alias on three browse tools and one brief tool; no breaking change in this release.

### Implementation Plan

#### Phase 1a: Config unification to `~/.config/sb/`
**Model:** opus

- Add `vault/src/paths.rs` with the constants above.
- Update `borg/src/config.rs` to read from `paths::borg_config()`; rename the loader's "primary" path.
- Update `cortex/src/config.rs` to read from `paths::cortex_config()`; remove the `~/.config/cortex/cortex.yml` literal at line 600 and the stale `obsidian-cortex` doc comment at line 592.
- Update `oracle/src/config.rs` to read from `paths::oracle_config()`.
- Update every site that touches the shared catalogue files (canonical-tags, tag-mapping, tag-proposals, patterns/) to use `paths::*`.
- Update `sb/src/cli/bootstrap.rs` targets list to use `paths::*`.
- Update `sb/src/cli/checks.rs:143-144` to check `paths::cortex_config()` and `paths::oracle_config()`.
- Implement `sb bootstrap --migrate` (and auto-trigger on detection per the migration spec above).
- Update borg/cortex `--install` systemd template generators to write the new `ExecStart` config path.
- Update `config/templates/*.yml.example` paths in any README references.
- Update CLAUDE.md "Key Conventions" section to document the new layout.
- Manual smoke test: stop both daemons, run `sb bootstrap --migrate`, start daemons, run `sb status`, assert all-green and no "missing" warnings.

#### Phase 1b: Vault-root resolution unification
**Model:** opus

- Add `vault::paths::resolve_vault_root(cli, config) -> Result<PathBuf>` per the architecture sketch.
- `borg/src/config.rs:800` - change `VaultConfig::default()` so `root_path` becomes `Option<String>` with default `None` (was `String` with default `"~/obsidian-vault"`). Audit every caller of `config.vault.root_path` in `borg/src/` and rewrite them to call `resolve_vault_root(cli_override, config.vault.root_path.as_deref())`.
- `oracle/src/config.rs:93` - delete `default_vault_root()` literal; change `vault.root_path` to `Option<String>` with default `None`; route through `resolve_vault_root`.
- `cortex/src/config.rs:143` - `root_path` already `Option<String>`, no shape change. Delete the inherent `Config::vault_root()` method at line 625-634. Update every cortex call site (currently `config.vault_root(cli.vault.as_ref())`) to call `resolve_vault_root` instead.
- Update Phase 1a's yaml rewriter (the migration tool) so that when a yaml file lacks a `root-path` value, it seeds the field from the source-of-truth precedence chain (borg > cortex > oracle > systemd `--vault`).
- Update `config/templates/{borg,cortex,oracle}.yml.example` to have a commented `# root-path: ~/path/to/vault` line in the `vault:` section, with a one-line note that if commented out, the runtime requires `--vault` or a `.obsidian/` directory in CWD.
- Tests in `vault/src/paths/tests.rs` (new file):
  - cli_override wins when both set
  - config wins when cli is None
  - cwd-with-marker wins when both None (tempdir fixture: create `.obsidian/` dir, set CWD)
  - cwd-without-marker errors with the expected message
  - all-None errors with the expected message
- Update the `sb status` / `sb doctor` checks at `sb/src/cli/checks.rs` to report a `Warn` when vault.root_path is unset in any subsystem config; the fix suggestion is "set `vault.root-path` in `~/.config/sb/<subsystem>.yml`".

#### Phase 2: Oracle response shape unification
**Model:** sonnet

- In `oracle/src/server.rs`, add `results` alias to:
  - line 503 (tag_search no-arg): `"results": tags.clone(), "tags": tags`
  - line 677 (creator_browse no-arg): `"results": creators.clone(), "creators": creators`
  - line 722 (source_browse no-host): `"results": sources.clone(), "sources": sources`
  - domain_brief response: `"results": recent_notes.clone(), "recent_notes": recent_notes`
- Fix `domain_brief.unread_count` null: replace `Option` with `Option::unwrap_or(0)` in the response builder.
- Update MCP tool descriptions (returned by `sb oracle call --list`) to mention `results` as the primary field; legacy names listed as deprecated.
- Add a test per affected tool: assert both `results` and the legacy key exist and are identical.
- Note in `docs/design/2026-03-21-oracle-tools-expansion.md` (or a follow-up note) that the legacy `tags`/`sources`/`creators`/`recent_notes` aliases are deprecated and will be removed in a follow-up release once we have confirmed no downstream tool reads them.

#### Phase 3: Eyre error Display polish
**Model:** sonnet

- Add `color-eyre` to the sb crate (or a hand-written `eyre::set_hook` in `sb/src/main.rs`).
- The hook hides the `Location:` block unless `--verbose` was parsed OR `RUST_BACKTRACE=1` is set OR `RUST_LIB_BACKTRACE=1` is set.
- Verify by running `sb borg replay` (no args) and confirming the output is just `Error: replay: must provide a trace_id ...` with no Location line.
- Verify `sb -v borg replay` still shows Location for debug.

#### Phase 4: `--scan` flag wiring
**Model:** sonnet

- Change `LinkArgs.scan` to `ScanScope` enum with `ValueEnum` derive.
- Plumb through `opts::LinkOpts`.
- In `cortex/src/lib.rs::link`, override `config.actions.linking.scan_for` from `opts.scan` when not `ScanScope::All`.
- Add a unit test in `cortex/src/tests.rs` (or wherever the link tests live) that asserts `--scan people` produces a strict subset of `--scan all`.

#### Phase 5: `--format` enum validation
**Model:** sonnet

- Change `LintArgs.format` from `String` to `LintFormat` enum with `ValueEnum`.
- Add a unit test: `LintArgs::try_parse_from(&["sb", "cortex", "lint", "--format", "yaml"])` returns an error mentioning "possible values: human, json".

#### Phase 6: `sb cortex daemon --status` parity
**Model:** sonnet

- Replace the hint-print in `sb/src/cli/cortex.rs` daemon handler with an exec of `systemctl --user status cortex --no-pager`, mirroring borg's pattern.
- Confirm output matches what `sb borg daemon --status` produces.

#### Phase 7: Cortex embed H1 leak
**Model:** opus

- 7a (always lands): add `vault/src/rss.rs::read_self_rss() -> Option<u64>` (reads `/proc/self/status` `VmRSS`). Log it at `info!` on every `run_embed` entry + exit. Ship.
- Watch the daemon for at least 24 hours of normal traffic. Capture: idle-tick delta, embedding-tick peak, post-drop delta.
- 7b (conditional): if the test below passes, move `load_active_model` into the cortex Daemon struct constructed at `--start`; pass the model by `&` into `run_embed`.
- Add `vault/tests/candle-bounded.rs` (the test sketch above), marked `#[ignore]`. Document the command to run it.
- If 7b ships: update `[[project-cortex-embed-memory-leak]]` memory to RESOLVED. If 7b does not ship: file the residual leak with the measurement data and link from the memory entry.

## Alternatives Considered

### Alternative 1: Single `~/.config/sb/sb.yml` (one file, all sections)

- **Description:** Collapse all three subsystem configs into one big YAML keyed by subsystem.
- **Pros:** One file, one path, one schema validator entry point. Slightly easier to ship to a new machine.
- **Cons:** Loses per-subsystem editability without diff noise. Daemons would have to re-read the whole file even when only their section changed. The shared catalogue files (canonical-tags etc.) still need to be separate from any per-subsystem yaml since they're hand-edited reference data.
- **Why not chosen:** the subsystems already have stable independent schemas; bundling them buys nothing the directory layout doesn't already give us and creates a load-time coupling.

### Alternative 2: Use XDG strictly - keep `~/.config/{borg,cortex,oracle}` but write a shared "config root override" path file

- **Description:** Leave existing paths alone; add a `~/.config/sb/config.toml` that lists where each subsystem's config lives, and have loaders read that pointer file first.
- **Pros:** Zero migration; all existing user configs keep working.
- **Cons:** Adds an indirection layer for no real benefit. Drift between the pointer file and reality is the same class of bug we are trying to fix.
- **Why not chosen:** the indirection IS the problem; we want one obvious place, not a "look here to find where to look."

### Alternative 3: Hard-break the legacy paths (no migration, no compatibility)

- **Description:** Just change the loader paths; if users have existing configs at the legacy paths, they get default behavior and have to manually move files.
- **Pros:** Smallest code change.
- **Cons:** Scott's own running daemons read from the legacy paths. The first restart after this version lands silently reverts him to defaults. Not acceptable.
- **Why not chosen:** see "Scott's own daemons."

### Alternative 4: Keep cortex's CWD fallback unconditional (no marker check)

- **Description:** Phase 1b's behavioral change (require `.obsidian/` in CWD to use the fallback) is dropped. Cortex keeps its current "any CWD wins" behavior; only the three different hardcoded defaults are unified.
- **Pros:** Smaller diff. Zero risk of breaking the "cd into vault && sb cortex lint" muscle memory for any user who is currently relying on it (though we know of none).
- **Cons:** Leaves the `--apply` blast radius open. The whole reason this surfaced in shakedown was a no-arg invocation silently picking up the wrong tree; the fix that lets the next reader trip the same wire is no fix at all.
- **Why not chosen:** "fix it or remove it" - the marker check IS the fix. Leaving it unconditional is the same defect with a less-embarrassing default value.

### Alternative 4b: Hardcode a single default path (e.g., `~/obsidian`) across all three subsystems

- **Description:** What the Architect initially proposed. All three `VaultConfig::default()` impls return `Some("~/obsidian")` so the binary always has a fallback.
- **Pros:** No errors on fresh install if the user happens to keep a vault at `~/obsidian`.
- **Cons:** Hardcodes a personal-machine assumption into a shared binary. Anyone with a vault elsewhere (most users) gets a useless default. The Architect's `~/obsidian-vault` and `~/repos/scottidler/obsidian` legacy defaults are the same anti-pattern that produced this drift in the first place; replacing three bad defaults with one bad default does not fix the class of bug.
- **Why not chosen:** `None` + explicit error is honest about what the binary knows. The marker-gated CWD branch gives ergonomic "cd into vault" usage when it's safe; the error message tells the user exactly how to fix the rest.

### Alternative 5: Remove the `--scan` flag entirely (treat it as config-only)

- **Description:** Phase 4 simpler: just delete the `--scan` flag from `LinkArgs`; users edit `cortex.yml` to change linking scope.
- **Pros:** Lower-surface CLI.
- **Cons:** The flag is documented in `--help`; users expect it to work; the design intent of having a CLI override was correct.
- **Why not chosen:** the flag was designed for a reason (per-invocation override of config default); the bug is that it was never wired. Fixing the wiring is the smaller diff than removing-and-explaining.

### Alternative 6: Phase 7b first - assume candle is well-behaved, ship the long-lived model now

- **Description:** Skip the measurement; ship the lifecycle change immediately. The bounding doc raised the risk of monotonic candle scratch growth, but maybe it is fine.
- **Pros:** One PR fewer.
- **Cons:** The bounding doc explicitly deferred this on lack of evidence; the shakedown evidence (1.2 -> 2.8 GB in 50 min on the current short-lived pattern) does NOT prove the long-lived pattern is better - it just proves the short-lived one is bad. If candle's per-instance state IS unbounded, the long-lived model would grow forever and we would ship a worse bug than the one we have.
- **Why not chosen:** "no known leaks on main" cuts both ways - shipping a different leak is worse than shipping the measurement to choose the right fix.

## Technical Considerations

### Dependencies

- Phase 3: either zero new deps (hand-rolled `eyre::set_hook`) or one new runtime dep (`color-eyre`). Hand-rolled preferred.
- No new runtime dependencies for Phases 1, 2, 4, 5, 6.
- Phase 7: no new deps; just read `/proc/self/status` to extract `VmRSS:` (linux-only; on macOS the RSS reader returns `None` and the log line is skipped).

### Performance

- Phase 1: zero runtime cost; path resolution moves from string literal to `&'static` `OnceLock` constants.
- Phase 2: response builders allocate one extra `Vec<Value>::clone()` per affected tool. Negligible (results lists are bounded by `limit`).
- Phase 3: error path only; no hot-path cost.
- Phase 4: per-link-invocation `Cow` decision; negligible.
- Phase 5: clap parse-time only.
- Phase 6: same exec cost as borg's daemon --status.
- Phase 7a: one `/proc/self/status` read per embed tick (every 10 min by default). Free.
- Phase 7b: faster (no per-tick model load) AND lower-RSS, if measurement supports.

### Security

- Phase 1 migration: never deletes legacy files; never reads outside `dirs::config_dir()`. The conflict-detection path uses byte comparison, not exec.
- No new network surface; no new credentials handling.

### Testing Strategy

| Phase | Unit | Integration | Manual |
|-------|------|-------------|--------|
| 1a | Path constants resolve correctly under tempdir HOME; migration copies, refuses on diff, idempotent on re-run | `sb bootstrap --migrate` from a fixture with legacy layout produces the new layout | live daemon restart on this machine |
| 1b | `resolve_vault_root` returns Ok for cli/config/cwd-with-marker; returns Err for cwd-without-marker and all-None | `sb cortex lint` from a non-vault tempdir errors with the new message; `sb cortex lint` from a vault tempdir works without `-r`; daemon ExecStart still uses `--vault` so daemons are unaffected | run `sb cortex lint` from `~/repos/scottidler/second-brain` and confirm the new error message; then `cd ~/repos/scottidler/obsidian && sb cortex lint` and confirm it works |
| 2 | Each browse tool's response contains both keys and they are byte-identical | n/a | n/a |
| 3 | A failing eyre `Result` rendered through the hook contains no "Location" substring unless verbose | `sb borg replay` shows clean error | n/a |
| 4 | `link --scan people` produces a strict subset of `link --scan all` over a fixture vault | n/a | n/a |
| 5 | `LintArgs::try_parse_from` rejects unknown formats | n/a | n/a |
| 6 | n/a (shells out) | command output contains "Active: active (running)" or "Active: inactive (dead)" | n/a |
| 7 | 7a: RSS reader returns Some(N>0) on linux; None on non-linux. 7b: synthetic candle 1000-batch test (`#[ignore]`d, run on demand) | 24h daemon observation post-7a | otto deploy + watch journal |

### Rollout Plan

- Phases 1a and 1b land in one PR together. They share the `vault::paths` module and the same set of touched files (3 config.rs, sb bootstrap, sb checks, templates, systemd unit generators). Splitting them just produces two PRs that conflict.
- Phase 2 in a follow-up PR (adds, never removes; safe).
- Phases 3, 4, 5, 6 can land in one bundled PR ("v0.8.5 shakedown punchlist") since each is small and independent.
- Phase 7a lands as a small standalone PR for observability.
- After 7a's 24h soak, Phase 7b (if it ships) lands.
- After all phases, cut the next release (version determined at cut time by `bump`, not pre-named here).

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Migration silently corrupts user config | Low | High | Refuse on byte diff; never delete; idempotent marker |
| Systemd unit ExecStart path drift breaks daemon restart on otto deploy | Med | High | Phase 1 manual smoke test stops daemons, migrates, starts again; otto deploy template is regenerated by `--install` |
| Phase 2 alias keys make jq pipelines ambiguous (which key to read?) | Low | Low | Documentation explicit: `results` is the canonical key; the legacy alias is deprecated; removed in a follow-up release once we have verified nothing reads them |
| Phase 1 yaml rewrite during migration corrupts a yaml with non-trivial structure (anchors, comments) | Med | Med | Use the existing `serde_yaml`-backed `Config::load` -> mutate -> `serde_yaml::to_string` round-trip; this loses comments but preserves structure. The migrated file is a copy under `~/.config/sb/`; legacy original remains untouched, so a user can re-paste their comments after inspecting the diff. |
| Migration runs but daemon has the legacy file open with old paths cached | Low | Med | The migration spec requires `sb borg daemon --stop && sb cortex daemon --stop` (or `otto deploy`-style restart) after migration. Document this in the migration's first-run banner. |
| Phase 1b breaks an unknown user who relied on cortex's no-arg CWD fallback in a real workflow | Low | Med | Error message names the exact three options (--vault, config, .obsidian/ in CWD). The shakedown report and CHANGELOG entry call this out as a deliberate behavior change. |
| Phase 1b's `.obsidian/` check produces a false negative on a vault that was just created and never opened in Obsidian | Very Low | Low | Obsidian creates `.obsidian/` on first open; any vault Scott actually uses has it. Edge case is "user clones a vault repo and runs sb before opening it" - error message tells them the fix (`--vault` or open in Obsidian once). |
| Phase 3 color-eyre hook hides errors that were actually useful (e.g., bug reports) | Low | Med | `--verbose` and `RUST_BACKTRACE=1` both restore the full output |
| Phase 4 `--scan all` semantics change when the loader becomes config-aware | Low | Low | Unit test asserts `--scan all` == "use config default"; default config is People+Projects+Concepts == All |
| Phase 7b ships and candle internals leak after all | Med | High | Synthetic test must pass before 7b is merged; if it fails, 7b does not ship |
| 24h soak misses a leak that takes days to surface | Med | Med | Phase 7a logging stays on after 7b; we keep watching |
| Migration leaves the legacy directories on disk forever | Low | Cosmetic | A follow-up `sb bootstrap --prune-legacy-config` verb (out of scope here) handles cleanup |

## Open Questions

- [x] **Auto-migrate on first invocation, or require explicit `sb bootstrap --migrate`?** Decided: auto-migrate on first invocation; `sb bootstrap --migrate` forces a re-run. A `.migrated-to-sb` marker is dropped in each legacy directory so subsequent runs noop. The daemon-cache-old-path failure mode is documented and tolerated; the next `sb cortex daemon --install` regenerates the unit with the new path.
- [x] **Phase 2 deprecation timeline.** Sealed by deviation: legacy `tags`/`sources`/`creators`/`recent` keys were removed in v0.8.6 itself; no transition window. See Implementation Notes.
- [x] **Phase 7a log level.** Implemented at `info!` for the run-entry / load-entry / load-exit / run-exit boundaries, `debug!` for daemon-tick boundaries. Demoting all of them to `debug!` is a follow-up once the per-tick deltas have been observed steady-state for a few days.
- [x] **Hand-rolled `eyre::set_hook` vs `color-eyre`.** Decided: hand-rolled (`sb/src/error.rs`). No new runtime dep, ~60 lines, produces the same user-visible output.
- [x] **Patterns directory location.** `~/.config/sb/patterns/` (with the other configs). Sealed.

## Implementation Notes

These record deviations from the spec body above. The status of every other phase matches the design as written.

### Phase 2: Oracle response key

Spec said: every list-shaped tool's response adds `results` as a *transitional alias* alongside the existing `tags` / `sources` / `creators` / `recent_notes` key, with the legacy alias removed in a follow-up release.

Shipped: **clean key rename, no bridge**. User direction during implementation was explicit ("no bridge features ... a clean break"). The legacy keys are gone in v0.8.6.

Consequences: any external MCP consumer that read `.tags` / `.sources` / `.creators` / `.recent` will silently receive `null` on v0.8.6 until they update to `.results`. No in-tree consumer relied on the legacy keys, so the only blast radius is third-party scripts the user has not catalogued.

### Phase 2: `domain_brief.unread_count`

Spec said: fix `domain_brief.unread_count: null` by adding `unwrap_or(0)` in the response builder.

Shipped: nothing. The Architect implementation audit retracted this finding after empirical re-review. `DomainBrief.unread` is `u64` (vault/src/search.rs:1851), populated by a `SELECT COUNT(*)` which always yields a non-null integer; the response builder serializes it as `"unread": brief.unread`, never `null`. The shakedown report's `null` symptom came from `jq .unread_count` against a response whose actual key is `unread` (no `_count` suffix) — a key-name mismatch on the consumer side, not a code defect at the location the design specified.

The `oracle/src/server.rs::domain_brief_returns_results_key` test added in the post-audit cleanup commit asserts `unread` is always a number, locking in the (already correct) shape.

### Phase 7: Lifecycle change shipped without the soak gate

Spec said: 7b (long-lived model in daemon) ships only if `vault/tests/candle-bounded.rs` confirms candle's per-instance scratch state is bounded across 1000 varied batches.

Shipped: 7a and 7b together, with the synthetic test added as a regression guard rather than a pre-flight gate (per `[[feedback-no-phase-gating]]` — "ship the whole roadmap back-to-back"). The test exists at `vault/tests/candle-bounded.rs`, is `#[ignore]`'d (costs ~1 minute of CPU + ~100 MB model download), and asserts the post-load RSS does not grow more than 200 MB over 1000 mixed-length batches. Run on demand with `cargo test --release -p vault --features vec-candle --test candle-bounded -- --ignored --nocapture`.

If the test ever fails, the lifecycle change in `cortex::daemon` is the suspect: reverting to per-tick `load_active_model` is the rollback.

### Phase 1b: bootstrap-time vault-root resolution

Spec said: `vault::paths::resolve_vault_root` is strict — no vault, no run.

Shipped: that's the runtime behavior, but `sb bootstrap`'s daemon-install path additionally falls back to CWD when the resolver errors. Rationale: `sb bootstrap` on a fresh machine needs to write the systemd unit before the user has set `root-path`; the daemon itself re-resolves via `--vault` (which the unit hardcodes) at start time, so the bootstrap-time CWD fallback never leaks into runtime behavior. The fallback is local to `sb/src/cli/bootstrap.rs::register_systemd_units` and noted in a comment.

## References

- `docs/shakedown-sb-v0.8.5.md` - the shakedown report that produced this list
- `docs/design/2026-05-19-cortex-embed-memory-bounding.md` - parent of Phase 7 (Phase 1 of bounding shipped in v0.8.1)
- `docs/design/2026-05-19-unified-sb-binary.md` - the v0.8 unified-bin work; Phase 1 of this doc is the logical "Phase B" of that one (configs after logs)
- Memory: `[[project-cortex-embed-memory-leak]]`, `[[feedback-no-known-leaks-on-main]]`, `[[reference-otto-deploy]]`, `[[feedback-self-contained]]`
- `vault/src/logging.rs` and `sb/src/logger.rs` - the existing unified logging layer is the template for Phase 1's path module
