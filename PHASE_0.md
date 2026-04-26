# Phase 0 — Foundations

**Duration:** 6 weeks
**Goal:** A working desktop app that can OAuth into a Gmail and a Fastmail account, list folders and recent messages on each, render one email via an embedded Servo WebView, and be contributed to cleanly (licensing, CI, conventions all locked in).

Phase 0 exists to de-risk the three things that are hardest to back out of later: protocol abstraction shape (IMAP vs JMAP behind one trait), storage contract (Turso behind `DbConn`), and Servo composition (native child surface across three platforms). If all three work, Phase 1 is straightforward feature work. If any one of them breaks down, we find out now, not in week 15.

This doc lives at `/PHASE_0.md` in the repo root and should be treated as the executable version of `DESIGN.md` §11 Phase 0.

---

## Week 1 — Project Bootstrap

**Objective:** Legal, structural, and process foundations in place before any domain code lands.

| Day | Task | Done when |
|---|---|---|
| 1 | Execute all 10 steps from `DESIGN.md` §10.2 | `LICENSE`, `NOTICE`, `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `deny.toml`, `.reuse/` all in place; `reuse lint` clean; README has License section |
| 1 | Repo init, branch protection rules, DCO check action | Main branch protected; GitHub Action rejects non-signed-off PRs |
| 2 | Cargo workspace skeleton with the crate layout from `DESIGN.md` §8 | `cargo check --workspace` succeeds with empty crates |
| 2 | `rust-toolchain.toml` pinned, `rustfmt.toml`, `.cargo/config.toml` | Fresh clone → `cargo build` works with no local Rust config |
| 3 | `crates/core` error types and ID newtypes per `TRAITS.md` | All domain types compile; `cargo test -p core` runs (even if zero tests) |
| 3 | `crates/core` common data types (`Folder`, `MessageHeaders`, `MessageBody`, etc.) per `TRAITS.md` | Types are `Serialize + Deserialize`; doc comments present |
| 4 | `tracing` setup with a shared init helper used by every binary | `mailcli` binary (stub) initializes tracing; logs go to stderr with configurable level |
| 4 | Test layout conventions: unit tests inline, integration tests in `tests/`, fixtures in `tests/fixtures/` | One example integration test exists and passes |
| 5 | CI scaffolding: fmt, clippy (deny warnings), test, `cargo-deny`, `reuse lint` | All pass on main; required to pass on PRs |
| 5 | Contributor docs: `CONTRIBUTING.md` expanded with local dev setup, testing, and release flow | New contributor can go from `git clone` to running tests in under 10 minutes |

**Exit criteria:** External contributor can clone the repo, follow README, and get a green `cargo test --workspace` within 10 minutes on a clean machine.

---

## Week 2 — Storage Layer

**Objective:** Turso is integrated, schema v1 is live, and all CRUD goes through the `DbConn` trait.

| Day | Task | Done when |
|---|---|---|
| 1 | Add Turso dependency to `crates/storage`, implement `DbConn` trait per `TRAITS.md` over Turso's connection API | `DbConn::execute`, `query`, `query_one`, `begin` all work against an in-memory Turso db |
| 2 | Migration runner: apply SQL files from `crates/storage/migrations/` in order, track in `_schema_version` table | Migrations apply, are idempotent, fail loudly on version mismatch |
| 3 | Schema v1 migration: `accounts`, `folders`, `messages`, `threads`, `attachments`, `outbox`, `contacts` per `DESIGN.md` §4.4 | Migration runs on fresh db; schema verified via `PRAGMA table_info` |
| 4 | Repository layer: one module per domain type with CRUD functions taking `&dyn DbConn` | `AccountRepo::insert`, `get`, `list`, `update`, `delete` for each type; all async |
| 4 | Blob store for raw `.eml` bodies: write to `<data_dir>/blobs/<account>/<folder>/<uid>.eml` with optional zstd | Round-trip: write a 100KB eml, read it back byte-identical |
| 5 | Integration tests: property test that round-trips every domain type through the db | `cargo test -p storage` passes with at least 80% coverage on repos |
| 5 | Document Turso upstream engagement: first bug report (if any), decision log in `docs/dependencies/turso.md` | File exists with Turso version pinned, known issues listed, upstream links |

**Exit criteria:** Any domain operation (insert an account, list folders, update message flags) works end-to-end via `DbConn`, is covered by tests, and uses no Turso-specific types above the `crates/storage` boundary.

**Open question to resolve this week:** Pick N for the "vendor a patch after N days of upstream stall" policy. Suggest 14.

---

## Week 3 — Authentication

**Objective:** OAuth2 flows work for Gmail and Fastmail from a CLI, tokens persist across restarts via OS keychain.

| Day | Task | Done when |
|---|---|---|
| 1 | `crates/auth` skeleton, `oauth2` crate integration, PKCE flow | Unit tests exercise PKCE code challenge/verifier generation |
| 2 | Provider profile trait: authorization URL, token URL, scopes, redirect URI handling (loopback) | `trait OAuthProvider` with `profile()` method returning config |
| 3 | Gmail profile: scopes `https://mail.google.com/`, loopback redirect on `http://127.0.0.1:<ephemeral>/`, PKCE | End-to-end flow in `mailcli auth add gmail` opens browser, captures redirect, exchanges code for tokens |
| 4 | Fastmail profile: scopes for JMAP, same loopback redirect model | Same flow works for Fastmail |
| 5 | Keyring integration: store refresh token per account, retrieve on startup, rotate on refresh | Refresh tokens survive app restart; OS keychain has one entry per account |
| 5 | Token refresh helper: given an `AccountId`, return a valid access token, refreshing via the stored refresh token if needed | Helper is the single code path used by both the IMAP and JMAP adapters in week 4 |

