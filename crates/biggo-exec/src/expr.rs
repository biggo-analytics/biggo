//! Evaluates column expressions on a batch, a whole column at a time.

use std::sync::Arc;

use arrow::array::{
    Array, ArrayRef, AsArray, BooleanArray, Datum, Decimal128Array, DurationMicrosecondArray,
    Float64Array, Int64Array, RecordBatch, StringArray, UInt32Array,
};
use arrow::buffer::BooleanBuffer;
use arrow::compute::kernels::concat_elements::concat_elements_utf8;
use arrow::compute::kernels::{boolean, cmp, comparison, numeric, temporal, zip};
use arrow::compute::{CastOptions, cast_with_options, filter_record_batch, take};
use arrow::datatypes::{
    DataType as ArrowType, Decimal128Type, DurationMicrosecondType, Float64Type, Int64Type,
    TimestampMicrosecondType,
};
use arrow::error::ArrowError;
use biggo_plan::{BinaryOp, DataType, Expr, ExprKind, ScalarFn, scalar};

use crate::convert::{arrow_type, decimals, scalar_array};
use crate::{Error, Result};

/// The value of an expression for a batch: one value per row, or a single value that holds
/// for every row.
#[derive(Clone)]
pub enum Col {
    Array(ArrayRef),
    /// An array of length one.
    Scalar(ArrayRef),
}

impl Datum for Col {
    fn get(&self) -> (&dyn Array, bool) {
        match self {
            Col::Array(array) => (array.as_ref(), false),
            Col::Scalar(array) => (array.as_ref(), true),
        }
    }
}

impl Col {
    pub(crate) fn values(&self) -> &ArrayRef {
        match self {
            Col::Array(array) | Col::Scalar(array) => array,
        }
    }

    pub(crate) fn is_scalar(&self) -> bool {
        matches!(self, Col::Scalar(_))
    }

    /// One value per row, repeating a single value `rows` times.
    pub fn into_array(self, rows: usize) -> Result<ArrayRef> {
        match self {
            Col::Array(array) => Ok(array),
            Col::Scalar(array) => {
                let indices = UInt32Array::from(vec![0u32; rows]);
                Ok(take(&array, &indices, None)?)
            }
        }
    }

    /// Applies an array kernel, keeping a single value single.
    pub(crate) fn map(self, f: impl FnOnce(&ArrayRef) -> Result<ArrayRef>) -> Result<Col> {
        Ok(match &self {
            Col::Array(array) => Col::Array(f(array)?),
            Col::Scalar(array) => Col::Scalar(f(array)?),
        })
    }
}

/// Evaluates `expr` for the rows of `batch`.
pub fn eval(expr: &Expr, batch: &RecordBatch) -> Result<Col> {
    match &expr.kind {
        ExprKind::Column(name) => match batch.column_by_name(name) {
            Some(column) => Ok(Col::Array(column.clone())),
            None => Err(Error(format!(
                "internal error: no column `{name}` in the batch"
            ))),
        },
        ExprKind::Literal(value) => Ok(Col::Scalar(scalar_array(value, expr.ty.dtype)?)),
        ExprKind::Param(index) => Err(Error(format!("internal error: unbound parameter {index}"))),
        ExprKind::Neg(operand) => eval(operand, batch)?.map(|array| Ok(numeric::neg(array)?)),
        ExprKind::Not(operand) => {
            eval(operand, batch)?.map(|array| Ok(Arc::new(boolean::not(array.as_boolean())?)))
        }
        ExprKind::Cast(operand) => {
            let to = arrow_type(expr.ty.dtype);
            eval(operand, batch)?.map(|array| cast(array, &to))
        }
        ExprKind::Binary(BinaryOp::And, left, right) => logical(batch, left, right, false),
        ExprKind::Binary(BinaryOp::Or, left, right) => logical(batch, left, right, true),
        ExprKind::Binary(BinaryOp::Coalesce, left, right) => coalesce(batch, left, right),
        ExprKind::Binary(BinaryOp::In, value, list) => match &list.kind {
            ExprKind::Literal(biggo_plan::Scalar::List(items)) => {
                membership(eval(value, batch)?, items, list.ty.dtype)
            }
            _ => Err(Error("internal error: `in` without its list".into())),
        },
        ExprKind::Binary(op, left, right) => {
            let left = eval(left, batch)?;
            let right = eval(right, batch)?;
            binary(*op, left, right, batch.num_rows())
        }
        ExprKind::If(cond, then, otherwise) => conditional(batch, cond, then, otherwise),
        ExprKind::Call(func, args) => {
            let mut values = Vec::with_capacity(args.len());
            for arg in args {
                values.push(eval(arg, batch)?);
            }
            call(*func, values, expr.ty.dtype, batch.num_rows())
        }
    }
}

