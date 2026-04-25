<!--
SPDX-FileCopyrightText: 2026 Capytain Contributors
SPDX-License-Identifier: Apache-2.0
-->

# Fastmail Engagement Log

Capytain's JMAP backend (`capytain-jmap-client`) talks to Fastmail's JMAP service via OAuth2 + bearer token. This document tracks the OAuth client setup runbook, observed protocol quirks, and the canonical session URL pin.

## Session URL

- **JMAP session URL:** `https://api.fastmail.com/.well-known/jmap`
- **Hardcoded in:** `apps/desktop/src-tauri/src/backend_factory.rs::open` and `apps/mailcli/src/main.rs::open_backend`. Both branches on the `"fastmail"` provider slug.

Fastmail also publishes the URL via the JMAP discovery `/.well-known/jmap` redirect (the `Location` header points to a versioned endpoint), so `Client::connect("https://api.fastmail.com/.well-known/jmap")` follows the redirect transparently — no manual bookkeeping needed.

## OAuth client setup (one-time)

Fastmail's OAuth2 is PKCE-only for native clients (no client secret). Steps for the maintainer:

1. Sign in to <https://www.fastmail.com> and go to **Settings → Privacy & Security → Connected apps & API tokens → Manage OAuth clients**.
2. Click **New OAuth Client**, set:
   - **Name:** `Capytain (dev)` (or similar)
   - **Redirect URI:** `http://127.0.0.1:0/callback` (port 0 = "any free port"; the loopback flow's listener picks one at runtime)
   - **Scopes:** at minimum `urn:ietf:params:jmap:core` and `urn:ietf:params:jmap:mail`. Add `urn:ietf:params:jmap:submission` for SMTP. The `protocol-imap` and `protocol-smtp` scopes (already in `crates/auth/src/providers/fastmail.rs`) cover the legacy IMAP/SMTP fallback.
   - **PKCE:** required (no secret).
3. Copy the **Client ID** Fastmail issues. Drop it into the workspace-root `.env`:

   ```sh
   echo "CAPYTAIN_FASTMAIL_CLIENT_ID=<paste-here>" >> .env
   echo "CAPYTAIN_FASTMAIL_CLIENT_SECRET=" >> .env  # PKCE-only — leave empty
   ```

4. `cargo build` — the `crates/auth/build.rs` script picks the values up via `dotenvy` and bakes them into the binary via `rustc-env`.
5. Add the account: `cargo run -p mailcli -- auth add fastmail <your@fm.address>`. The browser opens, you grant consent, the loopback flow captures the code, and a refresh token lands in your OS keychain (Secret Service on Linux, Keychain on macOS, Credential Manager on Windows).

Reference: <https://www.fastmail.com/dev/oauth/>.

## Smoke test runbook

Once the OAuth client is registered and the account is added:

```sh
cargo run -p mailcli -- sync <your@fm.address>
```

Expected output (per-folder counts plus an aggregate):

```
  INBOX: 50 new, 0 updated, 0 flag deltas, 0 removed, 50 bodies (0 failed)
  Drafts: 3 new, 0 updated, 0 flag deltas, 0 removed, 3 bodies (0 failed)
  ...
Total: ... in <ms> ms
```

Then open the desktop:

```sh
cargo run -p capytain-desktop
```

Tail the log for `JMAP EventSource watcher started` once per JMAP account. Send yourself a test message from another client → desktop log should show `JMAP push state change` within a few seconds, followed by `live sync_account folder` lines and a `sync_event` Tauri emit; the UI message-list pane refetches.

## Known quirks observed against Fastmail

### Email/changes returns full IDs, never prefixed

Fastmail's `Email/changes` response uses raw email IDs (e.g. `M01234`) for both `created` and `updated`. We round-trip those through the `MessageId` wrapper without prefixing. This contrasts with the IMAP adapter which encodes UIDVALIDITY + UID + folder into a `imap|<uv>|<uid>|<folder>` tuple via `MessageRef::encode`; for JMAP the server-side ID *is* the canonical id and we don't synthesize one.

### EventSource ping cadence

Per `crates/jmap-client/src/push.rs`, we set `ping=60` (seconds) when opening EventSource. Fastmail honors that — the server sends a `:` comment-line keepalive every 60s so a dropped TCP connection surfaces inside ~1min. Setting `ping=0` disables it, which makes a wedged connection invisible until the OS-level TCP keepalive eventually fires (10+ minutes).

### Mailbox/query unsorted

Fastmail's `Mailbox/query` returns ids in an unspecified order. `list_folders` does N+1 round-trips (one query, then one `Mailbox/get` per id). At Fastmail's mailbox count (<100) this is fine. The JMAP spec allows servers to bundle results into one `Mailbox/get(ids: …)` call — moving to that batched form is a Phase 1 polish item if the round-trips ever show up in a profile.

## Pinned versions

- **`jmap-client` crate:** v0.4.1 (workspace `[dependencies]`, semver-compatible within 0.4.x).
- **Stalwart-published source:** <https://github.com/stalwartlabs/jmap-client>. We don't currently fork or patch.

## When to revisit

- If Fastmail starts returning HTTP 429 on EventSource reconnect storms, add jitter to the watcher's backoff in `apps/desktop/src-tauri/src/jmap_push.rs`.
- If `Email/changes` ever returns a `cannotCalculateChanges` error, the engine treats it as a hard error and exits the cycle — we'd want to add a fallback that clears `sync_state` and re-runs an `Email/query`-backed initial fetch.
