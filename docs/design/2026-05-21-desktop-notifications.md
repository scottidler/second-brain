# Design Document: daemon-driven desktop notifications (parity with Telegram)

**Author:** Scott Idler
**Date:** 2026-05-21
**Status:** Implemented
**Review Passes Completed:** 5/5 + Advisor round 1 + Architect rounds 1-2 (all findings absorbed; round 2 approved)

> **Post-implementation amendment (2026-05-21):** During Phase 2 the
> structure / field names spelled out in this doc (`Notifier`,
> `DesktopNotifier`, `DesktopNotifierConfig`, AppState fields
> `notifier`/`desktop_notifier`, YAML key `desktop-notifier:`) were
> overridden in favor of consistent single-word names that follow the
> general/rust naming rules. The shipped names are listed at the top of
> [Data Model](#data-model). The Non-Goal "do not rename the existing
> Telegram `Notifier` to `TelegramNotifier`" was retained in spirit
> (the existing struct WAS renamed - to `Telegram`, not
> `TelegramNotifier` - so the channel-named single-word convention is
> symmetric across both sinks).

## Summary

Restore the Linux desktop push notification that disappeared after the May 12, 2026 fire-and-forget refactor of borg's HTTP intake (commit `fa79724`). Add a `DesktopNotifier` sink alongside the existing Telegram `Notifier`, fired from the same call sites in the daemon with the same content, so the two channels stay aligned by construction. Daemon-side delivery removes the cross-process dead-data dependency that broke the Firefox extension's notification path; the extension reverts to its only remaining responsibility (sending the URL).

## TL;DR

- New `DesktopNotifier` in `borg/src/notify.rs`, peer to the existing Telegram `Notifier`. Same `processing(...)` and `result(...)` semantics. Backed by `notify-rust = "4"` (already a dependency, no new crates).
- `AppState` grows a sibling field `desktop_notifier: Option<DesktopNotifier>`. Every existing call site that fires `notifier.processing(...)` or `notifier.result(...)` adds a parallel `desktop_notifier.processing(...)` / `desktop_notifier.result(...)`. Side-by-side, not behind a trait.
- Config-gated with `desktop-notifier: { enabled, host, timeout-ms, appname }` mirroring telegram/discord/ntfy host-gating, so a headless borg host stays silent instead of fighting a non-existent D-Bus session.
- Firefox extension stops trying to render result toasts. `background.js` keeps only the "borg unreachable" error toast - the one failure the daemon cannot deliver.
- `sb borg ingest` CLI helper stops firing its own `notify_rust` calls; the daemon now owns the notification lifecycle for both Telegram and desktop. No double-fire.
- `sb borg --help` REQUIRED TOOLS gains a `notify-send` row as a *runtime-dependency proxy* (we don't shell out; the binary's presence indicates the user's libnotify stack is sane and gives them a diagnostic one-liner when toasts go missing).

## Problem Statement

### Background

Borg's HTTP `/ingest` originally awaited the full pipeline inside the request handler and returned the terminal `IngestResult` with `title` and `status: Completed | Failed | Duplicate`. The Firefox extension's `background.js` rendered a `chrome.notifications.create(...)` toast from that response, giving the user a per-URL "Captured: <title>" or "Failed: <reason>" desktop notification on whichever machine ran Firefox.

Commit `fa79724` (May 12, 2026, "fix(borg): fire-and-forget HTTP /ingest /note /multipart") made `/ingest` detach the pipeline onto a `tokio::spawn` task. The fix was correct: Firefox MV3 service workers recycle after about 30 seconds of idleness, and the 15+ minute YouTube transcription pipeline was getting cancelled mid-flight when the service worker was recycled. Detaching the pipeline preserved progress after the client gave up. The HTTP response now returns within milliseconds with `IngestStatus::Queued` and no `title`.

The Firefox extension was never updated to match. Empirical verification against the live v0.8.10 daemon on desk.lan:

```
$ curl -X POST http://localhost:8181/ingest \
       -H 'Content-Type: application/json' \
       -d '{"url":"https://example.com/x"}'
{"status":"Queued","note_path":null,"title":null,"tags":[],"canonical_url":"https://example.com/x","trace_id":"ht-4ed581"}
```

`background.js:24-32` checks `result.title` (null → falsy) and then `result.status.Failed` (the `Failed` discriminant is undefined on a bare string `"Queued"` → falsy). Both branches fall through silently. The user lost the per-URL desktop toast and has not had one on the happy path since May 12. The only branch that still fires is `catch (err)` → "Error: ..." when the daemon itself is unreachable.

The Telegram channel still works because the detached spawn task calls `notifier.result(&result, ...)` from inside the spawned future when the pipeline terminates - the bot delivers from the daemon side, not the response side, and is unaffected by what the HTTP handler returns.

### Problem

The desktop and Telegram notification channels were structurally decoupled. They each had their own producer and their own contract:

- **Telegram** was daemon-driven from the spawned task. Producer: `notifier.processing` (synchronous before spawn) + `notifier.result` (from inside the spawned future). Contract: in-process, async-method call. Survived the refactor.
- **Desktop** was client-driven from the HTTP response. Producer: `chrome.notifications.create(...)` in the extension. Contract: cross-process, the response JSON shape. Silently broken by the refactor because the shape changed and the extension was never updated.

The user explicitly requested parity: both channels should fire the same two messages Telegram fires today (a `[trace_id] Processing...` toast at intake, then a `Saved/Duplicate/Failed: <title>` toast at terminal).

A correct fix has to do more than re-implement the desktop side - it has to remove the dead-data-dependency that caused the bug in the first place. The desktop channel must move into the daemon next to the Telegram channel, so any future refactor of the request/response contract physically cannot break a notification sink in isolation.

### Goals

- Restore the Linux desktop push notification for every intake path (HTTP, ntfy, Telegram, Discord), with content identical to what Telegram delivers today.
- Keep the fire-and-forget HTTP contract intact - the May 12 fix is load-bearing for pipeline survival and we are not undoing it.
- Move desktop notification production into the daemon so the two sinks share one producer, one trigger, and one bug surface.
- Mirror Telegram's host-gating model so the desktop notifier only runs on the machine with the actual desktop session.
- Reintroduce a `notify-send` row in `sb borg --help`'s existing `REQUIRED TOOLS:` block so users have a diagnostic anchor when toasts go missing.

### Non-Goals

- A new HTTP polling endpoint (`GET /trace/:trace_id`) or SSE stream (`GET /trace/:trace_id/events`). Both work in principle but reintroduce the cross-process dead-data dependency this design exists to eliminate.
- An abstract "notification sink" trait taking `Vec<Box<dyn Sink>>`. Two sinks does not justify a trait, and the Rust convention here is generics over trait objects. Revisit when a third channel arrives.
- Renaming the existing `Notifier` to `TelegramNotifier`. Mechanically large, not load-bearing for this change; the file-level docstring on `notify.rs` will spell out which struct is which.
- Notification rate-limiting for bulk reingest. Documented as a known limitation; Telegram has the same property and the user is aware. Add `desktop_notifier.batch_mode` later if it becomes painful.
- Cataloguing every external binary borg depends on (fabric, yt-dlp, ffmpeg, markitdown, whisper) in `REQUIRED TOOLS:`. The catalog gap exists but is broader than this design - tracked as an Open Question. This design only adds `notify-send`.

## Proposed Solution

### Overview

A `DesktopNotifier` lives in `borg/src/notify.rs` next to the existing Telegram `Notifier`. It carries an appname and a timeout, and exposes the same two methods Telegram exposes (`processing`, `result`) so the call-site pattern is mechanical: copy the line, change `n.` to `d.`. The daemon constructs both notifiers at `serve_init` time if their config blocks are present and host-gates pass. Every existing place that calls `notifier.processing/result` gains a parallel `desktop_notifier.processing/result` call right next to it.

The Firefox extension stops trying to render terminal toasts. Its only remaining job is sending the URL and surfacing transport-layer errors ("can't reach borg") that the daemon by definition cannot deliver.

The `sb borg ingest` CLI helper stops firing its own `notify_rust` calls. The daemon now produces the toast; firing one in the CLI too would double-fire on the local machine.

### Architecture

```
                         +-----------------+
                         |  borg daemon    |
HTTP /ingest      ------>+                 |
ntfy event        ------>+ record_intake   |
Telegram message  ------>+ (sync, at door) |
Discord message   ------>+                 |
                         |    |            |
                         |    v            |
                         | tokio::spawn    |
                         |    |            |
                         |    v            |
                         |  pipeline       |
                         |    |            |
                         |    v            |
                         |  IngestResult --+--> Notifier        (Telegram, HTTP/HTTPS)
                         |                 +--> DesktopNotifier (notify-rust → user D-Bus → notification daemon)
                         +-----------------+

                         Firefox extension: POST /ingest, render "Error: ..." on transport failure only.
                         sb borg ingest CLI: POST /ingest, print result; no notify_rust calls.
```

Every intake path is unchanged except for the parallel `desktop_notifier.processing/result` call right next to the existing Telegram call.

### Design Invariants

Two structural rules govern every notification call site in this design. Both apply to BOTH sinks (Telegram and desktop):

1. **Notifications never run on the inbound-handler hot path.** Today's `routes.rs:64-66` awaits `n.processing(...)` *before* `tokio::spawn`, which couples the HTTP `/ingest` response latency to whichever external service is slowest (Telegram HTTPS round trip today; D-Bus tomorrow). The May 12 refactor moved the *pipeline* off the hot path but left the processing-notification on it. This design promotes "off-hot-path" to a hard invariant: every `notifier.processing(...)` and `desktop_notifier.processing(...)` lives *inside* the `tokio::spawn` block, immediately before `pipeline::process_content(...)`. Ordering is preserved (the pipeline future won't start until the prior `.await`s yield); the HTTP response returns sub-millisecond regardless of notification-channel health.
2. **Every notification call is timeout-bounded.** A wedged notification daemon (D-Bus default timeout: ~25 seconds) or a slow Telegram round trip should not delay the pipeline. Every `processing(...)` and `result(...)` call wraps in `tokio::time::timeout(Duration::from_millis(500), ...)`, with the timeout-elapsed branch logged at `warn` and swallowed. The toast may not render; the pipeline still progresses. Implementation lives inside the notifier methods so call sites stay tidy.

### Data Model

**As shipped (post-rename per 2026-05-21 amendment):**

```rust
// borg/src/config.rs - new struct + optional field on Config
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct DesktopConfig {
    /// If false, no Desktop sink is constructed; daemon stays silent on the
    /// desktop. Telegram is unaffected. Default false so adding the config
    /// block is opt-in.
    pub enabled: bool,
    /// If set, only construct the sink on the host with this hostname.
    /// Mirrors the telegram/discord/ntfy host gating; keeps headless borg
    /// hosts from fighting a non-existent D-Bus session.
    pub host: Option<String>,
    /// Toast lifetime hint to the notification daemon. Default 5000.
    pub timeout_ms: u32,
    /// Application name shown by the notification daemon. Default "borg".
    pub appname: String,
}

impl Default for DesktopConfig {
    fn default() -> Self {
        Self { enabled: false, host: None, timeout_ms: 5000, appname: "borg".into() }
    }
}

// borg/src/config.rs - Config addition. YAML key is `desktop:`, parallel to
// `telegram:`/`discord:`/`ntfy:`.
pub struct Config {
    // ... existing fields ...
    pub telegram: Option<TelegramConfig>,
    pub discord: Option<DiscordConfig>,
    pub ntfy: Option<NtfyConfig>,
    pub desktop: Option<DesktopConfig>,  // NEW, optional like the others
    // ...
}

// borg/src/notify.rs - both sinks live here as single-word channel-named
// types. The pre-existing struct `Notifier` was renamed to `Telegram`.
#[derive(Clone)]
pub struct Telegram { /* Bot + ChatId */ }

#[derive(Clone)]
pub struct Desktop {
    appname: String,
    timeout: notify_rust::Timeout,
}

// borg/src/lib.rs - AppState fields renamed for symmetry
pub struct AppState {
    pub config: Arc<Config>,
    pub telegram: Option<Telegram>,  // was: notifier: Option<Notifier>
    pub desktop: Option<Desktop>,    // NEW, sibling
}

// borg/src/lib.rs - ServerStartup field renamed for symmetry
pub struct ServerStartup {
    pub addr: SocketAddr,
    pub telegram: SubsystemStatus,        // was: telegram_notifier
    pub telegram_bot: SubsystemStatus,    // unchanged - the inbound polling subsystem
    pub discord: SubsystemStatus,
    pub ntfy: SubsystemStatus,
    pub desktop: SubsystemStatus,         // NEW
    pub watchdog: SubsystemStatus,
}
```

The original design draft proposed `Notifier` / `DesktopNotifier` /
`DesktopNotifierConfig` and AppState fields `notifier`/`desktop_notifier`. Once
both sinks existed side-by-side, the asymmetry between bare `notifier`
(Telegram) and compound `desktop_notifier` (desktop) was unacceptable. The
shipped names follow the channel-named single-word convention used by the
other config sections, and the existing `Notifier` was renamed to `Telegram` as
part of the same pass. The "do not rename existing `Notifier` to
`TelegramNotifier`" Non-Goal was kept in spirit - the new name is `Telegram`,
not `TelegramNotifier`, so the suffix never enters the codebase.

### API Design

```rust
// As shipped: struct name is `Desktop`, config is `DesktopConfig`.
impl Desktop {
    /// Build a desktop sink from config. Returns `None` if
    /// `cfg.enabled == false` so the call sites can stay consistent with
    /// the Telegram `Telegram::new` pattern (Option<Self>).
    pub fn new(cfg: &DesktopConfig) -> Option<Self>;

    /// Fire the [trace_id] description popup at intake and return a handle
    /// to it. The handle is later passed to `result(...)` so the terminal
    /// popup REPLACES this one in place rather than stacking a second
    /// popup next to it (notify-rust's update-by-id pattern). Body is
    /// byte-identical to the Telegram processing message produced by
    /// `notify::Notifier::processing`. Returns `None` on D-Bus error
    /// (logged at warn, pipeline continues uninterrupted); a `None`
    /// handle later means `result(...)` falls back to creating a fresh
    /// popup. Internally wraps the show_async call in tokio::time::timeout
    /// (500ms) per the design invariant.
    pub async fn processing(
        &self,
        trace_id: &str,
        description: &str,
    ) -> Option<notify_rust::NotificationHandle>;

    /// Fire the terminal popup from an IngestResult. If `prior` is `Some`,
    /// mutate that handle in place (cleaner desktop UX, one popup per
    /// ingest) rather than creating a new one. If `None`, create fresh.
    /// Body reuses `crate::router::format_reply(result, display_source)` -
    /// the same source-of-truth Telegram uses via `format_telegram_reply` -
    /// so the rendered text is byte-identical between channels modulo HTML
    /// escape (Telegram is HTML, desktop is plain text). Wraps update/show
    /// in tokio::time::timeout (500ms).
    pub async fn result(
        &self,
        result: &IngestResult,
        display_source: &str,
        prior: Option<notify_rust::NotificationHandle>,
    );
}
```

Notes on the deliberate signature differences from `Telegram`:

- The `Telegram` methods take an extra `override_chat_id: Option<i64>` parameter that lets per-message rerouting (Telegram has a notion of "chat"; desktop does not). Dropping it on the desktop side keeps the call sites honest. The cost is one extra character at the call site (`t.processing(tid, desc, None)` vs `d.processing(tid, desc)`) which is preferable to wiring a parameter that has no semantic meaning on this channel and would have to be silenced with an underscore prefix (forbidden by `rules/rust.md`).
- `processing` returns `Option<NotificationHandle>` instead of `Result<(), ()>` because the desktop channel's two messages collapse into ONE popup that updates in place ("Processing..." → "Saved: Title"). The spawned task captures the handle from `processing` and threads it through to `result`. If `processing` failed (returned `None`), `result` creates a fresh popup - graceful degradation.
- `result` returns `()` not `Result<(), ()>` because the popup outcome is fire-and-forget; the daemon does not branch on whether libnotify delivered. Internal errors are logged at `warn`.

### Implementation Plan

Each phase ships back-to-back; no soak time, no evidence gate between them.

#### Phase 1: Add desktop sink and config plumbing
**Model:** sonnet
**As shipped:** structs are `Telegram` / `Desktop` / `DesktopConfig`; YAML key is `desktop:`. The pre-existing `Notifier` was renamed to `Telegram` in Phase 2 to make both sinks symmetric (single-word channel-named) per the rust.md/general.md naming rules.

- **Verify before coding**: confirm `notify-rust = "4"` (currently in `borg/Cargo.toml` with no features specified) resolves `show_async` and `NotificationHandle::id`/`Notification::id` on Linux default features for crate version 4.12. If either is gated, switch to `notify-rust = { version = "4", features = ["zbus", "async"] }`. Test with `cargo check` against a minimal example before proceeding. **(Done: 4.12.0 resolves both APIs with default features.)**
- Add `DesktopConfig` to `borg/src/config.rs` (struct, `Default` impl, and a `pub desktop: Option<DesktopConfig>` field on `Config`).
- Add `Desktop` struct to `borg/src/notify.rs` next to the existing Telegram sink. Top-of-file docstring documents that both structs are notification sinks with parallel semantics.
- Implement `Desktop::new`, `processing`, `result`:
  - `processing` calls `notify_rust::Notification::new().appname(&self.appname).summary("obsidian-borg").body(&format!("[{trace_id}] {description}")).timeout(self.timeout).show_async().await`, wrapped in `tokio::time::timeout(Duration::from_millis(500), ...)`. On timeout or D-Bus error, log at `warn` and return `None`. On success return `Some(handle)`.
  - `result` builds the body via `crate::router::format_reply` (with optional obsidian_url appended as plain text). Construct a fresh `Notification` either way; if `prior: Some(handle)`, set `.id(handle.id())` on the new `Notification` before calling `.show_async()`. The notification daemon (dunst/mako/gnome-shell) honors the id and replaces the prior popup in place. **Do NOT use `handle.update()`** - it is synchronous in notify-rust v4, which would defeat the 500 ms `tokio::time::timeout` wrapper (a sync function inside `timeout(async { ... })` has no await points and blocks the worker thread until D-Bus replies). The `.id() + show_async()` path is the only way to preserve the timeout invariant while replacing in place.
- Unit tests in `borg/src/notify/tests.rs` (the inline `mod tests { ... }` block was extracted into this submodule file at the same time, per rust.md):
  - `Desktop::new` returns `None` when `enabled: false`, `Some` when `enabled: true`.
  - `format_desktop_body` (free helper returning the body string given an `IngestResult`) byte-equals `format_reply` output for `Completed`, `Failed`, `Duplicate`, `Queued` variants - guards against rendering drift between channels.
  - One additional test pins the structural divergence: when `obsidian_url` is set, `format_desktop_body` appends it as plain text on a new line, NOT HTML-escaped, while `format_telegram_reply` would escape it.

#### Phase 2: Wire into `AppState` and every intake path
**Model:** sonnet
**As shipped:** rename pass folded into this phase. Existing `Notifier` -> `Telegram`; AppState fields `notifier`/`desktop_notifier` -> `telegram`/`desktop`; ServerStartup field `telegram_notifier` -> `telegram` and new `desktop`. `telegram_bot` stays as the distinct inbound polling subsystem.

- Add `telegram: Option<Telegram>` (renamed) and `desktop: Option<Desktop>` (new) to `AppState`; add `telegram: SubsystemStatus` (renamed) and `desktop: SubsystemStatus` (new) to `ServerStartup`.
- In `borg/src/lib.rs::serve_init`, construct the desktop sink next to the Telegram block. Host-gate via `config::is_local_host(&cfg.host)`. Status taxonomy mirrors the others: `Active`, `Disabled`, `SkippedHostMismatch`.
- In `borg/src/routes.rs` (`ingest`, `note`, `ingest_multipart`): clone `state.telegram` + `state.desktop` into the spawned-task closure. **Per the Design Invariant above, also move the existing `n.processing(...).await` call INSIDE the spawn block** (the pre-existing Telegram processing call was on the HTTP hot path). The new pattern at every site:

  ```rust
  // Inside tokio::spawn(async move { ... }):
  let prior = if let Some(d) = &desktop {
      d.processing(&trace_id, "Processing...").await
  } else { None };
  if let Some(t) = &telegram {
      let _ = t.processing(&trace_id, "Processing...", None).await;
  }
  let result = pipeline::process_content(...).await;
  if let Some(t) = telegram {
      t.result(&result, &display_source, None).await;
  }
  if let Some(d) = desktop {
      d.result(&result, &display_source, prior).await;
  }
  ```

  The `prior` local carries the processing-popup handle from desktop into the terminal `result` call so the popup updates in place (Option B from review).
- Repeat the same in-spawn relocation + parallel-call addition + prior-handle threading in `borg/src/ntfy.rs` (two URL/text producer pairs). `ntfy::run` gains a `desktop: Option<Desktop>` parameter alongside the renamed `telegram` parameter.
- Repeat in `borg/src/telegram.rs` (five producer-site clusters in the handlers for photo, voice, audio, document, plain text/URL). `telegram::run` gains a `desktop: Option<notify::Desktop>` parameter alongside the renamed `telegram: Option<notify::Telegram>` parameter.
- `borg/src/discord.rs`: Discord uses its own per-channel reply via `msg.channel_id.say(...)` rather than a shared notifier (a deliberate choice - Discord acks back into the Discord channel where the user submitted). Two pairs of producer points: the attachment path and the URL/text path. The concrete plumbing:
  1. Add `desktop: Option<Desktop>` parameter to `discord::run` (the public signature changes from `(token, dc_config, config)` to `(token, dc_config, config, desktop)`).
  2. Add a `desktop: Option<Desktop>` field to the `Handler` struct.
  3. In `serve_init`, clone the constructed `desktop` into the `discord::run` call site.
  4. At each producer site in `discord.rs`, capture the `prior` handle and pass it to `result` using the `if let Some(d) = ...` pattern that the rest of the codebase uses, NOT the `?` operator (which would short-circuit the entire Discord handler when `desktop` is `None`, silently dropping the message and skipping the pipeline):

  ```rust
  // Immediately before pipeline::process_content(...):
  let prior = if let Some(d) = &self.desktop {
      d.processing(&trace_id, "Processing...").await
  } else { None };
  // ... existing Discord channel reply via msg.channel_id.say(...) unchanged ...
  let result = pipeline::process_content(...).await;
  // ... existing format_discord_reply(...) call unchanged ...
  if let Some(d) = &self.desktop {
      d.result(&result, &display_source, prior).await;
  }
  ```

  The Discord channel reply via `msg.channel_id.say(...)` is unchanged; the desktop popup appears in addition and updates in place. When `desktop` is `None`, the Discord handler runs identically to before. Note: Discord's call sites are already "inside the spawn" structurally (they're inside the message handler future), so Design Invariant 1 doesn't apply - the Discord handler doesn't have an HTTP response to return.
