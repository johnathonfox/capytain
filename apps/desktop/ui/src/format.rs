// SPDX-License-Identifier: Apache-2.0

//! Small UI-side formatting helpers. The Dioxus components stay
//! readable when rendering logic is named and tested here rather
//! than inlined into `rsx!`.
//!
//! Phase 2 Week 16 ships only [`format_relative_date`]; the module
//! is the natural home for any future "this string came from raw
//! data" helpers (e.g. byte-count formatting for attachment sizes).

use chrono::{DateTime, Datelike, Local, Utc};

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
}
