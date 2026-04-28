// SPDX-License-Identifier: Apache-2.0

//! Tauri build-time code generator + Dioxus UI build orchestration.
//!
//! `tauri_build::build()` reads `tauri.conf.json`, validates it, generates
//! the `gen/` glue that `tauri::generate_context!()` consumes at compile
//! time, and wires up platform-specific metadata (Windows resources,
//! macOS Info.plist snippets, Linux desktop entry).
//!
//! Before that runs, we invoke `dx build --platform web` on
//! `apps/desktop/ui/` so `frontendDist: "../ui/dist"` in
//! `tauri.conf.json` resolves to real assets. Without this,
//! `cargo run -p qsl-desktop` shows webkit2gtk's "Connection
//! refused" error page (because `devUrl: http://localhost:1420`
//! falls back to frontendDist when no dev server is running, and
//! frontendDist is empty).
//!
//! The Dioxus build is skipped — with a loud warning — if `dx` isn't
//! on PATH or fails, so `cargo check` / `cargo clippy` / `cargo test`
//! on CI stay unaffected. `dx` is installed with
//! `cargo install dioxus-cli --locked` (see README).

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    build_dioxus_ui();
    tauri_build::build();
}

fn build_dioxus_ui() {
    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set"));
    let workspace_root = manifest_dir
        .ancestors()
        .nth(3)
        .expect("apps/desktop/src-tauri has a workspace root 3 levels up")
        .to_path_buf();
    let ui_dir = manifest_dir
        .parent()
        .expect("src-tauri has a parent")
        .join("ui");

    // Rebuild the UI when any source / asset / config changes.
    //
    // `cargo:rerun-if-changed=DIR` is unreliable in practice: cargo
    // stats the directory itself, and most filesystems do *not* bump
    // a directory's mtime when files inside it are modified, only
    // when entries are added or removed. So a plain edit to
    // `ui/src/app.rs` would slip past the watcher and the wasm bundle
    // in `ui/dist/` would stay stale across branch switches. The
    // workaround is to enumerate every relevant file and emit one
    // directive per file — cargo follows file mtimes correctly. (See
    // https://github.com/rust-lang/cargo/issues/2926 for the long-
    // running tracking issue.)
    for dir in [ui_dir.join("src"), ui_dir.join("assets")] {
        emit_rerun_for_files_in(&dir);
    }
    for file in [
        ui_dir.join("index.html"),
        ui_dir.join("Dioxus.toml"),
        ui_dir.join("Cargo.toml"),
    ] {
        println!("cargo:rerun-if-changed={}", file.display());
    }

    // `QSL_SKIP_UI_BUILD=1` lets CI skip the Dioxus step entirely
    // (CI only runs check / clippy / test, doesn't launch the binary).
    if std::env::var_os("QSL_SKIP_UI_BUILD").is_some() {
        println!("cargo:warning=qsl-desktop: UI build skipped (QSL_SKIP_UI_BUILD set)");
        ensure_dist_exists(&ui_dir);
        return;
    }

    match run_dx_build(&ui_dir) {
        Ok(()) => {
            // Copy the dx output into `apps/desktop/ui/dist/` so
            // Tauri's `frontendDist: "../ui/dist"` resolves. dx
            // writes to `target/dx/qsl-ui/<profile>/web/public`
            // which isn't a stable config-known path.
            let dx_out = workspace_root
                .join("target")
                .join("dx")
                .join("qsl-ui")
                .join("debug") // `cargo run` is debug; release handling lands with the prod-bundle PR
                .join("web")
                .join("public");
            let dist = ui_dir.join("dist");
            if let Err(e) = sync_dir(&dx_out, &dist) {
                println!("cargo:warning=qsl-desktop: could not sync dx output to ui/dist: {e}");
                ensure_dist_exists(&ui_dir);
            } else {
                println!(
                    "cargo:warning=qsl-desktop: dioxus UI built -> {}",
                    dist.display()
                );
            }
        }
        Err(e) => {
            println!(
                "cargo:warning=qsl-desktop: dioxus UI build skipped ({e}). \
                 Install with `cargo install dioxus-cli --locked` to enable."
            );
            ensure_dist_exists(&ui_dir);
        }
    }
}

fn run_dx_build(ui_dir: &Path) -> Result<(), String> {
    let status = Command::new("dx")
        .args(["build", "--platform", "web"])
        .current_dir(ui_dir)
        .status()
        .map_err(|e| format!("failed to invoke `dx`: {e}"))?;

    if !status.success() {
        return Err(format!("`dx build --platform web` exited with {status}"));
    }
    Ok(())
}

/// Mirror `src` into `dst`: remove `dst` first, then recursively copy.
/// Small enough that pulling in `fs_extra` or walkdir would be overkill.
fn sync_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    if !src.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("dx output directory missing: {}", src.display()),
        ));
    }
    if dst.exists() {
        std::fs::remove_dir_all(dst)?;
    }
    std::fs::create_dir_all(dst)?;
    copy_recursive(src, dst)
}

fn copy_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let target = dst.join(entry.file_name());
        let metadata = entry.file_type()?;
        if metadata.is_dir() {
            std::fs::create_dir_all(&target)?;
            copy_recursive(&path, &target)?;
        } else if metadata.is_file() {
            std::fs::copy(&path, &target)?;
        } else if metadata.is_symlink() {
            // Resolve symlinks to a regular file copy so Tauri's
            // bundler doesn't have to think about them.
            let resolved = std::fs::read_link(&path)?;
            let src_real = if resolved.is_absolute() {
                resolved
            } else {
                src.join(resolved)
            };
            if src_real.is_file() {
                std::fs::copy(&src_real, &target)?;
            }
        }
    }
    Ok(())
}

/// Walk `dir` recursively and emit one `cargo:rerun-if-changed`
/// directive per regular file. Workaround for cargo's directory-watch
/// behaviour (only directory mtime is tracked; intra-directory edits
/// slip past). Best-effort: I/O errors during the walk just stop the
/// recursion at that branch — the output is "fewer rerun
/// directives," not a fatal error, since the cargo build still
/// proceeds either way.
fn emit_rerun_for_files_in(dir: &Path) {
    // Always tell cargo to watch the directory itself so that
    // additions / removals still trigger a rebuild even if no
    // existing file's mtime changed.
    println!("cargo:rerun-if-changed={}", dir.display());

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => emit_rerun_for_files_in(&path),
            Ok(ft) if ft.is_file() => {
                println!("cargo:rerun-if-changed={}", path.display());
            }
            _ => {}
        }
    }
}

/// Ensure `apps/desktop/ui/dist/` exists (empty is fine) so
/// `tauri_build::build()` doesn't trip on a missing frontendDist dir
/// when the UI build is skipped or failed.
fn ensure_dist_exists(ui_dir: &Path) {
    let dist = ui_dir.join("dist");
    if !dist.exists() {
        if let Err(e) = std::fs::create_dir_all(&dist) {
            println!("cargo:warning=qsl-desktop: could not create empty {dist:?}: {e}");
        }
    }
}
