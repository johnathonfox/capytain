// SPDX-License-Identifier: Apache-2.0

//! Repository layer: one module per domain type.
//!
//! Every function takes `&dyn DbConn` (or `&mut dyn Tx` for
//! transaction-scoped calls) and returns domain types from `qsl-core`.
//! Serialization of ancillary fields (address vecs, flag bitmaps, labels)
//! goes through [`json`]; JSON was picked over ad-hoc columns because it
//! keeps the schema stable under additions to the domain types.

mod json;

pub mod accounts;
pub mod app_settings;
pub mod attachments;
pub mod contacts;
pub mod drafts;
pub mod folders;
pub mod messages;
pub mod outbox;
pub mod remote_content_opt_ins;
pub mod search;
pub mod sync_states;
pub mod threads;
