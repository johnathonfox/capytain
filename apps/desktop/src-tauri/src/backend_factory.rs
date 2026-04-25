// SPDX-License-Identifier: Apache-2.0

//! Build live `MailBackend` handles for stored accounts.
//!
//! `messages_get`'s lazy-fetch path calls [`get_or_open`] when a
//! reader-pane request arrives for a header-only row: it consults
//! the per-account cache on `AppState`, refreshes the OAuth token
//! and connects to the provider on a miss, then hands back an
//! `Arc<dyn MailBackend>` the caller can drive.
//!
//! Mirrors `mailcli::open_backend` — the two are kept in lockstep
//! deliberately rather than extracted into a shared crate, since
//! `capytain-sync` is meant to stay backend-agnostic via the
//! `MailBackend` trait. Pulling `capytain-imap-client` and
//! `capytain-jmap-client` into the engine would invert the
//! dependency direction.

use std::sync::Arc;

use capytain_auth::{lookup as provider_lookup, refresh_access_token, TokenVault};
use capytain_core::{Account, AccountId, BackendKind, MailBackend, MailError};
use capytain_imap_client::ImapBackend;
use capytain_jmap_client::JmapBackend;
use capytain_storage::repos;

use crate::state::AppState;

/// Connection parameters for opening a fresh IMAP session — used by
/// the IDLE watcher to dial side connections without going through
/// the cached `MailBackend`.
#[derive(Debug, Clone)]
pub struct ImapDialParams {
    pub host: String,
    pub port: u16,
    pub email: String,
    pub access_token: String,
}

/// Look up `account_id` in the backend cache; on miss, fetch the
/// account row, refresh its access token, dial the provider, and
/// install the resulting handle in the cache.
pub async fn get_or_open(
    state: &AppState,
    account_id: &AccountId,
) -> Result<Arc<dyn MailBackend>, MailError> {
    {
        let cache = state.backends.lock().await;
        if let Some(backend) = cache.get(account_id) {
            return Ok(backend.clone());
        }
    }

    let db = state.db.lock().await;
    let account = repos::accounts::get(&*db, account_id).await.map_err(|e| {
        MailError::Other(format!(
            "loading account {} for backend factory: {e}",
            account_id.0
        ))
    })?;
    drop(db);

    let backend = open(&account).await?;

    let mut cache = state.backends.lock().await;
    let entry = cache.entry(account_id.clone()).or_insert(backend);
    Ok(entry.clone())
}

/// Build a fresh `MailBackend` for `account` without going through
/// the cache. Splits provider slug → profile lookup → token refresh
/// → adapter construction.
async fn open(account: &Account) -> Result<Arc<dyn MailBackend>, MailError> {
    let slug = provider_slug_from_id(&account.id).ok_or_else(|| {
        MailError::Other(format!(
            "account id {} does not follow `<provider>:<email>`",
            account.id.0
        ))
    })?;
    let provider = provider_lookup(slug)
        .ok_or_else(|| MailError::Other(format!("unknown provider: {slug}")))?;
    let vault = TokenVault::new();
    let token_set = refresh_access_token(provider, &vault, &account.id)
        .await
        .map_err(|e| MailError::Auth(format!("refresh access token: {e}")))?;

    match account.kind {
        BackendKind::ImapSmtp => {
            let host = match slug {
                "gmail" => "imap.gmail.com",
                other => {
                    return Err(MailError::Other(format!(
                        "no hardcoded IMAP host for provider {other}"
                    )))
                }
            };
            let backend = ImapBackend::connect_tls(
                host,
                993,
                &account.email_address,
                token_set.access.expose(),
                account.id.clone(),
            )
            .await?;
            Ok(Arc::new(backend))
        }
        BackendKind::Jmap => {
            let session_url = match slug {
                "fastmail" => "https://api.fastmail.com/.well-known/jmap",
                other => {
                    return Err(MailError::Other(format!(
                        "no hardcoded JMAP session URL for provider {other}"
                    )))
                }
            };
            let backend =
                JmapBackend::connect(session_url, token_set.access.expose(), account.id.clone())
                    .await?;
            Ok(Arc::new(backend))
        }
        _ => Err(MailError::Other(format!(
            "account {} uses an unsupported backend kind",
            account.id.0
        ))),
    }
}

fn provider_slug_from_id(id: &AccountId) -> Option<&str> {
    id.0.split_once(':').map(|(slug, _)| slug)
}

/// Refresh the OAuth access token for an IMAP-backed account and
/// return the parameters needed to dial a fresh side session via
/// `capytain_imap_client::dial_session`.
///
/// Used by the IDLE watcher to open an additional connection per
/// folder (separate from the cached `MailBackend` that serves sync
/// requests). Each call hits the auth refresh endpoint, so the
/// caller should debounce — typically called once per reconnect
/// cycle, not in tight loops.
///
/// Returns an error if the account is JMAP-backed; JMAP push lives
/// behind EventSource (Phase 1 Week 11), not IMAP IDLE.
pub async fn fresh_imap_params(
    state: &AppState,
    account_id: &AccountId,
) -> Result<ImapDialParams, MailError> {
    let db = state.db.lock().await;
    let account = repos::accounts::get(&*db, account_id)
        .await
        .map_err(|e| MailError::Other(format!("loading account {} for IDLE: {e}", account_id.0)))?;
    drop(db);

    if !matches!(account.kind, BackendKind::ImapSmtp) {
        return Err(MailError::Other(format!(
            "account {} is not IMAP-backed; IDLE doesn't apply",
            account.id.0
        )));
    }

    let slug = provider_slug_from_id(&account.id).ok_or_else(|| {
        MailError::Other(format!(
            "account id {} does not follow `<provider>:<email>`",
            account.id.0
        ))
    })?;
    let provider = provider_lookup(slug)
        .ok_or_else(|| MailError::Other(format!("unknown provider: {slug}")))?;
    let vault = TokenVault::new();
    let token_set = refresh_access_token(provider, &vault, &account.id)
        .await
        .map_err(|e| MailError::Auth(format!("refresh access token: {e}")))?;

    let host = match slug {
        "gmail" => "imap.gmail.com",
        other => {
            return Err(MailError::Other(format!(
                "no hardcoded IMAP host for provider {other}"
            )))
        }
    };

    Ok(ImapDialParams {
        host: host.to_string(),
        port: 993,
        email: account.email_address,
        access_token: token_set.access.expose().to_string(),
    })
}
