// SPDX-License-Identifier: Apache-2.0

//! The `DbConn` abstraction — the one seam the repository layer depends on.
//!
//! Everything above this file sees `&dyn DbConn` and never imports `turso`.
//! The concrete implementation lives in [`crate::turso_conn`].
//!
//! The trait surface is intentionally small: `execute`, `query`, `query_one`,
//! `query_opt`, plus explicit `begin` → `commit` | `rollback` transactions.
//! There is no query-builder DSL; SQL lives as `const` strings in
//! [`crate::repos`].

use async_trait::async_trait;

use capytain_core::StorageError;

/// A set of positional parameters for a prepared statement.
///
/// Parameters bind in declaration order (SQLite `?1`, `?2`, …) — match the
/// order in your SQL. Construction is usually done with the [`params!`][crate::params] macro.
#[derive(Debug, Default, Clone)]
pub struct Params<'a>(pub Vec<Value<'a>>);

impl<'a> Params<'a> {
    /// Empty parameter list (for statements with no `?`-placeholders).
    pub fn empty() -> Self {
        Self(Vec::new())
    }
}

impl<'a, const N: usize> From<[Value<'a>; N]> for Params<'a> {
    fn from(values: [Value<'a>; N]) -> Self {
        Self(values.into_iter().collect())
    }
}

/// A single bind value. Borrowed variants are preferred in hot paths to
/// avoid allocations; owned variants exist for values constructed on the fly.
#[derive(Debug, Clone)]
pub enum Value<'a> {
    /// SQL `NULL`.
    Null,
    /// Signed 64-bit integer, matching SQLite's `INTEGER` storage class.
    Integer(i64),
    /// 64-bit IEEE float, matching SQLite's `REAL`.
    Real(f64),
    /// Borrowed UTF-8 text.
    Text(&'a str),
    /// Owned UTF-8 text.
    OwnedText(String),
    /// Borrowed byte slice.
    Blob(&'a [u8]),
    /// Owned byte buffer.
    OwnedBlob(Vec<u8>),
}

impl<'a> From<i64> for Value<'a> {
    fn from(v: i64) -> Self {
        Value::Integer(v)
    }
}

impl<'a> From<i32> for Value<'a> {
    fn from(v: i32) -> Self {
        Value::Integer(v.into())
    }
}

impl<'a> From<u32> for Value<'a> {
    fn from(v: u32) -> Self {
        Value::Integer(v.into())
    }
}

impl<'a> From<bool> for Value<'a> {
    fn from(v: bool) -> Self {
        Value::Integer(v.into())
    }
}

impl<'a> From<f64> for Value<'a> {
    fn from(v: f64) -> Self {
        Value::Real(v)
    }
}

impl<'a> From<&'a str> for Value<'a> {
    fn from(v: &'a str) -> Self {
        Value::Text(v)
    }
}

impl<'a> From<String> for Value<'a> {
    fn from(v: String) -> Self {
        Value::OwnedText(v)
    }
}

impl<'a> From<&'a [u8]> for Value<'a> {
    fn from(v: &'a [u8]) -> Self {
        Value::Blob(v)
    }
}

impl<'a> From<Vec<u8>> for Value<'a> {
    fn from(v: Vec<u8>) -> Self {
        Value::OwnedBlob(v)
    }
}

impl<'a, T> From<Option<T>> for Value<'a>
where
    T: Into<Value<'a>>,
{
    fn from(v: Option<T>) -> Self {
        match v {
            Some(inner) => inner.into(),
            None => Value::Null,
        }
    }
}

/// A materialized result row.
///
/// Rows are returned eagerly as name → value pairs so callers can access
/// columns by name without threading `Statement` metadata through the API.
/// For mail workloads the column count per query is small (tens at most),
/// so linear lookup is fine.
#[derive(Debug)]
pub struct Row {
    columns: Vec<(String, OwnedValue)>,
}

/// The value types a [`Row`] can hold. Symmetric to [`Value`] but always
/// owned — rows outlive any statement borrow.
#[derive(Debug, Clone)]
pub enum OwnedValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl Row {
    /// Construct a row from its column list. Used by [`crate::turso_conn`]
    /// when materializing `turso::Row` into our API.
    pub fn from_columns(columns: Vec<(String, OwnedValue)>) -> Self {
        Self { columns }
    }

    fn get(&self, col: &str) -> Result<&OwnedValue, StorageError> {
        self.columns
            .iter()
            .find(|(name, _)| name == col)
            .map(|(_, v)| v)
            .ok_or_else(|| StorageError::Db(format!("no such column: {col}")))
    }