/// Evaluates `expr` to one value per row.
pub fn eval_array(expr: &Expr, batch: &RecordBatch) -> Result<ArrayRef> {
    eval(expr, batch)?.into_array(batch.num_rows())
}

fn cast(array: &ArrayRef, to: &ArrowType) -> Result<ArrayRef> {
    let options = CastOptions {
        safe: false,
        ..CastOptions::default()
    };
    Ok(cast_with_options(array, to, &options)?)
}

/// Converts values as a program asks with `to_int` and its like, and says which value does
/// not convert in the words of the language.
fn convert(array: &ArrayRef, to: DataType) -> Result<ArrayRef> {
    cast(array, &arrow_type(to)).map_err(|err| match crate::scan::quoted_value(&err.0) {
        Some(value) => Error(format!("cannot convert '{value}' to {}", to.with_article())),
        None => err,
    })
}

/// Wraps the result of a kernel: single if both operands were.
fn result_of(left: &Col, right: &Col, array: ArrayRef) -> Col {
    if left.is_scalar() && right.is_scalar() {
        Col::Scalar(array)
    } else {
        Col::Array(array)
    }
}

fn binary(op: BinaryOp, left: Col, right: Col, rows: usize) -> Result<Col> {
    use BinaryOp::*;
    let strings = left.values().data_type() == &ArrowType::Utf8;
    let decimal = matches!(left.values().data_type(), ArrowType::Decimal128(..));
    if decimal && matches!(op, Mul | Div | Rem) {
        return decimal_binary(op, &left, &right, rows);
    }
    let array: ArrayRef = match op {
        Add if strings => {
            // The concatenation kernel needs two arrays of one length.
            let rows = if left.is_scalar() && right.is_scalar() {
                1
            } else {
                rows
            };
            let (l, r) = (
                left.clone().into_array(rows)?,
                right.clone().into_array(rows)?,
            );
            Arc::new(concat_elements_utf8(
                l.as_string::<i32>(),
                r.as_string::<i32>(),
            )?)
        }
        Add => numeric::add(&left, &right)?,
        Sub => numeric::sub(&left, &right)?,
        Mul => numeric::mul(&left, &right)?,
        Div => numeric::div(&left, &right)?,
        Rem => numeric::rem(&left, &right)?,
        Eq => Arc::new(cmp::eq(&left, &right)?),
        Ne => Arc::new(cmp::neq(&left, &right)?),
        Lt => Arc::new(cmp::lt(&left, &right)?),
        Le => Arc::new(cmp::lt_eq(&left, &right)?),
        Gt => Arc::new(cmp::gt(&left, &right)?),
        Ge => Arc::new(cmp::gt_eq(&left, &right)?),
        And | Or | Coalesce | In | NotIn => unreachable!("evaluated by the caller"),
    };
    // Arrow widens the type of a decimal sum; ours has one fixed type.
    let array = match decimal && matches!(op, Add | Sub) {
        true => cast(&array, &arrow_type(DataType::Decimal))?,
        false => array,
    };
    Ok(result_of(&left, &right, array))
}

