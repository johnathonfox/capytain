// SPDX-License-Identifier: Apache-2.0

//! Small UI-side formatting helpers. The Dioxus components stay
//! readable when rendering logic is named and tested here rather
//! than inlined into `rsx!`.

use std::borrow::Cow;

use chrono::{DateTime, Datelike, Local, Utc};
use qsl_core::FolderRole;
use qsl_ipc::MessageFlags;

/// One-character glyph + CSS class describing the dominant IMAP
/// state for a message row. Per `docs/ui-direction.md` § Message
/// list § Flag column glyphs the precedence is:
///
///   `D` draft  >  `F` flagged  >  `R` replied  >  `!` unread  >  `·` read
///
/// Only one glyph renders per row — the spec calls out that
/// stacking two glyphs in the 8px column would be unreadable.
pub fn flag_glyph(flags: &MessageFlags) -> (&'static str, &'static str) {
    if flags.draft {
        ("D", "msg-flag-draft")
    } else if flags.flagged {
        ("F", "msg-flag-flagged")
    } else if flags.answered {
        ("R", "msg-flag-replied")
    } else if !flags.seen {
        ("!", "msg-flag-unread")
    } else {
        ("·", "msg-flag-read")
    }
}

/// Map a raw IMAP / JMAP folder name to its human-friendly display
/// form for the sidebar and message-list header.
///
/// IMAP servers use `INBOX` as the canonical inbox identifier (RFC
/// 3501 §5.1). Older servers also use `Junk` / `Junk E-mail` for
/// what users now expect to see as `Spam`. Everything else passes
/// through unchanged: Gmail, iCloud, Fastmail, etc. all return
/// already-presentable leaf names like `Sent Mail`, `Drafts`,
/// `All Mail`, so this helper deliberately doesn't try to second-
/// guess them.
///
/// Backlog item 3 — see `docs/QSL_BACKLOG_FIXES.md`.
pub fn display_name_for_folder(name: &str) -> &str {
    if name.eq_ignore_ascii_case("INBOX") {
        "Inbox"
    } else if name.eq_ignore_ascii_case("Junk")
        || name.eq_ignore_ascii_case("Junk E-mail")
        || name.eq_ignore_ascii_case("Junk Email")
    {
        "Spam"
    } else {
        name
    }
}

/// Role-aware display name. Same as [`display_name_for_folder`] when
/// the server-provided name already looks human-friendly, but falls
/// back to [`FolderRole::canonical_display_name`] when the name looks
/// unfriendly (all-uppercase ASCII letters, e.g. `DRAFTS` / `SENT` /
/// `TRASH` from a self-hosted IMAP or Microsoft 365 server).
///
/// Gmail and Fastmail return mixed-case names like `Sent Mail` and
/// `All Mail`; we deliberately preserve those rather than overriding
/// them with the canonical role label, since the server name is what
/// the user sees in the official web client and matching that
/// reduces surprise.
///
/// `INBOX` is uppercase per the IMAP spec (RFC 3501 §5.1) and is
/// always normalized to `Inbox`, even when no role is attached.
pub fn display_name_for_folder_with_role<'a>(
    name: &'a str,
    role: Option<&FolderRole>,
) -> Cow<'a, str> {
    // Server name looks unfriendly when it's all uppercase ASCII
    // letters (length > 0). Pure all-caps short codes ("AOL", "BBC")
    // are rare for top-level mailbox names and the worst case is they
    // map to their role's canonical name when one is set — i.e. a
    // user folder named "AOL" with a role attached would render as
    // the role label, which would only happen if some adapter
    // mistakenly tagged a user folder with a role. Plain pass-through
    // is safe when `role.is_none()`.
    let looks_unfriendly = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || !c.is_ascii_alphabetic());
    if looks_unfriendly {
        if let Some(role) = role {
            return Cow::Borrowed(role.canonical_display_name());
        }
    }
    Cow::Borrowed(display_name_for_folder(name))
}

