# Diagnostic logging for IMAP, Turso, sync, auth, SMTP, JMAP

**Status:** Draft
**Date:** 2026-05-03
**Branch context:** post-`history-sync-perf-and-resume` (v0.1.1)

## Motivation

Two recent debugging sessions cost a disproportionate amount of time
because the relevant subsystems are under-logged.

- **2026-04-30 FTS-rebuild bug** — history sync slowed to ~1 insert/sec
  for five hours before the cause was identified. The storage layer
  emitted no per-operation timing, so nothing in the logs would have
  surfaced "this INSERT took 1.4s because Turso's Tantivy index is
  rebuilding on every implicit commit". The fix shipped in v0.1.1
  (`messages::batch_insert_skip_existing` wraps each chunk's inserts
  in one transaction). The diagnostic gap remains.
- **OAuth + IMAP smoke** — five separate quirks (see
  `feedback_oauth_gotchas.md`) were debugged blind because
  `qsl-imap-client::auth` and `capabilities` log nothing at the
  boundaries where things go wrong.

Today's coverage:

| Crate | Log sites | Notable gaps |
|---|---|---|
| `qsl-imap-client` | ~30 in `backend.rs`/`idle.rs` | `auth.rs`, `capabilities.rs`, `sync_state.rs` are silent. No per-command timings. |
| `qsl-storage` | 2 warns in `turso_conn.rs`, a handful in `migrations.rs` | No per-query timings. No transaction boundaries. Repos are silent. |
| `qsl-sync` | a few `#[instrument]` + warns | `pull_history` chunk loop has no per-chunk timing. |
| `qsl-auth`, `qsl-smtp-client`, `qsl-jmap-client` | minimal | No timings. No structured fields on errors. |

`qsl-telemetry` already does the work of installing `tracing-subscriber`
with `EnvFilter` precedence and Servo suppressions. The shape is good;
the call sites are thin.

## Goals

1. Fill in `tracing::debug!` / `tracing::error!` calls at the
   underlogged boundaries listed above so a `RUST_LOG=info` run gives
   a useful narrative of what's happening.
2. Add a watchdog that emits `warn!` whenever a bounded operation
   exceeds a documented threshold, so the FTS-rebuild class of bug
   surfaces in seconds rather than hours.
3. Keep all of (1) and (2) inside the existing `RUST_LOG`-driven
   model — no new sinks, no metrics, no Settings UI.
4. Verify on every new debug site that no token-bearing structure can
   leak.

## Non-goals

- Metrics, histograms, OpenTelemetry, Prometheus exporters.
- Persistent log files or rotation. stderr remains the only sink.
- Runtime threshold tuning. Thresholds are `const`s in
  `qsl-telemetry::slow::limits` and changed by recompile.
- Reworking the existing logs in `backend.rs` / `idle.rs`. Only add;
  only adjust where a current log is plain wrong.

## Design

### `qsl-telemetry::slow`

A new module exposing one macro and a const block.

```rust
pub mod limits {
    pub const IMAP_CMD_MS:    u64 = 5_000;
    pub const DB_QUERY_MS:    u64 = 250;
    pub const TX_COMMIT_MS:   u64 = 1_000;
    pub const SMTP_SUBMIT_MS: u64 = 10_000;
    pub const HTTP_JMAP_MS:   u64 = 5_000;
    pub const OAUTH_TOKEN_MS: u64 = 5_000;
}
```

`time_op!` semantics, regardless of exact `macro_rules!` shape:

- Takes `target: &str`, `limit_ms: u64`, `op: &str`, an async body, and
  a forwarded set of `tracing` fields.
- Records `Instant::now()`, awaits the body, computes elapsed ms.
- If `elapsed_ms >= limit_ms`: `tracing::warn!` on the supplied target
  with `op`, `elapsed_ms`, `limit_ms`, and the user-supplied fields.
- Otherwise: `tracing::debug!` on the same target with `op`,
  `elapsed_ms`, and the user-supplied fields.
- Returns the body's value unchanged. Works for `T`, `Result<T, E>`,
  and `()`.

