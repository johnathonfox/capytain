// SPDX-License-Identifier: Apache-2.0

//! Hand-rolled mock OAuth2 authorization server for integration testing
//! [`qsl_auth::run_loopback_flow_with`]. Hand-rolled rather than pulled
//! from a crate so the test stays in the same dependency footprint as
//! `crates/auth/src/loopback.rs` (raw tokio sockets + a request-line
//! parser; no axum, no warp).
//!
//! Endpoints:
//!
//! - `GET /authorize?...` — extracts `redirect_uri` and `state`, returns
//!   `302 Found` with `Location: <redirect_uri>?code=test-code&state=<state>`.
//!   Bypasses any user-consent step; the simulated browser is implicitly
//!   "approve and redirect immediately."
//!
//! - `POST /token` — accepts a form-encoded body, asserts the expected
//!   fields are present (`grant_type=authorization_code`, `code`,
//!   `code_verifier`, `redirect_uri`, `client_id`), returns 200 JSON
//!   with synthetic `access_token`, `refresh_token`, `expires_in`,
//!   `scope`, and `token_type`.
//!
//! Used by `tests/oauth_loopback_flow.rs` — kept as a separate file so
//! the helper can grow without crowding the test that uses it.

use std::collections::HashMap;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// Synthetic tokens the mock returns on `POST /token`. Constants so the
/// integration test can assert them verbatim.
pub const MOCK_ACCESS_TOKEN: &str = "mock-access-token-value";
pub const MOCK_REFRESH_TOKEN: &str = "mock-refresh-token-value";
pub const MOCK_EXPIRES_IN: i64 = 3600;
pub const MOCK_GRANTED_SCOPE: &str = "scope-a scope-b";

/// A running mock OAuth2 server. Drop the handle to stop accepting
/// new connections; the in-flight handler tasks finish on their own.
pub struct MockOAuthServer {
    pub authorize_url: String,
    pub token_url: String,
    _accept_task: JoinHandle<()>,
}

impl MockOAuthServer {
    /// Bind on `127.0.0.1:0`, return immediately. The accept loop runs
    /// in a background task; each connection is parsed once and closed.
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("mock bind");
        let addr = listener.local_addr().expect("mock local_addr");
        let base = format!("http://{addr}");
        let authorize_url = format!("{base}/authorize");
        let token_url = format!("{base}/token");

        let accept_task = tokio::spawn(async move {
            while let Ok((socket, _peer)) = listener.accept().await {
                tokio::spawn(handle(socket));
            }
        });

        Self {
            authorize_url,
            token_url,
            _accept_task: accept_task,
        }
    }
}

