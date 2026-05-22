# Design Document: Firefox Extension Lifecycle Owned by `sb borg extension`

**Author:** Scott Idler
**Date:** 2026-05-21
**Status:** Implemented (then partially superseded; see amendment below)
**Review Passes Completed:** 5/5 + Architect Rounds 1-2 (approved for implementation)
**Implemented at:** workspace v0.8.12 (all 7 phases landed 2026-05-21)

> **Amendment (2026-05-22):** The `validate`, `generate`, `extension-validate`,
> and `strip_volatile_fields` machinery described here is superseded by
> [`2026-05-22-extension-manifest-binary-versioned.md`](2026-05-22-extension-manifest-binary-versioned.md).
> The sign/install/uninstall user-facing behaviour is preserved; internals
> move to `tempfile::TempDir` staging and binary-versioned manifest threading
> (sb passes its own `env!("CARGO_PKG_VERSION")` into `extension::stage` /
> `extension::sign::run` / `extension::install::run`). Committed
> `manifest.json` and `ingest-schema.json` are deleted; `borg/build.rs`
> (along with `cortex/build.rs` and `oracle/build.rs`) is deleted in the
> same change. Read the newer doc for the current architecture.

## Summary

Fold the Firefox capture extension's entire lifecycle (generate, validate, sign, install, uninstall) into a single `sb borg extension` subcommand group backed by a focused `borg::extension` module. Eliminate every drift surface - manifest generator vs. committed manifest, `host_permissions` vs. CSP `connect-src`, signed `.xpi` vs. installed-in-Firefox, extension version vs. workspace tag, configured endpoint vs. permitted origins, daemon protocol vs. client request shape - by making each structurally impossible rather than just catchable. Replace the manual `xdg-open` install path with a one-time Firefox Enterprise Policy that force-installs from a stable local symlink, so the end-to-end loop becomes a single `sb borg extension install` and Firefox picks up the new build with zero clicks.

This design invalidates the most recent extension-tooling work (commit `8752ba4`, which added `bin/extension-sign`, the `otto extension` task that wraps it, and `docs/extension-signing.md`). That work was a thoughtful incremental fix to a flow that turned out to be the wrong abstraction. The right abstraction - this design - replaces it entirely. `bin/extension-sign`, `borg/clients/extension/sign.sh`, and the dead `borg::sign` Rust function all go away; one Rust module and one clap subcommand group replace them.

## Problem Statement

### Background

The Firefox extension lives at `borg/clients/extension/`. Its job is one HTTP POST: capture the active tab's URL, send `{url: ...}` to the running borg daemon's `/ingest` endpoint, surface a toast. It was originally written in `scottidler/obsidian-borg`, ported into the second-brain workspace at commit `b520196`, and has been version 0.4.0 since the port.

A Rust generator for `manifest.json` already exists at `borg/src/lib.rs:781` (`generate_manifest`). It correctly derives `host_permissions` and CSP `connect-src` from one source (`borg::config`), pulls the version from `env!("CARGO_PKG_VERSION")`, and produces a fully-formed manifest. A signing function `borg::sign` at `borg/src/lib.rs:869` wraps the generator + `web-ext sign`. A clap subcommand `Command::Sign` at `sb/src/cli/borg.rs:53` exposes it as `sb borg sign`.

None of these are used. The actual flow is:
- `otto extension` → `bin/extension-sign` (shell) → `borg/clients/extension/sign.sh` (shell) → `web-ext sign`
- The Rust generator is bypassed. The committed `manifest.json` is hand-edited (or rather, stale from the port).
- `sb borg sign` is broken anyway: `root.join("clients/extension")` resolves to a path that doesn't exist in this repo (should be `borg/clients/extension`).

The result: when borg's workspace version went from 0.4.0 → 0.8.12, the committed manifest stayed at 0.4.0. When the port lost the `*.lan` host_permission and the CSP `connect-src` override, no automation caught it. The user discovered the missing CSP only when the extension started silently failing on a fresh deploy with `NetworkError`.

### Problem

The extension lifecycle has eight independent drift surfaces, each of which has either burned us or is one mistake away from doing so:

1. **`host_permissions` ↔ CSP `connect-src`** must list the same origins. They were hand-maintained as two separate lists in obsidian-borg's gitignored manifest. The CSP entry got lost during the port; `host_permissions` survived.
2. **Generator output ↔ committed `manifest.json`** can diverge silently because nothing runs the generator. The committed manifest is whatever someone last hand-typed.
3. **Committed manifest version ↔ workspace version.** Workspace is `0.8.12`, manifest is `0.4.0`. The generator already uses `CARGO_PKG_VERSION` correctly - it's just never called.
4. **Signed `.xpi` ↔ source manifest.** `web-ext sign` happily packages whatever's on disk. If the source is stale, the .xpi is stale.
5. **Signed `.xpi` ↔ extension installed in Firefox.** Today the install path is `xdg-open .xpi` followed by a manual "Add" click. Easy to sign and forget to install, leaving the browser running the old build against a new daemon.
6. **Configured endpoint URL ↔ origins permitted by manifest.** User types `http://foo.example:8181` in the options text field, save succeeds, fetch fails at runtime with `NetworkError`. The validation that *should* be at save-time is at fetch-time.
7. **Daemon `/ingest` protocol ↔ extension request body.** If borg adds a required field to `IngestRequest`, the extension silently keeps sending the old shape; the daemon rejects with a parse error.
8. **`bin/extension-sign` (shell), `borg/clients/extension/sign.sh` (shell), `borg::sign` (Rust), `otto extension` (otto task), `sb borg sign` (clap subcommand)** are five overlapping entry points for the same task. The Rust one is broken and dead. The shell ones are the working path. There is no `install` verb anywhere.

These are not eight different design problems. They are one design problem: nothing owns the extension lifecycle end-to-end. Every drift surface above is a symptom of that.

### Goals

- One discoverable command surface for the entire extension lifecycle: `sb borg extension {generate, validate, sign, install, bump, version}`.
- One Rust module (`borg::extension`) owns all manifest construction, signing, schema generation, and install.
- Generator output is committed AND CI-enforced (drift = build failure).
- One-list-drives-two pattern: `host_permissions` and CSP `connect-src` derived from a single declaration.
- Extension version is `env!("CARGO_PKG_VERSION")` of the borg crate. `bump` on the workspace bumps the extension. No separate version concept.
- Install verb is fully automated via Firefox Enterprise Policy + stable symlink. No `xdg-open`. No clicks.
- Options-page endpoint input validates against manifest permissions at save time.
- Daemon protocol schema is JSON-schema-generated from `borg::http::IngestRequest`, committed, and validated client-side before send.
- Delete `bin/extension-sign`, `borg/clients/extension/sign.sh`, and `Command::Sign`. One entry point, not five.

### Non-Goals

- Cross-browser support (Chrome/Edge/Safari). Firefox-only by deliberate choice; the user runs Firefox, AMO signing is Firefox-specific.
- Hosted update server. `install_url` is a `file://` URL pointing at a local symlink; no need to run an update server when the .xpi lives next to the source.
- Auto-discovery of the daemon (mDNS, `_borg._tcp.local`). The options text field stays as the configuration mechanism; we only fix the silent-failure UX.
- Listed (public) AMO distribution. Channel stays `unlisted`.
- Persistent extension state migration when version changes. The extension stores one key (`endpoint`) and one cache (`lastResult`); both are forward-compatible.

