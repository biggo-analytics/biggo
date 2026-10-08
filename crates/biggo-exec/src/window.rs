//! Window functions. The input is sorted by partition and order, each function is computed
//! along that order, and the results are put back in the input's row order.

use std::sync::Arc;

use arrow::array::{Array, ArrayRef, AsArray, Float64Array, Int64Array, RecordBatch, UInt32Array};
use arrow::compute::kernels::cmp::distinct;
use arrow::compute::{cast, take};
use arrow::datatypes::{DataType as ArrowType, Float64Type, Int64Type};
use biggo_plan::{AggCall, DataType, Expr, Schema, SortKey, WindowCall, WindowFn};
use rayon::prelude::*;

use crate::aggregate::aggregate_groups;
use crate::convert::batch as make_batch;
use crate::expr::eval_array;
use crate::ops::{concat, sort_indices};
use crate::{BatchIter, Error, Result};

/// For each position in sorted order, whether its row differs from the previous row in the
/// given columns. Position 0 always counts as different.
fn changes(columns: &[ArrayRef], sorted: &UInt32Array) -> Result<Vec<bool>> {
    let rows = sorted.len();
    let mut changed: Vec<bool> = (0..rows).map(|position| position == 0).collect();
    if rows < 2 {
        return Ok(changed);
    }
    // Each column is put in sorted order and compared with itself one row along; two nulls
    // count as the same value. The columns are independent, so each has a core to itself.
    let differs = |column: &ArrayRef| -> Result<arrow::array::BooleanArray> {
        let ordered = take(column, sorted, None)?;
        let (after, before) = (ordered.slice(1, rows - 1), ordered.slice(0, rows - 1));
        Ok(distinct(&after, &before)?)
    };
    let per_column: Vec<_> = columns.par_iter().map(differs).collect::<Result<_>>()?;
    for column in per_column {
        for (position, differs) in column.values().iter().enumerate() {
            changed[position + 1] |= differs;
        }
    }
    Ok(changed)
}

/// Everything the functions need to know about the sorted input.
struct Layout {
    /// The partition of each position, numbered from 0.
    partition: Vec<u32>,
    partitions: usize,
    /// Where the partition of each position starts and ends.
    start: Vec<usize>,
    end: Vec<usize>,
    /// Whether each position differs from the one before it in the order columns, or starts
    /// a partition.
    new_peer: Vec<bool>,
}

fn layout(new_partition: &[bool], order_changes: &[bool]) -> Layout {
    let rows = new_partition.len();
    let mut partition = Vec::with_capacity(rows);
    let mut start = vec![0; rows];
    let mut current = 0u32;
    for position in 0..rows {
        if position > 0 && new_partition[position] {
            current += 1;
        }
        partition.push(current);
        start[position] = match new_partition[position] {
            true => position,
            false => start[position - 1],
        };
    }
    let mut end = vec![rows; rows];
    for position in (0..rows.saturating_sub(1)).rev() {
        end[position] = match new_partition[position + 1] {
            true => position + 1,
            false => end[position + 1],
        };
    }
    let new_peer = new_partition.iter().zip(order_changes);
    Layout {
        partitions: if rows == 0 { 0 } else { current as usize + 1 },
        partition,
        start,
        end,
        new_peer: new_peer
            .map(|(partition, order)| *partition || *order)
            .collect(),
    }
}

