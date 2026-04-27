// SPDX-License-Identifier: Apache-2.0

//! QSL MIME helpers — thin wrappers over `mail-parser`.
//!
//! Presents QSL domain types (`MessageHeaders`, `MessageBody`,
//! `Attachment`, `EmailAddress`) to callers and keeps the underlying
//! parser crate out of the public surface. Both `qsl-imap-client`
//! and `qsl-jmap-client` call into [`parse_rfc822`] when a
//! `fetch_message` response comes back as raw bytes.

use std::borrow::Cow;

use chrono::{TimeZone, Utc};
use mail_parser::{Address, HeaderValue, Message, MessageParser, MimeHeaders, PartType};

pub mod compose;
pub mod remote_content;

// mail-parser's Address::iter() returns Box<dyn DoubleEndedIterator<...>>,
// which gives us one uniform shape regardless of whether the underlying
// header was a list or a group of addresses.

use qsl_core::{
    AccountId, Attachment, AttachmentRef, EmailAddress, FolderId, MessageBody, MessageFlags,
    MessageHeaders, MessageId, ThreadId,
};

/// Parse a raw RFC 822 blob into a QSL [`MessageBody`].
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
///   which would bypass the remote-content policy.
/// - Tag stripping list (`script`, `iframe`, `object`, `embed`,
///   `form`, `input`, `button`, `textarea`, `select`, `style`,
///   `link`) is redundant with ammonia's default allowlist but
///   explicit — if ammonia ever loosens its defaults in a minor
///   release, these stay stripped.
///
/// Phase 1 Week 8 adds **remote-content blocking**: every URL in a
/// `src` / `background` / `poster` / `srcset` attribute runs
/// through [`remote_content::is_blocked`] against the default
/// curated adblock engine. Blocked URLs get the attribute dropped,
/// which neutralizes the resource load (browsers render the
/// placeholder "missing image" glyph when `<img>` has no `src`).
/// Link hrefs are deliberately not filtered here — blocking an
/// outbound anchor is user-hostile; link-click cleaning (utm_*
/// stripping, Mailchimp/SendGrid redirect unwrapping) is a
/// separate pipeline stage in the renderer's `on_link_click`
/// callback.
///
/// For senders the user has explicitly trusted (recorded in
/// `remote_content_opt_ins`), `messages_get` calls
/// [`sanitize_email_html_trusted`] instead, which keeps every other
/// sanitization rule but skips the URL filter.
///
/// Returns empty-ish output is acceptable: the reader UI's
/// `compose_reader_html` falls back to the plaintext path when the
/// sanitized result is empty or whitespace-only.
pub fn sanitize_email_html(raw_html: &str) -> String {
    sanitize(raw_html, /* block_remote = */ true)
}

/// Sanitize HTML for a sender the user has trusted via
/// `remote_content_opt_ins` for this account. Every rule from
/// [`sanitize_email_html`] still applies (script stripping,
/// `javascript:` URL removal, event-handler attribute removal,
/// element allowlist), but `src` / `background` / `poster` /
/// `srcset` URLs are passed through unchecked.
pub fn sanitize_email_html_trusted(raw_html: &str) -> String {
    sanitize(raw_html, /* block_remote = */ false)
}

fn sanitize(raw_html: &str, block_remote: bool) -> String {
    let engine = remote_content::default_engine();
    ammonia::Builder::default()
        .add_generic_attributes(["style"])
        .rm_tags([
            "script", "iframe", "object", "embed", "form", "input", "button", "textarea", "select",
            "style", "link",
        ])
        .attribute_filter(move |_element, attribute, value| -> Option<Cow<'_, str>> {
            // Only URL-bearing attributes on media elements go
            // through the blocker. Everything else passes.
            match attribute {
                "src" | "background" | "poster" | "srcset"
                    if block_remote && remote_content::is_blocked(engine, value, "image") =>
                {
                    None
                }
                "style" if block_remote => {
                    // CSS `background-image: url(...)` and friends are
                    // a second remote-content vector that the
                    // attribute-name match above misses. Drop blocked
                    // declarations from the inline style; keep the
                    // rest. See `filter_inline_style` for the parser.
                    Some(Cow::Owned(filter_inline_style(value, engine)))
                }
                _ => Some(Cow::Borrowed(value)),
            }
        })
        .clean(raw_html)
        .to_string()
}

