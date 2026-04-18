// SPDX-License-Identifier: Apache-2.0

//! `mailcli` — Capytain's headless protocol CLI.
//!
//! Phase 0 Week 1 ships the stub: parse `--log-level`, initialize shared
//! tracing via [`capytain_telemetry::init`], emit a single `info!` line, exit
//! cleanly. Subcommands (`auth add`, `list-folders`, `sync`, …) arrive in
//! Weeks 3–4 once the underlying flows are in place.

use clap::Parser;

/// Capytain headless protocol CLI.
#[derive(Debug, Parser)]
#[command(name = "mailcli", version, about, long_about = None)]
struct Cli {
    /// Tracing filter directive, e.g. `info`, `debug`, or
    /// `capytain_imap_client=trace,warn`. Takes precedence over `RUST_LOG`.
    #[arg(long, value_name = "FILTER", global = true)]
    log_level: Option<String>,
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = capytain_telemetry::init(cli.log_level.as_deref()) {
        eprintln!("mailcli: failed to initialize tracing: {e}");
        std::process::exit(1);
    }

    tracing::info!(
        target: "mailcli",
        version = env!("CARGO_PKG_VERSION"),
        "mailcli started (Phase 0 stub — subcommands land in Weeks 3\u{2013}4)"
    );
}