/// Computes one window function. `sorted` maps positions to input rows; the result has one
/// value per position.
fn compute(
    call: &WindowCall,
    batch: &RecordBatch,
    sorted: &UInt32Array,
    layout: &Layout,
) -> Result<ArrayRef> {
    let rows = sorted.len();
    let values = match &call.arg {
        Some(arg) => Some(take(&eval_array(arg, batch)?, sorted, None)?),
        None => None,
    };
    let argument = || {
        let missing = || {
            Error(format!(
                "internal error: `{}` without a column",
                call.func.name()
            ))
        };
        values.clone().ok_or_else(missing)
    };
    Ok(match call.func {
        WindowFn::RowNumber => {
            let numbers = (0..rows).map(|position| (position - layout.start[position] + 1) as i64);
            Arc::new(Int64Array::from_iter_values(numbers))
        }
        WindowFn::Rank => {
            // Rows that are equal in the order columns share the number of their first row.
            let mut rank = 0;
            let ranks = (0..rows).map(|position| {
                if layout.new_peer[position] {
                    rank = (position - layout.start[position] + 1) as i64;
                }
                rank
            });
            Arc::new(Int64Array::from_iter_values(ranks))
        }
        WindowFn::Lag | WindowFn::Lead => {
            let source: UInt32Array = (0..rows)
                .map(|position| {
                    let source = match call.func {
                        WindowFn::Lag => position.checked_sub(call.offset),
                        _ => position.checked_add(call.offset),
                    };
                    let inside = |source: &usize| {
                        *source >= layout.start[position] && *source < layout.end[position]
                    };
                    source.filter(inside).map(|source| source as u32)
                })
                .collect();
            take(&argument()?, &source, None)?
        }
        WindowFn::CumSum => {
            let values = argument()?;
            if values.data_type() == &ArrowType::Int64 {
                let ints = values.as_primitive::<Int64Type>();
                let mut total: Option<i64> = None;
                let mut overflowed = false;
                let sums: Int64Array = (0..rows)
                    .map(|position| {
                        if layout.start[position] == position {
                            total = None;
                        }
                        if ints.is_valid(position) {
                            let (sum, wrapped) =
                                total.unwrap_or(0).overflowing_add(ints.value(position));
                            overflowed |= wrapped;
                            total = Some(sum);
                        }
                        total
                    })
                    .collect();
                if overflowed {
                    return Err(Error("integer overflow".into()));
                }
                Arc::new(sums)
            } else {
                let floats = values.as_primitive::<Float64Type>();
                let mut total: Option<f64> = None;
                let sums: Float64Array = (0..rows)
                    .map(|position| {
                        if layout.start[position] == position {
                            total = None;
                        }
                        if floats.is_valid(position) {
                            total = Some(total.unwrap_or(0.0) + floats.value(position));
                        }
                        total
                    })
                    .collect();
                Arc::new(sums)
            }
        }
        WindowFn::MovingAvg => {
            let floats = cast(&argument()?, &ArrowType::Float64)?;
            let floats = floats.as_primitive::<Float64Type>();
            // Running totals let each window be a difference of two of them.
            let mut sums = Vec::with_capacity(rows + 1);
            let mut counts = Vec::with_capacity(rows + 1);
            let (mut sum, mut count) = (0.0, 0u64);
            sums.push(sum);
            counts.push(count);
            for position in 0..rows {
                if floats.is_valid(position) {
                    sum += floats.value(position);
                    count += 1;
                }
                sums.push(sum);
                counts.push(count);
            }
            let averages: Float64Array = (0..rows)
                .map(|position| {
                    let first = (position + 1)
                        .saturating_sub(call.offset)
                        .max(layout.start[position]);
                    let count = counts[position + 1] - counts[first];
                    (count > 0).then(|| (sums[position + 1] - sums[first]) / count as f64)
                })
                .collect();
            Arc::new(averages)
        }
        WindowFn::Agg(func) => {
            let agg = AggCall {
                func,
                arg: call.arg.clone(),
                arg2: None,
                ty: call.ty,
            };
            let per_partition =
                aggregate_groups(&agg, values.as_ref(), &layout.partition, layout.partitions)?;
            let partition = UInt32Array::from(layout.partition.clone());
            take(&per_partition, &partition, None)?
        }
    })
}

pub fn window(
    input: BatchIter,
    partition: &[Expr],
    order: &[SortKey],
    funcs: &[(Arc<str>, WindowCall)],
    input_schema: &Schema,
    schema: &Schema,
) -> Result<BatchIter> {
    let batch = concat(input, input_schema)?;
    let rows = batch.num_rows();
    let mut partition_columns = Vec::with_capacity(partition.len());
    for expr in partition {
        partition_columns.push(eval_array(expr, &batch)?);
    }
    let mut order_columns = Vec::with_capacity(order.len());
    for key in order {
        order_columns.push(eval_array(&key.expr, &batch)?);
    }
    let by_partition = partition_columns
        .iter()
        .map(|column| (column.clone(), false));
    let by_order = order_columns.iter().zip(order);
    let by_order = by_order.map(|(column, key)| (column.clone(), key.descending));
    let sorted = sort_indices(&batch, by_partition.chain(by_order), None)?;

    let new_partition = changes(&partition_columns, &sorted)?;
    let order_changes = match order_columns.is_empty() {
        // With no order, no row comes before another: all of a partition are peers.
        true => vec![false; rows],
        false => changes(&order_columns, &sorted)?,
    };
    let layout = layout(&new_partition, &order_changes);

    // `sorted[position]` is an input row; `positions[row]` is where that row sorted to.
    let mut positions = vec![0u32; rows];
    for (position, row) in sorted.values().iter().enumerate() {
        positions[*row as usize] = position as u32;
    }
    let positions = UInt32Array::from(positions);

    // The functions do not depend on each other: each is computed on a core of its own.
    let computed = funcs.par_iter().map(|(_, call)| -> Result<ArrayRef> {
        let in_sorted_order = compute(call, &batch, &sorted, &layout)?;
        let mut column = take(&in_sorted_order, &positions, None)?;
        if call.ty.dtype == DataType::Date && column.data_type() != &ArrowType::Date32 {
            column = cast(&column, &ArrowType::Date32)?;
        }
        Ok(column)
    });
    let mut columns: Vec<ArrayRef> = batch.columns().to_vec();
    columns.extend(computed.collect::<Result<Vec<_>>>()?);
    let batch = make_batch(schema, columns, rows)?;
    Ok(Box::new(std::iter::once(Ok(batch))))
}
