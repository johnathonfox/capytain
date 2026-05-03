// SPDX-License-Identifier: Apache-2.0

//! Slow-operation watchdog.
//!
//! [`time_op!`] times an awaitable body and emits a structured tracing
//! event. If the elapsed time meets or exceeds the supplied
//! `limit_ms`, the event is a `warn!`; otherwise it's a `debug!`. The
//! body's return value is passed through unchanged.
//!
//! Targets follow the convention `qsl::slow::<subsystem>` so a user
//! can do `RUST_LOG=warn,qsl::slow=warn` to see only the watchdog
//! hits without enabling info chatter everywhere.
//!
//! See `docs/superpowers/specs/2026-05-03-imap-turso-diagnostic-logging-design.md`
//! for the full design.
//!
//! # Example
//!
//! ```ignore
//! use qsl_telemetry::{time_op, slow::limits};
//!
//! let messages = time_op!(
//!     target: "qsl::slow::imap",
//!     limit_ms: limits::IMAP_CMD_MS,
//!     op: "fetch",
//!     fields: { account = %account_id, folder = %folder_name, uid_count = n },
//!     backend.fetch_chunk(uid_range)
//! )?;
//! ```
//!
//! Fields inside `fields: { … }` use `tracing` field syntax —
//! `name = %display_val` for `Display`, `name = ?debug_val` for
//! `Debug`, plain `name = expr` for values implementing
//! `tracing::Value`. Do not write a trailing comma after the last
//! field; the macro inserts one.

/// Default thresholds, in milliseconds, for the various subsystems.
///
/// These are conventions, not enforcement: each call site picks its
/// own `limit_ms`. The constants live here so tuning is one edit per
/// subsystem rather than scattered.
pub mod limits {
    /// IMAP command (FETCH, STORE, MOVE, SEARCH, …).
    pub const IMAP_CMD_MS: u64 = 5_000;
    /// Single SQL query / execute.
    pub const DB_QUERY_MS: u64 = 250;
    /// Transaction commit.
    pub const TX_COMMIT_MS: u64 = 1_000;
    /// SMTP submission via `lettre`.
    pub const SMTP_SUBMIT_MS: u64 = 10_000;
    /// JMAP HTTP call.
    pub const HTTP_JMAP_MS: u64 = 5_000;
    /// OAuth token refresh / revoke.
    pub const OAUTH_TOKEN_MS: u64 = 5_000;
}

/// Time an awaitable body and emit a tracing event.
///
/// On overrun (`elapsed_ms >= limit_ms`), emits `warn!` on the
/// supplied target with `op`, `elapsed_ms`, `limit_ms`, and any
/// caller-supplied fields. Otherwise emits `debug!` with `op`,
/// `elapsed_ms`, and the same fields. Returns the body's value.
///
/// `$body` must be an `IntoFuture` (typically an `async fn` call
/// without a trailing `.await`). The macro performs the `.await`.
///
/// See the module docs for field syntax.
#[macro_export]
macro_rules! time_op {
    // With fields.
    (
        target: $tgt:expr,
        limit_ms: $lim:expr,
        op: $op:expr,
        fields: { $($field:tt)+ },
        $body:expr $(,)?
    ) => {{
        let __start = ::std::time::Instant::now();
        let __out = ($body).await;
        let __elapsed_ms = __start.elapsed().as_millis() as u64;
        let __limit_ms: u64 = $lim;
        if __elapsed_ms >= __limit_ms {
            ::tracing::warn!(
                target: $tgt,
                op = $op,
                elapsed_ms = __elapsed_ms,
                limit_ms = __limit_ms,
                $($field)+ ,
                "slow {}", $op
            );
        } else {
            ::tracing::debug!(
                target: $tgt,
                op = $op,
                elapsed_ms = __elapsed_ms,
                $($field)+ ,
                "{} ok", $op
            );
        }
        __out
    }};
    // Without fields.
    (
        target: $tgt:expr,
        limit_ms: $lim:expr,
        op: $op:expr,
        $body:expr $(,)?
    ) => {{
        let __start = ::std::time::Instant::now();
        let __out = ($body).await;
        let __elapsed_ms = __start.elapsed().as_millis() as u64;
        let __limit_ms: u64 = $lim;
        if __elapsed_ms >= __limit_ms {
            ::tracing::warn!(
                target: $tgt,
                op = $op,
                elapsed_ms = __elapsed_ms,
                limit_ms = __limit_ms,
                "slow {}", $op
            );
        } else {
            ::tracing::debug!(
                target: $tgt,
                op = $op,
                elapsed_ms = __elapsed_ms,
                "{} ok", $op
            );
        }
        __out
    }};
}
