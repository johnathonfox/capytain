// SPDX-License-Identifier: Apache-2.0

//! Capytain authentication.
//!
//! OAuth2 with PKCE is the only supported auth path. Built-in provider
//! profiles live under `providers/`. Refresh tokens are persisted via the
//! `keyring` crate; no password code path exists anywhere in the workspace.
