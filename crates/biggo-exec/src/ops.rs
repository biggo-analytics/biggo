//! The operators that work a batch at a time: filter, project, limit, and sort.

use std::collections::VecDeque;
use std::sync::Arc;

use arrow::array::{ArrayRef, AsArray, RecordBatch, UInt32Array};
use arrow::compute::{
    SortColumn, SortOptions, concat_batches, filter_record_batch, lexsort_to_indices,
    take_record_batch,
};
use arrow::row::{RowConverter, SortField};
use biggo_plan::{Expr, Schema, SortKey};
use rayon::prelude::*;

use crate::convert::{arrow_schema, batch as make_batch};
use crate::expr::eval_array;
use crate::{BatchIter, Error, Result};

/// Applies `f` to every batch of `input`, several batches at a time on different cores, and
/// yields the results in input order.
pub fn par_map(
    input: BatchIter,
    f: impl Fn(RecordBatch) -> Result<RecordBatch> + Send + Sync + 'static,
) -> BatchIter {
    Box::new(ParMap {
        input,
        f,
        ready: VecDeque::new(),
        error: None,
        done: false,
    })
}

struct ParMap<F> {
    input: BatchIter,
    f: F,
    ready: VecDeque<RecordBatch>,
    /// An error to report once the batches before it have been yielded.
    error: Option<Error>,
    done: bool,
}

impl<F> Iterator for ParMap<F>
where
    F: Fn(RecordBatch) -> Result<RecordBatch> + Send + Sync,
{
    type Item = Result<RecordBatch>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(batch) = self.ready.pop_front() {
                return Some(Ok(batch));
            }
            if let Some(err) = self.error.take() {
                self.done = true;
                return Some(Err(err));
            }
            if self.done {
                return None;
            }
            let mut pending = Vec::new();
            for _ in 0..rayon::current_num_threads() {
                match self.input.next() {
                    Some(Ok(batch)) => pending.push(batch),
                    Some(Err(err)) => {
                        self.error = Some(err);
                        break;
                    }
                    None => {
                        self.done = true;
                        break;
                    }
                }
            }
            // The batches are worked on at once but handed on in order, and so is an error:
            // it comes after the batches before it, and before any error further on.
            let mapped: Vec<Result<RecordBatch>> = pending.into_par_iter().map(&self.f).collect();
            for batch in mapped {
                match batch {
                    Ok(batch) if batch.num_rows() > 0 => self.ready.push_back(batch),
                    Ok(_) => {}
                    Err(err) => {
                        self.error = Some(err);
                        break;
                    }
                }
            }
        }
    }
}

pub fn filter(input: BatchIter, predicate: Expr) -> BatchIter {
    par_map(input, move |batch| {
        let keep = eval_array(&predicate, &batch)?;
        // Rows for which the predicate is null are dropped, like those where it is false.
        Ok(filter_record_batch(&batch, keep.as_boolean())?)
    })
}

pub fn project(input: BatchIter, columns: Vec<(Arc<str>, Expr)>, schema: Arc<Schema>) -> BatchIter {
    par_map(input, move |batch| {
        let mut arrays = Vec::with_capacity(columns.len());
        for (_, expr) in &columns {
            arrays.push(eval_array(expr, &batch)?);
        }
        make_batch(&schema, arrays, batch.num_rows())
    })
}

pub fn limit(input: BatchIter, skip: usize, fetch: Option<usize>) -> BatchIter {
    let mut skip = skip;
    let mut fetch = fetch;
    let mut input = input;
    Box::new(std::iter::from_fn(move || {
        loop {
            if fetch == Some(0) {
                return None;
            }
            let batch = match input.next()? {
                Ok(batch) => batch,
                Err(err) => return Some(Err(err)),
            };
            let rows = batch.num_rows();
            if skip >= rows {
                skip -= rows;
                continue;
            }
            let length = (rows - skip).min(fetch.unwrap_or(usize::MAX));
            let batch = batch.slice(skip, length);
            skip = 0;
            if let Some(fetch) = &mut fetch {
                *fetch -= length;
            }
            return Some(Ok(batch));
        }
    }))
}

/// Gathers all of `input` into one batch.
pub fn concat(input: BatchIter, schema: &Schema) -> Result<RecordBatch> {
    let batches: Vec<RecordBatch> = input.collect::<Result<_>>()?;
    match batches.first() {
        Some(first) => Ok(concat_batches(&first.schema(), &batches)?),
        None => Ok(RecordBatch::new_empty(arrow_schema(schema))),
    }
}

/// From this many rows on, a whole table is sorted on all cores.
const PARALLEL_SORT_ROWS: usize = 1 << 14;

/// The keys of this many rows are encoded at a time, each lot on a core of its own. It is a
/// power of two, so that finding the lot of a row is a shift.
const ENCODE_SHIFT: u32 = 16;

