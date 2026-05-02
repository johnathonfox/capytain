# QSL

> A local-first, privacy-respecting desktop email client written
> end-to-end in Rust.

**Status:** v0.1 ready, not yet tagged. Linux is the primary
platform; macOS and Windows compile but are unverified at runtime.
Three release blockers remain — see
[`docs/releases/v0.1.0.md`](./docs/releases/v0.1.0.md). This is a
personal project published in the open; **there is no maintainer
committed to support, response, or review at this time.**

---

## What this is

QSL is a Tauri 2 + Dioxus 0.7 desktop app over a Rust core. It talks
directly to your mail provider (Gmail via IMAP+SMTP+OAuth2, Fastmail
via JMAP+OAuth2) — no intermediary servers, no telemetry, no ad
networks. Local state lives in Turso (pure-Rust SQLite-compatible).
The reader sanitizes incoming HTML through `ammonia` and renders it
in a sandboxed `<iframe sandbox="allow-scripts" srcdoc>` inside
webkit2gtk; remote content is blocked until you trust the sender.
The chrome is a warm-dark monospace UI in the aerc/mutt density
tradition rather than a webmail clone.

### Wait, didn't this use Servo?

It did, briefly. The embedded Servo renderer was removed on
2026-04-28 in favor of webkit2gtk's iframe — Servo's GL composition
path produced blank reader bodies on hybrid AMD/NVIDIA hardware, and
weeks of yak-shaving (NVIDIA EGL-Wayland explicit-sync, GTK 3
child-subsurface gaps, surfman/llvmpipe interactions) couldn't get
the multi-process embedder past "works on some hardware some of the
time." The full tombstone — what shipped, why we paused, and what
would have to happen to bring it back — is in
[`docs/servo-tombstone.md`](./docs/servo-tombstone.md). The
sandboxed iframe is GPU-agnostic, well-trodden, and gives up process
isolation while keeping every other architectural win.

## Features

See [`docs/releases/v0.1.0.md`](./docs/releases/v0.1.0.md) for the
full v0.1 surface. The shape:

- **Accounts & sync.** Gmail OAuth2+PKCE end-to-end; Fastmail JMAP
  wired (live-validation pending). IMAP IDLE + JMAP EventSource live
  push. History sync with chunked FETCH, UID-gap skipping, instant
  cancel. Multi-account from day one.
- **Reading.** Sanitized HTML in a sandboxed iframe with a
  CSP-locked egress fence. Per-sender remote-content opt-in.
  Stacked thread reader. Popup reader windows.
- **Writing.** Compose with reply / reply-all / forward,
  per-identity signatures, attachments, undo-send, `mailto:` deep
  links, Cc/Bcc reveal, address autocomplete from a write-only
  contact store, hunspell spell-check.
- **Lists & search.** Unified inbox, Gmail-style operators
  (`from:` / `subject:` / `has:attachment` / …), `⌘K` command
  palette, drag-and-drop into folders, multi-select with bulk apply,
  Gmail-family keyboard shortcuts.
- **Desktop.** System tray with unread tooltip, launch on login,
  window state restore, single-instance enforcement, default-mailto
  toggle, Settings window with live theme + density + notification
  controls, in-app first-run OAuth.
- **Privacy & security.** Tracker URL filter (`adblock-rust`),
  redirect-service unwrap on outbound clicks, OS-keychain refresh
  tokens with zeroize-on-drop and revoke-on-remove, OAuth2 + PKCE
  with explicit state validation.

## Build

Workspace has two user-facing binaries: `qsl-desktop` (the Tauri app)
and `mailcli` (the headless protocol CLI used during Phase 0 as a
forcing function and now used for maintenance: `mailcli reset`,
`mailcli doctor`).

### Prerequisites

