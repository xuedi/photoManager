//! The one date format, without pulling in a calendar library.

/// `YYYY-MM-DD HH:MM:SS` in UTC, now.
pub fn now() -> String {
    stamp(std::time::SystemTime::now())
}

/// `YYYY-MM-DD HH:MM:SS` in UTC.
pub fn stamp(time: std::time::SystemTime) -> String {
    let seconds = time
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or_default();
    let (days, rest) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    let (year, month, day) = civil(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        rest / 3600,
        (rest % 3600) / 60,
        rest % 60
    )
}

/// Days since 1970-01-01 as a date, by Howard Hinnant's civil_from_days.
fn civil(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u32;
    (year + i64::from(month <= 2), month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_timestamp_is_the_one_date_format() {
        assert_eq!(stamp(std::time::UNIX_EPOCH), "1970-01-01 00:00:00");
        assert_eq!(
            stamp(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_758_585_600)),
            "2025-09-23 00:00:00"
        );
        assert_eq!(
            stamp(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_078_027_261)),
            "2004-02-29 04:01:01",
            "a leap day"
        );
    }

    #[test]
    fn now_reads_as_the_one_format() {
        let text = now();
        assert_eq!(text.len(), 19);
        assert_eq!(&text[4..5], "-");
        assert_eq!(&text[10..11], " ");
        assert_eq!(&text[13..14], ":");
    }
}
