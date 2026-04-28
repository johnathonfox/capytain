# QSL — account setup workflow spec

**Status:** draft v0
**Target:** QSL 0.0.2 (pre-MCP)
**Audience:** Claude Code (implementer), Johnathon (reviewer)

## Goal

Replace the current implicit-account-via-dev-tooling assumption with a real first-run experience. Make QSL launchable on a clean machine, walk the user through adding an account via OAuth, and land them in the normal three-pane UI. Handle the auth-error and re-auth cases that occur during normal daily-driver use.

This is a pre-MCP unblocker: the MCP server needs at least one account to be useful, and the only way to add accounts today is through dev tooling. Shipping this also makes "reinstall on a fresh machine" a 5-minute operation instead of a multi-hour debugging session.

## Non-goals (v0)

- Generic IMAP setup (custom host/port/username/password/TLS). Gmail and Fastmail OAuth only.
- Settings UI for managing accounts post-setup. Dev tooling stays the path for delete/edit until there's pressure to build a settings screen.
- Per-account configuration (signatures, display name overrides, sync preferences, default folder behavior). Account is identified by its email address; everything else uses sensible defaults.
- Account reordering or default-account UI. First account added is the default; multi-account ordering is alphabetical until otherwise specified.
- Onboarding tour, value-prop marketing, or anything else that would make sense for an app with users that aren't Johnathon.
- Migration tools (importing from Thunderbird, Mailspring, etc.).

## What this spec covers

1. Empty-state detection on launch
2. Setup screen UI
3. OAuth flow integration (already-built, just needs UI wrapping)
4. Success / failure / cancel / timeout handling
5. Re-auth flow when an existing account's refresh token fails
6. Multi-account add (same flow, invoked from somewhere — TBD: see "Open" section)

## Architecture

The existing OAuth + TokenVault + accounts-table machinery does the work. This spec adds:

- A first-run gate at app boot that checks for accounts and routes to setup if zero.
- A SetupScreen component (Dioxus) with provider buttons and an in-flight state.
- An auth-status surface in the existing status bar / sync_event stream for re-auth prompts.
- A single Tauri command that wraps the OAuth flow with cancellation support.

No new crates. No new DB tables. Probably one new migration if account state needs an `auth_status` column (see DB section).

## Empty-state detection

On app boot, after DB migrations and before the sync engine starts:

```rust
let account_count = db.count_accounts().await?;
match account_count {
    0 => AppState::NeedsSetup,
    _ => AppState::Ready,
}
```

The UI reads this state and either mounts SetupScreen or the normal three-pane shell.

**No first-run flag.** "Zero accounts" is the source of truth. This correctly handles three cases with one code path:
- Genuine first launch (zero accounts ever)
- Reinstall (DB recreated, zero accounts)
- User deleted their last account (zero accounts again)

The sync engine must not start when `AppState::NeedsSetup`. The `ui_ready` gate already exists; extend it (or add a sibling gate) to also wait on "at least one account exists" before kicking off CONNECT/LIST traffic.

## Setup screen UI

Single centered card on an empty window. No header, no nav, no sidebar — the rest of the UI is hidden until setup completes.

**Layout:**

```
┌──────────────────────────────────────────────┐
│                                              │
│                                              │
│              ┌─────────────────────┐         │
│              │                     │         │
│              │       QSL           │         │
│              │                     │         │
│              │  Add an email       │         │
│              │  account to start.  │         │
│              │                     │         │
│              │  ┌───────────────┐  │         │
│              │  │  Add Gmail    │  │         │
│              │  └───────────────┘  │         │
│              │  ┌───────────────┐  │         │
│              │  │  Add Fastmail │  │         │
│              │  └───────────────┘  │         │
│              │                     │         │
│              │  Other (coming      │         │
│              │  soon)              │         │
│              │                     │         │
│              └─────────────────────┘         │
│                                              │
│                                              │
└──────────────────────────────────────────────┘
```

**Content:**
- App name "QSL" as a small wordmark — leave the existing logo treatment if there is one, otherwise plain text in the same font/weight as the sidebar account header
- One-sentence prompt: "Add an email account to get started."
- Two primary buttons: "Add Gmail" and "Add Fastmail"
- Disabled tertiary link: "Other (coming soon)" — visible so the user knows generic IMAP is planned, not abandoned
- No "Skip" option. There is no useful state to skip into.

**Visual style:** Match existing dark theme. Card on `var(--background-secondary)` or equivalent, no border or a subtle 0.5px one, padded generously. Buttons use the same component as the existing Reply/Archive buttons in the reader pane for consistency.

