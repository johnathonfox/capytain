// SPDX-License-Identifier: Apache-2.0

//! IMAP IDLE watcher orchestration for the desktop sync engine.
//!
//! [`spawn_watcher`] runs a self-restarting tokio task that:
//!
//! 1. Calls [`backend_factory::fresh_imap_params`] to refresh the
//!    account's OAuth access token.
//! 2. Dials a fresh side session via
//!    [`qsl_imap_client::dial_session`].
//! 3. Hands the session to
//!    [`qsl_imap_client::watch_folder`], which runs the IDLE
//!    state machine and emits [`BackendEvent::FolderChanged`] over
//!    `tx` on every untagged response.
//!
//! On any error (connect, auth, IDLE protocol, socket drop), the
//! task emits [`BackendEvent::ConnectionLost`], sleeps with jittered
//! exponential backoff (2s → 4s → ... capped at 5 min, ±50% per
//! sleep), and restarts from step 1. The receiver dropping `tx` ends
//! the task cleanly. Jitter is what keeps a multi-account box from
//! reconnecting in lock-step after a Wi-Fi flap.

use std::time::Duration;

use qsl_core::{AccountId, BackendEvent, FolderId};
use qsl_imap_client::{dial_session, watch_folder};
use tauri::{AppHandle, Manager};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::backend_factory;
use crate::reconnect::jittered;
use crate::state::AppState;

/// Initial reconnect delay. Doubles on each failure up to [`MAX_BACKOFF`].
const INITIAL_BACKOFF: Duration = Duration::from_secs(2);
/// Cap on the exponential backoff. 5 minutes matches what mail
/// clients typically settle to before a user notices.
const MAX_BACKOFF: Duration = Duration::from_secs(5 * 60);

/// Spawn a watcher task for one (account, folder) pair. Returns the
/// `JoinHandle` so the caller can abort the task on shutdown.
pub fn spawn_watcher(
    app: AppHandle,
    account_id: AccountId,
    folder: FolderId,
    tx: mpsc::Sender<BackendEvent>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        info!(
            account = %account_id.0,
            folder = %folder.0,
            "spawning IMAP IDLE watcher"
        );
        let mut backoff = INITIAL_BACKOFF;
        loop {
            match watch_one_session(&app, &account_id, &folder, &tx).await {
                Ok(()) => {
                    // Receiver dropped or ManualInterrupt — caller is
                    // shutting us down, exit cleanly.
                    return;
                }
                Err(e) => {
                    warn!(
                        account = %account_id.0,
                        folder = %folder.0,
                        "IMAP IDLE watcher error: {e}; reconnecting in {backoff:?}"
                    );
                    if tx.send(BackendEvent::ConnectionLost).await.is_err() {
                        return;
                    }
                    tokio::time::sleep(jittered(backoff)).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                    // Successful reconnect resets backoff inside the
                    // loop body via the Ok(()) path on the next
                    // dial — but failures here keep stretching it.
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
    folder: &FolderId,
    tx: &mpsc::Sender<BackendEvent>,
) -> Result<(), qsl_core::MailError> {
    let state: tauri::State<'_, AppState> = app.state();
    let params = backend_factory::fresh_imap_params(&state, account_id).await?;
    let session = dial_session(
        &params.host,
        params.port,
        &params.email,
        &params.access_token,
    )
    .await?
    .session;
    watch_folder(session, folder.clone(), account_id.clone(), tx.clone()).await
}
