// SPDX-License-Identifier: Apache-2.0

//! Capytain core: domain types, error enums, and protocol-agnostic traits.
//!
//! No I/O happens here. Types defined in this crate are shared across every
//! other crate in the workspace. See `TRAITS.md` for the full type catalogue.
//!
//! # Module layout
//!
//! - [`error`] — `MailError` and `StorageError`.
//! - [`ids`] — opaque newtype wrappers for every ID the core traffics in.
//! - [`account`] — [`Account`] and [`BackendKind`].
//! - [`folder`] — [`Folder`] and [`FolderRole`].
//! - [`message`] — `EmailAddress`, `MessageFlags`, `MessageHeaders`,
//!   `MessageBody`, `Attachment`.
//! - [`sync_state`] — [`SyncState`] (opaque per-folder sync cursor).
//!
//! Traits that depend on async runtimes (`MailBackend`, `EmailRenderer`) land
//! in a later Phase 0 week alongside the crates that implement them.

pub mod account;
pub mod error;
pub mod folder;
pub mod ids;
pub mod message;
pub mod sync_state;

pub use account::{Account, BackendKind};
pub use error::{MailError, StorageError};
pub use folder::{Folder, FolderRole};
pub use ids::{AccountId, AttachmentRef, DraftId, FolderId, MessageId, ThreadId};
pub use message::{Attachment, EmailAddress, MessageBody, MessageFlags, MessageHeaders};
pub use sync_state::SyncState;
