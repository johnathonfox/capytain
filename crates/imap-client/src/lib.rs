// SPDX-License-Identifier: Apache-2.0

//! Capytain IMAP adapter.
//!
//! Wraps `async-imap` and implements the `MailBackend` trait defined in
//! `capytain-core`. CONDSTORE, QRESYNC, and IDLE are required at connect time;
//! servers missing any of them are rejected with a clear error.