The exact `macro_rules!` form is an implementation detail (see "Open
questions"). The semantics above are the contract.

Targets follow `qsl::slow::<subsystem>` so users can do
`RUST_LOG=warn,qsl::slow=warn` to see only watchdog hits.

| Subsystem | Target |
|---|---|
| IMAP commands | `qsl::slow::imap` |
| DB queries / commits | `qsl::slow::db` |
| Sync chunk drivers | `qsl::slow::sync` |
| OAuth refresh / revoke | `qsl::slow::auth` |
| SMTP submission | `qsl::slow::smtp` |
| JMAP HTTP | `qsl::slow::jmap` |

### Per-crate fill-in

#### `qsl-imap-client`

- **`auth.rs`** — `#[instrument(skip(secret))]` on the SASL OAUTH2
  exchange. `info!` on entry with `mechanism`. `debug!` on each step
  with byte counts only — never the literal SASL string. `warn!` on
  auth failure with the server's untagged response truncated to 200
  chars. `time_op!` around the whole exchange at `OAUTH_TOKEN_MS`.
- **`capabilities.rs`** — `info!` once with the negotiated capability
  set sorted. `warn!` if `IDLE` or `UIDPLUS` is missing (we don't
  degrade gracefully for those). The existing raw-cap `debug!` in
  `backend.rs:330` stays for verbose mode.
- **`sync_state.rs`** — `debug!` on resume / save with
  `(uidvalidity, last_seen_uid, modseq)`. `warn!` on UIDVALIDITY
  change with old/new values.
- **`backend.rs`** — wrap each FETCH / STORE / MOVE / SEARCH / COPY /
  EXPUNGE / APPEND call in `time_op!` at `IMAP_CMD_MS`. Structured
  fields: `cmd`, `folder`, `uid_count`, `bytes` where applicable.
  Sweep the connect ladder for `?` paths that currently propagate
  without context and surface those at `error!` first.

#### `qsl-storage`

- **`turso_conn.rs`** — `#[instrument(skip(self, params))]` on
  `query` / `execute` / `begin` / `commit`. `time_op!` inside
  `query`/`execute` at `DB_QUERY_MS`; inside `commit` at
  `TX_COMMIT_MS`. The SQL string is truncated to its first 80 chars
  in any log site (full query is private — avoids dumping user-data
  substrings when a parameter binding contains a long subject line).
- **`repos/*`** — `#[instrument(skip(conn, params))]` on every public
  fn. Structured fields scoped to the repo: `account` (`?AccountId`),
  `count` (rows in/out), `folder` where the fn takes one. `time_op!`
  on batched fns only (`batch_insert_skip_existing`, anything that
  takes a slice). Single-row fns are bounded by the underlying
  `query`/`execute` watchdog and don't need a second one.
- **`migrations.rs`** — wrap `apply_one` in `time_op!` at
  `TX_COMMIT_MS`. Existing per-migration `info!` stays.

#### `qsl-sync`

- **`history.rs::pull_history`** —
  `#[instrument(skip(state, conn, backend),
  fields(account = %account_id.0, folder = %folder.0))]`. `info!` at
  start with `last_anchor_uid` / `low_uid`. `info!` at end with
  `total_inserted`, `chunks_processed`, `wall_clock_ms`. `time_op!`
  per chunk at `IMAP_CMD_MS` covering both the FETCH and the persist.
  Per-chunk `debug!` with chunk index, range, and rows inserted.
  Every `?` that currently propagates without context gains an
  `error!` log first.
- **`outbox_drain.rs`** — `time_op!` per dispatch at `IMAP_CMD_MS`.
  Fields: `op` (the operation kind), `account`.
- **`lib.rs`** — already instrumented; tighten the field set to match
  the conventions used elsewhere (`account` always present).

#### `qsl-auth`

- **`tokens.rs`** — `Display` / `Debug` are already redacted
  (regression test in place). Audit any new debug site that touches
  `AccessToken` / `RefreshToken` and confirm bytes never reach a log
  call.
- **`flow.rs`** — `time_op!` around the loopback redirect + token
  exchange at `OAUTH_TOKEN_MS`. Existing CSRF warn at line 127 stays.
- **`refresh.rs`** — `time_op!` around refresh and revoke. `debug!`
  per provider HTTP call (URL only — no headers, no body).

#### `qsl-smtp-client`

- `time_op!` around `lettre::send` at `SMTP_SUBMIT_MS`. Fields:
  `to_count`, `bytes`. `error!` on send failure with the SMTP
  response code; never the message body.

#### `qsl-jmap-client`

- `time_op!` per call at `HTTP_JMAP_MS`. Fields: `method` (e.g.
  `Email/get`), `account`. `Authorization` headers must never reach a
  log site — verify via redaction test.

### Redaction policy

Codified once and enforced via a regression test:

- No `Authorization` header, no `Bearer ` token, no SASL string, no
  refresh token, no client secret, no OAuth `code`, no `id_token`
  JWT body may appear in any log site introduced by this work.
- Token-bearing struct types (`AccessToken`, `RefreshToken`) are
  already `Debug`-redacted; the test verifies that the new sites
  don't bypass `Debug` by `.0`-accessing the inner string.

## Tests

1. **`qsl-telemetry` slow macro**
   - `time_op!` warns when body sleeps `> limit_ms`; emits debug
     otherwise. Use a capturing `tracing-subscriber` test layer to
     assert on level, target, and `elapsed_ms` field.
   - `time_op!` returns the body's value unchanged for `Result<T, E>`
     (both `Ok` and `Err`) and for `()` returns.
2. **Redaction regression**
   - One test per crate that exercises each new debug/warn site under
     a capturing layer and asserts no field starts with `Bearer `,
     no field matches `^eyJ` (JWT prefix), and no field equals the
     test fixtures used for OAuth `code` / `client_secret`.
3. **Manual smoke**
   - `RUST_LOG=qsl::slow=warn,qsl_storage=info cargo run -p
     qsl-desktop` on the maintainer's NVIDIA + KWin box.
   - Trigger a fresh history sync; observe per-chunk `info` line.
   - Confirm zero slow warnings post-FTS-rebuild on a healthy DB.
   - Force-induce drift via `mailcli reset` + small history pull,
     deliberately skip `--rebuild-fts`, confirm slow warns appear.
     This regression-protects the v0.1.1 fix.

## Verification before merge

- `QSL_SKIP_UI_BUILD=1 cargo clippy --workspace --all-targets -- -D warnings`
- `QSL_SKIP_UI_BUILD=1 cargo test --workspace`
- `cargo fmt --all` (CI's Check job rejects fmt drift).
- Manual smoke per the test plan.

## Files touched

- `crates/telemetry/src/slow.rs` (new)
- `crates/telemetry/src/lib.rs` (export `slow`)
- `crates/imap-client/src/{auth,capabilities,sync_state,backend}.rs`
- `crates/storage/src/{turso_conn,migrations}.rs`,
  `crates/storage/src/repos/*.rs`
- `crates/sync/src/{history,outbox_drain,lib}.rs`
- `crates/auth/src/{flow,refresh,tokens}.rs`
- `crates/smtp-client/src/lib.rs` (or wherever `submit` lives —
  confirm during planning)
- `crates/jmap-client/src/lib.rs` (or wherever the call entry-points
  live — confirm during planning)
- `crates/telemetry/tests/slow.rs` (new)
- `crates/telemetry/tests/redaction.rs` (new — or per-crate, decided
  during planning)

## Open questions for the planning pass

Two minor items, neither blocking:

- **Exact `macro_rules!` form for `time_op!`.** The contract above is
  unambiguous; the macro grammar is fiddly because `tracing` field
  syntax (`%foo`, `?foo`) is not a single Rust token. If the
  one-macro form fails, fall back to two narrower forms
  (`time_op_pct!` / `time_op_dbg!`) and pick the right one per call
  site.
- **JMAP / SMTP entry-point shape.** The plan walkthrough confirms
  whether each call point is wrappable in place or needs a small
  private helper to give `time_op!` a clean async-block to time.
