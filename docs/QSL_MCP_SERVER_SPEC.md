# QSL MCP server — design spec

**Status:** draft v0
**Target:** QSL 0.0.2 or 0.0.3
**Audience:** Claude Code (implementer), Johnathon (reviewer)

## Goal

Expose QSL's mail data as an MCP server so external agents (Claude Desktop, Claude Code, and other MCP clients) can read and act on the user's mail through QSL rather than connecting to providers directly. QSL owns the IMAP/SMTP/Gmail-API code, the OAuth tokens, the local cache, and the policy layer; the MCP server is a façade over QSL's existing internal mail API.

This spec covers v0 only: read-only tools, stdio transport. Write tools and streaming are explicitly out of scope here and will be specced separately once v0 is in use. Multi-account is in scope at the data-model level (the MCP surface is designed for it) but not at the UI level — QSL today is configured with a single account, and v0 of the MCP server doesn't need to wait for the multi-account UI to ship before exposing an account-aware API.

## Non-goals (v0)

- Write tools (`send_message`, `archive`, `mark_read`, `modify_labels`). Tracked separately. Validator layer must land before any of these ship.
- Network transports (HTTP/SSE). Stdio only for v0 — local consumers on the same machine.
- Authentication / consumer scoping. Local stdio means we trust the host.
- Push / streaming notifications. Polling-based reads only.
- Attachment binary content over MCP. Metadata only; binary fetch is v0.1+.

## Architecture

```
┌──────────────────────┐         ┌─────────────────────────────────┐
│ Claude Desktop /     │  stdio  │ QSL                             │
│ Claude Code /        │◄───────►│ ┌─────────────────────────────┐ │
│ other MCP clients    │   MCP   │ │ MCP server (rmcp)           │ │
└──────────────────────┘         │ └──────────────┬──────────────┘ │
                                 │                │                │
                                 │   trait MailStore (existing)    │
                                 │                │                │
                                 │ ┌──────────────▼──────────────┐ │
                                 │ │ SQLite cache + IMAP/Gmail   │ │
                                 │ └─────────────────────────────┘ │
                                 │                                 │
                                 │ ┌─────────────────────────────┐ │
                                 │ │ Tauri UI (also uses         │ │
                                 │ │ MailStore via commands)     │ │
                                 │ └─────────────────────────────┘ │
                                 └─────────────────────────────────┘
```

The MCP server and the Tauri UI are both consumers of the same internal `MailStore` trait. Neither calls IMAP directly. This is the key invariant: any feature exposed via MCP must be expressible as a `MailStore` method, and any `MailStore` method should be a candidate for MCP exposure.

## Process model

The MCP server runs as a **separate binary** in the same workspace, spawned by the user (not by the QSL Tauri app). Reasons:

- MCP consumers like Claude Desktop expect to spawn a stdio subprocess themselves via their own config. Having QSL spawn it would require a second IPC layer.
- The MCP binary and the QSL UI need to share the same SQLite cache and OAuth tokens. We solve this by putting both behind a shared data directory (XDG-compliant on Linux, `~/.local/share/qsl/` or similar) that both processes can read/write.
- If the UI is closed, MCP consumers can still read mail. If the UI is open, both are reading from the same cache.

**Concurrency:** SQLite handles multi-process access via WAL mode. Enable WAL on database open (`PRAGMA journal_mode = WAL`). Writes from either process are serialized by SQLite. Sync (fetching new mail from the provider) should only happen in one process at a time — the QSL UI when it's running, otherwise the MCP server. Use a file lock or SQLite advisory lock to coordinate; if QSL UI is running, the MCP binary skips its own sync loop and only reads from cache.

**Crate layout (suggested):**

```
qsl/
├── Cargo.toml          # workspace
├── crates/
│   ├── qsl-core/       # MailStore trait, models, sync engine, IMAP/Gmail clients
│   ├── qsl-cache/      # SQLite schema, queries, migrations
│   ├── qsl-app/        # Tauri app (existing UI)
│   └── qsl-mcp/        # MCP server binary (new)
```

If the current code isn't crate-split this way, the spec assumes step 1 is the refactor: extract `MailStore` and the cache layer into their own crate that both `qsl-app` and `qsl-mcp` depend on. This refactor is half the work of this feature; the MCP wiring on top is small.

## Dependencies

All Rust-native, no Python/Node/etc. shellouts.