/// Whether each value equals one of `items`, which have the type `dtype`. A value that
/// equals none of them gives null if a null is among them, as a chain of `==` joined by
/// `or` would.
fn membership(value: Col, items: &[biggo_plan::Scalar], dtype: DataType) -> Result<Col> {
    use biggo_plan::Scalar;
    /// Up to this many items are compared one after the other, a column at a time.
    const COMPARED: usize = 8;
    let unknown = items.iter().any(|item| matches!(item, Scalar::Null));
    // What a value that equals no item gives.
    let missed = if unknown { None } else { Some(false) };
    value.map(|array| {
        let hashed = matches!(array.data_type(), ArrowType::Int64 | ArrowType::Utf8);
        if items.len() > COMPARED && hashed {
            let found: BooleanArray = match array.data_type() {
                ArrowType::Int64 => {
                    let wanted: hashbrown::HashSet<i64> = items
                        .iter()
                        .filter_map(|item| match item {
                            Scalar::Int(item) => Some(*item),
                            _ => None,
                        })
                        .collect();
                    let values = array.as_primitive::<Int64Type>().iter();
                    values
                        .map(|value| {
                            if wanted.contains(&value?) {
                                Some(true)
                            } else {
                                missed
                            }
                        })
                        .collect()
                }
                _ => {
                    let wanted: hashbrown::HashSet<&str> = items
                        .iter()
                        .filter_map(|item| match item {
                            Scalar::Str(item) => Some(&**item),
                            _ => None,
                        })
                        .collect();
                    let values = array.as_string::<i32>().iter();
                    values
                        .map(|value| {
                            if wanted.contains(value?) {
                                Some(true)
                            } else {
                                missed
                            }
                        })
                        .collect()
                }
            };
            return Ok(Arc::new(found) as ArrayRef);
        }
        // Null for a null value, and otherwise false until an item is found equal.
        let nothing = BooleanBuffer::new_unset(array.len());
        let mut found = BooleanArray::new(nothing, array.logical_nulls());
        for item in items {
            let item = Col::Scalar(scalar_array(item, dtype)?);
            let equal = cmp::eq(&Col::Array(array.clone()), &item)?;
            found = boolean::or_kleene(&found, &equal)?;
        }
        Ok(Arc::new(found) as ArrayRef)
    })
}

fn decimal_overflow() -> ArrowError {
    ArrowError::ComputeError("decimal overflow".into())
}

/// Multiplies or divides decimals, rounding the result to the digits a decimal holds.
fn decimal_binary(op: BinaryOp, left: &Col, right: &Col, rows: usize) -> Result<Col> {
    let rows = if left.is_scalar() && right.is_scalar() {
        1
    } else {
        rows
    };
    let (l, r) = (
        left.clone().into_array(rows)?,
        right.clone().into_array(rows)?,
    );
    let (l, r) = (
        l.as_primitive::<Decimal128Type>(),
        r.as_primitive::<Decimal128Type>(),
    );
    let result: Decimal128Array = arrow::compute::try_binary(l, r, |a, b| match op {
        BinaryOp::Mul => scalar::decimal_mul(a, b).ok_or_else(decimal_overflow),
        _ if b == 0 => Err(ArrowError::DivideByZero),
        BinaryOp::Div => scalar::decimal_div(a, b).ok_or_else(decimal_overflow),
        _ => Ok(a % b),
    })?;
    Ok(result_of(left, right, decimals(result)?))
}

/// Evaluates `expr` only for the rows where `needed` is true; the other rows get null. This
/// is how an operand that can fail is kept away from rows that must not evaluate it.
fn eval_where(expr: &Expr, batch: &RecordBatch, needed: &BooleanArray) -> Result<ArrayRef> {
    let subset = filter_record_batch(batch, needed)?;
    let values = eval_array(expr, &subset)?;
    // Row i of the batch is row `position` of the subset if it is needed at all.
    let mut position = 0u32;
    let indices: UInt32Array = needed
        .iter()
        .map(|needed| {
            needed.unwrap_or(false).then(|| {
                position += 1;
                position - 1
            })
        })
        .collect();
    Ok(take(&values, &indices, None)?)
}

