# Capytain — Design Specification

> A modern, cross-platform, open-source desktop email client written primarily in Rust.
>
> **Status: experimental.** Capytain is a personal project published in the open under Apache 2.0. There is no maintainer committed to support or response at this time. Use, fork, and build on it freely under the terms of the license.

## 1. Overview and Goals

The goal is a fast, privacy-respecting, offline-capable email client that feels at home on macOS, Windows, and Linux. It should compete on user experience with Apple Mail, Thunderbird, Mailspring, and Spark, while being implemented in a memory-safe language with a small binary footprint and sensible defaults.

Primary goals:

- **Native feel on every platform.** Proper window chrome, keyboard shortcuts, system notifications, tray/menu-bar integration, and OS credential stores.
- **Offline-first.** Every message the user has synced is fully usable without a network. Actions queue and replay when connectivity returns.
- **Fast.** Cold start under one second on modern hardware. Search results under 50 ms on a 100k-message store.
- **Modern protocols only.** JMAP and IMAP (with CONDSTORE, QRESYNC, and IDLE required) as first-class citizens; SMTP submission for sending. **OAuth2 with PKCE is the only supported authentication mechanism** — no passwords, no app-specific passwords, no legacy auth. No POP3, no Exchange/EWS/EAS/MAPI. Providers that can't meet this bar are not supported.
- **Private by default.** No telemetry on by default. No third-party servers between the user and their mail provider. Credentials in OS keychains.
- **Rust-native throughout.** Prefer pure-Rust crates over C/C++ FFI bindings even when the pure-Rust option is newer or less battle-tested. When we find bugs, we file them, contribute fixes, and track upstream releases — that's part of the maintenance budget, not an emergency. The bet is that the pure-Rust ecosystem continues to mature faster if early adopters show up and contribute, and that the long-term gains (memory safety, consistent cross-platform behavior, single-language stack) are worth the short-term cost of rough edges.
- **Permissively open source.** Apache License 2.0. Built in the open with no restrictions on downstream use — commercial, closed-source, forked, rebranded, sublicensed, all fine. The only thing asked of anyone using this code is that they carry the license text and notices.

## 2. Non-Goals (v1)

To keep scope achievable these are explicitly deferred or excluded:

- **Exchange, EWS, EAS, and MAPI.** Not supported, not planned. Users on Microsoft 365 connect via IMAP+SMTP with OAuth2, which Microsoft supports.
- **POP3.** No offline-first client should be built on a protocol that lacks server-side state.
- **Any authentication mechanism other than OAuth2 with PKCE.** No passwords, no app-specific passwords, no NTLM, no GSSAPI/Kerberos, no plain auth. This explicitly excludes iCloud Mail and Yahoo Mail (both app-password-only in 2026) as well as any self-hosted server that doesn't implement OAuth2. TLS is mandatory for all connections; STARTTLS downgrade is never permitted.
- Mobile apps (though the architecture should not preclude them later).
- A hosted sync/backend service. The app talks directly to mail servers.
- Calendar, contacts, and task management as first-class modules. CardDAV contacts may be added for autocomplete only.
- Built-in CRM/tracking features (read receipts via tracking pixels, send-later-via-our-server, etc.).

## 3. Core Features

### 3.1 MVP (v0.1 – v0.5)

- Multi-account support with an architecture that accommodates multiple backend types behind a single `MailBackend` trait. V1 ships with **Gmail (IMAP+SMTP with OAuth2) and Fastmail (JMAP with OAuth2)** as two distinct backends, which forces the abstraction to be real from day one. Microsoft 365 and self-hosted follow in later phases (see §11).
- OAuth2 with PKCE as the exclusive auth path. Built-in provider profiles for **Gmail and Fastmail** at v1. Microsoft 365 and custom OAuth2 profiles for self-hosted servers follow in later phases.
- Unified inbox plus per-account views.
- Folder/label navigation with IMAP SPECIAL-USE and Gmail label awareness.
- Conversation threading (RFC 5322 References/In-Reply-To with subject fallback).
- HTML email rendering in a sandboxed webview with remote content blocked by default.
- **Tracker and ad-network blocking** by default, powered by the `adblock` engine (EasyList + EasyPrivacy + uBlock Origin filter lists). Blocks tracking pixels, tracking-domain image loads, and fingerprinting resources. Runs unconditionally — the per-sender remote-content opt-in cannot override tracker blocks.
- **Link cleaning on click.** Outbound clicks are stripped of known tracking parameters (`utm_*`, `fbclid`, `gclid`, `mc_cid`, `mc_eid`, `_ga`, `_gl`, and the EasyPrivacy `$removeparam` ruleset) and, where applicable, unwrapped from known redirect services (Mailchimp, SendGrid, Substack, HubSpot, t.co, etc.) before being handed to the system browser. Session tokens and functional parameters are preserved — only documented trackers are stripped.
- Full-text search across body, headers, and attachments' filenames.
- Compose with rich-text and plain-text modes, drafts synced via the Drafts folder/mailbox.
- Attachments: drag-and-drop, inline images, size warnings.
- Desktop notifications and unread counts on the dock/tray.
- Keyboard-driven navigation (Gmail-style shortcuts, remappable).
- Light and dark themes following the OS.

