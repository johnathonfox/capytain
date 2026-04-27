// SPDX-License-Identifier: Apache-2.0

//! Shared runtime state for the Tauri shell.
//!
//! `AppState` owns the long-lived handles every command needs: the
//! Turso-backed [`qsl_storage::TursoConn`] for persistence, a
//! per-account cache of live [`MailBackend`] implementations, and
//! — when the `servo` feature is on and the platform supports it —
//! the Servo-backed [`EmailRenderer`] attached to a secondary Tauri
//! window (the reader pane).
//!
//! The backend cache is lazily populated — a `MailBackend` is built and
//! logged in the first time a command actually reaches out to the
//! provider, not at boot. This keeps window-open fast even with many
//! accounts configured.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use qsl_core::{AccountId, EmailRenderer, MailBackend};
use qsl_storage::TursoConn;
use tokio::sync::{Mutex, Notify};

/// Long-lived state attached to the Tauri app via `manage`.
///
/// Commands reach it through `tauri::State<AppState>`.
pub struct AppState {
    /// Writable handle to the on-disk Turso database, used by the
    /// IPC command path (every `commands/*.rs` handler). All
    /// foreground repository calls go through this.
    pub db: Arc<Mutex<TursoConn>>,

    /// Second connection to the same database file, dedicated to
    /// the sync engine's background work (initial CONNECT/LIST
    /// bootstrap, IMAP IDLE / JMAP push refreshes,
    /// `messages_refresh_folder`). Without this split, the sync
    /// engine's writes would block IPC reads on the single
    /// `Arc<Mutex<TursoConn>>` lock — concretely, `messages_get`
    /// would stall behind a long sync transaction and the reader
    /// pane would freeze on "Loading…" until sync finished.
    /// Multi-connection concurrency requires WAL mode, which
    /// `TursoConn::open` enables.
    pub sync_db: Arc<Mutex<TursoConn>>,

    /// Per-account backend cache. Populated lazily on first use and
    /// evicted when an account is removed. `Arc<dyn MailBackend>` is
    /// cheap to clone so command handlers can drop the lock fast.
    #[allow(dead_code)] // populated by commands that land in week 5 part 2
    pub backends: Mutex<HashMap<AccountId, Arc<dyn MailBackend>>>,

    /// Root of the on-disk data directory (blobs, logs, future
    /// attachment spill). Resolved once at startup via `directories`
    /// and kept here so commands don't recompute it.
    #[allow(dead_code)] // consumed by later commands in week 5 part 2
    pub data_dir: PathBuf,

    /// Servo-backed email renderer, if the platform supports it. `None`
    /// when the `servo` feature is off (fallback path for environments
    /// without the Servo native toolchain) or when `new_linux` /
    /// `new_macos` failed at startup (e.g. unsupported window handle
    /// variant). Consumers MUST handle the `None` case — degrade the
    /// reader pane rather than crashing.
    ///
    /// Wrapped in `tokio::sync::Mutex` even though the renderer is
    /// `Send + Sync`: trait methods take `&mut self`, so exclusive
    /// access is required.
    pub servo_renderer: Mutex<Option<Box<dyn EmailRenderer>>>,

    /// Fired by the `ui_ready` IPC command once the Dioxus app has
    /// mounted in the webview. The sync engine awaits this (with a
    /// short safety timeout) before its bootstrap pass so the
    /// initial paint isn't competing with IMAP CONNECT + LIST +
    /// SELECT churn for tokio worker threads. `Notify` buffers a
    /// single permit, so the call ordering between the UI signal
    /// and the engine's wait is irrelevant.
    pub ui_ready: Arc<Notify>,
}

impl AppState {
    /// Build an `AppState` given an already-opened database and the
    /// resolved data directory. The renderer slot starts empty;
    /// `main::setup` installs the real renderer once the Tauri window
    /// exists and its raw handle can be queried.
    pub fn new(db: TursoConn, sync_db: TursoConn, data_dir: PathBuf) -> Self {
        Self {
            db: Arc::new(Mutex::new(db)),
            sync_db: Arc::new(Mutex::new(sync_db)),
            backends: Mutex::new(HashMap::new()),
            data_dir,
            servo_renderer: Mutex::new(None),
            ui_ready: Arc::new(Notify::new()),
        }
    }
}
