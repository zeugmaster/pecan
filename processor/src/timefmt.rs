//! UTC calendar formatting without a date-time dependency: the processor only
//! needs ISO dates for checklist copy and the activity CSV export.

/// Days since 1970-01-01 → (year, month, day) in the proleptic Gregorian
/// calendar (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// `2026-10-20`
pub fn iso_date(ts: u64) -> String {
    let (year, month, day) = civil_from_days((ts / 86_400) as i64);
    format!("{year:04}-{month:02}-{day:02}")
}

/// `2026-10-20T14:03:09Z`
pub fn iso_datetime(ts: u64) -> String {
    let secs = ts % 86_400;
    format!(
        "{}T{:02}:{:02}:{:02}Z",
        iso_date(ts),
        secs / 3_600,
        secs % 3_600 / 60,
        secs % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_utc_dates() {
        assert_eq!(iso_datetime(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_date(951_782_400), "2000-02-29");
        assert_eq!(iso_date(1_792_454_400), "2026-10-20");
        assert_eq!(iso_datetime(1_761_000_000), "2025-10-20T22:40:00Z");
    }
}