### 3.2 Post-MVP

- Rules and filters (client-side plus Sieve where supported).
- Snooze, send-later, undo-send (all client-side; no third-party server).
- Signatures, templates, and canned responses.
- PGP/MIME and S/MIME signing and encryption.
- Unified search scopes and saved searches.
- CardDAV contact autocomplete.
- Per-account identities and aliases.
- Plugin/extension API (see §9).

### 3.3 Supported Providers

The OAuth2-only stance narrows supported providers to those that implement RFC 6749 (OAuth 2.0) with RFC 7636 (PKCE). V1 ships Gmail and Fastmail together — the two providers represent the two backend types (IMAP and JMAP), so launching with both forces the `MailBackend` abstraction to be correct from the start rather than backfilled later.

| Provider | Protocol | Auth | Target Phase |
|---|---|---|---|
| Gmail / Google Workspace | IMAP + SMTP | OAuth2 | ✅ **v1** — first-class, built-in profile |
| Fastmail | JMAP | OAuth2 | ✅ **v1** — first-class, built-in profile (JMAP reference target) |
| Microsoft 365 / Outlook.com | IMAP + SMTP | OAuth2 | Phase 5 — second IMAP+OAuth2 provider |
| Stalwart (self-hosted) | IMAP and/or JMAP | OAuth2 | Phase 6 — custom OAuth2 config |
| Dovecot (self-hosted) | IMAP | OAuth2 (OAUTHBEARER) | Phase 6 — if admin configures OAuth2 token validation |
| Cyrus (self-hosted) | IMAP | OAuth2 (OAUTHBEARER) | Phase 6 — if admin configures OAuth2 token validation |
| Apple iCloud Mail | IMAP + SMTP | App-password only | ❌ No OAuth2 support from Apple |
| Yahoo Mail / AOL | IMAP + SMTP | App-password only | ❌ No OAuth2 for third-party clients |
| Self-hosted without OAuth2 | — | Password | ❌ Out of scope; see §12 |
| ProtonMail | — | — | ❌ Requires their Bridge; proprietary protocol |
| Tutanota | — | — | ❌ Proprietary protocol |
| On-prem Exchange / legacy | — | — | ❌ Explicitly unsupported |

The trade-off is conscious: dropping app-password providers gives up a significant user base (iCloud, Yahoo) in exchange for a simpler, safer auth implementation — one code path, one credential type, no plaintext-password handling ever.

## 4. Architecture

### 4.1 High-Level

The app is split into three layers that communicate over typed message channels:

```
┌──────────────────────────────────────────────────────────────┐
│                          UI Layer                            │
│   Dioxus (Rust → WASM), rendered in Tauri's system webview   │
│              (TypeScript is a documented alternative)        │
└───────────────▲───────────────────────────┬──────────────────┘
                │ IPC (serde + tauri cmds)  │
┌───────────────┴───────────────────────────▼──────────────────┐
│                        Core (Rust)                           │
│  ┌─────────────┐ ┌──────────────┐ ┌──────────────────────┐   │
│  │  Accounts   │ │  Sync Engine │ │  Search (tantivy)    │   │
│  └─────────────┘ └──────────────┘ └──────────────────────┘   │
│  ┌─────────────┐ ┌──────────────┐ ┌──────────────────────┐   │
│  │ Compose/Send│ │  Rules       │ │  Notifications       │   │
│  └─────────────┘ └──────────────┘ └──────────────────────┘   │
└───────────────▲───────────────────────────▲──────────────────┘
                │                           │
┌───────────────┴──────────┐ ┌──────────────┴──────────────────┐
│  Protocol Adapters       │ │  Storage                        │
│  IMAP | SMTP | JMAP      │ │  SQLite (messages, metadata)    │
│  OAuth2 | MIME parser    │ │  Blob store (raw RFC822 bodies) │
└──────────────────────────┘ └─────────────────────────────────┘
```

### 4.2 Process Model