/// The permutation that sorts `batch` by `keys`, nulls last. Rows that compare equal keep
/// their order. With `fetch`, only that many leading rows are returned.
pub fn sort_indices(
    batch: &RecordBatch,
    keys: impl IntoIterator<Item = (ArrayRef, bool)>,
    fetch: Option<usize>,
) -> Result<UInt32Array> {
    sort_indices_from(batch, keys, fetch, PARALLEL_SORT_ROWS)
}

/// `sort_indices`, sorting on all cores when there are at least `parallel_from` rows.
fn sort_indices_from(
    batch: &RecordBatch,
    keys: impl IntoIterator<Item = (ArrayRef, bool)>,
    fetch: Option<usize>,
    parallel_from: usize,
) -> Result<UInt32Array> {
    let keys: Vec<(ArrayRef, bool)> = keys.into_iter().collect();
    let options = |descending| SortOptions {
        descending,
        nulls_first: false,
    };
    // To sort everything, each row's keys are encoded as bytes that compare the way the keys
    // do. Comparing bytes is quick, and the row numbers can then be sorted on all cores.
    // Ties go by row number, which makes the order the same however the work was divided.
    if fetch.is_none() && batch.num_rows() >= parallel_from && !keys.is_empty() {
        let fields = keys.iter().map(|(values, descending)| {
            SortField::new_with_options(values.data_type().clone(), options(*descending))
        });
        let converter = RowConverter::new(fields.collect())?;
        let rows = batch.num_rows();
        let lot = 1usize << ENCODE_SHIFT;
        let lots: Vec<usize> = (0..rows.div_ceil(lot)).collect();
        let encoded: Vec<arrow::row::Rows> = lots
            .into_par_iter()
            .map(|index| {
                let (start, length) = (index * lot, lot.min(rows - index * lot));
                let columns: Vec<ArrayRef> = keys
                    .iter()
                    .map(|(values, _)| values.slice(start, length))
                    .collect();
                Ok(converter.convert_columns(&columns)?)
            })
            .collect::<Result<_>>()?;
        let key = |row: u32| {
            let row = row as usize;
            encoded[row >> ENCODE_SHIFT].row(row & (lot - 1))
        };
        // The first bytes of each key travel with its row number, so that most comparisons
        // are settled without looking the keys up. A key shorter than that is padded with
        // zeros, which never puts it after a key it comes before.
        let start = |row: u32| {
            let key = key(row);
            let bytes: &[u8] = key.as_ref();
            let mut buffer = [0u8; 16];
            let length = bytes.len().min(buffer.len());
            buffer[..length].copy_from_slice(&bytes[..length]);
            u128::from_be_bytes(buffer)
        };
        let rows = (0..rows as u32).into_par_iter();
        let mut order: Vec<(u64, u64, u32)> = rows
            .map(|row| {
                let start = start(row);
                ((start >> 64) as u64, start as u64, row)
            })
            .collect();
        order.par_sort_unstable_by(|a, b| {
            let by_start = (a.0, a.1).cmp(&(b.0, b.1));
            by_start
                .then_with(|| key(a.2).cmp(&key(b.2)))
                .then(a.2.cmp(&b.2))
        });
        let order: Vec<u32> = order.into_iter().map(|(_, _, row)| row).collect();
        return Ok(UInt32Array::from(order));
    }
    let mut columns: Vec<SortColumn> = keys
        .into_iter()
        .map(|(values, descending)| SortColumn {
            values,
            options: Some(options(descending)),
        })
        .collect();
    // The row number as the last key makes the order of ties definite.
    let rows = batch.num_rows() as u32;
    columns.push(SortColumn {
        values: Arc::new(UInt32Array::from_iter_values(0..rows)),
        options: None,
    });
    Ok(lexsort_to_indices(&columns, fetch)?)
}

pub fn sort(
    input: BatchIter,
    keys: &[SortKey],
    fetch: Option<usize>,
    schema: &Schema,
) -> Result<BatchIter> {
    let batch = concat(input, schema)?;
    let mut columns = Vec::with_capacity(keys.len());
    for key in keys {
        columns.push((eval_array(&key.expr, &batch)?, key.descending));
    }
    let indices = sort_indices(&batch, columns, fetch)?;
    let sorted = take_record_batch(&batch, &indices)?;
    Ok(Box::new(std::iter::once(Ok(sorted))))
}

