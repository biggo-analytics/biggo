//! Calendar functions on single values. A date is a number of days since 1970-01-01, and a
//! datetime a number of microseconds since the start of that day. The engine and the virtual
//! machine both compute with these, so a function gives the same answer on a column as on a
//! value.

use std::fmt::Write;

use biggo_syntax::ast::Date;
use chrono::format::{Fixed, Item, Numeric, Pad, Parsed, StrftimeItems};
use chrono::{Datelike, Months, NaiveDate, NaiveDateTime, NaiveTime, Timelike};

/// Microseconds in a day.
pub const DAY: i64 = 86_400_000_000;
const SECOND: i64 = 1_000_000;

/// What the year 1 of the common era is in the Buddhist era, which Thai dates are written in.
const BUDDHIST: i64 = 543;

/// The message for a result that no date can hold.
pub const OUT_OF_RANGE: &str = "the date is out of range";

/// The names of the months, January first.
const MONTHS: [&str; 12] = [
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
];

/// The names of the days of the week, Monday first.
const WEEKDAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

fn calendar(days: i32) -> Option<NaiveDate> {
    // The library counts days from the year 1; a date column holds the years 0 to 9999.
    Date::from_days(days)?;
    NaiveDate::from_num_days_from_ce_opt(days.checked_add(719_163)?)
}

fn day_number(date: NaiveDate) -> Option<i32> {
    let days = date.num_days_from_ce().checked_sub(719_163)?;
    Date::from_days(days).map(|_| days)
}

/// The day a moment falls on, and how far into that day it is.
pub fn split(micros: i64) -> (i64, i64) {
    (micros.div_euclid(DAY), micros.rem_euclid(DAY))
}

/// The day of a datetime, if it is one a date can hold.
pub fn day_of(micros: i64) -> Option<i32> {
    i32::try_from(split(micros).0).ok()
}

/// 1 for Monday to 7 for Sunday.
pub fn weekday(days: i32) -> i64 {
    // 1970-01-01 was a Thursday.
    (i64::from(days) + 3).rem_euclid(7) + 1
}

/// The number of the ISO week: weeks start on Monday, and week 1 has the first Thursday.
pub fn week(days: i32) -> Option<i64> {
    Some(i64::from(calendar(days)?.iso_week().week()))
}

pub fn quarter(days: i32) -> Option<i64> {
    Some(i64::from((calendar(days)?.month() - 1) / 3 + 1))
}

pub fn day_of_year(days: i32) -> Option<i64> {
    Some(i64::from(calendar(days)?.ordinal()))
}

pub fn month_name(days: i32) -> Option<&'static str> {
    Some(MONTHS[calendar(days)?.month0() as usize])
}

pub fn day_name(days: i32) -> &'static str {
    WEEKDAYS[(weekday(days) - 1) as usize]
}

/// The year of the Buddhist era, which is 543 years ahead.
pub fn buddhist_year(days: i32) -> Option<i64> {
    Some(i64::from(calendar(days)?.year()) + BUDDHIST)
}

/// The fiscal year a date belongs to, named for the calendar year it ends in, when fiscal
/// years start on the first day of `first_month`.
pub fn fiscal_year(days: i32, first_month: i64) -> Result<i64, String> {
    if !(1..=12).contains(&first_month) {
        return Err(format!(
            "the first month of a fiscal year is 1 to 12, not {first_month}"
        ));
    }
    let date = calendar(days).ok_or(OUT_OF_RANGE)?;
    let later = first_month > 1 && i64::from(date.month()) >= first_month;
    Ok(i64::from(date.year()) + i64::from(later))
}

/// The Monday on or before a date.
pub fn start_of_week(days: i32) -> Option<i32> {
    let start = days.checked_sub((weekday(days) - 1) as i32)?;
    Date::from_days(start).map(|_| start)
}

pub fn start_of_month(days: i32) -> Option<i32> {
    day_number(calendar(days)?.with_day(1)?)
}

pub fn start_of_quarter(days: i32) -> Option<i32> {
    let date = calendar(days)?;
    let month = (date.month() - 1) / 3 * 3 + 1;
    day_number(NaiveDate::from_ymd_opt(date.year(), month, 1)?)
}

pub fn start_of_year(days: i32) -> Option<i32> {
    day_number(NaiveDate::from_ymd_opt(calendar(days)?.year(), 1, 1)?)
}

