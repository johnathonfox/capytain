# Contributing to QSL

> **Before you start:** QSL is an experimental personal project published in the open. There is no maintainer committed to reviewing, responding to, or merging contributions at this time. If you open a PR or issue, it may sit without a response. That's the honest state — please set expectations accordingly.
>
> If you want to work on QSL anyway, this guide covers the shape of a good contribution so that, if one eventually gets reviewed, it has the best chance of being merged. It also covers the DCO sign-off that any merged contribution will need.

## Before You Start

1. Read the [Code of Conduct](./CODE_OF_CONDUCT.md). It still applies even without active moderation.
2. Skim [`DESIGN.md`](./DESIGN.md) for the project's values and architecture.
3. Check [`PHASE_0.md`](./PHASE_0.md) to see what's currently in flight and what's out of scope for the current phase.

If you're planning a large change, open a discussion issue first — not because you're guaranteed a reply, but because a public record of the design conversation is useful when someone does eventually look at the PR.

## Ways to Contribute

- **Report bugs.** Open an issue with a minimal reproduction. Include OS, Rust version, and what you expected vs. what happened.
- **Propose features.** Open a discussion issue with the problem you're trying to solve, not the solution you have in mind. We'll work out the design together.
- **Submit code.** Pull requests are welcome for open issues, or for changes discussed first on an issue.
- **Improve documentation.** Typos, clarifications, missing examples — all welcome. Doc PRs go through the same review process as code but move faster.
- **Add to the test corpus.** If you find an email that renders badly, contributing it (sanitized) to `tests/fixtures/emails/` is one of the highest-leverage things you can do.
- **Testing on real hardware.** We especially need people running the app on diverse Linux distributions and Windows versions to shake out platform-specific issues.

## Development Setup

