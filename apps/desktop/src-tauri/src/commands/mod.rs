// SPDX-License-Identifier: Apache-2.0

//! Tauri command handlers.
//!
//! One module per domain area of `COMMANDS.md`. Each command is a thin
//! adapter: parse input, delegate to `qsl-core` / `qsl-storage`
//! / `qsl-*-client`, map errors through `IpcError`, return
//! `IpcResult<T>`.
//!
//! # Phase 0 Week 5 part 1 scope
//!
//! Only `accounts_list` lands here. The rest of the command catalogue —
//! `accounts_add_oauth`, `folders_list`, `messages_*`, etc. — arrives in
//! Week 5 part 2 once the sidebar and message list components need
//! them.

pub mod accounts;
pub mod drafts;
pub mod folders;
pub mod messages;
pub mod reader;

use qsl_ipc::IpcResult;
use tauri::State;

use crate::state::AppState;

/// `ui_ready` — the Dioxus app calls this once the root component
/// has mounted. The sync engine awaits this (with a 10s safety
/// timeout) before kicking off its IMAP/JMAP bootstrap pass, so the
/// initial UI paint isn't competing with sync traffic for runtime
/// resources. Returns `Ok(())` immediately; the work is the
/// `notify_one` side-effect.
#[tauri::command]
pub async fn ui_ready(state: State<'_, AppState>) -> IpcResult<()> {
    state.ui_ready.notify_one();
    tracing::info!("ui_ready: signalled sync engine");
    Ok(())
}