    /// True if the named column is present and non-NULL.
    pub fn has_value(&self, col: &str) -> Result<bool, StorageError> {
        Ok(!matches!(self.get(col)?, OwnedValue::Null))
    }

    /// Borrow the column as an i64. Returns [`StorageError::Db`] if the
    /// column is missing, NULL, or holds a different type.
    pub fn get_i64(&self, col: &str) -> Result<i64, StorageError> {
        match self.get(col)? {
            OwnedValue::Integer(i) => Ok(*i),
            other => Err(StorageError::Db(format!(
                "column {col} is {other:?}, expected Integer"
            ))),
        }
    }

    /// Borrow the column as an optional i64. Returns `Ok(None)` for NULL.
    pub fn get_optional_i64(&self, col: &str) -> Result<Option<i64>, StorageError> {
        match self.get(col)? {
            OwnedValue::Null => Ok(None),
            OwnedValue::Integer(i) => Ok(Some(*i)),
            other => Err(StorageError::Db(format!(
                "column {col} is {other:?}, expected Integer or NULL"
            ))),
        }
    }

    /// Borrow the column as an f64.
    pub fn get_f64(&self, col: &str) -> Result<f64, StorageError> {
        match self.get(col)? {
            OwnedValue::Real(v) => Ok(*v),
            OwnedValue::Integer(i) => Ok(*i as f64),
            other => Err(StorageError::Db(format!(
                "column {col} is {other:?}, expected Real"
            ))),
        }
    }

    /// Borrow the column as a `&str`.
    pub fn get_str(&self, col: &str) -> Result<&str, StorageError> {
        match self.get(col)? {
            OwnedValue::Text(s) => Ok(s.as_str()),
            other => Err(StorageError::Db(format!(
                "column {col} is {other:?}, expected Text"
            ))),
        }
    }

    /// Borrow the column as an optional `&str`.
    pub fn get_optional_str(&self, col: &str) -> Result<Option<&str>, StorageError> {
        match self.get(col)? {
            OwnedValue::Null => Ok(None),
            OwnedValue::Text(s) => Ok(Some(s.as_str())),
            other => Err(StorageError::Db(format!(
                "column {col} is {other:?}, expected Text or NULL"
            ))),
        }
    }

    /// Borrow the column as a byte slice.
    pub fn get_blob(&self, col: &str) -> Result<&[u8], StorageError> {
        match self.get(col)? {
            OwnedValue::Blob(b) => Ok(b.as_slice()),
            other => Err(StorageError::Db(format!(
                "column {col} is {other:?}, expected Blob"
            ))),
        }
    }

    /// True if the column list is empty (degenerate case for tests).
    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }
}

/// Async database connection.
///
/// All repository code depends on `&dyn DbConn` — no concrete driver types
/// leak above this trait. Implementations must be `Send + Sync` because the
/// sync engine shares a handle across tokio tasks.
#[async_trait]
pub trait DbConn: Send + Sync {
    /// Run a non-SELECT statement; returns the number of rows affected.
    async fn execute(&self, sql: &str, params: Params<'_>) -> Result<u64, StorageError>;

    /// Run a SELECT and materialize every row.
    async fn query(&self, sql: &str, params: Params<'_>) -> Result<Vec<Row>, StorageError>;

    /// Run a SELECT that must return exactly one row.
    ///
    /// Returns [`StorageError::NotFound`] if zero rows come back and
    /// [`StorageError::Db`] if more than one does.
    async fn query_one(&self, sql: &str, params: Params<'_>) -> Result<Row, StorageError>;

    /// Run a SELECT that may return zero or one row.
    ///
    /// Returns [`StorageError::Db`] if more than one row comes back.
    async fn query_opt(&self, sql: &str, params: Params<'_>) -> Result<Option<Row>, StorageError>;

    /// Begin a transaction. Call [`Tx::commit`] or [`Tx::rollback`] to
    /// terminate it; dropping the transaction without either is treated as
    /// a rollback by the backend.
    async fn begin<'a>(&'a self) -> Result<Box<dyn Tx + 'a>, StorageError>;
}

/// Active transaction. All statements within a [`Tx`] run on the same
/// underlying connection and see each other's writes before commit.
#[async_trait]
pub trait Tx: Send {
    async fn execute(&mut self, sql: &str, params: Params<'_>) -> Result<u64, StorageError>;
    async fn query(&mut self, sql: &str, params: Params<'_>) -> Result<Vec<Row>, StorageError>;

    /// Commit the transaction. The `self: Box<Self>` signature consumes the
    /// handle so post-commit misuse is impossible.
    async fn commit(self: Box<Self>) -> Result<(), StorageError>;

    /// Roll back the transaction.
    async fn rollback(self: Box<Self>) -> Result<(), StorageError>;
}
