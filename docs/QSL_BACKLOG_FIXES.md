# QSL — backlog fixes for Claude Code

Pre-MCP cleanup pass. Each item is independently mergeable. Order is roughly priority-by-impact, but feel free to batch.

## 1. Fix charset handling in HTML body rendering

**Symptom:** Visible mojibake in rendered HTML emails. Examples from the Allstate marketing email:
- "Good HandsÃ®" (should be "Good Hands®")
- "AllstateÂ® policy" (should be "Allstate® policy")

**Cause:** UTF-8 bytes being interpreted as Latin-1 (or Windows-1252). The message declares its charset in `Content-Type: text/html; charset=UTF-8` but the renderer is decoding as Latin-1.

**Fix:**
- Parse the `Content-Type` header from the MIME part, extract the `charset` parameter
- Decode body bytes using the declared charset; fall back to UTF-8, then Windows-1252 (in that order — never Latin-1, which doesn't have the curly quotes/em dashes most mail uses)
- For multipart messages, charset is per-part; don't assume it's consistent across parts
- Test against: emails declaring UTF-8, emails declaring Windows-1252, emails declaring no charset, emails with `charset=us-ascii` that actually contain UTF-8 (common in spam)

**Verification:** Allstate email renders "Good Hands®" not "Good HandsÃ®". Find an email with em dashes or smart quotes and confirm they render correctly.

**Why this is #1:** Every HTML email in the inbox is affected. Also, the MCP server's `get_message` tool will return the same garbled text to agents if this isn't fixed first — agents will see "AllstateÂ® policy" and either ignore it or hallucinate corrections.

## 2. Right-edge sliver (overflow bug)

**Symptom:** Faint vertical band visible past the reader pane border, all the way down the window. Visible in every screenshot so far. May be a scrollbar gutter, a 1px overflow, or a sibling element peeking through.

**Diagnostic steps:**
- Open Servo devtools (or compare against the same UI in a regular browser if dev mode allows)
- Inspect the outermost layout container; check for `overflow: visible` where it should be `hidden`
- Check for any element with `width: 100vw` that isn't accounting for scrollbar width
- Check whether the grid template has an extra column or the rightmost pane has `margin-right` instead of `padding-right`

**Fix:** Whatever's overflowing, clamp it. The outer shell should be `overflow: hidden` on body and root container. The reader pane's right edge should align exactly with the window's right edge.

**Verification:** Resize the window across widths from 800px to 1920px; no sliver visible at any width.

## 3. Sentence case folder names

**Symptom:** Sidebar and message-list header show "INBOX" in all caps. Other folder names are sentence case ("Sent Mail", "Drafts", "All Mail", "Spam", "Trash") — only "INBOX" is the outlier.

**Cause:** Almost certainly the IMAP folder name is the literal string `INBOX` (it's the standard IMAP convention) and the UI is rendering it raw. Other folders are showing their display names from Gmail's metadata.

**Fix:**
- Add a display-name mapping for canonical IMAP folder names. At minimum: `INBOX` → "Inbox". Consider also: `Sent` → "Sent Mail", `Junk` → "Spam" if you encounter non-Gmail accounts.
- Apply the mapping at the UI layer, not in the cache. The cache should keep the canonical IMAP name; the UI translates for display.
- Apply consistently in sidebar and message-list header.

**Verification:** Sidebar shows "Inbox", message-list header shows "Inbox". Both update if folder is renamed.

**Side benefit:** This same display-name mapping will be used by the MCP server's `list_folders` tool to populate the `display_name` field per the spec. Build it in a place both UI and MCP can reuse.

## 4. Remote image gating (privacy/tracking)

**Status: Partial.** Sanitizer-side blocking is now complete:
- `<img src>`, `srcset`, `poster`, `background` (Phase 1 Week 8)
- Inline `style="background-image: url(...)"` and `style="background: url(...) ..."` (this pass)
- `<link rel="stylesheet" href="...">` stripped by the sanitizer

`RenderedMessage.remote_content_blocked` is plumbed end-to-end and a per-sender allowlist exists in `remote_content_opt_ins`. The UI banner ("Images blocked. [Load images] [Always load from this sender]") and dimension-preserving placeholder boxes are deferred to a follow-up — tracked in `docs/KNOWN_ISSUES.md`.

**Original symptom:** HTML emails with `<img src="https://...">` load remote images on render. Visible in the Allstate email — both the hero image and the family photo are remote URLs that fetch on open. This means senders can track open events and confirm the email address is active.

**Original fix scope** (kept for reference; banner/placeholders are the deferred half):
- Default to **not** loading remote images
- Show a per-message banner: "Images blocked. [Load images] [Always load from this sender]"
- Replace blocked `<img>` tags with placeholder boxes the same dimensions (read `width`/`height` attrs; fall back to a fixed placeholder if missing) so layout doesn't reflow when loaded
- Persist per-sender allowlist in cache (key: sender address)
- Inline images (`cid:` references) load normally — they're embedded in the message, not remote
- `data:` URIs load normally — also embedded

**Edge cases (resolved):**
- CSS `background-image: url(...)` is the same problem in a different syntax. The HTML sanitizer now strips remote URLs from inline styles too — `filter_inline_style` walks the declaration list and drops any whose `url(...)` argument matches a block rule.
- `<link rel="stylesheet" href="...">` is stripped entirely by the sanitizer (`rm_tags = ["...", "link"]`).

**Verification:** Open the Allstate email; images don't load, banner appears, layout is preserved. Click "Load images"; images load. Click "Always load from this sender"; close and reopen the email; images load automatically.

**Why this matters before MCP:** Once an agent is reading mail via MCP and following links or summarizing content, having remote-image tracking firing on every agent-read email is a privacy leak the user didn't consent to. Solve it once at the rendering boundary.

## 5. HTML body sanitization (security)

**Symptom:** No visible bug in current screenshots, but worth confirming. HTML email is hostile by default — script tags, event handlers, remote stylesheets, iframe content.

**Fix:** Run all HTML email bodies through `ammonia` (Rust HTML sanitizer) before rendering. Strict allowlist:
- Allow: structural tags (`p`, `div`, `span`, `a`, `img`, `table`, `tr`, `td`, headings, lists, `br`, `hr`, `blockquote`, `pre`, `code`)
- Allow: limited inline style attributes (`color`, `background-color`, `font-*`, `text-*`, `padding`, `margin`, `border-*`, `width`, `height`)
- Strip: `<script>`, `<iframe>`, `<object>`, `<embed>`, `<form>`, `<input>`, `<button>`, all event handlers (`on*` attributes), `<link>`, `<meta>`
- Rewrite: `href` and `src` attributes — block `javascript:` URIs, allow `https:`, `mailto:`, `cid:`, `data:` (with size limit on data URIs to prevent denial-of-service)

**Note:** Servo's own rendering may already block some of these, but don't rely on it. Sanitize at the boundary so the same HTML is safe whether rendered in Servo, fed to MCP, or piped anywhere else.

**Verification:** Hand-craft a test email with `<script>alert(1)</script>` and `<a href="javascript:alert(1)">click</a>`; neither should execute or be clickable as JS.

## 6. "INBOX" vs "Inbox" in message-list header

Already covered by #3 if you handle it at the display-name layer. Listing separately in case the message-list header is reading from a different source than the sidebar — they should both pull from the same display-name resolver.

## 7. Verify unread count accuracy

**Status: Done (audit only — already correctly wired).** Both the sidebar and the message-list header read unread counts via the same `count_unread_by_folder` repo function (`crates/storage/src/repos/messages.rs:252`). On the IPC side, `folders_list` recomputes the count live per folder before returning (`apps/desktop/src-tauri/src/commands/folders.rs:35-49`), and `messages_list` / `messages_list_unified` call the same helper. On the UI side, both the sidebar's `folders_list` resource and each message-list resource include `sync_tick` in their `use_reactive!` deps, so a sync event refetches both within the same tick. A new defensive integration test in `crates/storage/tests/roundtrip.rs::count_unread_by_folder_matches_seen_flag_state` locks in the contract across `update_flags`. The original symptom was likely a transient async-refetch window after a `\Seen` flip — sidebar and message-list both refetch on the same tick, but their async resources can resolve a few hundred ms apart.

**Original symptom:** Inbox shows "86 of 86 · 0 unread" but earlier sidebar screenshot showed "INBOX 6" suggesting 6 unread.

**Verification:** Open Gmail web in another window. Counts match QSL for at least 3 folders.

## 8. Compose button state

**Symptom:** Prominent "Compose" button in sidebar. Presumably non-functional or partially functional given the 0.0.1 state.

**Fix options (pick one):**
- If compose is wired up: leave as-is, just verify it actually sends
- If compose opens a window but can't send: either gate it behind a "not yet" toast or visually disable it (`opacity: 0.5`, no hover state, tooltip explains why)
- If compose does nothing on click: remove the button until it works

Don't ship a button that does nothing on click — it erodes trust in the rest of the UI.

## 9. Threading

**Lower priority — flagging for awareness, not necessarily this pass.**

The three "Johnathon Fox / [johnathonfox/capytain] PR run failed" entries in the message list are clearly the same conversation (CI notifications on the same PR) but are shown as separate rows. Gmail-style threading via `X-GM-THRID` is a chunk of work but the single biggest UX upgrade for an email client.

If you have appetite for it before MCP, the rough shape:
- Cache schema needs a `thread_id` column on messages (probably already does if the data model's been multi-account-aware)
- Fetch `X-GM-THRID` for Gmail accounts via `FETCH ... (X-GM-THRID)`; for non-Gmail, build threads by `In-Reply-To`/`References` headers
- Message-list groups messages by thread; expanding a thread shows individual messages
- Reader pane shows the full thread when a thread is selected

If not this pass: ship the rest, leave threading for after MCP. The MCP spec already exposes `get_thread` and a `thread_id` field, so the data model needs to support it eventually anyway.

## 10. Load-more-on-scroll (low priority)

The "Load 50 older messages" button is functional but the modern pattern is auto-load when scrolling near the bottom. Five-line change with `IntersectionObserver`-equivalent (or scroll position detection if Servo doesn't support `IntersectionObserver` reliably — check before assuming). Defer until other items are done.

## Suggested order

1. Charset fix (highest impact, lowest risk)
2. Right-edge sliver (annoying, probably small)
3. Sentence case + display-name resolver (sets up MCP work)
4. Sanitization (`ammonia`) + remote image gating (do these together, both touch the HTML rendering boundary)
5. Unread count consistency
6. Compose button state decision
7. Threading (only if time)
8. Load-more-on-scroll (only if time)

Stop here. Move to MCP server per `QSL_MCP_SERVER_SPEC.md`.

## Prompt for Claude Code

> Work through `docs/QSL_BACKLOG_FIXES.md` in the suggested order. Each item is independently mergeable; commit after each. Do not skip the verification steps — for each item, manually confirm the fix works before moving to the next. If an item turns out to be larger than estimated (more than ~2 hours of work), stop and flag it before continuing. Do not start the MCP server work; that's a separate spec for the next session. Items 9 and 10 are explicitly optional — skip them unless items 1–8 are done and there's appetite to keep going.