**Window chrome:** Same as normal — title bar, close/minimize/maximize. Don't make this a separate window or modal; it's a full-window state of the main window.

## OAuth flow states

The setup screen has four UI states. Treat them as a state machine, not separate screens — same layout, just swap the card content.

### State: Idle (default)
Two provider buttons enabled. Description prompt visible.

### State: Awaiting browser
User clicked a provider button. OAuth flow has started, browser tab opened, loopback listener is waiting for the redirect.

UI changes:
- Both provider buttons disabled (greyed)
- Description text replaced with: "Waiting for [Gmail/Fastmail] authentication in your browser…"
- "Cancel" button appears below
- Optional small spinner — but only if it doesn't add complexity; static text is fine

The Cancel button must:
- Stop the loopback listener
- Cancel any pending Tokio tasks for this OAuth flow
- Return UI to Idle state
- Not leave a zombie listener on the loopback port

### State: Completing
Loopback redirect received, exchanging authorization code for tokens, persisting to keychain, inserting account row, kicking off initial sync.

UI changes:
- Description: "Setting up your account…"
- Cancel button removed (this state is fast and shouldn't be cancelled mid-write)
- Should typically last 1–3 seconds. If it lasts longer than 10s, surface a "this is taking longer than expected" sub-message (don't auto-fail; sometimes Gmail's token endpoint is just slow)

### State: Error
Something went wrong. Stay on the setup screen with both providers re-enabled, but show an error message above the buttons.

Error categories to handle distinctly:
- **Browser closed without completing.** "Authentication was cancelled. Try again?" — friendly, non-alarming.
- **OAuth declined by user (`error=access_denied`).** "Permission was declined. To use QSL with [provider], you'll need to allow access."
- **Token exchange failed.** "Couldn't complete sign-in. [retry]" — log the underlying error to the app log, don't show the raw OAuth error to the user.
- **Network failure.** "Couldn't reach [provider]. Check your connection and try again."
- **Loopback listener timeout (60s without a callback).** "Authentication timed out. Try again?"
- **Account already exists.** If the user adds an account that's already in the DB (because they're hitting the flow from somewhere unexpected), surface "[email] is already added." This shouldn't happen from the empty-state setup flow but could happen from a future settings UI.
- **Internal error.** Catch-all. "Something went wrong. Check the logs and try again."

Error display: small inline banner above the buttons, dismissible by clicking either button (which starts a new attempt) or an explicit close X.

### Transitions

```
Idle ──[click provider]──> AwaitingBrowser
AwaitingBrowser ──[redirect received]──> Completing
AwaitingBrowser ──[user clicks cancel]──> Idle
AwaitingBrowser ──[60s timeout]──> Error(timeout)
AwaitingBrowser ──[browser error]──> Error(specific)
Completing ──[success]──> [exit setup, mount main UI]
Completing ──[failure]──> Error(specific)
Error ──[click provider]──> AwaitingBrowser
```

## Tauri command surface

