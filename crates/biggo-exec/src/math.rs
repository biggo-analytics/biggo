//! Scalar functions on numbers, the conversions that give null instead of failing, and the
//! functions that choose between their arguments.

use std::sync::Arc;

use arrow::array::{
    Array, ArrayRef, AsArray, BooleanArray, BooleanBuilder, Decimal128Array, Float64Array,
    Int64Array, new_null_array,
};
use arrow::compute::kernels::{boolean, cmp, zip};
use arrow::compute::{CastOptions, binary, cast_with_options, nullif, try_binary};
use arrow::datatypes::{DataType as ArrowType, Decimal128Type, Float64Type, Int64Type};
use arrow::error::ArrowError;
use biggo_plan::{DataType, ScalarFn, scalar, text};

use crate::convert::{arrow_type, decimals};
use crate::expr::{Col, canonical};
use crate::rows::{Bools, Ints, Strs, result, shape};
use crate::{Error, Result};

/// The floats that are whole numbers an int can hold.
const INT_RANGE: std::ops::Range<f64> = -9.223372036854776e18..9.223372036854776e18;

fn floats(col: &Col, f: impl Fn(f64) -> f64) -> Result<Col> {
    col.clone().map(|array| {
        let values: Float64Array = array.as_primitive::<Float64Type>().unary(f);
        Ok(Arc::new(values) as ArrayRef)
    })
}

/// The two arguments as arrays of one length, and whether both are single values.
fn pair(left: &Col, right: &Col, rows: usize) -> Result<(ArrayRef, ArrayRef, bool)> {
    let single = left.is_scalar() && right.is_scalar();
    let rows = if single { 1 } else { rows };
    let left = left.clone().into_array(rows)?;
    let right = right.clone().into_array(rows)?;
    Ok((left, right, single))
}

fn floats2(left: &Col, right: &Col, rows: usize, f: impl Fn(f64, f64) -> f64) -> Result<Col> {
    let (left, right, single) = pair(left, right, rows)?;
    let (left, right) = (
        left.as_primitive::<Float64Type>(),
        right.as_primitive::<Float64Type>(),
    );
    let values: Float64Array = binary(left, right, f)?;
    Ok(result(single, Arc::new(values)))
}

/// The larger or the smaller of two values. A null on either side gives null.
fn extreme(largest: bool, left: &Col, right: &Col, rows: usize) -> Result<Col> {
    let (left, right, single) = pair(left, right, rows)?;
    let (left, right) = (canonical(left), canonical(right));
    let keep_left = match largest {
        true => cmp::gt_eq(&left, &right)?,
        false => cmp::lt_eq(&left, &right)?,
    };
    let chosen = zip::zip(&keep_left, &left, &right)?;
    let both = boolean::and(
        &boolean::is_not_null(&left)?,
        &boolean::is_not_null(&right)?,
    )?;
    let nulls = new_null_array(left.data_type(), left.len());
    Ok(result(single, zip::zip(&both, &chosen, &nulls)?))
}

/// Converts with nulls for the values that do not convert.
fn try_convert(array: &ArrayRef, to: DataType) -> Result<ArrayRef> {
    let options = CastOptions {
        safe: true,
        ..CastOptions::default()
    };
    Ok(cast_with_options(array, &arrow_type(to), &options)?)
}

fn try_to_int(array: &ArrayRef) -> Result<ArrayRef> {
    Ok(match array.data_type() {
        // Converting to an int drops the fraction; Arrow would round instead.
        ArrowType::Float64 => {
            let floats = array.as_primitive::<Float64Type>();
            let ints: Int64Array = floats.unary_opt(|value| {
                let whole = value.trunc();
                INT_RANGE.contains(&whole).then_some(whole as i64)
            });
            Arc::new(ints)
        }
        ArrowType::Decimal128(..) => {
            let values = array.as_primitive::<Decimal128Type>();
            let one = 10i128.pow(scalar::DECIMAL_SCALE);
            let ints: Int64Array = values.unary_opt(|value| i64::try_from(value / one).ok());
            Arc::new(ints)
        }
        _ => try_convert(array, DataType::Int)?,
    })
}

/// A truth value from a string, an int, or a bool. With `strict`, a value that is none of
/// them is an error; without, it is null.
fn to_bool(col: &Col, strict: bool) -> Result<Col> {
    let single = col.is_scalar();
    let rows = col.values().len();
    let refuse = |shown: String| Error(format!("cannot convert {shown} to a bool"));
    let mut built = BooleanBuilder::with_capacity(rows);
    match col.values().data_type() {
        ArrowType::Boolean => {
            let values = Bools::new(col);
            (0..rows).for_each(|row| built.append_option(values.get(row)));
        }
        ArrowType::Int64 => {
            let values = Ints::new(col);
            for row in 0..rows {
                match values.get(row) {
                    Some(0) => built.append_value(false),
                    Some(1) => built.append_value(true),
                    Some(other) if strict => return Err(refuse(other.to_string())),
                    _ => built.append_null(),
                }
            }
        }
        _ => {
            let values = Strs::new(col);
            for row in 0..rows {
                match values.get(row).map(|value| (value, text::to_bool(value))) {
                    Some((_, Some(truth))) => built.append_value(truth),
                    Some((value, None)) if strict => return Err(refuse(format!("'{value}'"))),
                    _ => built.append_null(),
                }
            }
        }
    }
    Ok(result(single, Arc::new(built.finish())))
}

