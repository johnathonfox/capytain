// SPDX-License-Identifier: Apache-2.0

//! Phase 0 Week 6 Day 5 — corpus rendering regression harness.
//!
//! For each `.html` fixture in `tests/corpus/fixtures/`, render the
//! document through Servo's `SoftwareRenderingContext`, hash the
//! result, and compare against a committed reference in
//! `tests/corpus/reference/`.
//!
//! # What this test guarantees (every run)
//!
//! - Every fixture renders through to a `take_screenshot` callback —
//!   no panic, no timeout.
//! - The output image has non-zero dimensions and at least one non-
//!   background pixel.
//!
//! # What this test documents (on diff, doesn't fail)
//!
//! Reference hashes + thumbnail PNGs live under `tests/corpus/reference/`
//! and are treated as "this is what Servo rendered on the maintainer's
//! box at commit X." Hash mismatches print a loud diagnostic but don't
//! fail the run — exact-pixel equality across machines is flaky (font
//! hinting, subpixel AA, GL driver differences) and we don't want PRs
//! blocked on cosmetic drift. Use them instead as a review signal:
//! when the expected hash differs, open the PNG and eyeball.
//!
//! # Regenerating references
//!
//! Set `CAPYTAIN_CORPUS_REGEN=1` and run the test on a stable
//! environment. The harness will (re)write `.sha256` + `.png` pairs
//! under `tests/corpus/reference/`. Commit the result along with a
//! note about why they moved.
//!
//! ```bash
//! CAPYTAIN_CORPUS_REGEN=1 cargo test -p capytain-renderer \
//!     --features servo --test corpus -- --nocapture
//! ```
//!
//! # CI gating (opt-in)
//!
//! Marked `#[ignore]` so default `cargo test` runs skip it. Servo's
//! `SoftwareRenderingContext` is backed by `surfman`, which still needs
//! a working EGL driver on the host even on the software path:
//!
//! - `windows-latest` runners ship no EGL and panic with
//!   `"egl function was not loaded"` when the test constructs a
//!   context.
//! - `ubuntu-latest` runners have Mesa EGL installed but the
//!   `take_screenshot` callback never fires in the headless runner —
//!   the test sits waiting until the 6h job timeout.
//!
//! Every main-branch merge since PR #19 (when this harness landed)
//! has hit that Ubuntu hang, hence the opt-in gate. To run locally:
//!
//! ```bash
//! cargo test -p capytain-renderer --features servo --test corpus \
//!     -- --ignored --nocapture
//! ```
//!
//! Runtime validation on Windows and on headless CI is hardware- or
//! environment-gated separately; this harness is a maintainer-run
//! regression tool, not a required CI gate.

#![cfg(feature = "servo")]

use std::fs;
use std::path::{Path, PathBuf};

use capytain_renderer::CorpusRenderer;
use dpi::PhysicalSize;
use image::RgbaImage;

/// Fixed render size for every corpus fixture. 800x600 hits the
/// common breakpoints in email template CSS (many templates cap at
/// 600-640 inner width; 800 leaves enough margin to catch overflow).
const RENDER_WIDTH: u32 = 800;
const RENDER_HEIGHT: u32 = 600;