fn truth(scalar: &ArrayRef) -> Option<bool> {
    let scalar = scalar.as_boolean();
    scalar.is_valid(0).then(|| scalar.value(0))
}

/// Three-valued `and` (`decisive` false) or `or` (`decisive` true): an operand equal to
/// `decisive` settles the result whatever the other is, even null.
fn logical(batch: &RecordBatch, left: &Expr, right: &Expr, decisive: bool) -> Result<Col> {
    let rows = batch.num_rows();
    let left = eval(left, batch)?;
    if let Col::Scalar(value) = &left
        && truth(value) == Some(decisive)
    {
        return Ok(left);
    }
    let combine = |l: &BooleanArray, r: &BooleanArray| -> Result<ArrayRef> {
        Ok(Arc::new(match decisive {
            false => boolean::and_kleene(l, r)?,
            true => boolean::or_kleene(l, r)?,
        }))
    };
    // The program would not evaluate the right operand for rows the left one settles. That
    // matters only if evaluating it can fail.
    if right.can_fail() && !left.is_scalar() {
        let left = left.into_array(rows)?;
        let left = left.as_boolean();
        let needed: BooleanArray = left.iter().map(|l| Some(l != Some(decisive))).collect();
        let right = eval_where(right, batch, &needed)?;
        return Ok(Col::Array(combine(left, right.as_boolean())?));
    }
    let right = eval(right, batch)?;
    let rows = if left.is_scalar() && right.is_scalar() {
        1
    } else {
        rows
    };
    let (l, r) = (
        left.clone().into_array(rows)?,
        right.clone().into_array(rows)?,
    );
    Ok(result_of(
        &left,
        &right,
        combine(l.as_boolean(), r.as_boolean())?,
    ))
}

fn coalesce(batch: &RecordBatch, left: &Expr, right: &Expr) -> Result<Col> {
    let left = eval(left, batch)?;
    if let Col::Scalar(value) = &left {
        return if value.is_null(0) {
            eval(right, batch)
        } else {
            Ok(left)
        };
    }
    let values = left.values();
    if values.null_count() == 0 {
        return Ok(left);
    }
    let present = boolean::is_not_null(values)?;
    if right.can_fail() {
        let missing = boolean::is_null(values)?;
        let right = eval_where(right, batch, &missing)?;
        return Ok(Col::Array(zip::zip(&present, &left, &right)?));
    }
    let right = eval(right, batch)?;
    Ok(Col::Array(zip::zip(&present, &left, &right)?))
}

fn conditional(batch: &RecordBatch, cond: &Expr, then: &Expr, otherwise: &Expr) -> Result<Col> {
    let chosen = match eval(cond, batch)? {
        Col::Scalar(value) => {
            let branch = if truth(&value) == Some(true) {
                then
            } else {
                otherwise
            };
            return eval(branch, batch);
        }
        Col::Array(array) => array,
    };
    // A null condition takes the else branch, as a false one does.
    let chosen = chosen.as_boolean();
    let chosen = match chosen.null_count() {
        0 => chosen.clone(),
        _ => arrow::compute::prep_null_mask_filter(chosen),
    };
    if then.can_fail() || otherwise.can_fail() {
        let then = eval_where(then, batch, &chosen)?;
        let otherwise = eval_where(otherwise, batch, &boolean::not(&chosen)?)?;
        return Ok(Col::Array(zip::zip(&chosen, &then, &otherwise)?));
    }
    let then = eval(then, batch)?;
    let otherwise = eval(otherwise, batch)?;
    Ok(Col::Array(zip::zip(&chosen, &then, &otherwise)?))
}

fn floats(array: &ArrayRef, f: impl Fn(f64) -> f64) -> Result<ArrayRef> {
    let result: Float64Array = array.as_primitive::<Float64Type>().unary(f);
    Ok(Arc::new(result))
}

