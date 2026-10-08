//! Scalar functions on dates, datetimes and durations, computed a row at a time with the
//! calendar functions that the virtual machine also uses.

use std::sync::Arc;

use arrow::array::{
    ArrayRef, BooleanArray, Date32Array, Float64Array, Int64Array, StringBuilder,
    TimestampMicrosecondArray,
};
use biggo_plan::dates::{self, Era, Pattern};
use biggo_plan::{DataType, ScalarFn};

use crate::expr::Col;
use crate::rows::{Ints, Moments, Numbers, Spans, Strs, result, shape};
use crate::{Error, Result};

fn out_of_range() -> Error {
    Error(dates::OUT_OF_RANGE.into())
}

/// One int per row, where `f` gives `None` for a null.
fn ints(rows: usize, f: impl FnMut(usize) -> Result<Option<i64>>) -> Result<ArrayRef> {
    let values: Int64Array = (0..rows).map(f).collect::<Result<_>>()?;
    Ok(Arc::new(values))
}

fn days(rows: usize, f: impl FnMut(usize) -> Result<Option<i32>>) -> Result<ArrayRef> {
    let values: Date32Array = (0..rows).map(f).collect::<Result<_>>()?;
    Ok(Arc::new(values))
}

fn moments(rows: usize, f: impl FnMut(usize) -> Result<Option<i64>>) -> Result<ArrayRef> {
    let values: TimestampMicrosecondArray = (0..rows).map(f).collect::<Result<_>>()?;
    Ok(Arc::new(values))
}

/// A result that a date holds, or the error that it is out of range.
fn held<T>(value: Option<T>) -> Result<Option<T>> {
    value.map(Some).ok_or_else(out_of_range)
}

/// The pattern of a call that writes or reads dates: its second argument, in the era that
/// its third names. `None` if either is null. The checker lets neither depend on a column.
fn pattern(args: &[Col]) -> Result<Option<Pattern>> {
    let era = match args.get(2) {
        Some(era) => match Strs::new(era).get(0) {
            Some(era) => Era::named(era).map_err(Error)?,
            None => return Ok(None),
        },
        None => Era::Common,
    };
    match Strs::new(&args[1]).get(0) {
        Some(text) => Pattern::new(text, era).map(Some).map_err(Error),
        None => Ok(None),
    }
}

