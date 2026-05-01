// SPDX-License-Identifier: Apache-2.0

//! Integration test for [`qsl_auth::run_loopback_flow_with`] against a
//! mock OAuth2 authorization server.
//!
//! The test exercises the full happy path the production code follows
//! against Google / Fastmail — auth URL construction, browser
//! navigation (faked), loopback redirect capture, code exchange — but
//! everything happens on `127.0.0.1` so CI can run it without
//! credentials, browsers, or a working network.
//!
//! `mock_oauth::MockOAuthServer` is the helper; the closure passed to
//! `run_loopback_flow_with` plays the role of the user clicking
//! through the consent page (it just `reqwest::get`s the auth URL,
//! and the mock server immediately 302s back to the loopback redirect
//! URI with `?code=...&state=...`).

mod mock_oauth;

use qsl_auth::{run_loopback_flow_with, AuthError, OAuthProvider, ProviderKind, ProviderProfile};

use mock_oauth::{
    MockOAuthServer, MOCK_ACCESS_TOKEN, MOCK_EXPIRES_IN, MOCK_GRANTED_SCOPE, MOCK_REFRESH_TOKEN,
};

/// Test-only `OAuthProvider` whose profile points at a mock server.
/// We leak its strings into `'static` because `ProviderProfile` is
/// `&'static str` everywhere — fine for a one-shot test process.
struct TestProvider {
    profile: &'static ProviderProfile,
}

impl OAuthProvider for TestProvider {
    fn profile(&self) -> &'static ProviderProfile {
        self.profile
    }
}

fn leak_static_profile(authorize_url: String, token_url: String) -> &'static ProviderProfile {
    let profile = ProviderProfile {
        name: "MockProvider",
        slug: "mock",
        client_id: "mock-client-id",
        client_secret: "",
        authorization_url: Box::leak(authorize_url.into_boxed_str()),
        token_url: Box::leak(token_url.into_boxed_str()),
        revocation_url: "",
        scopes: &["scope-a", "scope-b"],
        kind: ProviderKind::ImapSmtp,
    };
    Box::leak(Box::new(profile))
}

#[tokio::test]
async fn loopback_flow_round_trips_against_mock_server() {
    // Spawn the mock authorization + token server.
    let server = MockOAuthServer::start().await;
    let provider = TestProvider {
        profile: leak_static_profile(server.authorize_url.clone(), server.token_url.clone()),
    };

    // Drive the flow. The "browser" is a tokio task that GETs the auth
    // URL and follows the 302 the mock issues — which lands on the
    // loopback listener `run_loopback_flow_with` is awaiting.
    let outcome = run_loopback_flow_with(&provider, Some("user@example.test"), |auth_url| {
        let auth_url = auth_url.to_string();
        tokio::spawn(async move {
            // reqwest follows redirects up to 10 hops by default, so a
            // single GET on the auth URL is enough to drive the mock
            // /authorize → loopback redirect → loopback success page.
            let _ = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::limited(10))
                .build()
                .expect("reqwest build")
                .get(&auth_url)
                .send()
                .await;
        });
        Ok::<(), AuthError>(())
    })
    .await
    .expect("loopback flow ok");

    assert_eq!(outcome.tokens.access.expose(), MOCK_ACCESS_TOKEN);
    let refresh = outcome
        .tokens
        .refresh
        .as_ref()
        .expect("refresh token present");
    assert_eq!(refresh.expose(), MOCK_REFRESH_TOKEN);
    let expires_at = outcome
        .tokens
        .expires_at
        .expect("expires_at populated from expires_in");
    let now = chrono::Utc::now();
    let delta = (expires_at - now).num_seconds();
    assert!(
        (MOCK_EXPIRES_IN - 5..=MOCK_EXPIRES_IN + 5).contains(&delta),
        "expires_at should be ~{MOCK_EXPIRES_IN}s in the future, was {delta}s"
    );
    let granted: Vec<&str> = outcome.granted_scopes.iter().map(String::as_str).collect();
    let expected: Vec<&str> = MOCK_GRANTED_SCOPE.split_whitespace().collect();
    assert_eq!(granted, expected);
}

#[tokio::test]
async fn loopback_flow_propagates_browser_open_failure() {
    // If the closure passed to `run_loopback_flow_with` returns Err,
    // the flow surfaces it before binding the loopback listener for
    // its read. This guards against a regression where a swallowed
    // browser-open error would leave the flow waiting on a redirect
    // that never comes.
    let server = MockOAuthServer::start().await;
    let provider = TestProvider {
        profile: leak_static_profile(server.authorize_url.clone(), server.token_url.clone()),
    };

    let result = run_loopback_flow_with(&provider, None, |_| {
        Err(AuthError::Browser("synthetic browser failure".into()))
    })
    .await;

    let err = result.expect_err("expected error from injected closure");
    assert!(
        matches!(err, AuthError::Browser(_)),
        "expected Browser variant, got: {err:?}"
    );
}
