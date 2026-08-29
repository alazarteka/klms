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
    if value.len() >= 10
        && value.as_bytes().get(4) == Some(&b'-')
        && value.as_bytes().get(7) == Some(&b'-')
    {
        if value.contains('T') {
            return Some(value.to_owned());
        }
        return Some(format!("{}T00:00:00{SEOUL_OFFSET}", &value[..10]));
    }
    moodle_datetime(value)
}

pub fn seoul_today() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
        + 9 * 60 * 60;
    civil_from_days(seconds.div_euclid(86_400))
}

pub fn add_days(date: &str, days: i64) -> Option<String> {
    let (year, month, day) = parse_date(date)?;
    Some(civil_from_days(days_from_civil(year, month, day) + days))
}

fn parse_date(value: &str) -> Option<(i32, u32, u32)> {
    let mut parts = value.get(..10)?.split('-');
    Some((
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    ))
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
    use super::{add_days, moodle_datetime};

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
}
