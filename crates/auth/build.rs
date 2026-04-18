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
//! [cargo-env]: https://doc.rust-lang.org/cargo/reference/build-scripts.html#rustc-env

fn main() {
    // Invalidate the cache whenever the env var changes so changing client
    // IDs doesn't require a `cargo clean`.
    println!("cargo:rerun-if-env-changed=CAPYTAIN_GMAIL_CLIENT_ID");
    println!("cargo:rerun-if-env-changed=CAPYTAIN_FASTMAIL_CLIENT_ID");

    // Always set the rustc env vars — empty string if unset. Provider
    // profiles guard against empty values at flow-start time.
    let gmail = std::env::var("CAPYTAIN_GMAIL_CLIENT_ID").unwrap_or_default();
    let fastmail = std::env::var("CAPYTAIN_FASTMAIL_CLIENT_ID").unwrap_or_default();
    println!("cargo:rustc-env=CAPYTAIN_GMAIL_CLIENT_ID={gmail}");
    println!("cargo:rustc-env=CAPYTAIN_FASTMAIL_CLIENT_ID={fastmail}");
}
