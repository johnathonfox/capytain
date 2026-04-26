// SPDX-License-Identifier: Apache-2.0

//! JMAP EventSource (RFC 8620 §7.3) watcher.
//!
//! [`watch_account`] subscribes to the JMAP server's EventSource
//! endpoint and emits a [`BackendEvent::AccountChanged`] for every
//! push notification that involves Email-shaped data types. The
//! sync engine reacts by running a debounced `sync_account` for the
//! affected account.
//!
//! Granularity tradeoff: JMAP push tells us "type X has new state"
//! without naming a mailbox, so we deliberately collapse all
//! Email-class events into one `AccountChanged`. Per-folder deltas
//! emerge during the follow-up `Email/changes` call inside
//! `list_messages`. Translating push into per-folder events would
//! require N extra round-trips (one `Email/get` per changed id) for
//! no observable benefit — the engine debounces 500ms before
//! syncing anyway.
//!
//! The `ping` interval below (60 seconds) is what the JMAP example
//! in `jmap-client`'s readme uses; the server sends a comment line
//! every `ping` seconds so we can detect a dropped TCP connection
//! quickly (otherwise EventSource just stays silent on idle
//! accounts).

use futures_util::StreamExt;
use jmap_client::client::Client;
use jmap_client::event_source::PushNotification;
use jmap_client::DataType;
use qsl_core::{AccountId, BackendEvent, MailError};
use tokio::sync::mpsc;
use tracing::{debug, info};

/// How often the server sends a comment-line keepalive on the
/// EventSource stream. Faster than IMAP's 29-min IDLE refresh
/// because EventSource has no protocol-level "still here" pings.
const PING_SECONDS: u32 = 60;

/// Run the EventSource loop on `client` and forward each
/// notification to `tx` as [`BackendEvent::AccountChanged`].
///
/// Returns `Ok(())` when the receiver drops (caller is shutting
/// down) or when the server closes the stream cleanly. Returns
/// `Err` on any HTTP, parse, or transport failure — the caller is
/// expected to recover by re-authenticating and calling this again.
pub async fn watch_account(
    client: &Client,
    account: AccountId,
    tx: mpsc::Sender<BackendEvent>,
) -> Result<(), MailError> {
    info!(
        account = %account.0,
        "JMAP EventSource watcher started"
    );

    let mut stream = client
        .event_source(
            Some([DataType::Email, DataType::EmailDelivery, DataType::Mailbox]),
            /* close_after_state = */ false,
            Some(PING_SECONDS),
            /* last_event_id = */ None,
        )
        .await
        .map_err(|e| MailError::Network(format!("JMAP event_source open: {e}")))?;

    while let Some(item) = stream.next().await {
        let notification = item.map_err(|e| MailError::Protocol(format!("EventSource: {e}")))?;
        match &notification {
            PushNotification::StateChange(changes) => {
                debug!(
                    account = %account.0,
                    id = ?changes.id(),
                    "JMAP push state change"
                );
                if tx.send(BackendEvent::AccountChanged).await.is_err() {
                    debug!(account = %account.0, "JMAP push receiver dropped — exiting");
                    return Ok(());
                }
            }
            PushNotification::CalendarAlert(_) => {
                // Calendar push isn't relevant to mail sync — ignore.
            }
        }
    }

    debug!(account = %account.0, "JMAP EventSource stream ended cleanly");
    Ok(())
}
