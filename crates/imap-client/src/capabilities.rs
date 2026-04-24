// SPDX-License-Identifier: Apache-2.0

//! IMAP capability enforcement.
//!
//! `DESIGN.md` §1 and §4.3 originally required CONDSTORE (RFC 7162)
//! for efficient flag sync, QRESYNC (also RFC 7162) for reconnect
//! resync, and IDLE (RFC 2177) for push. Gmail's IMAP service
//! advertises CONDSTORE + IDLE but **not** QRESYNC — it relies on
//! Gmail's proprietary `X-GM-EXT-1` (RFC-adjacent `X-GM-MSGID` and
//! `X-GM-THRID` server-stable IDs) for reconnect resync instead.
//! Since Gmail is one of the v1 target providers, we treat QRESYNC as
//! a soft dependency: required only when a server lacks both QRESYNC
//! and Gmail's extension family.
//!
//! Any server missing CONDSTORE or IDLE is still rejected outright;
//! there's no viable fallback for either.

use capytain_core::MailError;

/// Capabilities every supported IMAP server MUST advertise. Missing
/// any one of these is a hard connect-time failure.
pub const REQUIRED: &[&str] = &["CONDSTORE", "IDLE"];

/// At least one of these must be present for reconnect-resync to
/// work: standard QRESYNC (RFC 7162) or Gmail's `X-GM-EXT-1` (which
/// provides stable `X-GM-MSGID` IDs that deliver the same property
/// through a different mechanism).
pub const RESYNC_ANY: &[&str] = &["QRESYNC", "X-GM-EXT-1"];

/// Check the capability set and return a [`MailError::Protocol`] that
/// names each missing capability if any of the hard requirements are
/// absent, or if none of the `RESYNC_ANY` alternatives are present.
pub fn require(advertised: impl IntoIterator<Item = impl AsRef<str>>) -> Result<(), MailError> {
    let advertised: Vec<String> = advertised
        .into_iter()
        .map(|s| s.as_ref().to_ascii_uppercase())
        .collect();

    let mut missing: Vec<String> = REQUIRED
        .iter()
        .filter(|req| !advertised.iter().any(|c| c.as_str() == **req))
        .map(|s| (*s).to_string())
        .collect();

    if !RESYNC_ANY
        .iter()
        .any(|opt| advertised.iter().any(|c| c.as_str() == *opt))
    {
        missing.push(format!("one of {{{}}}", RESYNC_ANY.join(", ")));
    }

    if missing.is_empty() {
        Ok(())
    } else {
        Err(MailError::Protocol(format!(
            "server is missing required IMAP capabilities: {}",
            missing.join(", ")
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_when_all_required_present() {
        let advertised = ["IMAP4rev1", "CONDSTORE", "QRESYNC", "IDLE", "LIST-EXTENDED"];
        require(advertised).unwrap();
    }

    #[test]
    fn case_insensitive() {
        let advertised = ["condstore", "Qresync", "idle"];
        require(advertised).unwrap();
    }

    #[test]
    fn gmail_x_gm_ext_1_satisfies_resync() {
        // Gmail does not advertise QRESYNC but does advertise
        // X-GM-EXT-1 + CONDSTORE + IDLE. Both CONDSTORE and IDLE
        // are hard requirements; X-GM-EXT-1 serves as the QRESYNC
        // alternative (stable X-GM-MSGID for reconnect resync).
        let advertised = ["CONDSTORE", "IDLE", "X-GM-EXT-1"];
        require(advertised).unwrap();
    }

    #[test]
    fn flags_missing_both_resync_options() {
        let advertised = ["CONDSTORE", "IDLE"];
        let err = require(advertised).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("QRESYNC"), "{msg}");
        assert!(msg.contains("X-GM-EXT-1"), "{msg}");
    }

    #[test]
    fn flags_multiple_missing() {
        let advertised: [&str; 0] = [];
        let err = require(advertised).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("CONDSTORE"));
        assert!(msg.contains("QRESYNC"));
        assert!(msg.contains("IDLE"));
    }
}
