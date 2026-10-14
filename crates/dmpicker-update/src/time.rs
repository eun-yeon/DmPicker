//! Retry-After 읽기

use crate::{Error, Result};

pub(crate) const HOUR: i64 = 60 * 60;
pub(crate) const DAY: i64 = 24 * HOUR;

fn number(text: &str) -> Result<i64> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::invalid("invalid date number"));
    }
    text.parse()
        .map_err(|_| Error::invalid("date number overflow"))
}

fn part(text: &str, range: std::ops::Range<usize>) -> Result<i64> {
    number(
        text.get(range)
            .ok_or_else(|| Error::invalid("truncated date"))?,
    )
}

fn leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_before_year(year: i64) -> Result<i64> {
    let y = year
        .checked_sub(1)
        .ok_or_else(|| Error::invalid("invalid year"))?;
    y.checked_mul(365)
        .and_then(|days| days.checked_add(y.div_euclid(4)))
        .and_then(|days| days.checked_sub(y.div_euclid(100)))
        .and_then(|days| days.checked_add(y.div_euclid(400)))
        .ok_or_else(|| Error::invalid("date overflow"))
}

fn month_days(year: i64, month: i64) -> Option<i64> {
    match month {
        2 => Some(if leap(year) { 29 } else { 28 }),
        4 | 6 | 9 | 11 => Some(30),
        1 | 3 | 5 | 7 | 8 | 10 | 12 => Some(31),
        _ => None,
    }
}

fn timestamp(year: i64, month: i64, day: i64, hour: i64, minute: i64, second: i64) -> Result<i64> {
    let days_in_month = month_days(year, month).ok_or_else(|| Error::invalid("invalid month"))?;
    if !(0..=9999).contains(&year)
        || !(1..=days_in_month).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=59).contains(&second)
    {
        return Err(Error::invalid("invalid date"));
    }
    let mut days = days_before_year(year)?
        .checked_sub(days_before_year(1970)?)
        .and_then(|days| days.checked_add(day - 1))
        .ok_or_else(|| Error::invalid("date overflow"))?;
    for m in 1..month {
        days = days
            .checked_add(month_days(year, m).ok_or_else(|| Error::invalid("invalid month"))?)
            .ok_or_else(|| Error::invalid("date overflow"))?;
    }
    days.checked_mul(DAY)
        .and_then(|t| t.checked_add(hour * HOUR))
        .and_then(|t| t.checked_add(minute * 60))
        .and_then(|t| t.checked_add(second))
        .ok_or_else(|| Error::invalid("date overflow"))
}

fn clock(text: &str) -> Result<(i64, i64, i64)> {
    if text.len() != 8 || text.get(2..3) != Some(":") || text.get(5..6) != Some(":") {
        return Err(Error::invalid("invalid clock"));
    }
    Ok((part(text, 0..2)?, part(text, 3..5)?, part(text, 6..8)?))
}

fn month(text: &str) -> Result<i64> {
    match text {
        "Jan" => Ok(1),
        "Feb" => Ok(2),
        "Mar" => Ok(3),
        "Apr" => Ok(4),
        "May" => Ok(5),
        "Jun" => Ok(6),
        "Jul" => Ok(7),
        "Aug" => Ok(8),
        "Sep" => Ok(9),
        "Oct" => Ok(10),
        "Nov" => Ok(11),
        "Dec" => Ok(12),
        _ => Err(Error::invalid("invalid HTTP month")),
    }
}

/// HTTP-date의 IMF-fixdate 형식을 UTC 초로 읽음
fn parse_http_date(text: &str) -> Result<i64> {
    if text.len() != 29
        || !matches!(
            text.get(..3),
            Some("Mon" | "Tue" | "Wed" | "Thu" | "Fri" | "Sat" | "Sun")
        )
        || text.get(3..5) != Some(", ")
        || text.get(7..8) != Some(" ")
        || text.get(11..12) != Some(" ")
        || text.get(16..17) != Some(" ")
        || text.get(25..29) != Some(" GMT")
    {
        return Err(Error::invalid("invalid HTTP date"));
    }
    let (hour, minute, second) = clock(
        text.get(17..25)
            .ok_or_else(|| Error::invalid("invalid HTTP date"))?,
    )?;
    timestamp(
        part(text, 12..16)?,
        month(
            text.get(8..11)
                .ok_or_else(|| Error::invalid("invalid HTTP date"))?,
        )?,
        part(text, 5..7)?,
        hour,
        minute,
        second,
    )
}

/// Retry-After를 기한으로 변환, 최대 24시간만 기다림
pub fn retry_after_deadline(value: &str, now: i64) -> Option<i64> {
    let value = value.trim();
    let seconds = if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) {
        value.bytes().fold(0i64, |n, b| {
            n.saturating_mul(10)
                .saturating_add(i64::from(b - b'0'))
                .min(DAY)
        })
    } else {
        parse_http_date(value)
            .ok()?
            .saturating_sub(now)
            .clamp(0, DAY)
    };
    Some(now.saturating_add(seconds))
}