| Crate | Version (latest stable) | Purpose |
|---|---|---|
| `rmcp` | latest | Official Rust MCP SDK. Handles protocol, transport, serialization. |
| `tokio` | 1.x | Async runtime (already used by Tauri). |
| `serde` / `serde_json` | latest | Serialization for tool inputs/outputs. |
| `schemars` | latest | JSON schema generation for tool input types — `rmcp` integrates with this. |
| `rusqlite` or `sqlx` | latest | SQLite access. Pick whichever the existing cache uses; don't introduce a second. |
| `anyhow` / `thiserror` | latest | Error handling. `thiserror` for library errors, `anyhow` at binary boundary. |
| `tracing` / `tracing-subscriber` | latest | Structured logging. MCP server must log to stderr only (stdout is the protocol channel). |

Verify exact current versions when implementing — `rmcp` in particular is moving fast.

## Folders and labels

`list_folders` returns mailboxes (system folders like Inbox, Sent, Trash) and Gmail labels in the same flat list, distinguished by the `kind` field (`system | folder | label`). This is the right shape for agents and the right shape for the MCP API even if the UI eventually splits them into separate visual sections.

Reasons:

- **Agents reason about "places mail can live," not "mailboxes vs labels."** From an agent's perspective, "find emails in the Boomerang label" and "find emails in Sent" are the same operation. Forcing two separate tools (`list_mailboxes`, `list_labels`) makes the API harder to use without making it more correct.
- **Gmail's IMAP exposes labels as folders anyway.** This matches the underlying protocol shape. A Gmail label is a folder you can `SELECT` and `SEARCH` against via IMAP; the `kind` distinction is metadata for the UI.
- **The `kind` field is forward-compatible.** Future providers (Exchange categories, Fastmail labels) map cleanly into the same shape with `kind: label`.

For Gmail accounts specifically, suppress the `[Gmail]` and `[Gmail]/All Mail` system folders from the default `list_folders` output unless the agent passes `include_hidden: true`. They're noise for most agent workflows and they confuse the labels-vs-folders model. Map them internally to the canonical roles (`role: archive` for All Mail, etc.) so agents can find them via role rather than name.



QSL is single-account in the UI today but multi-account in the data model. The MCP server is built multi-account from day one: every tool that operates on mail accepts an `account_id` parameter, and `list_accounts` is exposed as a tool.

**Single-process, multiple accounts.** One `qsl-mcp` binary instance handles all accounts the user has configured. This is simpler than spawning one MCP server per account (which would force consumers to manage N stdio subprocesses) and matches how the cache and OAuth storage already work — they're keyed by account.

**`account_id` is required when ambiguous, optional when not.** Every tool that operates on mail accepts an optional `account_id`. If omitted and the user has exactly one account, the server uses that account. If omitted and the user has multiple accounts, the server returns an `account_required` error listing the available account IDs. This keeps single-account ergonomics simple while forcing explicit selection in the multi-account case.

**Account IDs are stable strings.** Use the email address as the account ID (e.g. `johnathon.fox@gmail.com`), not a UUID. Agents reason about this better, logs are readable, and the user knows what they're looking at. If two accounts ever share an address (unlikely but possible — e.g. same address across two providers), disambiguate with `address#provider`.

**Folder and message IDs are scoped to their account.** A `folder_id` like `INBOX` is meaningful only in the context of an account. The internal cache schema must already key these by account; the MCP layer just exposes that. Returning a message means returning `(account_id, message_id)`; agents pass both back when fetching the body.



All tools are **read-only**. Inputs are JSON; outputs are JSON. Schemas derived from Rust types via `schemars`.

### `list_accounts`

List all accounts configured in QSL.

**Input:** `{}` (no parameters)

**Output:**
```json
{
  "accounts": [
    {
      "id": "johnathon.fox@gmail.com",
      "address": "johnathon.fox@gmail.com",
      "display_name": "Johnathon Fox",
      "provider": "gmail",
      "last_sync": "2026-04-26T16:55:12Z",
      "sync_status": "ok"
    }
  ]
}
```

`provider` is one of `gmail | imap | exchange | other` (extend the enum as providers are added). `sync_status` is one of `ok | syncing | error | offline`.

### `list_folders`

List all folders (mailboxes + labels) for an account.

**Input:**
```json
{
  "account_id": "johnathon.fox@gmail.com",   // optional if user has exactly one account
  "include_hidden": false                     // optional, default false; set true to include [Gmail] etc.
}
```

**Output:**
```json
{
  "folders": [
    {
      "id": "INBOX",
      "display_name": "Inbox",
      "kind": "system",
      "role": "inbox",
      "unread_count": 6,
      "total_count": 84
    },
    {
      "id": "Label_42",
      "display_name": "Boomerang",
      "kind": "label",
      "role": null,
      "unread_count": 0,
      "total_count": 312
    }
  ]
}
```

