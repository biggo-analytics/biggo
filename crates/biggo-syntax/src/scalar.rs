//! The text forms of datetimes, durations, and decimals, and exact decimal arithmetic. They
//! are shared by everything that reads, computes, or prints such values, so that a literal, a
//! CSV field, and a printed result all agree.

use crate::ast::Date;

/// A decimal is stored as its value times 10 to this power, so it has six exact digits after
/// the point.
pub const DECIMAL_SCALE: u32 = 6;
const DECIMAL_ONE: i128 = 10i128.pow(DECIMAL_SCALE);

const SECOND: i64 = 1_000_000;
const DAY: i64 = 86_400 * SECOND;

fn digits(text: &str, count: usize) -> Option<i64> {
    (text.len() == count && text.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

/// Parses `YYYY-MM-DDTHH:MM`, optionally followed by `:SS` and a fraction of up to six
/// digits, into microseconds since 1970-01-01T00:00:00. A space may stand for the `T`.
pub fn parse_datetime(text: &str) -> Option<i64> {
    let (date, time) = text.split_once(['T', ' '])?;
    let date = Date::parse(date)?;
    let mut parts = time.splitn(3, ':');
    let hour = digits(parts.next()?, 2)?;
    let minute = digits(parts.next()?, 2)?;
    let (second, micros) = match parts.next() {
        None => (0, 0),
        Some(seconds) => {
            let (whole, fraction) = seconds.split_once('.').unwrap_or((seconds, "0"));
            if fraction.is_empty() || fraction.len() > 6 {
                return None;
            }
            let micros = digits(fraction, fraction.len())? * 10i64.pow(6 - fraction.len() as u32);
            (digits(whole, 2)?, micros)
        }
    };
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let seconds = hour * 3600 + minute * 60 + second;
    Some(i64::from(date.to_days()) * DAY + seconds * SECOND + micros)
}

fn clock(micros: u64) -> String {
    let seconds = micros / SECOND as u64;
    let (hour, minute, second) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    let mut text = format!("{hour:02}:{minute:02}:{second:02}");
    let fraction = micros % SECOND as u64;
    if fraction != 0 {
        text.push_str(format!(".{fraction:06}").trim_end_matches('0'));
    }
    text
}

/// Formats microseconds since 1970-01-01T00:00:00 as `YYYY-MM-DDTHH:MM:SS`, with a fraction
/// only when there is one.
pub fn format_datetime(micros: i64) -> String {
    let day = i32::try_from(micros.div_euclid(DAY))
        .ok()
        .and_then(Date::from_days);
    match day {
        Some(date) => format!("{date}T{}", clock(micros.rem_euclid(DAY) as u64)),
        None => format!("<datetime {micros}>"),
    }
}

/// Formats a duration in microseconds as `HH:MM:SS`, after `Nd ` when it is a day or more.
pub fn format_duration(micros: i64) -> String {
    let sign = if micros < 0 { "-" } else { "" };
    let total = micros.unsigned_abs();
    let (days, rest) = (total / DAY as u64, total % DAY as u64);
    match days {
        0 => format!("{sign}{}", clock(rest)),
        _ => format!("{sign}{days}d {}", clock(rest)),
    }
}

/// Parses a decimal number with at most six digits after the point.
pub fn parse_decimal(text: &str) -> Option<i128> {
    let text = text.trim();
    let (negative, unsigned) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let (whole, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    let all_digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
    if (whole.is_empty() && fraction.is_empty())
        || !all_digits(whole)
        || !all_digits(fraction)
        || fraction.len() > DECIMAL_SCALE as usize
    {
        return None;
    }
    let whole: i128 = if whole.is_empty() {
        0
    } else {
        whole.parse().ok()?
    };
    let fraction_digits = fraction.len() as u32;
    let fraction: i128 = if fraction.is_empty() {
        0
    } else {
        fraction.parse().ok()?
    };
    let scaled = whole
        .checked_mul(DECIMAL_ONE)?
        .checked_add(fraction * 10i128.pow(DECIMAL_SCALE - fraction_digits))?;
    Some(if negative { -scaled } else { scaled })
}

/// Formats a decimal without the zeros at its end: `19.9`, not `19.900000`.
pub fn format_decimal(value: i128) -> String {
    let sign = if value < 0 { "-" } else { "" };
    let (whole, fraction) = (
        value.unsigned_abs() / DECIMAL_ONE as u128,
        value.unsigned_abs() % DECIMAL_ONE as u128,
    );
    match fraction {
        0 => format!("{sign}{whole}"),
        _ => format!(
            "{sign}{whole}.{}",
            format!("{fraction:06}").trim_end_matches('0')
        ),
    }
}

/// Divides and rounds to the nearest whole number, halves away from zero.
fn div_round(numerator: i128, denominator: i128) -> Option<i128> {
    let quotient = numerator.checked_div(denominator)?;
    let remainder = numerator % denominator;
    let rounds_up = remainder.unsigned_abs() * 2 >= denominator.unsigned_abs();
    let away = if (numerator < 0) != (denominator < 0) {
        -1
    } else {
        1
    };
    quotient.checked_add(if rounds_up { away } else { 0 })
}

/// The product of two decimals, rounded to six places; `None` if it is out of range.
pub fn decimal_mul(a: i128, b: i128) -> Option<i128> {
    div_round(a.checked_mul(b)?, DECIMAL_ONE)
}

/// The quotient of two decimals, rounded to six places; `None` if it is out of range or `b`
/// is zero.
pub fn decimal_div(a: i128, b: i128) -> Option<i128> {
    div_round(a.checked_mul(DECIMAL_ONE)?, b)
}

/// Rounds a decimal to `places` digits after the point; negative places round whole digits.
pub fn decimal_round(value: i128, places: i64) -> Option<i128> {
    if places >= i64::from(DECIMAL_SCALE) {
        return Some(value);
    }
    let unit = 10i128.checked_pow(u32::try_from(i64::from(DECIMAL_SCALE) - places).ok()?)?;
    div_round(value, unit)?.checked_mul(unit)
}

pub fn decimal_from_int(value: i64) -> i128 {
    i128::from(value) * DECIMAL_ONE
}

/// `value` without its digits past `places` after the point, which `decimal_round` rounds at.
pub fn decimal_trunc(value: i128, places: i64) -> i128 {
    let drop = DECIMAL_SCALE as i64 - places;
    if drop <= 0 {
        return value;
    }
    match u32::try_from(drop)
        .ok()
        .and_then(|drop| 10i128.checked_pow(drop))
    {
        Some(unit) => value / unit * unit,
        // Nothing of the value is left when more digits go than it can hold.
        None => 0,
    }
}

/// The form of a float that stands for all that compare equal to it: zero without a sign,
/// and one "not a number" for the many there are.
pub fn float_key(value: f64) -> f64 {
    if value.is_nan() {
        f64::NAN
    } else {
        value + 0.0
    }
}

/// How two floats compare: as numbers do, with negative zero equal to zero, and with "not a
/// number" equal to itself and above every number. Comparing, sorting, grouping and joining
/// all go by this, so that they agree with each other and from one machine to the next.
pub fn float_cmp(a: f64, b: f64) -> std::cmp::Ordering {
    float_key(a).total_cmp(&float_key(b))
}

/// -1, 0 or 1 for a float, where the library gives 1 for zero. Not a number stays so.
pub fn float_sign(value: f64) -> f64 {
    if value == 0.0 { 0.0 } else { value.signum() }
}

/// A float without its digits past `digits` after the point.
pub fn float_trunc(value: f64, digits: i64) -> f64 {
    let scale = 10f64.powi(digits.clamp(-300, 300) as i32);
    (value * scale).trunc() / scale
}

pub fn decimal_to_float(value: i128) -> f64 {
    value as f64 / DECIMAL_ONE as f64
}

/// The decimal nearest to `value`; `None` if it is not finite or out of range.
pub fn decimal_from_float(value: f64) -> Option<i128> {
    let scaled = (value * DECIMAL_ONE as f64).round();
    (scaled.is_finite() && scaled.abs() < 1e38).then_some(scaled as i128)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datetimes_round_trip() {
        for text in [
            "1970-01-01T00:00:00",
            "2026-10-08T23:59:59",
            "1969-12-31T12:30:00.5",
            "2024-02-29T06:07:08.000123",
        ] {
            assert_eq!(format_datetime(parse_datetime(text).unwrap()), text);
        }
        assert_eq!(parse_datetime("1970-01-01T00:00"), Some(0));
        assert_eq!(parse_datetime("1970-01-02 00:00:01"), Some(DAY + SECOND));
        for bad in [
            "2026-01-01",
            "2026-01-01T24:00",
            "2026-01-01T10:60:00",
            "2026-13-01T10:00",
            "2026-01-01T1:00",
            "2026-01-01T10:00:00.",
        ] {
            assert_eq!(parse_datetime(bad), None, "{bad}");
        }
    }

    #[test]
    fn durations_format() {
        assert_eq!(format_duration(0), "00:00:00");
        assert_eq!(format_duration(90 * 60 * SECOND), "01:30:00");
        assert_eq!(
            format_duration(DAY + 3661 * SECOND + 250_000),
            "1d 01:01:01.25"
        );
        assert_eq!(format_duration(-2 * DAY), "-2d 00:00:00");
    }

    #[test]
    fn decimals_parse_and_format() {
        assert_eq!(parse_decimal("19.99"), Some(19_990_000));
        assert_eq!(parse_decimal("-0.000001"), Some(-1));
        assert_eq!(parse_decimal(".5"), Some(500_000));
        assert_eq!(parse_decimal("7"), Some(7_000_000));
        for bad in ["", ".", "1.2345678", "1e3", "abc", "1.2.3"] {
            assert_eq!(parse_decimal(bad), None, "{bad}");
        }
        for text in ["0", "19.9", "-0.000001", "123456789012345678901234567890.5"] {
            assert_eq!(format_decimal(parse_decimal(text).unwrap()), text);
        }
    }

    #[test]
    fn decimal_arithmetic_is_exact_and_rounds_half_away() {
        let d = |text: &str| parse_decimal(text).unwrap();
        assert_eq!(d("0.1") + d("0.2"), d("0.3"));
        assert_eq!(decimal_mul(d("1.5"), d("2.5")), Some(d("3.75")));
        assert_eq!(decimal_mul(d("0.000001"), d("0.5")), Some(d("0.000001")));
        assert_eq!(decimal_mul(d("-0.000001"), d("0.5")), Some(d("-0.000001")));
        assert_eq!(decimal_div(d("1"), d("3")), Some(d("0.333333")));
        assert_eq!(decimal_div(d("2"), d("3")), Some(d("0.666667")));
        assert_eq!(decimal_div(d("-2"), d("3")), Some(d("-0.666667")));
        assert_eq!(decimal_div(d("1"), 0), None);
        assert_eq!(decimal_round(d("2.345"), 2), Some(d("2.35")));
        assert_eq!(decimal_round(d("-2.345"), 2), Some(d("-2.35")));
        assert_eq!(decimal_round(d("1250"), -2), Some(d("1300")));
        assert_eq!(decimal_from_float(0.1), Some(d("0.1")));
        assert_eq!(decimal_to_float(d("2.5")), 2.5);
        assert_eq!(decimal_mul(i128::MAX / 2, d("3")), None);
    }
}
