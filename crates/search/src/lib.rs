// SPDX-License-Identifier: Apache-2.0

//! QSL full-text search.
//!
//! Wraps Tantivy to index message headers, body text, and attachment filenames
//! across every account behind a single query surface.