/// Turns `columns` into rows: every input row gives one row per column.
pub fn unpivot(input: BatchIter, columns: Vec<Arc<str>>, schema: Arc<Schema>) -> BatchIter {
    par_map(input, move |batch| {
        let (rows, width) = (batch.num_rows(), columns.len());
        // Input row `r` becomes output rows `r * width` to `r * width + width - 1`.
        let repeated = UInt32Array::from_iter_values(
            (0..rows as u32).flat_map(|row| std::iter::repeat_n(row, width)),
        );
        let mut values = Vec::with_capacity(width);
        for column in &columns {
            values.push(column_of(&batch, column)?);
        }
        let values: Vec<&dyn arrow::array::Array> = values.iter().map(|v| v.as_ref()).collect();
        let cells: Vec<(usize, usize)> = (0..rows)
            .flat_map(|row| (0..width).map(move |column| (column, row)))
            .collect();
        let names = (0..rows).flat_map(|_| columns.iter().map(|name| Some(&**name)));
        let names: ArrayRef = Arc::new(names.collect::<arrow::array::StringArray>());
        let values = arrow::compute::interleave(&values, &cells)?;

        // The schema lists the columns that are kept, then the name and the value.
        let kept = schema.fields.len() - 2;
        let mut arrays = Vec::with_capacity(schema.fields.len());
        for field in &schema.fields[..kept] {
            arrays.push(arrow::compute::take(
                &column_of(&batch, &field.name)?,
                &repeated,
                None,
            )?);
        }
        arrays.push(names);
        arrays.push(values);
        make_batch(&schema, arrays, rows * width)
    })
}

/// Splits the strings of `column` at `separator` and gives one row per piece, with the
/// spaces around a piece removed. A null stays a single row.
pub fn explode(input: BatchIter, column: Arc<str>, separator: Arc<str>) -> BatchIter {
    par_map(input, move |batch| {
        let position = batch.schema().index_of(&column)?;
        let strings = batch.column(position).as_string::<i32>();
        let mut rows = Vec::new();
        let mut pieces = arrow::array::StringBuilder::new();
        for (row, string) in strings.iter().enumerate() {
            match string {
                Some(string) => {
                    for piece in string.split(&*separator) {
                        rows.push(row as u32);
                        pieces.append_value(piece.trim());
                    }
                }
                None => {
                    rows.push(row as u32);
                    pieces.append_null();
                }
            }
        }
        let rows = UInt32Array::from(rows);
        let mut columns = take_record_batch(&batch, &rows)?.columns().to_vec();
        columns[position] = Arc::new(pieces.finish());
        Ok(RecordBatch::try_new(batch.schema(), columns)?)
    })
}

fn column_of(batch: &RecordBatch, name: &str) -> Result<ArrayRef> {
    let column = batch.column_by_name(name).cloned();
    column.ok_or_else(|| Error(format!("internal error: no column `{name}` in the batch")))
}

#[cfg(test)]
mod tests {
    use arrow::array::{Date32Array, Float64Array, Int64Array, StringArray};

    use super::*;

    /// A table of made-up values with many ties, nulls, and the awkward floats.
    fn sample(rows: usize) -> (RecordBatch, Vec<ArrayRef>) {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let floats = [
            0.0,
            -0.0,
            1.5,
            -2.25,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ];
        let words = [
            "",
            "a",
            "ab",
            "b",
            "ก",
            "ข้าว",
            "a longer string than sixteen bytes",
        ];
        let mut ints = Vec::with_capacity(rows);
        let mut reals = Vec::with_capacity(rows);
        let mut texts = Vec::with_capacity(rows);
        let mut days = Vec::with_capacity(rows);
        for _ in 0..rows {
            let pick = next();
            ints.push((pick % 11 != 0).then_some((pick % 7) as i64 - 3));
            reals.push((pick % 13 != 0).then_some(floats[(pick >> 8) as usize % floats.len()]));
            texts.push((pick % 17 != 0).then_some(words[(pick >> 16) as usize % words.len()]));
            days.push((pick % 19 != 0).then_some((pick >> 24) as i32 % 5));
        }
        let columns: Vec<ArrayRef> = vec![
            Arc::new(Int64Array::from(ints)),
            Arc::new(Float64Array::from(reals)),
            Arc::new(StringArray::from(texts)),
            Arc::new(Date32Array::from(days)),
        ];
        let named = ["i", "f", "s", "d"]
            .into_iter()
            .zip(columns.iter().cloned());
        let batch = RecordBatch::try_from_iter(named).unwrap();
        (batch, columns)
    }

    /// Sorting on all cores gives exactly the order of the plain sort, for every choice of
    /// keys and directions: the order must not depend on how a table is sorted.
    #[test]
    fn the_parallel_sort_agrees_with_the_plain_one() {
        let (batch, columns) = sample(40_000);
        let choices: [&[(usize, bool)]; 7] = [
            &[(0, false)],
            &[(1, true)],
            &[(2, false)],
            &[(3, true), (0, false)],
            &[(2, true), (1, false), (0, true)],
            &[(0, false), (3, false), (2, false), (1, false)],
            &[(1, false), (2, true)],
        ];
        for choice in choices {
            let keys = || {
                choice
                    .iter()
                    .map(|&(column, desc)| (columns[column].clone(), desc))
            };
            let plain = sort_indices_from(&batch, keys(), None, usize::MAX).unwrap();
            let parallel = sort_indices_from(&batch, keys(), None, 0).unwrap();
            assert_eq!(plain, parallel, "keys {choice:?}");
        }
    }
}