/// Pick the next reader-pane selection after `moved` messages leave
/// the current view. Mirrors Gmail / Apple Mail behaviour: prefer the
/// message *after* the now-departed selection in the visible list,
/// fall back to the one *before* if we're at the end, otherwise clear
/// (the entire list emptied or the selection wasn't there to begin
/// with). Skips any moved ids while walking — a multi-row drop
/// shouldn't land on another id that's also disappearing.
///
/// Pure: takes the visible list snapshot from *before* the move, the
/// current selection, and the ids being moved out. Returns the id to
/// select afterwards (or `None` to clear).
pub fn next_selection_after_move(
    visible: &[qsl_ipc::MessageId],
    current: Option<&qsl_ipc::MessageId>,
    moved: &[qsl_ipc::MessageId],
) -> Option<qsl_ipc::MessageId> {
    let cur = current?;
    let pos = visible.iter().position(|id| id.0 == cur.0)?;
    // Walk forward past the moved cohort.
    if let Some(id) = visible
        .iter()
        .skip(pos + 1)
        .find(|id| !moved.iter().any(|m| m.0 == id.0))
    {
        return Some(id.clone());
    }
    // Nothing forward; walk back.
    visible[..pos]
        .iter()
        .rev()
        .find(|id| !moved.iter().any(|m| m.0 == id.0))
        .cloned()
}

/// Drop-target eligibility for a folder by role. Drag-and-drop into
/// `Important`, `Flagged`, or `All` would either be meaningless (`All
/// Mail` already contains everything) or surprising (Gmail's
/// `Important` and `Starred` are *labels*, not real folders, so a
/// `messages_move` into them would relocate the message rather than
/// just flag it). Block at the UI layer for v1; revisit when label-add
/// semantics ship — see backlog item #14 in `docs/QSL_BACKLOG_FIXES.md`.
pub fn is_drop_blocked(role: Option<&FolderRole>) -> bool {
    matches!(
        role,
        Some(FolderRole::Important) | Some(FolderRole::Flagged) | Some(FolderRole::All)
    )
}

