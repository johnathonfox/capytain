// SPDX-License-Identifier: Apache-2.0

//! Turso-backed implementation of [`DbConn`].
//!
//! This module is the one place in `qsl-storage` that knows about
//! `turso::*` types. Everything above it sees only the trait surface in
//! [`crate::conn`].
//!
//! # A note on transactions
//!
//! Turso 0.5.3 ships `pub struct Transaction {}` as an empty placeholder —
//! no `commit`, no `rollback`, no `execute`. The real transaction API is a
//! planned-but-unimplemented feature upstream. We work around this by
//! issuing raw `BEGIN` / `COMMIT` / `ROLLBACK` statements on the same
//! connection, which is the SQLite-level behavior regardless of whether a
//! higher-level API wraps it. Tracked in `docs/dependencies/turso.md`.

use async_trait::async_trait;
use turso::params::params_from_iter;

use qsl_core::StorageError;

use crate::conn::{DbConn, OwnedValue, Params, Row, Tx, Value};

/// A live handle to a Turso database.
pub struct TursoConn {
    conn: turso::Connection,
}

impl TursoConn {
    /// Wrap an already-built `turso::Connection`.
    pub fn new(conn: turso::Connection) -> Self {
        Self { conn }
    }

    /// Open an in-memory database. Primarily used by tests.
    pub async fn in_memory() -> Result<Self, StorageError> {
        let db = turso::Builder::new_local(":memory:")
            .build()
            .await
            .map_err(err_db)?;
        let conn = db.connect().map_err(err_db)?;
        Ok(Self::new(conn))
    }

    /// Open (or create) a database file at `path`.
    pub async fn open<P: AsRef<std::path::Path>>(path: P) -> Result<Self, StorageError> {
        let path = path
            .as_ref()
            .to_str()
            .ok_or_else(|| StorageError::Db("db path is not valid UTF-8".into()))?;
        let db = turso::Builder::new_local(path)
            .build()
            .await
            .map_err(err_db)?;
        let conn = db.connect().map_err(err_db)?;
        Ok(Self::new(conn))
    }

    /// Borrow the underlying `turso::Connection` for Turso-specific
    /// operations (PRAGMAs, cache flush). Avoid if possible — going through
    /// [`DbConn`] keeps the abstraction intact.
    pub fn inner(&self) -> &turso::Connection {
        &self.conn
    }
}

#[async_trait]
impl DbConn for TursoConn {
    async fn execute(&self, sql: &str, params: Params<'_>) -> Result<u64, StorageError> {
        self.conn
            .execute(sql, params_from_iter(to_turso_values(params)))
            .await
            .map_err(err_db)
    }

    async fn query(&self, sql: &str, params: Params<'_>) -> Result<Vec<Row>, StorageError> {
        let mut rows = self
            .conn
            .query(sql, params_from_iter(to_turso_values(params)))
            .await
            .map_err(err_db)?;
        materialize_rows(&mut rows).await
    }

    async fn query_one(&self, sql: &str, params: Params<'_>) -> Result<Row, StorageError> {
        let mut rows = self.query(sql, params).await?;
        match rows.len() {
            0 => Err(StorageError::NotFound),
            1 => Ok(rows.remove(0)),
            n => Err(StorageError::Db(format!(
                "query_one expected 1 row, got {n}"
            ))),
        }
    }

    async fn query_opt(&self, sql: &str, params: Params<'_>) -> Result<Option<Row>, StorageError> {
        let mut rows = self.query(sql, params).await?;
        match rows.len() {
            0 => Ok(None),
            1 => Ok(Some(rows.remove(0))),
            n => Err(StorageError::Db(format!(
                "query_opt expected 0 or 1 rows, got {n}"
            ))),
        }
    }

    async fn begin<'a>(&'a self) -> Result<Box<dyn Tx + 'a>, StorageError> {
        // See module docs — Turso 0.5.3 doesn't implement its Transaction
        // wrapper yet, so we drive the transaction at the SQL level.
        self.conn
            .execute("BEGIN", turso::params::IntoParams::into_params(()).unwrap())
            .await
            .map_err(err_db)?;
        Ok(Box::new(TursoTx {
            conn: &self.conn,
            finished: false,
        }))
    }
}