fn sign(array: &ArrayRef) -> Result<ArrayRef> {
    Ok(match array.data_type() {
        ArrowType::Int64 => {
            let signs: Int64Array = array.as_primitive::<Int64Type>().unary(i64::signum);
            Arc::new(signs)
        }
        ArrowType::Decimal128(..) => {
            let one = 10i128.pow(scalar::DECIMAL_SCALE);
            let values = array.as_primitive::<Decimal128Type>();
            let signs: Decimal128Array = values.unary(|value| value.signum() * one);
            decimals(signs)?
        }
        _ => {
            let values = array.as_primitive::<Float64Type>();
            let signs: Float64Array = values.unary(scalar::float_sign);
            Arc::new(signs)
        }
    })
}

/// A stable number for each value: see `scalar::hash_int`. Values that compare equal have
/// the same number, whatever their type.
fn hash(array: &ArrayRef, seed: i64) -> Result<ArrayRef> {
    let hashes: Int64Array = match array.data_type() {
        ArrowType::Utf8 => {
            let strings = array.as_string::<i32>().iter();
            strings
                .map(|text| Some(scalar::hash_bytes(text?.as_bytes(), seed)))
                .collect()
        }
        ArrowType::Float64 => {
            let floats = array.as_primitive::<Float64Type>();
            floats.unary(|value| scalar::hash_int(scalar::float_key(value).to_bits() as i64, seed))
        }
        ArrowType::Decimal128(..) => {
            let values = array.as_primitive::<Decimal128Type>();
            values.unary(|value| {
                let high = scalar::hash_int((value >> 64) as i64, seed);
                scalar::hash_int(value as i64, high)
            })
        }
        // Whole numbers, and the dates, times and truth values that are kept as such.
        _ => {
            let ints = arrow::compute::cast(array, &ArrowType::Int64)?;
            ints.as_primitive::<Int64Type>()
                .unary(|value| scalar::hash_int(value, seed))
        }
    };
    Ok(Arc::new(hashes))
}

/// Applies one of the functions of this module. `rows` is the number of rows of the batch.
pub fn call(func: ScalarFn, args: &[Col], dtype: DataType, rows: usize) -> Result<Col> {
    use ScalarFn::*;
    let first = &args[0];
    match func {
        Greatest | Least => {
            let mut best = first.clone();
            for other in &args[1..] {
                best = extreme(func == Greatest, &best, other, rows)?;
            }
            Ok(best)
        }
        NullIf => {
            let (value, marker, single) = pair(first, &args[1], rows)?;
            let same = cmp::eq(&canonical(value.clone()), &canonical(marker))?;
            Ok(result(single, nullif(&value, &same)?))
        }
        TryToInt => first.clone().map(try_to_int),
        TryToFloat | TryToDecimal | TryToDate | TryToDateTime => {
            first.clone().map(|array| try_convert(array, dtype))
        }
        ToBool => to_bool(first, true),
        TryToBool => to_bool(first, false),
        Pow => floats2(first, &args[1], rows, f64::powf),
        Log => floats2(first, &args[1], rows, f64::log),
        Atan2 => floats2(first, &args[1], rows, f64::atan2),
        Exp => floats(first, f64::exp),
        Ln => floats(first, f64::ln),
        Log10 => floats(first, f64::log10),
        Log2 => floats(first, f64::log2),
        Sin => floats(first, f64::sin),
        Cos => floats(first, f64::cos),
        Tan => floats(first, f64::tan),
        Asin => floats(first, f64::asin),
        Acos => floats(first, f64::acos),
        Atan => floats(first, f64::atan),
        Degrees => floats(first, f64::to_degrees),
        Radians => floats(first, f64::to_radians),
        IsNan | IsFinite => first.clone().map(|array| {
            let values = array.as_primitive::<Float64Type>();
            let answers = match func {
                IsNan => BooleanArray::from_unary(values, f64::is_nan),
                _ => BooleanArray::from_unary(values, f64::is_finite),
            };
            Ok(Arc::new(answers) as ArrayRef)
        }),
        Sign => first.clone().map(sign),
        Div => {
            let (left, right, single) = pair(first, &args[1], rows)?;
            let (left, right) = (
                left.as_primitive::<Int64Type>(),
                right.as_primitive::<Int64Type>(),
            );
            let whole: Int64Array = try_binary(left, right, |a, b| match b {
                0 => Err(ArrowError::DivideByZero),
                _ => a
                    .checked_div(b)
                    .ok_or_else(|| ArrowError::ArithmeticOverflow(String::new())),
            })?;
            Ok(result(single, Arc::new(whole)))
        }
        Trunc => {
            let digits = match args.get(1) {
                Some(digits) => Ints::new(digits).get(0).unwrap_or(0),
                None => 0,
            };
            first.clone().map(|array| match array.data_type() {
                ArrowType::Decimal128(..) => {
                    let values = array.as_primitive::<Decimal128Type>();
                    let cut: Decimal128Array =
                        values.unary(|value| scalar::decimal_trunc(value, digits));
                    decimals(cut)
                }
                _ => {
                    let values = array.as_primitive::<Float64Type>();
                    let cut: Float64Array =
                        values.unary(|value| scalar::float_trunc(value, digits));
                    Ok(Arc::new(cut) as ArrayRef)
                }
            })
        }
        Hash => {
            let seed = match args.get(1) {
                Some(seed) => Ints::new(seed).get(0).unwrap_or(0),
                None => 0,
            };
            first.clone().map(|array| hash(array, seed))
        }
        ParseNumber => {
            let (rows, single) = shape(args, rows);
            let texts = Strs::new(first);
            let numbers: Float64Array = (0..rows)
                .map(|row| text::parse_number(texts.get(row)?))
                .collect();
            Ok(result(single, Arc::new(numbers)))
        }
        other => Err(Error(format!(
            "internal error: `{}` is not computed here",
            other.name()
        ))),
    }
}
