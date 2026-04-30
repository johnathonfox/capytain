<!--
SPDX-FileCopyrightText: 2026 QSL Contributors
SPDX-License-Identifier: Apache-2.0
-->

# Known issues

Open issues we've consciously accepted for Phase 0 and the path out of each. Entries get deleted when they're fixed, not silently left stale — if you fix one, remove the section in the same PR.

This is the short list. Phase-specific deferrals (Fastmail smoke, macOS / Windows runtime) live in `PHASE_0.md`'s "Deferred from Phase 0" section instead; they're too big to belong here.

---

## Branch protection not enabled on `main`

**State:** main is unprotected; force-pushes and deletes are not explicitly blocked by GitHub. Every PR check passes or fails visibly, and the project convention is to merge on green, but nothing *enforces* that green.

**Why it's a "known issue" rather than fixed:** enabling branch protection is a repo-admin config change and prior automated attempts were declined by the permission tooling as too broad ("`finish all tech debt items` does not specifically authorize branch-protection changes"). Needs explicit action by the maintainer via GitHub UI (Settings → Branches → Add rule) or an explicit "enable branch protection" directive.

**Acceptance criteria:**

- Branch protection rule on `main` requires the four current checks: `Check (ubuntu-latest)`, `Check (windows-latest)`, `cargo-deny`, `reuse lint`. No required reviews (solo-maintainer project). Force-pushes and deletions blocked. `enforce_admins: false` so the maintainer can still force-merge in genuine emergencies.

---

<!-- Remote-content placeholders shipped — see qsl_mime::sanitize +
     reader CSS. Removed from this doc per the project rule: fix the
     issue, delete the entry. Sanitizer now marks blocked `<img>`
     tags with `data-qsl-blocked` and the reader CSS frames each as a
     subtle dashed placeholder box; ammonia's default allowlist
     preserves any `width`/`height` attributes so the layout footprint
     is stable when the user clicks "Load images". -->

