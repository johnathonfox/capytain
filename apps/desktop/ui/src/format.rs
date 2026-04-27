// SPDX-License-Identifier: Apache-2.0

//! Small UI-side formatting helpers. The Dioxus components stay
//! readable when rendering logic is named and tested here rather
//! than inlined into `rsx!`.

use chrono::{DateTime, Datelike, Local, Utc};

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

/// Format a [`DateTime<Utc>`] for the message-list date column,
/// switching display style by recency relative to `now`:
///
/// - **Same calendar day** → `HH:MM` in the local timezone.
/// - **Within the last six days** → three-letter weekday (`Mon`, `Tue`).
/// - **Earlier in the same calendar year** → `Mon D` (`Apr 3`).
/// - **Older than that** → `YYYY-MM-DD` (`2024-12-08`).
///
/// `now` is injected for testability — the caller normally passes
/// `Utc::now()`. All comparisons are done in the local timezone so
/// the boundary lines up with the user's wall clock, not UTC's.
pub fn format_relative_date(when: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let when_local = when.with_timezone(&Local);
    let now_local = now.with_timezone(&Local);

    if when_local.date_naive() == now_local.date_naive() {
        return when_local.format("%H:%M").to_string();
    }

    // Days between calendar dates (positive when `when` is in the past).
    let diff_days = (now_local.date_naive() - when_local.date_naive()).num_days();

    if (1..7).contains(&diff_days) {
        return when_local.format("%a").to_string();
    }

    if when_local.year() == now_local.year() {
        return when_local.format("%b %-d").to_string();
    }

    when_local.format("%Y-%m-%d").to_string()
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
    fn yesterday_through_six_days_ago_uses_weekday() {
        let now = at(2026, 4, 25, 14, 0);
        for days in 1..=6 {
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
    fn last_year_or_older_uses_iso_date() {
        let now = at(2026, 4, 25, 14, 0);
        let when = at(2024, 12, 8, 9, 0);
        assert_eq!(format_relative_date(when, now), "2024-12-08");
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