## Proposed Solution

### Drift Surface Closure (the value proposition in one table)

| Drift surface | Status today | Closure mechanism |
|---|---|---|
| `host_permissions` ↔ CSP `connect-src` | Lost during port; caused current NetworkError | One `ORIGIN_PATTERNS` const → both lists derived. Structurally impossible to diverge. |
| Generator output ↔ committed `manifest.json` | Generator exists but is never run; manifest is stale | `sb borg extension validate` runs in `otto ci`; drift = build failure. |
| Committed manifest version ↔ workspace version | Manifest at 0.4.0; workspace at 0.8.12 | Generator uses `env!("CARGO_PKG_VERSION")`. Workspace `bump` bumps everything. |
| Signed `.xpi` ↔ source manifest | Sign happily packages whatever's on disk | `sign` calls `generate` first; no way to bypass. |
| Signed `.xpi` ↔ extension installed in Firefox | Manual `xdg-open` + click; easy to skip | `install` writes Firefox Enterprise Policy pointing at a stable `-latest.xpi` symlink; force-installed without prompt. |
| Configured endpoint ↔ origins permitted by manifest | Silent NetworkError at fetch time | `options.js` calls `chrome.permissions.contains` at save time; rejects with a fix path. |
| Daemon `/ingest` protocol ↔ extension request body | Daemon-side serde error 50ms after fetch | `IngestRequest` derives `JsonSchema`; schema is regenerated alongside manifest; a Rust integration test (`borg/tests/extension_body_matches_ingest_request.rs`) deserializes the extension's canonical body into `IngestRequest` at CI time. No runtime JS validator. Build-time invariant, not browser tax. |
| Five overlapping entry points for "sign the extension" | All exist; only the shell path is used; Rust path is broken | One module (`borg::extension`), one subcommand group (`sb borg extension`), one otto task. |

### Overview

A new `borg::extension` module owns the manifest, the schema, the signing flow, and the install flow. A new `sb borg extension <verb>` clap subcommand group is the only public entry point. `.otto.yml`'s `extension` task and `ci` task both call `sb borg extension <verb>`. The Firefox install side is closed by a one-time `/etc/firefox/policies/policies.json` that points at a stable symlink whose target is updated atomically by `sign`.

### Architecture

```
sb borg extension generate    --+
sb borg extension validate    --+--> sb/src/cli/borg/extension.rs
sb borg extension sign        --+    (clap shell; calls borg::extension::*)
sb borg extension install     --+
sb borg extension bump        --+
sb borg extension version     --+

                                     borg/src/extension/
                                       mod.rs       (pub API)
                                       manifest.rs  (build/write manifest.json)
                                       schema.rs    (build/write ingest-schema.json)
                                       sign.rs      (web-ext sign wrapper)
                                       install.rs   (policies.json + symlink)

.otto.yml:
  extension:  sb borg extension install                   # daily-use entry point (full install, may need sudo)
  ci task adds: sb borg extension validate                # drift gate
  deploy task adds: sb borg extension install \
                      --no-policy --if-installed          # post-deploy hook: re-sign + symlink swap iff
                                                          #   the extension is already installed on this
                                                          #   machine. No-op on daemon-only servers. No sudo.

bootstrap:
  sb bootstrap --extension       # write policies.json on first run
```

The `clients/extension/` directory remains the artifact root. After this change every file in it except the JS/icons is generated:

```
borg/clients/extension/
  background.js          (source, hand-maintained)
  options.html           (source, hand-maintained)
  options.js             (source, hand-maintained)
  icons/                 (source, hand-maintained)
  manifest.json          (GENERATED, committed, CI-validated)
  ingest-schema.json     (GENERATED, committed, CI-validated)
  web-ext-artifacts/
    obsidian-borg-{version}.xpi   (signed artifact, versioned)
    obsidian-borg-latest.xpi      (symlink → newest signed .xpi)
  .amo-upload-uuid       (managed by web-ext, committed)
```

`popup.html` and `popup.js` are dead since commit `d901af8` (action no longer has `default_popup`). Delete in this work; not referenced anywhere.

### Data Model

#### One declaration drives two lists

In `borg/src/extension/manifest.rs`:

```rust
/// Default origin patterns when the user has not declared their own under
/// `extension.origin-patterns` in borg.yml. Covers the standard single-user
/// LAN deployment. Each pattern produces a `host_permissions` entry
/// (`http://{pat}/*`) AND a CSP `connect-src` entry (`http://{pat}:*`).
/// Deriving both lists from one source makes drift structurally impossible.
const DEFAULT_ORIGIN_PATTERNS: &[&str] = &[
    "localhost",
    "*.lan",
    "*.local",
];

/// Resolve the origin patterns for this build. Precedence:
///   1. `extension.origin-patterns` in borg.yml (explicit user list)
///   2. `server.host` from borg.yml, normalized into a single-host pattern
///      AND merged with DEFAULT_ORIGIN_PATTERNS
///   3. DEFAULT_ORIGIN_PATTERNS alone
/// This preserves the existing config-driven behavior (`borg/src/lib.rs:787`
/// today derives from `config.server.host`) for users with non-LAN topologies
/// like Tailscale (`100.x.y.z`), ZeroTier, or a VPS hostname, while keeping
/// the structural one-list-two-derivations property.
fn origin_patterns(config: &borg::config::Config) -> Vec<String> {
    if let Some(explicit) = &config.extension.origin_patterns {
        return explicit.clone();
    }
    let mut patterns: Vec<String> = DEFAULT_ORIGIN_PATTERNS.iter().map(|s| s.to_string()).collect();
    let host = &config.server.host;
    if !host.is_empty() && !patterns.iter().any(|p| pattern_covers_host(p, host)) {
        patterns.push(host.clone());
    }
    patterns
}

fn host_permissions(patterns: &[String]) -> Vec<String> {
    patterns.iter().map(|p| format!("http://{p}/*")).collect()
}

