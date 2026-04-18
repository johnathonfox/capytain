// SPDX-License-Identifier: Apache-2.0

//! Capytain MIME helpers — thin wrappers over `mail-parser`.
//!
//! Presents Capytain domain types (`MessageHeaders`, `MessageBody`,
//! `Attachment`, `EmailAddress`) to callers and keeps the underlying
//! parser crate out of the public surface. Both `capytain-imap-client`
//! and `capytain-jmap-client` call into [`parse_rfc822`] when a
//! `fetch_message` response comes back as raw bytes.

use chrono::{TimeZone, Utc};
use mail_parser::{Address, HeaderValue, Message, MessageParser, MimeHeaders, PartType};

// mail-parser's Address::iter() returns Box<dyn DoubleEndedIterator<...>>,
// which gives us one uniform shape regardless of whether the underlying
// header was a list or a group of addresses.

use capytain_core::{
    AccountId, Attachment, AttachmentRef, EmailAddress, FolderId, MessageBody, MessageFlags,
    MessageHeaders, MessageId, ThreadId,
};

/// Parse a raw RFC 822 blob into a Capytain [`MessageBody`].
///
/// Identity fields the adapter supplies (its own opaque IDs, the account
/// and folder) are taken as parameters; the parser only owns the
/// RFC-5322 headers and part structure.
pub fn parse_rfc822(raw: &[u8], identity: MessageIdentity<'_>) -> Option<MessageBody> {
    let parsed = MessageParser::default().parse(raw)?;
    let headers = headers_from(&parsed, &identity);
    let (body_text, body_html, attachments) = body_from(&parsed);
    let in_reply_to = single_message_id(parsed.in_reply_to());
    let references = message_id_list(parsed.references());
    Some(MessageBody {
        headers,
        body_html,
        body_text,
        attachments,
        in_reply_to,
        references,
    })
}

/// Parse just the headers plus a short snippet — used in list responses
/// where we don't want to buffer the full body.
pub fn parse_headers(raw: &[u8], identity: MessageIdentity<'_>) -> Option<MessageHeaders> {
    let parsed = MessageParser::default().parse(raw)?;
    Some(headers_from(&parsed, &identity))
}

/// Identity fields the adapter knows that the MIME parser does not.
#[derive(Debug, Clone, Copy)]
pub struct MessageIdentity<'a> {
    pub id: &'a MessageId,
    pub account_id: &'a AccountId,
    pub folder_id: &'a FolderId,
    pub thread_id: Option<&'a ThreadId>,
    pub size: u32,
    pub flags: &'a MessageFlags,
    pub labels: &'a [String],
}

fn headers_from(parsed: &Message<'_>, identity: &MessageIdentity<'_>) -> MessageHeaders {
    let subject = parsed.subject().unwrap_or_default().to_string();
    let from = addresses_from_opt(parsed.from());
    let reply_to = addresses_from_opt(parsed.reply_to());
    let to = addresses_from_opt(parsed.to());
    let cc = addresses_from_opt(parsed.cc());
    let bcc = addresses_from_opt(parsed.bcc());
    let date = parsed
        .date()
        .and_then(|d| Utc.timestamp_opt(d.to_timestamp(), 0).single())
        .unwrap_or_else(Utc::now);

    let rfc822_message_id = parsed.message_id().map(|s| format!("<{s}>"));
    let snippet = snippet_from(parsed);
    let has_attachments = parsed.attachment_count() > 0;

    MessageHeaders {
        id: identity.id.clone(),
        account_id: identity.account_id.clone(),
        folder_id: identity.folder_id.clone(),
        thread_id: identity.thread_id.cloned(),
        rfc822_message_id,
        subject,
        from,
        reply_to,
        to,
        cc,
        bcc,
        date,
        flags: identity.flags.clone(),
        labels: identity.labels.to_vec(),
        snippet,
        size: identity.size,
        has_attachments,
    }
}

fn addresses_from_opt(addr: Option<&Address<'_>>) -> Vec<EmailAddress> {
    match addr {
        Some(a) => a.iter().map(addr_one).collect(),
        None => Vec::new(),
    }
}

fn addr_one(a: &mail_parser::Addr<'_>) -> EmailAddress {
    EmailAddress {
        address: a.address.as_deref().unwrap_or_default().to_string(),
        display_name: a.name.as_deref().map(str::to_string),
    }
}

fn single_message_id(hv: &HeaderValue<'_>) -> Option<String> {
    match hv {
        HeaderValue::Text(s) => Some(format!("<{s}>")),
        HeaderValue::TextList(list) => list.first().map(|s| format!("<{s}>")),
        _ => None,
    }
}

