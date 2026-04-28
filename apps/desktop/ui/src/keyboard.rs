// SPDX-License-Identifier: Apache-2.0

//! Keyboard-shortcut parsing for the QSL desktop app.
//!
//! This module is the *parser*: keystroke → `KeyboardCommand`. The
//! dispatcher that turns a `KeyboardCommand` into actual side effects
//! (opening compose, calling IPC, toggling overlays) lives in `app.rs`
//! because it needs the App-root signals.
//!
//! Pure logic, no Dioxus or wasm dependencies — kept in its own
//! module so it stays reachable from `cargo test` on the host (see
//! `main.rs`'s `cfg(all(test, not(target_arch = "wasm32")))`
//! mounting).
//!
//! The keymap follows Gmail's convention (per the post-phase-2 plan
//! assumption A3): single-letter shortcuts when no modifier is held,
//! `?` for help, `Esc` for cancel. We deliberately bail out when the
//! user is holding `Ctrl` / `Cmd` so OS / browser shortcuts pass
//! through (`Ctrl+C`, `Cmd+R`, etc.).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyboardCommand {
    /// Open a blank compose window.
    Compose,
    /// Close compose if open; otherwise clear the reader's message
    /// selection. If the help overlay is up, dismiss it first.
    Cancel,
    /// Archive the currently-selected message.
    Archive,
    /// Delete the currently-selected message.
    Delete,
    /// Open a reply draft for the currently-selected message.
    Reply,
    /// Open a reply-all draft for the currently-selected message.
    ReplyAll,
    /// Open a forward draft for the currently-selected message.
    Forward,
    /// Show / hide the keyboard cheatsheet overlay.
    ToggleHelp,
}

/// Map a `KeyboardEvent.key` value plus the modifier state to a
/// command, or `None` if the keystroke isn't ours to handle.
///
/// `key` is whatever the browser reports — for `Shift+/` the value is
/// `"?"`, for `Shift+3` it's `"#"`, for `Escape` it's `"Escape"`. We
/// match against that rather than reconstructing from raw codes.
///
/// `ctrl_or_meta` is `true` if the user is holding `Ctrl` (Linux /
/// Windows) or `Cmd` (macOS). Either modifier defers to the OS.
pub fn parse(key: &str, ctrl_or_meta: bool) -> Option<KeyboardCommand> {
    if ctrl_or_meta {
        return None;
    }
    match key {
        "c" => Some(KeyboardCommand::Compose),
        "Escape" => Some(KeyboardCommand::Cancel),
        "e" => Some(KeyboardCommand::Archive),
        "#" => Some(KeyboardCommand::Delete),
        "r" => Some(KeyboardCommand::Reply),
        "a" => Some(KeyboardCommand::ReplyAll),
        "f" => Some(KeyboardCommand::Forward),
        "?" => Some(KeyboardCommand::ToggleHelp),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_unmodified_letters() {
        assert_eq!(parse("c", false), Some(KeyboardCommand::Compose));
        assert_eq!(parse("e", false), Some(KeyboardCommand::Archive));
        assert_eq!(parse("r", false), Some(KeyboardCommand::Reply));
        assert_eq!(parse("a", false), Some(KeyboardCommand::ReplyAll));
        assert_eq!(parse("f", false), Some(KeyboardCommand::Forward));
    }

    #[test]
    fn parses_special_keys() {
        assert_eq!(parse("Escape", false), Some(KeyboardCommand::Cancel));
        // Browsers report Shift+/ as `key = "?"`, Shift+3 as `key = "#"` —
        // we match on the produced glyph, not raw codes.
        assert_eq!(parse("?", false), Some(KeyboardCommand::ToggleHelp));
        assert_eq!(parse("#", false), Some(KeyboardCommand::Delete));
    }

    #[test]
    fn ctrl_or_meta_swallows_everything() {
        // Ctrl+C / Cmd+R should reach the OS, not us — even for keys
        // whose unmodified form would be ours.
        for key in ["c", "e", "r", "a", "f", "Escape", "?", "#"] {
            assert_eq!(
                parse(key, true),
                None,
                "Ctrl/Cmd+{key} must not produce a KeyboardCommand"
            );
        }
    }

    #[test]
    fn unknown_keys_yield_none() {
        for key in ["x", "z", "Enter", "Tab", "ArrowUp", " "] {
            assert_eq!(parse(key, false), None, "{key} should not be ours");
        }
    }

    #[test]
    fn uppercase_letters_do_not_match() {
        // Caps-lock or Shift+letter sends the uppercase form. We don't
        // claim any uppercase shortcuts in v0.1; falling through to
        // None lets the user type capitals into a focused field with
        // no surprise side effect even if `is_typing` misses the
        // focus check for some reason.
        assert_eq!(parse("C", false), None);
        assert_eq!(parse("E", false), None);
    }
}
