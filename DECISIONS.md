<!--
SPDX-FileCopyrightText: 2026 Capytain Contributors
SPDX-License-Identifier: Apache-2.0
-->

# Capytain Engineering Decisions

Append-only log of meaningful design / implementation choices. Format per entry: a one-line summary, the **decision**, the **why**, and any **alternatives considered**. Newest entries on top.

---

## 2026-04-25 · IMAP move: prefer `UID MOVE`, fall back to `UID COPY` + `STORE \Deleted` + `UID EXPUNGE`

**Decision.** `ImapBackend::move_messages` first tries `UID MOVE` (RFC 6851). If the server responds with `BAD` or "not enabled", we fall back to the legacy three-step `UID COPY <set> <target>` → `UID STORE <set> +FLAGS (\Deleted)` → `UID EXPUNGE <set>`. The `UID EXPUNGE` form (RFC 4315) is intentional — plain `EXPUNGE` would also pick up any other `\Deleted`-flagged messages in the source folder, unsafe in shared-mailbox scenarios.

**Why.** Gmail and Fastmail both advertise `MOVE` so the fast path lands a single round-trip. Servers without it (some self-hosted Dovecot configs) need the fallback to function at all; replicating it explicitly here means the engine doesn't have to know which path the server took.

**Alternatives.**
- `UID MOVE` only, fail otherwise — simpler but loses self-host compatibility for no real benefit on the providers we care about.
- Always use the three-step fallback — costs an extra round-trip on every move against modern servers.

## 2026-04-25 · Delete = `\Deleted + EXPUNGE`, accepting Gmail "moves to Trash"

**Decision.** `delete_messages` issues `UID STORE +FLAGS (\Deleted)` followed by `UID EXPUNGE`. We don't try to detect Gmail and route through `UID MOVE` to `[Gmail]/Trash` instead.

**Why.** Gmail interprets `\Deleted + EXPUNGE` as "move to Trash" — which is what users expect from "delete." Forcing Trash-routing in code would re-implement the server's behavior in our adapter and require special-casing per provider. The user-visible difference between Gmail (recoverable) and Fastmail (permanent) is documented; the protocol is the same.

**Alternatives.**
- Provider-specific delete (Gmail → MOVE to Trash; Fastmail → Email/destroy) — adds an explicit "where does deleted mail go?" decision per backend; the spec doesn't call for it.
- Keep the row locally with a "tombstoned" flag pending server confirmation — defers risk to outbox failure handling but adds a second DB column and rendering rule for no observable benefit.

## 2026-04-25 · Outbox drain: 5-second tick, exponential backoff with jitter, hard `MAX_ATTEMPTS = 5`

**Decision.** The desktop sync engine spawns one fixed-cadence drain task on a 5-second `tokio::time::interval`. Per-row failures bump `attempts` and reschedule via the schedule `30s · 4^(attempts-1)` with ±20% uniform jitter, capped by transitioning the row to a dead-letter state once `attempts == MAX_ATTEMPTS = 5`. DLQ rows (`next_attempt_at = NULL`) are surfaced to the UI as a synthetic `SyncEvent::FolderError` so the existing `sync_event` listener picks them up without a new IPC type.

**Why.** The spec exit criterion ("Marking a message read in Capytain updates Gmail within seconds") wants short median latency without flooding the server when something genuinely fails. 5s is short enough that humans don't notice; the 30s/2m/8m/30m schedule is the standard pattern for non-critical retry. Jitter keeps a thundering-herd of optimistic clicks (e.g. "mark all as read" on 200 messages) from re-firing in lockstep. Hard cap at 5 keeps a permanently broken row from quietly retrying forever.

**Alternatives.**
- Eager drain on each `messages_mark_read` invocation — would land the typical case faster, but couples UI commands to a background concern. Tick-based stays simpler.
- Linear retry — simpler but burns server credit on persistently failing rows.
- Persistent backoff state outside the row — `last_error` already lives on the row and there's no other consumer.

## 2026-04-25 · Outbox payload as JSON, op_kind as string

**Decision.** `outbox.payload_json` is opaque JSON keyed by `op_kind` (string). The drain worker matches on `op_kind` and deserializes into the appropriate `Payload` struct (`UpdateFlagsPayload` for now; `MovePayload`, `DeletePayload` follow in the next PR).

**Why.** Lets us add new op_kinds without a migration. The serialization cost is negligible (single-row mutations are nowhere near the hot path); the tradeoff is type-erasure across the storage boundary, which the per-kind deserialize step recovers cheaply. Mirrors how `messages.flags_json` and `from_json` already use opaque-JSON columns for fields with stable per-row shape but evolving schemas.

**Alternatives.**
- One column per op_kind with NULL elsewhere — wastes columns and forces a migration per new mutation type.
- Separate table per op_kind — adds N tables for N mutations; the drain dispatch is the one place that benefits from union-shape access.

---

## 2026-04-25 · Threading: ASCII subject normalization, 30-day recency window

**Decision.** `capytain_sync::threading::normalize_subject` ASCII-lowercases the subject and collapses whitespace; it does **not** apply Unicode case folding. The subject-fallback step in the assembly pipeline only matches threads whose `last_date` is within the last **30 days**.

**Why.** `PHASE_1.md`'s "Open Questions for Phase 1" already named both choices and leaned ASCII / 30 days. Per-insert performance matters because thread assembly runs synchronously inside `sync_folder`. CJK subject threads might miss-match on this lower-cost path; the `In-Reply-To` and `References` chain steps run first and resolve well-behaved clients regardless of subject locale.

