//! The scalar functions on strings that have no ready-made kernel: they are computed a row at
//! a time with the functions the virtual machine also uses.

use std::cell::RefCell;
use std::sync::Arc;

use arrow::array::{
    Array, ArrayRef, AsArray, BooleanArray, Int64Array, StringArray, StringBuilder, new_null_array,
};
use arrow::datatypes::Int64Type;
use biggo_plan::{DataType, ScalarFn, text};
use regex::Regex;

use crate::convert::arrow_type;
use crate::expr::Col;
use crate::{Error, Result};

/// A string argument, read a row at a time. A single value holds for every row.
struct Strs<'a> {
    array: &'a StringArray,
    single: bool,
}

impl<'a> Strs<'a> {
    fn new(col: &'a Col) -> Self {
        Strs {
            array: col.values().as_string::<i32>(),
            single: col.is_scalar(),
        }
    }

    fn get(&self, row: usize) -> Option<&'a str> {
        let row = if self.single { 0 } else { row };
        self.array.is_valid(row).then(|| self.array.value(row))
    }
}

/// An int argument, read a row at a time.
struct Ints<'a> {
    array: &'a Int64Array,
    single: bool,
}

impl<'a> Ints<'a> {
    fn new(col: &'a Col) -> Self {
        Ints {
            array: col.values().as_primitive::<Int64Type>(),
            single: col.is_scalar(),
        }
    }

    fn get(&self, row: usize) -> Option<i64> {
        let row = if self.single { 0 } else { row };
        self.array.is_valid(row).then(|| self.array.value(row))
    }
}

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

/// Applies one of the string functions of this module. `rows` is the number of rows of the
/// batch, which is what the result has unless every argument is a single value.
pub fn call(func: ScalarFn, args: &[Col], dtype: DataType, rows: usize) -> Result<Col> {
    let single = args.iter().all(Col::is_scalar);
    let rows = if single { 1 } else { rows };
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
        ScalarFn::RegexMatch | ScalarFn::RegexExtract | ScalarFn::RegexReplace => {
            // The checker lets neither the pattern nor the group depend on a column.
            let Some(pattern) = Strs::new(&args[1]).get(0) else {
                let nulls = new_null_array(&arrow_type(dtype), rows);
                return Ok(if single {
                    Col::Scalar(nulls)
                } else {
                    Col::Array(nulls)
                });
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
    Ok(if single {
        Col::Scalar(array)
    } else {
        Col::Array(array)
    })
}
