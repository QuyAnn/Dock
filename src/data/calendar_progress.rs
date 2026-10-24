use chrono::{Datelike, NaiveDate, NaiveDateTime, Timelike};

/// Calendar fractions use local wall time: a day is 00:00..24:00 even on DST days.
pub fn fractions(now: NaiveDateTime) -> (f32, f32, f32, u32) {
    let day = now.num_seconds_from_midnight() as f32 / 86400.0;
    let date = now.date();
    let first = date.with_day(1).unwrap();
    let next = if date.month() == 12 { NaiveDate::from_ymd_opt(date.year() + 1, 1, 1) }
        else { NaiveDate::from_ymd_opt(date.year(), date.month() + 1, 1) }.unwrap();
    let days = (next - first).num_days() as f32;
    let year_days = if NaiveDate::from_ymd_opt(date.year(), 2, 29).is_some() { 366 } else { 365 };
    (day, (date.day0() as f32 + day) / days, (date.ordinal0() as f32 + day) / year_days as f32, year_days)
}

/// Fixed UTC offsets are explicit user settings; they intentionally do not infer DST rules.
pub fn city_time(now: chrono::DateTime<chrono::Utc>, offset_minutes: i32) -> chrono::DateTime<chrono::FixedOffset> {
    now.with_timezone(&chrono::FixedOffset::east_opt(offset_minutes.clamp(-1439, 1439) * 60).unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn leap_month_year_and_midnight() {
        let (d,m,y,n) = fractions(NaiveDate::from_ymd_opt(2024,2,29).unwrap().and_hms_opt(12,0,0).unwrap());
        assert_eq!(d,0.5); assert!((m - 28.5/29.0).abs() < 0.0001);
        assert_eq!(n,366); assert!((y - 59.5/366.0).abs() < 0.0001);
        let (d,m,y,n) = fractions(NaiveDate::from_ymd_opt(2025,1,1).unwrap().and_hms_opt(0,0,0).unwrap());
        assert_eq!((d,m,y,n),(0.0,0.0,0.0,365));
    }
    #[test]
    fn world_clock_crosses_date_and_supports_half_hours() {
        let n = chrono::DateTime::parse_from_rfc3339("2026-01-01T22:00:00Z").unwrap().to_utc();
        assert_eq!(city_time(n,330).format("%d %H:%M").to_string(), "02 03:30");
        assert_eq!(city_time(n,-300).format("%d %H:%M").to_string(), "01 17:00");
    }
}
