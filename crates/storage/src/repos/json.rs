// SPDX-License-Identifier: Apache-2.0

//! Helpers that round-trip `serde`-serializable values through text columns.

use qsl_core::StorageError;
use serde::{de::DeserializeOwned, Serialize};

pub(super) fn encode<T: Serialize>(value: &T) -> Result<String, StorageError> {
    serde_json::to_string(value).map_err(|e| StorageError::Serde(e.to_string()))
}

pub(super) fn decode<T: DeserializeOwned>(raw: &str) -> Result<T, StorageError> {
    serde_json::from_str(raw).map_err(|e| StorageError::Serde(e.to_string()))
}
