// SPDX-License-Identifier: Apache-2.0

//! Full-text search over `messages`, backed by Turso's experimental
//! Tantivy-powered FTS index (created in migration `0005_search_fts.sql`).
//!
//! The index covers `subject`, `from_json`, `to_json`, `snippet` and
//! is auto-maintained by the engine on every insert / update / delete
//! to the `messages` table — no write-side hooks live in this crate.
//! The repo exposes only the query path:
//!
//! - [`search_ids`] runs a Tantivy query string against the index
//!   and returns the matching `MessageId`s in BM25 rank order.
//!
//! Per the Turso 0.5.3 manual, FTS query results inside a
//! transaction don't see uncommitted writes (no read-your-writes).
//! That's a non-issue for the IPC search path because every search
//! command runs outside any transaction; the only place it matters
//! is unit tests, which insert via auto-commit before querying.

use qsl_core::{MessageId, StorageError};

use crate::conn::{DbConn, Params, Value};

/// Run a Tantivy FTS query and return matching `MessageId`s, best
/// match first, paginated by `limit` / `offset`.
///
/// `query` is the raw Tantivy query syntax — single terms, AND/OR,
/// quoted phrases, prefix `data*`, column scoping `subject:foo`,
/// boosting `subject:foo^2`. The Gmail-style operator parser
/// (`from:`, `subject:`, `is:unread`, …) lives a layer above this
/// in `qsl-search` (PR-S2) and translates to the Tantivy syntax
/// before calling here.
///
/// Implementation note: we use the table-level `fts_match(...)` /
/// `fts_score(...)` functions documented in the Turso manual, NOT
/// the SQLite `WHERE table MATCH '...'` form (the manual explicitly
/// says that operator is not supported). The functions take the
/// same column list the index was created over plus the query
/// string — keeping the column set in sync with migration 0005 is
/// the only correctness obligation; if a future migration adds a
/// column, both the index DDL and this query need to grow.
pub async fn search_ids(
    conn: &dyn DbConn,
    query: &str,
    limit: u32,
    offset: u32,
) -> Result<Vec<MessageId>, StorageError> {
    let sql = "
        SELECT id
          FROM messages
         WHERE fts_match(subject, from_json, to_json, snippet, ?1)
         ORDER BY fts_score(subject, from_json, to_json, snippet, ?1) DESC,
                  date DESC,
                  id ASC
         LIMIT ?2 OFFSET ?3
    ";
    let rows = conn
        .query(
            sql,
            Params(vec![
                Value::Text(query),
                Value::Integer(i64::from(limit)),
                Value::Integer(i64::from(offset)),
            ]),
        )
        .await?;
    rows.iter()
        .map(|r| Ok(MessageId(r.get_str("id")?.to_string())))
        .collect()
}
