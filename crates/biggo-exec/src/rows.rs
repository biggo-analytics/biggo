//! Reads the arguments of a scalar function a row at a time. An argument is a column or a
//! single value, and a single value holds for every row.

use arrow::array::{Array, ArrayRef, AsArray, BooleanArray, Int64Array, StringArray};
use arrow::datatypes::Int64Type;

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
