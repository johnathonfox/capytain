// SPDX-License-Identifier: Apache-2.0

//! Full-text search over `messages`, backed by Turso's experimental
//! Tantivy-powered FTS index (created in migration `0005_search_fts.sql`).
//!
//! The index covers `subject`, `from_json`, `to_json`, `snippet` and
//! is auto-maintained by the engine on every insert / update / delete
//! to the `messages` table — no write-side hooks live in this crate.
//! The repo exposes two query entry points:
//!
//! - [`search_ids`] — raw Tantivy query string.
//! - [`search_with_query`] — typed `qsl_search::Query` from the
//!   Gmail-style operator parser, combining FTS predicates with
//!   structured filters (date range, unread flag, labels) the FTS
//!   index can't express on its own.
//!
//! Per the Turso 0.5.3 manual, FTS query results inside a
//! transaction don't see uncommitted writes (no read-your-writes).
//! That's a non-issue for the IPC search path because every search
//! command runs outside any transaction; the only place it matters
//! is unit tests, which insert via auto-commit before querying.

use qsl_core::{MessageId, StorageError};
use qsl_search::Query;

use crate::conn::{DbConn, Params, Value};

/// Run a Tantivy FTS query and return matching `MessageId`s, best
/// match first, paginated by `limit` / `offset`.
///
/// `query` is the raw Tantivy query syntax — single terms, AND/OR,
/// quoted phrases, prefix `data*`, column scoping `subject:foo`,
/// boosting `subject:foo^2`. Higher-level callers should prefer
/// [`search_with_query`], which accepts a parsed `qsl_search::Query`
/// and adds structured-filter clauses (date, unread, label).
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

/// Run a parsed `qsl_search::Query` and return matching
/// `MessageId`s, paginated.
///
/// Three SQL shapes are produced depending on the query:
///
///   1. Both FTS and structured predicates — `WHERE
///      fts_match(...) AND <structured>` ordered by `fts_score`.
///   2. FTS only — same as [`search_ids`], factored through here so
///      the IPC path has one entry point.
///   3. Structured only — plain SELECT with no `fts_match`,
///      ordered by `date DESC`. Lets the user filter "is:unread
///      before:2026-01-01" without needing to type any free-text
///      term.
///
/// An entirely empty query (no predicates) returns `Ok(vec![])`
/// rather than running an unbounded scan; the IPC layer should
/// surface that as "no results yet" rather than "every message".
pub async fn search_with_query(
    conn: &dyn DbConn,
    q: &Query,
    limit: u32,
    offset: u32,
) -> Result<Vec<MessageId>, StorageError> {
    if q.is_empty() {
        return Ok(Vec::new());
    }

    let mut sql = String::from("SELECT id FROM messages WHERE ");
    let mut params: Vec<Value<'_>> = Vec::new();
    let mut next_param: usize = 1;
    let mut clauses: Vec<String> = Vec::new();

    // Owned strings for the parameter slots — `Value::Text` borrows,
    // so the param values have to outlive the `params` vec.
    let tantivy = q.to_tantivy_string();
    let label_pattern = q.label.as_ref().map(|l| format!("%\"{l}\"%"));

    let has_fts = tantivy.is_some();
    if let Some(ref s) = tantivy {
        clauses.push(format!(
            "fts_match(subject, from_json, to_json, snippet, ?{n})",
            n = next_param
        ));
        params.push(Value::Text(s));
        next_param += 1;
    }
    if let Some(unread) = q.is_unread {
        // `flags_json.seen` is the canonical "read" bit. unread =
        // seen=false; read = seen=true. Match the COALESCE pattern
        // count_unread_by_folder uses so a missing column is treated
        // as not-seen rather than crashing.
        let want_seen = if unread { "0" } else { "1" };
        clauses.push(format!(
            "COALESCE(json_extract(flags_json, '$.seen'), 0) = {want_seen}"
        ));
    }
    if let Some(has_attach) = q.has_attachment {
        clauses.push(format!("has_attachments = {}", i64::from(has_attach)));
    }
    if let Some(before) = q.before {
        clauses.push(format!("date < ?{n}", n = next_param));
        params.push(Value::Integer(before.timestamp()));
        next_param += 1;
    }
    if let Some(after) = q.after {
        clauses.push(format!("date >= ?{n}", n = next_param));
        params.push(Value::Integer(after.timestamp()));
        next_param += 1;
    }
    if let Some(ref pat) = label_pattern {
        // labels_json is a JSON array string like `["foo","bar"]`.
        // A LIKE on the literal `"label"` form catches the row
        // without needing JSON_EACH (Turso 0.5.3's JSON support is
        // partial; LIKE is universally available). False positives
        // require the user to label something `\"foo\"` literally,
        // which is extremely rare.
        clauses.push(format!("labels_json LIKE ?{n}", n = next_param));
        params.push(Value::Text(pat));
        next_param += 1;
    }

    debug_assert!(
        !clauses.is_empty(),
        "is_empty() guard above should have caught this case"
    );
    sql.push_str(&clauses.join(" AND "));

    if has_fts {
        // `tantivy` is borrowed by `params[0]`; reuse the same
        // placeholder here so we don't double-bind.
        sql.push_str(
            " ORDER BY fts_score(subject, from_json, to_json, snippet, ?1) DESC, date DESC, id ASC",
        );
    } else {
        sql.push_str(" ORDER BY date DESC, id ASC");
    }
    sql.push_str(&format!(
        " LIMIT ?{l} OFFSET ?{o}",
        l = next_param,
        o = next_param + 1
    ));
    params.push(Value::Integer(i64::from(limit)));
    params.push(Value::Integer(i64::from(offset)));

    let rows = conn.query(&sql, Params(params)).await?;
    rows.iter()
        .map(|r| Ok(MessageId(r.get_str("id")?.to_string())))
        .collect()
}