async fn handle(mut socket: TcpStream) {
    // Buffer up to ~16 KB. OAuth token-exchange bodies are tiny.
    const MAX: usize = 16 * 1024;
    let mut buf = vec![0u8; MAX];
    let mut read = 0;

    // First pass: read until `\r\n\r\n` (end of headers).
    let body_start = loop {
        if read >= MAX {
            return;
        }
        let n = match tokio::time::timeout(Duration::from_secs(5), socket.read(&mut buf[read..]))
            .await
        {
            Ok(Ok(0)) | Err(_) => return,
            Ok(Ok(n)) => n,
            Ok(Err(_)) => return,
        };
        read += n;
        if let Some(pos) = find_subsequence(&buf[..read], b"\r\n\r\n") {
            break pos + 4;
        }
    };

    // Parse request line + headers, **owning** the strings we keep so
    // the second read (which mutates `buf`) doesn't fight the borrow.
    let (method, target, content_length) = {
        let header_text = match std::str::from_utf8(&buf[..body_start - 4]) {
            Ok(s) => s,
            Err(_) => return,
        };
        let mut lines = header_text.split("\r\n");
        let request_line = lines.next().unwrap_or("").to_string();
        let mut headers: HashMap<String, String> = HashMap::new();
        for line in lines {
            if let Some((k, v)) = line.split_once(':') {
                headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
            }
        }
        let mut parts = request_line.splitn(3, ' ');
        let method = parts.next().unwrap_or("").to_string();
        let target = parts.next().unwrap_or("").to_string();
        let content_length: usize = headers
            .get("content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        (method, target, content_length)
    };

    // Second pass: read any remaining body bytes.
    while read < body_start + content_length {
        match socket.read(&mut buf[read..]).await {
            Ok(0) => break,
            Ok(n) => read += n,
            Err(_) => return,
        }
    }
    let body_end = (body_start + content_length).min(read);
    let body = &buf[body_start..body_end];

    let response: Vec<u8> = match (method.as_str(), target.as_str()) {
        ("GET", t) if t.starts_with("/authorize") => handle_authorize(t),
        ("POST", "/token") => handle_token(body),
        _ => http_response(404, "text/plain", b"not found".to_vec()),
    };

    let _ = socket.write_all(&response).await;
    let _ = socket.shutdown().await;
}

/// `/authorize` redirects to the supplied `redirect_uri` with a fixed
/// `code` and the original `state` echoed back. No real consent UI.
fn handle_authorize(target: &str) -> Vec<u8> {
    let url = url::Url::parse(&format!("http://127.0.0.1{target}")).expect("parse target");
    let mut redirect_uri = None;
    let mut state = None;
    for (k, v) in url.query_pairs() {
        match k.as_ref() {
            "redirect_uri" => redirect_uri = Some(v.into_owned()),
            "state" => state = Some(v.into_owned()),
            _ => {}
        }
    }
    let Some(redirect_uri) = redirect_uri else {
        return http_response(400, "text/plain", b"missing redirect_uri".to_vec());
    };
    let Some(state) = state else {
        return http_response(400, "text/plain", b"missing state".to_vec());
    };

    let separator = if redirect_uri.contains('?') { '&' } else { '?' };
    let location = format!(
        "{redirect_uri}{separator}code=mock-code&state={percent}",
        percent = state
    );
    let body = b"<a href=\"see other\">redirect</a>".to_vec();
    let response = format!(
        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: {len}\r\n\
         Connection: close\r\nContent-Type: text/html\r\n\r\n",
        len = body.len()
    );
    let mut out = response.into_bytes();
    out.extend_from_slice(&body);
    out
}

/// `/token` validates the form fields and returns a synthetic token JSON.
fn handle_token(body: &[u8]) -> Vec<u8> {
    let body_str = match std::str::from_utf8(body) {
        Ok(s) => s,
        Err(_) => return http_response(400, "text/plain", b"non-utf8 body".to_vec()),
    };
    let fields: HashMap<&str, String> = body_str
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| {
            let v = url::form_urlencoded::parse(v.as_bytes())
                .next()
                .map(|(s, _)| s.into_owned())
                .unwrap_or_else(|| v.to_string());
            (k, v)
        })
        .collect();
    for required in [
        "grant_type",
        "code",
        "code_verifier",
        "redirect_uri",
        "client_id",
    ] {
        if !fields.contains_key(required) {
            return http_response(
                400,
                "application/json",
                format!(
                    r#"{{"error":"invalid_request","error_description":"missing {required}"}}"#
                )
                .into_bytes(),
            );
        }
    }
    if fields.get("grant_type").map(String::as_str) != Some("authorization_code") {
        return http_response(
            400,
            "application/json",
            br#"{"error":"unsupported_grant_type"}"#.to_vec(),
        );
    }
    if fields.get("code").map(String::as_str) != Some("mock-code") {
        return http_response(
            400,
            "application/json",
            br#"{"error":"invalid_grant","error_description":"unknown code"}"#.to_vec(),
        );
    }
    let payload = format!(
        r#"{{"access_token":"{a}","refresh_token":"{r}","expires_in":{e},"scope":"{s}","token_type":"Bearer"}}"#,
        a = MOCK_ACCESS_TOKEN,
        r = MOCK_REFRESH_TOKEN,
        e = MOCK_EXPIRES_IN,
        s = MOCK_GRANTED_SCOPE,
    );
    http_response(200, "application/json", payload.into_bytes())
}

fn http_response(status: u16, content_type: &str, body: Vec<u8>) -> Vec<u8> {
    let reason = match status {
        200 => "OK",
        302 => "Found",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Other",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\n\
         Content-Length: {len}\r\nConnection: close\r\n\r\n",
        len = body.len()
    );
    let mut out = head.into_bytes();
    out.extend_from_slice(&body);
    out
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}
