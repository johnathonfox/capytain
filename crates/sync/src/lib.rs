// SPDX-License-Identifier: Apache-2.0

//! Capytain sync engine.
//!
//! Orchestrates offline-first sync against any `MailBackend`: local optimistic
//! mutations, an outbox that replays against the server, conflict
//! reconciliation, and client-side threading (References-chain plus
//! subject-normalization hybrid).