pub fn end_of_month(days: i32) -> Option<i32> {
    let first = calendar(days)?.with_day(1)?;
    day_number(first.checked_add_months(Months::new(1))?.pred_opt()?)
}

pub fn add_days(days: i32, count: i64) -> Option<i32> {
    let moved = i32::try_from(i64::from(days).checked_add(count)?).ok()?;
    Date::from_days(moved).map(|_| moved)
}

/// The date `count` months later, or earlier when negative. A day that the month does not
/// have becomes its last day.
pub fn add_months(days: i32, count: i64) -> Option<i32> {
    let date = calendar(days)?;
    let months = Months::new(u32::try_from(count.unsigned_abs()).ok()?);
    let moved = match count < 0 {
        true => date.checked_sub_months(months)?,
        false => date.checked_add_months(months)?,
    };
    day_number(moved)
}

/// Applies a function of dates to a datetime, keeping its time of day.
pub fn on_day(micros: i64, f: impl FnOnce(i32) -> Option<i32>) -> Option<i64> {
    let (day, time) = split(micros);
    let moved = f(i32::try_from(day).ok()?)?;
    i64::from(moved).checked_mul(DAY)?.checked_add(time)
}

/// The whole days from one moment to another, negative when the second is the earlier.
pub fn days_between(from: i64, to: i64) -> Option<i64> {
    Some(to.checked_sub(from)? / DAY)
}

/// The whole months from one moment to another: a month is complete when the same day of
/// the month and time of day come round again.
pub fn months_between(from: i64, to: i64) -> Option<i64> {
    if to < from {
        return months_between(to, from).map(|months| -months);
    }
    let (start_day, start_time) = split(from);
    let (end_day, end_time) = split(to);
    let start = calendar(i32::try_from(start_day).ok()?)?;
    let end = calendar(i32::try_from(end_day).ok()?)?;
    let months = i64::from(end.year() - start.year()) * 12 + i64::from(end.month())
        - i64::from(start.month());
    // The last month does not count until its day and time are reached. A start on a day
    // that the end month lacks is reached on that month's last day.
    let last_day = end_of_month(end_day as i32).and_then(calendar)?.day();
    let due = start.day().min(last_day);
    let reached = (end.day(), end_time) >= (due, start_time);
    Some(months - i64::from(!reached))
}

pub fn make_date(year: i64, month: i64, day: i64) -> Option<i32> {
    let date = NaiveDate::from_ymd_opt(
        i32::try_from(year).ok()?,
        u32::try_from(month).ok()?,
        u32::try_from(day).ok()?,
    )?;
    day_number(date)
}

pub fn make_datetime(parts: [i64; 6]) -> Option<i64> {
    let [year, month, day, hour, minute, second] = parts;
    let days = make_date(year, month, day)?;
    if !(0..24).contains(&hour) || !(0..60).contains(&minute) || !(0..60).contains(&second) {
        return None;
    }
    Some(i64::from(days) * DAY + (hour * 3600 + minute * 60 + second) * SECOND)
}

/// The start of the interval of `size` microseconds that a moment falls in, where intervals
/// are counted from 1970-01-01T00:00:00.
pub fn time_bucket(micros: i64, size: i64) -> Result<i64, String> {
    match size > 0 {
        true => Ok(micros - micros.rem_euclid(size)),
        false => Err("the size of a time bucket must be more than zero".into()),
    }
}

/// Seconds since 1970-01-01T00:00:00, and a moment that many seconds after it.
pub fn to_unix(micros: i64) -> i64 {
    micros.div_euclid(SECOND)
}

pub fn from_unix(seconds: f64) -> Option<i64> {
    let micros = (seconds * SECOND as f64).round();
    let fits = micros.is_finite() && micros.abs() < 2.5e17;
    let micros = fits.then_some(micros as i64)?;
    day_of(micros).and_then(Date::from_days).map(|_| micros)
}

fn moment(micros: i64) -> Option<NaiveDateTime> {
    let (day, time) = split(micros);
    let date = calendar(i32::try_from(day).ok()?)?;
    let seconds = u32::try_from(time / SECOND).ok()?;
    let nanos = u32::try_from(time % SECOND * 1000).ok()?;
    Some(date.and_time(NaiveTime::from_num_seconds_from_midnight_opt(
        seconds, nanos,
    )?))
}

/// Which calendar the years of a pattern are counted in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Era {
    Common,
    /// 543 years ahead of the common era, as Thai dates are written.
    Buddhist,
}

