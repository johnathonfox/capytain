// SPDX-License-Identifier: Apache-2.0

//! Build-time glue that captures OAuth2 client IDs from environment
//! variables and bakes them into `capytain-auth` via
//! [`cargo:rustc-env`][cargo-env]. The provider modules then read them
//! with `env!`.
//!
//! Phase 0 tolerates empty values — the per-provider profile reports an
//! unconfigured client as an error at runtime, which is what we want while
//! the initial dev registration is still in flight. Forks and release
//! builds set these before `cargo build`:
//!
//! ```sh
//! CAPYTAIN_GMAIL_CLIENT_ID=… CAPYTAIN_FASTMAIL_CLIENT_ID=… cargo build
//! ```
//!
//! A workspace-root `.env` file is also loaded (only populating
//! variables not already present in the shell env), so maintainer
//! client-id/secret pairs can persist across shells without needing
//! every invocation to re-export them. `.env` is gitignored.
//!
//! [cargo-env]: https://doc.rust-lang.org/cargo/reference/build-scripts.html#rustc-env

fn main() {
    // Load `.env` from the workspace root. `dotenvy::from_path` only
    // sets variables that are not already present in the real
    // environment, so explicit shell exports and CI job env values
    // always override .env entries. A missing / unreadable .env is
    // fine — the downstream env reads just fall through to empty.
    //
    // `CARGO_MANIFEST_DIR` is `<workspace>/crates/auth`; pop twice to
    // reach the workspace root.
    if let Some(dir) = std::env::var_os("CARGO_MANIFEST_DIR") {
        let mut workspace_root = std::path::PathBuf::from(dir);
        workspace_root.pop(); // crates/auth → crates
        workspace_root.pop(); // crates → workspace root
        let env_file = workspace_root.join(".env");
        if env_file.exists() {
            println!("cargo:rerun-if-changed={}", env_file.display());
            let _ = dotenvy::from_path(&env_file);
        }
    }

    // Invalidate the cache whenever the env var changes so changing client
    // IDs / secrets doesn't require a `cargo clean`.
    for var in [
        "CAPYTAIN_GMAIL_CLIENT_ID",
        "CAPYTAIN_GMAIL_CLIENT_SECRET",
        "CAPYTAIN_FASTMAIL_CLIENT_ID",
        "CAPYTAIN_FASTMAIL_CLIENT_SECRET",
    ] {
        println!("cargo:rerun-if-env-changed={var}");
        // Always set the rustc env vars — empty string if unset. Provider
        // profiles guard against empty `client_id` at flow-start time; an
        // empty `client_secret` means "PKCE-only, no confidential client."
        let value = std::env::var(var).unwrap_or_default();
        println!("cargo:rustc-env={var}={value}");
    }
}
