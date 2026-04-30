// SPDX-License-Identifier: Apache-2.0

//! `accounts_*` Tauri commands.
//!
//! See `COMMANDS.md §Accounts` for the catalogue. `accounts_list` is
//! the read path the sidebar consumes; the per-row mutators
//! (`accounts_set_display_name`, `accounts_set_signature`,
//! `accounts_set_notify_enabled`, `accounts_remove`) back the
//! Settings → Accounts tab. `accounts_add_oauth` is still stubbed
//! pending the first-run OAuth flow (PR-O1).

use chrono::Utc;
use qsl_auth::{run_loopback_flow, AuthError, ProviderKind, TokenVault};
use qsl_core::{AccountId, BackendKind};
use qsl_ipc::{Account, IpcResult};
use qsl_storage::repos::accounts as accounts_repo;
use qsl_storage::BlobStore;
use serde::Deserialize;
use tauri::{Emitter, State};

use crate::state::AppState;
use crate::sync_engine;

/// Tauri event fired whenever the set of configured accounts changes
/// (add succeeds, remove). Carries no payload — listeners just refetch
/// `accounts_list`. Used by the main window's sidebar + topbar and
/// the Settings → Accounts panel so the UI doesn't lag behind a
/// mutation done from another window. Defined here rather than in
/// `qsl-ipc` because it's a UI-side coordination signal, not part of
/// the public IPC surface other crates depend on.
pub const ACCOUNTS_EVENT: &str = "accounts_changed";

/// `accounts_list` — return every configured account, ordered by
/// `created_at`. Called by the sidebar on window open and by the
/// Settings → Accounts tab.
#[tauri::command]
pub async fn accounts_list(state: State<'_, AppState>) -> IpcResult<Vec<Account>> {
    let db = state.db.lock().await;
    let accounts = accounts_repo::list(&*db).await?;
    tracing::debug!(count = accounts.len(), "accounts_list");
    Ok(accounts)
}

#[derive(Debug, Deserialize)]
pub struct AccountsSetDisplayNameInput {
    pub id: AccountId,
    pub display_name: String,
}