/// Applies one of the functions of this module. `rows` is the number of rows of the batch.
pub fn call(func: ScalarFn, args: &[Col], dtype: DataType, rows: usize) -> Result<Col> {
    use ScalarFn::*;
    let (rows, single) = shape(args, rows);
    let first = &args[0];
    let array: ArrayRef = match func {
        Weekday | Week | Quarter | DayOfYear | BuddhistYear => {
            let when = Moments::new(first);
            ints(rows, |row| {
                let Some(day) = when.day(row) else {
                    return Ok(None);
                };
                held(match func {
                    Weekday => Some(dates::weekday(day)),
                    Week => dates::week(day),
                    Quarter => dates::quarter(day),
                    DayOfYear => dates::day_of_year(day),
                    _ => dates::buddhist_year(day),
                })
            })?
        }
        IsWeekend => {
            let when = Moments::new(first);
            let weekend: BooleanArray = (0..rows)
                .map(|row| Some(dates::weekday(when.day(row)?) >= 6))
                .collect();
            Arc::new(weekend)
        }
        MonthName | DayName => {
            let when = Moments::new(first);
            let mut names = StringBuilder::new();
            for row in 0..rows {
                let name = match when.day(row) {
                    Some(day) if func == DayName => Some(dates::day_name(day)),
                    Some(day) => Some(dates::month_name(day).ok_or_else(out_of_range)?),
                    None => None,
                };
                names.append_option(name);
            }
            Arc::new(names.finish())
        }
        StartOfWeek | StartOfMonth | StartOfQuarter | StartOfYear | EndOfMonth => {
            let when = Moments::new(first);
            days(rows, |row| {
                let Some(day) = when.day(row) else {
                    return Ok(None);
                };
                held(match func {
                    StartOfWeek => dates::start_of_week(day),
                    StartOfMonth => dates::start_of_month(day),
                    StartOfQuarter => dates::start_of_quarter(day),
                    StartOfYear => dates::start_of_year(day),
                    _ => dates::end_of_month(day),
                })
            })?
        }
        AddDays | AddMonths | AddYears => {
            let when = Moments::new(first);
            let count = Ints::new(&args[1]);
            let moved = |day: i32, count: i64| match func {
                AddDays => dates::add_days(day, count),
                AddMonths => dates::add_months(day, count),
                _ => count
                    .checked_mul(12)
                    .and_then(|months| dates::add_months(day, months)),
            };
            // A date stays a date, and a datetime keeps its time of day.
            match when.is_date() {
                true => days(rows, |row| match (when.day(row), count.get(row)) {
                    (Some(day), Some(count)) => held(moved(day, count)),
                    _ => Ok(None),
                })?,
                false => moments(rows, |row| match (when.micros(row), count.get(row)) {
                    (Some(micros), Some(count)) => {
                        held(dates::on_day(micros, |day| moved(day, count)))
                    }
                    _ => Ok(None),
                })?,
            }
        }
        DaysBetween | MonthsBetween | YearsBetween => {
            let (from, to) = (Moments::new(first), Moments::new(&args[1]));
            ints(rows, |row| {
                let (Some(from), Some(to)) = (from.micros(row), to.micros(row)) else {
                    return Ok(None);
                };
                held(match func {
                    DaysBetween => dates::days_between(from, to),
                    MonthsBetween => dates::months_between(from, to),
                    _ => dates::months_between(from, to).map(|months| months / 12),
                })
            })?
        }
        TotalDays | TotalHours | TotalMinutes => {
            let spans = Spans::new(first);
            let unit = match func {
                TotalDays => dates::DAY as f64,
                TotalHours => 3_600_000_000.0,
                _ => 60_000_000.0,
            };
            let lengths: Float64Array = (0..rows)
                .map(|row| Some(spans.get(row)? as f64 / unit))
                .collect();
            Arc::new(lengths)
        }
        MakeDate => {
            let parts: Vec<Ints> = args.iter().map(Ints::new).collect();
            days(rows, |row| {
                let (Some(year), Some(month), Some(day)) =
                    (parts[0].get(row), parts[1].get(row), parts[2].get(row))
                else {
                    return Ok(None);
                };
                let made = dates::make_date(year, month, day);
                made.map(Some)
                    .ok_or_else(|| Error(format!("there is no date {year}-{month:02}-{day:02}")))
            })?
        }
        MakeDateTime => {
            let parts: Vec<Ints> = args.iter().map(Ints::new).collect();
            moments(rows, |row| {
                let mut values = [0; 6];
                for (value, part) in values.iter_mut().zip(&parts) {
                    match part.get(row) {
                        Some(part) => *value = part,
                        None => return Ok(None),
                    }
                }
                dates::make_datetime(values).map(Some).ok_or_else(|| {
                    let [year, month, day, hour, minute, second] = values;
                    Error(format!(
                        "there is no datetime \
                         {year}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}"
                    ))
                })
            })?
        }
        FormatDate => {
            let when = Moments::new(first);
            let pattern = pattern(args)?;
            let mut texts = StringBuilder::new();
            for row in 0..rows {
                match (when.micros(row), &pattern) {
                    (Some(micros), Some(pattern)) => {
                        texts.append_value(pattern.format(micros).ok_or_else(out_of_range)?);
                    }
                    _ => texts.append_null(),
                }
            }
            Arc::new(texts.finish())
        }
        ParseDate | TryParseDate | ParseDateTime | TryParseDateTime => {
            let texts = Strs::new(first);
            let pattern = pattern(args)?;
            let strict = matches!(func, ParseDate | ParseDateTime);
            // The text that does not fit the pattern is an error, unless null was asked for.
            let refuse = |text: &str, pattern: &Pattern| {
                let what = if dtype == DataType::Date {
                    "date"
                } else {
                    "datetime"
                };
                Error(format!(
                    "cannot read '{text}' as a {what} in the pattern `{}`",
                    pattern.text()
                ))
            };
            match dtype {
                DataType::Date => days(rows, |row| {
                    let (Some(text), Some(pattern)) = (texts.get(row), &pattern) else {
                        return Ok(None);
                    };
                    match pattern.parse_date(text) {
                        None if strict => Err(refuse(text, pattern)),
                        read => Ok(read),
                    }
                })?,
                _ => moments(rows, |row| {
                    let (Some(text), Some(pattern)) = (texts.get(row), &pattern) else {
                        return Ok(None);
                    };
                    match pattern.parse_datetime(text) {
                        None if strict => Err(refuse(text, pattern)),
                        read => Ok(read),
                    }
                })?,
            }
        }
        TimeBucket => {
            let (when, size) = (Moments::new(first), Spans::new(&args[1]));
            moments(rows, |row| match (when.micros(row), size.get(row)) {
                (Some(micros), Some(size)) => {
                    dates::time_bucket(micros, size).map(Some).map_err(Error)
                }
                _ => Ok(None),
            })?
        }
        ToUnix => {
            let when = Moments::new(first);
            ints(rows, |row| Ok(when.micros(row).map(dates::to_unix)))?
        }
        FromUnix => {
            let seconds = Numbers::new(first);
            moments(rows, |row| match seconds.get(row) {
                Some(seconds) => held(dates::from_unix(seconds)),
                None => Ok(None),
            })?
        }
        FiscalYear => {
            let (when, first_month) = (Moments::new(first), Ints::new(&args[1]));
            ints(rows, |row| match (when.day(row), first_month.get(row)) {
                (Some(day), Some(month)) => dates::fiscal_year(day, month).map(Some).map_err(Error),
                _ => Ok(None),
            })?
        }
        other => {
            return Err(Error(format!(
                "internal error: `{}` is not a date function",
                other.name()
            )));
        }
    };
    Ok(result(single, array))
}