fn strings(array: &ArrayRef, f: impl Fn(&str) -> String) -> Result<ArrayRef> {
    let result: StringArray = array.as_string::<i32>().iter().map(|s| s.map(&f)).collect();
    Ok(Arc::new(result))
}

/// A duration of `count` units, each `unit` microseconds long.
fn durations(array: &ArrayRef, unit: i64) -> Result<ArrayRef> {
    let overflow = || ArrowError::ComputeError("duration overflow".into());
    let micros: DurationMicrosecondArray = match array.data_type() {
        ArrowType::Int64 => array
            .as_primitive::<Int64Type>()
            .try_unary(|count| count.checked_mul(unit).ok_or_else(overflow))?,
        _ => array.as_primitive::<Float64Type>().try_unary(|count| {
            let micros = (count * unit as f64).round();
            match micros.is_finite() && micros.abs() < 9.2e18 {
                true => Ok(micros as i64),
                false => Err(overflow()),
            }
        })?,
    };
    Ok(Arc::new(micros))
}

/// The text of each value, as `print` shows it.
pub fn to_strings(array: &ArrayRef) -> Result<ArrayRef> {
    let strings: StringArray = match array.data_type() {
        ArrowType::Decimal128(..) => {
            let values = array.as_primitive::<Decimal128Type>().iter();
            values.map(|v| v.map(scalar::format_decimal)).collect()
        }
        ArrowType::Timestamp(..) => {
            let values = array.as_primitive::<TimestampMicrosecondType>().iter();
            values.map(|v| v.map(scalar::format_datetime)).collect()
        }
        ArrowType::Duration(_) => {
            let values = array.as_primitive::<DurationMicrosecondType>().iter();
            values.map(|v| v.map(scalar::format_duration)).collect()
        }
        _ => return cast(array, &ArrowType::Utf8),
    };
    Ok(Arc::new(strings))
}

