# Changelog

## Unreleased

- Renamed the project from **Capytain** to **QSL**. Cargo packages
  (`capytain-*` → `qsl-*`), the Tauri app identifier
  (`app.capytain.desktop` → `app.qsl.desktop`), build-time env var
  prefix (`CAPYTAIN_*` → `QSL_*`, including the Gmail / Fastmail
  OAuth client-id slots and the `*_SKIP_UI_BUILD` /
  `*_CORPUS_REGEN` switches), workspace dep references, log
  prefixes, copyright notices, and all documentation have all been
  updated. **Action required for existing checkouts:**
  - Re-export `QSL_GMAIL_CLIENT_ID` / `QSL_GMAIL_CLIENT_SECRET` /
    `QSL_FASTMAIL_CLIENT_ID` / `QSL_FASTMAIL_CLIENT_SECRET` (any
    `.env` or shell config still using the `CAPYTAIN_*` names will
    fail to build the `qsl-auth` crate).
  - The Tauri identifier change means a fresh install on the same
    machine will look for its sqlite DB and keyring entries under
    `app.qsl.desktop` rather than `app.capytain.desktop`. Migrate
    existing data by moving `~/.local/share/app.capytain.desktop`
    → `~/.local/share/app.qsl.desktop` (or the equivalent on
    macOS / Windows) before first launch.
  - The git remote and the local working directory are not part of
    this commit — see the rebrand PR description for the
    `gh repo rename` and `mv` commands the maintainer runs by hand.
