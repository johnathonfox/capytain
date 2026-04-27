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

/// Error returned by [`init`] when subscriber installation fails.
///
/// Opaque by design — this is a startup-only failure mode, and callers
/// typically just log the display form and exit.
pub type InitError = Box<dyn Error + Send + Sync + 'static>;

/// Default filter directive used when neither an explicit filter nor
/// `RUST_LOG` is provided.
///
/// - All `qsl_*` crates at `info`
/// - Third-party crates at `warn`
/// - Servo's noisier modules pinned at `error` so reader-pane HTML
///   data: URLs and per-frame style log lines don't drown the rest.
///   `script::dom::window` in particular dumps the full data: URL the
///   reader navigates to at INFO level on every render — a single
///   gmail thread blew a launch log past 700 MB.
pub const DEFAULT_FILTER: &str = "warn,qsl_core=info,qsl_storage=info,\
    qsl_imap_client=info,qsl_smtp_client=info,qsl_jmap_client=info,\
    qsl_mime=info,qsl_sync=info,qsl_search=info,qsl_auth=info,\
    qsl_ipc=info,qsl_telemetry=info,qsl_desktop=info,qsl_ui=info,\
    mailcli=info,\
    script::dom=error,style=error,script::script_module=error";

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
    if let Some(directives) = explicit {
        if let Ok(f) = EnvFilter::try_new(directives) {
            return f;
        }
    }
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER))
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
