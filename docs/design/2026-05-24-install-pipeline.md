# Design Document: Self-Contained Install Pipeline

**Author:** Scott Idler
**Date:** 2026-05-24
**Status:** Implemented
**Review Passes Completed:** 4/4
  - Pass 1 (Draft): trace-driven; shaped phases around the actual public-consumer install path.
  - Pass 2 (Correctness): caught the originally-missed dependency on Daniel Miessler's external fabric pattern library. Added it as a separate row in the Data Model + Phase 5 doctor check + README install step.
  - Pass 3 (Edge cases): caught the precondition-vs-bootstrap-ordering issue (cortex hard-fail must live in `start_watching`, not `daemon::run`); expanded Phase 3 to also validate parse-ability of canonical-tags / tag-mapping (a malformed YAML today is silent-degrade just like a missing file); added macOS/Windows path fragility and Go-toolchain prerequisite to the Risks table.
  - Pass 4 (Architect review applied): the Architect challenged the scope and found three load-bearing defects: (a) `distillers::FabricShell::call` shells `vault::fabric::run_pattern` DIRECTLY (`distillers/src/fabric.rs:70`), bypassing `borg::fabric` — `vault::fabric::resolve_pattern` is the *only* resolver on the distill path, so its legacy `~/.config/borg/patterns/` hardcoding was not "becoming dead code" as my Open Question claimed but was actively breaking on fresh installs; (b) `cortex::fabric::run_pattern` (`cortex/src/fabric.rs:13-20`) does no local resolution and ignores `config.fabric.binary`, severing cortex from the unified patterns; (c) `sb cortex sweep` / `migrate` / `intel` / `classify` all bypass the daemon's `start_watching` precondition and hit `CanonicalTagsFile::load` directly with opaque `wrap_err` messages that never mention `sb bootstrap`. All three citations verified against the codebase at exact file:line. The vault::fabric cleanup is now in Phase 1 (no longer deferred); cortex pattern resolution is unified in Phase 1 (cortex::fabric::run_pattern becomes a thin wrapper around vault::fabric::run_pattern that respects config.fabric.binary); the precondition is consolidated into a `validate_canonical_assets()` helper called from every consumer entry point, not just the daemon (Phase 3). README's Phase 7 gains a Prerequisites section that calls out the Rust and Go toolchains explicitly.

## Summary