**Alternatives.**
- `unicode_normalization` + `caseless::canonical_caseless_match_str` — correct but pulls a non-trivial dep into the per-insert hot path with no observed benefit on Gmail/Fastmail traffic.
- Larger window (60 / 90 days) — increases false-positive merging of recurring "Re: lunch?" threads.

---

## 2026-04-25 · Threading: store `In-Reply-To` and `References` on `MessageHeaders`

**Decision.** Added `in_reply_to: Option<String>` and `references: Vec<String>` directly to `capytain_core::MessageHeaders`. Both adapters populate them at sync time — IMAP via a `BODY.PEEK[HEADER]` extension to the FETCH query, JMAP via `Email/get`'s native `inReplyTo` / `references` fields. Persisted to the messages table via a new migration `0003_threading_columns.sql` (a TEXT column for `in_reply_to` and a JSON-array column `references_json` paralleling the existing `from_json` / `labels_json` pattern).

**Why.** Thread assembly runs synchronously after a message insert and walks both fields; surfacing them only on `MessageBody` (which currently exists) would require a second async fetch path or postpone threading until the body-fetch pass. Carrying them on `MessageHeaders` keeps the assembly pipeline a pure DB operation and lets `messages_repo::find_by_rfc822_id` (the one new lookup it needs) run against the indexed `rfc822_message_id` column we already had.

**Alternatives.**
- A separate `thread_refs(message_id, ref_type, ref_message_id)` table — normalizes nicely but adds N+1 lookups per message and more migration surface for no observable benefit at the size where threading runs.
- Keep references on `MessageBody` and delay assembly until body fetch — defeats the purpose of running assembly per insert (the spec explicitly says "Thread assembly pipeline runs after each message insert").

---

## 2026-04-25 · IMAP threading via `BODY.PEEK[HEADER]`, not header-fields-only fetch

**Decision.** The IMAP `list_messages` FETCH query asks for the **full** RFC 5322 header block (`BODY.PEEK[HEADER]`) rather than the targeted `BODY.PEEK[HEADER.FIELDS (REFERENCES IN-REPLY-TO)]` shape that RFC 3501 §6.4.5 also permits.

**Why.** `imap-proto`'s `MessageSection` enum (the response parser type) carries only `Header` / `Mime` / `Text` variants — there's no structured way to represent `HEADER.FIELDS (…)`. The server-side response would arrive as either a wrong-shape `BodySection` (which `Fetch::header()` ignores) or fail to parse cleanly. Sending the unscoped `BODY.PEEK[HEADER]` returns the full block as `MessageSection::Header`, which `Fetch::header()` already exposes; we then parse with `mail-parser` (already a workspace dep via `capytain-mime`).

**Alternatives.**
- Targeted header-fields fetch — would save bytes on the wire (typical `Subject` + envelope fields are smaller than the full header), but the parser-side support isn't there in `imap-proto` 0.16.
- Defer References parsing until body fetch — cuts one round-trip when a folder syncs no new messages, but breaks per-insert assembly.

The full-header cost is typically <4 KB per message; against `RFC822.SIZE` + `INTERNALDATE` + `ENVELOPE` already in the response it's a 2–3× bump in FETCH response size, well below the cost of a second round-trip.

---

## 2026-04-25 · `BackendEvent::AccountChanged` for JMAP push

**Decision.** Phase 1 Week 11 introduced a new `BackendEvent::AccountChanged` variant rather than fanning out per-folder `FolderChanged` events on each JMAP push notification.

**Why.** JMAP's EventSource notification payload says "type Email/Mailbox/EmailDelivery has new state" without naming a mailbox. Resolving "which folder?" before emitting would mean an extra `Email/changes` round-trip per push *just to drive the per-folder dispatch* — wasted work because the engine then runs `sync_account` (which already calls `Email/changes`) for the actual sync. Engine-side debouncing already keeps the number of `sync_account` calls low.

**Alternatives.**
- Fan out one `FolderChanged` per known folder per push — multiplies engine work by N folders for no gain, and N can grow with Gmail-style label proliferation.
- Single `AccountChanged` with a list of changed mailbox ids — would let the engine skip unchanged folders, but JMAP push events don't carry that detail. Adding it would require a follow-up `Email/changes` round-trip; the engine already does that as part of `sync_account`.

---

## 2026-04-25 · Backend factory mirrored, not extracted

**Decision.** `apps/desktop/src-tauri/src/backend_factory.rs` and `apps/mailcli/src/main.rs::open_backend` carry parallel implementations of "given an `Account`, refresh OAuth and return a live `MailBackend`".

**Why.** `capytain-sync` is the natural shared home, but extracting the factory would force the engine crate to depend on `capytain-imap-client` + `capytain-jmap-client`, inverting the dependency direction (the adapter crates depend on `capytain-core`'s `MailBackend` trait — not the other way around). Keeping the engine backend-agnostic via the trait is more valuable than de-duplicating ~80 lines of straightforward dispatch.

**Alternatives.**
- Extract into a new `capytain-backends` crate that depends on both adapters and exposes one `open_backend(account) -> Arc<dyn MailBackend>` — defensible, but ships a crate whose only purpose is glue and inverts no real complexity.
- Move the factory into `capytain-sync` directly — same dependency-direction problem.
