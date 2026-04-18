// SPDX-License-Identifier: Apache-2.0

//! IMAP capability enforcement.
//!
//! `DESIGN.md` §1 and §4.3 require CONDSTORE (RFC 7162) for efficient
//! flag sync, QRESYNC (also RFC 7162) for reconnect resync, and IDLE
//! (RFC 2177) for push. Any server that doesn't advertise all three is
//! rejected at connect time with a clear error — not degraded, not
//! falling back to POLL.

use capytain_core::MailError;

pub const REQUIRED: &[&str] = &["CONDSTORE", "QRESYNC", "IDLE"];

/// Check the capability set and return a [`MailError::Protocol`] that
/// names each missing capability if any are absent.
pub fn require(advertised: impl IntoIterator<Item = impl AsRef<str>>) -> Result<(), MailError> {
    let advertised: Vec<String> = advertised
        .into_iter()
        .map(|s| s.as_ref().to_ascii_uppercase())
        .collect();

    let missing: Vec<&&str> = REQUIRED
        .iter()
        .filter(|req| !advertised.iter().any(|c| c.as_str() == **req))
        .collect();

    if missing.is_empty() {
        Ok(())
    } else {
        let names: Vec<String> = missing.iter().map(|s| (*s).to_string()).collect();
        Err(MailError::Protocol(format!(
            "server is missing required IMAP capabilities: {}",
            names.join(", ")
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
    fn flags_missing_qresync() {
        let advertised = ["CONDSTORE", "IDLE"];
        let err = require(advertised).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("QRESYNC"), "{msg}");
        assert!(!msg.contains("CONDSTORE"), "{msg}");
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
