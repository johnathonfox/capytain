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

/// Sanitize HTML email content for rendering in the Servo reader pane.
///
/// Uses `ammonia` with its conservative default allowlist plus two
/// project-specific adjustments:
///
/// - `style` attribute allowed on every accepted tag. HTML emails
///   rely heavily on inline styles for presentational layout
///   (centered tables, branded colors, etc.). Not allowing them
///   would render most marketing and transactional email as
///   unstyled text. Inline `style="..."` can't inject script — CSS
///   expressions were deprecated in every browser a decade ago, and
///   we're rendering via Servo with JavaScript disabled anyway
///   (belt + suspenders). The `<style>` **element** remains
///   stripped: it can load external resources via `@import url(...)`
///   which would bypass the Phase 1 Week 8 remote-content policy
///   before it's in place.
/// - Tag stripping list (`script`, `iframe`, `object`, `embed`,
///   `form`, `input`, `button`, `textarea`, `select`, `style`,
///   `link`) is redundant with ammonia's default allowlist but
///   explicit — if ammonia ever loosens its defaults in a minor
///   release, these stay stripped.
///
/// Returns empty-ish output is acceptable: the reader UI's
/// `compose_reader_html` falls back to the plaintext path when the
/// sanitized result is empty or whitespace-only. That matches the
/// behavior of a well-intentioned but stripping-heavy sanitizer on
/// a message whose HTML was almost entirely script content.
pub fn sanitize_email_html(raw_html: &str) -> String {
    ammonia::Builder::default()
        .add_generic_attributes(["style"])
        .rm_tags([
            "script", "iframe", "object", "embed", "form", "input", "button", "textarea", "select",
            "style", "link",
        ])
        .clean(raw_html)
        .to_string()
}

/// Decode a single RFC-2047 encoded-word header value
/// (`=?UTF-8?Q?…?=`, `=?UTF-8?B?…?=`, etc.) into a plain `String`.
///
/// IMAP `ENVELOPE` responses surface raw header bytes with their
/// encoded-word wrappers intact; `mail-parser`'s `Message::subject()`
/// handles decoding as part of full-message parsing but there's no
/// exposed standalone "decode this header value" helper. Work around
/// that by parsing a synthetic one-header message and pulling the
/// decoded subject back out.
///
/// Returns the input unchanged if the parser can't build a Message
/// (shouldn't happen for well-formed input, but staying lenient
/// beats losing the raw text entirely).
pub fn decode_header_value(raw: &str) -> String {
    if !raw.contains("=?") {
        // Fast path: not an encoded word, nothing to do.
        return raw.to_string();
    }
    let synthetic = format!("Subject: {raw}\r\n\r\n");
    MessageParser::default()
        .parse(synthetic.as_bytes())
        .and_then(|m| m.subject().map(|s| s.to_string()))
        .unwrap_or_else(|| raw.to_string())
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

    // ---------------------------------------------------------------
    // `sanitize_email_html` — XSS probes + benign HTML preservation.
    // ---------------------------------------------------------------

    #[test]
    fn sanitize_strips_script_tag() {
        let out = sanitize_email_html("<p>hi</p><script>alert('xss')</script>");
        assert!(!out.contains("<script"), "script tag survived: {out}");
        assert!(!out.contains("alert"), "script body survived: {out}");
        assert!(out.contains("<p>hi</p>"), "benign content lost: {out}");
    }

    #[test]
    fn sanitize_strips_iframe_object_embed() {
        for probe in [
            r#"<iframe src="https://attacker.example/"></iframe>"#,
            r#"<object data="evil.swf"></object>"#,
            r#"<embed src="evil.swf">"#,
        ] {
            let out = sanitize_email_html(probe);
            assert!(
                !out.contains("<iframe") && !out.contains("<object") && !out.contains("<embed"),
                "frame/object/embed survived on {probe:?} → {out:?}"
            );
        }
    }

    #[test]
    fn sanitize_strips_event_handler_attributes() {
        let out = sanitize_email_html(r#"<img src="x" onerror="alert(1)" alt="pic">"#);
        assert!(!out.contains("onerror"), "onerror survived: {out}");
        // The <img> itself is safe and should be preserved (remote
        // content blocking happens in a later Phase 1 step, not in
        // the sanitizer).
        assert!(out.contains("<img"), "img tag lost: {out}");
        assert!(out.contains(r#"alt="pic""#), "alt attribute lost: {out}");
    }

    #[test]
    fn sanitize_strips_javascript_urls() {
        let out = sanitize_email_html(r#"<a href="javascript:alert(1)">click</a>"#);
        assert!(
            !out.contains("javascript:"),
            "javascript URL survived: {out}"
        );
        // The anchor text stays; the href is just removed.
        assert!(out.contains("click"), "anchor text lost: {out}");
    }

    #[test]
    fn sanitize_strips_form_and_input() {
        let out = sanitize_email_html(
            r#"<form action="/steal"><input name="password" type="password"></form>"#,
        );
        assert!(!out.contains("<form"), "form survived: {out}");
        assert!(!out.contains("<input"), "input survived: {out}");
        assert!(!out.contains("password"), "field name survived: {out}");
    }

    #[test]
    fn sanitize_strips_style_tag_but_keeps_inline_style_attr() {
        let out = sanitize_email_html(
            r#"<style>@import url(https://attacker.example/evil.css);</style>
               <p style="color: #c00;">urgent</p>"#,
        );
        assert!(!out.contains("<style"), "style element survived: {out}");
        assert!(!out.contains("@import"), "style contents survived: {out}");
        assert!(
            out.contains(r#"style="color: #c00;""#),
            "inline style attr lost: {out}"
        );
    }

    #[test]
    fn sanitize_preserves_benign_email_html() {
        // Realistic marketing-email skeleton: table layout, inline
        // styles, an https anchor, an h1. None of this should be
        // touched.
        let input = r#"<h1 style="color:#222;">Hello, Jane</h1>
<table style="width: 100%;">
  <tr><td style="padding:1rem;">
    <a href="https://example.com/click?ref=abc">Visit our site</a>
  </td></tr>
</table>"#;
        let out = sanitize_email_html(input);
        assert!(out.contains("<h1"), "<h1> lost: {out}");
        // ammonia auto-adds `rel="noopener noreferrer"` on external
        // anchors — good security hygiene, so check href + anchor
        // text separately rather than the whole opening tag.
        assert!(
            out.contains(r#"href="https://example.com/click?ref=abc""#),
            "href lost: {out}"
        );
        assert!(out.contains("Visit our site"), "anchor text lost: {out}");
        assert!(out.contains("<table"), "<table> lost: {out}");
        // Inline styles preserved.
        assert!(out.contains(r#"style="color:#222;""#));
    }
}
