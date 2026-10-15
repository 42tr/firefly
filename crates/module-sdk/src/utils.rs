use chrono::{
    DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Utc,
};
use chrono_tz::Asia::Shanghai;
use chrono_tz::Tz;

/// Application timezone supplied by the stdio host; standalone modules default to Shanghai.
pub fn timezone() -> Tz {
    std::env::var("FIREFLY_MODULE_TIMEZONE").ok()
        .and_then(|value| value.parse().ok()).unwrap_or(Shanghai)
}

pub fn today() -> NaiveDate {
    Utc::now().with_timezone(&timezone()).date_naive()
}

/// 获取当前上海时区的日期
pub fn today_in_shanghai() -> NaiveDate {
    Utc::now().with_timezone(&Shanghai).date_naive()
}

/// 当前自然周（周一为起点）在宿主配置时区的起始时刻，返回 UTC 时间。
pub fn current_week_start_utc() -> DateTime<Utc> {
    week_start_utc(today(), timezone())
}

fn week_start_utc(today: NaiveDate, timezone: Tz) -> DateTime<Utc> {
    let monday = today - Duration::days(today.weekday().num_days_from_monday() as i64);
    first_local_instant(monday.and_hms_opt(0, 0, 0).unwrap(), timezone)
        .expect("a week begins at an actual local instant").with_timezone(&Utc)
}

fn first_local_instant(local: chrono::NaiveDateTime, timezone: Tz) -> Option<DateTime<Tz>> {
    // Resolve repeated times to the earlier instant, and advance across gaps.
    (0..=2880).find_map(|minute| {
        local.checked_add_signed(Duration::minutes(minute))
            .and_then(|at| timezone.from_local_datetime(&at).earliest())
    })
}

pub fn day_bounds(timestamp: i64) -> Option<(i64, i64)> {
    day_bounds_in(timestamp, timezone())
}

/// Calendar-day bounds, including the 23/25-hour days at daylight-saving transitions.
pub fn day_bounds_in(timestamp: i64, timezone: Tz) -> Option<(i64, i64)> {
    let date = timezone.timestamp_millis_opt(timestamp).earliest()?.date_naive();
    let start = first_local_instant(date.and_hms_opt(0, 0, 0)?, timezone)?;
    let end = first_local_instant(date.succ_opt()?.and_hms_opt(0, 0, 0)?, timezone)?;
    Some((start.timestamp_millis(), end.timestamp_millis() - 1))
}

/// 计算给定上海时间戳所在天的起止毫秒时间戳
pub fn shanghai_day_bounds(timestamp: i64) -> Option<(i64, i64)> {
    day_bounds_in(timestamp, Shanghai)
}

/// 根据宿主时区的日期和时间构建毫秒时间戳；夏令时缺失时间推进到第一个有效时刻。
pub fn timestamp_from_components(date: NaiveDate, time: Option<NaiveTime>) -> i64 {
    let time = time.unwrap_or_else(|| NaiveTime::from_hms_opt(12, 0, 0).unwrap());
    first_local_instant(date.and_time(time), timezone())
        .expect("local date/time is within the supported range").timestamp_millis()
}

/// 时间戳转时间 %y-%m-%d %H:%M
pub fn timestamp2time(timestamp: i64, format: &str) -> String {
    timezone()
        .timestamp_opt(timestamp, 0)
        .unwrap()
        .format(format)
        .to_string()
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
mod timezone_tests {
    use super::*;
    use chrono::Timelike;

    #[test]
    fn day_bounds_follow_calendar_dates_across_dst() {
        let zone = chrono_tz::America::New_York;
        for (date, hours) in [("2026-03-08T16:00:00Z", 23), ("2026-11-01T17:00:00Z", 25)] {
            let at: DateTime<Utc> = date.parse().unwrap();
            let (start, end) = day_bounds_in(at.timestamp_millis(), zone).unwrap();
            assert_eq!(end - start + 1, hours * 60 * 60 * 1000);
            assert_eq!(zone.timestamp_millis_opt(start).unwrap().hour(), 0);
        }
    }

    #[test]
    fn week_start_and_missing_or_repeated_local_times_use_the_selected_zone() {
        let zone = chrono_tz::America::New_York;
        let date = NaiveDate::from_ymd_opt(2026, 9, 10).unwrap();
        assert_eq!(week_start_utc(date, zone), "2026-09-07T04:00:00Z".parse::<DateTime<Utc>>().unwrap());
        let gap = NaiveDate::from_ymd_opt(2026, 3, 8).unwrap().and_hms_opt(2, 30, 0).unwrap();
        assert_eq!(first_local_instant(gap, zone).unwrap().with_timezone(&Utc), "2026-03-08T07:00:00Z".parse::<DateTime<Utc>>().unwrap());
        let repeated = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap().and_hms_opt(1, 30, 0).unwrap();
        assert_eq!(first_local_instant(repeated, zone).unwrap().with_timezone(&Utc), "2026-11-01T05:30:00Z".parse::<DateTime<Utc>>().unwrap());
    }
}
