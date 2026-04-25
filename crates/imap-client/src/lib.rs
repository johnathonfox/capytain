// SPDX-License-Identifier: Apache-2.0

//! Capytain IMAP adapter — [`MailBackend`] implementation over
//! `async-imap`.
//!
//! This crate bridges the IMAP protocol and Capytain's
//! protocol-agnostic mail abstraction. At construction time the backend
//! is handed a fresh OAuth2 access token (minted by `capytain-auth`) and
//! a TLS stream; it then:
//!
//! 1. Authenticates via SASL XOAUTH2.
//! 2. Fetches capabilities and enforces that CONDSTORE, QRESYNC, and
//!    IDLE are all advertised. Servers missing any of them are
//!    rejected with a clear error — per `DESIGN.md` §1 and `PHASE_0.md`
//!    Week 4 Day 2.
//! 3. Delegates subsequent `MailBackend` calls to the appropriate IMAP
//!    commands.
//!
//! Phase 0 Week 4 ships the **read** path (`list_folders`,
//! `list_messages`, `fetch_message`). The write-side methods
//! (`update_flags`, `move_messages`, `delete_messages`, `save_draft`,
//! `submit_message`) return `MailError::Other("not yet implemented")`
//! until Phase 1 Week 2.
//!
//! `watch()` stays at the trait's default empty stream until IDLE is
//! wired in Phase 1 Week 1.

pub mod auth;
pub mod backend;
pub mod capabilities;
pub mod idle;
pub mod sync_state;

pub use backend::{dial_session, ImapBackend, StreamT};
pub use idle::watch_folder;
pub use sync_state::BackendState;