`kind` is one of `system | folder | label`. `role` is one of `inbox | sent | drafts | trash | spam | archive | starred | important | null`.

### `search_messages`

Search messages and return a page of metadata (no bodies).

**Input:**
```json
{
  "account_id": "johnathon.fox@gmail.com",   // optional if user has exactly one account
  "query": "from:alpaca",
  "folder_id": "INBOX",         // optional, omit to search all folders in the account
  "limit": 50,                  // default 50, max 200
  "cursor": "opaque-string"     // optional, from previous response
}
```

**Output:**
```json
{
  "messages": [
    {
      "id": "msg_abc123",
      "account_id": "johnathon.fox@gmail.com",
      "thread_id": "thr_xyz789",
      "folder_id": "INBOX",
      "from": { "name": "Alpaca Markets", "address": "noreply@alpaca.markets" },
      "to": [{ "name": null, "address": "johnathon.fox@gmail.com" }],
      "subject": "Daily account summary",
      "snippet": "Your portfolio closed at $24,891.32 today, up 0.42%...",
      "date": "2026-04-26T16:42:00Z",
      "flags": ["unread"],
      "labels": ["Important"],
      "has_attachments": false
    }
  ],
  "next_cursor": "opaque-string-or-null"
}
```

**Query syntax:** Pass through to underlying provider where possible.
- For Gmail accounts, support full Gmail search syntax (`from:`, `to:`, `subject:`, `has:attachment`, `older_than:`, `newer_than:`, `label:`, etc.) by routing through `X-GM-RAW`.
- For non-Gmail IMAP, translate a subset to IMAP `SEARCH` criteria.
- For local-cache-only search, fall back to SQLite FTS5.
- The tool should accept the query string as-is and let the cache/provider layer figure out the routing. Document the supported syntax in the tool description so agents know what works.

**Cursor:** Opaque string. Implementation can be base64-encoded `(date, message_id)` tuple. Don't expose offset-based pagination — agents shouldn't need to reason about page numbers.

### `get_message`

Fetch a single message including body.

**Input:**
```json
{
  "account_id": "johnathon.fox@gmail.com",   // optional if user has exactly one account
  "id": "msg_abc123",
  "body_format": "text"   // "text" | "html" | "both", default "text"
}
```

**Output:**
```json
{
  "id": "msg_abc123",
  "account_id": "johnathon.fox@gmail.com",
  "thread_id": "thr_xyz789",
  "folder_id": "INBOX",
  "from": { "name": "Alpaca Markets", "address": "noreply@alpaca.markets" },
  "to": [...],
  "cc": [...],
  "bcc": [...],
  "reply_to": [...],
  "subject": "Daily account summary",
  "date": "2026-04-26T16:42:00Z",
  "flags": ["unread"],
  "labels": ["Important"],
  "headers": {
    "message-id": "<...>",
    "in-reply-to": "<...>",
    "references": "<...> <...>"
  },
  "body_text": "Hi Johnathon,\n\nHere's your daily...",
  "body_html": null,
  "attachments": [
    {
      "id": "att_1",
      "filename": "report.pdf",
      "mime_type": "application/pdf",
      "size_bytes": 124567,
      "content_id": null,
      "is_inline": false
    }
  ]
}
```

**Body handling for agents:** Default to `body_format: "text"`. HTML bodies are noisy and waste tokens. If the agent explicitly needs HTML (rendering preview, parsing structured content), they ask for it. `"both"` returns both fields populated.

**HTML sanitization:** Even when returning HTML, run it through a sanitizer (`ammonia` crate) to strip `<script>`, event handlers, and remote resources. Agents shouldn't be loading tracking pixels by side effect of reading mail.

### `get_thread`

Fetch all messages in a thread.

**Input:**
```json
{
  "account_id": "johnathon.fox@gmail.com",   // optional if user has exactly one account
  "id": "thr_xyz789",
  "body_format": "text"
}
```

**Output:**
```json
{
  "id": "thr_xyz789",
  "account_id": "johnathon.fox@gmail.com",
  "subject": "Daily account summary",
  "messages": [
    { /* same shape as get_message output */ },
    { /* ... */ }
  ]
}
```

Messages ordered chronologically. Each entry is a full message including body.

### `get_account_info`

Return account metadata for a specific account.

**Input:**
```json
{
  "account_id": "johnathon.fox@gmail.com"   // optional if user has exactly one account
}
```

