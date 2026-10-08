//! Reads the arguments of a scalar function a row at a time. An argument is a column or a
//! single value, and a single value holds for every row.

use arrow::array::{
    Array, ArrayRef, AsArray, BooleanArray, Date32Array, DurationMicrosecondArray, Float64Array,
    Int64Array, StringArray, TimestampMicrosecondArray,
};
use arrow::datatypes::{
    DataType as ArrowType, Date32Type, DurationMicrosecondType, Float64Type, Int64Type,
    TimestampMicrosecondType,
};
use biggo_plan::dates;

use crate::expr::Col;

pub struct Strs<'a> {
    array: &'a StringArray,
    single: bool,
}

impl<'a> Strs<'a> {
    pub fn new(col: &'a Col) -> Self {
        Strs {
            array: col.values().as_string::<i32>(),
            single: col.is_scalar(),
        }
    }

    pub fn get(&self, row: usize) -> Option<&'a str> {
        let row = if self.single { 0 } else { row };
        self.array.is_valid(row).then(|| self.array.value(row))
    }
}

pub struct Ints<'a> {
    array: &'a Int64Array,
    single: bool,
}

impl<'a> Ints<'a> {
    pub fn new(col: &'a Col) -> Self {
        Ints {
            array: col.values().as_primitive::<Int64Type>(),
            single: col.is_scalar(),
        }
    }

    pub fn get(&self, row: usize) -> Option<i64> {
        let row = if self.single { 0 } else { row };
        self.array.is_valid(row).then(|| self.array.value(row))
    }
}

pub struct Bools<'a> {
    array: &'a BooleanArray,
    single: bool,
}

impl<'a> Bools<'a> {
    pub fn new(col: &'a Col) -> Self {
        Bools {
            array: col.values().as_boolean(),
            single: col.is_scalar(),
        }
    }

    pub fn get(&self, row: usize) -> Option<bool> {
        let row = if self.single { 0 } else { row };
        self.array.is_valid(row).then(|| self.array.value(row))
    }
}

/// A date or a datetime argument, read as either.
pub struct Moments<'a> {
    array: Moment<'a>,
    single: bool,
}

enum Moment<'a> {
    Days(&'a Date32Array),
    Micros(&'a TimestampMicrosecondArray),
}

impl<'a> Moments<'a> {
    pub fn new(col: &'a Col) -> Self {
        let array = match col.values().data_type() {
            ArrowType::Date32 => Moment::Days(col.values().as_primitive::<Date32Type>()),
            _ => Moment::Micros(col.values().as_primitive::<TimestampMicrosecondType>()),
        };
        Moments {
            array,
            single: col.is_scalar(),
        }
    }

    /// Whether the argument is a date, with no time of day.
    pub fn is_date(&self) -> bool {
        matches!(self.array, Moment::Days(_))
    }

    /// The day, as days since 1970-01-01.
    pub fn day(&self, row: usize) -> Option<i32> {
        let row = if self.single { 0 } else { row };
        match self.array {
            Moment::Days(days) => days.is_valid(row).then(|| days.value(row)),
            Moment::Micros(micros) => match micros.is_valid(row) {
                true => dates::day_of(micros.value(row)),
                false => None,
            },
        }
    }

    /// The moment, as microseconds since 1970-01-01T00:00:00. A date is the start of its day.
    pub fn micros(&self, row: usize) -> Option<i64> {
        let row = if self.single { 0 } else { row };
        match self.array {
            Moment::Days(days) => days
                .is_valid(row)
                .then(|| i64::from(days.value(row)) * dates::DAY),
            Moment::Micros(micros) => micros.is_valid(row).then(|| micros.value(row)),
        }
    }
}

/// A duration argument, in microseconds.
pub struct Spans<'a> {
    array: &'a DurationMicrosecondArray,
    single: bool,
}

impl<'a> Spans<'a> {
    pub fn new(col: &'a Col) -> Self {
        Spans {
            array: col.values().as_primitive::<DurationMicrosecondType>(),
            single: col.is_scalar(),
        }
    }

    pub fn get(&self, row: usize) -> Option<i64> {
        let row = if self.single { 0 } else { row };
        self.array.is_valid(row).then(|| self.array.value(row))
    }
}

/// An int or a float argument, read as a float.
pub struct Numbers<'a> {
    array: Number<'a>,
    single: bool,
}

enum Number<'a> {
    Ints(&'a Int64Array),
    Floats(&'a Float64Array),
}

impl<'a> Numbers<'a> {
    pub fn new(col: &'a Col) -> Self {
        let array = match col.values().data_type() {
            ArrowType::Int64 => Number::Ints(col.values().as_primitive::<Int64Type>()),
            _ => Number::Floats(col.values().as_primitive::<Float64Type>()),
        };
        Numbers {
            array,
            single: col.is_scalar(),
        }
    }

    pub fn get(&self, row: usize) -> Option<f64> {
        let row = if self.single { 0 } else { row };
        match self.array {
            Number::Ints(ints) => ints.is_valid(row).then(|| ints.value(row) as f64),
            Number::Floats(floats) => floats.is_valid(row).then(|| floats.value(row)),
        }
    }
}

/// The number of rows a function of `args` gives for a batch of `rows`, and whether that is
/// a single value: it is when every argument is one.
pub fn shape(args: &[Col], rows: usize) -> (usize, bool) {
    let single = args.iter().all(Col::is_scalar);
    (if single { 1 } else { rows }, single)
}

/// The result of a function: a single value if its arguments all were.
pub fn result(single: bool, array: ArrayRef) -> Col {
    match single {
        true => Col::Scalar(array),
        false => Col::Array(array),
    }
}
