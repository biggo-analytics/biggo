//! The scalar functions on strings that have no ready-made kernel: they are computed a row at
//! a time with the functions the virtual machine also uses.

use std::cell::RefCell;
use std::sync::Arc;

use arrow::array::{
    Array, ArrayRef, AsArray, BooleanArray, Int64Array, StringBuilder, new_null_array,
};
use arrow::datatypes::{DataType as ArrowType, Decimal128Type, Float64Type, Int64Type};
use biggo_plan::{DataType, ScalarFn, text};
use regex::Regex;

use crate::convert::arrow_type;
use crate::expr::{Col, to_strings};
use crate::rows::{Ints, Strs, result, shape};
use crate::{Error, Result};

thread_local! {
    /// The pattern compiled last on this thread. A query uses one pattern for batch after
    /// batch, and a program in a loop for value after value.
    static LAST_REGEX: RefCell<Option<(String, Regex)>> = const { RefCell::new(None) };
}

fn compiled(pattern: &str) -> Result<Regex> {
    LAST_REGEX.with_borrow_mut(|last| {
        if let Some((text, regex)) = last
            && text == pattern
        {
            return Ok(regex.clone());
        }
        let regex = text::regex(pattern).map_err(Error)?;
        *last = Some((pattern.to_string(), regex.clone()));
        Ok(regex)
    })
}

/// Collects one string per row, where `f` gives `None` for a null.
fn strings<S: AsRef<str>>(
    rows: usize,
    mut f: impl FnMut(usize) -> Result<Option<S>>,
) -> Result<ArrayRef> {
    let mut built = StringBuilder::new();
    for row in 0..rows {
        match f(row)? {
            Some(value) => built.append_value(value),
            None => built.append_null(),
        }
    }
    Ok(Arc::new(built.finish()))
}

/// The functions that write values of other types as text: numbers in a set form, and values
/// of any type one after the other.
fn written(func: ScalarFn, args: &[Col], rows: usize) -> Result<Col> {
    let (rows, single) = shape(args, rows);
    if func == ScalarFn::Concat {
        // Each value as `to_string` gives it; a null adds nothing.
        let texts: Vec<ArrayRef> = args
            .iter()
            .map(|arg| to_strings(arg.values()))
            .collect::<Result<_>>()?;
        let parts: Vec<(&arrow::array::StringArray, bool)> = texts
            .iter()
            .zip(args)
            .map(|(text, arg)| (text.as_string::<i32>(), arg.is_scalar()))
            .collect();
        let joined = strings(rows, |row| {
            let mut joined = String::new();
            for (part, single) in &parts {
                let row = if *single { 0 } else { row };
                if part.is_valid(row) {
                    joined.push_str(part.value(row));
                }
            }
            Ok(Some(joined))
        })?;
        return Ok(result(single, joined));
    }
    // The checker lets neither the count of decimals nor the separator depend on a column.
    let decimals = match args.get(1) {
        Some(decimals) => Ints::new(decimals).get(0),
        None => Some(0),
    };
    let separator = match args.get(2) {
        Some(separator) => Strs::new(separator).get(0),
        None => Some(","),
    };
    let (Some(decimals), Some(separator)) = (decimals, separator) else {
        return Ok(result(single, new_null_array(&ArrowType::Utf8, rows)));
    };
    if !(0..=text::MAX_DECIMALS).contains(&decimals) {
        return Err(Error(format!(
            "the number of decimals is from 0 to {}, not {decimals}",
            text::MAX_DECIMALS
        )));
    }
    let percent = func == ScalarFn::FormatPercent;
    let end = |mut text: String| {
        if percent {
            text.push('%');
        }
        text
    };
    let values = args[0].values();
    let at = |row: usize| if args[0].is_scalar() { 0 } else { row };
    // A share is written as so many in a hundred.
    let times = if percent { 100 } else { 1 };
    let formatted = match values.data_type() {
        ArrowType::Int64 => {
            let ints = values.as_primitive::<Int64Type>();
            strings(rows, |row| {
                let row = at(row);
                let value = ints
                    .is_valid(row)
                    .then(|| i128::from(ints.value(row)) * times);
                Ok(value.map(|value| end(text::format_exact(value, 0, decimals, separator))))
            })?
        }
        ArrowType::Decimal128(..) => {
            let exact = values.as_primitive::<Decimal128Type>();
            let scale = biggo_plan::scalar::DECIMAL_SCALE;
            strings(rows, |row| {
                let row = at(row);
                let value = exact
                    .is_valid(row)
                    .then(|| exact.value(row).saturating_mul(times));
                Ok(value.map(|value| end(text::format_exact(value, scale, decimals, separator))))
            })?
        }
        _ => {
            let floats = values.as_primitive::<Float64Type>();
            strings(rows, |row| {
                let row = at(row);
                let value = floats
                    .is_valid(row)
                    .then(|| floats.value(row) * times as f64);
                Ok(value.map(|value| end(text::format_float(value, decimals, separator))))
            })?
        }
    };
    Ok(result(single, formatted))
}