`sb bootstrap` is incomplete: it writes the three `.yml` templates and stops, leaving the canonical-tag vocabulary and 14 fabric distill patterns absent. Borg runs anyway in a silently-degraded mode (tag filtering disabled, distill falls through to Daniel Miessler's external `fabric` CLI). The author's working install only exists because `otto deploy` does an ad-hoc `cp` to legacy paths (`~/.config/borg/patterns/`, `~/.config/second-brain/`) that the daemons happen to also have populated from history. No public consumer following `cargo install --git ... --bin sb && sb bootstrap` gets a working install. The fix: embed all canonical assets in the `sb` binary, have `sb bootstrap` extract them, make borg/cortex hard-fail when they are missing, and restore drift detection in `sb doctor` against the embedded source-of-truth.

## Problem Statement

### Background

#### Trace of what a public consumer actually experiences

Walking the install path end-to-end against the current code (`sb` v0.8.24):

| Step | What the consumer does                                                   | What actually happens                                                                                          | Gap                                                                                                                                                                  |
| ---- | ------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 0    | Reads `README.md`                                                        | Two lines: `# second-brain / Rust Workspace Project with all of my Second Brain Machinery`                     | No documented install path. Operator has to read `CLAUDE.md` (an AI-instruction file), the workspace `Cargo.toml`, or grep the source.                               |
| 1    | `cargo install --git github.com/scottidler/second-brain --bin sb`        | `~/.cargo/bin/sb` lands.                                                                                       | OK.                                                                                                                                                                  |
| 2    | `sb bootstrap`                                                           | Writes `borg.yml`, `cortex.yml`, `oracle.yml` templates to `~/.config/sb/`. Installs systemd units. Prefetches embedding model. | Does NOT write `canonical-tags.yml`, `tag-mapping.yml`, `tag-proposals.yml`, or any of the 14 `patterns/*.md` files.                                                  |
| 3    | Edits `borg.yml` (uncomments telegram, adds bot token, etc.)             | OK.                                                                                                            | OK.                                                                                                                                                                  |
| 4    | `systemctl --user start borg cortex`                                     | Daemons start. Borg logs `WARN: Could not load canonical tags. Tag filtering disabled.`                        | **Silent degrade.** Ingestion works but produces uncontrolled tags. Distill falls through to fabric's own pattern resolution, succeeding only if the operator has separately run `fabric -y --update-patterns`. |
| 5    | (Optional) Operator uncomments `signal:` block; tries `signal-rs link`   | `signal-rs: command not found`.                                                                                | Nothing in `sb bootstrap` or any docs tells the operator to `cargo install --git github.com/scottidler/signal-rs --bin signal-rs --tag v0.2.1` first.                |
| 6    | `sb doctor`                                                              | Reports systemd active, configs parse, telegram bot reachable. ✅ Mostly green.                                | Has no check for the missing canonical-tags, missing patterns, or missing signal-rs CLI. The drift checks dropped in `d97e0aa` were CWD-relative and broken anyway.  |

#### Why the author's machine works

`.otto.yml:240,245` does an ad-hoc post-build `cp -f borg/patterns/*.md "$HOME/.config/borg/patterns/"` and `cp -f config/*.yml "$HOME/.config/second-brain/"`. These are **legacy paths from before the 2026-05-19 unified-sb-binary refactor**. The currently-running daemons read from `~/.config/sb/patterns/` and `~/.config/sb/` (the unified paths). The author's unified paths got populated by historical accident: an early `sb bootstrap` plus manual sync, captured in dotfiles symlinks. Both legacy and unified directories now coexist on the author's machine; the system works because the unified paths got there first and never got overwritten.

This is a one-machine accident. Every public consumer following the documented (such-as-it-is) install verb gets a half-installed system that silently degrades.

#### Why our review process didn't catch the gap

The Architect review on `docs/design/2026-05-24-signal-state-dir-internalization.md` verified the narrow claim "removing `state_dir` is safe" and concluded the design was sound. Neither the doc nor the review asked the broader question "is the install pipeline coherent for a non-author consumer?" — that question wasn't in scope. The doc-and-review pipeline catches errors *within* a stated scope; it does not catch errors *of* scope. This memo is the corrective: the scope should have been "make the install pipeline self-contained," with `state_dir` internalization as one slice of it.

### Problem

`sb` is structurally a single-binary tool with a self-bootstrap verb, but `sb bootstrap` ships only ~40% of the canonical assets borg/cortex/oracle need at runtime. The missing assets (`canonical-tags.yml`, `tag-mapping.yml`, `tag-proposals.yml`, 14 `patterns/*.md`) are provisioned on the author's machine by ad-hoc `cp` in `.otto.yml` to the WRONG paths (legacy `~/.config/borg/patterns/` and `~/.config/second-brain/`), and the daemons read from the right paths (`~/.config/sb/`) by historical accident.

Three concrete failure modes for public consumers:

1. **Silent-degrade ingest.** Missing canonical-tags / mapping is a `log::warn!` in `borg/src/pipeline.rs:55-66`, not an error. Tags ingest unfiltered. The vault accumulates junk tags that violate the canonical contract every other subsystem depends on.
2. **Distill path-dependent (two pattern collections, only one bootstrapped).** Two distinct sets of fabric patterns are load-bearing in this system. The first is our 14-file `borg/patterns/` set (`distill-article.md`, `distill-video.md`, etc.) consumed by borg's Stage-2 pipeline. The second is Daniel Miessler's ~200-file default fabric pattern library (`extract_wisdom`, `summarize`, `create_tags`) consumed by both borg's pipeline AND cortex (cortex/src/config.rs:407 sets `fabric_patterns: ["extract_wisdom", "summarize"]` as defaults). Three pattern resolvers exist: `borg::fabric::resolve_pattern` (looks at `~/.config/sb/patterns/`), `vault::fabric::resolve_pattern` (looks at the LEGACY `~/.config/borg/patterns/` — a fossil from before the unified-sb refactor), and `cortex::fabric::run_pattern` (does no local resolution, passes bare names to the fabric CLI). The two pattern *collections* require two install steps: `sb bootstrap` for our custom set, and `fabric -y --update-patterns` for Daniel Miessler's set. Today's install path documents neither.
3. **Signal CLI off-radar.** The `signal:` block in `borg.yml.example` directs operators to run `signal-rs link --state-dir <path>` but never tells them where that binary comes from.

The drift checks I deleted in `d97e0aa` (`shared_config_findings` and `pattern_findings`) were attempting the right kind of check (installed-copy vs source-of-truth) but anchored at the wrong source (`./config/`, `./borg/patterns/` relative to CWD) — meaningful only if the operator runs `sb doctor` from inside a clone of the repo. They were broken for everyone including the author.

### Goals

- Public consumer flow: `cargo install --git github.com/scottidler/second-brain --bin sb && sb bootstrap` produces a fully-functional install. No additional manual `cp`, no ad-hoc deploy scripts, no need to clone the repo.
- All canonical assets the daemons depend on (config templates, shared config, patterns) live inside the `sb` binary via `include_str!` and are extracted by `sb bootstrap` to `~/.config/sb/`.
- Borg and cortex hard-fail at startup when canonical assets are missing, with an error message that names `sb bootstrap` as the fix. No more silent-degrade.
- `sb doctor` compares installed copies against the binary's embedded source-of-truth. Drift detection works from any CWD, on any machine, without a repo present.
- `.otto.yml` pivots from ad-hoc `cp` to invoking `sb bootstrap --force` (or equivalent); becomes thin wrapper around the same install path the public consumer uses.
- `sb doctor` detects the absence of the `signal-rs` CLI binary and emits an actionable error with the exact `cargo install --git --tag` command.
- `README.md` gets a one-page install section documenting the canonical sequence end-to-end.

### Non-Goals

- Not publishing `sb` or `signal-rs` to crates.io. Git-tag pinning stays per `feedback-self-contained` (second-brain owns its install paths; not coupling to fabric's directory conventions).
- Not distro-packaging (no Homebrew formula, no apt/dnf, no Nix). `cargo install` remains the single supported install verb.
- Not auto-installing the `signal-rs` CLI binary as part of `sb bootstrap`. The operator chooses Signal transport explicitly; auto-installing a network-CLI binary they didn't ask for is overreach. Doctor surfaces the absence; the operator runs the explicit command.
- Not changing the per-machine config layer. Operator still hand-edits `~/.config/sb/borg.yml` after `sb bootstrap` to wire transports and secrets. The templates ship with everything commented and the operator opts in.
- Not migrating away from the existing legacy `~/.config/borg/patterns/` and `~/.config/second-brain/` directories on the author's machine in this change. They're orphans; the existing `sb bootstrap --migrate` flag (`sb/src/cli/bootstrap/migrate.rs`) covers legacy → unified migration, and adding `--prune-legacy-config` is tracked separately per the memory note on `project-deploy-debt`.
- Not changing how borg / cortex / oracle resolve canonical-tag locations at runtime; `vault::paths::canonical_tags()` etc stay as the source of truth. Only what `sb bootstrap` *writes* to those paths changes.

## Proposed Solution

### Overview

The `sb` binary becomes the single source of truth for every canonical asset under `~/.config/sb/`. Asset bytes are embedded at compile time via `include_str!`. `sb bootstrap` extracts them. Daemons refuse to start if the assets are missing. Doctor verifies the installed copies match what the binary expects.

`otto deploy` becomes a passthrough: build the binary, install, run `sb bootstrap --force` to refresh extracted assets, restart daemons. Same code path as a public consumer's first install.

### Architecture

Before:

```
sb binary                       ~/.config/sb/                  daemon at runtime
─────────                       ─────────────                  ─────────────────
include_str!:                   borg.yml      (template)  ──┐
  - borg.yml.example            cortex.yml    (template)  ──┼─→ daemons read
  - cortex.yml.example          oracle.yml    (template)  ──┘    these. OK.
  - oracle.yml.example
                                canonical-tags.yml   ←─ from where? otto deploy
                                tag-mapping.yml      ←─ from where? otto deploy
                                tag-proposals.yml    ←─ from where? otto deploy
                                patterns/*.md (14)   ←─ from where? otto deploy
                                                          (and otto deploy writes
                                                           to LEGACY paths, not
                                                           these unified paths)
```

After:

```
sb binary                       ~/.config/sb/                  daemon at runtime
─────────                       ─────────────                  ─────────────────
include_str!:                   borg.yml      (template)  ──┐
  - borg.yml.example            cortex.yml    (template)  ──┼─→ daemons read
  - cortex.yml.example          oracle.yml    (template)  ──┤    these. OK.
  - oracle.yml.example          canonical-tags.yml         ──┤
  - canonical-tags.yml          tag-mapping.yml            ──┤
  - tag-mapping.yml             tag-proposals.yml          ──┤
  - tag-proposals.yml           patterns/*.md (14)         ──┘
  - patterns/distill-*.md (14)         ↑
                                       │
                              sb bootstrap extracts all of them
                              (write-if-missing; --force overwrites)
```

The daemon-side change: at startup, `borg::lib::serve_init` validates that `vault::paths::canonical_tags()` exists, returns `eyre::bail!` with `run \`sb bootstrap\` to provision canonical assets` if missing. Same for the patterns directory.

Doctor's drift checks compare installed file bytes against embedded constants. No CWD anchor, no repo assumption.

### Data Model

No new persistent state. The binary's `include_str!`-d constants are the new source of truth for canonical assets at install time. Once extracted, the operator owns the installed copies (free to edit; doctor will flag the drift).

Schema of what gets embedded vs what gets installed:

| Embedded constant                | Extracted to                                    | Operator-editable? | Doctor behavior on drift                |
| -------------------------------- | ----------------------------------------------- | ------------------ | --------------------------------------- |
| `BORG_TEMPLATE`                  | `~/.config/sb/borg.yml`                         | Yes (per-host)     | None — template diverges immediately    |
| `CORTEX_TEMPLATE`                | `~/.config/sb/cortex.yml`                       | Yes (per-host)     | None — template diverges immediately    |
| `ORACLE_TEMPLATE`                | `~/.config/sb/oracle.yml`                       | Yes (per-host)     | None — template diverges immediately    |
| `CANONICAL_TAGS_YML`             | `~/.config/sb/canonical-tags.yml`               | Yes (operator may tune vocabulary) | Info finding: "drifted from binary; OK if intentional" |
| `TAG_MAPPING_YML`                | `~/.config/sb/tag-mapping.yml`                  | Yes (operator may add mappings)   | Info finding (same)                     |
| `TAG_PROPOSALS_YML`              | `~/.config/sb/tag-proposals.yml`                | Yes (cortex writes here)          | Info finding (same)                     |
| `PATTERNS[]` (14 entries)        | `~/.config/sb/patterns/<name>.md` (14 files)    | Yes (tuning prompts is normal)    | Warn finding: "drifted from binary; sb bootstrap --force to refresh" |

External dependencies (NOT embedded; verified by doctor, not bootstrap):

| Asset                                            | Provisioned by                                  | Doctor behavior on absence                    |
| ------------------------------------------------ | ----------------------------------------------- | --------------------------------------------- |
| `fabric` CLI binary on `PATH`                    | Operator installs Daniel Miessler's fabric      | Error: "install via `go install ... fabric`"  |
| Daniel Miessler's default patterns in `~/.config/fabric/patterns/` (~200 files including `extract_wisdom`, `summarize`, `create_tags`) | Operator runs `fabric -y --update-patterns`     | Error: "run `fabric -y --update-patterns`"    |
| `signal-rs` CLI binary on `PATH`                 | Operator installs via `cargo install --git`     | Error: install command surfaces in `sb doctor signal` |

The asymmetry: our custom 14 patterns get embedded because they ship with this repo and have no other source. Daniel Miessler's defaults are a separate upstream project (`danielmiessler/fabric`); embedding them in our binary would duplicate +200 files we don't author and couple our release cadence to theirs. The right design is: bootstrap installs OURS; doctor verifies THEIRS is present and actionable when not.

The `Info`-vs-`Warn` distinction: the three .yml templates are explicit per-host config; drift IS the expected state. The shared YAMLs are vocabulary the operator may tune; drift is allowed but worth flagging. The patterns are LLM prompts that are version-sensitive; drift from binary means "your binary has been bumped but you never refreshed your patterns" which is more often a bug than a feature.

### API Design

#### New embedded constants in `sb/src/cli/bootstrap.rs`

```rust
// Existing.
const BORG_TEMPLATE: &str = include_str!("../../../config/templates/borg.yml.example");
const CORTEX_TEMPLATE: &str = include_str!("../../../config/templates/cortex.yml.example");
const ORACLE_TEMPLATE: &str = include_str!("../../../config/templates/oracle.yml.example");

// New.
const CANONICAL_TAGS_YML: &str = include_str!("../../../config/canonical-tags.yml");
const TAG_MAPPING_YML: &str = include_str!("../../../config/tag-mapping.yml");
const TAG_PROPOSALS_YML: &str = include_str!("../../../config/tag-proposals.yml");

const PATTERNS: &[(&str, &str)] = &[
    ("condense.md", include_str!("../../../borg/patterns/condense.md")),
    ("distill-article.md", include_str!("../../../borg/patterns/distill-article.md")),
    ("distill-image.md", include_str!("../../../borg/patterns/distill-image.md")),
    // ... 11 more entries, one per file in borg/patterns/
];
```

Listing the 14 patterns explicitly (rather than using `include_dir!` which would require a new crate dependency and pulls in directory traversal) makes "what's bundled" a code change you have to acknowledge. Adding a new pattern means adding to this list; removing one means removing it. No surprises.

Doctor reads these same constants via a `pub(crate)` accessor (`sb::cli::bootstrap::embedded_assets()`) to do the drift comparison.

#### Extended `sb bootstrap` behavior

```rust
// Existing: writes if missing, skips if present.
let targets = [
    ("borg", vault::paths::borg_config(), BORG_TEMPLATE),
    ("cortex", vault::paths::cortex_config(), CORTEX_TEMPLATE),
    ("oracle", vault::paths::oracle_config(), ORACLE_TEMPLATE),
];
for (name, path, template) in &targets {
    write_if_missing(name, path, template)?;
}

// New: shared YAMLs.
let shared = [
    ("canonical-tags", vault::paths::canonical_tags(), CANONICAL_TAGS_YML),
    ("tag-mapping", vault::paths::tag_mapping(), TAG_MAPPING_YML),
    ("tag-proposals", vault::paths::tag_proposals(), TAG_PROPOSALS_YML),
];
for (name, path, contents) in &shared {
    write_if_missing(name, path, contents)?;
}

// New: patterns directory.
std::fs::create_dir_all(vault::paths::patterns_dir())?;
for (filename, contents) in PATTERNS {
    let path = vault::paths::patterns_dir().join(filename);
    write_if_missing(filename, &path, contents)?;
}
```

`--force` flag changes `write_if_missing` → `write_always` for the shared YAMLs and patterns only. Templates always stay write-if-missing because the operator's per-host config is in them.

#### Hard-fail discipline via a shared `validate_canonical_assets()` helper

Per the Architect's Pass 4 finding, the precondition cannot live only at `borg::serve_init` — `sb cortex sweep`, `sb cortex intel`, `sb cortex classify`, and the migrate/scan-proposals entry points all bypass the daemon's startup. Inlined checks at each site would drift; consolidate into a single helper per crate.

```rust
// borg/src/startup.rs (or borg/src/lib.rs)
pub fn validate_canonical_assets() -> Result<()> {
    let canonical = vault::paths::canonical_tags();
    if !canonical.exists() {
        eyre::bail!(
            "missing canonical-tags vocabulary at {}\nrun `sb bootstrap` to provision (or `sb bootstrap --force` to refresh from the binary's embedded copy)",
            canonical.display()
        );
    }
    CanonicalTagsFile::load(&canonical).map_err(|e| eyre::eyre!(
        "canonical-tags vocabulary at {} failed to parse: {e}\nrun `sb bootstrap --force` to restore from the binary's embedded copy",
        canonical.display()
    ))?;

    let mapping = vault::paths::tag_mapping();
    if !mapping.exists() {
        eyre::bail!(
            "missing tag-mapping at {}\nrun `sb bootstrap`",
            mapping.display()
        );
    }
    canonical::load_tag_mapping(&mapping).map_err(|e| eyre::eyre!(
        "tag-mapping at {} failed to parse: {e}\nrun `sb bootstrap --force`",
        mapping.display()
    ))?;

    let patterns = vault::paths::patterns_dir();
    if !patterns.is_dir() {
        eyre::bail!(
            "missing fabric patterns directory at {}\nrun `sb bootstrap`",
            patterns.display()
        );
    }
    Ok(())
}
```

`cortex::startup::validate_canonical_assets()` mirrors the above but omits the patterns-dir check (cortex sweep/intel/classify use canonical-tags + tag-mapping but not the patterns directly — patterns are consumed via cortex::fabric::run_pattern → vault::fabric::run_pattern, which has its own error path for missing patterns).

Every consumer entry point calls the helper as its FIRST statement before any other work:

| Crate  | Entry point                                          | File:line                       |
| ------ | ---------------------------------------------------- | ------------------------------- |
| borg   | `serve_init` (daemon)                                | borg/src/lib.rs:150             |
| cortex | `daemon::start_watching` (daemon main loop)          | cortex/src/daemon.rs:67         |
| cortex | `sweep::run` (`sb cortex sweep`)                     | cortex/src/sweep.rs:45          |
| cortex | `sweep::migrate`                                     | cortex/src/sweep.rs:133         |
| cortex | `sweep::scan_proposals`                              | cortex/src/sweep.rs:180         |
| cortex | `intel::run` (`sb cortex intel`)                     | cortex/src/intel.rs:30          |
| cortex | `classify::run` (`sb cortex classify`)               | cortex/src/classify.rs:24       |

NOT called from `cortex::daemon::run` (cortex/src/daemon.rs:44) because that handles `--install` and `sb bootstrap` itself goes through that path before the canonical assets are written.

An operator who genuinely wants no canonical filtering writes an empty `canonical-tags.yml` (zero-tag vocabulary). The hard-fail catches "you forgot to bootstrap," not "you chose zero tags."

#### Restored drift detection in `sb/src/cli/checks.rs`

```rust
fn shared_config_findings() -> Vec<Finding> {
    let installed_dir = vault::paths::config_root();
    let mut findings = Vec::new();
    for (filename, expected) in &[
        ("canonical-tags.yml", bootstrap::CANONICAL_TAGS_YML),
        ("tag-mapping.yml", bootstrap::TAG_MAPPING_YML),
        ("tag-proposals.yml", bootstrap::TAG_PROPOSALS_YML),
    ] {
        let path = installed_dir.join(filename);
        match std::fs::read_to_string(&path) {
            Ok(actual) if actual.as_str() == *expected => {
                findings.push(Finding::ok(format!("{filename} matches binary")));
            }
            Ok(_) => {
                findings.push(Finding::info(format!(
                    "{filename} differs from binary (operator edit or stale binary?)"
                )));
            }
            Err(_) => {
                findings.push(Finding::error(
                    format!("{filename} missing at {}", path.display()),
                    "sb bootstrap",
                ));
            }
        }
    }
    findings
}

fn pattern_findings() -> Vec<Finding> {
    let installed_dir = vault::paths::patterns_dir();
    let mut findings = Vec::new();
    let mut drift = 0usize;
    let mut missing = 0usize;
    for (filename, expected) in bootstrap::PATTERNS {
        let path = installed_dir.join(filename);
        match std::fs::read_to_string(&path) {
            Ok(actual) if actual.as_str() == *expected => {}
            Ok(_) => drift += 1,
            Err(_) => missing += 1,
        }
    }
    if missing > 0 {
        findings.push(Finding::error(
            format!("{missing} of {} patterns missing", bootstrap::PATTERNS.len()),
            "sb bootstrap",
        ));
    }
    if drift > 0 {
        findings.push(Finding::warn(
            format!("{drift} of {} patterns drifted from binary", bootstrap::PATTERNS.len()),
            "sb bootstrap --force to refresh (or accept the local edits)",
        ));
    }
    if drift == 0 && missing == 0 {
        findings.push(Finding::ok(format!("{} patterns match binary", bootstrap::PATTERNS.len())));
    }
    findings
}
```

#### Signal-rs CLI detection in `signal_findings_for`

After the existing host / state_dir / probe checks:

```rust
match std::process::Command::new("signal-rs").arg("--version").output() {
    Ok(out) if out.status.success() => {
        let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
        findings.push(Finding::ok(format!("signal-rs CLI installed: {version}")));
    }
    _ => {
        findings.push(Finding::error(
            "signal-rs CLI not found on PATH",
            "cargo install --git https://github.com/scottidler/signal-rs --bin signal-rs --tag v0.2.1",
        ));
    }
}
```

#### `.otto.yml` pivot

Replace the post-build `cp` block (lines 235-245) with a single `sb bootstrap --force` invocation:

```yaml
# Before: ad-hoc cp to legacy paths.
mkdir -p "$HOME/.config/borg/patterns"
cp -f borg/patterns/*.md "$HOME/.config/borg/patterns/"
mkdir -p "$HOME/.config/second-brain"
cp -f config/*.yml "$HOME/.config/second-brain/"

# After: same code path the public consumer uses.
sb bootstrap --force --skip-systemd --skip-prefetch-model
```

`--skip-systemd` because the deploy task already restarts daemons explicitly. `--skip-prefetch-model` because the model is already cached on the author's machine. A first-machine bootstrap (the public consumer's case) runs `sb bootstrap` with no flags.

#### `README.md` install section

Replace the two-line README with an install section that documents the canonical sequence end-to-end:

```markdown
# second-brain

[brief description ...]

## Install

```bash
# 1. Install sb (this repo's single binary)
cargo install --git https://github.com/scottidler/second-brain --bin sb

# 2. Install Daniel Miessler's fabric (external dep; provides extract_wisdom,
#    summarize, create_tags, etc.)
go install github.com/danielmiessler/fabric/cmd/fabric@latest
fabric -y --update-patterns

# 3. Provision sb's canonical assets, install systemd units, prefetch embeddings
sb bootstrap
```

Bootstrap drops config templates, shared vocabulary, and 14 fabric distill
patterns into `~/.config/sb/`. It also installs the borg and cortex systemd
units and prefetches the embedding model (~100 MB).

Edit `~/.config/sb/borg.yml` to wire transports (Telegram, Signal, Discord,
desktop notifications). The template ships with every transport commented out
so you opt in to what you need.

Start the daemons:

```bash
systemctl --user start borg cortex
sb doctor   # verify every section reports green
```

### Optional: Signal transport

```bash
cargo install --git https://github.com/scottidler/signal-rs --bin signal-rs --tag v0.2.1
systemctl --user stop borg
signal-rs link --name borg --state-dir ~/.local/share/sb/borg/signal-state/
# Scan the QR with the primary phone (Settings → Linked Devices → +).
# Uncomment the signal: block in ~/.config/sb/borg.yml, set host: <this-machine>.
systemctl --user start borg
sb doctor   # signal section should now show "linked"
```
```

(Body of the actual README will be longer but this is the install slice.)

### Implementation Plan

#### Phase 1: Embed canonical assets + unify pattern resolution
**Model:** sonnet

Two parallel changes, both load-bearing and both required before Phase 2 has any value:

**1a — embed assets in the sb binary.**

- Add three new `include_str!` constants in `sb/src/cli/bootstrap.rs`: `CANONICAL_TAGS_YML`, `TAG_MAPPING_YML`, `TAG_PROPOSALS_YML`, each pointing at the corresponding `config/<name>.yml` file.
- Add a `PATTERNS: &[(&str, &str)]` array with one tuple per `borg/patterns/*.md` (14 entries). Verify the count matches `ls borg/patterns/*.md | wc -l` before committing.
- Make the four new constants `pub(crate)` (and re-export from the bootstrap module) so `sb::cli::checks` can read them for drift detection.

**1b — unify pattern resolution.** Per the Architect's Pass 4 finding, three resolvers exist in the workspace and two are wrong:

- **Update `vault::fabric::resolve_pattern` (vault/src/fabric.rs:10-24)** to use `vault::paths::patterns_dir()` instead of the hardcoded legacy `~/.config/borg/patterns/`. **This is load-bearing, not cleanup**: `distillers::FabricShell::call` (distillers/src/fabric.rs:70) calls `vault::fabric::run_pattern` directly, bypassing `borg::fabric::resolve_pattern` entirely. On the distill path `vault::fabric::resolve_pattern` is the only resolver. Without this change, fresh installs crash at first distill because the patterns live at `~/.config/sb/patterns/` but vault::fabric looks at `~/.config/borg/patterns/`.

- **Rewrite `cortex::fabric::run_pattern` (cortex/src/fabric.rs:13-20)** as a thin wrapper around `vault::fabric::run_pattern`. Today it hardcodes `Command::new("fabric")`, does no local resolution, and ignores `config.fabric.binary`. After: cortex builds a `vault::fabric::run_pattern` call using `config.fabric.binary`, `config.fabric.model`, and `config.fabric.max_content_chars` (extending the cortex fabric config struct as needed to mirror borg's). Keep the function name `cortex::fabric::run_pattern` so the call sites in `cortex::autotag`, `cortex::classify`, `cortex::intel` don't need to change. This unifies pattern semantics across borg, cortex, and distillers: every fabric invocation in the workspace goes through `vault::fabric::run_pattern`, which goes through the updated `vault::fabric::resolve_pattern`, which reads from `~/.config/sb/patterns/`.

- After 1a + 1b, run `cargo build --workspace` and confirm the 17 `include_str!` paths resolve and the cortex/vault changes compile. Add a unit test on `vault::fabric::resolve_pattern` asserting it returns the canonical-path join for a bare-name input that exists at `vault::paths::patterns_dir()`.

#### Phase 2: Extend `sb bootstrap` to extract all canonical assets
**Model:** sonnet

- After the existing template extraction in `bootstrap::run`, extract the three shared YAMLs (canonical-tags, tag-mapping, tag-proposals) using the same `write_if_missing` helper. Resolve each path via `vault::paths::canonical_tags()` / `tag_mapping()` / `tag_proposals()`.
- Create `vault::paths::patterns_dir()` if it doesn't exist (`std::fs::create_dir_all`), then write-if-missing each entry in `PATTERNS`.
- Add a `--force` flag to `BootstrapArgs` that swaps `write_if_missing` for an always-write path for the shared YAMLs and patterns ONLY. Templates stay write-if-missing under `--force` because they hold per-host config the operator has edited.
- Refresh the bootstrap success message to enumerate what was written or skipped.

#### Phase 3: Hard-fail every consumer entry point on missing or malformed canonical assets
**Model:** sonnet

Per the Architect's Pass 4 finding, the precondition must fire at *every* consumer entry point, not just the daemon. Consolidate the check into a single helper invoked consistently.

- **Add `borg::startup::validate_canonical_assets() -> Result<()>`** (new module or add to existing `borg/src/startup.rs`) that:
  - Verifies `vault::paths::canonical_tags()` exists AND parses via `CanonicalTagsFile::load`.
  - Verifies `vault::paths::tag_mapping()` exists AND parses via `canonical::load_tag_mapping`.
  - Verifies `vault::paths::patterns_dir()` exists and is a directory.
  - Bails with an actionable error naming `sb bootstrap` (write-if-missing) or `sb bootstrap --force` (refresh from binary) as the fix. Error message includes the missing path verbatim so the operator can `ls` it themselves.

- **Borg call sites:** `borg::lib::serve_init` (borg/src/lib.rs:150) calls `validate_canonical_assets()` before the per-subsystem setup. The existing soft-fail in `borg/src/pipeline.rs:50-77` is removed (precondition makes it dead code; leaving it in invites future regressions).

- **Cortex helper:** add `cortex::startup::validate_canonical_assets() -> Result<()>` mirroring borg's (cortex doesn't need the patterns dir, only canonical-tags + tag-mapping). Invoked from:
  - `cortex::daemon::start_watching` (cortex/src/daemon.rs:67) — the daemon main loop. NOT in `cortex::daemon::run` (cortex/src/daemon.rs:44), which handles `--install`; that path must remain free of the precondition because `sb bootstrap` itself calls it before the canonical assets are written.
  - `cortex::sweep::run` (cortex/src/sweep.rs:45) — `sb cortex sweep`.
  - `cortex::sweep::migrate` (cortex/src/sweep.rs:133) — invoked from sweep subcommand.
  - `cortex::sweep::scan_proposals` (cortex/src/sweep.rs:180).
  - `cortex::intel::run` (cortex/src/intel.rs:30) — `sb cortex intel`.
  - `cortex::classify::run` (cortex/src/classify.rs:24) — `sb cortex classify`.
  - Any other one-shot command that reads canonical-tags or tag-mapping (grep `CanonicalTagsFile::load\|canonical::load_tag_mapping` and ensure every site is either inside one of these entry points or is itself a call site).

- **Tests:** unit test on each of borg's and cortex's `validate_canonical_assets` that:
  - Returns Ok when all three (or two) paths exist and parse.
  - Returns Err containing the substring `sb bootstrap` when any path is missing.
  - Returns Err containing the substring `sb bootstrap --force` (or similar refresh hint) when a file exists but fails to parse.
  - Add one integration test per entry point asserting the entry-point function returns the precondition error when canonical-tags is missing (parameterize over a `TempDir` with `$XDG_CONFIG_HOME` redirected).

#### Phase 4: Restore drift detection in `sb doctor`
**Model:** sonnet

- Re-add `shared_config_findings` and `pattern_findings` in `sb/src/cli/checks.rs` per the API design above.
- Restore both sections in `all_sections()`.
- Update the existing tests in `sb/src/cli/checks/tests` if they exist (or write new ones) to exercise: drift detected, missing file detected, exact match → ok finding.

#### Phase 5: External-binary detection in doctor
**Model:** sonnet

- Add a new doctor section `external_binaries` that shells out to verify each external CLI dependency:
  - `fabric --version` → Ok finding shows version; Error finding directs operator at the canonical install command (`go install github.com/danielmiessler/fabric/cmd/fabric@latest` per the upstream README).
  - `fabric -l` (list patterns) → parse output for `extract_wisdom`, `summarize`, `create_tags`. Missing-pattern Error directs at `fabric -y --update-patterns`.
  - `signal-rs --version` (only when `config.signal` is set) → Ok finding shows version; Error directs at `cargo install --git https://github.com/scottidler/signal-rs --bin signal-rs --tag v0.2.1`.
  - Optional polish: compare reported `signal-rs` version against the pinned `borg/Cargo.toml` dep; Warn on mismatch.
- These are operator-actionable defects, not borg-internal bugs. The doctor's job here is to make the actionable-ness explicit (exact command to paste).
- The fabric CLI check is unconditional (borg pipeline always uses fabric). The signal-rs CLI check is gated on `config.signal.is_some()` (don't pester an operator who never enabled Signal).

#### Phase 6: Pivot `.otto.yml` to invoke `sb bootstrap --force`
**Model:** sonnet

- Replace the post-build `cp` block in `.otto.yml:235-245` with `sb bootstrap --force --skip-systemd --skip-prefetch-model`.
- The systemd-restart block stays; bootstrap's `--skip-systemd` short-circuits unit registration but does NOT touch running daemons.
- Verify `otto deploy` on the author's machine produces the same end-state as before (compare `~/.config/sb/` before and after).
- The legacy `~/.config/borg/patterns/` and `~/.config/second-brain/` directories are NOT touched by this phase — orphans, cleaned up by a future `sb bootstrap --prune-legacy-config` per the existing tracked debt.

#### Phase 7: README install section
**Model:** sonnet

- Rewrite `README.md` per the API design above. Include FIVE blocks (Prerequisites was added in Pass 4 per the Architect's Go-toolchain finding):

  1. **Brief description** — one paragraph on what second-brain is.
  2. **Prerequisites** — explicitly list every external toolchain the consumer must have before `cargo install`:
     - Rust toolchain (`rustup` or distro package).
     - Go toolchain (for `go install` of Daniel Miessler's fabric). Link to fabric's pre-compiled releases page as an alternative for operators who don't want Go.
     - Optional: Firefox (for the capture extension).
  3. **Install** — the canonical three-step sequence: `cargo install` sb, `go install` fabric + `fabric -y --update-patterns`, `sb bootstrap`.
  4. **Configure and start daemons** — edit `~/.config/sb/borg.yml`, `systemctl --user start borg cortex`, `sb doctor` to verify.
  5. **Optional: Signal transport** — `cargo install --git ... signal-rs`, stop borg, `signal-rs link`, scan QR, uncomment signal block, restart, verify.

- Cross-link the design docs that operators might want to read after install: `docs/design/2026-05-24-signal-as-borg-transport.md` and `docs/design/2026-05-24-signal-state-dir-internalization.md` for Signal; this memo for the install pipeline; `CLAUDE.md` for architectural context.

## Alternatives Considered

### Alternative 1: Patch the existing `.otto.yml` to write to the right paths

- **Description:** Change `cp -f borg/patterns/*.md "$HOME/.config/borg/patterns/"` to write to `~/.config/sb/patterns/`. Same for shared config. Leaves `sb bootstrap` as-is.
- **Pros:** One-line fix. Trivial.
- **Cons:** Only fixes the author's machine. Public consumers still hit silent-degrade because they don't run `otto deploy`. CLAUDE.md still describes a system that doesn't match the code (the install verb is still incomplete). Drift detection in doctor still has no source-of-truth to compare against.
- **Why not chosen:** Treats the symptom, not the disease. The disease is that `sb bootstrap` is incomplete; the cure has to be there, not in `.otto.yml`.

### Alternative 2: Include all assets via `include_dir!` crate

- **Description:** Use the [`include_dir`](https://crates.io/crates/include_dir) crate to embed `borg/patterns/` and `config/` as directory trees. Iterate at runtime.
- **Pros:** No explicit per-file list. Adding/removing a file in the source tree automatically updates what's bundled.
- **Cons:** New dependency for marginal gain. "Automatic" inclusion is exactly the opposite of what we want — adding a new pattern should be a deliberate code change reviewable in a PR, not a side effect of touching a file in `borg/patterns/`. Explicit `include_str!` per file makes the bundled set visible at the language level and forces a code change to add or remove one.
- **Why not chosen:** YAGNI; explicit list is the better default.

### Alternative 3: Soft-fail with auto-bootstrap on first run

- **Description:** Keep the current soft-fail in `borg/src/pipeline.rs`. When canonical-tags is missing, instead of `log::warn!`, invoke `sb bootstrap` internally to provision and continue.
- **Pros:** No operator action needed. Daemon self-heals.
- **Cons:** Hides the install gap from the operator. A daemon that "fixes itself" makes diagnosis harder when something genuinely is wrong. The operator never learns that `sb bootstrap` is a verb they need to know about. And the daemon-as-installer pattern means startup time becomes variable (first run is slow because of model prefetch) and we'd have to handle systemd's startup-timeout semantics around it.
- **Why not chosen:** "Make the error message louder" beats "make the error invisible." Hard-fail with `sb bootstrap` in the message is the cleanest path.

### Alternative 4: Drop the canonical-tags vocabulary entirely; make tag normalization opt-in via config flag

- **Description:** Don't require canonical-tags / tag-mapping. Borg ingests whatever tags fabric produces; operator opts in to filtering via a config flag pointing at their own vocabulary file.
- **Pros:** Removes the install dependency entirely. Each install starts with no opinion on tags.
- **Cons:** Discards a load-bearing design decision from earlier — the 110-tag canonical vocabulary is what keeps cortex's sweep, oracle's tag-search, and the dashboard's stats coherent across the workspace. Optional-tag-filtering means every downstream consumer has to handle the no-canonical case, which is more code in more places than just shipping the vocabulary in the binary.
- **Why not chosen:** The canonical vocabulary is a design choice, not an accident; removing it would be a separate, much larger refactor with cross-system implications. Out of scope.

### Alternative 5: Ship a separate `second-brain-data` crate that provides the canonical assets

- **Description:** Move `borg/patterns/*.md` and `config/*.yml` into their own published crate. `sb` depends on it; doctor reads from it.
- **Pros:** Separation of "code" from "canonical content." Could be versioned independently.
- **Cons:** New crate to maintain. Adds a publishing step we don't currently have (nothing in the workspace is published). Doesn't actually decouple anything meaningful — the patterns and the canonical vocabulary co-evolve with borg's code (Phase 2 distillers consume the patterns; cortex sweep consumes the vocabulary). Independent versioning is theoretical complexity for a single-author project.
- **Why not chosen:** YAGNI. `include_str!` in the existing binary is the simplest thing that could possibly work.

## Technical Considerations

### Dependencies

- No new crates. Everything embedded uses `std::include_str!` (built-in macro).
- The `which` crate could simplify Phase 5's `signal-rs --version` shellout, but `std::process::Command` is enough for a one-shot.

### Performance

- **Binary size impact:** 14 patterns × ~5-10 KB each + 3 shared YAMLs × ~2-5 KB each = roughly **100-180 KB added to the `sb` binary**. Current `sb` release binary is ~50 MB; this is rounding noise (<0.5%). No measurable effect on `cargo install` time or daemon startup.
- **Bootstrap runtime:** 17 file writes total. Sub-millisecond. The bottleneck is still `sb bootstrap`'s embedding-model prefetch (~30s on first run, fully cached on subsequent runs).
- **Daemon startup:** Two `Path::exists()` checks added before the existing setup. Microsecond cost. No hot-path impact.
- **Doctor runtime:** 17 file reads + 17 byte-compares against in-memory constants. Bytes already in CPU cache from the embedded constants; reads are sequential. <10 ms total even on cold cache.

### Security

- **No new external network calls.** Bootstrap was already prefetching the embedding model; signal-rs CLI detection shells out to a binary already on the operator's PATH (no fetch).
- **Embedded assets are public.** `borg/patterns/*.md` and `config/*.yml` are already in the public repo. Embedding them in the binary changes nothing about who can see them; it just removes the runtime path-resolution step.
- **Hard-fail at daemon startup is a net security improvement.** Today's soft-fail means an operator who never ran `otto deploy` runs borg with disabled tag filtering — every ingested URL ends up in the vault with whatever tags fabric produces, including potentially leaking sensitive page content via tag names. Refusing to start until canonical-tags is provisioned closes that gap.
- **Signal-rs CLI detection runs a subprocess.** `signal-rs --version` is a safe invocation (no user-controlled input); the only failure modes are "binary not found" (handled) or "version output is non-UTF-8" (handled by `String::from_utf8_lossy`).

### Testing Strategy

- **Phase 1:** `cargo build -p sb` exercises every `include_str!` at compile time. If a path is wrong, the build fails. No runtime test needed for the bundling itself.
- **Phase 2:** Add unit tests on `bootstrap::run` that:
  - Run bootstrap into a `tempfile::TempDir` (override `vault::paths::config_root()` via `$XDG_CONFIG_HOME`).
  - Assert all 7 .yml files exist and their contents match the embedded constants byte-for-byte.
  - Assert all 14 patterns/*.md files exist.
  - Run bootstrap a second time; assert no files were rewritten (write-if-missing semantics).
  - Run bootstrap with `--force`; assert the shared YAMLs and patterns were rewritten (templates stayed).
- **Phase 3:** Add tests on `borg::serve_init` and `cortex::daemon::run` that:
  - Provide a config that resolves `canonical_tags()` to a nonexistent path.
  - Assert the function returns `Err` with a message containing `sb bootstrap`.
- **Phase 4:** Tests on `shared_config_findings` and `pattern_findings`:
  - Set up a tempdir with a known-good extract via `bootstrap::run`; doctor reports all OK.
  - Mutate one file; doctor reports the right Warn/Info finding for it.
  - Delete one file; doctor reports the Error finding.
- **Phase 5:** Test `signal_findings_for` against `PATH` mutations:
  - Set `$PATH` to a tempdir containing no `signal-rs`; doctor emits the install-suggestion Error.
  - Set `$PATH` to a tempdir containing a shell script that prints a fake version; doctor emits the Ok finding.
- **Phase 6:** Manual smoke on the author's machine: `otto deploy` after this change must produce a `~/.config/sb/` byte-identical (modulo edited templates) to before. Diff `find ~/.config/sb -type f | xargs md5sum | sort` before and after to verify.
- **End-to-end:** Spin up a throwaway `cargo install --git --bin sb` on a clean container (or wipe `~/.config/sb/` on a test machine), run `sb bootstrap`, verify `borg/cortex` start cleanly. This is the test the trace above documented as failing today.

### Rollout Plan

- Ship as a single bumped release; phases land back-to-back per `feedback-no-phase-gating` ([[feedback-no-phase-gating]]).
- The author's machine: `bump && otto deploy` runs `sb bootstrap --force` as part of deploy, idempotently catches up the unified `~/.config/sb/` paths. Legacy directories stay as orphans (tracked debt; future `--prune-legacy-config`).
- Other machines (laptop, secondary hosts): same `otto deploy` path. Bootstrap is idempotent; if their `~/.config/sb/` already has the assets at the right path (from earlier accident), bootstrap is a no-op modulo `--force`-driven refresh.
- Public consumers: get the new install path on next `cargo install --git`. README updated in the same commit so the docs are accurate from cut-over.
- The hard-fail in `borg::serve_init` and `cortex::daemon::run` is the riskiest change for the author's machine — if any of the canonical asset paths are subtly different than what bootstrap writes, the daemon refuses to start. Mitigate by running `sb bootstrap --force` BEFORE the daemon restart in `otto deploy`'s task ordering. The deploy task already restarts daemons last; just ensure bootstrap runs before the restart.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Hard-fail daemon startup breaks an existing operator who genuinely wanted unfiltered ingest | Low | Medium | Operator writes an empty canonical-tags.yml (zero-tag vocabulary). The hard-fail catches "missing file," not "empty file." Document this in the bail error message. |
| `include_str!` path is wrong; build breaks | Low | Low | Build-time check is exhaustive: if any of the 17 paths don't resolve, `cargo build` fails before anything ships. CI catches this. |
| Bumping binary version + operator-edited canonical-tags = drift warning every `sb doctor` | Medium | Low | The Info finding is informational, not an Error. Operators who tune canonical-tags accept the drift state. `sb bootstrap --force` is the explicit re-sync. |
| Signal-rs version drift between binary's pinned dep and operator's installed CLI | Low | Low | Phase 5's warning surfaces this. The current pinned tag (v0.2.1) is stable. |
| `.otto.yml` pivot breaks the author's existing deploy workflow | Low | Medium | `sb bootstrap --force --skip-systemd --skip-prefetch-model` is idempotent and substitutes for the current `cp` block byte-for-byte (after Phase 2 ships the right paths). Manual md5 diff before/after on first deploy validates. |
| README install instructions go stale as the project evolves | Medium | Low | The install instructions stay close to the `sb bootstrap` source-of-truth. When bootstrap behavior changes, the README has to change in the same PR. Add a smoke-test that exercises the README's canonical sequence verbatim in CI on a clean container (Phase 7 nicety, can be deferred). |
| The 14-pattern explicit list rots: someone adds a pattern to `borg/patterns/` and forgets to update `PATTERNS`-array in `bootstrap.rs` | Medium | Low | Add a `build.rs` assertion in `sb/build.rs` that `count(borg/patterns/*.md) == PATTERNS.len()` and fails the build if not. (Optional Phase 1 polish.) Otherwise CI tests in Phase 2 catch this — a new pattern file with no entry in `PATTERNS` means bootstrap won't write it, and the file's absence at `~/.config/sb/patterns/` will be flagged by Phase 4's doctor drift check (`missing` count) on the author's machine immediately. |
| Embedded constants increase compile time for downstream consumers of the `sb` crate as a library | Very Low | Negligible | `sb` is a binary, not consumed as a library by anything else in the workspace. Library crates (`borg`, `cortex`, `oracle`, `vault`, `distillers`) don't import from `sb`. |
| Hard-fail in `cortex::daemon::run` accidentally fires during `sb bootstrap` (which calls into cortex::daemon::run with `--install`) | Low | High | Phase 3 places the cortex precondition in `start_watching` (the daemon's main-loop entry), NOT in `run` (which handles `--install`). Bootstrap's install call path never touches the precondition. Tests in Phase 3 cover both directions: precondition fires on a real start, precondition skipped on `--install`. |
| Daniel Miessler's fabric CLI is required but undocumented; operators install `sb` and never realise they need fabric until first distill fails opaquely | High (today) → Low (with this design) | Medium | Phase 5 doctor section catches the absence with the exact install command. README install section in Phase 7 lists fabric as Step 2 of the canonical sequence. Hard-fail in Phase 3 doesn't help here directly (fabric's absence shows up at first distill, not at startup), but doctor + README close the gap. |
| `fabric -l` output format changes upstream; doctor's check breaks | Low | Low | Use substring containment (`stdout.contains("extract_wisdom")`) rather than full parsing. Robust to formatting changes; only sensitive to renames. |
| Operator on macOS or Windows hits the legacy `vault::fabric::resolve_pattern` fallback path (hardcoded `~/.config/borg/patterns/` is Linux-specific) | Low (no current non-Linux operators) | Low | Pre-existing condition; `vault::fabric::resolve_pattern` fallback becomes dead code once `~/.config/sb/patterns/` is reliably populated by bootstrap. Cleanup of that legacy resolver is tracked separately. |
| `go install` for fabric requires Go toolchain that the operator may not have | Medium | Low | README install section calls out Go as a prerequisite, OR links to fabric's own install docs which cover other install paths (Homebrew on macOS, etc.). |
| Distillers path crash on fresh install because `vault::fabric::resolve_pattern` still hardcodes the legacy path | **Was Medium-Critical in the original draft; eliminated by moving the vault::fabric cleanup into Phase 1 (Architect Pass 4).** | High | Phase 1's 1b step updates `vault::fabric::resolve_pattern` to use `vault::paths::patterns_dir()`. Unit test on the resolver asserts the new behavior. After this change, distillers (which calls `vault::fabric::run_pattern` directly per distillers/src/fabric.rs:70) resolves correctly from the canonical path. |
| `sb cortex sweep` / `migrate` / `intel` / `classify` bypass the daemon's precondition and emit opaque "failed to load canonical tags" errors on fresh installs | **Was Medium in the original draft; eliminated by Phase 3's consolidated `validate_canonical_assets()` helper (Architect Pass 4).** | Medium | Every cortex one-shot entry point calls `cortex::startup::validate_canonical_assets()` as its first statement. The opaque `wrap_err` sites at `cortex/src/sweep.rs:135,137,182,184` become unreachable for the missing-file case (still handle parse failures gracefully but those are caught by the precondition's parse check). |

## Open Questions

- [ ] Should the embedded `canonical-tags.yml` ship as a fully populated vocabulary (110 canonical tags, current state) or as a minimal seed that the operator extends? **Resolved:** ships fully populated. The vocabulary IS the design; operators tuning it is the exception, not the norm.
- [ ] Should Phase 6's `.otto.yml` change land alongside the binary changes, or as a follow-up? **Resolved:** same commit. Otherwise the author's next `otto deploy` runs against the new bootstrap with the old `.otto.yml` and double-syncs to both unified and legacy paths.
- [ ] Phase 5's signal-rs version match check: warn or info on mismatch? **Resolved:** warn. A mismatch is operator-actionable ("re-install signal-rs to match the binary") without being fatal.
- [ ] Should `sb bootstrap --force` overwrite the operator's edited `canonical-tags.yml`? **Resolved:** yes, that's the entire purpose of `--force`. The operator either accepts the drift (no `--force`) or accepts the refresh (with `--force`). No middle ground.
- [ ] Should the cleanup of `vault::fabric::resolve_pattern` (the legacy `~/.config/borg/patterns/` fallback) be in this PR or follow-up? **Originally resolved as "follow-up" — REVERSED in Pass 4 (Architect review).** `distillers::FabricShell::call` (distillers/src/fabric.rs:70) shells `vault::fabric::run_pattern` directly, so `vault::fabric::resolve_pattern` is the ONLY resolver on the distill path. Without updating it in this PR, fresh installs crash at first distill. Moved into Phase 1.
- [ ] Should `sb bootstrap` also bootstrap fabric and its default patterns, since they're a hard dependency? **Resolved:** no. Auto-installing a Go binary the operator didn't ask for is overreach. The doctor's Error finding with the exact install command is the right pattern — same posture as the signal-rs CLI handling.

- [ ] Should `cortex::fabric::run_pattern` be deleted or rewrapped? **Resolved (Pass 4):** rewrapped, not deleted. Keep the function name and signature stable so the call sites in `cortex::autotag`, `cortex::classify`, `cortex::intel` don't all need updating. The body becomes a thin call into `vault::fabric::run_pattern` using `config.fabric.binary`, `config.fabric.model`, `config.fabric.max_content_chars`. This is the smallest diff that satisfies the Architect's "delete it entirely" intent while keeping the surface stable.

- [ ] Should the canonical-asset precondition be inlined at every call site, or consolidated into a helper? **Resolved (Pass 4):** helper. `borg::startup::validate_canonical_assets()` and `cortex::startup::validate_canonical_assets()` are the single sources of truth for what "canonical assets are present and parseable" means. Every consumer entry point calls the helper as its first statement. Inline checks would drift over time as new entry points get added; a helper is a single point to maintain and test.

### Hardest question, by self-direction

- [ ] Is "mirror Telegram" the right load-bearing invariant for the operator surface, OR is the actual invariant "the operator surface holds only operator-load-bearing knobs," with Telegram-mirror being one consequence? If the latter, the rule generalizes beyond Signal — any future transport's config block, any future external-dep doctor section, any future bootstrap step should be audited against "is this a knob the operator can actually act on, or am I leaking internal state?" That's the lesson the prior retraction memo named but did not generalize. This memo extends the lesson to the install surface. The next memo should not require a third retraction to extend it again.

## References

- `docs/design/2026-05-24-signal-as-borg-transport.md` - parent design memo, retracted in part by the state-dir internalization memo and now extended by this install-pipeline memo.
- `docs/design/2026-05-24-signal-state-dir-internalization.md` - the prior retraction memo. This memo extends the same diagnostic pattern (Claude shipped one slice of a problem; the broader scope wasn't caught in review) to the install pipeline.
- `CLAUDE.md:84-88` - the (currently inaccurate) install documentation that this memo updates by fixing the underlying system rather than the docs.
- `sb/src/cli/bootstrap.rs` - existing bootstrap entry point that gains the 14 new constants + 7 new write_if_missing calls.
- `sb/src/cli/checks.rs:213-219` - the comment marking where `shared_config_findings` and `pattern_findings` used to live (removed in `d97e0aa` of v0.8.24). This memo restores them with a working source-of-truth.
- `borg/src/lib.rs:serve_init` - where the hard-fail precondition lands.
- `borg/src/pipeline.rs:50-77` - the soft-fail code path that becomes unreachable (or gets removed) under hard-fail.
- `cortex/src/sweep.rs` - cortex side of the same precondition.
- `.otto.yml:235-245` - the broken `cp` block that gets replaced by `sb bootstrap --force`.
- `README.md` - currently two lines; gets the install section.
- `vault/src/paths.rs:54-68` - canonical paths for `canonical-tags.yml`, `tag-mapping.yml`, `tag-proposals.yml`, `patterns/`.
- `feedback-signal-mirrors-telegram` - memory note saved during the prior retraction; this memo applies the same "consult the analog before adding a knob" discipline at a higher level (consult the install path before adding an install step).