**Exit criteria:** `mailcli auth add gmail foo@gmail.com` and `mailcli auth add fastmail foo@fastmail.com` both complete OAuth flows, store refresh tokens in the keyring, and `mailcli auth list` shows both accounts. Restarting `mailcli` does not require re-authenticating.

**Phase 0 invariant:** No password handling anywhere, per §1. The only input the user types is the email address; everything else is browser-based.

---

## Week 4 — Protocol Adapters (Read)

**Objective:** The `MailBackend` trait is implemented for both IMAP (Gmail) and JMAP (Fastmail), and `mailcli` can list folders and fetch headers against real accounts.

| Day | Task | Done when |
|---|---|---|
| 1 | `crates/imap-client` — `MailBackend` impl over `async-imap`. Just `list_folders` and `list_messages` first. | `mailcli list-folders <gmail-account>` returns all folders, including Gmail's label-as-folder mapping |
| 2 | IMAP sync state: `(uidvalidity, highestmodseq, uidnext)` as the `SyncState.backend_state` payload (serialized) | `list_messages(folder, None)` returns headers + new state; `list_messages(folder, Some(state))` returns only the delta |
| 2 | Server capability check: reject at connect time if CONDSTORE/QRESYNC/IDLE missing, with a clear error | Connection to a hypothetical server without QRESYNC fails with `MailError::Protocol("QRESYNC required")` |
| 3 | `crates/jmap-client` — `MailBackend` impl over `jmap-client`. Same two methods. | `mailcli list-folders <fastmail-account>` returns mailboxes; `list_messages` returns via `Email/query` + `Email/get` |
| 4 | JMAP sync state: opaque state string from the server, passed back for `Email/changes` | Delta sync works symmetrically to IMAP |
| 4 | `fetch_message` on both backends: full body with parsed HTML/text parts via `mail-parser` | `mailcli show-message <id>` prints headers + plaintext body |
| 5 | Integration tests against recorded fixtures: VCR-style cassettes for Gmail and Fastmail flows | Tests pass without network access; CI runs them on every PR |

**Exit criteria:** `mailcli sync <account>` fetches the latest state of the INBOX folder from either backend, writes headers to the db via `DbConn`, and prints `Synced N new messages, M removed, in T ms`.

**Open question to resolve this week:** Whether to wrap `async-imap` and `jmap-client` as-is or to adopt Stalwart's protocol crates. Default recommendation: use them directly; adopt Stalwart later if our thin wrappers prove insufficient.

---

## Week 5 — App Shell (Tauri + Dioxus)

**Objective:** A window opens, it shows real data, and one IPC command round-trips end-to-end.

| Day | Task | Done when |
|---|---|---|
| 1 | `apps/desktop/src-tauri/` scaffolding: Tauri 2 project, build config, signing placeholders | `cargo tauri dev` opens an empty window |
| 1 | `apps/desktop/ui/` scaffolding: Dioxus web target, WASM build, served by Tauri | The empty window shows "Hello from Dioxus" |
| 2 | IPC surface scaffolding per `COMMANDS.md`: derive `serde` for all command inputs/outputs, register tauri commands, route to core | Dioxus calls `invoke("accounts_list")`, gets `Vec<Account>` back as JSON |
| 3 | Sidebar component: lists accounts and their folders using real data | After running `mailcli auth add gmail ...` in week 3, the sidebar shows the Gmail account |
| 4 | Message list pane: given a selected folder, shows headers via `messages_list` command | Selecting a folder displays its last 50 headers |
| 5 | Message reader pane placeholder: shows subject/from/plaintext body. HTML rendering comes in week 6. | Clicking a message shows its text/plain fallback |
| 5 | Hot reload works end-to-end for both Rust and Dioxus changes | `dx serve` hot-reloads the UI; core changes require restart (known limitation, documented) |

**Exit criteria:** Running `cargo tauri dev` produces a window with working sidebar (accounts/folders), message list, and text-only reader, all backed by the real IMAP and JMAP implementations from week 4.

---

## Week 6 — Servo Composition Spike

**Objective:** Answer the "can we actually embed Servo in a Tauri window cross-platform" question definitively, document what we learned, and have a working email renderer on all three platforms.

