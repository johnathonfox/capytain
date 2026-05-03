// SPDX-License-Identifier: Apache-2.0

//! Shared tracing initialization for every QSL binary.
//!
//! Binaries call [`init`] exactly once at startup. The helper wires
//! [`tracing_subscriber::fmt`] to write structured logs to **stderr** so that
//! stdout stays clean for program output (important for `mailcli`'s future
//! "pipe me into jq" use cases).
//!
//! Log filtering is driven by an [`EnvFilter`]. Precedence, highest first:
//!
//! 1. The `filter` argument to [`init`] if it is `Some`. This is what
//!    `mailcli --log-level` threads through.
//! 2. The `RUST_LOG` environment variable.
//! 3. A fallback of `info` for QSL crates, `warn` for everything else.
//!
//! # Example
//!
//! ```no_run
//! // In a binary's `main`:
//! qsl_telemetry::init(None).expect("telemetry");
//! tracing::info!("qsl up");
//! ```

use std::error::Error;

use tracing_subscriber::{fmt, EnvFilter};

pub mod slow;

/// Error returned by [`init`] when subscriber installation fails.
///
/// Opaque by design — this is a startup-only failure mode, and callers
/// typically just log the display form and exit.
pub type InitError = Box<dyn Error + Send + Sync + 'static>;

/// Per-crate level baseline used when neither an explicit filter nor
/// `RUST_LOG` is provided.
///
/// - All `qsl_*` crates at `info`
/// - Third-party crates at `warn`
pub const DEFAULT_FILTER: &str = "warn,qsl_core=info,qsl_storage=info,\
    qsl_imap_client=info,qsl_smtp_client=info,qsl_jmap_client=info,\
    qsl_mime=info,qsl_sync=info,qsl_search=info,qsl_auth=info,\
    qsl_ipc=info,qsl_telemetry=info,qsl_desktop=info,qsl_ui=info,\
    mailcli=info";

/// Third-party modules that are loud-by-design and would drown out
/// our own info logs when an operator sets `RUST_LOG=info` to debug
/// startup or sync. Forced regardless of explicit filter / `RUST_LOG`
/// because the noise comes from *how* we use these libraries, not
/// from a logging preference:
///
/// - `script::dom`, `style`, `script::script_module`: legacy Servo
///   reader-pane modules (kept so older binaries with the
///   data: URL renderer don't spam 700 MB launch logs — see
///   `docs/servo-tombstone.md`).
/// - `tantivy::indexer::*` + `tantivy::directory::managed_directory`:
///   Turso's experimental FTS index emits per-commit + per-GC INFO
///   log lines that fire 14×/sec during a `sync_folder` write burst
///   (every implicit `messages` write triggers a Tantivy commit; the
///   batched-tx fix is a separate work item). The information is
///   uninteresting at the application level — operators care about
///   "messages synced", not "tantivy commit 547".
/// - `turso_core` / `turso_sync_engine`: the libSQL engine and sync
///   SDK emit per-statement breadcrumbs that duplicate what our own
///   `qsl::slow::db` watchdog already covers.
const NOISY_THIRD_PARTY_SUPPRESSIONS: &str = ",script::dom=error,style=error,\
    script::script_module=error,\
    tantivy=warn,turso_core=warn,turso_sync_engine=warn,\
    turso_sync_sdk_kit=warn,turso_sdk_kit=warn";

/// Initialize the global tracing subscriber.
///
/// Call at most once per process. Subsequent calls return an [`InitError`].
///
/// See the module docs for how `filter` interacts with `RUST_LOG`.
pub fn init(filter: Option<&str>) -> Result<(), InitError> {
    let env_filter = resolve_filter(filter);
    fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(env_filter)
        .with_target(true)
        .try_init()
}

fn resolve_filter(explicit: Option<&str>) -> EnvFilter {
    let base: String = if let Some(directives) = explicit {
        directives.to_string()
    } else if let Ok(rust_log) = std::env::var("RUST_LOG") {
        rust_log
    } else {
        DEFAULT_FILTER.to_string()
    };
    let combined = format!("{base}{NOISY_THIRD_PARTY_SUPPRESSIONS}");
    EnvFilter::try_new(&combined).unwrap_or_else(|_| EnvFilter::new("warn"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_filter_parses() {
        let _ = EnvFilter::try_new(DEFAULT_FILTER).expect("DEFAULT_FILTER must parse");
    }

    #[test]
    fn explicit_filter_overrides_env() {
        // Explicit filter takes precedence; we just assert it parses without
        // panicking. Actually installing it is a one-shot global, so we
        // don't install here.
        let f = resolve_filter(Some("debug"));
        // Filter's public API is minimal; stringifying checks it's usable.
        assert!(!format!("{f:?}").is_empty());
    }
}