- Rust toolchain (pinned in `rust-toolchain.toml`; install via
  [rustup](https://rustup.rs/))
- `dioxus-cli` for the UI build: `cargo install dioxus-cli --locked`.
  `apps/desktop/src-tauri/build.rs` invokes `dx build --platform web`
  so `cargo run -p qsl-desktop` produces a working UI bundle. Set
  `QSL_SKIP_UI_BUILD=1` to skip the Dioxus build for fast Rust-only
  iteration (CI already does).
- Platform deps:
  - **Linux:** `build-essential`, `libwebkit2gtk-4.1-dev`,
    `libssl-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev`,
    `hunspell` + dictionaries (e.g. `hunspell-en_us`)
  - **macOS:** Xcode command-line tools
  - **Windows:** Visual Studio 2022 with "Desktop development
    with C++"

### Common commands

```sh
git clone https://github.com/johnathonfox/qsl.git
cd qsl

cargo check --workspace          # verifies the whole workspace compiles
cargo test --workspace           # runs all tests
cargo run -p qsl-desktop         # the desktop app (Tauri + Dioxus)
cargo run -p mailcli -- --help   # the headless protocol CLI
```

Fast iteration when you're only changing Rust:

```sh
QSL_SKIP_UI_BUILD=1 cargo run -p qsl-desktop
```

The Dioxus UI is built standalone with:

```sh
dx build --platform web --package qsl-ui
```

A full quickstart — including PR-gate local checks — lives in
[`CONTRIBUTING.md`](./CONTRIBUTING.md#development-setup).

### Linux: NVIDIA / hybrid GPU

`qsl-desktop` exports `WEBKIT_DISABLE_DMABUF_RENDERER=1` at startup
on Linux (only if it isn't already set), rolling webkit2gtk back to
its SHM rendering path. Without this, hybrid AMD/NVIDIA boxes paint
nothing — webkit2gtk's DMA-BUF renderer can't allocate GBM buffers
on the proprietary NVIDIA driver. To force the DMA-BUF path back
(e.g. on a pure Mesa box), export `WEBKIT_DISABLE_DMABUF_RENDERER=0`
before launching. macOS and Windows are unaffected.

## Project layout

```
apps/
  desktop/            # qsl-desktop — Tauri shell + Dioxus UI
    src-tauri/        #   Rust host (commands, tray, mailto, sync engine)
    ui/               #   Dioxus 0.7 webview UI
  mailcli/            # mailcli — headless protocol CLI (reset, doctor)
crates/
  core/               # domain types: Folder, Message, MailBackend trait
  storage/            # Turso schema + repos + migrations + outbox
  imap-client/        # async IMAP with CONDSTORE / QRESYNC / IDLE
  smtp-client/        # SMTP submission via lettre + XOAUTH2
  jmap-client/        # JMAP client for Fastmail (auth, sync, push, send)
  mime/               # RFC 5322 assembly + ammonia sanitization wrapper
  search/             # query AST → Turso FTS MATCH
  sync/               # the unified sync engine + outbox replay loop
  auth/               # OAuth2 + PKCE + libsecret token storage
  ipc/                # IPC commands shared across host + UI
  telemetry/          # tracing init + log routing
docs/                 # design, phases, plans, security audit, release notes
```

## Local data

| Platform | Path |
|---|---|
| **Linux** | `~/.local/share/qsl/` |
| **macOS** | `~/Library/Application Support/app.qsl.qsl/` |
| **Windows** | `%APPDATA%\qsl\qsl\data\` |

Refresh tokens live in the OS keychain under service `com.qsl.app`,
separate from the cache.

```sh
# Wipe local state
cargo run -p mailcli -- reset

# Diagnose / repair drift
cargo run -p mailcli -- doctor --fix --rebuild-fts --vacuum --yes

# Clear OAuth tokens (Linux)
secret-tool clear service com.qsl.app
```

## Documentation

- **[`docs/releases/v0.1.0.md`](./docs/releases/v0.1.0.md)** — what
  shipped in v0.1 and what's deferred.
- **[`docs/servo-tombstone.md`](./docs/servo-tombstone.md)** — the
  Servo renderer's life and removal.
- **[`docs/QSL_BACKLOG_FIXES.md`](./docs/QSL_BACKLOG_FIXES.md)** —
  consciously-accepted gaps with path-out criteria.
- **[`docs/security/audit-2026-05-01.md`](./docs/security/audit-2026-05-01.md)** —
  most recent security review.
- **[`DESIGN.md`](./DESIGN.md)** — full design specification:
  protocols, architecture, security, licensing.
- **[`PHASE_0.md`](./PHASE_0.md)**, **[`PHASE_1.md`](./PHASE_1.md)**,
  **[`PHASE_2.md`](./PHASE_2.md)** — execution-plan archive.
- **[`docs/plans/post-phase-2.md`](./docs/plans/post-phase-2.md)** —
  the v0.1 feature plan that this README's "Features" section
  summarizes.
- **[`CONTRIBUTING.md`](./CONTRIBUTING.md)** — DCO, PR gate, local
  checks.

## Contributing

Pull requests are welcome, but please read the status note at the
top of this README first: **there is no maintainer committed to
reviewing or merging contributions at this time.** Fork freely.
[`CONTRIBUTING.md`](./CONTRIBUTING.md) covers the DCO sign-off and
the shape of a good PR if you want to open one anyway.

## License

Apache License 2.0. Every source file carries an SPDX header. See
[`LICENSE`](./LICENSE) for the full text and [`NOTICE`](./NOTICE)
for third-party attributions.

> Fork it, sell it, close it, rebrand it — just keep the license
> text and notices.

## Trademarks

QSL is not a registered trademark and the project asserts no
trademark rights. If you hold an existing trademark and our name
conflicts with yours, open an issue and we'll rename. No letters
or lawyers required.