/// Run as one `#[test]` because Servo is one-per-process: we construct
/// one `CorpusRenderer`, reuse it across all fixtures, and keep the
/// harness trivially single-threaded.
#[test]
#[ignore = "maintainer-run regression harness; hangs on headless CI and panics on Windows — see module docs"]
fn corpus_renders_every_fixture_without_panic() {
    let fixtures_dir = workspace_path(&["crates", "renderer", "tests", "corpus", "fixtures"]);
    let reference_dir = workspace_path(&["crates", "renderer", "tests", "corpus", "reference"]);

    assert!(
        fixtures_dir.is_dir(),
        "corpus fixtures dir missing: {}",
        fixtures_dir.display()
    );
    fs::create_dir_all(&reference_dir).unwrap();

    let regen = std::env::var("CAPYTAIN_CORPUS_REGEN").as_deref() == Ok("1");

    let mut fixtures = fs::read_dir(&fixtures_dir)
        .unwrap()
        .filter_map(|e| {
            let p = e.unwrap().path();
            (p.extension().and_then(|s| s.to_str()) == Some("html")).then_some(p)
        })
        .collect::<Vec<_>>();
    fixtures.sort(); // stable ordering for report output
    assert!(!fixtures.is_empty(), "no corpus fixtures found");

    eprintln!(
        "corpus: {} fixtures, {}x{}, regen={}",
        fixtures.len(),
        RENDER_WIDTH,
        RENDER_HEIGHT,
        regen,
    );

    let renderer = CorpusRenderer::new(PhysicalSize::new(RENDER_WIDTH, RENDER_HEIGHT))
        .expect("corpus renderer construction");

    let mut mismatches = Vec::new();
    let mut hard_failures = Vec::new();

    for fixture in &fixtures {
        let name = fixture.file_stem().unwrap().to_string_lossy().into_owned();
        let html = fs::read_to_string(fixture).unwrap();

        match renderer.render(&html) {
            Err(e) => {
                hard_failures.push(format!("{name}: render error: {e}"));
            }
            Ok(img) => {
                // Baseline invariants: non-empty + has non-background pixels.
                if img.width() == 0 || img.height() == 0 {
                    hard_failures.push(format!("{name}: zero-sized output"));
                    continue;
                }
                if is_entirely_background(&img) {
                    hard_failures.push(format!(
                        "{name}: output is a uniform background — likely a layout failure"
                    ));
                    continue;
                }

                // Compare against reference (or write a new one).
                let hash = sha256_hex(img.as_raw());
                let hash_path = reference_dir.join(format!("{name}.sha256"));
                let png_path = reference_dir.join(format!("{name}.png"));

                if regen {
                    fs::write(&hash_path, &hash).unwrap();
                    img.save(&png_path)
                        .unwrap_or_else(|e| panic!("{name}: write png: {e}"));
                    eprintln!("corpus: regenerated {name} ({} bytes)", img.as_raw().len());
                } else if hash_path.exists() {
                    let expected = fs::read_to_string(&hash_path).unwrap().trim().to_owned();
                    if expected != hash {
                        mismatches.push(format!(
                            "{name}: hash drift (expected {}, got {})",
                            &expected[..16.min(expected.len())],
                            &hash[..16]
                        ));
                        // Dump the actual render next to target/ for eyeballing.
                        let actual_path =
                            std::env::temp_dir().join(format!("capytain-corpus-{name}-actual.png"));
                        let _ = img.save(&actual_path);
                        eprintln!("  actual written to {}", actual_path.display());
                    }
                } else {
                    // No reference yet — write one (first-run behavior).
                    // Mention it loudly so the CI maintainer sees that
                    // a new fixture landed without a committed baseline.
                    fs::write(&hash_path, &hash).unwrap();
                    img.save(&png_path)
                        .unwrap_or_else(|e| panic!("{name}: write png: {e}"));
                    eprintln!(
                        "corpus: created first reference for {name} — commit the new .sha256 + .png",
                    );
                }
            }
        }
    }

    // Reporting: hard failures FAIL the test; drift is informational.
    if !mismatches.is_empty() {
        eprintln!(
            "\ncorpus: {} hash drift(s) — review only, not a test failure:",
            mismatches.len()
        );
        for m in &mismatches {
            eprintln!("  - {m}");
        }
    }
    if !hard_failures.is_empty() {
        for f in &hard_failures {
            eprintln!("HARD FAIL: {f}");
        }
        panic!(
            "{} corpus fixture(s) failed to render as expected",
            hard_failures.len()
        );
    }
}

/// Walk up from `CARGO_MANIFEST_DIR` to the workspace root, then rejoin
/// the supplied path components. Makes the test robust against being
/// invoked from either the crate dir or the workspace root.
fn workspace_path(relative: &[&str]) -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    // `CARGO_MANIFEST_DIR` is `<workspace>/crates/renderer`.
    // Two levels up is the workspace root.
    let workspace = manifest.ancestors().nth(2).unwrap();
    relative
        .iter()
        .fold(workspace.to_path_buf(), |p, seg| p.join(seg))
}

/// Hex-encoded SHA-256 of the raw RGBA pixel buffer.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    let hash = sha2::Sha256::digest(bytes);
    hex_encode(&hash)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

/// Returns true if every pixel in the image is identical. A uniform
/// image almost certainly means the render failed silently — Servo
/// produced the clear-color background and nothing on top.
fn is_entirely_background(img: &RgbaImage) -> bool {
    let raw = img.as_raw();
    if raw.len() < 4 {
        return true;
    }
    let first = &raw[..4];
    raw.chunks_exact(4).all(|px| px == first)
}
