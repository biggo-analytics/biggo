use std::sync::Arc;

use arrow::array::{
    Array, ArrayRef, AsArray, BooleanArray, BooleanBuilder, Date32Array, Decimal128Array,
    DurationMicrosecondArray, Float64Array, Int64Array, RecordBatch, RecordBatchOptions,
    StringArray, StringBuilder, TimestampMicrosecondArray, new_null_array,
};
use arrow::datatypes::{
    DataType as ArrowType, Date32Type, Decimal128Type, DurationMicrosecondType,
    Field as ArrowField, Float64Type, Int64Type, Schema as ArrowSchema, SchemaRef, TimeUnit,
    TimestampMicrosecondType,
};
use biggo_plan::scalar::{self, DECIMAL_SCALE};
use biggo_plan::{DataType, Scalar, Schema};

use crate::{Error, Result};

/// The most digits a decimal column holds.
pub const DECIMAL_PRECISION: u8 = 38;

pub fn arrow_type(dtype: DataType) -> ArrowType {
    match dtype {
        DataType::Int => ArrowType::Int64,
        DataType::Float => ArrowType::Float64,
        DataType::Bool => ArrowType::Boolean,
        DataType::Str => ArrowType::Utf8,
        DataType::Date => ArrowType::Date32,
        DataType::DateTime => ArrowType::Timestamp(TimeUnit::Microsecond, None),
        DataType::Duration => ArrowType::Duration(TimeUnit::Microsecond),
        DataType::Decimal => ArrowType::Decimal128(DECIMAL_PRECISION, DECIMAL_SCALE as i8),
    }
}

pub fn arrow_schema(schema: &Schema) -> SchemaRef {
    let fields = schema
        .fields
        .iter()
        .map(|field| ArrowField::new(&*field.name, arrow_type(field.ty.dtype), field.ty.nullable));
    Arc::new(ArrowSchema::new(fields.collect::<Vec<_>>()))
}

/// Builds a batch of `columns` with the names and types of `schema`.
pub fn batch(schema: &Schema, columns: Vec<ArrayRef>, rows: usize) -> Result<RecordBatch> {
    let options = RecordBatchOptions::new().with_row_count(Some(rows));
    // Nullability is checked where data enters; marking every field nullable here keeps
    // Arrow from rejecting a batch over what is only a static promise.
    let fields = schema
        .fields
        .iter()
        .map(|field| ArrowField::new(&*field.name, arrow_type(field.ty.dtype), true));
    let schema = Arc::new(ArrowSchema::new(fields.collect::<Vec<_>>()));
    Ok(RecordBatch::try_new_with_options(
        schema, columns, &options,
    )?)
}

/// Marks an array of scaled integers as a decimal column.
pub fn decimals(values: Decimal128Array) -> Result<ArrayRef> {
    let typed = values.with_precision_and_scale(DECIMAL_PRECISION, DECIMAL_SCALE as i8)?;
    Ok(Arc::new(typed))
}

/// An array of one element holding `value`.
pub fn scalar_array(value: &Scalar, dtype: DataType) -> Result<ArrayRef> {
    Ok(match value {
        Scalar::Null => new_null_array(&arrow_type(dtype), 1),
        Scalar::Bool(value) => Arc::new(BooleanArray::from(vec![*value])),
        Scalar::Int(value) => Arc::new(Int64Array::from(vec![*value])),
        Scalar::Float(value) => Arc::new(Float64Array::from(vec![scalar::float_key(*value)])),
        Scalar::Str(value) => Arc::new(StringArray::from(vec![&**value])),
        Scalar::Date(value) => Arc::new(Date32Array::from(vec![*value])),
        Scalar::DateTime(value) => Arc::new(TimestampMicrosecondArray::from(vec![*value])),
        Scalar::Duration(value) => Arc::new(DurationMicrosecondArray::from(vec![*value])),
        Scalar::Decimal(value) => decimals(Decimal128Array::from(vec![*value]))?,
        Scalar::List(_) => return Err(Error("a list is not a value of a column".into())),
    })
}

/// The element of `array` at `index`.
pub fn scalar_at(array: &dyn Array, index: usize) -> Result<Scalar> {
    if array.is_null(index) {
        return Ok(Scalar::Null);
    }
    Ok(match array.data_type() {
        ArrowType::Boolean => Scalar::Bool(array.as_boolean().value(index)),
        ArrowType::Int64 => Scalar::Int(array.as_primitive::<Int64Type>().value(index)),
        ArrowType::Float64 => Scalar::Float(array.as_primitive::<Float64Type>().value(index)),
        ArrowType::Utf8 => Scalar::Str(array.as_string::<i32>().value(index).into()),
        ArrowType::Date32 => Scalar::Date(array.as_primitive::<Date32Type>().value(index)),
        ArrowType::Timestamp(TimeUnit::Microsecond, None) => Scalar::DateTime(
            array
                .as_primitive::<TimestampMicrosecondType>()
                .value(index),
        ),
        ArrowType::Duration(TimeUnit::Microsecond) => {
            Scalar::Duration(array.as_primitive::<DurationMicrosecondType>().value(index))
        }
        ArrowType::Decimal128(..) => {
            Scalar::Decimal(array.as_primitive::<Decimal128Type>().value(index))
        }
        other => return Err(Error(format!("unexpected column type {other}"))),
    })
}

/// Builds a column of type `dtype` from single values.
pub fn scalars_to_array(values: impl Iterator<Item = Scalar>, dtype: DataType) -> Result<ArrayRef> {
    let mismatch = |value: &Scalar| {
        Error(format!(
            "internal error: {value} in a {} column",
            dtype.name()
        ))
    };
    // Collects the values of one variant into an array of options.
    macro_rules! collect {
        ($variant:ident, $array:ty) => {{
            let items = values.map(|value| match value {
                Scalar::$variant(value) => Ok(Some(value)),
                Scalar::Null => Ok(None),
                other => Err(mismatch(&other)),
            });
            items.collect::<Result<$array>>()?
        }};
    }
    Ok(match dtype {
        DataType::Int => Arc::new(collect!(Int, Int64Array)),
        DataType::Float => {
            let floats = collect!(Float, Float64Array);
            crate::expr::canonical(Arc::new(floats))
        }
        DataType::Date => Arc::new(collect!(Date, Date32Array)),
        DataType::DateTime => Arc::new(collect!(DateTime, TimestampMicrosecondArray)),
        DataType::Duration => Arc::new(collect!(Duration, DurationMicrosecondArray)),
        DataType::Decimal => decimals(collect!(Decimal, Decimal128Array))?,
        DataType::Bool => {
            let mut builder = BooleanBuilder::new();
            for value in values {
                match value {
                    Scalar::Bool(value) => builder.append_value(value),
                    Scalar::Null => builder.append_null(),
                    other => return Err(mismatch(&other)),
                }
            }
            Arc::new(builder.finish())
        }
        DataType::Str => {
            let mut builder = StringBuilder::new();
            for value in values {
                match value {
                    Scalar::Str(value) => builder.append_value(&*value),
                    Scalar::Null => builder.append_null(),
                    other => return Err(mismatch(&other)),
                }
            }
            Arc::new(builder.finish())
        }
    })
}
