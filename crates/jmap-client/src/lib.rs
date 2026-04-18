// SPDX-License-Identifier: Apache-2.0

//! Capytain JMAP adapter.
//!
//! Wraps `jmap-client` and implements the `MailBackend` trait defined in
//! `capytain-core`. Uses server-provided state strings plus `Email/changes`
//! for delta sync and EventSource for push.
