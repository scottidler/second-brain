# Design Document: Internalize Signal State Dir

**Author:** Scott Idler
**Date:** 2026-05-24
**Status:** Implemented
**Review Passes Completed:** 3/3 + Architect design review (1 round)

## Summary

The `state_dir: PathBuf` field on `SignalConfig` exposes a `signal-rs`-internal storage path through the operator-visible `borg.yml`, breaking the "Signal mirrors Telegram" invariant (Telegram exposes zero teloxide-internal paths). Remove the field, canonicalize the path inside borg via a new `vault::paths::signal_state_dir()` helper, and have every consumer (signal supervisor, doctor section, runbook) read from that one source.

## Problem Statement

### Background

#### What was shipped, what was wrong, and who shipped it

The v0.8.21 Signal transport (per `docs/design/2026-05-24-signal-as-borg-transport.md`) included `state_dir: PathBuf` as a field on `SignalConfig`, with a matching `state-dir:` line in the operator-visible `borg.yml`. **That field should not have existed.** This memo is the retraction.

Origin: during the original design pass, the field was lifted directly from `signal-rs`'s own CLI surface (`signal-rs link --state-dir`, `signal-rs status --state-dir`). The Claude pass that wrote that design treated `signal-rs`'s CLI argument set as the menu of fields borg's config should expose, and assumed without checking that "operator needs to run `signal-rs link --state-dir <here>`" implied "operator must configure that same path in borg." Neither implication holds: `signal-rs` exposes `--state-dir` because it's a general-purpose linked-device tool that can manage multiple identities under different directories; borg embeds `signal-rs` as a library and operates exactly one identity per host. A library consumer does not inherit its dependency's CLI argument set as its own operator surface.

The Architect's two-round design review on the parent memo focused on the privacy filter, rate gate, and supervisor wiring. It did not flag `state_dir` as leakage. The first time the leakage was named explicitly was during the live deploy, in the user-facing prompt: "why does borg need to know the state-dir? that seems like internal leakage from signal-rs to borg" and then "there is nothing similar for Telegram." Both observations are obviously correct in retrospect. They should have been the first question the design memo answered.

The trigger for the live deploy catching it was a separate, smaller bug: `SignalConfig::state_dir` is typed `PathBuf` and deserializes literally, so a `state-dir: ~/.local/share/...` line in YAML produced a path that `signal-rs::Client::open` could not open ("unable to open database file"). Fixing that bug ran into the deeper question of why the field was there at all. The deeper question has only one defensible answer: it shouldn't be.

The persistent fix is to delete the field, internalize the path inside borg, and add a feedback memory (already saved: `feedback-signal-mirrors-telegram`) so future Claude passes consult the Telegram analog before adding any new field to the Signal surface. The Telegram analog is the canonical "what does the operator see" reference; any divergence requires explicit justification.

#### Why "mirror Telegram" is the load-bearing invariant

Telegram's analog in borg.yml is the `telegram:` block:

```yaml
telegram:
  bot-token: TELEGRAM_BOT_TOKEN
  allowed-chat-ids: [8474692082]
  host: desk
```

Three fields, all operator-load-bearing: a secret reference, a privacy allowlist, a host pin. Zero teloxide-internal paths. teloxide does HTTP long-polling against api.telegram.org; whatever transient state it keeps lives in memory and is irrelevant to the operator.

Signal is different in that protocol-mandated state must persist (the Double Ratchet's per-conversation key chains, our identity keypair, prekeys uploaded to the server) - the `~/.local/share/sb/borg/signal-state/` SQLite database is real protocol state, not a `signal-rs` design indulgence. (AsamK's signal-cli does the same thing for the same reasons: `lib/src/main/java/.../storage/Database.java:98` opens `jdbc:sqlite:account.db?foreign_keys=ON&journal_mode=wal` - same engine, same WAL mode, same FK enforcement; every Signal client has equivalent persistence because the protocol demands it.) But the *path* to that database is an implementation detail of borg's chosen embedding, not an operator-load-bearing config choice. Exposing it propagates internal directory layout into the operator surface for no benefit.

### Problem

`SignalConfig::state_dir` is leakage of `signal-rs`-internal storage layout into the operator-visible `borg.yml`. Concrete failure modes:

