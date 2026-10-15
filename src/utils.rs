use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;

/// Start of the current Monday-based week in the application timezone.
pub fn current_week_start_utc() -> DateTime<Utc> {
    let timezone = crate::config::timezone();
    week_start_utc(Utc::now().with_timezone(&timezone).date_naive(), timezone)
}

fn week_start_utc(today: NaiveDate, timezone: Tz) -> DateTime<Utc> {
    let monday = today - Duration::days(today.weekday().num_days_from_monday() as i64);
    let midnight = monday.and_hms_opt(0, 0, 0).unwrap();
    // Some zones skip midnight or an entire date. Use the first actual instant.
    (0..=2880).find_map(|minute| {
        timezone.from_local_datetime(&(midnight + Duration::minutes(minute))).earliest()
    }).expect("a week begins at an actual local instant").with_timezone(&Utc)
}

pub fn truncate_to_char_boundary(value: &mut String, limit: usize) {
    if value.len() <= limit {
        return;
    }
    let mut idx = limit;
    while idx > 0 && !value.is_char_boundary(idx) {
        idx -= 1;
    }
    value.truncate(idx);
}

#[cfg(test)]
mod week_tests {
    use super::*;

    #[test]
    fn week_start_is_monday_midnight_shanghai() {
        let timezone = chrono_tz::Asia::Shanghai;
        // 2026-09-10 是周四，所在自然周的周一是 2026-09-07 00:00（UTC+8）= 2026-09-06 16:00 UTC
        let expected = Utc.with_ymd_and_hms(2026, 9, 6, 16, 0, 0).unwrap();
        let thursday = NaiveDate::from_ymd_opt(2026, 9, 10).unwrap();
        assert_eq!(week_start_utc(thursday, timezone), expected);
        // 周日仍属于同一周
        let sunday = NaiveDate::from_ymd_opt(2026, 9, 13).unwrap();
        assert_eq!(week_start_utc(sunday, timezone), expected);
        // 周一当天从当天零点开始
        let monday = NaiveDate::from_ymd_opt(2026, 9, 7).unwrap();
        assert_eq!(week_start_utc(monday, timezone), expected);
    }

    #[test]
    fn week_start_uses_the_selected_zone_and_its_dst_offset() {
        let date = NaiveDate::from_ymd_opt(2026, 9, 10).unwrap();
        assert_eq!(week_start_utc(date, chrono_tz::UTC), Utc.with_ymd_and_hms(2026, 9, 7, 0, 0, 0).unwrap());
        assert_eq!(week_start_utc(date, chrono_tz::America::New_York), Utc.with_ymd_and_hms(2026, 9, 7, 4, 0, 0).unwrap());
    }
}
