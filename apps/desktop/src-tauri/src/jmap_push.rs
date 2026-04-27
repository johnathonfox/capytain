// SPDX-License-Identifier: Apache-2.0

//! JMAP EventSource watcher orchestration for the desktop sync engine.
//!
//! Mirror of [`crate::imap_idle`] for JMAP accounts. [`spawn_watcher`]
//! runs a self-restarting tokio task that:
//!
//! 1. Calls [`backend_factory::fresh_jmap_params`] to refresh the
//!    account's OAuth access token.
//! 2. Dials a fresh side `Client` via
//!    [`qsl_jmap_client::dial_client`].
//! 3. Hands the client to [`qsl_jmap_client::watch_account`],
//!    which reads the EventSource stream and emits
//!    [`BackendEvent::AccountChanged`] over `tx` on every push.
//!
//! On any error (connect, auth, transport, parse), the task emits
//! [`BackendEvent::ConnectionLost`], sleeps with exponential backoff
//! (2s → 4s → ... capped at 5 min), and restarts. Receiver dropping
//! `tx` ends the task cleanly.

use std::time::Duration;

use qsl_core::{AccountId, BackendEvent};
use qsl_jmap_client::{dial_client, watch_account};
use tauri::{AppHandle, Manager};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::backend_factory;
use crate::state::AppState;

const INITIAL_BACKOFF: Duration = Duration::from_secs(2);
const MAX_BACKOFF: Duration = Duration::from_secs(5 * 60);

/// Spawn a JMAP push watcher for one account. Returns the
/// `JoinHandle` so the caller can abort on shutdown.
pub fn spawn_watcher(
    app: AppHandle,
    account_id: AccountId,
    tx: mpsc::Sender<BackendEvent>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        info!(account = %account_id.0, "spawning JMAP EventSource watcher");
        let mut backoff = INITIAL_BACKOFF;
        loop {
            match watch_one_session(&app, &account_id, &tx).await {
                Ok(()) => return,
                Err(e) => {
                    warn!(
                        account = %account_id.0,
                        "JMAP push watcher error: {e}; reconnecting in {backoff:?}"
                    );
                    if tx.send(BackendEvent::ConnectionLost).await.is_err() {
                        return;
                    }
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                    if tx.send(BackendEvent::ConnectionRestored).await.is_err() {
                        return;
                    }
                }
            }
        }
    })
}

async fn watch_one_session(
    app: &AppHandle,
    account_id: &AccountId,
    tx: &mpsc::Sender<BackendEvent>,
) -> Result<(), qsl_core::MailError> {
    let state: tauri::State<'_, AppState> = app.state();
    let params = backend_factory::fresh_jmap_params(&state, account_id).await?;
    let client = dial_client(&params.session_url, &params.access_token).await?;
    watch_account(&client, account_id.clone(), tx.clone()).await
}
