// SPDX-License-Identifier: Apache-2.0

//! Built-in provider profiles. One module per provider; each exposes a
//! `pub static` that implements [`crate::OAuthProvider`].

pub mod fastmail;
pub mod gmail;