/// Walk a CSS inline-style declaration list, drop any declaration
/// whose `url(...)` argument is blocked by `engine`, and re-emit the
/// rest. Not a full CSS parser — handles the `prop: url(arg) [other]`
/// shape that marketing emails actually ship and falls back to
/// keeping a declaration when the URL token can't be cleanly
/// extracted (so a malformed declaration with a tracking pixel stays
/// blocked at the higher-level `<img src>` filter).
///
/// Why drop the whole declaration rather than just the `url(...)`
/// token: the alternatives (rewriting to `none`, leaving an empty
/// `url()`) tend to fight the email's existing fallback chain,
/// whereas dropping the property entirely lets any earlier
/// declaration in the cascade or the user-agent default take over.
fn filter_inline_style(style: &str, engine: &adblock::Engine) -> String {
    // Fast path: scan once for blocked URLs. If nothing matches,
    // return the input verbatim — this preserves exact whitespace +
    // trailing-semicolon shape, which keeps the common no-tracker
    // case (most inline styles) byte-identical to the input and
    // avoids spurious diffs in existing sanitizer tests.
    let mut any_blocked = false;
    let kept: Vec<&str> = style
        .split(';')
        .filter_map(|decl| {
            let trimmed = decl.trim();
            if trimmed.is_empty() {
                return None;
            }
            for url in css_url_tokens(trimmed) {
                if remote_content::is_blocked(engine, url, "image") {
                    any_blocked = true;
                    return None;
                }
            }
            Some(trimmed)
        })
        .collect();
    if !any_blocked {
        return style.to_string();
    }
    // Slow path: rebuild from the kept declarations.
    let mut out = String::with_capacity(style.len());
    let mut first = true;
    for decl in kept {
        if !first {
            out.push_str("; ");
        }
        first = false;
        out.push_str(decl);
    }
    // Preserve a trailing semicolon when the input had one — keeps
    // emitted styles consistent with the canonical CSS shorthand
    // shape downstream tooling expects.
    if !out.is_empty() && style.trim_end().ends_with(';') {
        out.push(';');
    }
    out
}

/// Yield each `url(...)` argument inside a CSS declaration. Strips
/// surrounding whitespace and matched single / double quotes.
/// Returns an empty iterator when no `url(` token is present.
fn css_url_tokens(decl: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = decl.as_bytes();
    let mut i = 0;
    while i + 4 <= bytes.len() {
        if !decl[i..].starts_with("url(") {
            i += 1;
            continue;
        }
        i += 4;
        // Skip leading whitespace inside the parens.
        while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
            i += 1;
        }
        // Optional surrounding quote.
        let quote = match bytes.get(i) {
            Some(&b'"') => {
                i += 1;
                Some(b'"')
            }
            Some(&b'\'') => {
                i += 1;
                Some(b'\'')
            }
            _ => None,
        };
        let start = i;
        let end = match quote {
            Some(q) => bytes[i..].iter().position(|&b| b == q).map(|off| i + off),
            None => bytes[i..]
                .iter()
                .position(|&b| b == b')')
                .map(|off| i + off),
        };
        let Some(end) = end else { return out };
        let slice = decl[start..end].trim_end();
        if !slice.is_empty() {
            out.push(slice);
        }
        // Advance past the closing quote (if any) and the closing `)`.
        i = end + 1;
        if quote.is_some() {
            // Skip whitespace and the closing ')'.
            while i < bytes.len() && bytes[i] != b')' {
                i += 1;
            }
            if i < bytes.len() {
                i += 1;
            }
        }
    }
    out
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

