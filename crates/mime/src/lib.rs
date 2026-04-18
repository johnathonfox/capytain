// SPDX-License-Identifier: Apache-2.0

//! Capytain MIME helpers.
//!
//! Thin wrappers over `mail-parser` and `mail-builder` that present Capytain
//! domain types (`MessageHeaders`, `MessageBody`, `Attachment`) to callers and
//! keep the underlying parser crates out of the public surface.
