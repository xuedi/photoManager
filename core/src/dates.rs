//! Dates as a photo holds them: the one format, the offset a time zone gives a local time, and a
//! shift a camera's clock is moved by. The IANA rules are the system's, through `jiff`; the one
//! date format stays ours.

use jiff::Span;
use jiff::civil::DateTime;
use jiff::tz::{AmbiguousOffset, TimeZone};

/// `YYYY-MM-DD HH:MM:SS` as a local time, or why it is not one.
pub fn parse(at: &str) -> Result<DateTime, String> {
    let wrong = || format!("{at:?} is not YYYY-MM-DD HH:MM:SS");
    let (date, time) = at.trim().split_once(' ').ok_or_else(wrong)?;
    let numbers = |text: &str, separator: char, widths: [usize; 3]| -> Option<[i64; 3]> {
        let parts: Vec<&str> = text.split(separator).collect();
        if parts.len() != 3 {
            return None;
        }
        let mut found = [0; 3];
        for (index, (part, width)) in parts.iter().zip(widths).enumerate() {
            if part.len() != width || !part.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            found[index] = part.parse().ok()?;
        }
        Some(found)
    };
    let [year, month, day] = numbers(date, '-', [4, 2, 2]).ok_or_else(wrong)?;
    let [hour, minute, second] = numbers(time, ':', [2, 2, 2]).ok_or_else(wrong)?;
    DateTime::new(
        year as i16,
        month as i8,
        day as i8,
        hour as i8,
        minute as i8,
        second as i8,
        0,
    )
    .map_err(|_| format!("{at} is not a day on the calendar"))
}

/// The one date format.
pub fn format(at: DateTime) -> String {
    at.strftime("%Y-%m-%d %H:%M:%S").to_string()
}

/// A local time as seconds on one line, to measure between two of them.
pub fn seconds(at: DateTime) -> i64 {
    TimeZone::UTC
        .to_timestamp(at)
        .map(|stamp| stamp.as_second())
        .unwrap_or_default()
}

/// The local time that many seconds stand for.
pub fn from_seconds(seconds: i64) -> Option<DateTime> {
    let stamp = jiff::Timestamp::from_second(seconds).ok()?;
    Some(TimeZone::UTC.to_datetime(stamp))
}

/// `+08:00`.
pub fn offset_text(seconds: i32) -> String {
    let sign = if seconds < 0 { '-' } else { '+' };
    let minutes = seconds.unsigned_abs() / 60;
    format!("{sign}{:02}:{:02}", minutes / 60, minutes % 60)
}

/// The offset a zone had at a local time, from its IANA rules. The hour summer time skips and the
/// hour it repeats have no one offset, and are refused with that reason.
pub fn offset_in(zone: &str, at: &str) -> Result<String, String> {
    let rules = TimeZone::get(zone).map_err(|_| format!("the time zone {zone} is not known on this computer"))?;
    let local = parse(at)?;
    match rules.to_ambiguous_timestamp(local).offset() {
        AmbiguousOffset::Unambiguous { offset } => Ok(offset_text(offset.seconds())),
        AmbiguousOffset::Gap { .. } => Err(format!("{at} never happened in {zone}: the clocks went forward")),
        AmbiguousOffset::Fold { .. } => Err(format!("{at} happened twice in {zone}: the clocks went back")),
    }
}

/// How far a camera's clock was off: years, days and a time of day, forward or back. Written the
/// way it is typed, `-640d` or `+1y 2d 03:00`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shift {
    pub back: bool,
    pub years: i64,
    pub days: i64,
    pub seconds: i64,
}

const SHIFT_SHAPE: &str = "a shift is written like -640d or +1y 2d 03:00";

impl Shift {
    /// Whole days.
    pub fn days(days: i64) -> Shift {
        Shift {
            back: days < 0,
            years: 0,
            days: days.abs(),
            seconds: 0,
        }
    }