Single application process with an async runtime (Tokio). Long-running sync work runs on dedicated task groups per account so one slow mailbox cannot block others. The UI runs on the webview's own process (Tauri model); UI and core communicate over JSON-serialized commands and event streams.

### 4.3 Sync Engine

- **Strategy:** offline-first with optimistic local mutations. Every user action becomes a local state change plus an outbox entry that the sync engine replays against the server. All backend-specific logic sits behind a single `MailBackend` trait with two v1 implementations (IMAP and JMAP) that exercise the abstraction in parallel.
- **IMAP:** requires CONDSTORE (RFC 7162) for efficient flag sync, QRESYNC for reconnect resync, and IDLE for push. Servers missing these are rejected at account setup with a clear error. Folder sync state is stored as `(uidvalidity, highestmodseq, uidnext)`.
- **JMAP:** uses server-provided state strings and `Email/changes` for delta sync, with EventSource for real-time push. Submission goes through `EmailSubmission/set`. First-class peer of IMAP, shipping in v1 against Fastmail.
- **Reconciliation:** local operations carry deterministic IDs; server confirmations update them in place. Conflicts on flags are resolved by last-write-wins per flag; conflicts on moves are resolved by server state (authoritative).

### 4.4 Data Model

The storage layer keeps metadata relationally and raw message bodies on disk. Sketch:

```
accounts(id, kind, display_name, address, auth_kind, auth_ref, server_config_json)
folders(id, account_id, path, role, uidvalidity, highestmodseq, uidnext, unread_count)
messages(id, account_id, folder_id, uid, message_id, thread_id,
         from_addr, to_addrs, subject, date, size,
         flags_bitmap, labels_json, snippet, body_path, indexed_at)
threads(id, account_id, root_message_id, subject_normalized, last_date, message_count)
attachments(id, message_id, filename, mime_type, size, inline, content_id, path)
outbox(id, account_id, op_kind, payload_json, created_at, attempts, next_attempt_at)
contacts(id, account_id, address, display_name, frequency, last_seen)
```

Raw `.eml` bodies live under `<data_dir>/blobs/<account>/<folder>/<uid>.eml` (optionally compressed with zstd). The search index is a separate Tantivy directory.

### 4.5 HTML Email Rendering

Email HTML is one of the most hostile inputs any desktop app handles — it's authored by anyone who can send a message, has historically been the attack surface for tracking pixels, credential-phishing overlays, XSS against the client itself, and CVE-worthy rendering-engine exploits. The pipeline is layered so that failure in any one layer still leaves the others holding:

**1. Extraction.** `mail-parser` pulls out the `text/html` alternative. If only `text/plain` exists, it's wrapped in a minimal stylesheet for consistent presentation; we don't guess at HTML from plain text.

**2. Sanitization.** `ammonia` strips all JavaScript (script tags, event handlers, `javascript:` URLs), form elements, `<iframe>`, `<object>`, `<embed>`, and anything else non-presentational. The allowlist is conservative and explicit — safer to miss a piece of formatting than to admit a tag that shouldn't render.

**3. Remote content rewriting and filter-list blocking.** External resource URLs (images, stylesheets, fonts) go through two gates. First, every URL is checked against the `adblock` engine loaded with EasyList, EasyPrivacy, and the uBlock Origin unbreak list. Anything that matches — tracking pixels, mailchimp/sendgrid telemetry endpoints, ad-network CDNs, fingerprinting scripts — is blocked unconditionally. Second, anything that survived is either replaced with a placeholder (default) or allowed through (if the sender is on the per-sender opt-in list, stored in the contacts table). The filter-list pass runs *before* the opt-in check, so opting in to a sender's images never opens a back door to their tracking infrastructure. URLs are never rewritten through a proxy — that would violate the "no third party between user and their mail" principle.

**4. Rendering via Servo.** The sanitized HTML is handed to an embedded Servo `WebView` with JavaScript execution disabled at the engine level (both ammonia stripping and Servo config — belt and suspenders). The Servo instance:

- Runs with a locked-down configuration that disables networking entirely, except for an internal resolver that serves inline/opted-in resources from our content pipeline.
- Is composed into the Tauri window as a native child surface (NSView on macOS, HWND on Windows, GTK widget on Linux), positioned where the email body sits in the app layout.
- Captures link-click events and, before routing to the system default browser, runs the URL through a cleaning pass: known tracking parameters are stripped (via the `adblock` engine's `$removeparam` rules plus a small hardcoded list of tracker param names), and URLs from known redirect wrappers (Mailchimp, SendGrid, Substack, HubSpot, t.co, etc.) are unwrapped to their real destination. Session tokens and unknown parameters are preserved. The cleaned URL is what actually opens.
- Is torn down and recreated between messages so no state can leak across senders.

All rendering goes through a narrow trait in the core:

```
trait EmailRenderer {
    fn render(&mut self, sanitized_html: &str, policy: RenderPolicy) -> RenderHandle;
    fn on_link_click(&mut self, cb: impl FnMut(Url));
    fn destroy(&mut self);
}
```

The v1 implementation is `ServoRenderer`. The trait exists for clean architecture and testability — mockable in tests, swappable if the pure-Rust rendering landscape produces something better later — not as an escape hatch. Per §1, we're committed to the Rust-native path and treat upstream Servo engagement as part of the project's maintenance budget.

**Rendering consistency is actually a Servo win.** The system-webview approach means emails render differently on macOS (WebKit), Windows (Chromium/Edge), and Linux (WebKitGTK) — the Linux path in particular frequently surprises people because email marketers don't test against WebKitGTK. Servo renders the same HTML the same way on all three platforms, which is a meaningful improvement for a cross-platform email client. The trade-off is that Servo's absolute compatibility with weird real-world email HTML is less proven than WebKit's two decades of battle-testing.

**Alternative considered and rejected:** offscreen rasterization with image blit. Render the email to a bitmap, display as an image. Maximum isolation but breaks text selection, copy/paste, accessibility, link clicking, reflow, and dark-mode adaptation. The native-surface embedding model keeps all of those working.

## 5. Technology Stack

### 5.1 Primary Recommendation: Tauri 2 + Rust Core

**Why Tauri as the shell:**

- HTML email rendering is a hard requirement, and Tauri uses the system webview (WebKit on macOS, WebView2 on Windows, WebKitGTK on Linux), so you get a real browser engine without bundling Chromium. (Email bodies themselves render via Servo — see §4.5 — but the app chrome leans on the system webview.)
- Binary size is small (single-digit MBs) compared to Electron.
- Tauri's IPC, updater, tray, notifications, and signing pipeline are mature as of 2026.
- Per §1 (Rust-native throughout), the UI is written in **Dioxus** (Rust, compiled to WASM and rendered inside Tauri's webview). TypeScript remains a documented alternative for contributor pool reasons — see §5.2.

**Suggested stack:**

| Layer | Choice | Notes |
|---|---|---|
| Shell (app UI) | **Tauri 2** | cross-platform, system webview for app chrome/UI, good signing/updater story; does *not* render email bodies |
| Email rendering | **Servo** (v0.1.x, crates.io) | pure-Rust browser engine, embedded as a native child surface; cross-platform rendering consistency |
| UI framework | **Dioxus** (web target, Rust → WASM) | Rust-native UI framework with React-like model; built-in Tailwind support in 0.7+; renders inside Tauri's webview |
| Styling | **Tailwind CSS** via Dioxus's built-in support, or inline styles | Dioxus 0.7 ships zero-setup Tailwind; keep it small either way |
| Async runtime | **Tokio** | standard |
| IMAP | **async-imap** | CONDSTORE, QRESYNC, IDLE all required; reject servers missing them |
| SMTP | **lettre** | submission port 587 / implicit TLS 465 only; SASL XOAUTH2 for OAuth providers |
| JMAP | **jmap-client** | v1 implementation; Fastmail as the reference server for the JMAP backend |
| MIME parsing | **mail-parser** | fast, zero-copy where possible |
| MIME building | **mail-builder** | pairs with mail-parser |
| HTML sanitization | **ammonia** | strip scripts, remote content on request |
| Tracker/ad blocking | **adblock** (Brave's adblock-rust) | EasyList/EasyPrivacy/uBlock Origin filter-list matching; pure Rust, production-proven in Brave |
| URL parsing and cleaning | **url** crate + hand-maintained tracker-param / redirect-wrapper lists | strips `utm_*`, `fbclid`, etc.; unwraps Mailchimp/SendGrid/t.co-style redirects; `adblock`'s `$removeparam` rules feed the same pipeline |
| Auth | **oauth2** crate + PKCE | exclusive auth path; built-in profiles for Gmail/Microsoft/Fastmail, custom profiles for self-hosted OAuth2 |
| Credential storage | **keyring** | wraps macOS Keychain, Windows Credential Manager, Secret Service |
| Database | **Turso** (pure-Rust, SQLite-compatible) | native async, BEGIN CONCURRENT for MVCC writes, encryption at rest built-in; beta as of early 2026 — see note below |
| Migrations | Hand-rolled runner or **refinery** against Turso's connection | standard SQL DDL; works unchanged thanks to SQLite compatibility |
| Full-text search | **tantivy** | embedded, Lucene-like, fast |
| DKIM/SPF/DMARC | **mail-auth** | for outbound signing and inbound verification |
| Logging | **tracing** + **tracing-subscriber** | structured logs, spans per account/folder |
| Error handling | **thiserror** in libs, **anyhow** at the binary edges | standard Rust pattern |
| Serialization | **serde** + **serde_json** | for IPC and config |
| Config | **figment** or hand-rolled TOML | per-user config at `~/.config/capytain/` |

**A note on Turso.** Turso keeps the storage layer pure Rust with native async, MVCC via BEGIN CONCURRENT, and encryption at rest built-in. It's in beta as of early 2026, which means we should expect to file bugs, occasionally carry patches, and track releases closely — that's the cost of the Rust-native principle in §1, and it's budgeted in. The `crates/storage` layer sits behind a connection trait for clean separation and testability, not as a swap-out hatch. Turso's file-format compatibility with SQLite is still a useful property — any SQLite CLI works against our database files for debugging and forensics — but it's a tooling bonus, not a rip-cord.

**A note on Servo.** Servo's `0.1.0` embedding crate shipped in April 2026 — very new, with API churn expected through the `0.1.x` line. We're betting on it for rendering consistency across platforms (emails look the same on macOS, Windows, and Linux instead of drifting per system webview), for pure-Rust memory safety in the most exposed parsing surface in the app, and for the long-term pure-Rust story per §1. Expect to file compatibility bugs against real-world email HTML, vendor patches when needed, and track Servo releases in our own release train. The `EmailRenderer` trait in §4.5 exists for clean architecture and testability; it's not a swap-out hatch. Phase 0 includes a composition spike to hit platform-specific integration issues early, while the code base is small enough to react.

### 5.2 Alternatives

**TypeScript + SolidJS or React** (contribution-pool escape hatch). Dioxus is young and its contributor base is smaller than the React/Solid world. If attracting external contributors becomes a priority that outweighs the Rust-native principle in §1, TypeScript in the Tauri webview is a clean swap — the Rust core and IPC surface stay identical, only the `apps/desktop/ui/` tree changes language. This is documented as a fallback for project-health reasons, not a technical preference.

**Dioxus Native (Blitz) — more ambitious, post-v1.** Dioxus Native renders the UI with Blitz (which uses Stylo, Taffy, Parley, and Vello — the pure-Rust layout/render stack from the Servo ecosystem) and skips the system webview entirely. This would make the app UI rendering pure Rust end-to-end, consistent with the §1 principle taken to its limit. Blitz is still alpha, so we're not adopting it for v1, but it's the natural long-term direction if the project wants to be fully independent of system webviews for its own UI.

### 5.3 Frameworks Considered and Rejected for v1

- **Iced, egui, Slint, Floem:** beautiful pure-Rust GUIs, but they don't render HTML and can't host an embedded Servo `WebView` for email content as naturally as Tauri can.
- **Electron:** works fine but conflicts with the Rust-native principle and the small-footprint goal.
- **GTK/Qt direct bindings:** locks the UI to C/C++ ecosystem conventions and complicates cross-platform styling.

## 6. Security and Privacy

- Only OAuth2 refresh tokens are persisted, and they live in the OS keychain via the `keyring` crate. The client never handles, requests, or stores user passwords of any kind. Refresh tokens are treated as secrets: never logged, never sent to the UI process, never written to the database, and rotated whenever the provider issues a new one.
- The message-rendering webview is sandboxed: no Node/Rust API access, strict CSP, external content (`<img src="http…">`, fonts, CSS) blocked until the user opts in per-sender.
- Outbound links open in the default browser, never in-app. Before opening, URLs pass through a cleaning pass that strips known tracking parameters and unwraps known redirect services (see §4.5 layer 4). Only documented tracker param names and redirect-wrapper patterns are touched — session tokens, functional query parameters, and unknown params pass through unchanged to avoid breaking receipts, password-reset links, and similar.
- Tracker and ad-network blocking runs unconditionally via the `adblock` engine (EasyList, EasyPrivacy, uBlock Origin unbreak list). Filter lists ship with the app and are refreshed on a configurable cadence (default: weekly). The user can disable specific lists or add custom rules, but the block pass cannot be bypassed by per-sender remote-content opt-in — that would let any newsletter subscription open a tracking back door.
- A content-security layer strips scripts, event handlers, `<object>`, `<iframe>`, and forms from received HTML via `ammonia` before rendering.
- The on-disk store can be optionally encrypted with a user passphrase using Turso's built-in encryption at rest. Default is filesystem-level trust; encryption is opt-in in v1.
- TLS is required for all server connections. STARTTLS downgrade is never permitted; an account that advertises STARTTLS but fails to negotiate it is refused, not fallen back to plaintext. Implicit TLS (IMAPS 993, SMTPS 465, JMAP over HTTPS) is preferred.
- No analytics, no crash reporting, no remote config by default. If crash reporting is ever added it must be opt-in, local-first, and clearly disclosed.

## 7. Build, Distribution, and Platforms

### 7.1 Supported Targets

- **macOS:** universal2 binary (x86_64 + aarch64), signed and notarized, distributed as `.dmg`.
- **Windows:** x86_64, signed MSI and portable ZIP. ARM64 (Snapdragon X-class laptops, Apple Silicon Macs via Parallels) is a plausible post-v1 addition but deferred — see §12.
- **Linux:** x86_64 AppImage, Flatpak, and `.deb`/`.rpm`. Consider a Flathub submission early.

### 7.2 CI/CD

- GitHub Actions with matrix builds: macOS (universal2), Windows x86_64, and Ubuntu x86_64.
- Reproducible builds where feasible; pin Rust toolchain in `rust-toolchain.toml` with target triples listed (`x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`, `x86_64-unknown-linux-gnu`).
- Release automation: tag → build all platforms → sign → upload to GitHub Releases and the Tauri updater endpoint.
- A staged rollout channel (`stable`, `beta`, `nightly`).

### 7.3 Updates

Tauri's built-in updater, signed with a key held by the release maintainers. Users can disable auto-update.

## 8. Project Structure (Cargo Workspace)

```
/
├── Cargo.toml                  # workspace root
├── crates/
│   ├── core/                   # domain types, traits, no I/O
│   ├── storage/                # Turso (with rusqlite-compatible fallback) + blob store + migrations
│   ├── imap-client/            # IMAP adapter wrapping async-imap
│   ├── smtp-client/            # SMTP adapter wrapping lettre
│   ├── jmap-client/            # JMAP adapter
│   ├── mime/                   # parse + build helpers
│   ├── sync/                   # sync engine, outbox, reconciliation
│   ├── search/                 # tantivy indexing + query
│   ├── auth/                   # OAuth2 flows, keyring integration
│   └── ipc/                    # serde types shared with the UI
├── apps/
│   └── desktop/                # Tauri app (src-tauri + ui/)
│       ├── src-tauri/          # Rust shell, tauri commands
│       └── ui/                 # Dioxus UI (Rust, compiled to WASM)
├── docs/
├── xtask/                      # build helpers, release scripts
├── LICENSE                     # Apache License 2.0 (full text)
├── NOTICE                      # Apache 2.0 attribution file; list of bundled third-party notices
└── README.md                   # project overview, install, contributing pointer
```

Keeping protocol adapters and the sync engine in separate crates means a future mobile app or headless CLI can reuse them.

## 9. Extensibility (Post-v1)

A plugin API should be designed but not shipped in v1. Sketch:

- Plugins run in WASM (wasmtime) with a capability-based host API: read message metadata, add UI actions, transform outgoing mail, register rules.
- No raw network or filesystem access; all I/O goes through host-provided capabilities the user approves.
- Plugins are distributed as signed `.wasm` bundles; the registry can be a plain Git repo to start.

## 10. Licensing and Governance

### 10.1 License Decisions

- **License.** **Apache License 2.0** (unchanged since January 2004; there is no newer version). Chosen for maximum permissiveness: anyone can use, modify, redistribute, sublicense, and build commercial products from this code, including closed-source derivatives. The only obligations are preserving the license text and notices, and accepting the standard patent-grant / trademark disclaimers. The project authors do not assert ownership over downstream use.
- **CLA vs DCO.** DCO (signed-off-by) is the default — lowers contributor friction and doesn't require assigning rights to a single entity. Pairs cleanly with Apache 2.0: contributions come in under the same license as the project.
- **Code of Conduct.** Contributor Covenant 2.1 is the standard choice.
- **Trademarks.** The project does not assert trademark rights in the project name or logo and will not defend against trademark claims. If any third party objects to our use of a name, the response is to rename — not litigate, not negotiate, not counter-claim. This is the cheapest, lowest-drama posture for a project that genuinely doesn't want to own anything it doesn't have to. Apache 2.0's trademark clause is compatible with this: the license grant covers copyright and patents but explicitly does *not* grant trademark rights, so downstream users already have to respect trademarks separately regardless of our stance.

### 10.2 Initial License Setup

Concrete steps to execute before the first public commit. Skipping or rushing these creates legal ambiguity that's expensive to fix retroactively:

1. **Drop the full Apache 2.0 text into `LICENSE`** at the repo root. Copy verbatim from https://apache.org/licenses/LICENSE-2.0.txt — do not paraphrase, reformat, or "clean up" the text. The exact wording is what's enforceable. Replace the `[yyyy]` and `[name of copyright owner]` placeholders in the appendix with the project's copyright line (e.g. `Copyright 2026 <Project Name> Contributors`).
2. **Create a `NOTICE` file** at the repo root. Apache 2.0 Section 4(d) requires distributing a NOTICE file if the work contains one. List attribution for any bundled third-party code — at minimum the major Rust crates we vendor or redistribute (Turso, Servo, adblock-rust, ammonia, async-imap, jmap-client, lettre, mail-parser, tantivy, and anything else whose license requires attribution). Each entry is a one-line acknowledgment with a link to the upstream LICENSE file.
<!-- REUSE-IgnoreStart -->
3. **Add SPDX headers to every source file.** The top of every `.rs`, `.toml`, and build-script file gets a single comment line: `// SPDX-License-Identifier: Apache-2.0` (or `# SPDX-License-Identifier: Apache-2.0` for TOML/shell). This is the machine-readable standard (ISO/IEC 5962) for per-file license marking. It's what tools like `cargo-deny`, `reuse`, and corporate compliance scanners read to verify license boundaries.
<!-- REUSE-IgnoreEnd -->
4. **Add a `CONTRIBUTING.md`** covering: how to submit PRs, the required DCO sign-off (`git commit -s` adds `Signed-off-by:` trailers; the project's GitHub Actions should reject PRs missing this), a link to the CoC, and a note that all contributions are implicitly licensed under Apache 2.0 via the DCO.
5. **Add `CODE_OF_CONDUCT.md`** with Contributor Covenant 2.1, copied verbatim from https://contributor-covenant.org/version/2/1/code_of_conduct/, with a contact address filled in (can be an alias that forwards to the maintainers).
6. **Configure `cargo-deny` in CI** with a policy that: (a) fails the build if any dependency is under GPL, AGPL, SSPL, or any non-OSI license; (b) warns on copyleft licenses (MPL, LGPL) that require special handling if we ever vendor them; (c) verifies every `Cargo.toml` has a `license` field matching `Apache-2.0`. A minimal `deny.toml` belongs in the workspace root alongside `Cargo.toml`.
7. **Configure REUSE compliance in CI.** Run `reuse lint` on every PR to verify SPDX headers are present on all source files and that `LICENSES/Apache-2.0.txt` exists. This catches contributors who add new files without the header.
8. **Add a short "License" section to `README.md`** pointing at `LICENSE`, naming Apache 2.0, and noting that contributions require DCO sign-off. Three sentences is enough.
9. **Set the `license` field in every `Cargo.toml`** (workspace root and every crate) to `"Apache-2.0"`. Crates published to crates.io need this; it also surfaces the license in `cargo tree` and in tooling like deps.rs.
10. **Add a short trademark disclaimer to `README.md`** (one paragraph). Plain-English version: "The project name and logo are not registered trademarks and the project does not assert any. If you're an existing trademark holder and our name conflicts with yours, open an issue and we will rename — no letters or lawyers required." This pre-commits to the rename-if-challenged posture from §10.1 and defuses any future dispute at zero cost.

## 11. Roadmap

**Phase 0 — Foundations (weeks 1–6)**
Workspace scaffolding, Tauri shell, Turso storage layer with migrations (behind a driver-abstraction trait so an `rusqlite` fallback remains possible), account model, keychain integration, `MailBackend` trait designed against both protocols concurrently. OAuth2 with PKCE flows built and tested for both Gmail and Fastmail. Basic "list one folder, fetch headers" working against each, exercising the trait from both sides. **Servo composition spike:** embed a Servo `WebView` as a native child surface in the Tauri window on all three platforms (macOS NSView, Windows HWND, Linux GTK), render a fixed corpus of real-world emails, validate link-click interception and teardown. This is the critical de-risking work for the Phase 1 read path. Platform-specific glue may need to be written or contributed upstream; that effort is expected and budgeted per the Rust-native principle.

**Phase 1 — Read Path, Gmail + Fastmail (weeks 7–15)**
Full IMAP sync against Gmail with CONDSTORE/QRESYNC/IDLE, and full JMAP sync against Fastmail with `Email/changes` and EventSource push, both behind the `MailBackend` trait. MIME parsing, HTML rendering via the Servo `EmailRenderer` with remote content blocking, folder/mailbox and label navigation, threading, unified inbox across both backend types, notifications. The trait gets stress-tested because the two protocols disagree on almost everything (UIDs vs opaque IDs, flags vs keywords, folders vs mailboxes, pull vs push models).

**Phase 2 — Write Path, Gmail + Fastmail (weeks 16–20)**
Compose window, SMTP send to Gmail with XOAUTH2, JMAP `EmailSubmission/set` to Fastmail, drafts synced via each backend's native mechanism, attachments, outbox with retries, signatures.

**Phase 3 — Polish (weeks 21–26)**
Full-text search with Tantivy (single index across both backends), keyboard shortcuts, preferences UI, themes, onboarding flow with provider selection (Gmail / Fastmail), rules and filters (client-side).

**Phase 4 — 0.1 Release, Gmail + Fastmail (weeks 27–30)**
Cross-platform signing and packaging, auto-updater, website, docs. Public release supporting Gmail and Fastmail as two first-class providers.

**Phase 5 — Microsoft 365 (weeks 31–34)**
Second IMAP+OAuth2 provider. If the abstraction held up in Phase 1 this is primarily a new provider profile plus OAuth2 endpoint changes — a direct test of how reusable the Gmail IMAP code actually is.

**Phase 6 — Self-hosted via custom OAuth2 (weeks 35–38)**
UI for adding arbitrary OAuth2 providers (authorization URL, token URL, scopes, IMAP/JMAP server config). Targets Stalwart, Dovecot+OAUTHBEARER, Cyrus+OAUTHBEARER. Since both protocol backends already ship, this is mostly configuration UI plus discovery/validation.

**Phase 7+ — Beyond**
Sieve filters, PGP/MIME and S/MIME, snooze/send-later, CardDAV contacts, plugin API.

## 12. Open Questions

Resolved decisions are documented in `PHASE_0.md`, `TRAITS.md`, and `COMMANDS.md` and no longer appear here. What follows are the questions that still need an explicit call.

### Genuinely Open

- **Self-hosters on password-only IMAP** (default Dovecot without the OAuth2 module) are currently locked out per §1 and §2. Do we ship a documented, clearly-labelled "I understand the risks" escape hatch for homelab users, or hold firm? Not blocking until Phase 6, so safe to defer, but a decision before then prevents Phase 6 scope creep.
- **Turso upstream-patch policy** — how many days does an upstream fix stall before we vendor a patch? Suggested N = 14. Needs a final call before Phase 0 Week 2.
- **Servo compatibility corpus ownership** — who maintains the test-email corpus in `tests/fixtures/emails/`, how often is it refreshed, what's the process for adding new senders? Not blocking until Phase 3 polish, but worth a designated owner by end of Phase 1.
- **Trademark renaming threshold** — §10 commits to renaming if challenged. If the project gets popular, the cost of renaming rises sharply. At some threshold (say, 10,000 users) it might be worth revisiting the no-assertion stance. Future decision, not current.
- **Windows ARM64 as a post-v1 target** — deferred from v1. Shipping native ARM avoids x86-on-ARM emulation's perf/battery cost but carries risk from Servo's ARM64 Windows CI maturity. Worth revisiting after the 0.1 release.

### Resolved in Companion Docs

The following were previously open questions and are now resolved. See the linked docs for details.

- IMAP/JMAP protocol crate choice → `async-imap` and `jmap-client` directly, not Stalwart's crates (`TRAITS.md`, Week 4 of `PHASE_0.md`).
- Threading algorithm → References-chain + subject-normalization hybrid, not JWZ. Lives in `crates/sync`, not the `MailBackend` trait (`TRAITS.md` design notes).
- Body caching strategy → fetch on open, per-account LRU cache, default 1 GB, configurable via `Settings.body_cache_size_mb` (`COMMANDS.md` Settings).
- Headless CLI → yes, `mailcli` ships from Phase 0 as a forcing function for crate boundaries (`PHASE_0.md`).
- Filter-list policy → bundle EasyList + EasyPrivacy + uBO unbreak snapshot with the app, fetch weekly from easylist.to, silently fall back to last-known-good on fetch failure, user-visible in Settings (`COMMANDS.md` Settings → `adblock_filter_lists`).
- Servo composition specifics → answered by the Week 6 spike in `PHASE_0.md`; `EmailRenderer` trait in `TRAITS.md` is shaped to accept whatever the spike concludes.
