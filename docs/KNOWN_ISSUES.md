<!--
SPDX-FileCopyrightText: 2026 QSL Contributors
SPDX-License-Identifier: Apache-2.0
-->

# Known issues

Open issues we've consciously accepted for Phase 0 and the path out of each. Entries get deleted when they're fixed, not silently left stale — if you fix one, remove the section in the same PR.

This is the short list. Phase-specific deferrals (Fastmail smoke, macOS / Windows runtime) live in `PHASE_0.md`'s "Deferred from Phase 0" section instead; they're too big to belong here.

---

## Ubuntu CI corpus integration test hangs in `take_screenshot`

**Test:** `crates/renderer/tests/corpus.rs::corpus_renders_every_fixture_without_panic`.
**State:** `#[ignore]`d on all platforms (file-level `cfg(not(target_os = "windows"))` also keeps the test out of Windows CI entirely since stock `windows-latest` has no EGL driver).

**Symptom:** `cargo test --workspace` on `ubuntu-latest` sits forever in `Test (workspace)` when the corpus test is un-ignored. Hits the 6h GitHub Actions job timeout; no output from the test binary. Windows and local NVIDIA + Wayland boxes pass fine with the shared Mesa workaround from PR #24.

**Investigation log:**

| PR | Theory | Outcome |
|---|---|---|
| #25 | Mesa env override (`LIBGL_ALWAYS_SOFTWARE=1` + `MESA_LOADER_DRIVER_OVERRIDE=llvmpipe`) unsticks the headless EGL path | Ubuntu still hangs |
| #27 | Virtual `$DISPLAY` via `xvfb-run -a` | Ubuntu still hangs |
| #28 | Harness-side rAF nudge in `CorpusRenderer::wait_for_rendering_to_be_ready` (mirrors Servo's own reftest harness) | Nudge fires in 10s; `take_screenshot` still hangs past 40 min |

**What we know:** the hang is past `WebView::wait_for_rendering_to_be_ready`'s gate — the compositor emits at least one frame post-load, but `take_screenshot` still doesn't return. Ruled-in plausible causes: font loading on a headless image cache, late stylesheet settling, or a GPU-pipeline flush specific to the surfaceless EGL backend. Ruled-out: display availability, mesa driver selection, post-load frame emission.

**Acceptance criteria (when this becomes fixable):**

1. A Servo upstream patch to `WebView::take_screenshot` adds either (a) a tighter internal timeout with an error return instead of an indefinite wait, or (b) an explicit "skip the image-cache wait" flag we can set for corpus contexts.
2. OR: a minimal Servo-only repro outside `qsl-renderer` that isolates which internal dependency is blocking; that points at a fix we can apply server-side.

Neither is tractable for Phase 0. Un-ignore on Ubuntu only after one of the two lands.

---

## Reader-render button multi-fire observation

**Observed in:** PR #30 headless probing. A single physical click on the "Render test page in Servo" button logged `reader_render` ~6 times at ~200ms intervals.

**State:** defensive guards shipped in PR #31 (`use_signal` in-flight flag that disables the button mid-invocation, `type="button"` to prevent latent form-submit semantics, `stop_propagation()` + `prevent_default()` on the click event). Code-level, this should be single-fire per physical click; not verified in interactive use because the observation came from a headless session.

**Acceptance criteria:**

1. Next normal interactive session with the button visible: click it once; confirm exactly one `reader_render` log line per click. If verified, delete this section.
2. If the multi-fire recurs even with the guards in place, re-open the investigation — probably something deeper in Dioxus 0.7 event wiring or the webkit2gtk synthetic-event path.

No action required until someone's sitting in front of the app and paying attention to the log. Low priority.

---

## Branch protection not enabled on `main`

**State:** main is unprotected; force-pushes and deletes are not explicitly blocked by GitHub. Every PR check passes or fails visibly, and the project convention is to merge on green, but nothing *enforces* that green.

**Why it's a "known issue" rather than fixed:** enabling branch protection is a repo-admin config change and prior automated attempts were declined by the permission tooling as too broad ("`finish all tech debt items` does not specifically authorize branch-protection changes"). Needs explicit action by the maintainer via GitHub UI (Settings → Branches → Add rule) or an explicit "enable branch protection" directive.

**Acceptance criteria:**

- Branch protection rule on `main` requires the four current checks: `Check (ubuntu-latest)`, `Check (windows-latest)`, `cargo-deny`, `reuse lint`. No required reviews (solo-maintainer project). Force-pushes and deletions blocked. `enforce_admins: false` so the maintainer can still force-merge in genuine emergencies.