fn message_id_list(hv: &HeaderValue<'_>) -> Vec<String> {
    match hv {
        HeaderValue::Text(s) => vec![format!("<{s}>")],
        HeaderValue::TextList(list) => list.iter().map(|s| format!("<{s}>")).collect(),
        _ => Vec::new(),
    }
}

fn body_from(parsed: &Message<'_>) -> (Option<String>, Option<String>, Vec<Attachment>) {
    let body_text = parsed
        .body_text(0)
        .map(|b| b.into_owned())
        .filter(|s| !s.is_empty());
    let body_html = parsed
        .body_html(0)
        .map(|b| b.into_owned())
        .filter(|s| !s.is_empty());

    let mut attachments = Vec::new();
    for (i, part) in parsed.parts.iter().enumerate() {
        if !matches!(part.body, PartType::Binary(_) | PartType::InlineBinary(_)) {
            continue;
        }
        let filename = part.attachment_name().unwrap_or("attachment").to_string();
        let mime_type = part
            .content_type()
            .map(|ct| match (ct.ctype(), ct.subtype()) {
                (main, Some(sub)) => format!("{main}/{sub}"),
                (main, None) => main.to_string(),
            })
            .unwrap_or_else(|| "application/octet-stream".to_string());
        let size = part.len() as u64;
        let inline = matches!(part.body, PartType::InlineBinary(_));
        let content_id = part.content_id().map(str::to_string);
        attachments.push(Attachment {
            // The part index is the stable, backend-agnostic handle for
            // this attachment within the message. IMAP callers can map
            // this to a BODY[n] section; JMAP already exposes blob ids.
            id: AttachmentRef(format!("part/{i}")),
            filename,
            mime_type,
            size,
            inline,
            content_id,
        });
    }
    (body_text, body_html, attachments)
}

fn snippet_from(parsed: &Message<'_>) -> String {
    const MAX: usize = 140;
    let raw = parsed
        .body_text(0)
        .or_else(|| parsed.body_html(0))
        .map(|b| b.into_owned())
        .unwrap_or_default();
    let one_line: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.len() <= MAX {
        one_line
    } else {
        let mut truncated = one_line;
        truncated.truncate(MAX);
        truncated.push('\u{2026}');
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capytain_core::{AccountId, FolderId, MessageFlags, MessageId};

    const SAMPLE: &[u8] = b"From: Jane Doe <jane@example.com>\r\n\
To: me@example.com\r\n\
Subject: Hello\r\n\
Date: Fri, 18 Apr 2026 10:00:00 +0000\r\n\
Message-ID: <abc@example.com>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Just checking in.\r\n";

    fn ident<'a>(
        id: &'a MessageId,
        acct: &'a AccountId,
        folder: &'a FolderId,
        flags: &'a MessageFlags,
    ) -> MessageIdentity<'a> {
        MessageIdentity {
            id,
            account_id: acct,
            folder_id: folder,
            thread_id: None,
            size: SAMPLE.len() as u32,
            flags,
            labels: &[],
        }
    }

    #[test]
    fn parses_headers_and_plaintext_body() {
        let msg_id = MessageId("m1".into());
        let acct = AccountId("a1".into());
        let folder = FolderId("INBOX".into());
        let flags = MessageFlags::default();

        let body = parse_rfc822(SAMPLE, ident(&msg_id, &acct, &folder, &flags)).unwrap();
        assert_eq!(body.headers.subject, "Hello");
        assert_eq!(body.headers.from.len(), 1);
        assert_eq!(body.headers.from[0].address, "jane@example.com");
        assert_eq!(
            body.headers.from[0].display_name.as_deref(),
            Some("Jane Doe")
        );
        assert_eq!(body.headers.to[0].address, "me@example.com");
        assert_eq!(
            body.headers.rfc822_message_id.as_deref(),
            Some("<abc@example.com>")
        );
        assert_eq!(
            body.body_text.as_deref().unwrap().trim(),
            "Just checking in."
        );
        // mail-parser synthesizes an HTML fallback from the text/plain
        // part when no text/html is present; the important invariant is
        // that body_text is correct. Asserting body_html.is_none() would
        // over-specify the parser's behavior.
        assert!(body.attachments.is_empty());
        assert!(body.headers.snippet.contains("Just checking in"));
    }

    #[test]
    fn parse_headers_matches_body_headers() {
        let msg_id = MessageId("m1".into());
        let acct = AccountId("a1".into());
        let folder = FolderId("INBOX".into());
        let flags = MessageFlags::default();

        let hdrs = parse_headers(SAMPLE, ident(&msg_id, &acct, &folder, &flags)).unwrap();
        assert_eq!(hdrs.subject, "Hello");
    }
}
