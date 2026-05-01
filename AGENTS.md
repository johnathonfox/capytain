# QSL — Agent Notes

## What this is
A cross-platform desktop email client in Rust. Tauri 2 shell + Dioxus UI + Servo renderer + Turso (SQLite-compatible) storage. Pure-Rust end-to-end. Experimental — no maintainer committed to reviewing/merging PRs.

## Workspace layout
```
crates/          # 11 library crates: core, storage, imap-client, smtp-client, jmap-client, mime, sync, search, auth, ipc, telemetry
apps/desktop/    # Tauri + Dioxus desktop app (src-tauri/ shell, ui/ frontend)
apps/mailcli/    # headless protocol CLI
xtask/           # cargo xtask build/release scripts
```

## Toolchain
- Rust 1.94.0 pinned in `rust-toolchain.toml`. Auto-installed by rustup on first cargo invocation.
- Edition 2021, rustfmt `max_width = 100`, Unix newlines.
- `cargo-deny` v0.14+ for license/advisory checks. `reuse` for REUSE spec compliance.
- Node.js 20+ only for Tauri CLI tooling.

## Developer commands

### PR gate (run in this order before pushing)
```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
reuse lint
```
CI runs all five. `cargo deny check` takes ~30s; `reuse lint` is instant.

### Focused work
```sh
cargo check -p qsl-core          # fast compile check for one crate
cargo test -p qsl-core           # tests for one crate
cargo clippy -p qsl-core         # lint one crate
```

### Desktop app (dev mode — two terminals)
```sh
# Terminal 1: Dioxus UI watch build (emits to apps/desktop/ui/dist/)
cd apps/desktop/ui && dx serve --platform web

# Terminal 2: Tauri shell
cd apps/desktop/src-tauri && cargo tauri dev
```
- Requires `dioxus-cli` and `tauri-cli` installed (`cargo install dioxus-cli`, `cargo install tauri-cli --version '^2'`).
- Core Rust changes require restarting `cargo tauri dev`. UI changes hot-reload.
- Set `QSL_SKIP_UI_BUILD=1` to skip the `dx build --platform web` step in `build.rs` (CI does this).

### Platform build dependencies (Linux)
```sh
sudo apt install build-essential libssl-dev pkg-config libwebkit2gtk-4.1-dev libgtk-3-dev libxdo-dev libayatana-appindicator3-dev librsvg2-dev
```
macOS needs Xcode CLI tools; Windows needs VS2022 with C++ workload.

## Architecture notes
- **Workspace lints:** `unsafe_code = "deny"`, `clippy::all = "warn"` at workspace level. Do not add `unsafe`.
- **IPC contract:** All Tauri commands defined in `crates/ipc`. Full command list and conventions in `COMMANDS.md`. No `imap_*` or `jmap_*` command names at IPC — backend is abstracted.
- **TLS:** All TLS crates use rustls + ring. Never add native-tls. `lettre` pinned with `default-features = false` for this reason.
- **Turso:** `default-features = false` to avoid mimalloc `#[global_allocator]` conflict with Servo's jemalloc.
- **Linux webkit2gtk:** On NVIDIA proprietary driver, DMA-BUF renderer fails. Binary sets `WEBKIT_DISABLE_DMABUF_RENDERER=1` on Linux startup if not already set.
- **License:** Apache-2.0. cargo-deny enforces this strictly. MPL-2.0 deps allowed only via explicit exceptions in `deny.toml` — do not add new ones without `deny.toml` update and justification.

## Testing conventions
- **Unit tests:** inline `#[cfg(test)] mod tests` in source files.
- **Integration tests:** `<crate>/tests/` directory.
- **Fixtures:** `<crate>/tests/fixtures/` (test emails, OAuth responses).
- **Network in tests: forbidden.** Use recorded fixtures or mocked backends. Live tests must be `#[ignore]` with a comment explaining manual run.
- **Snapshots:** use `cargo insta` convention, snapshots alongside tests.
- **Proptest:** available for property-based testing in storage layer.

## New file conventions
- **Source files (.rs):** add `// SPDX-License-Identifier: Apache-2.0` header.
- **Docs/config (.md, .toml, .yml, .json, etc.):** no inline header needed — covered by `REUSE.toml`.
- Every new source file needs an `SPDX-FileCopyrightText` either inline or via REUSE.toml.

## Commit conventions
- **DCO sign-off required:** use `git commit -s` (or `-S -s` for GPG + DCO). CI fails without it.
- Fix: `git commit --amend --signoff && git push --force-with-lease`
- Conventional Commits encouraged but not required. Squash merge is the strategy.

## Key docs
- `DESIGN.md` — full design spec (protocols, architecture, security, licensing)
- `PHASE_0.md` — current execution plan and status
- `TRAITS.md` — core trait signatures (`MailBackend`, `DbConn`, `EmailRenderer`)
- `COMMANDS.md` — IPC surface between Dioxus UI and Rust core
- `CONTRIBUTING.md` — DCO, PR process, testing expectations
- `docs/KNOWN_ISSUES.md` — consciously-accepted issues with resolution criteria