/// Applies one of the string functions of this module. `rows` is the number of rows of the
/// batch, which is what the result has unless every argument is a single value.
pub fn call(func: ScalarFn, args: &[Col], dtype: DataType, rows: usize) -> Result<Col> {
    // These take values that are not strings.
    if matches!(
        func,
        ScalarFn::FormatNumber | ScalarFn::FormatPercent | ScalarFn::Concat
    ) {
        return written(func, args, rows);
    }
    let (rows, single) = shape(args, rows);
    let text = Strs::new(&args[0]);
    let array: ArrayRef = match func {
        ScalarFn::Substring => {
            let start = Ints::new(&args[1]);
            let length = args.get(2).map(Ints::new);
            strings(rows, |row| {
                let length = match &length {
                    Some(length) => match length.get(row) {
                        Some(length) => Some(length),
                        None => return Ok(None),
                    },
                    None => None,
                };
                Ok(match (text.get(row), start.get(row)) {
                    (Some(text), Some(start)) => Some(text::substring(text, start, length)),
                    _ => None,
                })
            })?
        }
        ScalarFn::Replace => {
            let (from, to) = (Strs::new(&args[1]), Strs::new(&args[2]));
            strings(rows, |row| {
                Ok(match (text.get(row), from.get(row), to.get(row)) {
                    (Some(text), Some(from), Some(to)) => Some(text::replace(text, from, to)),
                    _ => None,
                })
            })?
        }
        ScalarFn::SplitPart => {
            let (separator, index) = (Strs::new(&args[1]), Ints::new(&args[2]));
            strings(rows, |row| {
                Ok(match (text.get(row), separator.get(row), index.get(row)) {
                    (Some(text), Some(separator), Some(index)) => {
                        text::split_part(text, separator, index)
                    }
                    _ => None,
                })
            })?
        }
        ScalarFn::PadLeft | ScalarFn::PadRight => {
            let width = Ints::new(&args[1]);
            let fill = args.get(2).map(Strs::new);
            let before = func == ScalarFn::PadLeft;
            strings(rows, |row| {
                let fill = match &fill {
                    Some(fill) => match fill.get(row) {
                        Some(fill) => fill,
                        None => return Ok(None),
                    },
                    None => " ",
                };
                match (text.get(row), width.get(row)) {
                    (Some(text), Some(width)) => {
                        let padded = text::pad(text, width, fill, before).map_err(Error)?;
                        Ok(Some(padded))
                    }
                    _ => Ok(None),
                }
            })?
        }
        ScalarFn::IndexOf => {
            let part = Strs::new(&args[1]);
            let found: Int64Array = (0..rows)
                .map(|row| text::index_of(text.get(row)?, part.get(row)?))
                .collect();
            Arc::new(found)
        }
        ScalarFn::Trim | ScalarFn::TrimLeft | ScalarFn::TrimRight => {
            let chars = args.get(1).map(Strs::new);
            let (start, end) = match func {
                ScalarFn::TrimLeft => (true, false),
                ScalarFn::TrimRight => (false, true),
                _ => (true, true),
            };
            strings(rows, |row| {
                let chars = match &chars {
                    Some(chars) => match chars.get(row) {
                        Some(chars) => Some(chars),
                        None => return Ok(None),
                    },
                    None => None,
                };
                Ok(text
                    .get(row)
                    .map(|text| text::trim(text, chars, start, end)))
            })?
        }
        ScalarFn::Left | ScalarFn::Right => {
            let count = Ints::new(&args[1]);
            let first = func == ScalarFn::Left;
            strings(rows, |row| {
                Ok(match (text.get(row), count.get(row)) {
                    (Some(text), Some(count)) => Some(text::edge(text, count, first)),
                    _ => None,
                })
            })?
        }
        ScalarFn::Repeat => {
            let count = Ints::new(&args[1]);
            strings(rows, |row| match (text.get(row), count.get(row)) {
                (Some(text), Some(count)) => text::repeat(text, count).map(Some).map_err(Error),
                _ => Ok(None),
            })?
        }
        ScalarFn::Reverse => strings(rows, |row| Ok(text.get(row).map(text::reverse)))?,
        ScalarFn::Title => strings(rows, |row| Ok(text.get(row).map(text::title)))?,
        ScalarFn::Sha256 => strings(rows, |row| Ok(text.get(row).map(text::sha256)))?,
        ScalarFn::Md5 => strings(rows, |row| Ok(text.get(row).map(text::md5)))?,
        ScalarFn::Like | ScalarFn::RegexCount => {
            // The checker does not let the pattern depend on a column.
            let Some(pattern) = Strs::new(&args[1]).get(0) else {
                return Ok(result(single, new_null_array(&arrow_type(dtype), rows)));
            };
            if func == ScalarFn::Like {
                let regex = text::like(pattern).map_err(Error)?;
                let fits: BooleanArray = (0..rows)
                    .map(|row| Some(regex.is_match(text.get(row)?)))
                    .collect();
                Arc::new(fits)
            } else {
                let regex = compiled(pattern)?;
                let counts: Int64Array = (0..rows)
                    .map(|row| Some(regex.find_iter(text.get(row)?).count() as i64))
                    .collect();
                Arc::new(counts)
            }
        }
        ScalarFn::RegexMatch | ScalarFn::RegexExtract | ScalarFn::RegexReplace => {
            // The checker lets neither the pattern nor the group depend on a column.
            let Some(pattern) = Strs::new(&args[1]).get(0) else {
                return Ok(result(single, new_null_array(&arrow_type(dtype), rows)));
            };
            let regex = compiled(pattern)?;
            match func {
                ScalarFn::RegexMatch => {
                    let found: BooleanArray = (0..rows)
                        .map(|row| Some(regex.is_match(text.get(row)?)))
                        .collect();
                    Arc::new(found)
                }
                ScalarFn::RegexExtract => {
                    let group = match args.get(2) {
                        Some(group) => Ints::new(group).get(0).unwrap_or(0),
                        None => 0,
                    };
                    let group = text::regex_group(&regex, group).map_err(Error)?;
                    strings(rows, |row| {
                        let text = text.get(row);
                        Ok(text.and_then(|text| text::regex_extract(&regex, text, group)))
                    })?
                }
                _ => {
                    let to = Strs::new(&args[2]);
                    strings(rows, |row| {
                        Ok(match (text.get(row), to.get(row)) {
                            (Some(text), Some(to)) => Some(regex.replace_all(text, to)),
                            _ => None,
                        })
                    })?
                }
            }
        }
        other => {
            return Err(Error(format!(
                "internal error: `{}` is not a string function",
                other.name()
            )));
        }
    };
    Ok(result(single, array))
}
