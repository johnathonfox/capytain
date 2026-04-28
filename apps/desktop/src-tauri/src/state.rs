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
use qsl_ipc::{MessageId, RenderedMessage};
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

    /// Servo-backed email renderers, keyed by Tauri window label
    /// (`"main"`, `"reader-<msg_id>"`, …). Empty when the `servo`
    /// feature is off; otherwise `"main"` is populated at setup time
    /// and popup-window labels are populated lazily on first
    /// `reader_render` for that label. Consumers MUST handle a
    /// missing key — degrade the reader pane rather than crashing.
    ///
    /// Wrapped in `tokio::sync::Mutex` because trait methods on the
    /// renderer take `&mut self`, so exclusive access is required.
    pub servo_renderers: Mutex<HashMap<String, Box<dyn EmailRenderer>>>,

    /// Last `(width, height)` we passed into Servo's `renderer.resize`,
    /// keyed by Tauri window label. `reader_set_position` consults
    /// this before calling `resize` again — when the dimensions are
    /// unchanged (the user is dragging the splitter or scrolling and
    /// only the rect's `(x, y)` shifted) Servo doesn't need to
    /// re-layout, and skipping the resize avoids the visible reflow
    /// flicker the reader pane exhibited at every mouse-move during
    /// window resize.
    pub last_reader_size: Mutex<HashMap<String, (u32, u32)>>,

    /// Trailing-edge debounce handles for `renderer.resize`, keyed by
    /// Tauri window label. `reader_set_position` schedules each new
    /// size into a deferred tokio task and aborts the previous handle
    /// for the same window — so a fast continuous drag fires a single
    /// `resize` on the trailing edge instead of cascading Servo
    /// relayouts at ~60 Hz. `set_position` itself stays unbatched
    /// (it's cheap) so the GTK overlay tracks the cursor live.
    ///
    /// Experimental — the residual reader-pane flicker is partly
    /// structural (Servo's offscreen surface size lags its viewport
    /// handle, see `KNOWN_ISSUES.md`). This debounce caps the rate
    /// at which we *ask* Servo to relayout but doesn't fix the
    /// surface-vs-viewport mismatch frame; a follow-up pause-paint
    /// approach would be needed for that.
    #[allow(dead_code)] // unused on the webkit-iframe branch (Servo-only)
    pub resize_debounce: Mutex<HashMap<String, tauri::async_runtime::JoinHandle<()>>>,

    /// Single-entry cache of the most recently rendered message —
    /// the `RenderedMessage` produced by the last `messages_get` call.
    /// `messages_open_in_window` (popup-reader path) consults this
    /// before re-issuing `messages_get`: when the user double-clicks
    /// a row that's currently selected in the main reader pane, the
    /// body has just been fetched, sanitized, and decoded, so paying
    /// the lazy-fetch + sanitize cost a second time wastes ~50–500 ms.
    /// We cache only the last one because the typical "select then
    /// pop out" pattern hits exactly that — a multi-entry LRU would
    /// pay more in eviction bookkeeping than the second-most-recent
    /// hit rate is worth.
    ///
    /// Cache validity: the cached `RenderedMessage` may carry stale
    /// flags if the user mutates them between the fetch and the
    /// pop-out, but the popup body-render only consumes
    /// `sanitized_html` / `body_text` (immutable after first parse)
    /// so flag drift is not a correctness issue here. UI surfaces
    /// that need fresh flags re-call `messages_get` regardless.
    pub last_rendered: Mutex<Option<(MessageId, RenderedMessage)>>,

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
            servo_renderers: Mutex::new(HashMap::new()),
            last_reader_size: Mutex::new(HashMap::new()),
            last_rendered: Mutex::new(None),
            resize_debounce: Mutex::new(HashMap::new()),
            ui_ready: Arc::new(Notify::new()),
        }
    }
}
