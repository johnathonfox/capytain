// SPDX-License-Identifier: Apache-2.0

//! Build the full HTML document the Servo reader pane renders.
//!
//! Both the in-process Dioxus UI and the desktop popup-window install
//! path need to compose this same document — the UI side from the
//! reactive `RenderedMessage` it just fetched, the desktop side at
//! Servo install time so the renderer can paint into the GTK overlay
//! before Dioxus mounts and `reader_set_position` arrives. Keeping
//! the function here means both paths produce byte-identical markup
//! and share the placeholder / theming rules in one place.
//!
//! Inputs are primitive borrowed slices rather than the
//! `RenderedMessage` struct so this crate doesn't have to depend on
//! `qsl-ipc` (which is a heavier sibling crate that itself pulls in
//! Tauri-side serialization helpers).

/// Compose the wrapper HTML document for the reader pane. `body_html`
/// wins if non-empty; `body_text` is the plaintext fallback wrapped in
/// `<pre>`; if both are empty/whitespace-only the document falls back
/// to a "no body" hint.
///
/// The caller must have already passed any HTML body through
/// [`crate::sanitize_email_html`] — this function does **not**
/// sanitize, it just frames the body in the wrapper chrome.
pub fn compose_reader_html(body_html: Option<&str>, body_text: Option<&str>) -> String {
    let body_section = render_body_section(body_html, body_text);

    // Headers (subject / from / date / recipients) are rendered by
    // the Dioxus side as a styled card; Servo's pane is body-only,
    // so the user sees each piece of info exactly once whether
    // Servo is reparented next to the webview (Linux) or running
    // in a separate auxiliary window (macOS / Windows).
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8">
  <style>
    body {{
      font: 14px/1.5 -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
      color: #e6e8eb;
      background: #0f1115;
      margin: 0;
      padding: 1.25rem;
    }}
    @media (prefers-color-scheme: light) {{
      body {{ color: #14161a; background: #ffffff; }}
    }}
    .qsl-body {{ color: inherit; }}
    .qsl-body pre {{ white-space: pre-wrap; word-wrap: break-word; margin: 0; font: inherit; }}
    .qsl-body a {{ color: #74b4ff; }}
    @media (prefers-color-scheme: light) {{
      .qsl-body a {{ color: #2563eb; }}
    }}
    /* Dimension-preserving placeholder for `<img>` tags whose src was
     * blocked by the remote-content filter. The sanitizer marks each
     * blocked img with `data-qsl-blocked`; ammonia's default allowlist
     * preserves any `width` / `height` HTML attribute so the box keeps
     * its original layout footprint. The min-width/min-height fallback
     * covers tags that had no dimensions at all. The dashed border is
     * deliberately subtle — visible enough to communicate "blocked"
     * without dominating the rendered email. */
    .qsl-body img[data-qsl-blocked] {{
      box-sizing: border-box;
      min-width: 24px;
      min-height: 24px;
      border: 1px dashed rgba(180, 180, 200, 0.30);
      background: rgba(255, 255, 255, 0.025);
    }}
    @media (prefers-color-scheme: light) {{
      .qsl-body img[data-qsl-blocked] {{
        border-color: rgba(20, 22, 26, 0.20);
        background: rgba(20, 22, 26, 0.025);
      }}
    }}
  </style>
</head>
<body>
  <div class="qsl-body">{body_section}</div>
  <script>
    // Click forwarder. Two render paths consume this document:
    //
    //   1. Servo (legacy) — the wrapper is loaded into a Servo WebView
    //      with `window.parent === window`. Anchor clicks set
    //      `window.location.href`, which Servo's navigation delegate
    //      intercepts and routes to `webbrowser::open`. The delegate
    //      denies the navigation in-page, so the email body stays put.
    //
    //   2. webkit2gtk iframe — the wrapper is loaded into a sandboxed
    //      `<iframe>` inside the host webview. Top-level navigation
    //      is blocked by the sandbox (no `allow-top-navigation`), so
    //      `window.location.href` would silently no-op. Instead we
    //      postMessage the URL to the parent frame, where the Dioxus
    //      shell forwards it to the host's `open_external_url`
    //      Tauri command.
    document.addEventListener('click', function(e) {{
      var node = e.target;
      while (node && node.nodeName !== 'A') node = node.parentNode;
      if (!node || !node.href) return;
      e.preventDefault();
      var url = node.href;
      if (window.parent && window.parent !== window) {{
        try {{ window.parent.postMessage({{ type: 'qsl-link-click', url: url }}, '*'); }} catch (err) {{}}
      }} else {{
        try {{ window.location.href = url; }} catch (err) {{}}
      }}
    }}, true);
  </script>
</body>
</html>"#
    )
}

/// Pick the right body rendering for the reader pane. Separated from
/// `compose_reader_html` so the preference order (sanitized HTML →
/// escaped plaintext → "no body" hint) is easy to read and test.
fn render_body_section(body_html: Option<&str>, body_text: Option<&str>) -> String {
    if let Some(html) = body_html {
        if !html.trim().is_empty() {
            return html.to_string();
        }
    }
    if let Some(text) = body_text {
        if !text.trim().is_empty() {
            return format!("<pre>{}</pre>", minimal_escape(text));
        }
    }
    // `messages_get` already lazy-fetches the body if it's missing
    // locally, so reaching this branch means the message genuinely
    // has no body content (headers-only, or a fetch error already
    // surfaced).
    "<em>No body content available for this message.</em>".to_string()
}

/// Minimal HTML escaping for plaintext content. Not a full sanitizer
/// — only used on fields we know are plain text (subject, plaintext
/// body). HTML bodies go through [`crate::sanitize_email_html`]
/// before reaching this module.
fn minimal_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_wins_when_present() {
        let out = compose_reader_html(Some("<p>hello</p>"), Some("plain"));
        assert!(out.contains("<p>hello</p>"));
        // Plaintext fallback should NOT have been emitted.
        assert!(!out.contains("<pre>plain</pre>"));
    }

    #[test]
    fn plaintext_used_when_html_empty() {
        let out = compose_reader_html(Some("   "), Some("hi & bye"));
        assert!(
            out.contains("<pre>hi &amp; bye</pre>"),
            "plaintext branch did not render: {out}"
        );
    }

    #[test]
    fn plaintext_used_when_html_none() {
        let out = compose_reader_html(None, Some("only text"));
        assert!(out.contains("<pre>only text</pre>"));
    }

    #[test]
    fn falls_back_to_hint_when_both_empty() {
        let out = compose_reader_html(None, None);
        assert!(out.contains("No body content available"));
    }

    #[test]
    fn html_carries_blocked_img_marker_through() {
        // Sanitizer marks blocked imgs with data-qsl-blocked; the
        // wrapper must preserve that attribute byte-for-byte so the
        // CSS selector matches.
        let body = r#"<img data-qsl-blocked alt="hero">"#;
        let out = compose_reader_html(Some(body), None);
        assert!(out.contains("data-qsl-blocked"));
        // CSS rule is also present so the placeholder is actually styled.
        assert!(out.contains("img[data-qsl-blocked]"));
    }

    #[test]
    fn minimal_escape_handles_each_unsafe_char() {
        assert_eq!(minimal_escape("&"), "&amp;");
        assert_eq!(minimal_escape("<"), "&lt;");
        assert_eq!(minimal_escape(">"), "&gt;");
        assert_eq!(minimal_escape("\""), "&quot;");
        assert_eq!(minimal_escape("'"), "&#39;");
        assert_eq!(minimal_escape("plain"), "plain");
    }
}