/// Pull `In-Reply-To` and `References` out of an RFC 5322 header
/// block (no body). Used by `qsl-imap-client` after a
/// `BODY.PEEK[HEADER]` FETCH — the structured ENVELOPE only
/// surfaces `In-Reply-To`, so the threading pipeline needs a
/// follow-up parse to recover `References` for the chain-walk
/// fallback.
///
/// Both Message-IDs are returned angle-bracket-wrapped to match the
/// shape the rest of the workspace already uses
/// (`<id@example.com>`).
pub fn extract_thread_headers(header_bytes: &[u8]) -> (Option<String>, Vec<String>) {
    // mail-parser wants a complete message. The header block from
    // `BODY.PEEK[HEADER]` ends with a CRLF separator already; tack
    // on an empty body so the parser stops cleanly.
    let synthetic = [header_bytes, b"\r\n"].concat();
    let Some(parsed) = MessageParser::default().parse(&synthetic) else {
        return (None, Vec::new());
    };
    let in_reply_to = single_message_id(parsed.in_reply_to());
    let references = message_id_list(parsed.references());
    (in_reply_to, references)
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
    let in_reply_to = single_message_id(parsed.in_reply_to());
    let references = message_id_list(parsed.references());

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
        in_reply_to,
        references,
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
    use qsl_core::{AccountId, FolderId, MessageFlags, MessageId};

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
    fn body_decodes_declared_windows_1252_charset() {
        // Defensive regression: the user's real-world mojibake report
        // (`Â®` / `Ã®` for `®`) was pinned to the renderer's data-URL
        // encoder, not to mail-parser. Lock in the assumption that
        // mail-parser honors the declared charset so MCP `get_message`
        // (and the desktop reader pane) both receive correctly-decoded
        // strings. If mail-parser ever regresses, this fails loudly.
        let mut raw = b"From: sender@example.com\r\n\
To: me@example.com\r\n\
Subject: Test\r\n\
Date: Fri, 18 Apr 2026 10:00:00 +0000\r\n\
Message-ID: <cp1252@example.com>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: text/plain; charset=windows-1252\r\n\
Content-Transfer-Encoding: 8bit\r\n\
\r\n\
Good Hands"
            .to_vec();
        // Windows-1252 0xAE → ®.
        raw.push(0xAE);
        raw.extend_from_slice(b"\r\n");

        let msg_id = MessageId("m1".into());
        let acct = AccountId("a1".into());
        let folder = FolderId("INBOX".into());
        let flags = MessageFlags::default();
        let body = parse_rfc822(&raw, ident(&msg_id, &acct, &folder, &flags)).unwrap();
        let text = body.body_text.expect("text body present");
        assert!(
            text.contains("Good Hands®"),
            "windows-1252 0xAE was not decoded to ®; got {text:?}"
        );
    }

    #[test]
    fn body_decodes_declared_iso_8859_1_charset() {
        let mut raw = b"From: sender@example.com\r\n\
To: me@example.com\r\n\
Subject: Test\r\n\
Date: Fri, 18 Apr 2026 10:00:00 +0000\r\n\
Message-ID: <iso@example.com>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: text/plain; charset=iso-8859-1\r\n\
Content-Transfer-Encoding: 8bit\r\n\
\r\n\
caf"
        .to_vec();
        // ISO-8859-1 0xE9 → é.
        raw.push(0xE9);
        raw.extend_from_slice(b"\r\n");

        let msg_id = MessageId("m1".into());
        let acct = AccountId("a1".into());
        let folder = FolderId("INBOX".into());
        let flags = MessageFlags::default();
        let body = parse_rfc822(&raw, ident(&msg_id, &acct, &folder, &flags)).unwrap();
        let text = body.body_text.expect("text body present");
        assert!(
            text.contains("café"),
            "iso-8859-1 0xE9 was not decoded to é; got {text:?}"
        );
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
    fn sanitize_strips_mailchimp_tracking_pixel() {
        // Realistic Mailchimp open-tracking pixel URL.
        let probe = r#"<p>body</p><img src="https://acme.list-manage.com/track/open.php?u=abc&id=xyz" width="1" height="1">"#;
        let out = sanitize_email_html(probe);
        // The `src` is gone — ammonia drops just that attribute
        // when the filter returns None; the `<img>` wrapper stays
        // but is now sourceless and won't load anything.
        assert!(!out.contains("list-manage"), "pixel URL survived: {out}");
        assert!(!out.contains(r#"src=""#), "src attr survived: {out}");
    }

    #[test]
    fn sanitize_strips_google_analytics_pixel() {
        let probe = r#"<img src="https://www.google-analytics.com/collect?tid=UA-99">"#;
        let out = sanitize_email_html(probe);
        assert!(!out.contains("google-analytics"), "GA URL survived: {out}");
    }

    #[test]
    fn sanitize_preserves_benign_image_src() {
        // Not in any filter rule — should pass through intact.
        let probe = r#"<img src="https://example.com/logo.png" alt="logo">"#;
        let out = sanitize_email_html(probe);
        assert!(
            out.contains(r#"src="https://example.com/logo.png""#),
            "benign src lost: {out}"
        );
        assert!(out.contains(r#"alt="logo""#), "alt lost: {out}");
    }

    #[test]
    fn sanitize_preserves_href_even_when_host_matches_tracker() {
        // Link hrefs go through unfiltered — link-click cleaning
        // is a separate pipeline stage, and blocking an outbound
        // anchor is user-hostile.
        let probe = r#"<a href="https://acme.list-manage.com/subscribe/confirm">Confirm</a>"#;
        let out = sanitize_email_html(probe);
        assert!(
            out.contains(r#"href="https://acme.list-manage.com/subscribe/confirm""#),
            "href erroneously dropped: {out}"
        );
    }

    #[test]
    fn sanitize_trusted_keeps_tracker_image_src() {
        // Same Mailchimp pixel that the default sanitizer drops. With
        // the trusted variant, the user has explicitly opted in to
        // remote content from this sender, so the URL filter is
        // skipped and the `src` survives.
        let probe =
            r#"<p>body</p><img src="https://acme.list-manage.com/track/open.php?u=abc&id=xyz">"#;
        let out = sanitize_email_html_trusted(probe);
        assert!(
            out.contains("list-manage.com/track/open.php"),
            "trusted-sender pixel src dropped: {out}"
        );
    }

    #[test]
    fn sanitize_trusted_still_strips_scripts() {
        // Trust applies to remote content only — script/iframe/etc.
        // stripping must remain unconditional.
        let out = sanitize_email_html_trusted(
            "<p>hi</p><script>alert('xss')</script><iframe src=\"https://attacker.example/\"></iframe>",
        );
        assert!(!out.contains("<script"), "script survived: {out}");
        assert!(!out.contains("<iframe"), "iframe survived: {out}");
        assert!(out.contains("<p>hi</p>"));
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

    // ---------------------------------------------------------------
    // Inline-style remote-content gating (backlog item 4).
    // ---------------------------------------------------------------

    #[test]
    fn sanitize_strips_blocked_background_image_url() {
        // Mailchimp pixel hidden inside an inline style.
        let probe = r#"<div style="background-image: url(https://acme.list-manage.com/track/open.php?u=abc&id=xyz); color: #c00;">x</div>"#;
        let out = sanitize_email_html(probe);
        assert!(
            !out.contains("list-manage"),
            "background-image URL survived: {out}"
        );
        assert!(
            out.contains("color: #c00"),
            "sibling declaration dropped along with the blocked one: {out}"
        );
    }

    #[test]
    fn sanitize_strips_blocked_background_shorthand_url() {
        // `background:` shorthand also carries url(...) values.
        let probe = r#"<td style="background: url(https://www.google-analytics.com/collect?tid=UA-1) no-repeat;">x</td>"#;
        let out = sanitize_email_html(probe);
        assert!(
            !out.contains("google-analytics"),
            "shorthand background URL survived: {out}"
        );
    }

    #[test]
    fn sanitize_keeps_benign_background_image_url() {
        let probe =
            r#"<div style="background-image: url(https://example.com/hero.png);">hero</div>"#;
        let out = sanitize_email_html(probe);
        assert!(
            out.contains("hero.png"),
            "benign background URL erroneously dropped: {out}"
        );
    }

    #[test]
    fn sanitize_handles_quoted_url_arg() {
        // Quoted URL forms must also be parsed — single, double, none.
        for probe in [
            r#"<div style='background-image: url("https://acme.list-manage.com/x.gif")'>x</div>"#,
            r#"<div style="background-image: url('https://acme.list-manage.com/x.gif')">x</div>"#,
            r#"<div style="background-image: url(https://acme.list-manage.com/x.gif)">x</div>"#,
        ] {
            let out = sanitize_email_html(probe);
            assert!(
                !out.contains("list-manage"),
                "tracker pixel survived in {probe:?} → {out:?}"
            );
        }
    }

    #[test]
    fn sanitize_inline_style_with_no_url_passes_through() {
        let probe = r#"<p style="color: #222; padding: 8px 16px; font-weight: 600;">hello</p>"#;
        let out = sanitize_email_html(probe);
        assert!(out.contains("color: #222"), "color lost: {out}");
        assert!(out.contains("padding: 8px 16px"), "padding lost: {out}");
        assert!(out.contains("font-weight: 600"), "font-weight lost: {out}");
    }

    #[test]
    fn sanitize_trusted_keeps_blocked_background_image() {
        // Trusted variant skips remote-content checks for inline
        // styles too, matching the per-attribute behavior.
        let probe = r#"<div style="background-image: url(https://acme.list-manage.com/track/open.php?u=abc&id=xyz);">x</div>"#;
        let out = sanitize_email_html_trusted(probe);
        assert!(
            out.contains("list-manage"),
            "trusted-sender background URL dropped: {out}"
        );
    }
}