fn csp_extension_pages(patterns: &[String]) -> String {
    let connect = patterns.iter()
        .map(|p| format!("http://{p}:*"))
        .collect::<Vec<_>>()
        .join(" ");
    format!("default-src 'self'; connect-src {connect}")
}
```

Pattern choice on this user's single-machine LAN deployment defaults to (`localhost`, `*.lan`, `*.local`) - intentionally broad because pinning to `desk.lan` requires re-signing when borg moves. Over-permitting to `*.lan` is acceptable: the user controls every host on their LAN, the extension is personal, and the action only fires on Alt+Shift+B (no background traffic).

For users running borg on Tailscale, ZeroTier, a VPS, or any host not covered by the default patterns: set `extension.origin-patterns` in `~/.config/sb/borg.yml` to an explicit list (e.g. `["100.64.0.0/10", "borg.example.com"]`) and re-run `sb borg extension install`. The generator picks up the config-declared patterns; the structural-non-drift invariant (`host_permissions` ↔ CSP `connect-src` derived from the same source) is preserved. The hardcoded-const regression flagged by the Architect (Round 1) is closed by this design.

#### Manifest construction

```rust
pub fn build_manifest() -> serde_json::Value {
    let version = env!("CARGO_PKG_VERSION");
    serde_json::json!({
        "manifest_version": 3,
        "name": "obsidian-borg Capture",
        "description": "Send the current tab URL to obsidian-borg for ingestion",
        "version": version,
        "icons": { "16": "icons/locutus-16.png", "48": "icons/locutus-48.png", "128": "icons/locutus-128.png" },
        "action": { "default_icon": { "16": "icons/locutus-16.png", "48": "icons/locutus-48.png", "128": "icons/locutus-128.png" } },
        "background": { "scripts": ["background.js"], "service_worker": "background.js" },
        "permissions": ["activeTab", "storage", "notifications"],
        "host_permissions": host_permissions(),
        "content_security_policy": { "extension_pages": csp_extension_pages() },
        "options_ui": { "page": "options.html", "open_in_tab": false },
        "commands": {
            "capture-url": {
                "description": "Capture current tab URL",
                "suggested_key": { "default": "Alt+Shift+B" }
            }
        },
        "browser_specific_settings": {
            "gecko": {
                "id": "obsidian-borg@scottidler",
                "strict_min_version": "140.0",
                "data_collection_permissions": { "required": ["none"], "optional": [] }
            }
        }
    })
}
```

#### Schema generation

`borg::types::IngestRequest` (the struct at `borg/src/types.rs:262` that backs the `/ingest` POST body via `Json<IngestRequest>`) gets `#[derive(JsonSchema)]` (from `schemars`). Current shape: `{ url: String, tags: Option<Vec<String>>, priority: Option<Priority>, force: bool, method: Option<IngestMethod> }`. The extension currently only sends `{url}`, which is the minimum the schema requires.

The generator writes the schema to `ingest-schema.json` alongside `manifest.json`:

```rust
pub fn build_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(borg::types::IngestRequest)).unwrap()
}
```

The schema file is committed and useful as a contract document (and for future non-JS clients), but **the extension does NOT validate against it at runtime**. The Architect's Round 1 critique stands: shipping a JSON Schema evaluator into vanilla JS means either bundling Ajv (~30 KB minified, opaque dependency surface) or writing a hand-rolled subset that diverges from `schemars`. Both shift a build-time invariant into a runtime tax inside the browser. The right place for that check is CI.

Drift detection moves to a Rust integration test, `borg/tests/extension_body_matches_ingest_request.rs`:

```rust
#[test]
fn extension_post_body_deserializes_into_ingest_request() {
    // Canonical body shape the extension sends, mirroring background.js literally.
    let body = serde_json::json!({ "url": "https://example.com/" });
    let _req: borg::types::IngestRequest = serde_json::from_value(body)
        .expect("extension body must remain compatible with IngestRequest; \
                 if you added a required field to IngestRequest, update background.js \
                 (and the canonical body in this test) at the same commit");
}
```

If a future PR adds a required field to `IngestRequest` without updating the extension, this test fails in `otto ci` with a directly actionable message. No JS schema library, no runtime check, no skew between schemars and ajv semantics. `background.js` stays at its current 2.2 KB.

The `ingest-schema.json` file remains generated and committed so that (a) external/future clients have a machine-readable contract, (b) `sb borg extension validate` flags any unintended `IngestRequest` shape change in the same diff that surfaces manifest drift, and (c) the test above stays in lockstep with the schema file via the existing CI drift gate. The runtime in-browser validation step is dropped entirely.

### API Design

#### `borg::extension` public API

```rust
// borg/src/extension/mod.rs
pub fn generate(repo_root: &Path, config: &borg::config::Config) -> Result<GenerateResult>;
pub fn validate(repo_root: &Path, config: &borg::config::Config) -> Result<ValidateResult>;
pub fn sign(repo_root: &Path, config: &borg::config::Config) -> Result<SignResult>;
pub fn install(repo_root: &Path, config: &borg::config::Config, opts: InstallOpts) -> Result<InstallResult>;
pub fn current_version() -> &'static str;                     // env!("CARGO_PKG_VERSION")

pub struct GenerateResult {
    pub manifest_path: PathBuf,
    pub schema_path: PathBuf,
    pub manifest_changed: bool,
    pub schema_changed: bool,
}

pub struct ValidateResult {
    pub manifest_drift: Option<String>,  // unified diff if drifted
    pub schema_drift: Option<String>,
}
impl ValidateResult {
    pub fn is_ok(&self) -> bool { self.manifest_drift.is_none() && self.schema_drift.is_none() }
}

pub struct SignResult {
    pub xpi_path: PathBuf,        // versioned: obsidian-borg-0.8.12.xpi
    pub latest_link: PathBuf,     // symlink: obsidian-borg-latest.xpi → xpi_path
    pub version: String,
}

pub struct InstallOpts {
    pub write_policy: bool,       // false on machines where policy is already in place
    /// Explicit override for the policies.json destination. When `Some`, skip
    /// Firefox install detection entirely and write to this path. For users on
    /// managed machines where /etc/firefox/policies/policies.json is owned by
    /// Ansible/Puppet/Chef and our deep-merge would race the config-management
    /// agent. Typical override target: a side file the management tool is
    /// configured to merge in (e.g. /etc/firefox/policies.d/obsidian-borg.json),
    /// or a user-level policies directory.
    pub policy_file: Option<PathBuf>,
    /// When true, the whole install is a no-op unless the auto-detected
    /// policies.json already contains our extension ID. Used by the
    /// `otto deploy` hook so deploys on daemon-only servers (no Firefox,
    /// never bootstrapped) skip silently instead of failing on a missing
    /// Firefox install or prompting for sudo.
    pub if_installed: bool,
}

pub struct InstallResult {
    pub xpi_path: PathBuf,
    pub policy_path: Option<PathBuf>,  // Some if we wrote it, None if it already existed
    pub firefox_restart_required: bool, // true if policies.json changed; false if only symlink updated
}
```

#### sb clap subcommand

```rust
// sb/src/cli/borg/extension.rs
#[derive(Subcommand)]
pub enum ExtensionCommand {
    /// Regenerate manifest.json and ingest-schema.json from code
    Generate,
    /// Regenerate and fail if committed files differ (for CI)
    Validate,
    /// Generate + sign via AMO (produces a versioned .xpi)
    Sign,
    /// Sign + drop policies.json (sudo first run) + update symlink
    Install {
        /// Skip writing policies.json (assumes it's already in place)
        #[arg(long)]
        no_policy: bool,
        /// Write policies.json to this path instead of the auto-detected
        /// Firefox install location. Use this on machines where
        /// /etc/firefox/policies/policies.json is owned by config management
        /// (Ansible/Puppet/Chef) to avoid racing the management agent.
        #[arg(long, value_name = "PATH")]
        policy_file: Option<PathBuf>,
        /// Refresh only if the extension is already installed on this machine
        /// (i.e., the policies.json contains our extension ID). No-op on
        /// machines that have never been bootstrapped with the extension.
        /// Designed for unattended post-deploy hooks: `otto deploy` calls
        /// `sb borg extension install --no-policy --if-installed` so the same
        /// deploy command works on the laptop (refresh extension) and on
        /// daemon-only servers (no-op, no sudo, no Firefox dependency).
        #[arg(long)]
        if_installed: bool,
    },
    /// Remove policies.json entry + delete signed artifacts (does NOT
    /// uninstall from a running Firefox - restart Firefox to clear)
    Uninstall {
        /// Also delete the web-ext-artifacts/ directory entirely
        #[arg(long)]
        purge: bool,
    },
    /// Print the extension version (= workspace version)
    Version,
}
```