Add a single new command (or extend the existing OAuth command, depending on how it's currently shaped):

```rust
#[tauri::command]
async fn account_add(
    provider: Provider,           // enum Gmail | Fastmail
    cancellation_token: CancellationToken,
) -> Result<AccountId, AccountAddError>;
```

The cancellation token is wired to the UI Cancel button. When triggered, the command must:
1. Stop the loopback listener
2. Drop any pending HTTP requests
3. Return `AccountAddError::Cancelled`

The command returns the new `AccountId` on success; the UI uses this to confirm and transition out of setup.

Errors are typed (`AccountAddError` enum) so the UI can branch cleanly to the right error message:

```rust
enum AccountAddError {
    Cancelled,
    UserDeclined,
    NetworkError(String),
    TokenExchangeFailed(String),
    Timeout,
    AlreadyExists(EmailAddress),
    Internal(String),
}
```

## Re-auth flow

Separate from initial setup but uses the same OAuth machinery.

**Trigger:** Sync engine encounters a token failure that refresh can't recover from (refresh token revoked, account password changed, etc.). Today this presumably surfaces as a generic sync error.

**Detection:** The IMAP/JMAP backend distinguishes auth errors from other sync errors. When detected, the account row gets `auth_status = "needs_reauth"` (see DB section), the sync engine skips that account on subsequent runs, and a SyncEvent fires to the UI.

**UI surface:** In the sidebar, the account header shows an inline warning state — small icon, text "Sign in needed," clickable. Don't use a banner across the top of the app; that's too disruptive when a single account out of multiple has the issue.

Click flow: takes the user through the same OAuth flow as initial setup, but with the email pre-known. On success: clear `auth_status`, resume sync. On cancel/error: stay in `needs_reauth`, user can retry whenever.

**Important:** Re-auth must use the same provider that was originally used and re-authenticate the same email address. If the user authenticates with a different account, surface an error rather than silently linking — that's a footgun.

## DB changes

Probably one migration to add `auth_status` to the accounts table:

```sql
ALTER TABLE accounts ADD COLUMN auth_status TEXT NOT NULL DEFAULT 'ok';
-- Values: 'ok' | 'needs_reauth'
-- Future: 'syncing_disabled' | 'expired' | etc.
```

Skip if there's already a column or status mechanism that fits. Don't add a parallel one.

## Logging and observability

Every state transition logs at `info`. Errors log at `warn` with the full underlying cause (which is *not* shown to the user). The OAuth provider's raw error response is sensitive-adjacent — log it but redact tokens (the provider sometimes echoes back the auth code in error responses).

Loopback listener bind/unbind events log at `debug`. If the bind fails (port in use), log at `error` and surface to the UI as a "couldn't start the auth listener" error — this is rare but happens (another QSL instance running, or in dev when the previous instance didn't clean up).

## Testing

Three layers, none of them automated for v0:

1. **Manual: clean install, add Gmail, verify mail loads.** The basic happy path. Run on a fresh `~/.local/share/app.qsl.desktop/` directory.
2. **Manual: clean install, add Fastmail, verify mail loads.** Validates the otherwise-untested JMAP setup path against a real account. (This is the "Fastmail OAuth + JMAP smoke-test" deferral from the project state — this spec doesn't deliver it on its own, but creates the surface where you'll do it.)
3. **Manual: each error case.** Cancel mid-flow, decline in browser, kill network during token exchange, time out by leaving the browser tab idle, attempt to add an already-added account.

Test infrastructure (mock OAuth server, fault injection) is deferred. The flow is short enough that manual testing through the listed cases is reasonable.

## Implementation sequence

Each step independently mergeable.

1. **DB migration** for `auth_status` column. Trivial; do first because it's the only thing that requires schema change.
2. **`account_add` Tauri command** with cancellation. Wraps existing OAuth machinery, adds typed errors, adds cancellation. No UI yet — verify via mailcli or manual `tauri invoke`.
3. **Empty-state detection** at boot. Route to a placeholder SetupScreen component (just text "Setup goes here, no accounts found") until step 4. Verify by deleting all accounts and relaunching.
4. **SetupScreen UI** — Idle and AwaitingBrowser states only. Verify happy path: zero accounts → setup screen → click provider → browser opens → auth completes → main UI mounts.
5. **Cancel and timeout handling.** Add Cancel button, wire 60s timeout. Verify by clicking Cancel mid-flow and by waiting out the timeout.
6. **Error states.** Wire all error categories from the OAuth flow to the right UI message. Verify by killing network, declining in browser, etc.
7. **Re-auth flow.** Detect auth errors in sync engine, set `auth_status`, surface in sidebar account header, route through same OAuth flow. Verify by manually revoking the refresh token in Google's account settings and confirming the UI prompts re-auth.

Stop here. Account-add-from-settings-UI and generic IMAP setup are out of scope.

## Open

- **Where does "add another account" live post-setup?** Settings UI is out of scope, but the user (Johnathon) needs *some* way to add the second account when the time comes. Options: (a) dev tooling only for now, document in README; (b) a small "+" button somewhere in the sidebar that triggers the same flow. Probably (b) since the work is trivial once the flow exists, but flagging in case Johnathon prefers (a) for v0 to keep the surface small.
- **Should the loopback port be fixed or random?** Random is more robust (no conflicts), but Google's OAuth console requires registered redirect URIs, so it has to be fixed for Gmail. Confirm what the existing flow uses and stick with it.
- **What happens if the user has multiple browsers and the OAuth opens in the wrong one?** Probably nothing actionable — `webbrowser::open` uses the system default. Document the limitation if it bites in practice.

## Prompt for Claude Code

> Implement the QSL account setup workflow per `docs/QSL_ACCOUNT_SETUP_SPEC.md`. Read the spec end-to-end before writing any code. Existing OAuth, TokenVault, and accounts-table machinery is already in place — this spec adds the UI wrapping, cancellation, error handling, and first-run gate around them. Do not change the underlying OAuth flow itself unless required by cancellation support.
>
> Implement steps 1–4 from the implementation sequence (DB migration, account_add command, empty-state detection, SetupScreen with Idle and AwaitingBrowser states). Stop after step 4 and let me verify the happy path before continuing. Do not start steps 5–7 in the same session.
>
> If the existing OAuth code does not cleanly support cancellation, flag it before refactoring — that's a larger change than this spec implies and warrants discussion before implementation.
