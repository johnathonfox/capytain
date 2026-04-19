// SPDX-License-Identifier: Apache-2.0

//! Shared runtime state for the Tauri shell.
//!
//! `AppState` owns the long-lived handles every command needs: the
//! Turso-backed [`capytain_storage::TursoConn`] for persistence and a
//! per-account cache of live [`MailBackend`] implementations.
//!
//! The backend cache is lazily populated — a `MailBackend` is built and
//! logged in the first time a command actually reaches out to the
//! provider, not at boot. This keeps window-open fast even with many
//! accounts configured.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use capytain_core::{AccountId, MailBackend};
use capytain_storage::TursoConn;
use tokio::sync::Mutex;

/// Long-lived state attached to the Tauri app via `manage`.
///
/// Commands reach it through `tauri::State<AppState>`.
pub struct AppState {
    /// Writable handle to the on-disk Turso database. All repository
    /// calls go through this.
    pub db: Arc<Mutex<TursoConn>>,

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
}

impl AppState {
    /// Build an `AppState` given an already-opened database and the
    /// resolved data directory.
    pub fn new(db: TursoConn, data_dir: PathBuf) -> Self {
        Self {
            db: Arc::new(Mutex::new(db)),
            backends: Mutex::new(HashMap::new()),
            data_dir,
        }
    }
}