`bump` is intentionally omitted from this subcommand group: the extension version IS the workspace version, so the existing workspace `bump` flow already bumps the extension. Adding a separate `extension bump` would resurrect the version-drift problem this design closes.

#### Firefox Enterprise Policy

`sb borg extension install` writes (if absent) the policy file at the location appropriate for the detected Firefox install:

| Install type | Policy path | Notes |
|---|---|---|
| Mozilla tarball (this user's machine: `/opt/firefox/`) | `/opt/firefox/distribution/policies.json` | Cleanest case. Requires sudo. Mozilla launcher reads it directly. |
| Ubuntu/Debian apt package | `/etc/firefox/policies/policies.json` | Requires sudo. |
| Fedora/RHEL dnf package | `/etc/firefox/policies/policies.json` | Same as apt. |
| Snap | `/etc/firefox/policies/policies.json` (per snap docs) | Snap confinement may block `file://` `install_url` - documented risk. |
| Flatpak | `~/.var/app/org.mozilla.firefox/.mozilla/firefox/policies/` (user-level) | Different surface entirely; flatpak sandbox blocks `/etc`. |

Install detection is by `readlink -f $(which firefox)` (this user resolves to `/opt/firefox/firefox` → tarball). The implementation auto-detects and fails loudly with the correct setup instructions if the install type is unsupported or known-broken (snap).

Policy content for all paths is identical:

```json
{
  "policies": {
    "ExtensionSettings": {
      "obsidian-borg@scottidler": {
        "installation_mode": "force_installed",
        "install_url": "file:///home/saidler/repos/scottidler/second-brain/borg/clients/extension/web-ext-artifacts/obsidian-borg-latest.xpi",
        "updates_disabled": false,
        "default_area": "navbar"
      }
    }
  }
}
```

The `install_url` is the stable `-latest.xpi` symlink. Each `sign` re-points the symlink to the freshly-signed versioned .xpi atomically (`ln -sfT new old.tmp && mv old.tmp old`).

Per Mozilla's enterprise-policy documentation (verified against `mozilla/policy-templates` `docs/index.md`, ExtensionSettings → `install_url` section): "If installing from the local file system, use a `file:///` URL. **Firefox will update or re-install the extension whenever the XPI file at that path changes.** You can also manually trigger an update by changing the file name or path." Firefox itself watches the file at `install_url`. When the symlink target changes (new versioned .xpi materializes, `mv old.tmp obsidian-borg-latest.xpi` re-points it), Firefox detects the change and re-installs without restart, polling interval, or external signal. The `updates_disabled` field is therefore left at `false` (Mozilla default): setting it `true` would disable the file-watch and break the symlink-swap mechanism.

Firefox does **not** expose a supported "reload extensions from outside the browser" signal (an earlier draft incorrectly claimed `pkill -USR1 firefox` worked; SIGUSR1 is not handled by Firefox on Linux and will either be dropped or terminate the browser). None is needed, given the above. The `install` verb prints a closing line: "extension installed; Firefox will pick up the change automatically" - no operator action required.

#### CSP / origin validation in options.js

Replace the current options.js save handler with:

```js
document.getElementById("save").addEventListener("click", async () => {
  const value = input.value.trim().replace(/\/+$/, "");
  let url;
  try { url = new URL(value); } catch {
    msg.textContent = `Not a valid URL: ${value}`; return;
  }
  const origin = `${url.protocol}//${url.host}/*`;
  const allowed = await chrome.permissions.contains({ origins: [origin] });
  if (!allowed) {
    msg.textContent = `Origin ${url.origin} is not in the extension's manifest. ` +
                      `Add to ORIGIN_PATTERNS in borg/src/extension/manifest.rs and re-sign.`;
    return;
  }
  await chrome.storage.local.set({ endpoint: value });
  msg.textContent = "Saved.";
  setTimeout(() => { msg.textContent = ""; }, 2000);
});
```

This converts the silent-NetworkError class of bug into a save-time error with a fix path.

### Implementation Plan

#### Phase 1: Move generator into focused module, delete dead code
**Model:** sonnet

- Create `borg/src/extension/mod.rs`, `manifest.rs`, `sign.rs`, `install.rs`, `schema.rs` (the last as an empty stub - Phase 3 fills it).
- Move `generate_manifest` from `borg/src/lib.rs:781` into `borg::extension::manifest::build_manifest`. **Keep** the `config: &borg::config::Config` parameter; route origin pattern selection through the new `origin_patterns(config)` helper (explicit `extension.origin-patterns` → server.host merged with defaults → defaults). Add an `extension: ExtensionConfig` field to `borg::config::Config` with `origin_patterns: Option<Vec<String>>`; default `None`.
- Move the body of `borg::sign` from `borg/src/lib.rs:869` into `borg::extension::sign::run`. Fix the path bug on the way: `root.join("clients/extension")` → `root.join("borg/clients/extension")`.
- Add `pub fn generate(repo_root: &Path) -> Result<GenerateResult>` to `borg::extension`; it writes `manifest.json` (always) and `ingest-schema.json` (initially an empty `{}` stub - Phase 3 fills it). `sign::run` calls `generate` first.
- Leave the old `borg::sign` as a `#[deprecated = "use borg::extension::sign::run"]` shim. Leave `Command::Sign` in place but mark it `#[deprecated]` too. Both go away in Phase 7.
- Delete `borg/clients/extension/popup.html` and `popup.js` (dead since commit `d901af8` removed `default_popup`; nothing references them).
- Unit tests in `borg/src/extension/manifest/tests.rs`:
  - `build_manifest()` returns a JSON object with `version` equal to `env!("CARGO_PKG_VERSION")`.
  - The set of origins appearing in `host_permissions` (parsed as `http://{host}/*` → `{host}`) equals the set in CSP `connect-src` (parsed as `http://{host}:*` → `{host}`). This is the structural-non-drift assertion.
  - All `ORIGIN_PATTERNS` entries appear in both lists.

At end of Phase 1 the extension still ships from the working `bin/extension-sign` path (which now produces a correctly-regenerated manifest because the generator path bug is fixed). Behavior unchanged for the user; correctness restored under the hood.

#### Phase 2: Add `sb borg extension` subcommand group
**Model:** sonnet

- Create `sb/src/cli/borg/extension.rs` with the `ExtensionCommand` enum from API Design.
- Wire each verb to its `borg::extension::*` function. Pure shell: parse args, call, print result, map errors to exit codes.
- Add `extension(ExtensionArgs)` variant to the existing `Command` enum in `sb/src/cli/borg.rs`.
- Delete `Command::Sign` and the inline handler that printed "Signing extension v{}".
- Delete `bin/extension-sign` and `borg/clients/extension/sign.sh`.
- Update `.otto.yml`'s `extension` task to call `sb borg extension install`.