1. **Tilde-expansion bug at runtime.** `state_dir: ~/.local/share/sb/borg/signal-state/` deserializes as a literal `~` path; `Client::open` fails with "unable to open database file." The first link attempt on the deploy machine crashed the signal subsystem cleanly (the supervisor isolates it) but only because the daemon's `select!` arm catches the bail-out. Tilde expansion in YAML config is a footgun this field invented.
2. **Two paths of truth.** The operator types the path twice: once on the `signal-rs link --state-dir <path>` command line, once into `borg.yml`. A mismatch (e.g. one uses a trailing slash, one doesn't; one uses tilde, one is absolute) produces a "not linked" doctor finding when the link actually succeeded.
3. **Footgun for the `signal-rs` CLI default.** The CLI default for `signal-rs link` is `~/.local/share/signal-rs/`. An operator who runs the link command without `--state-dir` lands at the CLI default; if they don't notice and don't update `borg.yml`, borg keeps looking at its configured path and reports "not linked." The v0.8.21 doctor section detected this with a collision warning, but the warning exists to paper over a problem the config field invented.
4. **Asymmetry with Telegram.** Telegram does not expose teloxide-internal paths in `telegram:`. Signal should not expose `signal-rs`-internal paths in `signal:`. The two transports are peers; their config surfaces should be the same shape modulo Signal-specific privacy backstops.

### Goals

- Remove `state_dir` from `SignalConfig` entirely.
- Canonicalize the path inside borg via `vault::paths::signal_state_dir()`, mirroring the `vault::receipts::receipts_db_path()` precedent.
- All four consumers (the `borg::signal::run` supervisor in `lib.rs`, the receive loop's `Client::open` call, the `sb doctor` section, the runbook) read from the same helper. One source of truth.
- Updates to the runbook so `signal-rs link --state-dir <here>` is a literal command operators paste, not a path they pick.
- Remove the field from the user's live `~/.config/sb/borg.yml` (no migration data loss: the state dir at the canonical path is already where the user linked).

### Non-Goals

- Not refactoring `signal-rs`'s storage layer. SQLite stays; the Double Ratchet still demands persistence; `~/.local/share/sb/borg/signal-state/store.db` keeps its schema unchanged. Per the cross-implementation survey, AsamK/signal-cli does the same thing (SQLite via JDBC, WAL mode, FK enforcement) for the same protocol reasons.
- Not removing other `SignalConfig` fields. `allowed_senders`, `notification_recipient`, `host`, and `notetoself_rate_threshold_per_hour` all have either a Telegram analog (`allowed-chat-ids`, `notification-chat-id`, `host`) or a Signal-specific privacy backstop justification (the rate gate). They stay.
- Not removing the `signal` doctor section. The section is still meaningful: state_dir existence + linked-status probe + (in the future) device-list verification. Only the data source changes (config → path helper).
- Not changing `signal-rs`'s CLI surface. `signal-rs link --state-dir` keeps its current shape; the change is internal to borg, not upstream.
- Not changing how `signal-rs link` is invoked during bootstrap. Operators still run it once per host with the canonical path as the `--state-dir` argument; that command is now a paste-the-line operation rather than a "make sure the path you type here matches the one in borg.yml" operation.

## Proposed Solution

### Overview

borg owns its `signal-rs` storage path. The path is `dirs::data_local_dir()/sb/borg/signal-state/` (i.e. `~/.local/share/sb/borg/signal-state/` on Linux, `~/Library/Application Support/sb/borg/signal-state/` on macOS), resolved at runtime via a new helper in `vault::paths`. `SignalConfig` loses the `state_dir` field. Every code site that needs the path calls the helper.

### Architecture

Before:

```
borg.yml             SignalConfig          signal::run (lib.rs supervisor)
─────────            ─────────────         ────────────────────────────────
signal:              state_dir: PathBuf  → signal::run(signal_config, ...)
  state-dir: ...                              └─ open_or_fail(&signal_config.state_dir)
  host: desk                                  └─ Client::open(state_dir)

borg.yml             SignalConfig          sb doctor (checks.rs)
─────────            ─────────────         ─────────────────────
signal:              state_dir: PathBuf  → state_dir_findings(&sg.state_dir)
  state-dir: ...                              └─ exists? collision-warn vs CLI default?
                                          → signal_probe_status(&sg.state_dir)
                                              └─ Client::open(state_dir)
```

After:

```
vault::paths
────────────
pub fn signal_state_dir() -> PathBuf
  └─ dirs::data_local_dir().join("sb/borg/signal-state")

borg.yml             SignalConfig          signal::run (lib.rs supervisor)
─────────            ─────────────         ────────────────────────────────
signal:              (no state_dir)      → let state_dir = vault::paths::signal_state_dir();
  host: desk                                signal::run(signal_config, &state_dir, ...)
                                              └─ open_or_fail(&state_dir)

borg.yml             SignalConfig          sb doctor (checks.rs)
─────────            ─────────────         ─────────────────────
signal:              (no state_dir)      → let state_dir = vault::paths::signal_state_dir();
  host: desk                                state_dir_findings(&state_dir)
                                              └─ exists? (collision-warn dropped)
                                          → signal_probe_status(&state_dir)
                                              └─ Client::open(state_dir)
```

The supervisor in `borg/src/lib.rs` is where the path resolution happens once per process startup. `signal::run` takes the resolved `&Path` as a new parameter (alongside the existing `signal_config: SignalConfig`). The doctor's `signal_findings_for` resolves the path internally because it doesn't run under the daemon supervisor. The path helper is `vault::paths::signal_state_dir()` so both borg and sb depend on it through the existing `vault` crate.

### Data Model

`SignalConfig` (`borg/src/config.rs:702-729`) loses one field:

```rust
// Before
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct SignalConfig {
    pub state_dir: PathBuf,                       // <-- removed
    #[serde(default)]
    pub allowed_senders: Vec<String>,
    #[serde(default)]
    pub notification_recipient: Option<String>,
    pub host: String,
    #[serde(default = "default_signal_rate_threshold")]
    pub notetoself_rate_threshold_per_hour: u32,
}

// After
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct SignalConfig {
    #[serde(default)]
    pub allowed_senders: Vec<String>,
    #[serde(default)]
    pub notification_recipient: Option<String>,
    pub host: String,
    #[serde(default = "default_signal_rate_threshold")]
    pub notetoself_rate_threshold_per_hour: u32,
}
```

The remaining fields are exactly the Signal analogs of `TelegramConfig` (`allowed_senders` ↔ `allowed_chat_ids`, `notification_recipient` ↔ `notification_chat_id`, `host` ↔ `host`) plus the Signal-specific rate-gate threshold.

### API Design

#### New helper in `vault/src/paths.rs`

```rust
/// Subdirectory under `dirs::data_local_dir()` that owns borg's
/// signal-rs linked-device state (Double Ratchet sessions, prekeys,
/// identity). One canonical path per borg installation; the operator
/// does NOT pick it. signal-rs link's `--state-dir` argument matches
/// this constant by convention.
pub const SB_BORG_SIGNAL_STATE_DIR: &str = "sb/borg/signal-state";

/// `~/.local/share/sb/borg/signal-state/` on Linux,
/// `~/Library/Application Support/sb/borg/signal-state/` on macOS.
/// Resolved at runtime via `dirs::data_local_dir()`.
///
/// Panics only when `dirs::data_local_dir()` returns `None`, which
/// requires both `$HOME` and `$XDG_DATA_HOME` to be unset - a broken
/// environment where the rest of borg would also fail.
pub fn signal_state_dir() -> PathBuf {
    dirs::data_local_dir()
        .expect("dirs::data_local_dir() returned None (set HOME or XDG_DATA_HOME)")
        .join(SB_BORG_SIGNAL_STATE_DIR)
}
```

The `.expect()` style matches the rest of `vault::paths` (e.g. `config_root`). The companion `vault::receipts::receipts_db_path()` uses `Result`-returning style; the divergence is historical and not worth reconciling in this change.

#### Updated `signal::run` signature

```rust
// Before
pub async fn run(
    signal_config: SignalConfig,
    config: Arc<Config>,
    desktop: Option<notify::Desktop>,
) -> Result<()>

// After
pub async fn run(
    signal_config: SignalConfig,
    state_dir: PathBuf,
    config: Arc<Config>,
    desktop: Option<notify::Desktop>,
) -> Result<()>
```

`state_dir` is passed by value (owned `PathBuf`) so `signal::run`'s long-running async body owns it without borrow-lifetime gymnastics across the reconnect loop. The supervisor computes the path once per process and clones it into the task.

#### Updated `lib.rs` supervisor

```rust
// Before
let signal_status = if let Some(signal_config) = config.signal.clone() {
    if config::is_local_host(&Some(signal_config.host.clone())) {
        log::info!("Signal transport enabled (state_dir: {}, allowed_senders: {})",
            signal_config.state_dir.display(),
            signal_config.allowed_senders.len(),
        );
        tasks.spawn_local(signal::run(signal_config, ...));
        SubsystemStatus::Running
    } else {
        SubsystemStatus::SkippedHostMismatch
    }
} else { SubsystemStatus::NotConfigured };

// After
let signal_status = if let Some(signal_config) = config.signal.clone() {
    if config::is_local_host(&Some(signal_config.host.clone())) {
        let state_dir = vault::paths::signal_state_dir();
        log::info!("Signal transport enabled (state_dir: {}, allowed_senders: {})",
            state_dir.display(),
            signal_config.allowed_senders.len(),
        );
        tasks.spawn_local(signal::run(signal_config, state_dir, ...));
        SubsystemStatus::Running
    } else {
        SubsystemStatus::SkippedHostMismatch
    }
} else { SubsystemStatus::NotConfigured };
```

The log message still shows the state_dir (useful diagnostic), but the value now comes from the helper, not the config.

#### Updated doctor section

The pure helper `state_dir_findings(&Path) -> Vec<Finding>` keeps its signature so unit tests can pass arbitrary paths. The wrapper `signal_findings_for` no longer threads `sg.state_dir`:

```rust
// Before
fn signal_findings_for(sg: &SignalConfig) -> Vec<Finding> {
    ...
    findings.extend(state_dir_findings(&sg.state_dir));
    match signal_probe_status(&sg.state_dir) { ... }
    ...
}

// After
fn signal_findings_for(sg: &SignalConfig) -> Vec<Finding> {
    ...
    let state_dir = vault::paths::signal_state_dir();
    findings.extend(state_dir_findings(&state_dir));
    match signal_probe_status(&state_dir) { ... }
    ...
}
```

The collision warning against `signal-rs`'s CLI default (`~/.local/share/signal-rs/`) is dropped from `state_dir_findings`. With borg owning a canonical path, we can never accidentally collide with the CLI default - they are different hardcoded strings. The remaining checks (exists / linked / safety-number / device list) cover everything operator-actionable.

### Implementation Plan

#### Phase 1: Internalize the path
**Model:** sonnet

- Add `SB_BORG_SIGNAL_STATE_DIR` constant and `signal_state_dir()` function to `vault/src/paths.rs`. Mirror the `SB_BORG_DATA_DIR` / `receipts_db_path()` precedent in `vault/src/receipts.rs` for style.
- Remove `state_dir: PathBuf` from `SignalConfig` in `borg/src/config.rs`. Update the four YAML deserialization tests in the same file (lines ~1236, 1260, 1278, 1294) so the `state-dir: ...` line is gone from the fixture YAML and the corresponding `assert_eq!(sg.state_dir, ...)` lines are removed.
- Update `borg/src/notify/tests.rs::mk_signal_config` to drop the `state_dir:` field initialization.
- Update `borg/src/signal.rs::run` to take an additional `state_dir: PathBuf` parameter. Replace the three call sites that currently use `signal_config.state_dir` (the entry log, the `open_or_fail` call, the `Deauthorized` bail message) with the new parameter.
- Update `borg/src/lib.rs` supervisor to compute `let state_dir = vault::paths::signal_state_dir();` once and pass it to `signal::run`. Update the "Signal transport enabled" log to use the resolved path.
- Update `sb/src/cli/checks.rs::signal_findings_for` to resolve the path via `vault::paths::signal_state_dir()` rather than reading from `sg.state_dir`. Drop the collision-warning branch in `state_dir_findings` (it can never fire). Update `sb/src/cli/checks.rs::tests::make_signal` to drop the `state_dir` field initialization. Drop the `signal_state_dir_collision_with_default_is_warn` test (the path it asserted is gone).
- `cargo build -p borg -p sb` to confirm the workspace compiles with the new shape.

#### Phase 2: Docs, template, and operator config
**Model:** sonnet

- `config/templates/borg.yml.example`: drop the `state-dir: ~/.local/share/sb/borg/signal-state/` line from the commented `signal:` block. Update the docstring above the block to name the canonical path borg uses internally so the operator still knows where to point `signal-rs link --state-dir`.
- `docs/design/2026-05-24-signal-as-borg-transport.md`: add a one-paragraph addendum at the bottom referencing this internalization design memo and noting that `state-dir` is no longer an operator-visible field. Do NOT rewrite history in the original sections; mark the addendum clearly.
- `CLAUDE.md`: update the Signal Key Convention paragraph to drop "state_dir defaults to..." and replace with "state_dir is internal to borg (`vault::paths::signal_state_dir()`); `signal-rs link --state-dir <path>` uses the same canonical path by convention."
- `~/.config/sb/borg.yml` (live config, symlinks to `~/repos/scottidler/dotfiles/HOME/.config/sb/borg.yml`): remove the `state-dir:` line from the `signal:` block. Do NOT commit the dotfiles change; that's a user-driven commit on their schedule.

#### Phase 3: Ship and verify
**Model:** sonnet

- `otto ci` to confirm tests pass, format/lint clean.
- `bump -a` to v0.8.23 (patch; this is an internal refactor with no API impact for operators beyond the removed config field).
- `git push && git push --tags`.
- `otto deploy` (rebuilds sb, installs to `~/.cargo/bin/`, restarts borg + cortex daemons).
- `systemctl --user status borg` and tail `~/.local/share/sb/borg.log` to confirm "Signal transport enabled" with the canonical state_dir path and no "unable to open database file" crash.
- `sb doctor` to confirm the `signal` section reports the linked state cleanly (no "not configured" or path-related findings).
- Send a Note-to-Self URL from the phone, observe ingest in `sb borg log`, confirm the Saved reply lands back in Note-to-Self.

## Alternatives Considered

### Alternative 1: Fix tilde expansion in `SignalConfig::state_dir` and call it done

- **Description:** Pass `state_dir` through `shellexpand::tilde` (the established pattern at `vault/src/paths.rs:86`) during config load or in the `open_or_fail` call. Keep the field in `borg.yml`. Total change: one line.
- **Pros:** Minimal diff. Preserves operator's theoretical freedom to put the state dir somewhere else. Doesn't touch the type signature of `signal::run`.
- **Cons:** Addresses the runtime bug but not the design defect. The leakage of `signal-rs`-internal storage layout into the operator surface remains. Telegram's analog still has zero such knobs. The collision-with-CLI-default footgun stays (since the operator can still type the wrong path). The "two paths of truth" problem (borg.yml + `signal-rs link --state-dir`) stays. The operator's freedom to point the state dir elsewhere is theoretical, not practical: any non-default path means the `signal-rs link` runbook becomes "look at your borg.yml and use whatever path is there" instead of a literal pasteable command.
- **Why not chosen:** Treating the field as legitimate doubles down on the leakage instead of removing it. The user explicitly flagged the leakage during ship-out; the right fix is to delete the field, not patch around it.

### Alternative 2: Move the helper into `borg::paths` rather than `vault::paths`

- **Description:** Add a new `borg/src/paths.rs` module owning Signal- and Telegram-specific path resolution. Keep `vault::paths` for cross-crate paths only. Avoids growing `vault::paths` with borg-specific knowledge.
- **Pros:** Cleaner module ownership: borg-specific paths in borg, vault-shared paths in vault. Symmetric with the principle that `borg/src/intake.rs` owns borg's bookkeeping while shared receipts schema lives in `vault::receipts`.
- **Cons:** Two helpers in the workspace doing the same conceptual job: "resolve a borg-owned data path under `dirs::data_local_dir()/sb/borg/...`." Splitting between `vault::receipts::receipts_db_path()` and a new `borg::paths::signal_state_dir()` makes the conceptual cluster less discoverable. The doctor section in `sb/` would have to import from `borg::paths` for one helper and `vault::receipts` for another - asymmetric. The existing `vault::paths` module already holds `borg_config()` and other borg-specific paths, so it's already where "borg path constants" live.
- **Why not chosen:** The existing `vault::paths` has the precedent (`borg_config()`, `cortex_config()`). Adding `signal_state_dir()` there matches the established shape; sb and borg already depend on `vault`. The cleanliness gain of a separate `borg::paths` module isn't worth the duplication.

### Alternative 3: Configurable default via env var fallback (`SB_SIGNAL_STATE_DIR`)

- **Description:** `signal_state_dir()` reads `$SB_SIGNAL_STATE_DIR` if set, falls back to the canonical path otherwise. The config field stays gone, but operators with custom storage needs can still override.
- **Pros:** Preserves an escape hatch for edge cases (multi-tenant, custom backup strategies, testing with isolated state dirs).
- **Cons:** Same leakage in env-var form. The operator can still point at a path that disagrees with their `signal-rs link --state-dir` invocation. The env var is harder to discover than a config field. Adds a code path that's only exercised by edge cases, increasing the bug surface for tests and doctor.
- **Why not chosen:** YAGNI. No real-world scenario requires this. If a future operator genuinely needs to relocate the state dir, the right answer is to add the override at that point, not to ship one preemptively.

### Alternative 4: Remove the `signal:` block from `borg.yml` entirely; enable purely via the presence of a linked state dir

- **Description:** No `signal:` block in `borg.yml`. borg checks at startup whether `signal_state_dir()` contains a linked store; if yes, run the signal supervisor with defaults. Privacy (allowed_senders, notification_recipient) and host pinning move to other config locations or hardcoded defaults.
- **Pros:** Maximum symmetry-with-zero-Telegram-leakage: not even a `signal:` block to argue about.
- **Cons:** Loses the privacy allowlist (`allowed_senders` has no Telegram analog but no logical home outside `signal:`). Loses the host pin (mandatory for sole-machine ingest discipline). Loses the rate gate (a Signal-specific privacy backstop with no other home). Operator can't disable signal on a single machine without `rm -rf`ing the state dir, which is destructive.
- **Why not chosen:** The `signal:` block earns its keep with `allowed_senders`, `host`, `notification_recipient`, `notetoself_rate_threshold_per_hour`. Only `state_dir` is leakage. Keep the block; remove the field.

## Technical Considerations

### Dependencies

- No new crates.
- The `vault` crate already depends on `dirs`; the new helper uses it directly.
- The `borg` and `sb` crates already depend on `vault`; the new helper is reachable from both.

### Performance

- `signal_state_dir()` does one `dirs::data_local_dir()` call and one `PathBuf::join()`. Cost is negligible (microseconds) and amortized: borg resolves it once at supervisor startup; sb doctor resolves it once per `sb doctor` invocation. No hot-path cost.
- The signal subsystem's reconnect loop already owns the `state_dir` value (via the `signal_config` clone today); after the change it owns a `PathBuf` instead, which is a thin Vec<u8> wrapper. No change in steady-state memory.

### Security

- The canonical path lives under the user's `$XDG_DATA_HOME` (typically `~/.local/share/`), same as before. No permission or ownership change.
- Removing the operator's ability to relocate the state dir reduces the attack surface: there is one path, owned by the daemon's UID, and the path is the same across all installations. This makes operational discipline (backup, monitoring, ACL) easier to standardize. The pre-existing 2-layer privacy defense (signal-rs's wire-ACI → typed-variant remap + borg's `accepted_envelope` pattern match) is unchanged by this refactor.
- The rate gate, allowlist enforcement, and group-id filtering are all in `signal::dispatch_envelope` and `signal::accepted_envelope`. None of those code paths touches `state_dir`; the refactor leaves them byte-for-byte unchanged.

### Testing Strategy

- **Compile-time enforcement:** The compiler rejects the workspace if any remaining call site references `SignalConfig::state_dir`. Phase 1 is "done" only when `cargo build -p borg -p sb` is clean.
- **Unit tests updated, not added:** The four `SignalConfig` deserialization tests in `borg/src/config.rs::tests` lose their `state-dir:` lines and `state_dir` assertions. The `notify::tests::mk_signal_config` and `sb/src/cli/checks.rs::tests::make_signal` test helpers lose the `state_dir:` initialization. The `signal_state_dir_collision_with_default_is_warn` test is deleted (the path it asserted is gone). The remaining doctor tests (host mismatch, empty host, state_dir missing) still exercise the relevant branches.
- **No new tests for the path helper:** `vault::paths::signal_state_dir()` is one `expect()` + one `join()`. The cost of a test that asserts `dirs::data_local_dir().unwrap().join("sb/borg/signal-state")` against the helper's output is higher than the bug surface it covers.
- **End-to-end smoke** (Phase 3): The `sb doctor` invocation against a live linked state on the user's machine is the load-bearing test. If the linked state at the canonical path is detected, the path helper, supervisor wiring, and doctor wiring are all working. If not, `sb doctor` reports it clearly.

### Rollout Plan

- Single PR, single `bump -a` to v0.8.23, single `otto deploy`.
- The operator's existing linked state at `~/.local/share/sb/borg/signal-state/` matches the new canonical path exactly; no data migration. The first restart after deploy picks up the same store.db, reconnects via the same identity, and resumes receiving envelopes from the same conversation ratchet positions. No re-link required.
- The user's `~/.config/sb/borg.yml` keeps its `signal:` block but loses the `state-dir:` line. The remaining fields (`allowed_senders`, `host`, optionally `notification_recipient` / `notetoself_rate_threshold_per_hour`) are unchanged.
- Other machines (laptops, etc.) where `signal:` is configured with `host: <not-this-host>` are unaffected: the supervisor's `is_local_host` check short-circuits before the new code path runs.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| The operator has a non-default state_dir in `borg.yml` that the canonical helper does not match | None on this machine; potentially Low elsewhere | Medium | This is a single-user deployment; the user is the operator and confirms the live config used the default path. No third-party operators exist to be surprised. |
| `dirs::data_local_dir()` returns a different path on macOS than what `signal-rs link --state-dir` defaults to on macOS, breaking the cross-platform symmetry | Low | Low | Runbook on macOS calls out the full path returned by the helper (`~/Library/Application Support/sb/borg/signal-state/`). `signal-rs link` accepts arbitrary paths via `--state-dir`, so the runbook command stays the same shape modulo path. |
| Removing `state_dir` from `SignalConfig` is technically a breaking change to the public `Config` deserialization surface (extra fields are tolerated by default; missing required fields are caught at deserialize time) | Low | Low | `state_dir` is removed (not renamed) and is being deleted from the only known YAML that contains it (the user's live `borg.yml` in this same PR). serde's default behavior tolerates unknown fields in `borg.yml` if the new code adds `#[serde(default)]` or simply ignores them; the field's removal means the field's absence from YAML is no longer a deserialization error. If a stale `state-dir:` line stays in some operator's YAML, serde silently ignores it (we don't `deny_unknown_fields`). Acceptable. |
| The CLI-default collision warning is genuinely useful for an operator who runs `signal-rs link` without `--state-dir`, hits the CLI default, and never realises borg is looking elsewhere | Low | Low | The state_dir-existence check at the canonical path catches this: borg's check fails with "state_dir does not exist" and the suggested fix is "signal-rs link --name borg --state-dir <canonical>", which is the right correction. The collision warning was paper over a problem the config field invented; once the field is gone, the original check is sufficient. |
| Future scenario where multi-account Signal support requires multiple state dirs | Very Low | Low | Out of scope for v1 by design. If it ever lands, `signal:` becomes a list-of-blocks (or map-keyed-by-account-name) and the path helper becomes parameterized. Today, hardcoding is correct. |

## Open Questions

- [ ] Should the path helper be `signal_state_dir()` (the chosen name) or `borg_signal_state_dir()` (more disambiguated, matches `SB_BORG_DATA_DIR` style)? Picked `signal_state_dir()` for brevity since `vault::paths` already namespaces it.

## References

- `docs/design/2026-05-24-signal-as-borg-transport.md` - parent design memo for the Signal transport.
- `docs/signal-rs-consumer-integration-handoff.md` - signal-rs's consumer-side contract; describes `--state-dir` as a CLI-level argument, not a downstream-consumer config requirement.
- `vault/src/paths.rs` - existing module for canonical path resolution.
- `vault/src/receipts.rs:172-202` - `SB_BORG_DATA_DIR` + `receipts_db_path()` precedent.
- `borg/src/config.rs:702-729` - current `SignalConfig` definition.
- `borg/src/lib.rs:~320-345` - supervisor's signal subsystem launch.
- `borg/src/signal.rs:706-834` - `signal::run` entry point and state_dir consumers.
- `sb/src/cli/checks.rs:698-868` - `signal_findings` + probe.
- `~/repos/AsamK/signal-cli/lib/src/main/java/.../storage/Database.java:98` - prior-art for the SQLite-backed Signal storage decision (same engine, same WAL mode).