- Update `sb/src/cli/borg.rs::print_server_banner` to read `s.telegram` (renamed) and `s.desktop` (new) and emit a "desktop notifier active" / "desktop notifier skipped (host mismatch)" line in the startup banner.

#### Phase 3: Clean up Firefox extension and CLI helper
**Model:** sonnet

- `borg/clients/extension/background.js`:
  - Delete the `if (result.title) { ... }` branch (dead since fire-and-forget).
  - Delete the `else if (result.status && result.status.Failed) { ... }` branch (dead since fire-and-forget).
  - Keep the `catch (err) { ... }` branch that fires `Error: ${err.message}` on transport failure; this is the one notification the daemon by definition cannot deliver (it isn't running).
  - Keep the "No active tab URL" guard at the top.
  - Drop the corresponding badge-text updates from the deleted branches; keep them for the `catch` branch.
- `borg/src/lib.rs::ingest` (CLI helper at lines 697-762):
  - **Untangle a latent signature bug** discovered by the Architect review: the caller at `sb/src/cli/borg.rs:351` is `borg::ingest(config, resolved_url, tags, force, clipboard, method)`, while the callee's signature is `ingest(config, url, tags, force, notify: bool, method)`. The fifth positional argument is `clipboard` on the caller side and `notify` on the callee side - they're being shoved into each other today. Practical consequence: `sb borg ingest --clipboard <url>` fires CLI notifications; `sb borg ingest <url>` (no clipboard) does not. Neither is what the original author intended; the parameters were silently misaligned. The fix:
    - Remove the `notify: bool` parameter from `borg::ingest`'s signature entirely (the daemon owns notifications now; the CLI never needs to opt in or out).
    - Inspect whether `clipboard: bool` is used inside `borg::ingest` itself. Reading lines 697-762, it is not - the caller already resolves the URL via `borg::resolve_ingest_url(url, clipboard)` before this call. So `clipboard` should not be a parameter either; drop it.
    - The final signature: `pub async fn ingest(config: Config, url: String, tags: Option<Vec<String>>, force: bool, method: types::IngestMethod) -> Result<IngestOutcome>`.
    - Update the caller at `sb/src/cli/borg.rs:351` to match: `borg::ingest(config, resolved_url, tags, force, method).await?`.
  - Remove the five **happy-path** `send_notification(...)` calls (the `Ingesting...`, `Saved`, `Duplicate`, `Failed`, and `Queued` toasts at lines 710, 740, 746, 752, 758). The daemon now produces these; firing them again locally would double-fire on the same machine.
  - **Keep** the `send_notification("Error", &msg)` call at line 727-729 in the `client.post(...).send(...).map_err(...)` arm. With `notify: bool` removed, the call becomes **unconditional** (drop the `if notify { ... }` guard around it). This fires only when the HTTP POST itself fails (`is_connect()` → `cannot reach obsidian-borg at http://... - is the daemon running?`) - exactly the case the daemon by definition cannot deliver. Mirrors the symmetric decision to keep the Firefox extension's `catch (err)` branch. Particularly load-bearing because `sb borg ingest` may be wired to a desktop hotkey where stderr is not visible.
  - Keep the private `fn send_notification` helper since the Error path still uses it.
- Bump `borg/clients/extension/manifest.json` version (the static one checked in is a development artifact; the daemon's `generate_manifest` at `borg/src/lib.rs:774-...` produces the runtime manifest from `CARGO_PKG_VERSION`, so a fresh package bump catches both).

#### Phase 4: REQUIRED TOOLS + bootstrap template + CLAUDE.md
**Model:** sonnet

- `sb/src/cli/borg.rs::get_tool_validation_help` (the `REQUIRED TOOLS:` builder around line 1126): add a `notify-send` row using the existing `check_tool_version("notify-send", "--version", "")` machinery. A comment documents that `notify-send` is a *runtime-dependency proxy* - borg doesn't shell out to it (the in-process `notify-rust` crate handles delivery), but its presence indicates a working libnotify stack and gives the operator a one-liner (`notify-send foo`) for debugging missing toasts.
- `config/templates/borg.yml.example` (this is the actual filename in the repo; the design originally said `borg.yml`): add a `desktop:` block with `enabled: true`, `host: desk`, `timeout-ms: 5000`, `appname: borg`, plus an explanatory comment. The YAML key matches the in-code Config field. (`sb bootstrap` drops this template into `~/.config/sb/borg.yml` on a fresh machine.)
- CLAUDE.md: add one sentence under the **Key Conventions** section: "Borg ships two notification sinks (`notify::Telegram` and `notify::Desktop`) wired in parallel from the daemon - the desktop sink replaces the dead path the Firefox extension used to render. Both fire from the same producer points inside every spawned ingest task; future channels go side-by-side, not behind a trait."
- Update this doc's Status to "Implemented" on landing, AND fold a post-implementation amendment into the doc capturing the rename + cleanup that happened during Phase 2 / Phase 3 (the rename was not in the original draft).

## Alternatives Considered

### Alternative 1: Extension polls a new `GET /trace/:trace_id` endpoint
- **Description:** Borg exposes a read-only endpoint backed by the receipts SQLite. Extension polls every second for up to N minutes after submitting and renders the toast from the polled response.
- **Pros:** Channel-agnostic - works on any future client that can poll (mobile, web UI, CLI).
- **Cons:** Re-introduces the dead-data-dependency that caused this bug (client and server must agree on response shape forever). Polling timeouts have to bound the slowest pipeline (15+ min for YouTube transcription). MV3 service worker recycling will kill the polling loop the same way it killed the original sync request, requiring durable client-side state in `chrome.storage.local` plus a wake-up scheduler. Substantial net new code (client and server) to restore identical UX to what a `notify-rust::Notification::show()` call delivers in one line.
- **Why not chosen:** The desktop already has the daemon running on it. Going out-of-process and back to render a toast is theater.

### Alternative 2: SSE stream on `GET /trace/:trace_id/events`
- **Description:** Borg holds an SSE connection per trace; the spawned pipeline task publishes terminal events to an in-memory broadcast channel keyed by trace_id; extension subscribes immediately after submitting.
- **Pros:** Real-time push, no polling, cleaner than #1.
- **Cons:** Service-worker recycling still kills the SSE connection. Axum SSE wiring + per-trace broadcast fan-out for an audience of one (the local browser). All the cross-process dead-data-dependency pathology of #1 with more moving parts.
- **Why not chosen:** Same as #1.

### Alternative 3: Stopgap "Submitted" toast only
- **Description:** One-line extension change to fire `chrome.notifications.create({ message: "Submitted: <url>" })` on the Queued response. Rely on Telegram for the terminal outcome.
- **Pros:** Ships today. Zero daemon changes.
- **Cons:** Telegram and desktop diverge - desktop shows only "Submitted", Telegram shows "Submitted" + "Saved/Failed/Duplicate". The user explicitly asked for parity. Also keeps the cross-process notification coupling, so the next refactor of the request/response contract can re-break it.
- **Why not chosen:** Does not meet the stated parity goal.

### Alternative 4: Sink trait taking `Vec<Box<dyn NotificationSink>>`
- **Description:** Define `trait NotificationSink { async fn processing(...); async fn result(...); }`. Hold `Vec<Box<dyn NotificationSink>>` in `AppState`. Iterate at every call site.
- **Pros:** O(1) change to call sites when adding a third sink.
- **Cons:** Two sinks does not justify a trait, and the Rust convention here is generics over `dyn Trait`. Premature abstraction.
- **Why not chosen:** YAGNI. Revisit if/when a third channel appears.

## Technical Considerations

### Dependencies

- `notify-rust = "4"` (resolved to 4.12.0 in `Cargo.lock`): already in `borg/Cargo.toml`; already used by the existing `send_notification` helper at `borg/src/lib.rs:765-772`. No new crate. notify-rust 4.5+ exposes `Notification::show_async()` (truly async, returns a future) which we use from day one - this avoids the blocking-on-tokio-worker pathology that `Notification::show()` would have under concurrent bulk-ingest load.
- Runtime: a user-session D-Bus must be reachable from the systemd `--user` unit. systemd `--user` units inherit `DBUS_SESSION_BUS_ADDRESS` from the user manager by default. Empirically confirmed: the existing `send_notification` helper at `lib.rs:765-772` has been working from the same unit context on desk.lan since the CLI helper landed.
- `notify-send` binary (`libnotify-bin` package on Debian/Ubuntu): NOT a code-level dependency. Listed in `REQUIRED TOOLS:` only as a runtime-dependency proxy and diagnostic anchor (see Phase 4).

### Performance

- One D-Bus call per `processing` and per `result` (the result call is an `.update()` on an existing handle in the common path, not a fresh notification). Submilliseconds on a warm session bus, single-digit milliseconds cold. Negligible on the 15-minute pipeline timeline.
- We use `notify_rust::Notification::show_async()` (added in 4.5; we're on 4.12.0). It's truly async and returns a future, so the call composes naturally inside the tokio spawned task without blocking a worker thread.
- Every call is wrapped in `tokio::time::timeout(Duration::from_millis(500), ...)` per the Design Invariant. A wedged notification daemon does not delay the pipeline beyond 500 ms; on timeout the call logs `warn` and returns the "no popup rendered" outcome (None for processing; nothing for result).
- The blocking `show()` path used by the legacy CLI helper at `borg/src/lib.rs:765-772` is single-call-per-CLI-invocation in the "borg unreachable" error arm (the kept-by-design CLI Error toast). It does not run in async context after Phase 3, so it stays as-is.

### Security

- Toasts render URLs, titles, and `Failed` reason strings to the user's notification daemon. Same trust surface as the existing Telegram messages, just to a different (more local) consumer.
- `notify_rust::Notification::body` takes a plain `&str`; no markup interpretation, no HTML, no shell. No injection surface.

### Testing Strategy

- **Unit tests** (`borg/src/notify/tests.rs`):
  - `DesktopNotifier::new` returns `Some` / `None` correctly across enabled/disabled.
  - Internal `format_desktop_body` is byte-equivalent to `format_reply` for each `IngestStatus` variant (guards against silent rendering drift between channels).
- **Empirical verification on desk.lan** (the only host that meets the runtime D-Bus prerequisite):
  - POST `/ingest` with a known URL; confirm desktop toast `[trace_id] Processing...` appears within 1 second.
  - Wait for pipeline completion; confirm desktop toast `Saved: <title>` appears.
  - Force a `Duplicate` (reingest a known URL); confirm desktop toast `Duplicate: ...`.
  - Force a `Failed` (ingest a URL behind an auth wall); confirm desktop toast `Failed: ...`.
  - Confirm Telegram delivers the same four messages with byte-equivalent bodies (modulo HTML escape).
- **CI**: no D-Bus available; unit tests cover the logic, integration is empirical. Documented as such.

### Rollout Plan

1. Phases 1-4 land back-to-back in a single PR or sequential commits on `main`. No feature flag, no soak time between phases.
2. The `DesktopConfig::default()` is `enabled: false` so existing installs that don't have the new `desktop:` block boot silent (no behavior change for anyone who hasn't bootstrapped).
3. The `config/templates/borg.yml.example` template ships with the block populated as `enabled: true, host: desk, timeout-ms: 5000, appname: borg`. New machines that run `sb bootstrap` after this lands pick this up by default.
4. **Existing-install activation (desk):** edit the live `~/.config/sb/borg.yml` to append the `desktop:` block. Use the new entry in `config/templates/borg.yml.example` as the source. Without this step the daemon will boot with `desktop: Disabled` and the verification step below silently fails. (Do not re-run `sb bootstrap` to avoid touching unrelated fields; a surgical edit to the existing live config is the safe path.)
5. `bump` to v0.8.11 (patch), `otto deploy` (rebuilds and restarts `borg.service`).
6. Verify by hitting `/ingest` from Firefox on desk; expect two desktop toasts (`[trace_id] Processing...` then terminal) AND two matching Telegram messages.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| D-Bus session not reachable from systemd `--user` unit | Low | Medium | The existing `send_notification` helper has been working from this unit on desk.lan, confirming the bus is reachable in this context. Failure path is `log::warn` and continue; the daemon does not abort. |
| Bulk ingest spam (200 toasts for a replay) | Medium | Low | Telegram already has this property; the user has an existing mental model for muting during bulk replays. Documented as known. Follow-up: `desktop-notifier.batch-mode` flag analogous to whatever Telegram exposes. |
| `notify_rust::Notification::show()` blocks the tokio runtime under load | Eliminated | - | Day-one use of `show_async()` (notify-rust 4.5+; we're on 4.12.0). Sync `show()` only used by the legacy CLI helper. |
| D-Bus call hangs (wedged notification daemon, dbus broker issue) | Low | Medium | Every notification call wrapped in `tokio::time::timeout(500ms, ...)` per the Design Invariant. Hang turns into a logged warning at the 500 ms mark; pipeline continues. |
| Telegram or D-Bus latency couples to HTTP `/ingest` response time | High before this design, Eliminated after | Medium | Every `processing(...)` call moves INSIDE the `tokio::spawn` block per the Design Invariant. HTTP handler returns sub-millisecond regardless of notification-channel health. Latent defect in today's code that this design fixes as a side effect. |
| `notify-rust = "4"` default features do not include `show_async` / `NotificationHandle::update` | Low | Low | Phase 1 explicitly verifies before coding. If gated, switch to `features = ["zbus", "async"]` (one Cargo.toml line). |
| CLI signature misalignment (`clipboard` shoved into `notify` slot) silently changes behavior post-refactor | High | Low | Phase 3 explicitly untangles both parameters from `borg::ingest`'s signature with rationale documented. The original misalignment was a latent bug; cleanup removes the broken plumbing entirely rather than preserving it. |
| `tokio::time::timeout` silently bypassed by a synchronous `notify-rust` call | Medium pre-design, Eliminated | Medium | Architect round 2 found that `NotificationHandle::update()` is synchronous in notify-rust v4; wrapping it in `timeout(async { ... })` has no await points and blocks the worker until D-Bus replies. Design uses `.id(handle.id())` + `show_async()` (async, returns a Future) for the replace-in-place path instead, preserving the timeout invariant. |
| `?` on `Option` in Discord handler short-circuits and drops the pipeline | Medium pre-design, Eliminated | High | Architect round 2 caught a shorthand `self.desktop_notifier.as_ref()?.processing(...)` pattern that would skip the pipeline whenever the desktop notifier is `None`. Design uses the `if let Some(d) = &self.desktop_notifier { ... }` Option-handling pattern that the rest of the codebase uses; when the notifier is `None`, the handler runs identically to today. |
| Long titles or non-ASCII content mis-render in the notification daemon | Low | Low | `notify-rust` passes raw bytes; the daemon (dunst/mako/gnome-shell) handles truncation and Unicode. Same risk Telegram has; user has not reported issues. |
| User moves to a different desk machine, `host: desk.lan` becomes wrong | Low | Low | Same host-gating model as telegram/discord/ntfy; user updates the config or `bootstrap` re-derives. Documented in the template. |
| Future request/response refactor silently re-breaks a channel | Low | Medium | The pattern of "all sinks fire from the same producer points in the daemon" is now the convention. Any future refactor that touches the spawned task naturally touches both calls together. This is structural defense, not contractual. |
| Adding `notify-send` to `REQUIRED TOOLS:` confuses users into thinking we shell out to it | Low | Low | Inline comment in `HELP_TEXT` builder explaining the proxy role. If the icon lookup wants a stronger signal, suffix the row with a hint (e.g. `notify-send  0.8.8  (libnotify diagnostic)`). |

## Open Questions

- [ ] Full external-tool catalog in `REQUIRED TOOLS:` (fabric, yt-dlp, ffmpeg, markitdown, whisper, pandoc, etc.). Bigger than this design; tracked as a separate follow-up. This design adds `notify-send` only because it's the proxy for the runtime dependency this design introduces.
- [ ] Should `sb borg --help`'s `REQUIRED TOOLS:` distinguish "hard dependency" (we shell out) from "runtime proxy" (presence signals a working stack)? `notify-send` would be the first proxy entry. Out of scope for this design; default for now is a code comment, no UI distinction. Worth revisiting when the full external-tool catalog lands.

## References

- Commit `fa79724de20c602ffad9f4688bc1dc7ca1b94969` - the fire-and-forget refactor that broke the extension's notification path.
- `borg/src/routes.rs:43-107` - current `/ingest` handler returning `Queued`.
- `borg/clients/extension/background.js:24-32` - the two dead happy-path branches.
- `borg/src/notify.rs` - existing Telegram `Notifier`, target for the new `DesktopNotifier` sibling.
- `borg/src/lib.rs:179-200` (`serve_init` Telegram block) - construction pattern to mirror.
- `borg/src/lib.rs:765-772` - existing `notify_rust` call site confirming D-Bus is reachable from this unit.
- `sb/src/cli/borg.rs:1145-1178` - existing `REQUIRED TOOLS:` builder to extend.
- Memories: `feedback-design-doc-first`, `feedback-no-phase-gating`, `feedback-no-deferments`, `feedback-no-known-leaks-on-main`.