#### Phase 3: Schema generation + build-time body check + options-page validation
**Model:** opus

- Add `#[derive(JsonSchema)]` to `borg::types::IngestRequest`.
- `cargo add schemars` (workspace dep).
- Implement `borg::extension::schema::build_schema`; wire into `generate`.
- Write `ingest-schema.json` alongside `manifest.json`.
- **NO runtime JS schema validator.** `background.js` stays as-is for body construction. Per the Architect's Round 1 critique: shipping Ajv or a hand-rolled schema evaluator into the browser is a build-time invariant masquerading as a runtime check.
- Add the build-time gate: `borg/tests/extension_body_matches_ingest_request.rs` deserializes the canonical extension body (`{"url": "https://example.com/"}`) into `IngestRequest`. Test fails loudly with an instructive message if a future required field is added.
- Update `options.js`: implement save-time `chrome.permissions.contains` check (covers the options-page drift surface; unrelated to the schema check above).
- Unit-test: schema round-trips through `serde_json`; the body-matches test catches additive-required drift.

#### Phase 4: Install verb + Firefox Enterprise Policy
**Model:** opus

- Implement `borg::extension::install::detect_firefox()` → `enum FirefoxInstall { Tarball(PathBuf), AptDeb, Snap, Flatpak, Unknown }`. Resolves via `readlink -f $(which firefox)` and inspects the path.
- Implement `borg::extension::install::policy_path(install: &FirefoxInstall) -> Result<PathBuf>` per the table in API Design. Returns an error for `Snap` ("known broken: `file://` install_url blocked by snap confinement; switch to apt or tarball") and `Unknown` ("could not detect Firefox install type; supported: tarball, apt, flatpak").
- Implement `borg::extension::install::run(repo_root, opts)`:
  0. **If `opts.if_installed`**: detect Firefox install + compute policy path (or use `opts.policy_file`). Read the policy file; if it doesn't exist OR doesn't contain `policies.ExtensionSettings["obsidian-borg@scottidler"]`, return `Ok(InstallResult { policy_path: None, firefox_restart_required: false, ..Default::default() })` immediately - this is a daemon-only machine where the extension was never installed; the `otto deploy` hook short-circuits without signing, without sudo, without touching Firefox. If the entry IS present, continue with the normal flow below (but `opts.no_policy` is typically also set in the `--if-installed` flow, so step 3 will short-circuit the policy write too; the symlink swap still happens because that's the point of the deploy hook).
  1. Call `sign::run(repo_root)` (which calls `generate` and produces a versioned .xpi).
  2. Update the `obsidian-borg-latest.xpi` symlink atomically: `std::os::unix::fs::symlink(versioned, tmp)` + `std::fs::rename(tmp, latest)`. Rename-over-symlink is atomic on POSIX.
  3. If `opts.no_policy` → return `Ok(InstallResult { policy_path: None, firefox_restart_required: false, ... })`.
  4. Verify the just-signed `-latest.xpi` target exists and is readable. If the symlink dangles, bail with "sign produced no .xpi" (defends against a silent web-ext failure).
  5. Compute target policy path: `opts.policy_file` if set, else auto-detect Firefox install and use the path from the table above. The override exists so users on machines where `/etc/firefox/policies/policies.json` is owned by Ansible/Puppet/Chef can point us at a `policies.d/` drop-in their management tool merges, instead of racing the management agent on the canonical file.
  6. Read existing policy file if present and parse as JSON. **Deep-merge** our entry into `.policies.ExtensionSettings["obsidian-borg@scottidler"]` - do not overwrite the file or sibling entries. Preserves any other policies the user has set (password managers, certificate roots, etc.).
  7. If the merged JSON equals the existing JSON byte-for-byte → policy is current, return `Ok(...)` without writing. Otherwise: write the merged JSON via the path-appropriate mechanism (`sudo tee` for `/opt/firefox/` and `/etc/firefox/`; direct write for flatpak's user-level path).
  8. Set `firefox_restart_required: true` only if the policy file changed (writing the same content twice doesn't require restart; updating the symlink that the policy points at doesn't either, Firefox re-fetches on its own).
- No automatic Firefox-reload signal: Firefox's extension reload from outside the browser is not a supported API. The user restarts Firefox when convenient; the message at the end of `install` says "policy updated; Firefox will load the new extension on next launch" or "symlink updated; Firefox will pick up the new .xpi on its next update check (~24h) or on next launch."
- `cfg(target_os = "linux")` gate on the whole `install` module. Other targets get a clear `unimplemented!("install verb is Linux-only; macOS/Windows users use sign + manual .xpi install")`.
- E2E test (manual checklist in the doc): on a clean Firefox tarball install with no extensions, `sb borg extension install` results in policy file written; restart Firefox; observe extension installed and pinned to navbar with no install dialog; press Alt+Shift+B on `https://example.com`; observe note created in vault.

#### Phase 5: CI drift gate
**Model:** sonnet

- Add `extension-validate` task to `.otto.yml`; it runs `sb borg extension validate`.
- Add `extension-validate` to the `ci` task's `before` list (so `otto ci` always runs it). Existing `check` and `test` stay as separate concerns.
- The `validate` verb exits 0 if `manifest.json` and `ingest-schema.json` match what the generator would produce, non-zero with a `--- a/file +++ b/file` unified diff on stdout otherwise.
- Document in CLAUDE.md (project section, not user): "any change to `borg/src/extension/manifest.rs`, `ORIGIN_PATTERNS`, or `borg::types::IngestRequest` requires `sb borg extension generate && git add borg/clients/extension/manifest.json borg/clients/extension/ingest-schema.json`."
- Optional pre-commit hook in `~/.git/hooks/pre-commit`: runs `sb borg extension validate`. Not committed (lives in the user's repo's `.git/hooks/`); the CI gate is the enforcement mechanism. The hook is convenience.

#### Phase 6: Bootstrap + otto-deploy integration
**Model:** sonnet

- Teach `sb bootstrap` to optionally install the extension: `sb bootstrap --extension` calls `sb borg extension install` after the standard bootstrap steps. First-machine setup: `sb borg daemon --install` + `sudo sb borg extension install` (or `sudo sb bootstrap --extension`) is the full personal-deployment sequence.
- **Add the otto deploy hook.** Append to `.otto.yml`'s `deploy` task (which today builds sb, installs it to `~/.cargo/bin/`, syncs fabric patterns and canonical tags to `~/.config/sb/`, and restarts existing borg/cortex systemd units):

  ```yaml
  deploy:
    # ...existing build + install + systemd-restart steps...
    - sb borg extension install --no-policy --if-installed
  ```

  The `--if-installed` flag (Phase 4) short-circuits to a no-op when this machine doesn't have the extension installed (e.g., a daemon-only server with no Firefox), so the same `otto deploy` is safe to run everywhere. The `--no-policy` flag skips the policy file write (no sudo required), since the policy is already in place on any machine where `--if-installed` finds the extension. Net effect of a single `otto deploy` invocation on the user's machine: rebuild sb, install it, sync config, restart systemd units, **re-sign the extension and atomically swap `obsidian-borg-latest.xpi`**. Firefox's file-watch picks up the swap and re-installs the extension immediately, with no operator action.
- Document the canonical workflow in `docs/extension-signing.md`:
  - First-machine setup (one-time, requires sudo): `sudo sb borg extension install`
  - Day-to-day (zero-sudo, zero-operator-action): `bump && otto deploy` - the extension follows the bump automatically.
  - Daemon-only servers: nothing changes; `otto deploy` no-ops on the extension step.

#### Phase 7: Cleanup
**Model:** sonnet

- Delete `borg::sign` (the `#[deprecated]` shim from Phase 1).
- Delete `Command::Sign` from `sb/src/cli/borg.rs` and its handler arm. `sb borg sign` becomes unknown - users get the standard clap "unknown subcommand, did you mean: extension" error.
- Update `docs/extension-signing.md`: replace the `bin/extension-sign` instructions with `sb borg extension install`. Keep the AMO credential setup section verbatim; only the invocation changes.
- Bump workspace version via the existing `bump` flow. The manifest version follows automatically (it's `env!("CARGO_PKG_VERSION")`).
- Run `sb borg extension install` once to verify the full end-to-end. Commit the regenerated `manifest.json`, `ingest-schema.json`, the new versioned `web-ext-artifacts/obsidian-borg-{version}.xpi`, and the moved `-latest.xpi` symlink target.

`popup.html` and `popup.js` were deleted in Phase 1 (they were dead before this work started).

#### Treatment of `borg/clients/extension/web-ext-artifacts/`

- `web-ext-artifacts/*.xpi` is **gitignored**. Versioned `.xpi` files are build outputs; they're large (current 0.4.0 .xpi is 5.6 MB), bumping the version every release would balloon the git history. They're reproducible from source + AMO so no information is lost.
- `obsidian-borg-latest.xpi` (the symlink) is also gitignored.
- `.amo-upload-uuid` is **committed**. It's the AMO listing identifier and must persist across machines and clones to keep signing the same listing.
- The signed-and-installed extension only needs to exist on the machine that runs the daemon; remote machines that only consume `sb borg extension install` get a fresh sign per machine anyway.

## Alternatives Considered

### Alternative 1: Keep the existing shell-script flow, just fix the manifest

- **Description:** Edit `borg/clients/extension/manifest.json` by hand to restore the missing CSP and `*.lan` entries. Leave `bin/extension-sign`, `sign.sh`, and the shell-driven flow in place.
- **Pros:** Smallest possible change. Ships in one commit. Existing muscle memory unchanged.
- **Cons:** Closes one drift surface (CSP/host) and leaves seven open. Doesn't address version drift, install drift, schema drift, endpoint-validation drift. Hand-edited manifest can drift again the next time someone touches it.
- **Why not chosen:** The user explicitly asked for "no half measures." A targeted fix to one drift surface is the half measure they ruled out.

### Alternative 2: Treat manifest.json as generated, gitignore it

- **Description:** Generator owns the manifest; `manifest.json` is added to `.gitignore`; `sign.sh` always regenerates first.
- **Pros:** Can't be hand-edited. One less file in git.
- **Cons:** This is exactly the obsidian-borg failure pattern (commit `5870387`) replayed: a gitignored generated file invites hand-editing on the side, and the edits vanish on the next clean checkout or directory copy. Even committed-but-CI-validated is safer because hand-edits get reverted noisily on the next regen instead of evaporating silently.
- **Why not chosen:** Repeats the original sin. The right pattern is "generated, committed, CI-enforced," not "generated, gitignored."

### Alternative 3: Tie manifest version to a dedicated extension version const

- **Description:** Extension version is its own `const EXT_VERSION: &str = "0.5.0"` in the generator, bumped independently from the workspace.
- **Pros:** Extension protocol can evolve at a different cadence than borg internals. Reflects the conceptual fact that an extension protocol change is rare.
- **Cons:** Adds a second version to keep track of. Requires extending the `bump` tool with knowledge of this const. Resurrects "what's the actual version of the thing I'm running" confusion. The protocol-vs-version concern is solved better by Goal 7 (JSON-schema validation) than by a separate version number.
- **Why not chosen:** Coupling extension version to workspace version is precisely the "make drift impossible" answer the user asked for. Cost of unnecessary re-signs is low (it's automated and unattended); cost of version-drift bugs is high.

### Alternative 4: Use `web-ext run` for development, AMO sign only for "real" installs

- **Description:** Day-to-day dev uses `web-ext run` with a temporary profile. The signed-and-installed path is only for the user's primary browser.
- **Pros:** Faster dev loop (no AMO round trip per change). Doesn't touch the user's primary profile.
- **Cons:** Doesn't help with the actual problem (the user IS running it as their daily extension on their primary profile). Adds a parallel dev path that itself can drift from the signed path. Tests the wrong thing.
- **Why not chosen:** The user's workflow is the signed path. Optimizing the dev path doesn't move the needle on the actual failure modes.

### Alternative 5: Auto-discover the daemon via mDNS / .well-known

- **Description:** Extension publishes `_borg._tcp.local` via the daemon, extension discovers via mDNS, no text-field endpoint configuration at all.
- **Pros:** Zero-configuration. Topology changes need zero extension config changes.
- **Cons:** mDNS adds a dependency (avahi on Linux, Bonjour on macOS); Firefox MV3 has no native mDNS API so the extension would need to bridge through the daemon; substantial new code surface for a problem that doesn't actually bite the user today.
- **Why not chosen:** Overengineered for a two-machine personal deployment. The text-field config is fine; the failure mode (silent NetworkError on misconfig) is closed by save-time validation, not auto-discovery.

## Technical Considerations

### Dependencies

**New:** `schemars` (for `JsonSchema` derive on `IngestRequest`). Add via `cargo add schemars`.

**Existing, leveraged more:** `web-ext` CLI (already required, doc'd in `docs/extension-signing.md`); `serde_json` (already pervasive); `firefox >= 140` (already declared in manifest's `strict_min_version`).

**New external system:** Firefox Enterprise Policy file at `/etc/firefox/policies/policies.json`. Requires sudo on first write; no further sudo for subsequent .xpi updates (only the symlink moves).

### Performance

`generate` and `validate` are pure in-memory + one file diff each; sub-millisecond. `sign` is gated by AMO round-trip latency (~10-30 seconds, unchanged from today). `install`'s symlink update is atomic and instant; the policy write is one shell-out to `sudo tee`. Firefox extension reload via `pkill -USR1` is essentially free. The whole `install` happy path is dominated by `web-ext sign`.

### Security

Three considerations:

1. **Sudo for policy writes.** First-run `install` shells out to `sudo tee`. This is the standard pattern for "write a system file from a user-space tool"; the user is on their personal machine and has sudo. Subsequent installs don't need sudo (only the `-latest.xpi` symlink moves; the policy's `install_url` doesn't change).

2. **Broad host patterns.** `*.lan` and `*.local` permit the extension to talk to any LAN host. Personal-use trade-off: the user controls all hosts on their LAN; the extension only acts on Alt+Shift+B (no background fetches). Acceptable. Other users with shared LANs (corporate, dorm) would want narrower patterns - documented in CLAUDE.md.

3. **`file://` install URL.** The policy points at a path on the user's filesystem. Anyone with write access to that path can replace the .xpi with a malicious one. The path is under `~/repos/scottidler/second-brain/borg/clients/extension/web-ext-artifacts/`, which has the user's normal file permissions. This is the same trust boundary as anything else in the repo. Acceptable for personal use; not acceptable for multi-user machines (documented).

### Testing Strategy

- **Unit tests** in `borg::extension::*` for: manifest construction (origins/CSP agreement; version pulled from `CARGO_PKG_VERSION`), schema generation (round-trip through `serde_json`), validate (drift detection on synthetic stale file).
- **Integration test** for `validate`: write a hand-edited `manifest.json` to a tempdir, run `validate`, assert non-zero exit + diff output.
- **Manual E2E** for `install`: clean Firefox profile, run `sb borg extension install`, observe extension installed and pinned, fire Alt+Shift+B on a test page, confirm the daemon receives the request.
- **CI gate** via `.otto.yml`'s `validate` task: regenerates and diffs. Prevents merging a manifest-touching commit without the regenerated artifacts.

### User-Visible Migration

After each phase merges, what the operator on lappy/desk does:

| Phase merged | What the operator does | What changes for the user |
|---|---|---|
| 1 | Nothing | None - `otto extension` still calls the shell script under the hood. Path bug fixed. |
| 2 | Nothing | None - `otto extension` now calls `sb borg extension sign` instead of the shell script; same outcome. |
| 3 | `sb borg extension install` once to pick up the new options.js and ingest-schema.json | Save in options now validates; bad endpoint gets rejected at save time. |
| 4 | `sudo sb borg extension install` once (sudo needed for first policy write) | Future `install`s no longer need sudo; Firefox watches the `-latest.xpi` symlink and re-installs the extension automatically the moment the symlink swap completes (per Mozilla `file://` install_url semantics). |
| 5 | Nothing | `otto ci` will catch any manifest/schema drift in a PR. |
| 6 | On a new machine: `sudo sb bootstrap --extension`. On existing machines: nothing - `otto deploy` now auto-runs `sb borg extension install --no-policy --if-installed` as a post-deploy step. | New-machine setup is one command; day-to-day `bump && otto deploy` automatically re-signs the extension and Firefox file-watch picks it up. Zero ongoing operator action. Daemon-only servers no-op silently. |
| 7 | Nothing | `bin/extension-sign`, `sign.sh`, `Command::Sign` removed; `sb borg sign` is gone. |

### Rollout Plan

Phases land in order; each is independently mergeable.

1. **Phase 1** (module move) is mechanical and ships first. After this commit, the generator lives in the right place and the path bug is fixed; the user can run `sb borg sign` and have it work end-to-end against the regenerated manifest.
2. **Phase 2** (sb subcommand) lands the new CLI surface. After this commit, `sb borg extension sign` works and `otto extension` calls it; `bin/extension-sign` and `sign.sh` are deleted in the same commit.
3. **Phase 3** (schema + runtime validation) lands the JSON-schema generation and the options.js / background.js updates. Requires re-signing and reinstalling the extension to take effect, but the daemon side ships independently.
4. **Phase 4** (install verb + policy) lands the full automation. The user runs `sb borg extension install` once with sudo; from then on every `sb borg extension install` is unattended.
5. **Phase 5** (CI gate) lands the drift enforcement.
6. **Phase 6** (bootstrap + otto-deploy hook) lands the new-machine path AND the post-deploy auto-refresh hook (`sb borg extension install --no-policy --if-installed` appended to the `deploy` task). After this commit, `bump && otto deploy` is the entire day-to-day workflow.
7. **Phase 7** (cleanup) removes dead code (popup.html, popup.js, deprecated shims, old Command::Sign).

Each phase is `otto ci`-validated before merge. Phase 4 has a manual E2E component (signed Firefox install in a real browser); the rest are unit-tested.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Firefox install method changes (tarball → snap) and policy path moves | Low (user uses tarball) | High (install verb silently writes to wrong path; extension never appears) | Phase 4 install detection runs on every `install` invocation - `readlink -f $(which firefox)` resolves the install root. If the detected path doesn't match the path we previously wrote a policy to, fail loudly: "Firefox install moved from X to Y; remove old policy at /old/path and re-run install." Snap explicitly errors with "snap-installed Firefox cannot load `file://` install_url - use apt or tarball Firefox." |
| `chrome.permissions.contains` doesn't exist in Firefox MV3 | Low | Med (options-page save validation breaks) | Verify in Phase 3 against Firefox 140+ docs (`browser.permissions.contains` is standard MV2/MV3 API per MDN). Fallback: regex-match origin against the patterns const compiled into background.js. |
| `JsonSchema` generation diverges from runtime serde behavior | Low | Med (extension validates against a wrong shape) | Round-trip test in unit tests: serde-serialize a known-good IngestRequest, then validate that the schema accepts the serialized JSON. Catches drift between `derive(Serialize)` and `derive(JsonSchema)`. |
| `web-ext sign` fails (AMO down, JWT expired) | Med | Low (sign verb fails) | Already handled today: `web-ext sign` returns non-zero, the wrapper bails with the AMO error. Surface stays the same. |
| User edits manifest.json directly to test a one-off change | High | Low (CI catches it on next commit) | CI `validate` task surfaces the diff. Document the workflow: change `ORIGIN_PATTERNS` and `sb borg extension generate`, not the manifest. |
| Bump tool doesn't run `extension generate` after bumping workspace version | High | Low (manifest version drifts from Cargo.toml) | Either: (a) teach `bump` to invoke `sb borg extension generate` after writing Cargo.toml, OR (b) CI `validate` catches it on the next push (which is enforced by the merge gate, so it can't slip past). (b) is simpler and sufficient. |
| User's existing `policies.json` has other unrelated policies (password manager, cert pinning) | Low | High (overwriting destroys user's other policies) | Implementation deep-merges into `policies.ExtensionSettings[<our-id>]`, never overwrites the file. Read existing JSON, modify only our subtree, write the merged result. Round-trip preserves formatting via `serde_json::to_string_pretty`. |
| `web-ext sign` succeeds but produces an .xpi filename we don't expect | Low | Med (`-latest.xpi` symlink points at nothing) | After sign, glob for `web-ext-artifacts/*-{version}.xpi`; assert exactly one match; symlink target = that file. Fail loudly if zero or multiple matches. |
| User's `chrome.storage.local.endpoint` is set to an old value that no longer matches a current `ORIGIN_PATTERNS` pattern (e.g., user added a new machine and removed `*.lan`) | Low | Med (existing config silently broken) | The save-time check covers NEW values, not stored ones. On install, `background.js` re-validates the stored endpoint against current permissions on extension startup; if it fails, fire a "stored endpoint no longer permitted by manifest" notification and clear the value. User reconfigures via options. |
| Firefox `installation_mode: force_installed` references a missing/deleted `.xpi` at `install_url` | Low | High (extension silently fails to load; user sees nothing in toolbar) | `install` verb verifies the symlink target exists before writing the policy. `uninstall --purge` is the only sanctioned way to remove the .xpi. If the user manually deletes `web-ext-artifacts/`, they get an extension-load error in `about:debugging`. |
| Multiple Firefox installs on one machine (e.g., apt + flatpak) | Low | Med (only one gets the policy) | `install` reports which Firefox install it detected and acted on. Document: install affects the Firefox on `$PATH`; use `PATH=... sb borg extension install` to target a different one explicitly. |
| Firefox running with old extension while new .xpi is signed | Low | Low | Per Mozilla policy-templates docs, Firefox watches the file at `install_url` and re-installs whenever the .xpi at that path changes. The atomic symlink swap (Phase 4) is itself the update signal; Firefox detects the new file and reloads the extension without restart, polling delay, or external signal. The earlier `pkill -USR1 firefox` claim was bogus (caught by Architect Round 2) and has been removed; the Mozilla file-watch mechanism makes it unnecessary anyway. Worst case: Firefox is mid-fetch when the symlink swap completes and re-installs on the next file-watch tick (sub-second). |
| `*.lan` pattern doesn't actually match user-typed `desk.lan:8181` in `chrome.permissions.contains` | Low | High (save-time check always rejects) | The MV3 origin match pattern is documented (MDN match patterns); `http://*.lan/*` matches `http://desk.lan/*`. Verify in Phase 3 unit tests by mocking the API or testing in `web-ext run`. |
| Concurrent writer (Ansible/Puppet/Chef) on `/etc/firefox/policies/policies.json`; our `sudo tee` read-modify-write races the management agent and corrupts global browser policy | Low (this user has no config-mgmt agent on their laptop) | High (broken browser policy affects every Firefox feature, not just our extension) | Two mitigations: (a) the `--policy-file` flag (default `None`, auto-detect) lets managed-environment users target a `/etc/firefox/policies.d/obsidian-borg.json` drop-in their management tool merges in, instead of the canonical file. (b) Write is atomic-on-rename (write to `policies.json.tmp` in the same directory, `rename(2)` over the original); if a concurrent writer interleaves they win the rename race instead of corrupting the file mid-write. The atomic-rename property bounds corruption to "last writer wins" rather than "garbled middle." Document the flag in `docs/extension-signing.md` under a "Managed-environment installs" section. |

## Resolved Decisions

All design questions raised during drafting and review have been resolved against primary sources. The doc has no remaining open questions; implementation can proceed.

### Closed during drafting + Architect Rounds 1-2

- **Schema validation location.** Runtime in-browser JSON Schema validation rejected per the Architect's anti-pattern flag (Round 1). Replaced with build-time Rust integration test `borg/tests/extension_body_matches_ingest_request.rs`. See Phase 3 and the Schema generation section.
- **`ORIGIN_PATTERNS` hardcoding regression.** Const-only approach rejected per Architect Round 1. Replaced with config-driven `origin_patterns(config)` derivation that preserves the existing `borg/src/lib.rs:787` behavior (`server.host` participation) and adds explicit `extension.origin-patterns` override. See the Data Model section.
- **Concurrent writer on managed `/etc/firefox/policies/policies.json`.** Per Architect Round 1: added `--policy-file` override + atomic-rename write. See the Risks table.
- **`pkill -USR1 firefox` as an immediate-reload signal.** Bogus claim caught by Architect Round 2 and removed. SIGUSR1 is not handled by Firefox on Linux; sending it either does nothing or terminates the browser. Replaced with the Mozilla-documented file-watch mechanism (see next item).
- **Whether Firefox needs a restart or polling interval to pick up a new .xpi when only the symlink target changes.** Resolved against `mozilla/policy-templates` `docs/index.md` (ExtensionSettings → `install_url`): "Firefox will update or re-install the extension whenever the XPI file at that path changes." Firefox watches the file at `install_url`. The atomic symlink swap is itself the update signal. No restart, no polling delay, no external trigger. See the Architecture → Firefox Enterprise Policy section.

### Resolved during cleanup pass

- **`chrome.permissions.contains` pattern subsumption.** Resolved against MDN `permissions.contains` docs (verified by direct fetch): "For host permissions, if the extension's permissions pattern-match the permissions listed in `origins`, then they are considered to match." MDN's worked example shows `https://developer.mozilla.org/` matching `*://*.mozilla.org/*`. Therefore `permissions.contains({origins: ["http://desk.lan/*"]})` returns `true` when the manifest declares `host_permissions: ["http://*.lan/*"]`. The options-page save-time check works as designed. Risk-table fallback (regex-match against patterns) retained as defense-in-depth but is not expected to fire.
- **`policies.json` `updates_disabled` setting.** Resolved against Mozilla policy-templates docs: setting `true` "disables automatic updates for an individual extension," which for a `file://` `install_url` disables Firefox's file-watch and breaks the symlink-swap auto-update mechanism. Therefore: `updates_disabled: false` (leave at Mozilla default). Earlier draft recommended `true` based on a misread (the docs are about polling AMO `update_url` XML, not about file:// install_urls); corrected during cleanup.
- **Daemon `/health` `schema_version` field.** Out of scope for this design. Tracked separately as a daemon-protocol concern. Not a blocker for the extension lifecycle work.
- **Move `borg/clients/extension/` to top-level `clients/extension/`?** No. The borg crate owns the `IngestRequest` schema and the manifest version (`env!("CARGO_PKG_VERSION")`); promoting the extension out of the borg directory inverts the dependency direction. Keep at `borg/clients/extension/`.
- **Teach the workspace `bump` tool to auto-invoke `sb borg extension generate` post-bump?** No. The CI drift gate (`sb borg extension validate` in `otto ci`) is sufficient and avoids cross-tool coupling between `bump` and the extension lifecycle. If the user bumps and forgets to regenerate, the next push fails CI with a clear diff. Bump stays unaware of the extension.

### Protocol evolution rule (locked-in)

**`borg::types::IngestRequest` evolves additively-only.** Adding an `Option<>` field is non-breaking (serde tolerates absence and unknown fields by default; verified no `#[serde(deny_unknown_fields)]` on the type). Adding a required (non-`Option<>`) field is a breaking change to every client and requires a coordinated extension re-sign + redeploy in the same PR; the build-time `extension_body_matches_ingest_request` test fails at CI otherwise. This rule is what makes Phase 3's deletion of runtime client-side validation safe: required-field additions cannot land silently because CI blocks them.

## References

- Existing implementation: `borg/src/lib.rs:781` (`generate_manifest`), `borg/src/lib.rs:869` (`sign`), `sb/src/cli/borg.rs:53` (`Command::Sign`), `bin/extension-sign`, `borg/clients/extension/sign.sh`, `.otto.yml` `extension` task.
- Prior doc: `docs/extension-signing.md` (current credential setup, signing flow - to be partly superseded).
- Firefox Enterprise Policy: <https://mozilla.github.io/policy-templates/> (`ExtensionSettings`, `force_installed`).
- Match patterns: <https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/Match_patterns>.
- `chrome.permissions.contains` MV3 API: <https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/API/permissions/contains>.
- `schemars` crate: <https://docs.rs/schemars>.
- Originating debugging thread: this conversation. Failure mode reproduced as `NetworkError when attempting to fetch resource` on lappy → desk.lan, root-caused to missing `content_security_policy.extension_pages` in committed 0.4.0 manifest after port from obsidian-borg.
