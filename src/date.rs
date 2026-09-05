use std::time::{SystemTime, UNIX_EPOCH};

pub const SEOUL_OFFSET: &str = "+09:00";

pub fn moodle_datetime(value: &str) -> Option<String> {
    let parts: Vec<_> = value.split(',').map(str::trim).collect();
    let (date, time) = match parts.as_slice() {
        [_, date, time] => (*date, *time),
        [date, time] => (*date, *time),
        _ => return None,
    };
    let date_parts: Vec<_> = date.split_whitespace().collect();
    let [day, month, year] = date_parts.as_slice() else {
        return None;
    };
    let day = day.parse::<u32>().ok()?;
    let year = year.parse::<i32>().ok()?;
    let month = month_number(month)?;
    let time_parts: Vec<_> = time.split_whitespace().collect();
    let [clock, period] = time_parts.as_slice() else {
        return None;
    };
    let (hour, minute) = clock.split_once(':')?;
    let mut hour = hour.parse::<u32>().ok()?;
    let minute = minute.parse::<u32>().ok()?;
    if hour == 0 || hour > 12 || minute > 59 || day == 0 || day > days_in_month(year, month) {
        return None;
    }
    hour %= 12;
    if period.eq_ignore_ascii_case("PM") {
        hour += 12;
    } else if !period.eq_ignore_ascii_case("AM") {
        return None;
    }
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:00{SEOUL_OFFSET}"
    ))
}

pub fn normalize_datetime(value: &str) -> Option<String> {
    let value = value.trim();
    if value.as_bytes().get(10) == Some(&b'T') {
        return iso_datetime_to_seoul(value);
    }
    if value.as_bytes().get(10) == Some(&b' ') && parse_date(&value[..10]).is_some() {
        return iso_datetime_to_seoul(&format!("{}T{}", &value[..10], &value[11..]));
    }
    if value.len() == 10 && value.as_bytes().get(4) == Some(&b'-') {
        let (year, month, day) = parse_date(value)?;
        return Some(format!(
            "{year:04}-{month:02}-{day:02}T00:00:00{SEOUL_OFFSET}"
        ));
    }
    moodle_datetime(value)
}

fn iso_datetime_to_seoul(value: &str) -> Option<String> {
    let (date, time_and_zone) = value.split_once('T')?;
    let (year, month, day) = parse_date(date)?;

    let (clock, offset_seconds) = if let Some(clock) = time_and_zone.strip_suffix('Z') {
        (clock, 0)
    } else if let Some(index) = time_and_zone
        .char_indices()
        .skip(1)
        .find_map(|(index, character)| matches!(character, '+' | '-').then_some(index))
    {
        let (clock, offset) = time_and_zone.split_at(index);
        let sign = if offset.starts_with('-') { -1 } else { 1 };
        let (hours, minutes) = offset.get(1..)?.split_once(':')?;
        if hours.len() != 2
            || minutes.len() != 2
            || !hours
                .bytes()
                .chain(minutes.bytes())
                .all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        let hours = hours.parse::<i64>().ok()?;
        let minutes = minutes.parse::<i64>().ok()?;
        if hours > 23 || minutes > 59 {
            return None;
        }
        (clock, sign * (hours * 3600 + minutes * 60))
    } else {
        (time_and_zone, 9 * 3600)
    };

    let mut parts = clock.split(':');
    let hour = parts.next()?.parse::<i64>().ok()?;
    let minute = parts.next()?.parse::<i64>().ok()?;
    let second = parts.next().unwrap_or("0");
    let second = if let Some((second, fraction)) = second.split_once('.') {
        if fraction.is_empty() || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        second
    } else {
        second
    }
    .parse::<i64>()
    .ok()?;
    if parts.next().is_some()
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=59).contains(&second)
    {
        return None;
    }
    let unix = days_from_civil(year, month, day)
        .checked_mul(86_400)?
        .checked_add(hour * 3600 + minute * 60 + second)?
        .checked_sub(offset_seconds)?;
    epoch_to_seoul(unix)
}

pub fn seoul_today() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
        + 9 * 60 * 60;
    civil_from_days(seconds.div_euclid(86_400))
}