    /// A difference in seconds, rounded to the minute, as days and a time of day.
    pub fn rounded(seconds: i64) -> Shift {
        let minutes = (seconds as f64 / 60.0).round() as i64;
        let total = minutes.abs();
        Shift {
            back: minutes < 0,
            years: 0,
            days: total / 1440,
            seconds: (total % 1440) * 60,
        }
    }

    pub fn is_nothing(&self) -> bool {
        self.years == 0 && self.days == 0 && self.seconds == 0
    }

    /// Whether it moves a photo by more than a year, which a question says out loud.
    pub fn over_a_year(&self) -> bool {
        self.years > 1 || (self.years == 1 && (self.days > 0 || self.seconds > 0)) || self.days > 366
    }

    pub fn read(text: &str) -> Result<Shift, String> {
        let text = text.trim();
        let (back, rest) = match text.chars().next() {
            Some('-') => (true, &text[1..]),
            Some('+') => (false, &text[1..]),
            _ => (false, text),
        };
        let mut shift = Shift {
            back,
            years: 0,
            days: 0,
            seconds: 0,
        };
        let (mut years, mut days, mut time) = (false, false, false);
        for part in rest.split_whitespace() {
            let wrong = || format!("{part:?} is not part of a shift: {SHIFT_SHAPE}");
            let number = |digits: &str| -> Result<i64, String> {
                match !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
                    true => digits.parse().map_err(|_| wrong()),
                    false => Err(wrong()),
                }
            };
            if let Some(digits) = part.strip_suffix('y') {
                if std::mem::replace(&mut years, true) {
                    return Err(format!("the years are given twice: {SHIFT_SHAPE}"));
                }
                shift.years = number(digits)?;
            } else if let Some(digits) = part.strip_suffix('d') {
                if std::mem::replace(&mut days, true) {
                    return Err(format!("the days are given twice: {SHIFT_SHAPE}"));
                }
                shift.days = number(digits)?;
            } else if part.contains(':') {
                if std::mem::replace(&mut time, true) {
                    return Err(format!("the time is given twice: {SHIFT_SHAPE}"));
                }
                let pieces: Vec<&str> = part.split(':').collect();
                let [hours, minutes, seconds] = match pieces.as_slice() {
                    [hours, minutes] => [number(hours)?, number(minutes)?, 0],
                    [hours, minutes, seconds] => [number(hours)?, number(minutes)?, number(seconds)?],
                    _ => return Err(wrong()),
                };
                if minutes > 59 || seconds > 59 || pieces[1].len() != 2 {
                    return Err(wrong());
                }
                shift.seconds = hours * 3600 + minutes * 60 + seconds;
            } else {
                return Err(wrong());
            }
        }
        if !(years || days || time) {
            return Err(format!("{text:?} is not a shift: {SHIFT_SHAPE}"));
        }
        if shift.years > 200 || shift.days > 100_000 || shift.seconds > 100_000 * 86_400 {
            return Err(format!("{text} is further than any camera's clock is off"));
        }
        if shift.is_nothing() {
            return Err("a shift of nothing changes nothing".to_string());
        }
        Ok(shift)
    }

    pub fn written(&self) -> String {
        let mut parts = Vec::new();
        if self.years > 0 {
            parts.push(format!("{}y", self.years));
        }
        if self.days > 0 {
            parts.push(format!("{}d", self.days));
        }
        if self.seconds > 0 || parts.is_empty() {
            let (hours, rest) = (self.seconds / 3600, self.seconds % 3600);
            parts.push(match rest % 60 {
                0 => format!("{hours:02}:{:02}", rest / 60),
                seconds => format!("{hours:02}:{:02}:{seconds:02}", rest / 60),
            });
        }
        format!("{}{}", if self.back { '-' } else { '+' }, parts.join(" "))
    }

    /// The local time moved by the shift, in the one format.
    pub fn apply(&self, at: &str) -> Result<String, String> {
        let local = parse(at)?;
        let span = Span::new()
            .try_years(self.years)
            .and_then(|span| span.try_days(self.days))
            .and_then(|span| span.try_seconds(self.seconds))
            .map_err(|_| format!("{} is too far", self.written()))?;
        let span = if self.back { span.negate() } else { span };
        local
            .checked_add(span)
            .map(format)
            .map_err(|_| format!("{at} moved by {} is off the calendar", self.written()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_one_format_reads_and_writes_back() {
        let at = parse("2016-06-11 10:02:00").unwrap();
        assert_eq!(format(at), "2016-06-11 10:02:00");
        assert!(parse("2016-06-11").is_err());
        assert!(parse("2016-02-30 10:00:00").is_err(), "not a day");
        assert!(parse("2016-6-11 10:02:00").is_err());
        assert_eq!(
            from_seconds(seconds(at) + 61).map(format).as_deref(),
            Some("2016-06-11 10:03:01")
        );
    }

    #[test]
    fn beijing_has_one_offset_all_year() {
        assert_eq!(offset_in("Asia/Shanghai", "2012-01-15 12:00:00").unwrap(), "+08:00");
        assert_eq!(offset_in("Asia/Shanghai", "2012-07-15 12:00:00").unwrap(), "+08:00");
    }

    #[test]
    fn hamburg_follows_summer_time() {
        assert_eq!(offset_in("Europe/Berlin", "2016-01-11 10:00:00").unwrap(), "+01:00");
        assert_eq!(offset_in("Europe/Berlin", "2016-07-11 10:00:00").unwrap(), "+02:00");
        assert_eq!(offset_in("America/New_York", "2016-07-11 10:00:00").unwrap(), "-04:00");
        assert_eq!(offset_in("Asia/Kolkata", "2016-07-11 10:00:00").unwrap(), "+05:30");
    }

    #[test]
    fn the_hour_summer_time_skips_or_repeats_is_refused() {
        let gap = offset_in("Europe/Berlin", "2016-03-27 02:30:00").unwrap_err();
        assert!(gap.contains("never happened"), "{gap}");
        let fold = offset_in("Europe/Berlin", "2016-10-30 02:30:00").unwrap_err();
        assert!(fold.contains("happened twice"), "{fold}");
        assert_eq!(offset_in("Europe/Berlin", "2016-03-27 03:30:00").unwrap(), "+02:00");
        assert!(offset_in("Nowhere/Atlantis", "2016-03-27 03:30:00").is_err());
    }

    #[test]
    fn a_shift_is_read_as_it_is_typed_and_written_back_the_same() {
        let back = Shift::read("-640d").unwrap();
        assert_eq!(back, Shift::days(-640));
        assert_eq!(back.written(), "-640d");
        let forward = Shift::read("+1y 2d 03:00").unwrap();
        assert_eq!((forward.years, forward.days, forward.seconds), (1, 2, 3 * 3600));
        assert_eq!(forward.written(), "+1y 2d 03:00");
        assert_eq!(Shift::read(&forward.written()), Ok(forward));
        assert_eq!(Shift::read("00:07:30").unwrap().written(), "+00:07:30");
        assert_eq!(Shift::rounded(-(640 * 86_400 + 7 * 60 + 20)).written(), "-640d 00:07");
    }

    #[test]
    fn a_bad_shift_says_why() {
        for (text, why) in [
            ("", "is not a shift"),
            ("soon", "is not part of a shift"),
            ("+3w", "is not part of a shift"),
            ("+1d 2d", "given twice"),
            ("+0d", "nothing"),
            ("+10:75", "is not part of a shift"),
            ("+999y", "further than"),
        ] {
            let error = Shift::read(text).unwrap_err();
            assert!(error.contains(why), "{text:?}: {error}");
        }
    }

    #[test]
    fn a_shift_moves_the_calendar_and_keeps_what_it_does_not_name() {
        assert_eq!(
            Shift::days(-640).apply("2009-12-31 23:10:00").unwrap(),
            "2008-03-31 23:10:00"
        );
        assert_eq!(
            Shift::read("+1y 2d 03:00")
                .unwrap()
                .apply("2008-02-28 22:00:00")
                .unwrap(),
            "2009-03-03 01:00:00"
        );
        assert!(Shift::days(-640).over_a_year());
        assert!(!Shift::days(-30).over_a_year());
    }
}
