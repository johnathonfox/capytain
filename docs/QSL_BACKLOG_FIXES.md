# QSL — backlog fixes for Claude Code

Pre-MCP cleanup pass. Each item is independently mergeable. Order is roughly priority-by-impact, but feel free to batch.

## Status legend

- **Open** — not started.
- **Partial** — some pieces shipped, gap remains; details under each item.
- **Done** — verified shipped; nothing to do.

## 1. Fix charset handling in HTML body rendering

**Status: Done.** Root cause was *not* in `mail-parser` — the parser already honors declared charsets via its built-in charset table. The actual bug was in `crates/renderer/src/servo.rs::percent_encode`: it called `out.push(b as char)` on each byte of `&str::bytes()`, which treats every UTF-8 continuation byte (≥ 0x80) as a Latin-1 codepoint and re-encodes it. A single `®` (UTF-8 0xC2 0xAE) became the two Latin-1 chars `Â®`, which then re-encoded back to UTF-8 as 0xC3 0x82 0xC2 0xAE — Servo decoded those four bytes as UTF-8 and rendered `Â®`. Fix: percent-encode all bytes ≥ 0x80 too.

**Symptom (resolved):** "Good HandsÃ®" / "AllstateÂ® policy" in the Allstate marketing email.

**Verification:** Two unit tests added (`crates/renderer/src/servo.rs::tests::percent_encode_escapes_non_ascii_bytes` and `data_url_round_trips_utf8_through_percent_decode`), plus two defensive tests in `crates/mime/src/lib.rs` that lock in `mail-parser`'s charset behavior so MCP `get_message` doesn't regress.

## 2. Right-edge sliver (overflow bug)

**Status: Open.** No `overflow: hidden` on `.reader-pane` in `tailwind.css`; no other obvious culprit found in the survey.

**Symptom:** Faint vertical band visible past the reader pane border, all the way down the window. Visible in every screenshot so far. May be a scrollbar gutter, a 1px overflow, or a sibling element peeking through.

**Diagnostic steps:**
- Open Servo devtools (or compare against the same UI in a regular browser if dev mode allows)
- Inspect the outermost layout container; check for `overflow: visible` where it should be `hidden`
- Check for any element with `width: 100vw` that isn't accounting for scrollbar width
- Check whether the grid template has an extra column or the rightmost pane has `margin-right` instead of `padding-right`

**Fix:** Whatever's overflowing, clamp it. The outer shell should be `overflow: hidden` on body and root container. The reader pane's right edge should align exactly with the window's right edge.

**Verification:** Resize the window across widths from 800px to 1920px; no sliver visible at any width.

## 3. Sentence case folder names

**Status: Open.** UI renders the raw IMAP `Folder.name` directly. No display-name mapper exists today.

**Symptom:** Sidebar and message-list header show "INBOX" in all caps. Other folder names are sentence case ("Sent Mail", "Drafts", "All Mail", "Spam", "Trash") — only "INBOX" is the outlier.