| Day | Task | Done when |
|---|---|---|
| 1 | `crates/core/src/renderer.rs` — `EmailRenderer` trait per `TRAITS.md`. Null implementation for tests. | Trait compiles; mock renderer passes a test |
| 1–2 | `ServoRenderer` on macOS: embed Servo `WebView` as an `NSView` child of the Tauri window's `NSWindow`, render the sanitized HTML of one email | A hardcoded test email renders in the reader pane, with link clicks opening in Safari |
| 3 | `ServoRenderer` on Windows: same, as a child `HWND` of the Tauri `HWND` | Same test email renders on Windows |
| 4 | `ServoRenderer` on Linux: same, as a GTK widget | Same test email renders on Linux |
| 5 | Corpus rendering: 10 real-world emails (Gmail marketing, Substack newsletter, Stripe receipt, plaintext, GitHub notification, etc.), visual diff against reference screenshots | Each renders without crash; any visual regressions are filed as Servo issues |
| 5 | `docs/servo-composition.md` with findings: which parts of Servo's API we use, which platform quirks exist, what we contributed upstream if anything | Document is readable by the next engineer who touches the renderer |

**Exit criteria:** The app opens a real email from either a Gmail or Fastmail account and renders it via Servo with link clicks routed to the system browser, on all three platforms. No regressions in text selection, dark mode, or copy/paste.

---

## Phase 0 Done

By end of week 6:

- Two providers connected (Gmail via IMAP, Fastmail via JMAP), one abstraction.
- Storage layer pure Rust via Turso, all CRUD behind `DbConn`.
- OAuth2-only auth, refresh tokens in OS keychain, no password code path anywhere.
- Tauri + Dioxus app shell running with real data.
- Servo rendering real email bodies on all three platforms.
- Licensing, CI, tests, and contribution flow all solid.

At this point, Phase 1 (full read path — threading, notifications, remote-content blocking, etc.) becomes a feature-build rather than a platform-build.

---

## Phase 0 Deliverables Summary

| Deliverable | Path |
|---|---|
| Compiles and runs on all 3 platforms | `cargo tauri dev` |
| `mailcli` headless CLI for protocol testing | `apps/mailcli/` |
| Turso schema v1 | `crates/storage/migrations/0001_initial.sql` |
| OAuth flows for Gmail and Fastmail | `crates/auth/src/providers/` |
| `MailBackend` implementations | `crates/imap-client/`, `crates/jmap-client/` |
| `DbConn` implementation | `crates/storage/src/turso.rs` |
| `EmailRenderer` implementation | `crates/renderer/src/servo.rs` |
| Servo composition findings | `docs/servo-composition.md` |
| Turso engagement log | `docs/dependencies/turso.md` |
| Licensing artifacts | `LICENSE`, `NOTICE`, `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `deny.toml` |

## Deferred from Phase 0

Work that was scoped into Phase 0 but intentionally deferred after
smoke-testing the happy path on the available hardware / credentials.
Tracked here as "features for later" rather than Phase 0 exit
blockers.

| Deferred item | Why deferred | Rough shape of the remaining work |
|---|---|---|
| Fastmail OAuth + JMAP smoke test | Gmail path landed and validated end-to-end on a real account; Fastmail code (provider profile, JMAP adapter, scopes) all shipped in the same PRs but has not been exercised against a real Fastmail account. | Register a Fastmail OAuth client (Settings → Privacy & Security → Connected apps), set `QSL_FASTMAIL_CLIENT_ID` (+ `QSL_FASTMAIL_CLIENT_SECRET` if the registration type needs it), run `mailcli auth add fastmail <email>` → `mailcli sync <email>`. Debug whatever surfaces and update this row or remove it when green. |
| macOS runtime validation | `crates/renderer/src/servo/macos.rs` is marked UNVERIFIED — written to the `docs/servo-composition.md` §4.3 target shape without Mac hardware. CI builds it; nothing exercises `new_macos` on an actual AppKit window. | A one-session pass on Mac hardware: run `cargo run -p qsl-desktop`, confirm the reader pane reparents into an `NSView` child of the main Tauri window, confirm Servo paint lands there. Update the module's `# UNVERIFIED` marker to `# VERIFIED` or file whatever shows up. |
| Windows runtime validation | `crates/renderer/src/servo/windows.rs` same story: shipped UNVERIFIED, CI compiles it on `windows-latest`, no hardware-backed check. The stock `windows-latest` runner doesn't have an EGL driver, so the corpus test is already `cfg`-gated off Windows. | Equivalent to the macOS item above but on Windows hardware — verify `new_windows` gives Servo a usable `HWND` handle inside the Tauri frame and pixels land there. |

These three are the only items on the original Phase 0 plan that
are not verified end-to-end on main after the Gmail-smoke PR lands.
Everything else on the deliverables table above ships real, tested
code.

## Phase 0 Non-Goals

Explicitly **not** in Phase 0 (these belong to later phases):

- Threading / conversation view
- Search (Tantivy)
- Compose / send / SMTP / EmailSubmission
- Notifications
- Remote-content blocking / ad filter
- Rules / filters
- Preferences UI (any persisted preference can be a hardcoded constant in Phase 0)
- Multiple windows
- Accessibility polish (beyond what Servo + Dioxus give us for free)
- Code signing and distribution (Phase 4)
