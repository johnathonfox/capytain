// SPDX-License-Identifier: Apache-2.0

//! QSL core: domain types, error enums, and protocol-agnostic traits.
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
//! - [`renderer`] — [`EmailRenderer`] trait and [`NullRenderer`] test double.
//!   The Servo-backed implementation lives in `qsl-renderer`.

pub mod account;
pub mod draft;
pub mod error;
pub mod folder;
pub mod ids;
pub mod mail_backend;
pub mod message;
pub mod renderer;
pub mod sync_state;

pub use account::{Account, BackendKind};
pub use draft::{Draft, DraftAttachment, DraftBodyKind};
pub use error::{MailError, StorageError};
pub use folder::{Folder, FolderRole};
pub use ids::{AccountId, AttachmentRef, DraftId, FolderId, MessageId, ThreadId};
pub use mail_backend::{BackendEvent, MailBackend, MessageList};
pub use message::{Attachment, EmailAddress, MessageBody, MessageFlags, MessageHeaders};
pub use renderer::{ColorScheme, EmailRenderer, NullRenderer, RenderHandle, RenderPolicy};
pub use sync_state::SyncState;
