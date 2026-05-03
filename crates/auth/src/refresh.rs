// SPDX-License-Identifier: Apache-2.0

//! Access-token refresh helper.
//!
//! This is the single code path the IMAP and JMAP adapters will use to
//! ask "what's a fresh access token for this account?" (Week 4 wires it
//! in). Callers are expected to cache the returned [`TokenSet`]
//! in-memory; the vault is hit only for the long-lived refresh token.

use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use serde::Deserialize;
use tracing::{debug, info, warn};

use qsl_core::AccountId;

use crate::error::AuthError;
use crate::keyring::TokenVault;
use crate::provider::OAuthProvider;
use crate::tokens::{AccessToken, RefreshToken, TokenSet};

/// Best-effort POST to the provider's RFC 7009 revocation endpoint to
/// invalidate a refresh token server-side. Returns `Ok(false)` if the
/// provider didn't publish a static revocation URL (Fastmail today —
/// see the comment on `ProviderProfile::revocation_url`); `Ok(true)`
/// on a successful revoke; `Err` on transport / 4xx / 5xx.
///
/// Callers (`accounts_remove`) treat this as best-effort: a failure
/// (offline, provider 5xx, expired token) is logged but never blocks
/// the local keychain + DB cleanup. The token still becomes useless
/// once the keychain entry is gone — server-side revocation is a
/// belt-and-braces measure for the case where the token was already
/// exfiltrated.
///
/// 5-second timeout because remove flows shouldn't stall the UI on
/// a flaky network — `webbrowser::open` and the JMAP/IMAP auth
/// retries are the realistic latency budget; revoke is a
/// fire-and-(mostly-)forget extra.
pub async fn revoke_refresh_token(
    provider: &dyn OAuthProvider,
    refresh: &RefreshToken,
) -> Result<bool, AuthError> {
    let profile = provider.profile();
    if profile.revocation_url.is_empty() {
        debug!(
            provider = profile.slug,
            "no revocation_url configured; skipping server-side revoke"
        );
        return Ok(false);
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| AuthError::Other(format!("reqwest build: {e}")))?;

    // RFC 7009 §2.1: POST application/x-www-form-urlencoded with
    // `token=<token>`. `token_type_hint=refresh_token` is optional but
    // helps providers route to the right revocation path. Google
    // accepts both `token=` and a query-string variant; the body
    // form is the standards-compliant shape.
    let resp = qsl_telemetry::time_op!(
        target: "qsl::slow::auth",
        limit_ms: qsl_telemetry::slow::limits::OAUTH_TOKEN_MS,
        op: "token_revoke",
        fields: { provider = %profile.slug },
        client
            .post(profile.revocation_url)
            .header("Accept", "application/json")
            .form(&[
                ("token", refresh.expose()),
                ("token_type_hint", "refresh_token"),
            ])
            .send()
    )
    .map_err(|e| AuthError::Other(format!("revoke HTTP: {e}")))?;

    let status = resp.status();
    if status.is_success() {
        info!(provider = profile.slug, "refresh token revoked at provider");
        Ok(true)
    } else {
        // Read up to ~200 chars of the body so the warn is debuggable
        // without flooding logs on a long HTML 502.
        let raw = resp.text().await.unwrap_or_default();
        let snippet: String = raw.chars().take(200).collect();
        warn!(
            provider = profile.slug,
            %status,
            "revoke endpoint returned non-success: {snippet:?}"
        );
        Err(AuthError::Other(format!(
            "revoke endpoint returned HTTP {status}"
        )))
    }
}

