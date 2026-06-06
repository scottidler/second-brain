# borg clients — Browser & Hotkey Capture

> Local node. Parent crate: `../AGENTS.md`. These clients are NOT Rust — they only speak the borg HTTP ingest envelope.

## Purpose

Three lightweight, independent clients send capture requests to the borg HTTP server (`POST /ingest` with an `IngestRequest` JSON body). Different languages/runtimes/deployment models; the only shared contract is the HTTP body schema. Clients never touch the `vault` schema or frontmatter — tag classification and vault structure are entirely server-side.

## Clients

- **`extension/`** — WebExtension (Firefox/MV3). Popup (`popup.html`/`popup.js`) reads the active-tab URL and POSTs to `/ingest` with `keepalive: true` so the request survives popup close on focus loss. Checks `res.ok` (fetch doesn't reject on HTTP errors). `options.html`/`options.js` configure the daemon endpoint (default `localhost:8181`). `ingest-schema.json` (generated from `IngestRequest` via schemars at build) is a web-accessible resource for validation.
- **`hotkey/`** — POSIX shell (`obsidian-borg-capture.sh`). Bound system-wide (GNOME/KDE/i3/sway/macOS). Reads clipboard (`pbpaste`/`xclip`/`wl-paste`), validates `^https?://`, shells out to `sb borg ingest "$URL"` (the CLI POSTs to the daemon). Desktop notification via `osascript`/`notify-send`.
- **`bookmarklet/`** — one-line JavaScript bookmark. POSTs `location.href` to `/ingest`; relies on the daemon's CORS layer. Subject to mixed-content blocks (HTTPS page → HTTP localhost).

## The Wire Contract

`POST http://{host}:{port}/ingest`, JSON body:

```json
{ "url": "https://…", "tags": ["optional"], "priority": "Normal|High", "force": false, "method": "http" }
```

Only `url` is required; the rest use serde defaults. **Additive-only** (mirrors `borg/src/types.rs::IngestRequest`): new fields may be added, but existing names/types are never changed or removed. A required-field addition requires re-signing the extension in the same PR.

## Patterns

- **Add a client:** extract/prompt a URL → build the JSON body (at least `url`) → POST with `Content-Type: application/json` → use the response only to detect network/daemon errors (ingest is async; `Queued` ≠ done).

## Anti-patterns

- Don't filter by URL scheme at the client — forward any non-empty URL; the daemon's blocklist and gates classify it.
- Don't treat the HTTP response as proof of successful ingest — the pipeline completes asynchronously.
- Don't hardcode the daemon endpoint in distributed builds — use config/prompts.
- Don't put auth tokens in the body — the daemon assumes a trusted local network; gate remote access at a reverse proxy.