Target: from a fresh machine and a fresh clone to `cargo test --workspace` passing in under 10 minutes. If it takes you longer than that, please [file an issue](https://github.com/johnathonfox/qsl/issues) — the quickstart is broken and we want to know.

### 1. Install the Rust toolchain

QSL pins its toolchain with `rust-toolchain.toml`, so you don't have to pick a version. Install [rustup](https://rustup.rs/) and `cargo` will auto-download the pinned toolchain the first time you run a cargo command in the repo:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # macOS / Linux
# or, on Windows: https://rustup.rs/ → rustup-init.exe
```

### 2. Install platform build dependencies

- **macOS** — Xcode command-line tools: `xcode-select --install`. WKWebView (the webview Tauri uses) ships with the OS.
- **Windows** — Visual Studio 2022 with "Desktop development with C++" workload (needed for the MSVC linker). WebView2 Evergreen Runtime ships with Windows 11; on Windows 10 install it from Microsoft if not already present.
- **Linux** — Debian/Ubuntu: `sudo apt install build-essential libssl-dev pkg-config libwebkit2gtk-4.1-dev libgtk-3-dev libxdo-dev libayatana-appindicator3-dev librsvg2-dev`. Other distros: install the equivalent WebKit2GTK 4.1 + GTK 3 + AppIndicator3 + librsvg development packages. Tauri pulls these in through `cargo check`; you need them to build `apps/desktop/src-tauri`, not just to run it.

### 2a. Install Tauri and Dioxus CLIs

Only needed for running the desktop app; not required for `cargo test --workspace`.

```sh
cargo install tauri-cli --version '^2'   # `cargo tauri dev`, `cargo tauri build`
cargo install dioxus-cli                 # `dx serve`, `dx build --platform web`
```

Running the app in dev mode:

```sh
# Terminal 1 — build + watch the Dioxus UI, emits to apps/desktop/ui/dist/
cd apps/desktop/ui && dx serve --platform web

# Terminal 2 — launch the Tauri shell, which serves apps/desktop/ui/dist/
cd apps/desktop/src-tauri && cargo tauri dev
```

Hot reload works for UI changes (`dx` re-emits, Tauri picks it up). Core Rust changes require restarting `cargo tauri dev`.

### 3. Clone and check

```sh
git clone git@github.com:johnathonfox/qsl.git
cd qsl
cargo test --workspace
```

The first `cargo` invocation downloads the pinned toolchain (~100 MB) and compiles every workspace crate. Subsequent runs are incremental and fast.

### 4. Install contributor tooling

Each of these is only needed for the matching local check. CI runs its own isolated copies, so if you only ever push and let CI gate your PR you don't strictly need them locally:

```sh
cargo install cargo-deny          # license + advisory auditing
pipx install reuse                # or: pip install --user reuse
```

### 5. Run the full PR gate locally

These are the exact checks CI runs. Running them before you push saves a CI round-trip:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
reuse lint
```

### Editor setup

- **VS Code:** install [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer). No further config needed.
- **JetBrains:** [RustRover](https://www.jetbrains.com/rust/) works out of the box; IntelliJ + the Rust plugin also works.
- **Neovim / Helix / Zed:** `rust-analyzer` over LSP.

If rust-analyzer is slow on a full workspace check, add `--package <name>` when you're only editing one crate.

### Troubleshooting

- *"error: no override and no default toolchain set"* — You haven't installed rustup. See step 1.
- *Slow or stalled `cargo test` on first run* — The pinned toolchain is downloading; it's a one-time ~100 MB fetch.
- *`reuse lint` complains on files I didn't touch* — every new file needs either an `SPDX-License-Identifier` header inline or coverage by `REUSE.toml`. For docs and config, add the glob to `REUSE.toml`; for source files, add the inline header.
- *`cargo tauri dev` fails* — Tauri isn't wired up yet. It lands in Phase 0 Week 5.

## Developer Certificate of Origin (DCO)

**All contributions require a DCO sign-off.** We use DCO instead of a CLA because it doesn't require you to sign any document or assign rights to anyone — you just certify that you wrote the code (or have permission to contribute it) and that it can be distributed under Apache 2.0.

The DCO is a simple text at [developercertificate.org](https://developercertificate.org/). You agree to it by adding a `Signed-off-by:` line to every commit message:

```
Signed-off-by: Your Name <your.email@example.com>
```

The easiest way is to use `git commit -s` (or `-S -s` if you also GPG-sign), which adds the line automatically using your `user.name` and `user.email` git config.

If you forget, our CI will fail. You can fix it by amending:

```sh
git commit --amend --signoff
git push --force-with-lease
```

Or for a whole branch:

```sh
git rebase --signoff HEAD~N   # where N is the number of commits to fix
```

**All contributions are licensed under Apache 2.0 via the DCO.** You keep the copyright on your contribution; you grant the project and its downstream users the Apache 2.0 license to use it. We do not take ownership of anyone's code.

## Pull Request Process

1. **Fork and branch.** Branch off `main`. Name your branch something descriptive: `fix/gmail-oauth-redirect-parsing`, `feat/jmap-email-changes-delta`.
2. **Keep PRs small and focused.** One logical change per PR. If you find yourself writing "this also...", split it.
3. **Write tests.** Every PR that changes behavior should include tests. If testing is impractical for your change, say so in the PR description.
4. **Update docs.** If your change affects how the project is used or built, update the relevant `.md` file in the same PR.
5. **Run the local checks** listed in [Development Setup](#development-setup).
6. **Fill out the PR template.** It asks three things: what problem does this solve, how does it solve it, how did you test it.
7. **Respond to review.** Reviews are best-effort — reviewers are volunteers. If your PR has been sitting for more than two weeks without a response, a polite ping is fine.

### What makes a PR easy to merge

- Minimal scope. One concern at a time.
- Clear commit messages explaining the *why*, not just the *what*.
- Tests that would have caught the bug you're fixing.
- Clean diff — no unrelated whitespace or import reordering.
- DCO sign-off on every commit.
- Passes CI on first push.

### What makes a PR hard to merge (avoid these)

- Mixing refactor and feature changes.
- Touching files outside what your change actually needs.
- "Fix typo" commits that cross into behavioral changes.
- Generated code or vendored dependencies added without discussion.
- Changes to core traits (`MailBackend`, `DbConn`, `EmailRenderer`) without an RFC-style discussion first.

## Commit Messages

We don't enforce a strict format, but good commit messages follow this shape:

```
Short summary (50 chars or less)

Longer explanation if needed, wrapped at 72 characters. Explain what
the change does and why it's needed. Reference issues with "Fixes #123"
or "Refs #456".

Signed-off-by: Your Name <your.email@example.com>
```

[Conventional Commits](https://www.conventionalcommits.org/) (e.g. `feat:`, `fix:`, `docs:`, `refactor:`) are encouraged but not required. If you use them, be consistent across your branch.

## Review and Merge

- **Review turnaround:** none promised. Per the status note, there's no maintainer committed to reviewing PRs. When a review does happen, expect it to be thorough; when none happens, please don't take it personally.
- **Required approvals:** one maintainer approval to merge, two for changes to core traits (`MailBackend`, `DbConn`, `EmailRenderer`) or the IPC surface.
- **Merge strategy:** squash merge. Your commit message becomes the merge commit. This keeps `main` history clean; your branch history is preserved in the PR.

## Testing Expectations

- **Unit tests** live inline in `#[cfg(test)] mod tests` within each source file.
- **Integration tests** live in each crate's `tests/` directory. One example test ships in `crates/core/tests/serde_roundtrip.rs`; use it as a template for shape.
- **Fixtures** (test emails, test OAuth responses) live in `tests/fixtures/` per crate.
- **Network access in tests is forbidden.** Use recorded fixtures or mocked backends. If a test genuinely needs a live server, mark it `#[ignore]` with a comment explaining how to run it manually.
- **Snapshots** (for rendering tests) live alongside the test that generates them; `cargo insta` is the convention.

Run the whole suite with `cargo test --workspace`; run a single crate with `cargo test -p <crate-name>` (e.g. `cargo test -p qsl-core`).

## Release Flow

Phase 0 is pre-release; there is no signing, distribution, or update channel yet. Release tooling (`cargo tauri build`, code signing, notarization, `.deb`/`.rpm`/AppImage/Flatpak packaging, auto-updater) lands in Phase 4. See [`DESIGN.md` §7](./DESIGN.md#7-build-distribution-and-platforms) and [`PHASE_0.md`](./PHASE_0.md) Phase 4 for the plan.

For now, the "release" process is: PR merges to `main`, CI is green, contributors run `git pull`.

## Security Issues

Do not open public issues for security vulnerabilities. Use GitHub's private vulnerability reporting feature on the `johnathonfox/qsl` repository (Security tab → "Report a vulnerability"). This routes the report through GitHub without requiring a maintained email address.

**Note on response times:** per the status note on the repository, there is no maintainer committed to security triage at this time. Reports will be seen when someone with access checks, not on any guaranteed timeline. If you need a timely response, a public CVE filing is faster.

## Communication

- **GitHub Issues:** bugs, feature requests, documentation problems. Responses are not guaranteed.
- **GitHub Discussions:** questions, design conversations, show-and-tell. Responses are not guaranteed.

There is no Discord, Matrix, mailing list, or maintainer email. GitHub is the only channel.

## Areas Especially in Need of Help

- **Servo composition on Windows and Linux.** macOS is further along.
- **Turso integration testing.** We're early downstream and want to catch bugs before our users do.
- **Real-world email corpus.** If you receive mail from senders that produces weird rendering, contributing sanitized samples helps everyone.
- **Accessibility.** Dioxus and Servo both have accessibility stories; we need contributors who care about a11y to help shape ours.
- **Linux packaging.** AppImage, Flatpak, and `.deb`/`.rpm` packaging each has its own conventions.

## Recognizing Contributors

All contributors are acknowledged in [`NOTICE`](./NOTICE) after their first merged PR. We don't maintain a separate CONTRIBUTORS file; git history is the record of truth.

## License

By contributing, you agree that your contribution is licensed under the Apache License 2.0, same as the rest of the project. The DCO sign-off on your commits is the legal record of this.