/// Format a [`DateTime<Utc>`] for the message-list date column.
/// Follows `docs/ui-direction.md` § Message list § Timestamp:
///
/// - **Same calendar day** → `HH:MM` (e.g. `14:23`).
/// - **Yesterday** → `yest`.
/// - **Within the last six days** → three-letter weekday (`Mon`, `Tue`).
/// - **Earlier in the same calendar year** → `Mon D` (`Apr 23`).
/// - **Older than that** → `Mon 'YY` (`Mar '25`).
///
/// All comparisons are done in the local timezone so the boundary
/// lines up with the user's wall clock, not UTC's. `now` is
/// injected for testability — production callers pass `Utc::now()`.
pub fn format_relative_date(when: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let when_local = when.with_timezone(&Local);
    let now_local = now.with_timezone(&Local);

    if when_local.date_naive() == now_local.date_naive() {
        return when_local.format("%H:%M").to_string();
    }

    // Days between calendar dates (positive when `when` is in the past).
    let diff_days = (now_local.date_naive() - when_local.date_naive()).num_days();

    if diff_days == 1 {
        return "yest".to_string();
    }

    if (2..7).contains(&diff_days) {
        return when_local.format("%a").to_string();
    }

    if when_local.year() == now_local.year() {
        return when_local.format("%b %-d").to_string();
    }

    // Older: `Mon 'YY` (two-digit year). Apostrophe matches the
    // ui-direction.md spec example `Mar '25`.
    let two_digit_year = when_local.year() % 100;
    format!("{} '{:02}", when_local.format("%b"), two_digit_year)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, m: u32, d: u32, hh: u32, mm: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, hh, mm, 0).unwrap()
    }

    #[test]
    fn same_day_uses_hh_mm() {
        let now = at(2026, 4, 25, 14, 0);
        let when = at(2026, 4, 25, 9, 30);
        // Format depends on the test runner's local TZ; we assert
        // shape rather than exact string.
        let out = format_relative_date(when, now);
        assert_eq!(out.len(), 5, "expected HH:MM, got {out:?}");
        assert!(out.chars().nth(2) == Some(':'), "got {out:?}");
    }

    #[test]
    fn yesterday_renders_as_yest() {
        let now = at(2026, 4, 25, 14, 0);
        let when = now - chrono::Duration::days(1);
        assert_eq!(format_relative_date(when, now), "yest");
    }

    #[test]
    fn two_through_six_days_ago_uses_weekday() {
        let now = at(2026, 4, 25, 14, 0);
        for days in 2..=6 {
            let when = now - chrono::Duration::days(days);
            let out = format_relative_date(when, now);
            assert_eq!(
                out.len(),
                3,
                "expected three-letter weekday for d={days}, got {out:?}"
            );
        }
    }

    #[test]
    fn earlier_this_year_uses_month_day() {
        let now = at(2026, 4, 25, 14, 0);
        let when = at(2026, 1, 3, 9, 0);
        // `%b %-d` → e.g. "Jan 3". Allow a leading-zero variant too
        // in case the platform's strftime ignores `%-d`.
        let out = format_relative_date(when, now);
        assert!(
            out.starts_with("Jan ") && (out.ends_with(" 3") || out.ends_with(" 03")),
            "got {out:?}"
        );
    }

    #[test]
    fn last_year_or_older_uses_two_digit_year_with_apostrophe() {
        let now = at(2026, 4, 25, 14, 0);
        let when = at(2024, 12, 8, 9, 0);
        assert_eq!(format_relative_date(when, now), "Dec '24");
        let earlier = at(2025, 3, 15, 9, 0);
        assert_eq!(format_relative_date(earlier, now), "Mar '25");
    }

    #[test]
    fn flag_glyph_precedence_draft_wins() {
        let f = MessageFlags {
            draft: true,
            flagged: true,
            answered: true,
            ..Default::default()
        };
        assert_eq!(flag_glyph(&f), ("D", "msg-flag-draft"));
    }

    #[test]
    fn flag_glyph_flagged_beats_replied_and_unread() {
        let f = MessageFlags {
            flagged: true,
            answered: true,
            seen: false,
            ..Default::default()
        };
        assert_eq!(flag_glyph(&f), ("F", "msg-flag-flagged"));
    }

    #[test]
    fn flag_glyph_replied_beats_unread() {
        let f = MessageFlags {
            answered: true,
            seen: false,
            ..Default::default()
        };
        assert_eq!(flag_glyph(&f), ("R", "msg-flag-replied"));
    }

    #[test]
    fn flag_glyph_unread_when_unseen() {
        // `seen` defaults to false; everything else default → "unread".
        let f = MessageFlags::default();
        assert_eq!(flag_glyph(&f), ("!", "msg-flag-unread"));
    }

    #[test]
    fn flag_glyph_read_dot_when_seen() {
        let f = MessageFlags {
            seen: true,
            ..Default::default()
        };
        assert_eq!(flag_glyph(&f), ("·", "msg-flag-read"));
    }

    #[test]
    fn display_name_uppercases_inbox() {
        assert_eq!(display_name_for_folder("INBOX"), "Inbox");
        assert_eq!(display_name_for_folder("inbox"), "Inbox");
        assert_eq!(display_name_for_folder("Inbox"), "Inbox");
    }

    #[test]
    fn display_name_remaps_junk_variants_to_spam() {
        assert_eq!(display_name_for_folder("Junk"), "Spam");
        assert_eq!(display_name_for_folder("Junk E-mail"), "Spam");
        assert_eq!(display_name_for_folder("Junk Email"), "Spam");
        assert_eq!(display_name_for_folder("JUNK"), "Spam");
    }

    #[test]
    fn display_name_passes_through_other_names() {
        // Gmail-style leaf names are already presentable.
        assert_eq!(display_name_for_folder("Sent Mail"), "Sent Mail");
        assert_eq!(display_name_for_folder("All Mail"), "All Mail");
        assert_eq!(display_name_for_folder("Drafts"), "Drafts");
        assert_eq!(display_name_for_folder("Newsletters"), "Newsletters");
        // Unknown / user-defined names too.
        assert_eq!(display_name_for_folder("Receipts/2026"), "Receipts/2026");
    }

    #[test]
    fn display_name_with_role_canonicalizes_unfriendly_names() {
        // Self-hosted IMAP / Microsoft 365 conventions: ALL CAPS leaf
        // names with a SPECIAL-USE role attached. Map to canonical.
        assert_eq!(
            display_name_for_folder_with_role("DRAFTS", Some(&FolderRole::Drafts)),
            "Drafts"
        );
        assert_eq!(
            display_name_for_folder_with_role("SENT", Some(&FolderRole::Sent)),
            "Sent"
        );
        assert_eq!(
            display_name_for_folder_with_role("TRASH", Some(&FolderRole::Trash)),
            "Trash"
        );
        // INBOX always maps to Inbox even with no role attached
        // (IMAP RFC 3501 §5.1 mandate).
        assert_eq!(
            display_name_for_folder_with_role("INBOX", Some(&FolderRole::Inbox)),
            "Inbox"
        );
        assert_eq!(display_name_for_folder_with_role("INBOX", None), "Inbox");
    }

    #[test]
    fn display_name_with_role_keeps_friendly_server_names() {
        // Gmail / Fastmail return mixed-case display names that the
        // user already recognizes from the official client. Don't
        // override even when a role is set.
        assert_eq!(
            display_name_for_folder_with_role("Sent Mail", Some(&FolderRole::Sent)),
            "Sent Mail"
        );
        assert_eq!(
            display_name_for_folder_with_role("All Mail", Some(&FolderRole::All)),
            "All Mail"
        );
        assert_eq!(
            display_name_for_folder_with_role("Drafts", Some(&FolderRole::Drafts)),
            "Drafts"
        );
        // User-defined folders pass through.
        assert_eq!(
            display_name_for_folder_with_role("Receipts/2026", None),
            "Receipts/2026"
        );
    }

    fn mid(s: &str) -> qsl_ipc::MessageId {
        qsl_ipc::MessageId(s.to_string())
    }

    #[test]
    fn next_after_move_picks_next_visible() {
        let visible = vec![mid("a"), mid("b"), mid("c"), mid("d")];
        let cur = mid("b");
        let moved = vec![mid("b")];
        assert_eq!(
            next_selection_after_move(&visible, Some(&cur), &moved),
            Some(mid("c"))
        );
    }

    #[test]
    fn next_after_move_skips_other_moved() {
        let visible = vec![mid("a"), mid("b"), mid("c"), mid("d")];
        let cur = mid("b");
        let moved = vec![mid("b"), mid("c")];
        assert_eq!(
            next_selection_after_move(&visible, Some(&cur), &moved),
            Some(mid("d"))
        );
    }

    #[test]
    fn next_after_move_falls_back_to_previous_at_end() {
        let visible = vec![mid("a"), mid("b"), mid("c")];
        let cur = mid("c");
        let moved = vec![mid("c")];
        assert_eq!(
            next_selection_after_move(&visible, Some(&cur), &moved),
            Some(mid("b"))
        );
    }

    #[test]
    fn next_after_move_returns_none_when_list_empties() {
        let visible = vec![mid("a"), mid("b")];
        let cur = mid("a");
        let moved = vec![mid("a"), mid("b")];
        assert_eq!(
            next_selection_after_move(&visible, Some(&cur), &moved),
            None
        );
    }

    #[test]
    fn next_after_move_handles_missing_current() {
        let visible = vec![mid("a"), mid("b")];
        assert_eq!(next_selection_after_move(&visible, None, &[]), None);
        // Selection points at something not in the list (race) → None.
        let stranger = mid("z");
        let moved = vec![mid("z")];
        assert_eq!(
            next_selection_after_move(&visible, Some(&stranger), &moved),
            None
        );
    }

    #[test]
    fn drop_blocked_for_label_view_roles() {
        // Gmail label-views: Important / Flagged / All are read-only
        // from a "move into me" perspective.
        assert!(is_drop_blocked(Some(&FolderRole::Important)));
        assert!(is_drop_blocked(Some(&FolderRole::Flagged)));
        assert!(is_drop_blocked(Some(&FolderRole::All)));
    }

    #[test]
    fn drop_allowed_for_real_folders() {
        // Real mailbox folders: drops should land normally.
        assert!(!is_drop_blocked(Some(&FolderRole::Inbox)));
        assert!(!is_drop_blocked(Some(&FolderRole::Sent)));
        assert!(!is_drop_blocked(Some(&FolderRole::Drafts)));
        assert!(!is_drop_blocked(Some(&FolderRole::Trash)));
        assert!(!is_drop_blocked(Some(&FolderRole::Spam)));
        assert!(!is_drop_blocked(Some(&FolderRole::Archive)));
        // User-defined folders (no role) are always allowed too.
        assert!(!is_drop_blocked(None));
    }

    #[test]
    fn display_name_with_role_falls_back_when_role_missing() {
        // ALL CAPS leaf name without a role tag: we don't know what
        // it should map to, so let the string-based helper do its
        // existing INBOX / Junk handling and pass everything else
        // through untouched.
        assert_eq!(display_name_for_folder_with_role("DRAFTS", None), "DRAFTS");
        assert_eq!(display_name_for_folder_with_role("URGENT", None), "URGENT");
    }
}
