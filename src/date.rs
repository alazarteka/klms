use std::time::{SystemTime, UNIX_EPOCH};

const SEOUL_OFFSET: &str = "+09:00";
const SEOUL_SECS: i64 = 9 * 3600;

fn seoul(date: impl std::fmt::Display, (hour, minute, second): (u32, u32, u32)) -> String {
    format!("{date}T{hour:02}:{minute:02}:{second:02}{SEOUL_OFFSET}")
}

fn digits(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

pub fn moodle_datetime(value: &str) -> Option<String> {
    if let Some(normalized) = korean_datetime(value) {
        return Some(normalized);
    }
    let parts: Vec<_> = value.split(',').map(str::trim).collect();
    let (date, time) = match parts.as_slice() {
        [_, date, time] | [date, time] => (*date, *time),
        _ => return None,
    };
    let [day, month, year] = date.split_whitespace().collect::<Vec<_>>()[..] else {
        return None;
    };
    let (day, year) = (day.parse::<u32>().ok()?, year.parse::<i32>().ok()?);
    let month =
        "January February March April May June July August September October November December"
            .split(' ')
            .position(|name| name.eq_ignore_ascii_case(month))? as u32
            + 1;
    let [clock, period] = time.split_whitespace().collect::<Vec<_>>()[..] else {
        return None;
    };
    let (hour, minute) = clock.split_once(':')?;
    let (hour, minute) = (hour.parse::<u32>().ok()?, minute.parse::<u32>().ok()?);
    if hour == 0 || hour > 12 || minute > 59 || day == 0 || day > days_in_month(year, month) {
        return None;
    }
    let pm = match period.to_ascii_uppercase().as_str() {
        "PM" => 12,
        "AM" => 0,
        _ => return None,
    };
    Some(seoul(
        format!("{year:04}-{month:02}-{day:02}"),
        (hour % 12 + pm, minute, 0),
    ))
}

fn korean_datetime(value: &str) -> Option<String> {
    let (year, rest) = value.trim().split_once('년')?;
    let (month, rest) = rest.split_once('월')?;
    let (day, mut rest) = rest.split_once('일')?;
    let year = year.trim().parse::<u32>().ok()?;
    let month = month.trim().parse::<u32>().ok()?;
    let day = day.trim().parse::<u32>().ok()?;
    if !(1..=9999).contains(&year) || day == 0 || day > days_in_month(year as i32, month) {
        return None;
    }
    rest = rest.trim();
    if let Some(weekday) = rest.strip_prefix('(') {
        rest = weekday.split_once(')')?.1.trim();
    }
    Some(seoul(
        format!("{year:04}-{month:02}-{day:02}"),
        localized_clock(rest)?,
    ))
}

/// Relative calendar labels are resolved against an explicit Seoul date so
/// tests and callers never accidentally use the machine's local timezone.
pub fn calendar_datetime(value: &str, today: &str) -> Option<String> {
    if let Some(normalized) = normalize_datetime(value) {
        return Some(normalized);
    }
    let (day, clock) = value.trim().split_once(',')?;
    let offset = match day.trim().to_ascii_lowercase().as_str() {
        "오늘" | "today" => 0,
        "내일" | "tomorrow" => 1,
        "어제" | "yesterday" => -1,
        _ => return None,
    };
    Some(seoul(add_days(today, offset)?, localized_clock(clock)?))
}

/// `[오전|오후] H:MM[:SS]` or `H:MM[:SS] [AM|PM]` or a 24-hour clock.
fn localized_clock(value: &str) -> Option<(u32, u32, u32)> {
    let value = value.trim();
    let (clock, pm) = if let Some(clock) = value.strip_prefix("오전") {
        (clock, Some(false))
    } else if let Some(clock) = value.strip_prefix("오후") {
        (clock, Some(true))
    } else if let Some((clock, suffix)) = value.rsplit_once(' ') {
        (
            clock,
            Some(match suffix.to_ascii_lowercase().as_str() {
                "am" => false,
                "pm" => true,
                _ => return None,
            }),
        )
    } else {
        (value, None)
    };
    let parts: Vec<_> = clock.trim().split(':').collect();
    if !(2..=3).contains(&parts.len()) || !parts.iter().all(|part| digits(part)) {
        return None;
    }
    let number = |index: usize| {
        parts
            .get(index)
            .map_or(Some(0), |part| part.parse::<u32>().ok())
    };
    let (mut hour, minute, second) = (number(0)?, number(1)?, number(2)?);
    if minute > 59 || second > 59 {
        return None;
    }
    match pm {
        Some(pm) if (1..=12).contains(&hour) => hour = hour % 12 + if pm { 12 } else { 0 },
        None if hour <= 23 => {}
        _ => return None,
    }
    Some((hour, minute, second))
}

pub fn normalize_datetime(value: &str) -> Option<String> {
    let value = value.trim();
    let date = value.get(..10).and_then(parse_date);
    match value.as_bytes().get(10) {
        Some(b'T') => iso_datetime_to_seoul(value),
        Some(b' ') if date.is_some() => {
            iso_datetime_to_seoul(&format!("{}T{}", &value[..10], &value[11..]))
        }
        None if value.len() == 10 && value.as_bytes().get(4) == Some(&b'-') => {
            let (year, month, day) = date?;
            Some(seoul(format!("{year:04}-{month:02}-{day:02}"), (0, 0, 0)))
        }
        _ => moodle_datetime(value),
    }
}

fn iso_datetime_to_seoul(value: &str) -> Option<String> {
    let (date, time_and_zone) = value.split_once('T')?;
    let (year, month, day) = parse_date(date)?;
    let zone_start = time_and_zone
        .char_indices()
        .skip(1)
        .find_map(|(index, character)| matches!(character, '+' | '-').then_some(index));
    let (clock, offset_seconds) = if let Some(clock) = time_and_zone.strip_suffix('Z') {
        (clock, 0)
    } else if let Some(index) = zone_start {
        let (clock, offset) = time_and_zone.split_at(index);
        let (hours, minutes) = offset[1..].split_once(':')?;
        let two = |part: &str| (part.len() == 2 && digits(part)).then(|| part.parse::<i64>().ok());
        let (hours, minutes) = (two(hours)??, two(minutes)??);
        if hours > 23 || minutes > 59 {
            return None;
        }
        let sign = if offset.starts_with('-') { -1 } else { 1 };
        (clock, sign * (hours * 3600 + minutes * 60))
    } else {
        (time_and_zone, SEOUL_SECS)
    };
    let clock = match clock.rsplit_once('.') {
        Some((head, fraction)) if head.matches(':').count() == 2 && digits(fraction) => head,
        Some(_) => return None,
        None => clock,
    };
    let (hour, minute, second) = hms(clock)?;
    let unix = days_from_civil(year, month, day)
        .checked_mul(86_400)?
        .checked_add(i64::from(hour * 3600 + minute * 60 + second))?
        .checked_sub(offset_seconds)?;
    epoch_to_seoul(unix)
}

/// A 24-hour `H:MM[:SS]` clock.
fn hms(clock: &str) -> Option<(u32, u32, u32)> {
    let plain = clock.starts_with(|c: char| c.is_ascii_digit()) && !clock.contains(' ');
    localized_clock(clock).filter(|_| plain)
}

pub fn epoch_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub fn seoul_today() -> String {
    civil_from_days((epoch_now() + SEOUL_SECS).div_euclid(86_400))
}

pub fn epoch_to_seoul(timestamp: i64) -> Option<String> {
    let seconds = timestamp.checked_add(SEOUL_SECS)?;
    let day_seconds = seconds.rem_euclid(86_400) as u32;
    Some(seoul(
        civil_from_days(seconds.div_euclid(86_400)),
        (
            day_seconds / 3600,
            day_seconds % 3600 / 60,
            day_seconds % 60,
        ),
    ))
}

pub fn add_days(date: &str, days: i64) -> Option<String> {
    let (year, month, day) = parse_date(date)?;
    Some(civil_from_days(days_from_civil(year, month, day) + days))
}

/// `YYYY-MM-DD` that names a real calendar day.
fn parse_date(value: &str) -> Option<(i32, u32, u32)> {
    let [year, month, day] = value.split('-').collect::<Vec<_>>()[..] else {
        return None;
    };
    if [year.len(), month.len(), day.len()] != [4, 2, 2]
        || ![year, month, day].iter().all(|p| digits(p))
    {
        return None;
    }
    let (year, month, day) = (year.parse().ok()?, month.parse().ok()?, day.parse().ok()?);
    (day > 0 && day <= days_in_month(year, month)).then_some((year, month, day))
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 400 == 0 || year % 4 == 0 && year % 100 != 0 => 29,
        2 => 28,
        _ => 0,
    }
}

fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let year = year - i32::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = month as i32;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day as i32 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    (era * 146_097 + day_of_era - 719_468) as i64
}

fn civil_from_days(days: i64) -> String {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::{add_days, calendar_datetime, epoch_to_seoul, moodle_datetime, normalize_datetime};

    #[test]
    fn normalizes_supported_formats_and_rejects_invalid_ones() {
        for (input, expected) in [
            (
                "2030년 3월 17일(일요일) 오후 11:50",
                "2030-03-17T23:50:00+09:00",
            ),
            ("2030년 3월 17일 오전 12:05", "2030-03-17T00:05:00+09:00"),
            ("2030년 3월 17일 오후 12:05", "2030-03-17T12:05:00+09:00"),
            ("2030년 3월 17일 23:50", "2030-03-17T23:50:00+09:00"),
            (
                "Tuesday, 17 March 2026, 11:59 PM",
                "2026-03-17T23:59:00+09:00",
            ),
            ("1 January 2026, 12:05 AM", "2026-01-01T00:05:00+09:00"),
            ("2026-09-01", "2026-09-01T00:00:00+09:00"),
            ("2026-09-01 16:30:00", "2026-09-01T16:30:00+09:00"),
            ("2026-09-01 16:30:00Z", "2026-09-02T01:30:00+09:00"),
            ("2026-09-01T16:30", "2026-09-01T16:30:00+09:00"),
            ("2026-09-01T16:00:00Z", "2026-09-02T01:00:00+09:00"),
            ("2026-09-01T23:30:00-05:00", "2026-09-02T13:30:00+09:00"),
        ] {
            assert_eq!(
                normalize_datetime(input).as_deref(),
                Some(expected),
                "{input}"
            );
        }
        for input in [
            "2030년 2월 29일 오후 11:50",
            "2030년 3월 17일 오후 0:05",
            "2030년 3월 17일 24:00",
            "2030년 3월 17일 23:60",
            "2030년 3월 17일 23:50 trailing",
            "2026-09-01 garbage",
            "2026-09-01extra",
            "2026-00-01",
            "2026-02-29",
            "2026-02-30T12:00:00+09:00",
            "2026-09-01extraT12:00",
            "2026-09-01T12:00:00.garbage",
            "2026-09-01T12:00:00+09:-1",
        ] {
            assert!(normalize_datetime(input).is_none(), "{input}");
        }
        for input in ["2030년 2월 29일 오후 11:50", "2030년 3월 17일 23:60"] {
            assert!(moodle_datetime(input).is_none(), "{input}");
        }
    }

    #[test]
    fn relative_calendar_dates_use_the_explicit_seoul_day() {
        for (label, today, expected) in [
            ("내일 , 23:50", "2030-12-31", "2031-01-01T23:50:00+09:00"),
            (
                "오늘, 오전 12:05",
                "2031-01-01",
                "2031-01-01T00:05:00+09:00",
            ),
            (
                "Yesterday, 11:50 PM",
                "2031-01-01",
                "2030-12-31T23:50:00+09:00",
            ),
            ("Tomorrow, 23:50", "2031-01-01", "2031-01-02T23:50:00+09:00"),
        ] {
            assert_eq!(calendar_datetime(label, today).as_deref(), Some(expected));
        }
        assert!(calendar_datetime("someday, 23:50", "2030-12-31").is_none());
    }

    #[test]
    fn day_arithmetic_and_epoch_conversion() {
        assert_eq!(add_days("2026-12-31", 1).as_deref(), Some("2027-01-01"));
        assert_eq!(add_days("2028-02-28", 1).as_deref(), Some("2028-02-29"));
        assert!(add_days("2026-02-29", 1).is_none());
        assert!(add_days("2026-09-01T12:00", 1).is_none());
        assert_eq!(
            epoch_to_seoul(1_767_225_600).as_deref(),
            Some("2026-01-01T09:00:00+09:00")
        );
    }
}