/// Exchange a stored refresh token for a fresh access token.
///
/// If the provider returns a rotated refresh token (some do; Google
/// typically doesn't, Fastmail sometimes does), the vault is updated
/// atomically — a later `refresh` won't reuse an expired token.
pub async fn refresh_access_token(
    provider: &dyn OAuthProvider,
    vault: &TokenVault,
    account: &AccountId,
) -> Result<TokenSet, AuthError> {
    let profile = provider.profile();
    let client_id = profile.require_client_id()?;
    let refresh = vault.get(account).await?;

    debug!(account = %account.0, provider = profile.slug, "refreshing access token");

    let tokens = qsl_telemetry::time_op!(
        target: "qsl::slow::auth",
        limit_ms: qsl_telemetry::slow::limits::OAUTH_TOKEN_MS,
        op: "token_refresh",
        fields: { provider = %profile.slug, account = %account.0 },
        post_refresh(
            profile.token_url,
            client_id,
            profile.client_secret,
            &refresh,
        )
    )?;

    // If the provider rotated the refresh token, store the new one.
    if let Some(new_refresh) = &tokens.refresh {
        if new_refresh.expose() != refresh.expose() {
            info!(account = %account.0, "rotating refresh token in keychain");
            vault.put(account, new_refresh).await?;
        }
    }

    Ok(tokens)
}

/// Return an access token that is known to be valid right now, using a
/// refresh only when `cached` is absent or already stale.
///
/// The caller owns the cache; we take it mutably so a refresh can update
/// it in place. Cache is just an `Option<TokenSet>` — no LRU, one
/// entry-per-account is what the sync engine needs.
pub async fn access_token_for(
    provider: &dyn OAuthProvider,
    vault: &TokenVault,
    account: &AccountId,
    cached: &mut Option<TokenSet>,
) -> Result<AccessToken, AuthError> {
    if let Some(tokens) = cached.as_ref() {
        if !tokens.is_expired() {
            return Ok(tokens.access.clone());
        }
    }
    let fresh = refresh_access_token(provider, vault, account).await?;
    let access = fresh.access.clone();
    *cached = Some(fresh);
    Ok(access)
}

// ---------- wire format ----------

#[derive(Debug, Deserialize)]
struct RefreshResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

async fn post_refresh(
    endpoint: &str,
    client_id: &str,
    client_secret: &str,
    refresh: &RefreshToken,
) -> Result<TokenSet, AuthError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| AuthError::Other(format!("reqwest build: {e}")))?;

    let mut form: Vec<(&str, &str)> = vec![
        ("grant_type", "refresh_token"),
        ("client_id", client_id),
        ("refresh_token", refresh.expose()),
    ];
    if !client_secret.is_empty() {
        form.push(("client_secret", client_secret));
    }

    let resp = client
        .post(endpoint)
        .header("Accept", "application/json")
        .form(&form)
        .send()
        .await
        .map_err(|e| AuthError::TokenExchange(format!("HTTP error: {e}")))?;

    let status = resp.status();
    let body: RefreshResponse = resp
        .json()
        .await
        .map_err(|e| AuthError::TokenExchange(format!("JSON parse: {e}")))?;

    if !status.is_success() || body.error.is_some() {
        // Classify 4xx from the token endpoint as an auth failure — the
        // caller will typically surface this as "re-authenticate".
        let msg = body
            .error_description
            .or(body.error)
            .unwrap_or_else(|| format!("HTTP {status}"));
        return Err(AuthError::TokenExchange(msg));
    }

    let expires_at = body
        .expires_in
        .map(|n| Utc::now() + ChronoDuration::seconds(n));

    Ok(TokenSet {
        access: AccessToken(body.access_token),
        refresh: body.refresh_token.map(RefreshToken),
        expires_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ProviderKind, ProviderProfile};

    /// A provider with `revocation_url: ""` short-circuits without
    /// touching the network. Locks the contract so a future "make it
    /// fall through to a default endpoint" change has to consciously
    /// remove this regression.
    #[tokio::test]
    async fn revoke_short_circuits_on_empty_url() {
        struct NoRevokeProvider;
        impl OAuthProvider for NoRevokeProvider {
            fn profile(&self) -> &'static ProviderProfile {
                static P: ProviderProfile = ProviderProfile {
                    name: "Test",
                    slug: "test",
                    client_id: "x",
                    client_secret: "",
                    authorization_url: "https://example.test/auth",
                    token_url: "https://example.test/token",
                    revocation_url: "",
                    scopes: &[],
                    kind: ProviderKind::ImapSmtp,
                };
                &P
            }
        }

        let result = revoke_refresh_token(&NoRevokeProvider, &RefreshToken("x".into())).await;
        assert!(matches!(result, Ok(false)));
    }
}