fn call(func: ScalarFn, mut args: Vec<Col>, dtype: DataType, rows: usize) -> Result<Col> {
    let first = args.remove(0);
    let part = |part| {
        first
            .clone()
            .map(|array| cast(&temporal::date_part(array, part)?, &ArrowType::Int64))
    };
    match func {
        ScalarFn::IsNull => first.map(|array| Ok(Arc::new(boolean::is_null(array)?))),
        ScalarFn::Abs => first.map(|array| match array.data_type() {
            ArrowType::Int64 => {
                let ints = array.as_primitive::<Int64Type>();
                let overflow = || ArrowError::ArithmeticOverflow(String::new());
                let result: Int64Array =
                    ints.try_unary(|v| v.checked_abs().ok_or_else(overflow))?;
                Ok(Arc::new(result) as ArrayRef)
            }
            ArrowType::Decimal128(..) => {
                let values = array.as_primitive::<Decimal128Type>();
                let result: Decimal128Array =
                    values.try_unary(|v| v.checked_abs().ok_or_else(decimal_overflow))?;
                decimals(result)
            }
            _ => floats(array, f64::abs),
        }),
        ScalarFn::Round => {
            let digits = match args.first() {
                Some(Col::Scalar(digits)) if digits.is_valid(0) => {
                    digits.as_primitive::<Int64Type>().value(0)
                }
                _ => 0,
            };
            first.map(|array| match array.data_type() {
                ArrowType::Decimal128(..) => {
                    let values = array.as_primitive::<Decimal128Type>();
                    let rounded: Decimal128Array = values.try_unary(|v| {
                        scalar::decimal_round(v, digits).ok_or_else(decimal_overflow)
                    })?;
                    decimals(rounded)
                }
                _ => {
                    let scale = 10f64.powi(digits.clamp(-300, 300) as i32);
                    floats(array, |v| (v * scale).round() / scale)
                }
            })
        }
        ScalarFn::Floor => first.map(|array| floats(array, f64::floor)),
        ScalarFn::Ceil => first.map(|array| floats(array, f64::ceil)),
        ScalarFn::Sqrt => first.map(|array| floats(array, f64::sqrt)),
        ScalarFn::Lower => first.map(|array| strings(array, str::to_lowercase)),
        ScalarFn::Upper => first.map(|array| strings(array, str::to_uppercase)),
        ScalarFn::Trim => first.map(|array| strings(array, |s| s.trim().to_string())),
        ScalarFn::Length => first.map(|array| {
            let lengths = array.as_string::<i32>().iter();
            let lengths: Int64Array = lengths
                .map(|s| s.map(|s| s.chars().count() as i64))
                .collect();
            Ok(Arc::new(lengths) as ArrayRef)
        }),
        ScalarFn::Contains | ScalarFn::StartsWith | ScalarFn::EndsWith => {
            let pattern = args.remove(0);
            let found = match func {
                ScalarFn::Contains => comparison::contains(&first, &pattern)?,
                ScalarFn::StartsWith => comparison::starts_with(&first, &pattern)?,
                _ => comparison::ends_with(&first, &pattern)?,
            };
            Ok(result_of(&first, &pattern, Arc::new(found)))
        }
        ScalarFn::Year => part(temporal::DatePart::Year),
        ScalarFn::Month => part(temporal::DatePart::Month),
        ScalarFn::Day => part(temporal::DatePart::Day),
        ScalarFn::Hour => part(temporal::DatePart::Hour),
        ScalarFn::Minute => part(temporal::DatePart::Minute),
        ScalarFn::Second => part(temporal::DatePart::Second),
        ScalarFn::Days => first.map(|array| durations(array, 86_400_000_000)),
        ScalarFn::Hours => first.map(|array| durations(array, 3_600_000_000)),
        ScalarFn::Minutes => first.map(|array| durations(array, 60_000_000)),
        ScalarFn::Seconds => first.map(|array| durations(array, 1_000_000)),
        ScalarFn::TotalSeconds => first.map(|array| {
            let micros = array.as_primitive::<DurationMicrosecondType>();
            let seconds: Float64Array = micros.unary(|micros| micros as f64 / 1e6);
            Ok(Arc::new(seconds) as ArrayRef)
        }),
        ScalarFn::ToString => first.map(to_strings),
        ScalarFn::ToInt => first.map(|array| match array.data_type() {
            // Converting to an int drops the fraction; Arrow would round instead.
            ArrowType::Float64 => {
                let floats = array.as_primitive::<Float64Type>();
                let result: Int64Array = floats.try_unary(|v| {
                    let whole = v.trunc();
                    if (-9.223372036854776e18..9.223372036854776e18).contains(&whole) {
                        Ok(whole as i64)
                    } else {
                        Err(ArrowError::ComputeError(format!(
                            "{v} does not fit in an int"
                        )))
                    }
                })?;
                Ok(Arc::new(result) as ArrayRef)
            }
            ArrowType::Decimal128(..) => {
                let values = array.as_primitive::<Decimal128Type>();
                let one = 10i128.pow(scalar::DECIMAL_SCALE);
                let result: Int64Array = values.try_unary(|v| {
                    i64::try_from(v / one).map_err(|_| {
                        let text = scalar::format_decimal(v);
                        ArrowError::ComputeError(format!("{text} does not fit in an int"))
                    })
                })?;
                Ok(Arc::new(result) as ArrayRef)
            }
            _ => convert(array, dtype),
        }),
        ScalarFn::ToFloat | ScalarFn::ToDecimal | ScalarFn::ToDate | ScalarFn::ToDateTime => {
            first.map(|array| convert(array, dtype))
        }
        ScalarFn::Substring
        | ScalarFn::Replace
        | ScalarFn::SplitPart
        | ScalarFn::PadLeft
        | ScalarFn::PadRight
        | ScalarFn::IndexOf
        | ScalarFn::RegexMatch
        | ScalarFn::RegexExtract
        | ScalarFn::RegexReplace => {
            args.insert(0, first);
            crate::text::call(func, &args, dtype, rows)
        }
        _ => {
            args.insert(0, first);
            crate::math::call(func, &args, dtype, rows)
        }
    }
}