pub fn epoch_to_seoul(timestamp: i64) -> Option<String> {
    let seconds = timestamp.checked_add(9 * 60 * 60)?;
    let days = seconds.div_euclid(86_400);
    let day_seconds = seconds.rem_euclid(86_400);
    let hour = day_seconds / 3600;
    let minute = day_seconds % 3600 / 60;
    let second = day_seconds % 60;
    Some(format!(
        "{}T{hour:02}:{minute:02}:{second:02}{SEOUL_OFFSET}",
        civil_from_days(days)
    ))
}

pub fn add_days(date: &str, days: i64) -> Option<String> {
    let (year, month, day) = parse_date(date)?;
    Some(civil_from_days(days_from_civil(year, month, day) + days))
}

fn parse_date(value: &str) -> Option<(i32, u32, u32)> {
    if value.len() != 10
        || value.as_bytes()[4] != b'-'
        || value.as_bytes()[7] != b'-'
        || !value
            .bytes()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
    {
        return None;
    }
    let mut parts = value.split('-');
    let (year, month, day) = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    (day > 0 && day <= days_in_month(year, month)).then_some((year, month, day))
}

fn month_number(value: &str) -> Option<u32> {
    [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ]
    .iter()
    .position(|month| month.eq_ignore_ascii_case(value))
    .map(|index| index as u32 + 1)
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
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    let year = year + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::{add_days, epoch_to_seoul, moodle_datetime, normalize_datetime};

    #[test]
    fn normalizes_moodle_deadlines_to_seoul_iso() {
        assert_eq!(
            moodle_datetime("Tuesday, 17 March 2026, 11:59 PM").as_deref(),
            Some("2026-03-17T23:59:00+09:00")
        );
        assert_eq!(
            moodle_datetime("1 January 2026, 12:05 AM").as_deref(),
            Some("2026-01-01T00:05:00+09:00")
        );
    }

    #[test]
    fn adds_days_across_month_and_year_boundaries() {
        assert_eq!(add_days("2026-12-31", 1).as_deref(), Some("2027-01-01"));
        assert_eq!(add_days("2028-02-28", 1).as_deref(), Some("2028-02-29"));
    }

    #[test]
    fn converts_unix_event_time_to_seoul() {
        assert_eq!(
            epoch_to_seoul(1_767_225_600).as_deref(),
            Some("2026-01-01T09:00:00+09:00")
        );
    }

    #[test]
    fn converts_iso_offsets_to_seoul_across_date_boundaries() {
        assert_eq!(
            normalize_datetime("2026-09-01T16:00:00Z").as_deref(),
            Some("2026-09-02T01:00:00+09:00")
        );
        assert_eq!(
            normalize_datetime("2026-09-01T23:30:00-05:00").as_deref(),
            Some("2026-09-02T13:30:00+09:00")
        );
        assert!(normalize_datetime("2026-02-30T12:00:00+09:00").is_none());
    }

    #[test]
    fn normalizes_supported_formats_without_discarding_time() {
        for (input, expected) in [
            (
                "Tuesday, 17 March 2026, 11:59 PM",
                "2026-03-17T23:59:00+09:00",
            ),
            ("1 January 2026, 12:05 AM", "2026-01-01T00:05:00+09:00"),
            ("2026-09-01", "2026-09-01T00:00:00+09:00"),
            ("2026-09-01 16:30:00", "2026-09-01T16:30:00+09:00"),
            ("2026-09-01 16:30:00Z", "2026-09-02T01:30:00+09:00"),
            ("2026-09-01T16:30", "2026-09-01T16:30:00+09:00"),
        ] {
            assert_eq!(
                normalize_datetime(input).as_deref(),
                Some(expected),
                "{input}"
            );
        }
        for input in [
            "2026-09-01 garbage",
            "2026-09-01extra",
            "2026-00-01",
            "2026-02-29",
            "2026-09-01extraT12:00",
            "2026-09-01T12:00:00.garbage",
            "2026-09-01T12:00:00+09:-1",
        ] {
            assert!(normalize_datetime(input).is_none(), "{input}");
        }
        assert!(add_days("2026-02-29", 1).is_none());
        assert!(add_days("2026-09-01T12:00", 1).is_none());
    }
}
