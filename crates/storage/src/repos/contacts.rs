// SPDX-License-Identifier: Apache-2.0

//! Contacts collection — every address QSL has seen, surfaced to
//! the compose pane's autocomplete dropdown.
//!
//! The shape is intentionally narrow. There's no "delete" or
//! "merge" operation; observed addresses just accumulate. The
//! dropdown filters at query time on prefix match against
//! `address` or `display_name`.
//!
//! Two write entry points:
//!
//! - `qsl-sync` calls [`upsert_seen`] with `Source::Inbound` for
//!   every `From:` of every newly-inserted message. Deduplication
//!   is by the address column's `COLLATE NOCASE` constraint, so
//!   re-syncing the same message is idempotent.
//! - The desktop's `messages_send` Tauri command calls
//!   [`upsert_seen`] with `Source::Outbound` for every `To:` /
//!   `Cc:` / `Bcc:` of an outgoing draft.
//!
//! One read entry point: [`query_prefix`]. It returns up to
//! `limit` rows whose address or display_name starts with the
//! given prefix, ordered most-recent-first then most-popular-first.

use qsl_core::StorageError;

use crate::conn::{DbConn, Params, Value};

/// Where the address came from. Stored as a TEXT discriminator in
/// the `source` column so a future privacy toggle ("don't suggest
/// people I haven't written to") can filter on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Observed on the `From:` of an incoming message.
    Inbound,
    /// Observed on the `To:` / `Cc:` / `Bcc:` of an outgoing send.
    Outbound,
}

impl Source {
    fn as_str(self) -> &'static str {
        match self {
            Source::Inbound => "inbound",
            Source::Outbound => "outbound",
        }
    }
}

/// One row in the autocomplete dropdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contact {
    pub address: String,
    pub display_name: Option<String>,
    pub last_seen_at: i64,
    pub seen_count: i64,
}

/// Insert or refresh a row for `address`. Idempotent — the second
/// call increments `seen_count` and bumps `last_seen_at` rather
/// than failing on the primary-key conflict.
///
/// `display_name` is updated to the supplied value only when it's
/// non-empty; once a row has a real name attached it never reverts
/// to NULL on a subsequent sighting that came in without one.
/// Mailing-list senders frequently arrive with and without their
/// display name across messages; this rule means the dropdown
/// stabilizes on the first non-empty form rather than flickering.
///
/// `now_secs` is the unix-timestamp the caller wants written.
/// Taking it as an argument (instead of pulling `Utc::now()`
/// inside) keeps the function deterministic against tests, which
/// pass fixed timestamps to assert the `last_seen_at` ordering.
pub async fn upsert_seen(
    conn: &dyn DbConn,
    address: &str,
    display_name: Option<&str>,
    source: Source,
    now_secs: i64,
) -> Result<(), StorageError> {
    let trimmed = address.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let display_for_insert = display_name.filter(|s| !s.is_empty()).map(str::to_string);

    let sql = "
        INSERT INTO contacts_v1 (address, display_name, last_seen_at, seen_count, source)
        VALUES (?1, ?2, ?3, 1, ?4)
        ON CONFLICT(address) DO UPDATE SET
            -- Hold the most recent non-empty display name.
            -- COALESCE(NULLIF(?2, ''), display_name) keeps the
            -- existing value when the incoming one is empty / NULL.
            display_name  = COALESCE(NULLIF(?2, ''), display_name),
            last_seen_at  = ?3,
            seen_count    = seen_count + 1,
            -- Remember the latest source as well; the autocomplete
            -- doesn't filter on it today, but a future privacy
            -- toggle can.
            source        = ?4
    ";

    let display_value = match display_for_insert.as_deref() {
        Some(s) => Value::Text(s),
        None => Value::Null,
    };

    conn.execute(
        sql,
        Params(vec![
            Value::Text(trimmed),
            display_value,
            Value::Integer(now_secs),
            Value::Text(source.as_str()),
        ]),
    )
    .await
    .map(|_| ())
}

/// Look up the row for `address` (case-insensitive). Returns `None`
/// if no contact has been observed at this address yet.
pub async fn find(conn: &dyn DbConn, address: &str) -> Result<Option<Contact>, StorageError> {
    let sql = "
        SELECT address, display_name, last_seen_at, seen_count
          FROM contacts_v1
         WHERE address = ?1 COLLATE NOCASE
         LIMIT 1
    ";
    let row_opt = conn
        .query_opt(sql, Params(vec![Value::Text(address.trim())]))
        .await?;
    row_opt
        .map(|row| {
            Ok(Contact {
                address: row.get_str("address")?.to_string(),
                display_name: row.get_optional_str("display_name")?.map(str::to_string),
                last_seen_at: row.get_i64("last_seen_at")?,
                seen_count: row.get_i64("seen_count")?,
            })
        })
        .transpose()
}

/// Prefix search across `address` and `display_name`, ordered by
/// recency then popularity. Used by the compose pane's
/// `AddressField` dropdown.
///
/// Empty / whitespace-only `prefix` returns an empty list — the
/// dropdown only opens once the user has typed at least 2 chars,
/// but defending here costs nothing.
///
/// SQL `LIKE` with a literal `%` suffix is fine for prefix scans
/// against the `contacts_v1_address_idx` index; the planner uses
/// the index range when the pattern's leading character is fixed.
pub async fn query_prefix(
    conn: &dyn DbConn,
    prefix: &str,
    limit: u32,
) -> Result<Vec<Contact>, StorageError> {
    let trimmed = prefix.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let pattern = format!("{trimmed}%");

    let sql = "
        SELECT address, display_name, last_seen_at, seen_count
          FROM contacts_v1
         WHERE address LIKE ?1 COLLATE NOCASE
            OR (display_name IS NOT NULL AND display_name LIKE ?1 COLLATE NOCASE)
         ORDER BY last_seen_at DESC, seen_count DESC, address ASC
         LIMIT ?2
    ";

    let rows = conn
        .query(
            sql,
            Params(vec![
                Value::OwnedText(pattern),
                Value::Integer(i64::from(limit)),
            ]),
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(Contact {
                address: row.get_str("address")?.to_string(),
                display_name: row.get_optional_str("display_name")?.map(str::to_string),
                last_seen_at: row.get_i64("last_seen_at")?,
                seen_count: row.get_i64("seen_count")?,
            })
        })
        .collect()
}

/// Wipe every row in `contacts_v1`. Called by `accounts_remove` when
/// the last account is removed — without this, the autocomplete
/// table keeps every email address the user has ever corresponded
/// with even after the originating account is gone, which the user
/// reasonably reads as "identity information sticking around after
/// I deleted my account." Per-account scoping isn't possible here
/// because `contacts_v1` doesn't carry an `account_id` column
/// (schema design limit, see migration 0006); switching to a global
/// truncate-on-empty policy is the pragmatic shape.
pub async fn clear_all(conn: &dyn DbConn) -> Result<(), StorageError> {
    conn.execute("DELETE FROM contacts_v1", Params::empty())
        .await
        .map(|_| ())
}