impl Era {
    pub fn named(name: &str) -> Result<Era, String> {
        match name {
            "common" => Ok(Era::Common),
            "buddhist" => Ok(Era::Buddhist),
            _ => Err(format!(
                "the era is \"common\" or \"buddhist\", not {name:?}"
            )),
        }
    }
}

/// A pattern for writing and reading dates, such as `%d/%m/%Y`.
#[derive(Clone, Debug)]
pub struct Pattern {
    items: Vec<Item<'static>>,
    era: Era,
    text: String,
}

impl Pattern {
    /// Compiles a pattern. The error says what is wrong with it.
    pub fn new(text: &str, era: Era) -> Result<Pattern, String> {
        let wrong = || format!("`{text}` is not a date pattern; one looks like `%d/%m/%Y`");
        let items = StrftimeItems::new(text)
            .parse_to_owned()
            .map_err(|_| wrong())?;
        let pattern = Pattern {
            items,
            era,
            text: text.to_string(),
        };
        // A part that a datetime cannot supply, such as a time zone, shows when one is
        // written, so one is written now.
        let sample = NaiveDate::from_ymd_opt(2000, 1, 1)
            .expect("a real date")
            .and_time(NaiveTime::MIN);
        match pattern.write(sample) {
            Some(_) => Ok(pattern),
            None => Err(format!(
                "`{text}` asks for a part that a date does not have, such as a time zone"
            )),
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    fn write(&self, moment: NaiveDateTime) -> Option<String> {
        let mut text = String::new();
        for item in &self.items {
            // Years of another era are written here; the library writes everything else.
            let year = i64::from(moment.year()) + BUDDHIST;
            let shifted = match (self.era, item) {
                (Era::Buddhist, Item::Numeric(Numeric::Year, pad)) => Some((year, 4, *pad)),
                (Era::Buddhist, Item::Numeric(Numeric::YearDiv100, pad)) => {
                    Some((year.div_euclid(100), 2, *pad))
                }
                (Era::Buddhist, Item::Numeric(Numeric::YearMod100, pad)) => {
                    Some((year.rem_euclid(100), 2, *pad))
                }
                _ => None,
            };
            let written = match shifted {
                Some((number, _, Pad::None)) => write!(text, "{number}"),
                Some((number, width, Pad::Space)) => write!(text, "{number:width$}"),
                Some((number, width, Pad::Zero)) => write!(text, "{number:0width$}"),
                None => write!(text, "{}", moment.format_with_items(std::iter::once(item))),
            };
            written.ok()?;
        }
        Some(text)
    }

    /// A datetime as text. `None` if the moment is outside the years a date can hold.
    pub fn format(&self, micros: i64) -> Option<String> {
        self.write(moment(micros)?)
    }

    fn parsed(&self, text: &str) -> Option<Parsed> {
        let mut parsed = Parsed::new();
        chrono::format::parse(&mut parsed, text, self.items.iter()).ok()?;
        Some(parsed)
    }

    fn date_of(&self, parsed: &Parsed) -> Option<NaiveDate> {
        match self.era {
            Era::Common => parsed.to_naive_date().ok(),
            // The year is another calendar's, so the date is put together here.
            Era::Buddhist => {
                let year = i64::from(parsed.year()?) - BUDDHIST;
                NaiveDate::from_ymd_opt(i32::try_from(year).ok()?, parsed.month()?, parsed.day()?)
            }
        }
    }

    /// The date that `text` spells in this pattern, as days since 1970-01-01.
    pub fn parse_date(&self, text: &str) -> Option<i32> {
        day_number(self.date_of(&self.parsed(text)?)?)
    }

    /// The moment that `text` spells in this pattern. A pattern with no time of day gives
    /// the start of the day.
    pub fn parse_datetime(&self, text: &str) -> Option<i64> {
        let parsed = self.parsed(text)?;
        let days = day_number(self.date_of(&parsed)?)?;
        let timed = self.items.iter().any(|item| {
            matches!(
                item,
                Item::Numeric(
                    Numeric::Hour
                        | Numeric::Hour12
                        | Numeric::Minute
                        | Numeric::Second
                        | Numeric::Nanosecond
                        | Numeric::Timestamp,
                    _
                ) | Item::Fixed(Fixed::Nanosecond | Fixed::LowerAmPm | Fixed::UpperAmPm)
            )
        });
        let time = match timed {
            true => parsed.to_naive_time().ok()?,
            false => NaiveTime::MIN,
        };
        let micros = i64::from(time.num_seconds_from_midnight()) * SECOND
            + i64::from(time.nanosecond() / 1000);
        Some(i64::from(days) * DAY + micros)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(text: &str) -> i32 {
        Date::parse(text).unwrap().to_days()
    }

    fn at(text: &str) -> i64 {
        biggo_syntax::scalar::parse_datetime(text).unwrap()
    }

    #[test]
    fn weeks_start_on_monday() {
        assert_eq!(weekday(day("2026-10-09")), 5);
        assert_eq!(weekday(day("2026-10-11")), 7);
        assert_eq!(weekday(day("1969-12-29")), 1);
        assert_eq!(day_name(day("2026-10-12")), "Monday");
        assert_eq!(start_of_week(day("2026-10-09")), Some(day("2026-10-05")));
        assert_eq!(start_of_week(day("2026-10-05")), Some(day("2026-10-05")));
        // The first days of 2027 belong to the last week of 2026.
        assert_eq!(week(day("2027-01-01")), Some(53));
        assert_eq!(week(day("2027-01-04")), Some(1));
    }

    #[test]
    fn periods_have_first_and_last_days() {
        let d = day("2024-02-15");
        assert_eq!(start_of_month(d), Some(day("2024-02-01")));
        assert_eq!(end_of_month(d), Some(day("2024-02-29")));
        assert_eq!(end_of_month(day("2023-02-01")), Some(day("2023-02-28")));
        assert_eq!(start_of_quarter(day("2026-08-20")), Some(day("2026-07-01")));
        assert_eq!(start_of_year(d), Some(day("2024-01-01")));
        assert_eq!(quarter(day("2026-12-31")), Some(4));
        assert_eq!(day_of_year(day("2024-12-31")), Some(366));
        assert_eq!(month_name(d), Some("February"));
        assert_eq!(end_of_month(day("9999-12-05")), Some(day("9999-12-31")));
    }

    #[test]
    fn months_keep_to_the_end_of_the_month() {
        assert_eq!(add_months(day("2026-01-31"), 1), Some(day("2026-02-28")));
        assert_eq!(add_months(day("2024-01-31"), 1), Some(day("2024-02-29")));
        assert_eq!(add_months(day("2026-03-31"), -1), Some(day("2026-02-28")));
        assert_eq!(add_months(day("2026-05-15"), 12), Some(day("2027-05-15")));
        assert_eq!(add_months(day("2024-02-29"), 12), Some(day("2025-02-28")));
        assert_eq!(add_months(day("9999-12-01"), 1), None);
        assert_eq!(add_months(day("2026-01-01"), i64::MAX), None);
        assert_eq!(add_days(day("2026-12-31"), 1), Some(day("2027-01-01")));
        assert_eq!(add_days(day("9999-12-31"), 1), None);
        assert_eq!(
            on_day(at("2026-01-31T10:30:00"), |d| add_months(d, 1)),
            Some(at("2026-02-28T10:30:00"))
        );
    }

    #[test]
    fn whole_units_between_moments() {
        let between = |from: &str, to: &str| months_between(at(from), at(to)).unwrap();
        assert_eq!(between("2026-01-15T00:00", "2026-02-15T00:00"), 1);
        assert_eq!(between("2026-01-15T00:00", "2026-02-14T00:00"), 0);
        assert_eq!(between("2026-01-15T12:00", "2026-02-15T11:59"), 0);
        assert_eq!(between("2026-01-31T00:00", "2026-02-28T00:00"), 1);
        assert_eq!(between("2026-02-15T00:00", "2026-01-15T00:00"), -1);
        assert_eq!(between("2026-02-14T00:00", "2026-01-15T00:00"), 0);
        // Born on a leap day: a year older on the last day of February.
        assert_eq!(between("2000-02-29T00:00", "2026-02-28T00:00") / 12, 26);
        assert_eq!(between("2000-02-29T00:00", "2026-02-27T00:00") / 12, 25);
        assert_eq!(
            days_between(at("2026-01-01T12:00"), at("2026-01-03T11:00")),
            Some(1)
        );
        assert_eq!(
            days_between(at("2026-01-03T00:00"), at("2026-01-01T00:00")),
            Some(-2)
        );
    }

    #[test]
    fn dates_are_made_from_parts() {
        assert_eq!(make_date(2024, 2, 29), Some(day("2024-02-29")));
        assert_eq!(make_date(2026, 2, 29), None);
        assert_eq!(make_date(2026, 13, 1), None);
        assert_eq!(make_date(-1, 1, 1), None);
        assert_eq!(make_date(10_000, 1, 1), None);
        assert_eq!(
            make_datetime([2026, 1, 2, 3, 4, 5]),
            Some(at("2026-01-02T03:04:05"))
        );
        assert_eq!(make_datetime([2026, 1, 2, 24, 0, 0]), None);
        assert_eq!(fiscal_year(day("2025-10-01"), 10), Ok(2026));
        assert_eq!(fiscal_year(day("2025-09-30"), 10), Ok(2025));
        assert_eq!(fiscal_year(day("2025-09-30"), 1), Ok(2025));
        assert!(fiscal_year(day("2025-09-30"), 13).is_err());
        assert_eq!(buddhist_year(day("2026-10-09")), Some(2569));
    }

    #[test]
    fn moments_fall_in_buckets() {
        let quarter_hour = 15 * 60 * SECOND;
        assert_eq!(
            time_bucket(at("2026-01-02T10:44:59"), quarter_hour),
            Ok(at("2026-01-02T10:30:00"))
        );
        assert_eq!(
            time_bucket(at("1969-12-31T23:59:00"), quarter_hour),
            Ok(at("1969-12-31T23:45:00"))
        );
        assert!(time_bucket(0, 0).is_err());
        assert_eq!(to_unix(at("1970-01-01T00:01:00")), 60);
        assert_eq!(to_unix(-1), -1);
        assert_eq!(from_unix(60.5), Some(60_500_000));
        assert_eq!(from_unix(f64::NAN), None);
        assert_eq!(from_unix(1e18), None);
    }

    #[test]
    fn patterns_write_and_read_dates() {
        let slashes = Pattern::new("%d/%m/%Y", Era::Common).unwrap();
        assert_eq!(
            slashes.format(at("2026-01-05T00:00")).unwrap(),
            "05/01/2026"
        );
        assert_eq!(slashes.parse_date("05/01/2026"), Some(day("2026-01-05")));
        assert_eq!(slashes.parse_date("5/1/2026"), Some(day("2026-01-05")));
        assert_eq!(slashes.parse_date("31/02/2026"), None);
        assert_eq!(slashes.parse_date("2026-01-05"), None);
        assert_eq!(
            slashes.parse_datetime("05/01/2026"),
            Some(at("2026-01-05T00:00"))
        );
        let timed = Pattern::new("%Y%m%d %H:%M:%S", Era::Common).unwrap();
        assert_eq!(
            timed.parse_datetime("20260105 10:30:15"),
            Some(at("2026-01-05T10:30:15"))
        );
        assert_eq!(timed.parse_datetime("20260105"), None);
        assert_eq!(
            timed.format(at("2026-01-05T10:30:15")).unwrap(),
            "20260105 10:30:15"
        );
        let named = Pattern::new("%A %-d %B %Y", Era::Common).unwrap();
        assert_eq!(
            named.format(at("2026-10-09T00:00")).unwrap(),
            "Friday 9 October 2026"
        );
    }

    #[test]
    fn buddhist_years_are_543_ahead() {
        let thai = Pattern::new("%d/%m/%Y", Era::Buddhist).unwrap();
        assert_eq!(thai.format(at("2026-10-09T00:00")).unwrap(), "09/10/2569");
        assert_eq!(thai.parse_date("09/10/2569"), Some(day("2026-10-09")));
        // 2567 is not a leap year, but the year it stands for is.
        assert_eq!(thai.parse_date("29/02/2567"), Some(day("2024-02-29")));
        assert_eq!(thai.parse_date("29/02/2568"), None);
        let short = Pattern::new("%d/%m/%y", Era::Buddhist).unwrap();
        assert_eq!(short.format(at("2026-10-09T00:00")).unwrap(), "09/10/69");
        assert_eq!(short.parse_date("09/10/69"), None);
        assert_eq!(Era::named("buddhist"), Ok(Era::Buddhist));
        assert!(Era::named("be").is_err());
    }

    #[test]
    fn bad_patterns_are_refused() {
        assert!(Pattern::new("%Q", Era::Common).is_err());
        assert!(Pattern::new("%Y-%m-%d %Z", Era::Common).is_err());
        assert!(Pattern::new("plain text", Era::Common).is_ok());
    }
}
