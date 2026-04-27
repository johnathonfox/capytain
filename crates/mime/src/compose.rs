// SPDX-License-Identifier: Apache-2.0

//! RFC 5322 message assembly for outgoing mail.
//!
//! Phase 2 Week 18 turns a [`Draft`] (the local compose-pane shape) plus
//! the user's `From` identity into a sendable byte stream. SMTP submission
//! (Week 18) and JMAP `EmailSubmission/set` (Week 19) both consume the
//! same RFC 5322 representation produced here.
//!
//! The Message-ID is **minted at build time** and returned alongside the
//! bytes so the desktop side can persist it on the outbox row. After
//! submission, the IDLE / EventSource push surfaces a server-side Sent
//! copy; matching by `Message-ID` is the reconciliation key called out
//! in `PHASE_2.md`'s open-questions section.
//!
//! Phase 2 Week 17's compose UX only writes `text/plain` bodies, so this
//! module ships a single-part `text/plain; charset=utf-8` builder and
//! defers the `multipart/alternative` (markdown → HTML) work to Week 20
//! where it lives next to the renderer's HTML sanitizer.

use std::time::{SystemTime, UNIX_EPOCH};

use mail_builder::{
    headers::{address::Address, message_id::MessageId, text::Text},
    MessageBuilder,
};
use rand::RngCore;
use thiserror::Error;

use qsl_core::{Draft, EmailAddress};

/// Successful build output. The Message-ID is angle-bracket-wrapped to
/// match the rest of the workspace's convention (`qsl-mime` parses
/// inbound headers with the brackets included).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltMessage {
    /// RFC 5322-formatted message bytes, ready for SMTP `DATA` or JMAP
    /// `EmailSubmission/set`.
    pub bytes: Vec<u8>,
    /// `Message-ID` of the message we just built, in `<id@host>` form.
    /// Persist on the outbox row so the post-send Sent-folder copy can
    /// be reconciled by id.
    pub message_id: String,
}

/// Errors produced by [`build_rfc5322`].
#[derive(Debug, Error)]
pub enum ComposeError {
    /// All of `to`, `cc`, and `bcc` are empty — nothing to send to.
    /// SMTP servers reject `RCPT TO:<>` so we catch this at the
    /// builder boundary rather than letting the submission fail.
    #[error("draft has no recipients (To/Cc/Bcc all empty)")]
    NoRecipients,

    /// Couldn't extract a domain from the `From` address. Means the
    /// caller passed something that doesn't look like an email.
    #[error("from address has no domain: {0:?}")]
    InvalidFromAddress(String),

    /// `mail-builder` write failure. Should be effectively unreachable
    /// since we write to `Vec<u8>`, but the API returns `io::Result`.
    #[error("mail-builder io: {0}")]
    Io(#[from] std::io::Error),
}

/// Assemble a sendable RFC 5322 message from the user's draft and their
/// `From` identity. Mints a `Message-ID` deterministically from the
/// `From` domain + a 64-bit random + nanosecond timestamp.
///
/// Headers set, in order:
///
/// 1. `Date` — auto-filled by mail-builder if not provided. We let it
///    use the system clock at build time.
/// 2. `Message-ID` — minted here as `<{rand_hex}.{nanos}.qsl@{host}>`.
///    `host` comes from the `From` domain so receivers can still
///    DKIM/SPF align even if the SMTP submission rewrites the envelope.
/// 3. `From`, `To`, `Cc`, `Bcc`, `Subject` — verbatim from the draft;
///    mail-builder handles RFC 2047 encoded-word for non-ASCII names
///    and subject lines.
/// 4. `In-Reply-To` and `References` — for replies. Mail-builder strips
///    the surrounding angle-brackets we store and re-wraps on output.
/// 5. `MIME-Version: 1.0` and a single `text/plain; charset=utf-8` body.
pub fn build_rfc5322(draft: &Draft, from: &EmailAddress) -> Result<BuiltMessage, ComposeError> {
    if draft.to.is_empty() && draft.cc.is_empty() && draft.bcc.is_empty() {
        return Err(ComposeError::NoRecipients);
    }

    let host = from
        .address
        .rsplit_once('@')
        .map(|(_, h)| h.to_string())
        .ok_or_else(|| ComposeError::InvalidFromAddress(from.address.clone()))?;

    // mail-builder expects the message-id without angle brackets; we
    // wrap on output (its `Header for MessageId` impl writes `<id>`).
    // Persist with brackets to match `MessageHeaders.rfc822_message_id`.
    let mid_inner = mint_message_id_inner(&host);
    let mid_wrapped = format!("<{mid_inner}>");

    let mut builder = MessageBuilder::new()
        .from(build_addr(from))
        .subject(Text::from(draft.subject.clone()))
        .message_id(MessageId::new(mid_inner))
        .text_body(draft.body.clone());

    if !draft.to.is_empty() {
        builder = builder.to(addrs_to_address(&draft.to));
    }
    if !draft.cc.is_empty() {
        builder = builder.cc(addrs_to_address(&draft.cc));
    }
    if !draft.bcc.is_empty() {
        builder = builder.bcc(addrs_to_address(&draft.bcc));
    }
    if let Some(parent) = &draft.in_reply_to {
        builder = builder.in_reply_to(MessageId::new(strip_brackets(parent).to_string()));
    }
    if !draft.references.is_empty() {
        let refs: Vec<String> = draft
            .references
            .iter()
            .map(|r| strip_brackets(r).to_string())
            .collect();
        builder = builder.references(MessageId::from(refs));
    }

    let bytes = builder.write_to_vec()?;
    Ok(BuiltMessage {
        bytes,
        message_id: mid_wrapped,
    })
}

fn build_addr(a: &EmailAddress) -> Address<'static> {
    Address::new_address(
        a.display_name.as_ref().filter(|s| !s.is_empty()).cloned(),
        a.address.clone(),
    )
}

