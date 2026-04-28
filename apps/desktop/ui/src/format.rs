// SPDX-License-Identifier: Apache-2.0

//! Small UI-side formatting helpers. The Dioxus components stay
//! readable when rendering logic is named and tested here rather
//! than inlined into `rsx!`.

use chrono::{DateTime, Datelike, Local, Utc};
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
}
