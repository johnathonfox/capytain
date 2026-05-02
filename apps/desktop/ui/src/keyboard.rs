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
    /// Focus the search bar above the message list. `/` matches
    /// Gmail; the dispatcher calls `.focus()` on the search input.
    FocusSearch,
    /// Move the message-list selection to the next message.
    /// `j` matches Gmail; wraps at the end of the list.
    NextMessage,
    /// Move the message-list selection to the previous message.
    /// `k` matches Gmail; wraps at the start of the list.
    PrevMessage,
    /// Jump to the next unread message after the current selection.
    /// `n` — there is no canonical Gmail binding for this (Gmail's
    /// `n` walks the conversation, not unread mail), so we claim
    /// the freed key. Wraps to the first unread when no later one
    /// exists.
    NextUnread,
    /// Toggle the command palette overlay (⌘K / Ctrl+K).
    /// The only Ctrl/Cmd-modified shortcut we claim — every other
    /// modified keystroke still passes through to the OS / browser.
    TogglePalette,
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
        // ⌘K / Ctrl+K toggles the command palette. Every other modified
        // keystroke passes through to the OS / browser.
        return match key {
            "k" | "K" => Some(KeyboardCommand::TogglePalette),
            _ => None,
        };
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
        "/" => Some(KeyboardCommand::FocusSearch),
        "j" => Some(KeyboardCommand::NextMessage),
        "k" => Some(KeyboardCommand::PrevMessage),
        "n" => Some(KeyboardCommand::NextUnread),
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
        assert_eq!(parse("/", false), Some(KeyboardCommand::FocusSearch));
    }

    #[test]
    fn ctrl_or_meta_swallows_everything_except_palette() {
        // Ctrl+C / Cmd+R should reach the OS, not us — every modified
        // keystroke except `k` (palette toggle) passes through.
        for key in ["c", "e", "r", "a", "f", "j", "n", "Escape", "?", "#", "/"] {
            assert_eq!(
                parse(key, true),
                None,
                "Ctrl/Cmd+{key} must not produce a KeyboardCommand"
            );
        }
    }

    #[test]
    fn parses_n_for_next_unread() {
        assert_eq!(parse("n", false), Some(KeyboardCommand::NextUnread));
    }

    #[test]
    fn ctrl_or_meta_k_toggles_palette() {
        assert_eq!(parse("k", true), Some(KeyboardCommand::TogglePalette));
        assert_eq!(parse("K", true), Some(KeyboardCommand::TogglePalette));
        // Unmodified `k` still navigates messages.
        assert_eq!(parse("k", false), Some(KeyboardCommand::PrevMessage));
    }

    #[test]
    fn parses_j_k_navigation() {
        assert_eq!(parse("j", false), Some(KeyboardCommand::NextMessage));
        assert_eq!(parse("k", false), Some(KeyboardCommand::PrevMessage));
    }

    #[test]
    fn unknown_keys_yield_none() {
        for key in ["x", "z", "Enter", "Tab", "ArrowUp", " "] {
            assert_eq!(parse(key, false), None, "{key} should not be ours");
        }
    }

    #[test]
    fn n_is_claimed_for_next_unread() {
        // Sanity: `n` is in our keymap so it doesn't fall through
        // to the unknown-keys list above.
        assert!(parse("n", false).is_some());
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
