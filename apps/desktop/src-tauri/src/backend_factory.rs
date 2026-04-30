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
//! `qsl-sync` is meant to stay backend-agnostic via the
//! `MailBackend` trait. Pulling `qsl-imap-client` and
//! `qsl-jmap-client` into the engine would invert the
//! dependency direction.

use std::sync::Arc;
use std::time::{Duration, Instant};

use qsl_auth::{lookup as provider_lookup, refresh_access_token, TokenVault};
use qsl_core::{Account, AccountId, BackendKind, MailBackend, MailError};
use qsl_imap_client::ImapBackend;
use qsl_jmap_client::JmapBackend;
use qsl_storage::repos;

use crate::state::{AppState, CachedBackend};

/// How long a cached backend may serve foreground commands before it
/// gets rebuilt. Both major providers we support today (Google,
/// Fastmail) issue OAuth access tokens with ~3600s TTLs; rebuilding
/// at 30 minutes leaves a comfortable margin so a still-hot token
/// gets refreshed long before the server starts 401ing.
///
/// The IDLE / EventSource watchers don't go through this cache —
/// they run their own reconnect loop calling `fresh_imap_params` /
/// `fresh_jmap_params` per dial — so push notifications are
/// independently self-healing. This constant only affects the
/// foreground sync + Tauri-command path.
const MAX_BACKEND_AGE: Duration = Duration::from_secs(30 * 60);

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

/// Connection parameters for opening a fresh JMAP `Client` — used by
/// the EventSource watcher to dial a side connection without
/// contending on the cached `MailBackend`'s client mutex.
#[derive(Debug, Clone)]
pub struct JmapDialParams {
    pub session_url: String,
    pub access_token: String,
}

/// Look up `account_id` in the backend cache; on miss (or when the
/// cached entry is older than [`MAX_BACKEND_AGE`]), fetch the account
/// row, refresh its access token, dial the provider, and install
/// the resulting handle in the cache.
///
/// The age-based rebuild is what keeps foreground commands working
/// past the OAuth access-token TTL: without it, an app left running
/// for an hour sees its first post-expiry IMAP / JMAP call return
/// `MailError::Auth` and the cached backend stays poisoned forever.
pub async fn get_or_open(
    state: &AppState,
    account_id: &AccountId,
) -> Result<Arc<dyn MailBackend>, MailError> {
    {
        let mut cache = state.backends.lock().await;
        if let Some(entry) = cache.get(account_id) {
            if entry.built_at.elapsed() < MAX_BACKEND_AGE {
                return Ok(entry.backend.clone());
            }
            // Cached backend is past the OAuth-TTL window. Drop it
            // so the open below builds a fresh one with a refreshed
            // access token.
            tracing::debug!(
                account = %account_id.0,
                age_secs = entry.built_at.elapsed().as_secs(),
                "evicting aged backend cache entry; will rebuild"
            );
            cache.remove(account_id);
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
    let entry = cache
        .entry(account_id.clone())
        .or_insert_with(|| CachedBackend {
            backend: backend.clone(),
            built_at: Instant::now(),
        });
    Ok(entry.backend.clone())
}

/// Drop the cached backend for `account_id` if any. Called when the
/// account is removed by the user or — in a future iteration — when
/// a foreground command catches an explicit `MailError::Auth` and
/// wants to force a rebuild on the next call.
#[allow(dead_code)] // call sites land in the W19 follow-up
pub async fn evict(state: &AppState, account_id: &AccountId) {
    let mut cache = state.backends.lock().await;
    if cache.remove(account_id).is_some() {
        tracing::debug!(account = %account_id.0, "evicted backend cache entry");
    }
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
            let backend = JmapBackend::connect(
                session_url,
                token_set.access.expose(),
                account.id.clone(),
                &account.email_address,
            )
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
/// `qsl_imap_client::dial_session`.
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

/// Refresh the OAuth access token for a JMAP-backed account and
/// return the parameters needed to dial a fresh side `Client` via
/// `qsl_jmap_client::dial_client`. Mirror of
/// [`fresh_imap_params`] for the EventSource watcher.
pub async fn fresh_jmap_params(
    state: &AppState,
    account_id: &AccountId,
) -> Result<JmapDialParams, MailError> {
    let db = state.db.lock().await;
    let account = repos::accounts::get(&*db, account_id).await.map_err(|e| {
        MailError::Other(format!(
            "loading account {} for JMAP push: {e}",
            account_id.0
        ))
    })?;
    drop(db);

    if !matches!(account.kind, BackendKind::Jmap) {
        return Err(MailError::Other(format!(
            "account {} is not JMAP-backed; EventSource doesn't apply",
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

    let session_url = match slug {
        "fastmail" => "https://api.fastmail.com/.well-known/jmap",
        other => {
            return Err(MailError::Other(format!(
                "no hardcoded JMAP session URL for provider {other}"
            )))
        }
    };

    Ok(JmapDialParams {
        session_url: session_url.to_string(),
        access_token: token_set.access.expose().to_string(),
    })
}
