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
pub mod compose;
pub mod contacts;
pub mod drafts;
pub mod folders;
pub mod history_sync;
pub mod messages;
pub mod reader;
pub mod settings;

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
    use std::sync::atomic::Ordering;
    state.ui_ready.notify_one();
    if state
        .first_paint_logged
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
        .is_ok()
    {
        let elapsed_ms = state.boot_at.elapsed().as_millis() as u64;
        tracing::info!(
            elapsed_ms,
            "ui_ready: first paint (process start → wasm mounted)"
        );
    } else {
        tracing::info!("ui_ready: signalled sync engine");
    }
    Ok(())
}

/// `ui_log` — bridge for `qsl-ui`'s wasm `tracing::*!` events to the
/// host's stderr stream. The UI side runs `host_log::install()` to
/// register a custom subscriber Layer that posts each event here so
/// operators can read host + UI narration in one place rather than
/// the webview's DevTools console. Capped at INFO on the wasm side
/// before we ever cross the IPC boundary.
#[derive(serde::Deserialize)]
pub struct UiLogInput {
    pub level: String,
    pub target: String,
    pub message: String,
}

#[tauri::command]
pub async fn ui_log(input: UiLogInput) -> IpcResult<()> {
    // Single static target on the host side so operators can filter
    // with `RUST_LOG=qsl_ui::bridge=info`. The original UI module
    // path is preserved as a `ui_target` field.
    match input.level.as_str() {
        "error" => tracing::error!(
            target: "qsl_ui::bridge",
            ui_target = %input.target,
            "{}",
            input.message
        ),
        "warn" => tracing::warn!(
            target: "qsl_ui::bridge",
            ui_target = %input.target,
            "{}",
            input.message
        ),
        "info" => tracing::info!(
            target: "qsl_ui::bridge",
            ui_target = %input.target,
            "{}",
            input.message
        ),
        // The bridge layer caps wasm-side events at INFO so debug /
        // trace shouldn't reach here. Treat them as info if they do.
        _ => tracing::info!(
            target: "qsl_ui::bridge",
            ui_target = %input.target,
            level = %input.level,
            "{}",
            input.message
        ),
    }
    Ok(())
}