**Output:**
```json
{
  "id": "johnathon.fox@gmail.com",
  "address": "johnathon.fox@gmail.com",
  "display_name": "Johnathon Fox",
  "provider": "gmail",
  "last_sync": "2026-04-26T16:55:12Z",
  "sync_status": "ok"
}
```

`sync_status` is one of `ok | syncing | error | offline`. If `error`, include an `error_message` field. For listing all accounts at once, use `list_accounts`.

## MCP server surface — v0 resources

MCP supports addressable resources alongside tools. Expose:

- `mail://accounts` — returns the account list
- `mail://account/{account_id}` — returns account info
- `mail://account/{account_id}/folders` — returns folder list for that account
- `mail://account/{account_id}/folder/{folder_id}` — returns folder metadata
- `mail://account/{account_id}/message/{message_id}` — returns full message
- `mail://account/{account_id}/thread/{thread_id}` — returns full thread

Resources are good for "I have an ID and want the thing" patterns. Tools are good for actions and queries. Agents handle resources better when reasoning about mail because they're addressable and cacheable on the client side. The account ID is part of every URI because mail IDs are only meaningful within their account.

## Tool descriptions (the part that matters for agent UX)

This is the most important part of the spec and the one that's easiest to get wrong. Agent behavior is driven heavily by tool descriptions. Each tool's description should:

1. State what it does in one sentence.
2. State when to use it ("Use this when you want to find specific messages by sender, subject, or content").
3. State when **not** to use it ("Don't use this to fetch a full message body — use `get_message` after finding the ID").
4. Document non-obvious parameter behavior (e.g. that `query` supports Gmail syntax for Gmail accounts).
5. Note any rate/cost considerations ("This may hit the provider; prefer `search_messages` over fetching every message").

Example for `search_messages`:

> Search the user's mail and return message metadata (no bodies). Use this to find messages matching a query, e.g. by sender, subject, date range, or label. For Gmail accounts, supports Gmail search syntax: `from:`, `to:`, `subject:`, `has:attachment`, `older_than:7d`, `newer_than:1d`, `label:`. Returns up to 200 messages per call with a cursor for pagination. Does not include message bodies — use `get_message` to fetch a body once you've identified the message you want. Prefer narrowing with `folder_id` when you know which folder to search; searching across all folders is slower.

Treat tool descriptions as documentation that an agent will read once and act on hundreds of times. They earn their length.

## Cache freshness

Tools return cached data immediately. They do not block on a sync round-trip to the provider. Three reasons:

- **Latency.** Agent calls should feel instant. A round-trip to Gmail's API on every `search_messages` call makes the MCP server unusable for interactive agent workflows.
- **Cost.** Provider rate limits are real. An agent doing a multi-step task ("find all Alpaca emails, summarize each") would burn through Gmail API quota fast if every call hit the network.
- **Offline correctness.** The cache works without network; agent flows that don't strictly need fresh data should keep working when the user is offline.

The cost is potential staleness. Mitigations:

- **`last_sync` is always exposed.** `list_accounts` and `get_account_info` include `last_sync` so agents can tell how fresh the data is. Tool descriptions instruct agents to check this when freshness matters ("if you need messages from the last few minutes, check `last_sync` first; if it's older than your tolerance, the user may need to open QSL or wait for the next sync").
- **Background sync runs continuously.** When QSL UI is running, it owns the sync loop. When only `qsl-mcp` is running (UI closed, agent connected), the MCP server runs the sync loop itself. Use the file/SQLite advisory lock described in the process model section to coordinate — whichever process holds the lock owns sync; the other one is read-only against the cache.
- **No `force_sync` tool in v0.** Adding it later if needed is cheap. Starting without it forces honest design of the freshness story; agents that need real-time mail probably need streaming, which is a separate v0.1 spec.



The MCP server reads its config from QSL's existing config dir. No separate config for v0.

For Claude Desktop integration, the user adds to their Claude Desktop MCP config:

```json
{
  "mcpServers": {
    "qsl": {
      "command": "/path/to/qsl-mcp",
      "args": []
    }
  }
}
```

QSL should provide a CLI helper or a UI button that prints the correct snippet for the current install path.

## Error handling

All tool errors return a structured MCP error. Categories:

- `not_found` — message/thread/folder ID doesn't exist or isn't accessible
- `rate_limited` — provider rate limit hit; agent should back off
- `provider_error` — IMAP/Gmail-API returned an error; include provider message
- `cache_miss_offline` — requested data not in cache and provider unreachable
- `invalid_input` — schema validation failed (rmcp handles most of this)
- `internal` — bug; include enough context to debug

Errors must not leak OAuth tokens, full headers, or raw IMAP responses.

## Logging

