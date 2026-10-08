//! Scalar functions on single values. The engine computes every scalar function a column at
//! a time, and a single value can go the same way as a column of one row; but setting a
//! column up for one value costs far more than the function itself. The common cases are
//! therefore computed here, in the same way the engine computes them, and the rest go to
//! the engine. A test holds the two to the same answers.

use biggo_plan::{Date, ScalarFn, scalar, text as strings};

use crate::value::{Decimal, Value};

const MICROS: [(ScalarFn, i64); 4] = [
    (ScalarFn::Days, 86_400_000_000),
    (ScalarFn::Hours, 3_600_000_000),
    (ScalarFn::Minutes, 60_000_000),
    (ScalarFn::Seconds, 1_000_000),
];

/// Applies `func` to `args` if it is a case computed here: `None` sends the call to the
/// engine. An error is the message to stop the program with.
pub fn call(func: ScalarFn, args: &[Value]) -> Option<Result<Value, String>> {
    use ScalarFn::*;
    if func == IsNull {
        return Some(Ok(Value::Bool(matches!(args[0], Value::Null))));
    }
    // `null_if` keeps its value when what it looks for is null; the engine works that out.
    if func == NullIf {
        return None;
    }
    // Every other function gives null for a null argument.
    if args.iter().any(|arg| matches!(arg, Value::Null)) {
        return Some(Ok(Value::Null));
    }
    let text = |text: String| Value::Str(text.into());
    Some(Ok(match (func, args) {
        (ToString, [Value::Str(value)]) => Value::Str(value.clone()),
        (ToString, [Value::Int(value)]) => text(value.to_string()),
        (ToString, [Value::Bool(value)]) => text(value.to_string()),
        (ToString, [Value::Date(days)]) => text(Date::from_days(*days)?.to_string()),
        (ToString, [Value::DateTime(micros)]) => text(scalar::format_datetime(*micros)),
        (ToString, [Value::Duration(micros)]) => text(scalar::format_duration(*micros)),
        (ToString, [Value::Decimal(value)]) => text(scalar::format_decimal(value.scaled())),

        (ToInt, [Value::Int(value)]) => Value::Int(*value),
        (ToInt, [Value::Bool(value)]) => Value::Int(i64::from(*value)),
        // Converting to an int drops the fraction.
        (ToInt, [Value::Float(value)]) => {
            let whole = value.trunc();
            if !(-9.223372036854776e18..9.223372036854776e18).contains(&whole) {
                return Some(Err(format!("{value} does not fit in an int")));
            }
            Value::Int(whole as i64)
        }
        (ToInt, [Value::Decimal(value)]) => {
            let whole = value.scaled() / 10i128.pow(scalar::DECIMAL_SCALE);
            match i64::try_from(whole) {
                Ok(whole) => Value::Int(whole),
                Err(_) => {
                    let shown = scalar::format_decimal(value.scaled());
                    return Some(Err(format!("{shown} does not fit in an int")));
                }
            }
        }
        (ToFloat, [Value::Int(value)]) => Value::Float(*value as f64),
        (ToFloat, [Value::Float(value)]) => Value::Float(*value),
        (ToFloat, [Value::Decimal(value)]) => {
            Value::Float(scalar::decimal_to_float(value.scaled()))
        }
        (ToDecimal, [Value::Int(value)]) => {
            Value::Decimal(Decimal::new(scalar::decimal_from_int(*value)))
        }
        (ToDecimal, [Value::Decimal(value)]) => Value::Decimal(*value),

        (Abs, [Value::Int(value)]) => match value.checked_abs() {
            Some(value) => Value::Int(value),
            None => return Some(Err("integer overflow".into())),
        },
        (Abs, [Value::Float(value)]) => Value::Float(value.abs()),
        (Floor, [Value::Float(value)]) => Value::Float(value.floor()),
        (Ceil, [Value::Float(value)]) => Value::Float(value.ceil()),
        (Sqrt, [Value::Float(value)]) => Value::Float(value.sqrt()),
        (Round, [Value::Float(value)]) => Value::Float(value.round()),
        (Round, [Value::Float(value), Value::Int(digits)]) => {
            let scale = 10f64.powi((*digits).clamp(-300, 300) as i32);
            Value::Float((value * scale).round() / scale)
        }

        (Length, [Value::Str(value)]) => Value::Int(value.chars().count() as i64),
        (Lower, [Value::Str(value)]) => text(value.to_lowercase()),
        (Upper, [Value::Str(value)]) => text(value.to_uppercase()),
        (Trim, [Value::Str(value)]) => text(value.trim().to_string()),
        (Contains, [Value::Str(value), Value::Str(part)]) => Value::Bool(value.contains(&**part)),
        (StartsWith, [Value::Str(value), Value::Str(part)]) => {
            Value::Bool(value.starts_with(&**part))
        }
        (EndsWith, [Value::Str(value), Value::Str(part)]) => Value::Bool(value.ends_with(&**part)),
        (Substring, [Value::Str(value), Value::Int(start)]) => {
            text(strings::substring(value, *start, None).to_string())
        }
        (Substring, [Value::Str(value), Value::Int(start), Value::Int(length)]) => {
            text(strings::substring(value, *start, Some(*length)).to_string())
        }
        (Replace, [Value::Str(value), Value::Str(from), Value::Str(to)]) => {
            text(strings::replace(value, from, to))
        }
        (SplitPart, [Value::Str(value), Value::Str(separator), Value::Int(index)]) => {
            match strings::split_part(value, separator, *index) {
                Some(piece) => text(piece.to_string()),
                None => Value::Null,
            }
        }
        (PadLeft | PadRight, [Value::Str(value), Value::Int(width), fill @ ..]) => {
            let fill = match fill {
                [] => " ",
                [Value::Str(fill)] => fill,
                _ => return None,
            };
            match strings::pad(value, *width, fill, func == PadLeft) {
                Ok(padded) => text(padded),
                Err(message) => return Some(Err(message)),
            }
        }
        (IndexOf, [Value::Str(value), Value::Str(part)]) => match strings::index_of(value, part) {
            Some(position) => Value::Int(position),
            None => Value::Null,
        },

        (Pow, [Value::Float(a), Value::Float(b)]) => Value::Float(a.powf(*b)),
        (Log, [Value::Float(a), Value::Float(b)]) => Value::Float(a.log(*b)),
        (Atan2, [Value::Float(a), Value::Float(b)]) => Value::Float(a.atan2(*b)),
        (Exp, [Value::Float(value)]) => Value::Float(value.exp()),
        (Ln, [Value::Float(value)]) => Value::Float(value.ln()),
        (Log10, [Value::Float(value)]) => Value::Float(value.log10()),
        (Log2, [Value::Float(value)]) => Value::Float(value.log2()),
        (Sin, [Value::Float(value)]) => Value::Float(value.sin()),
        (Cos, [Value::Float(value)]) => Value::Float(value.cos()),
        (Tan, [Value::Float(value)]) => Value::Float(value.tan()),
        (Asin, [Value::Float(value)]) => Value::Float(value.asin()),
        (Acos, [Value::Float(value)]) => Value::Float(value.acos()),
        (Atan, [Value::Float(value)]) => Value::Float(value.atan()),
        (Degrees, [Value::Float(value)]) => Value::Float(value.to_degrees()),
        (Radians, [Value::Float(value)]) => Value::Float(value.to_radians()),
        (IsNan, [Value::Float(value)]) => Value::Bool(value.is_nan()),
        (IsFinite, [Value::Float(value)]) => Value::Bool(value.is_finite()),
        (Sign, [Value::Int(value)]) => Value::Int(value.signum()),
        (Sign, [Value::Float(value)]) => Value::Float(scalar::float_sign(*value)),
        (Div, [Value::Int(_), Value::Int(0)]) => return Some(Err("division by zero".into())),
        (Div, [Value::Int(a), Value::Int(b)]) => match a.checked_div(*b) {
            Some(whole) => Value::Int(whole),
            None => return Some(Err("integer overflow".into())),
        },
        (Trunc, [Value::Float(value)]) => Value::Float(scalar::float_trunc(*value, 0)),
        (Trunc, [Value::Float(value), Value::Int(digits)]) => {
            Value::Float(scalar::float_trunc(*value, *digits))
        }
        (ParseNumber, [Value::Str(value)]) => match strings::parse_number(value) {
            Some(number) => Value::Float(number),
            None => Value::Null,
        },
        (ToBool | TryToBool, [Value::Bool(value)]) => Value::Bool(*value),
        (ToBool | TryToBool, [Value::Str(value)]) => match strings::to_bool(value) {
            Some(truth) => Value::Bool(truth),
            None if func == TryToBool => Value::Null,
            None => return Some(Err(format!("cannot convert '{value}' to a bool"))),
        },
        (ToBool | TryToBool, [Value::Int(value)]) => match value {
            0 => Value::Bool(false),
            1 => Value::Bool(true),
            _ if func == TryToBool => Value::Null,
            other => return Some(Err(format!("cannot convert {other} to a bool"))),
        },

        (Year, [Value::Date(days)]) => Value::Int(i64::from(Date::from_days(*days)?.year)),
        (Month, [Value::Date(days)]) => Value::Int(i64::from(Date::from_days(*days)?.month)),
        (Day, [Value::Date(days)]) => Value::Int(i64::from(Date::from_days(*days)?.day)),
        (Days | Hours | Minutes | Seconds, [Value::Int(count)]) => {
            let (_, unit) = MICROS.iter().find(|(unit, _)| *unit == func)?;
            Value::Duration(count.checked_mul(*unit)?)
        }
        (TotalSeconds, [Value::Duration(micros)]) => Value::Float(*micros as f64 / 1e6),
        _ => return None,
    }))
}