/// `accounts_set_display_name` — rename an account. The display name
/// is the user-facing label in the sidebar; renaming is rename-safe
/// (id stays put, no folder churn).
#[tauri::command]
pub async fn accounts_set_display_name(
    state: State<'_, AppState>,
    input: AccountsSetDisplayNameInput,
) -> IpcResult<()> {
    let db = state.db.lock().await;
    accounts_repo::set_display_name(&*db, &input.id, &input.display_name).await?;
    tracing::info!(account = %input.id.0, "accounts_set_display_name");
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct AccountsSetSignatureInput {
    pub id: AccountId,
    /// `None` / empty string clears the signature.
    pub signature: Option<String>,
}

/// `accounts_set_signature` — patch the per-account signature
/// appended to outbound mail. The compose pane reads this on draft
/// open; existing drafts are not retroactively rewritten.
#[tauri::command]
pub async fn accounts_set_signature(
    state: State<'_, AppState>,
    input: AccountsSetSignatureInput,
) -> IpcResult<()> {
    let db = state.db.lock().await;
    accounts_repo::set_signature(&*db, &input.id, input.signature.as_deref()).await?;
    tracing::info!(account = %input.id.0, "accounts_set_signature");
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct AccountsSetNotifyEnabledInput {
    pub id: AccountId,
    pub enabled: bool,
}

/// `accounts_set_notify_enabled` — toggle whether new-mail
/// notifications fire for this account. Sync continues either way;
/// only the desktop notification bridge consults the flag.
#[tauri::command]
pub async fn accounts_set_notify_enabled(
    state: State<'_, AppState>,
    input: AccountsSetNotifyEnabledInput,
) -> IpcResult<()> {
    let db = state.db.lock().await;
    accounts_repo::set_notify_enabled(&*db, &input.id, input.enabled).await?;
    tracing::info!(account = %input.id.0, enabled = input.enabled, "accounts_set_notify_enabled");
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct AccountsAddOauthInput {
    /// Provider slug as known to `qsl_auth::lookup` — e.g. `"gmail"`,
    /// `"fastmail"`. The Dioxus side picks from `qsl_auth::provider::builtin`.
    pub provider: String,
    /// Email address the user typed. Becomes the persisted account's
    /// `email_address` and is sent as `login_hint` to the provider so
    /// the OAuth picker lands on the right mailbox.
    pub email: String,
}

/// `accounts_add_oauth` — drive the OAuth2 + PKCE loopback flow for
/// `provider`, persist the resulting account row + refresh token,
/// and kick off a one-shot bootstrap sync so the sidebar populates
/// with folders + recent mail.
///
/// The command blocks for the full flow duration (default cap 5
/// minutes — `qsl_auth::flow::DEFAULT_FLOW_TIMEOUT`) because that's
/// how long we're willing to wait for the user to approve in the
/// browser. The Dioxus side renders a "Waiting for browser approval…"
/// state for the duration; on resolution it closes the oauth-add
/// window and the main window's sidebar refetches via the existing
/// `sync_event` listener.
///
/// Re-add semantics: if the email already has an account row,
/// `update` overwrites it (and the new refresh token replaces the
/// old keychain entry). Same shape as `mailcli auth add`.
#[tauri::command]
pub async fn accounts_add_oauth(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    input: AccountsAddOauthInput,
) -> IpcResult<Account> {
    let AccountsAddOauthInput { provider, email } = input;
    let provider_obj = qsl_auth::lookup(&provider).ok_or_else(|| {
        qsl_ipc::IpcError::new(
            qsl_ipc::IpcErrorKind::NotFound,
            format!("unknown provider: {provider}"),
        )
    })?;

    tracing::info!(provider = %provider, %email, "accounts_add_oauth: starting OAuth + PKCE flow");
    let outcome = run_loopback_flow(provider_obj, Some(&email))
        .await
        .map_err(map_auth_error)?;

    let refresh = outcome.tokens.refresh.ok_or_else(|| {
        qsl_ipc::IpcError::new(
            qsl_ipc::IpcErrorKind::Internal,
            "provider returned no refresh_token (user must include offline_access scope)"
                .to_string(),
        )
    })?;

    let kind = match provider_obj.profile().kind {
        ProviderKind::ImapSmtp => BackendKind::ImapSmtp,
        ProviderKind::Jmap => BackendKind::Jmap,
        _ => {
            return Err(qsl_ipc::IpcError::new(
                qsl_ipc::IpcErrorKind::Internal,
                format!("provider {provider} uses an unsupported backend kind"),
            ));
        }
    };

    let account_id = AccountId(format!("{provider}:{email}"));
    let account = Account {
        id: account_id.clone(),
        kind: kind.clone(),
        display_name: email.clone(),
        email_address: email.clone(),
        created_at: Utc::now(),
        signature: None,
        notify_enabled: true,
    };

    {
        let db = state.db.lock().await;
        match accounts_repo::find(&*db, &account.id).await? {
            Some(_) => {
                tracing::info!(account = %account.id.0, "accounts_add_oauth: re-add — updating");
                accounts_repo::update(&*db, &account).await?;
            }
            None => {
                accounts_repo::insert(&*db, &account).await?;
            }
        }
    }

    let vault = TokenVault::new();
    vault.put(&account_id, &refresh).await.map_err(|e| {
        qsl_ipc::IpcError::new(
            qsl_ipc::IpcErrorKind::Internal,
            format!("persist refresh token: {e}"),
        )
    })?;

    // Kick a bootstrap sync. `sync_one_account` walks list_folders →
    // sync_folder per folder and emits a `sync_event` for each, which
    // the UI's existing listener consumes to bump `sync_tick` and
    // refetch the sidebar's account list. Failure is non-fatal: the
    // account row is already persisted, the user can hit refresh.
    let blobs = BlobStore::new(state.data_dir.join("blobs"));
    let app_for_sync = app.clone();
    let account_id_for_sync = account_id.clone();
    tauri::async_runtime::spawn(async move {
        sync_engine::sync_one_account(&app_for_sync, &blobs, &account_id_for_sync).await;
    });

    if let Err(e) = app.emit(ACCOUNTS_EVENT, ()) {
        tracing::warn!("accounts_add_oauth: emit accounts_changed failed: {e}");
    }

    tracing::info!(
        account = %account.id.0,
        scopes = outcome.granted_scopes.len(),
        "accounts_add_oauth: complete"
    );
    Ok(account)
}

fn map_auth_error(e: AuthError) -> qsl_ipc::IpcError {
    use qsl_ipc::IpcErrorKind;
    let kind = match &e {
        AuthError::Browser(_) => IpcErrorKind::Internal,
        AuthError::AuthResponse(_) => IpcErrorKind::Permission,
        AuthError::TokenExchange(_) => IpcErrorKind::Permission,
        _ => IpcErrorKind::Internal,
    };
    qsl_ipc::IpcError::new(kind, format!("OAuth flow: {e}"))
}

#[derive(Debug, Deserialize)]
pub struct AccountsRemoveInput {
    pub id: AccountId,
}

/// `accounts_remove` — delete an account row. Schema-level cascades
/// take care of folders / messages / threads / outbox / contacts;
/// the in-memory `state.backends` cache is also purged so a
/// re-added-immediately account doesn't hit the stale handle. Fires
/// `ACCOUNTS_EVENT` so the main window's sidebar + topbar refetch
/// immediately rather than waiting for the user to manually refresh.
#[tauri::command]
pub async fn accounts_remove(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    input: AccountsRemoveInput,
) -> IpcResult<()> {
    let db = state.db.lock().await;
    accounts_repo::delete(&*db, &input.id).await?;
    drop(db);
    state.backends.lock().await.remove(&input.id);
    if let Err(e) = app.emit(ACCOUNTS_EVENT, ()) {
        tracing::warn!("accounts_remove: emit accounts_changed failed: {e}");
    }
    tracing::info!(account = %input.id.0, "accounts_remove");
    Ok(())
}
