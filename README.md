# Capytain

> A modern, Rust-native, privacy-respecting desktop email client.

**Status:** 🚧 Experimental. Do not use for real email. This is a personal experiment published in the open — **there is no maintainer committed to support, response, or review at this time.** Issues and pull requests may or may not receive a reply. See [`PHASE_0.md`](./PHASE_0.md) for the execution plan and [`DESIGN.md`](./DESIGN.md) for the full design.

---

## What this is

Capytain is a cross-platform desktop email client for macOS, Windows, and Linux, written end-to-end in Rust. It connects directly to your mail provider — no intermediary servers, no telemetry, no ad networks — and is built on a deliberately experimental pure-Rust stack: [Tauri](https://tauri.app/) + [Dioxus](https://dioxuslabs.com/) for the app, [Servo](https://servo.org/) for rendering HTML email, [Turso](https://turso.tech/) for storage, [adblock-rust](https://github.com/brave/adblock-rust) for tracker blocking.

The goal is a mail client that respects the user by default and demonstrates that a fully Rust-native desktop stack is viable for a real-world, consumer-facing application.

## Why another email client

Every modern desktop client makes at least one of the following compromises:

- Ships a full Chromium engine (Electron), bloating binary size and memory use.
- Routes mail, images, or links through the vendor's servers (for "features" like link tracking, image proxying, or AI summarization).
- Uses the system webview uniformly, causing emails to render differently across platforms.
- Depends on C/C++ libraries for the most security-sensitive parts of the pipeline (HTML parsing, image decoding).

Capytain makes the opposite bet on each: pure-Rust end to end, no intermediary servers, consistent rendering via Servo, memory-safe by default.

## Design principles

1. **Rust-native throughout.** Prefer pure-Rust crates over C/C++ bindings even when the pure-Rust option is newer. File bugs upstream; carry patches when needed. See [`DESIGN.md` §1](./DESIGN.md#1-overview-and-goals).
2. **Modern protocols only.** OAuth2-only authentication. IMAP with CONDSTORE/QRESYNC/IDLE, JMAP. No POP3, no Exchange/EAS, no password auth.
3. **Private by default.** No telemetry. No third-party servers between you and your mail provider. Tracker and ad-network blocking built in. Link cleaning on click.
4. **Offline-first.** Your mail works without a network. Actions queue and replay when connectivity returns.
5. **Permissively open source.** Apache License 2.0. Fork it, sell it, close it, rebrand it — just keep the license and notices.

## Supported providers

| Provider | Protocol | Target phase |
|---|---|---|
| Gmail / Google Workspace | IMAP + SMTP + OAuth2 | **v1** |
| Fastmail | JMAP + OAuth2 | **v1** |
| Microsoft 365 / Outlook.com | IMAP + SMTP + OAuth2 | v1.x |
| Self-hosted with OAuth2 (Stalwart, Dovecot+OAUTHBEARER) | IMAP / JMAP + OAuth2 | v1.x |
| iCloud, Yahoo | — | ❌ No OAuth2 support from the provider |
| ProtonMail, Tutanota | — | ❌ Proprietary protocols |
| On-prem Exchange | — | ❌ Out of scope |

## System requirements

- **macOS** 12 Monterey or newer (x86_64 or Apple Silicon)
- **Windows** 10 22H2 or newer (x86_64)
- **Linux** with a modern Wayland or X11 session (x86_64); GTK 3.24+ for the webview. On **NVIDIA proprietary driver + Wayland** the reader pane falls back to software rendering by default — see [Linux: NVIDIA + Wayland note](#linux-nvidia--wayland-note) below.

## Getting started (for developers)

> Note: there is nothing to download yet. These instructions are for working on the code.

### Prerequisites

- Rust toolchain (version pinned in `rust-toolchain.toml`; install via [rustup](https://rustup.rs/))
- Node.js 20+ (only for the Tauri CLI tooling)
- `dioxus-cli` for the UI build: `cargo install dioxus-cli --locked`. `apps/desktop/src-tauri/build.rs` invokes `dx build --platform web` so `cargo run -p capytain-desktop` produces a working UI bundle. Set `CAPYTAIN_SKIP_UI_BUILD=1` to skip this step (CI already does).
- Platform build deps:
  - **macOS:** Xcode command-line tools
  - **Windows:** Visual Studio 2022 with "Desktop development with C++"
  - **Linux:** `build-essential`, `libwebkit2gtk-4.1-dev`, `libssl-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev`

### Linux: NVIDIA + Wayland note

On Linux + NVIDIA proprietary driver + a Wayland compositor that advertises the `wp_linux_drm_syncobj_surface_v1` explicit-sync protocol (KWin, and likely others), the first surfman commit tears the Wayland connection with a protocol error — NVIDIA's closed-source EGL-Wayland layer auto-joins the protocol but doesn't supply an acquire timeline point. Tracked upstream as [servo/surfman#354](https://github.com/servo/surfman/issues/354); full investigation in [`docs/upstream/surfman-explicit-sync.md`](./docs/upstream/surfman-explicit-sync.md).

To work around this without waiting on the upstream fix, `capytain-desktop` sets three environment variables at startup (only if they are not already set) that force Mesa's llvmpipe software EGL and bypass the NVIDIA EGL-Wayland path entirely:

```
MESA_LOADER_DRIVER_OVERRIDE=llvmpipe
LIBGL_ALWAYS_SOFTWARE=1
__EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/50_mesa.json
```

The reader pane then renders on CPU rather than GPU. For 720×560 email HTML this is fine; the reader is not a GPU-bound workload.

If you want to **reproduce the native-NVIDIA bug** (e.g. to test a driver fix or an upstream surfman patch), export any one of those variables to a different value — e.g. `LIBGL_ALWAYS_SOFTWARE=0` — before launching. The code only overrides unset variables, so your export wins.

Non-Linux platforms are unaffected; the workaround is a no-op on macOS and Windows.

### Building from source

```sh
git clone https://github.com/johnathonfox/capytain.git
cd capytain
cargo check --workspace          # verifies the whole workspace compiles
cargo test --workspace           # runs all tests
cargo run -p mailcli -- --help   # the headless protocol CLI
```

The Tauri desktop app (`cargo tauri dev`) lands in Phase 0 Week 5. See [`PHASE_0.md`](./PHASE_0.md) for the current state.

A full quickstart — including platform build deps, contributor tooling, and the PR-gate local checks — lives in [`CONTRIBUTING.md`](./CONTRIBUTING.md#development-setup).

### Running the headless protocol CLI

`mailcli` is Capytain's headless protocol CLI, used during Phase 0 as a forcing function for crate boundaries. Once the Phase 0 weeks 3–4 subcommands ship, it'll look like:

```sh
cargo run -p mailcli -- auth add gmail your@gmail.com     # Phase 0 Week 3
cargo run -p mailcli -- list-folders your@gmail.com       # Phase 0 Week 4
cargo run -p mailcli -- list-messages your@gmail.com INBOX
```

Until then, `cargo run -p mailcli -- --log-level debug` exercises the binary stub and shared tracing init.

## Project structure

See [`DESIGN.md` §8](./DESIGN.md#8-project-structure-cargo-workspace) for the full layout. At a glance:

```
crates/         # library crates: core, storage, imap-client, jmap-client, ...
apps/
  desktop/      # Tauri + Dioxus desktop app
  mailcli/      # headless protocol CLI
docs/           # long-form design + operational docs
```

## Documentation

- **[`DESIGN.md`](./DESIGN.md)** — full design specification; protocols, architecture, security, licensing.
- **[`PHASE_0.md`](./PHASE_0.md)** — the current six-week execution plan.
- **[`TRAITS.md`](./TRAITS.md)** — core trait signatures (`MailBackend`, `DbConn`, `EmailRenderer`) and domain types.
- **[`COMMANDS.md`](./COMMANDS.md)** — IPC surface between the Dioxus UI and the Rust core.
- **[`CONTRIBUTING.md`](./CONTRIBUTING.md)** — how to contribute, DCO sign-off, PR process.
- **[`CODE_OF_CONDUCT.md`](./CODE_OF_CONDUCT.md)** — Contributor Covenant 2.1.

## Contributing

Pull requests are welcome, but please read the status note at the top of this README first: **there is no maintainer committed to reviewing or merging contributions at this time.** If you open a PR, it may sit. That's not rudeness; it's the honest state of the project. Fork freely.

If you want to contribute anyway, see [`CONTRIBUTING.md`](./CONTRIBUTING.md) for the DCO sign-off process (required for anything that does get merged) and the general shape of a good PR.

## Status and roadmap

Currently in **Phase 0** — foundations. The eventual 0.1 release targets Gmail and Fastmail support with a polished desktop experience on macOS, Windows, and Linux. See [`DESIGN.md` §11](./DESIGN.md#11-roadmap) for the full phased roadmap.

Phase weeks are aspirational, not scheduled. No release date is committed.

## License

Apache License 2.0. See [`LICENSE`](./LICENSE) for the full text and [`NOTICE`](./NOTICE) for third-party attributions.

> This project is provided as-is. The authors do not assert ownership over downstream use — fork it, sell it, rebrand it, build a business on it. The only thing asked is that you carry the license text and notices along with it.

## Trademarks

Capytain is not a registered trademark and the project asserts no trademark rights. If you hold an existing trademark and our name conflicts with yours, open an issue and we will rename. No letters or lawyers required.

## Acknowledgments

This project stands on the work of many:

- [Servo](https://servo.org/) — pure-Rust browser engine
- [Tauri](https://tauri.app/) — cross-platform Rust app framework
- [Dioxus](https://dioxuslabs.com/) — React-like UI in Rust
- [Turso](https://turso.tech/) — pure-Rust SQLite-compatible database
- [adblock-rust](https://github.com/brave/adblock-rust) — Brave's tracker-blocking engine
- [mail-parser](https://github.com/stalwartlabs/mail-parser), [lettre](https://github.com/lettre/lettre), [async-imap](https://github.com/async-email/async-imap), [jmap-client](https://github.com/stalwartlabs/jmap-client), and the broader Rust mail ecosystem

See [`NOTICE`](./NOTICE) for complete third-party attributions.