/// Transaction handle that dispatches through the shared connection.
///
/// Because Turso's `Connection` is serialized internally, intermixing direct
/// `DbConn::execute` calls on the parent handle with calls on an active
/// `TursoTx` will route them all through the same in-flight transaction.
/// Callers should treat `Tx::execute` as the canonical path while a tx is
/// open.
struct TursoTx<'a> {
    conn: &'a turso::Connection,
    finished: bool,
}

#[async_trait]
impl<'a> Tx for TursoTx<'a> {
    async fn execute(&mut self, sql: &str, params: Params<'_>) -> Result<u64, StorageError> {
        self.conn
            .execute(sql, params_from_iter(to_turso_values(params)))
            .await
            .map_err(err_db)
    }

    async fn query(&mut self, sql: &str, params: Params<'_>) -> Result<Vec<Row>, StorageError> {
        let mut rows = self
            .conn
            .query(sql, params_from_iter(to_turso_values(params)))
            .await
            .map_err(err_db)?;
        materialize_rows(&mut rows).await
    }

    async fn commit(mut self: Box<Self>) -> Result<(), StorageError> {
        self.conn
            .execute(
                "COMMIT",
                turso::params::IntoParams::into_params(()).unwrap(),
            )
            .await
            .map_err(err_db)?;
        self.finished = true;
        Ok(())
    }

    async fn rollback(mut self: Box<Self>) -> Result<(), StorageError> {
        self.conn
            .execute(
                "ROLLBACK",
                turso::params::IntoParams::into_params(()).unwrap(),
            )
            .await
            .map_err(err_db)?;
        self.finished = true;
        Ok(())
    }
}

impl Drop for TursoTx<'_> {
    fn drop(&mut self) {
        // Sync `Drop` can't await, so we can't issue ROLLBACK here. Leaving
        // a BEGIN pending would wedge the connection — warn loudly so the
        // bug surfaces in tests.
        if !self.finished {
            tracing::warn!(
                target: "qsl_storage::tx",
                "TursoTx dropped without commit() or rollback(); the underlying \
                 connection still has a BEGIN in flight. This is a storage-layer bug."
            );
        }
    }
}

fn to_turso_values(params: Params<'_>) -> Vec<turso::Value> {
    params.0.into_iter().map(to_turso_value).collect()
}

fn to_turso_value(v: Value<'_>) -> turso::Value {
    match v {
        Value::Null => turso::Value::Null,
        Value::Integer(i) => turso::Value::Integer(i),
        Value::Real(f) => turso::Value::Real(f),
        Value::Text(s) => turso::Value::Text(s.to_string()),
        Value::OwnedText(s) => turso::Value::Text(s),
        Value::Blob(b) => turso::Value::Blob(b.to_vec()),
        Value::OwnedBlob(b) => turso::Value::Blob(b),
    }
}

async fn materialize_rows(rows: &mut turso::Rows) -> Result<Vec<Row>, StorageError> {
    let column_names = rows.column_names();
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.map_err(err_db)? {
        let mut cols = Vec::with_capacity(column_names.len());
        for (i, name) in column_names.iter().enumerate() {
            let v = row.get_value(i).map_err(err_db)?;
            cols.push((name.clone(), owned_from_turso(v)));
        }
        out.push(Row::from_columns(cols));
    }
    Ok(out)
}

fn owned_from_turso(v: turso::Value) -> OwnedValue {
    match v {
        turso::Value::Null => OwnedValue::Null,
        turso::Value::Integer(i) => OwnedValue::Integer(i),
        turso::Value::Real(f) => OwnedValue::Real(f),
        turso::Value::Text(s) => OwnedValue::Text(s),
        turso::Value::Blob(b) => OwnedValue::Blob(b),
    }
}

fn err_db<E: std::fmt::Display>(e: E) -> StorageError {
    StorageError::Db(e.to_string())
}
