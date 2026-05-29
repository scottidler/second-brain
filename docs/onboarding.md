# Onboarding: standing up second-brain from scratch

This is the end-to-end, gotcha-aware walkthrough for a new person bringing
the whole system up on their own machine and accounts. The
[README](../README.md) has the canonical install commands; this guide is the
linear path with the traps called out, plus the runtime dependencies the
README does not list.

## What you are signing up for

second-brain is a *personal* ingestion + governance daemon. It is wired to
**your** messaging accounts (Telegram/Signal), **your** LLM key (via Fabric),
a fistful of **external CLIs**, and **your** Obsidian vault. The Rust code is
portable and `sb bootstrap` does the config legwork, but standing the whole
thing up is an afternoon of installing dependencies and linking accounts, not
a one-line `cargo install`. The good news: the friction is enumerable, and
`sb doctor` is your checklist the entire way.

You do **not** need all of it. The minimum useful loop is: `sb` + `fabric` +
an LLM key + one transport (Telegram is easiest) + a vault. Video, voice,
image-OCR, Signal, and the Firefox extension are each opt-in and add their own
dependencies.

## Dependency checklist

Install these before configuring anything. Grouped by what they unlock.

**Core (required for any ingest):**
- **Rust toolchain** (`rustup`) - to build `sb`.
- **Go toolchain** - to install Fabric (`go install ...`), or grab a
  pre-built Fabric binary from its releases page.
- **Fabric** (Daniel Miessler's) - the distill engine. `sb doctor`
  hard-errors if it is missing. Needs its own config and an **LLM API key**
  (Anthropic/OpenAI) - this is the single biggest "it does not just work"
  dependency. After install: `fabric -y --update-patterns`.
- **An Obsidian vault** - a directory containing a `.obsidian/` folder. This
  is where notes land. It can be empty to start.

**Preferred but optional:**
- **`rkvr`** (github.com/scottidler/rkvr) - borg prefers it for safe,
  *recoverable* deletes (it archives before deleting). It is **not required**:
  if `rkvr` is not on PATH, borg falls back to a normal non-recoverable
  delete (`rm -rf` semantics) and logs a WARN. Install it only if you want
  deleted notes/artifacts to be recoverable.

**For the full content pipeline (each fails *silently at pipeline time* if
missing - they are NOT checked by `sb doctor`):**
- **`markitdown`** (`pipx install markitdown`) - article/document text
  extraction.
- **`yt-dlp`** (`pipx install yt-dlp`) - YouTube metadata, subtitles, frames.
- **`ffmpeg`** (distro package) - video/audio processing, keyframe extraction.
- **`tesseract`** (distro `tesseract-ocr`) - OCR for image ingest.

**Optional add-ons:**
- **`signal-rs`** CLI - only if you want the Signal transport (see below).
- **Firefox** + **`web-ext`** - only for the capture extension
  (`sb bootstrap --extension`).

> Use `pipx`, never `pip`, for the Python tools.

## Step-by-step

### 1. Install `sb` and Fabric

Follow the README's [Install](../README.md#install) section:

```bash
cargo install --git https://github.com/scottidler/second-brain --bin sb
go install github.com/danielmiessler/fabric/cmd/fabric@latest
fabric -y --update-patterns       # then configure fabric with your LLM key
```

### 2. Install the silent runtime dependencies

These are the ones the README omits. Without them, ingest of the
corresponding content type fails mid-pipeline with no up-front warning:

```bash
pipx install markitdown            # articles / documents
pipx install yt-dlp                # youtube
sudo apt install ffmpeg tesseract-ocr   # (or brew) - video/audio, image OCR
# optional: rkvr (github.com/scottidler/rkvr) for recoverable deletes;
#           borg falls back to plain rm -rf + a WARN without it
```

### 3. Provision config and assets

```bash
sb bootstrap
```

This drops config templates, the shared vocabulary
(`canonical-tags.yml`, `tag-mapping.yml`, `tag-proposals.yml`), and the
Fabric distill patterns into `~/.config/sb/`, installs the borg and cortex
systemd user units, and prefetches the embedding model (~100 MB first run).
Idempotent; `--force` refreshes vocabulary/patterns from the binary.

### 4. Point it at your vault

`sb` will not guess a vault. Set the root in each config (or pass `--vault`,
or run from inside the vault directory):

```bash
$EDITOR ~/.config/sb/borg.yml      # set vault.root-path: ~/path/to/your/vault
# repeat for cortex.yml and oracle.yml, or keep them consistent
```

If unset, commands error with: "vault root not set: pass --vault, set
`vault.root-path`, or run from a directory that contains a `.obsidian/`".

> **You inherit the schema.** Domains, note types, and the 110 canonical tags
> live in `vault::schema`. Out of the box your notes use *that* taxonomy; fork
> it if you want your own.

### 5. Wire one transport (start with Telegram)

Telegram is the lowest-friction first transport: create a bot via
`@BotFather`, get the token and your chat id, and fill in the `telegram:`
block in `borg.yml`. Set `telegram.host` to this machine's hostname if you
run daemons on more than one box.

### 6. Enable lingering and start the daemons

So the user daemons survive logout:

```bash
loginctl enable-linger "$USER"
systemctl --user start borg cortex
```

### 7. Verify

```bash
sb doctor      # work top to bottom until every section is green
sb status      # live state
```

Then the real test: send a URL to your Telegram bot and confirm a note lands
in your vault. `sb borg log` shows the receipt trail (received -> succeeded).

## Optional: Signal transport

Signal is the spiciest piece. It runs through `signal-rs` - a from-scratch
Rust implementation of the Signal protocol (not a wrapper around
`signal-cli`), pulled as a dependency. Follow the README's
[Signal section](../README.md#optional-signal-transport): install the
`signal-rs` CLI, stop borg, `signal-rs link` and scan the QR with your phone,
set `signal.host` to this machine, restart.

Two things specific to Signal:
- **It is single-machine.** Signal-Server fans Note-to-Self to every linked
  device, so ingest is pinned to one host via `signal.host`. Do not link it
  on multiple daemon machines.
- **Cold-start is auto-handled.** A freshly-linked device receives nothing
  until it has *sent* once (the phone builds its sync session lazily). borg
  now self-pings once at first start to establish that session - you will see
  one "borg: establishing Signal sync session" message in Note-to-Self, then
  it never repeats. If `sb doctor` ever warns "linked but ... not yet
  established," just restart borg. See
  `docs/design/2026-05-28-signal-cold-start-bootstrap.md`.

## Known traps (lived experience)

- **Fabric under systemd needs `go/bin` on PATH.** Fabric works in your
  interactive shell but the daemon's `PATH` may omit `~/go/bin`, silently
  breaking every distill. If ingest "succeeds" but produces empty/garbage
  notes, check the daemon's environment.
- **`sb doctor` only checks Fabric, not the other binaries.** A green doctor
  does not prove `yt-dlp`/`ffmpeg`/`markitdown`/`tesseract` are installed -
  those only surface when you ingest that content type. Install them up front.
- **Multi-machine = per-host install.** `otto install` / `sb bootstrap` act on
  the machine you run them on; there is no fleet push. Each daemon host
  installs and configures independently. Host pins (`*.host`) keep
  single-machine transports from double-running.
- **Lingering is easy to forget** - without `enable-linger`, the user daemons
  stop when you log out.

## Where to go next

- [`CLAUDE.md`](../CLAUDE.md) - architecture overview and per-subsystem
  invariants (the authoritative map).
- [`docs/design/`](design/) - design memos for each subsystem and decision.
- `sb --help`, `sb doctor`, `sb status` - the system documents itself.
