// SPDX-License-Identifier: Apache-2.0

//! SASL XOAUTH2 authenticator for `async-imap::Client::authenticate`.
//!
//! Wire format per Google / Microsoft / RFC 7628:
//!
//! ```text
//! user=<email>\x01auth=Bearer <access_token>\x01\x01
//! ```
//!
//! The whole string is base64-encoded when sent over the wire. async-imap's
//! `Authenticator` trait is called with the server challenge (empty for
//! XOAUTH2) and asks us for the response bytes; we return the pre-encoded
//! challenge payload and the library base64-wraps it for us.

use async_imap::Authenticator;

/// XOAUTH2 authenticator. Construct once per connection attempt.
pub struct XOAuth2 {
    email: String,
    access_token: String,
}

impl XOAuth2 {
    pub fn new(email: impl Into<String>, access_token: impl Into<String>) -> Self {
        Self {
            email: email.into(),
            access_token: access_token.into(),
        }
    }
}

impl Authenticator for &XOAuth2 {
    type Response = String;

    fn process(&mut self, _challenge: &[u8]) -> Self::Response {
        // Note: the server sends an empty challenge for XOAUTH2's initial
        // response; `async-imap` calls `process` once and uses the return
        // value as the SASL initial response (already base64-encoded by
        // async-imap).
        format!(
            "user={}\x01auth=Bearer {}\x01\x01",
            self.email, self.access_token
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_rfc_7628_payload() {
        let auth = XOAuth2::new("me@example.com", "tok");
        let mut r = &auth;
        let resp = r.process(b"");
        assert_eq!(resp, "user=me@example.com\x01auth=Bearer tok\x01\x01");
    }
}