**Cause:** Almost certainly the IMAP folder name is the literal string `INBOX` (it's the standard IMAP convention) and the UI is rendering it raw. Other folders are showing their display names from Gmail's metadata.

**Fix:**
- Add a display-name mapping for canonical IMAP folder names. At minimum: `INBOX` → "Inbox". Consider also: `Sent` → "Sent Mail", `Junk` → "Spam" if you encounter non-Gmail accounts.
- Apply the mapping at the UI layer, not in the cache. The cache should keep the canonical IMAP name; the UI translates for display.
- Apply consistently in sidebar and message-list header.

**Verification:** Sidebar shows "Inbox", message-list header shows "Inbox". Both update if folder is renamed.

**Side benefit:** This same display-name mapping will be used by the MCP server's `list_folders` tool to populate the `display_name` field per the spec. Build it in a place both UI and MCP can reuse.

## 4. Remote image gating (privacy/tracking)

**Status: Done (UI banner + opt-in shipped; dimension-preserving placeholders remain optional).** Sanitizer-side blocking is now complete:
- `<img src>`, `srcset`, `poster`, `background` (Phase 1 Week 8)
- Inline `style="background-image: url(...)"` and `style="background: url(...) ..."` (this pass)
- `<link rel="stylesheet" href="...">` stripped by the sanitizer

`RenderedMessage.remote_content_blocked` is plumbed end-to-end and a per-sender allowlist exists in `remote_content_opt_ins`. The reader-pane banner ("Images blocked. [Load images] [Always load from this sender]") shipped in the follow-up: `messages_get` now takes `force_trusted: bool` for one-shot loads, and `messages_trust_sender` persists the opt-in. **Still optional:** dimension-preserving placeholder boxes for blocked `<img>` tags so layout doesn't reflow when images load. Tracked in `docs/KNOWN_ISSUES.md`.

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

**Status: Done.** Phase 1 Week 7 (PR #37) wired ammonia through `qsl_mime::sanitize_email_html` with the strict allowlist below. Tests cover script-tag stripping, iframe/object/embed stripping, event-handler attribute stripping, `javascript:` URI stripping, form/input stripping, style-tag stripping (inline `style` preserved), tracking-pixel stripping, and benign-image-src preservation. `messages_get` calls it on every body fetch.

**Symptom:** No visible bug in current screenshots, but worth confirming. HTML email is hostile by default — script tags, event handlers, remote stylesheets, iframe content.

**Fix:** Run all HTML email bodies through `ammonia` (Rust HTML sanitizer) before rendering. Strict allowlist:
- Allow: structural tags (`p`, `div`, `span`, `a`, `img`, `table`, `tr`, `td`, headings, lists, `br`, `hr`, `blockquote`, `pre`, `code`)
- Allow: limited inline style attributes (`color`, `background-color`, `font-*`, `text-*`, `padding`, `margin`, `border-*`, `width`, `height`)
- Strip: `<script>`, `<iframe>`, `<object>`, `<embed>`, `<form>`, `<input>`, `<button>`, all event handlers (`on*` attributes), `<link>`, `<meta>`
- Rewrite: `href` and `src` attributes — block `javascript:` URIs, allow `https:`, `mailto:`, `cid:`, `data:` (with size limit on data URIs to prevent denial-of-service)

**Note:** Servo's own rendering may already block some of these, but don't rely on it. Sanitize at the boundary so the same HTML is safe whether rendered in Servo, fed to MCP, or piped anywhere else.

**Verification:** Hand-craft a test email with `<script>alert(1)</script>` and `<a href="javascript:alert(1)">click</a>`; neither should execute or be clickable as JS.

## 6. "INBOX" vs "Inbox" in message-list header

**Status: Folded into #3.** The sidebar and the message-list header already pull from the same `Folder` struct; once the display-name mapper from #3 lands and is applied at both call sites (`SidebarMailboxRow` + `folder_title_from_selection`), this is fixed in the same change.

## 7. Verify unread count accuracy

**Status: Done (audit only — already correctly wired).** Both the sidebar and the message-list header read unread counts via the same `count_unread_by_folder` repo function (`crates/storage/src/repos/messages.rs:252`). On the IPC side, `folders_list` recomputes the count live per folder before returning (`apps/desktop/src-tauri/src/commands/folders.rs:35-49`), and `messages_list` / `messages_list_unified` call the same helper. On the UI side, both the sidebar's `folders_list` resource and each message-list resource include `sync_tick` in their `use_reactive!` deps, so a sync event refetches both within the same tick. A new defensive integration test in `crates/storage/tests/roundtrip.rs::count_unread_by_folder_matches_seen_flag_state` locks in the contract across `update_flags`. The original symptom was likely a transient async-refetch window after a `\Seen` flip — sidebar and message-list both refetch on the same tick, but their async resources can resolve a few hundred ms apart.

**Original symptom:** Inbox shows "86 of 86 · 0 unread" but earlier sidebar screenshot showed "INBOX 6" suggesting 6 unread.

**Verification:** Open Gmail web in another window. Counts match QSL for at least 3 folders.

## 8. Compose button state

**Status: Done (working as designed).** Compose button opens a full compose pane with to/cc/bcc/subject/body fields, auto-saves to the local drafts table every 5s, and offers Close / Discard / Save buttons. **There is no Send button** — sending is intentionally deferred to Phase 2 Week 18 (Gmail SMTP) / Week 19 (Fastmail JMAP), and the pane footer states this explicitly: "Sending lands in Phase 2 Week 18 (Gmail SMTP) and Week 19 (Fastmail JMAP). Drafts are saved locally for now." The button doesn't dangle; the user gets a working drafts experience and a clear "send isn't here yet" signal.

**Symptom:** Prominent "Compose" button in sidebar. Presumably non-functional or partially functional given the 0.0.1 state.

**Fix options (pick one):**
- If compose is wired up: leave as-is, just verify it actually sends
- If compose opens a window but can't send: either gate it behind a "not yet" toast or visually disable it (`opacity: 0.5`, no hover state, tooltip explains why)
- If compose does nothing on click: remove the button until it works

Don't ship a button that does nothing on click — it erodes trust in the rest of the UI.

## 9. Threading

**Status: Done.** Phase 1 Week 13 (PR #54) shipped the threading pipeline: `thread_id` column on messages, `X-GM-THRID` fetch on Gmail, In-Reply-To / References / subject+30d fallback on non-Gmail, full assembly pipeline. The MCP spec's `get_thread` tool maps onto the same data model.

**Lower priority — flagging for awareness, not necessarily this pass.**

The three "Johnathon Fox / [johnathonfox/capytain] PR run failed" entries in the message list are clearly the same conversation (CI notifications on the same PR) but are shown as separate rows. Gmail-style threading via `X-GM-THRID` is a chunk of work but the single biggest UX upgrade for an email client.

If you have appetite for it before MCP, the rough shape:
- Cache schema needs a `thread_id` column on messages (probably already does if the data model's been multi-account-aware)
- Fetch `X-GM-THRID` for Gmail accounts via `FETCH ... (X-GM-THRID)`; for non-Gmail, build threads by `In-Reply-To`/`References` headers
- Message-list groups messages by thread; expanding a thread shows individual messages
- Reader pane shows the full thread when a thread is selected

If not this pass: ship the rest, leave threading for after MCP. The MCP spec already exposes `get_thread` and a `thread_id` field, so the data model needs to support it eventually anyway.

## 10. Load-more-on-scroll (low priority)

**Status: Done.** Shipped 2026-04-26 in PR #60 (commit `d4fc1d2`). Replaced the "Load 50 older messages" button with an `onscroll` handler on `.msglist-scroll` that fires `messages_load_older` whenever the user gets within 200px of the bottom, gated by `loading_older` so a fast scroll only triggers one batch.

## 11. Popup reader: reuse the main pane's RenderedMessage cache

**Status: Open.** `messages_open_in_window` always calls `messages_get` to build the popup preload. When the user double-clicks a message that's already selected in the main pane, we pay the lazy-fetch cost a second time — measured ~493 ms on a marketing email whose body wasn't yet on disk.

**Symptom:** First popup open for a not-yet-cached message takes ~500 ms longer than subsequent opens of the same message, because the second open hits the warm body blob.

**Diagnostic / fix sketch:**
- The main reader pane already calls `messages_get` for the selected message; the result lives in Dioxus signal state, not a server-side cache.
- Two paths: (a) lift a `RenderedMessage` cache to `AppState` keyed by `MessageId` with TTL/LRU; (b) have the UI's double-click handler pass the already-rendered HTML directly to the popup command, bypassing re-fetch.
- (a) helps any consumer of `messages_get`; (b) is one-line on the UI side but only helps the "double-click while reader pane shows it" case.

**Verification:** Open a message in the main pane → wait for it to render → double-click to popup. Popup `preload fetched` line in the log should be <50 ms regardless of body size.

## 12. Popup reader: reduce the install → first-paint gap

**Status: Open.** Even with the GTK layout pump capped at 100 ms (commit `044d1cf`), there's still ~250 ms between Servo install completing and the first `reader_set_position` arriving. The popup's Servo overlay paints into its install-time off-screen rect until Dioxus boots, mounts `ReaderOnlyApp`, and the `ResizeObserver` pushes the real bounding rect.

**Symptom:** Popup window visibly shows header-only chrome before the body appears, even on warm-cache opens.

**Fix sketch:**
- Compose the reader HTML on the Rust side (move/duplicate `compose_reader_html` from `apps/desktop/ui/src/app.rs` into a shared crate, e.g. `qsl-mime`) and call `renderer.render(html)` immediately after Servo install completes.
- Pre-position the Servo overlay using known popup dimensions (window inner size minus a fixed header height) so the body paints into the correct rect before Dioxus mounts. ResizeObserver still pushes corrections later.

**Verification:** Time between `Servo install completed` and the first visible body paint should drop from ~250 ms to <50 ms.

## Suggested order

Updated to reflect status. Strikethrough = nothing to do.

1. ~~Charset fix~~ — Done (root cause was the renderer's `percent_encode`, not `mail-parser`)
2. **Right-edge sliver** (annoying, probably small) — Open
3. **Sentence case + display-name resolver** (sets up MCP work) — Open. Also closes #6.
4. **Remote image gating: sanitizer half** (CSS `background-image` filter only; UI banner deferred) — Partial
5. ~~Sanitization (`ammonia`)~~ — Done
6. ~~"INBOX" vs "Inbox" header~~ — Folded into #3
7. **Unread count consistency** (audit + likely add `sync_tick` to a `use_reactive!` dep) — Open
8. ~~Compose button state decision~~ — Done (working as designed; pane footer states the Phase 2 deferral)
9. ~~Threading~~ — Done (Phase 1 Week 13)
10. ~~Load-more-on-scroll~~ — Done (PR #60, 2026-04-26)

**Active work this pass:** items 1, 2, 3, 4 (sanitizer half), 7. Five commits. After this pass, MCP server per `QSL_MCP_SERVER_SPEC.md`.

**Deferred to a follow-up PR:** the UI banner + Load-images / Allow-from-sender buttons for #4. Tracked in `docs/KNOWN_ISSUES.md`.

## Prompt for Claude Code

> Work through `docs/QSL_BACKLOG_FIXES.md` in the suggested order. Each item is independently mergeable; commit after each. Do not skip the verification steps — for each item, manually confirm the fix works before moving to the next. If an item turns out to be larger than estimated (more than ~2 hours of work), stop and flag it before continuing. Do not start the MCP server work; that's a separate spec for the next session. Items 9 and 10 are explicitly optional — skip them unless items 1–8 are done and there's appetite to keep going.
