// SPDX-License-Identifier: Apache-2.0

//! QSL authentication.
//!
//! OAuth2 with PKCE is the only supported auth path (per `DESIGN.md` §1).
//! This crate exposes:
//!
//! - [`pkce`] — verifier + challenge primitives (RFC 7636).
//! - [`provider`] — [`OAuthProvider`] trait and the built-in Gmail /
//!   Fastmail profiles.
//! - [`loopback`] — a minimal tokio-based HTTP server that catches the
//!   `http://127.0.0.1:<port>/` redirect with `?code=…&state=…`.
//! - [`flow`] — the high-level orchestrator: `run_loopback_flow(…)` opens
//!   the browser, awaits the redirect, and exchanges the code for tokens.
//! - [`keyring`] — [`TokenVault`], one refresh-token entry per account
//!   under the `com.qsl.app` keychain service.
//! - [`refresh`] — helper that returns a valid access token, refreshing
//!   via the stored refresh token when needed.
//!
//! No password code path exists anywhere in this crate or its
//! dependencies — per `DESIGN.md` §1 and §2, that's a hard rule.

pub mod error;
pub mod flow;
pub mod keyring;
pub mod loopback;
pub mod pkce;
pub mod provider;
pub mod providers;
pub mod refresh;
pub mod tokens;

pub use error::AuthError;
pub use flow::{run_loopback_flow, run_loopback_flow_with, FlowOutcome};
pub use keyring::TokenVault;
pub use provider::{lookup, OAuthProvider, ProviderKind, ProviderProfile};
pub use refresh::{access_token_for, refresh_access_token};
pub use tokens::{AccessToken, RefreshToken, TokenSet};