`tracing` for structured logs. **stderr only** — stdout is the MCP protocol channel and any non-protocol bytes corrupt the stream. Default level `info`; `RUST_LOG=qsl_mcp=debug` for development.

Log every tool call with: tool name, input params (with addresses redacted to domain only — log `*@gmail.com` not the full address), duration, result kind (ok / error category). This becomes the audit log for "what did agents do with my mail."

## Testing

Three layers:

1. **Unit tests on `MailStore`.** Existing coverage; no new requirements.
2. **MCP server integration tests.** Spin up the MCP server against a fake `MailStore` impl, send tool calls, assert outputs. `rmcp` should provide test utilities; if not, raw stdio transport works fine.
3. **End-to-end smoke test.** A small Rust binary that connects to a real running `qsl-mcp` (against a test Gmail account), calls each tool, asserts plausible results. Run manually before each release until it's worth automating.

No agent-in-the-loop testing in v0. The next-best signal is "does Claude Desktop give useful answers when you ask about your inbox," and that's a manual check.

## Security & privacy notes

- **Stdio transport only.** No network listener. If a future version adds HTTP/SSE, that's where auth becomes mandatory.
- **No telemetry.** Logs stay local.
- **Token storage is QSL's existing storage.** The MCP binary accesses tokens via the same path the UI does — usually libsecret on Linux. Don't reinvent.
- **Sanitize HTML bodies.** `ammonia` with a strict allowlist. Agents shouldn't trigger remote loads by reading mail.
- **Redact in logs.** Email addresses logged as `*@domain` only. Subjects truncated to 32 chars in logs. Bodies never logged.

## Implementation sequence

Suggested order, each step independently mergeable:

1. **Refactor.** Extract `MailStore` trait and cache into `qsl-core` and `qsl-cache` crates. UI consumes the trait. No behavior change. Verify by running QSL and confirming nothing broke. *Half the total work of this feature lives here.*

2. **`qsl-mcp` binary skeleton.** Empty MCP server, no tools, just connects via stdio and responds to `initialize`. Wire it up to Claude Desktop and confirm the server appears in Claude Desktop's UI.

3. **`get_account_info` + `list_folders`.** Two simplest tools. Confirms the trait → MCP plumbing works end-to-end. Test by asking Claude "what email account is connected and what folders exist."

4. **`search_messages`.** The workhorse tool. Wire query passthrough to Gmail search syntax. Test by asking Claude "find emails from Alpaca in the last week."

5. **`get_message` + `get_thread`.** With HTML sanitization. Test by asking Claude "summarize my last email from Tim."

6. **Resources.** Add the `mail://` URI scheme handlers, mostly thin wrappers over the tools.

7. **Polish.** Tool descriptions written carefully (see the section above; this matters more than people expect). Error categories. Logging redaction. CLI helper for printing Claude Desktop config snippet.

8. **End-to-end smoke test binary.** One-shot test runner against a real account.

Stop here for v0. Write tools come in a separate spec once the read-only surface has been used for a few weeks and the access patterns are clearer.

## Decisions log

Resolved during spec review:

- **Crate and binary name.** Both `qsl-mcp`.
- **Multi-account.** In scope at the API level from v0. Single MCP server process, multiple accounts. `account_id` is optional when there's only one account, required otherwise. UI multi-account is a separate milestone but doesn't gate the MCP work.
- **Cache freshness.** Tools return cached data immediately. `last_sync` is exposed so agents can reason about staleness. No `force_sync` tool in v0. See "Cache freshness" section.
- **Labels and folders.** Flattened into one list with a `kind` discriminator. `[Gmail]` system folders hidden by default. See "Folders and labels" section.

Still open (not blocking):

- **Sync loop ownership when both QSL UI and `qsl-mcp` are running.** Spec says use a SQLite advisory lock or file lock; whoever holds it runs sync. Implementation should pick one and document it. Not blocking v0 step 1 (refactor) or step 2 (skeleton) — surfaces in step 3+.


## Prompt for Claude Code

When implementing, the prompt to hand Claude Code (with this spec attached) should include:

> Implement the QSL MCP server per `docs/QSL_MCP_SERVER_SPEC.md`. Read the spec end-to-end before writing any code. Start with the refactor in step 1 (extracting `MailStore` and cache into separate crates) — do not skip this even if it seems like overhead. After the refactor, implement steps 2 and 3 (skeleton + first two tools) and stop. We'll review and continue in the next session. Do not implement write tools, network transports, or anything else marked out of scope. If a design question comes up that isn't answered in the spec, write the question into a "Questions" section at the bottom of the spec and stop — don't guess.