fn addrs_to_address(addrs: &[EmailAddress]) -> Address<'static> {
    if addrs.len() == 1 {
        build_addr(&addrs[0])
    } else {
        Address::new_list(addrs.iter().map(build_addr).collect())
    }
}

fn strip_brackets(s: &str) -> &str {
    s.strip_prefix('<')
        .unwrap_or(s)
        .strip_suffix('>')
        .unwrap_or(s)
}

/// Mint the inner part of a Message-ID: 16 hex chars of randomness
/// plus a nanosecond timestamp, joined by `.qsl@<host>`. Random side
/// keeps the value globally unique even on a fast local clock; the
/// timestamp side keeps it monotonic per-process for ordering in
/// debug logs.
fn mint_message_id_inner(host: &str) -> String {
    let mut buf = [0u8; 8];
    rand::rng().fill_bytes(&mut buf);
    let entropy = u64::from_be_bytes(buf);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{entropy:016x}.{nanos}.qsl@{host}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use qsl_core::{AccountId, DraftBodyKind, DraftId};

    fn email(name: Option<&str>, addr: &str) -> EmailAddress {
        EmailAddress {
            display_name: name.map(str::to_string),
            address: addr.to_string(),
        }
    }

    fn draft(subject: &str, body: &str, to: Vec<EmailAddress>) -> Draft {
        let now = Utc::now();
        Draft {
            id: DraftId("d1".into()),
            account_id: AccountId("a1".into()),
            in_reply_to: None,
            references: vec![],
            to,
            cc: vec![],
            bcc: vec![],
            subject: subject.to_string(),
            body: body.to_string(),
            body_kind: DraftBodyKind::Plain,
            attachments: vec![],
            created_at: now,
            updated_at: now,
        }
    }

    fn body_str(b: &[u8]) -> String {
        String::from_utf8_lossy(b).into_owned()
    }

    #[test]
    fn no_recipients_is_an_error() {
        let d = draft("Hi", "body", vec![]);
        let from = email(Some("Me"), "me@example.com");
        let err = build_rfc5322(&d, &from).unwrap_err();
        assert!(matches!(err, ComposeError::NoRecipients));
    }

    #[test]
    fn invalid_from_is_an_error() {
        let d = draft("Hi", "body", vec![email(None, "to@example.com")]);
        let from = email(None, "no-at-sign");
        let err = build_rfc5322(&d, &from).unwrap_err();
        assert!(matches!(err, ComposeError::InvalidFromAddress(_)));
    }

    #[test]
    fn ascii_message_round_trip_has_required_headers() {
        let d = draft(
            "Hello",
            "Body line 1\nLine 2",
            vec![email(None, "to@example.com")],
        );
        let from = email(Some("Me"), "me@example.com");
        let built = build_rfc5322(&d, &from).unwrap();
        let s = body_str(&built.bytes);
        assert!(s.contains("From: \"Me\" <me@example.com>"), "got: {s}");
        assert!(s.contains("To: <to@example.com>") || s.contains("To: to@example.com"));
        assert!(s.contains("Subject: Hello"));
        assert!(s.contains("MIME-Version: 1.0"));
        assert!(s.contains("Date: "));
        assert!(s.contains("Message-ID: <"));
        assert!(s.contains("Body line 1"));
        // Body separator (header / body boundary) is CRLF CRLF.
        assert!(s.contains("\r\n\r\n"));
    }

    #[test]
    fn message_id_is_returned_with_brackets_and_uses_from_domain() {
        let d = draft("Hi", "body", vec![email(None, "to@example.com")]);
        let from = email(None, "user@qsl.test");
        let built = build_rfc5322(&d, &from).unwrap();
        assert!(built.message_id.starts_with('<'));
        assert!(built.message_id.ends_with('>'));
        assert!(
            built.message_id.contains("@qsl.test>"),
            "host should match from-domain: {}",
            built.message_id
        );
        // The wire format also includes the Message-ID, with brackets.
        let s = body_str(&built.bytes);
        assert!(s.contains(&format!("Message-ID: {}", built.message_id)));
    }

    #[test]
    fn message_ids_are_unique_across_calls() {
        let d = draft("Hi", "body", vec![email(None, "to@example.com")]);
        let from = email(None, "me@example.com");
        let a = build_rfc5322(&d, &from).unwrap().message_id;
        let b = build_rfc5322(&d, &from).unwrap().message_id;
        assert_ne!(a, b, "two builds produced the same Message-ID");
    }

    #[test]
    fn non_ascii_subject_is_rfc2047_encoded() {
        // Smart quotes + em dash + a non-Latin name. mail-builder
        // wraps in `=?utf-8?Q?...?=` or `=?utf-8?B?...?=` as
        // appropriate.
        let d = draft(
            "Quarterly review — Q1 2026 \u{201c}strategy\u{201d}",
            "body",
            vec![email(None, "to@example.com")],
        );
        let from = email(Some("Renée"), "me@example.com");
        let built = build_rfc5322(&d, &from).unwrap();
        let s = body_str(&built.bytes);
        // Subject line must be encoded — raw UTF-8 bytes shouldn't
        // appear in the wire form.
        let subject_line = s.lines().find(|l| l.starts_with("Subject:")).unwrap();
        assert!(
            subject_line.contains("=?utf-8?") || subject_line.contains("=?UTF-8?"),
            "subject not RFC 2047 encoded: {subject_line}"
        );
        assert!(
            !subject_line.contains("\u{2014}"),
            "raw em dash leaked into subject line: {subject_line}"
        );
        // Same for the From display name.
        let from_line = s.lines().find(|l| l.starts_with("From:")).unwrap();
        assert!(
            from_line.contains("=?utf-8?") || from_line.contains("=?UTF-8?"),
            "from name not RFC 2047 encoded: {from_line}"
        );
    }

    #[test]
    fn multiple_to_cc_bcc_are_emitted() {
        let mut d = draft(
            "Hi",
            "body",
            vec![
                email(Some("Alice"), "alice@example.com"),
                email(None, "bob@example.com"),
            ],
        );
        d.cc = vec![email(None, "carol@example.com")];
        d.bcc = vec![email(None, "dave@example.com")];
        let from = email(None, "me@example.com");
        let s = body_str(&build_rfc5322(&d, &from).unwrap().bytes);

        assert!(s.contains("alice@example.com"));
        assert!(s.contains("bob@example.com"));
        assert!(s
            .lines()
            .any(|l| l.starts_with("Cc:") && l.contains("carol@example.com")));
        assert!(s
            .lines()
            .any(|l| l.starts_with("Bcc:") && l.contains("dave@example.com")));
    }

    #[test]
    fn reply_headers_strip_then_rewrap_brackets() {
        let mut d = draft("Re: Hi", "body", vec![email(None, "to@example.com")]);
        d.in_reply_to = Some("<parent@example.com>".to_string());
        d.references = vec!["<a@example.com>".into(), "<b@example.com>".into()];
        let from = email(None, "me@example.com");
        let s = body_str(&build_rfc5322(&d, &from).unwrap().bytes);

        let irt = s.lines().find(|l| l.starts_with("In-Reply-To:")).unwrap();
        assert!(irt.contains("<parent@example.com>"), "got: {irt}");
        // No double-wrapping (no `<<` anywhere in the line).
        assert!(!irt.contains("<<"), "double-wrapped: {irt}");

        let refs = s.lines().find(|l| l.starts_with("References:")).unwrap();
        assert!(
            refs.contains("<a@example.com>") && refs.contains("<b@example.com>"),
            "got: {refs}"
        );
        assert!(!refs.contains("<<"), "double-wrapped: {refs}");
    }

    #[test]
    fn body_uses_text_plain_utf8() {
        let d = draft("Hi", "Hello — world", vec![email(None, "to@example.com")]);
        let from = email(None, "me@example.com");
        let s = body_str(&build_rfc5322(&d, &from).unwrap().bytes);
        // Expect a Content-Type: text/plain; charset="utf-8" on the
        // body part (mail-builder may quote attribute values).
        assert!(
            s.lines().any(|l| l.starts_with("Content-Type:")
                && l.contains("text/plain")
                && l.to_lowercase().contains("utf-8")),
            "missing text/plain;charset=utf-8 header in: {s}"
        );
    }
}
