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
use tracing::{debug, info};

use qsl_core::AccountId;

use crate::error::AuthError;
use crate::keyring::TokenVault;
use crate::provider::OAuthProvider;
use crate::tokens::{AccessToken, RefreshToken, TokenSet};

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

    let tokens = post_refresh(
        profile.token_url,
        client_id,
        profile.client_secret,
        &refresh,
    )
    .await?;

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
