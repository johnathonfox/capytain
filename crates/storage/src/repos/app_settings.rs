// SPDX-License-Identifier: Apache-2.0

//! Global key/value store for app-wide settings (theme, density,
//! "always load remote images" master toggle, etc.).
//!
//! Backs the Appearance / Privacy tabs of the Settings window. Keys
//! are short stable strings (`"theme"`, `"density"`,
//! `"remote_content.always_load"`); values are plain TEXT and the
//! caller is responsible for any per-key serialization.
//!
//! Per-account preferences (signature, notify-enabled) live on the
//! `accounts` row instead of here — they're 1:1 with the account so
//! a join-free read path beats an extra SELECT.

use qsl_core::StorageError;

use crate::conn::{DbConn, Params, Value};

const UPSERT: &str = "
    INSERT INTO app_settings_v1 (key, value)
    VALUES (?1, ?2)
    ON CONFLICT(key) DO UPDATE SET value = excluded.value
";

const SELECT_BY_KEY: &str = "SELECT value FROM app_settings_v1 WHERE key = ?1";

const DELETE_BY_KEY: &str = "DELETE FROM app_settings_v1 WHERE key = ?1";

/// Insert-or-update a key. Empty string is a legitimate value (callers
/// that want "missing" semantics should `delete` instead).
pub async fn set(conn: &dyn DbConn, key: &str, value: &str) -> Result<(), StorageError> {
    conn.execute(UPSERT, Params(vec![Value::Text(key), Value::Text(value)]))
        .await
        .map(|_| ())
}

/// Read a key. `None` means "no row" — the caller decides whether
/// that's a user-facing default or an error.
pub async fn get(conn: &dyn DbConn, key: &str) -> Result<Option<String>, StorageError> {
    let row = conn
        .query_opt(SELECT_BY_KEY, Params(vec![Value::Text(key)]))
        .await?;
    match row {
        Some(r) => Ok(Some(r.get_str("value")?.to_string())),
        None => Ok(None),
    }
}

/// Remove a key. A missing key is success — callers that need to
/// distinguish should `get` first.
pub async fn delete(conn: &dyn DbConn, key: &str) -> Result<(), StorageError> {
    conn.execute(DELETE_BY_KEY, Params(vec![Value::Text(key)]))
        .await
        .map(|_| ())
}
