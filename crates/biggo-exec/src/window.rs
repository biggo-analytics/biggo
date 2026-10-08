//! Window functions. The input is sorted by partition and order, each function is computed
//! along that order, and the results are put back in the input's row order.

use std::collections::VecDeque;
use std::sync::Arc;

use arrow::array::{
    Array, ArrayRef, ArrowPrimitiveType, AsArray, Float64Array, Int64Array, PrimitiveArray,
    RecordBatch, UInt32Array,
};
use arrow::compute::kernels::cmp::distinct;
use arrow::compute::{cast, take};
use arrow::datatypes::{DataType as ArrowType, Float64Type, Int64Type};
use arrow::row::{RowConverter, SortField};
use biggo_plan::{AggCall, DataType, Expr, Schema, SortKey, WindowCall, WindowFn};
use rayon::prelude::*;

use crate::aggregate::aggregate_groups;
use crate::convert::batch as make_batch;
use crate::expr::{canonical, eval_array};
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

/// The best of the values at each position and the `size - 1` positions before it in its
/// partition, where `better` says that its first argument beats its second. Nulls are passed
/// over, and a window with nothing but nulls gives null.
fn moving_extreme<T: ArrowPrimitiveType>(
    values: &PrimitiveArray<T>,
    layout: &Layout,
    size: usize,
    better: impl Fn(T::Native, T::Native) -> bool,
) -> PrimitiveArray<T> {
    // The positions that can still be the best of a window to come, best first: a value
    // outlasts every earlier one that does not beat it.
    let mut candidates: VecDeque<usize> = VecDeque::new();
    let best = (0..values.len()).map(|position| {
        if layout.start[position] == position {
            candidates.clear();
        }
        let first = (position + 1)
            .saturating_sub(size)
            .max(layout.start[position]);
        while candidates.front().is_some_and(|&front| front < first) {
            candidates.pop_front();
        }
        if values.is_valid(position) {
            let value = values.value(position);
            while candidates
                .back()
                .is_some_and(|&back| !better(values.value(back), value))
            {
                candidates.pop_back();
            }
            candidates.push_back(position);
        }
        candidates.front().map(|&front| values.value(front))
    });
    best.collect()
}

/// The sum and the number of the values at each position and the `size - 1` positions before
/// it in its partition, nulls left out.
///
/// Running totals make each window a difference of two of them. They start afresh in every
/// partition, and take in only the finite values: a value that is infinite or not a number
/// would otherwise spoil every window after it, where it should only mark its own.
fn moving_totals(floats: &Float64Array, layout: &Layout, size: usize) -> Vec<(f64, u64)> {
    let rows = floats.len();
    // What is counted along the way: values, and those that are not a number, positive
    // infinity, and negative infinity.
    let mut counts = vec![[0u64; 4]; rows + 1];
    let mut totals = vec![0.0; rows + 1];
    for position in 0..rows {
        let before = match layout.start[position] == position {
            true => 0.0,
            false => totals[position],
        };
        let mut count = counts[position];
        let mut finite = 0.0;
        if floats.is_valid(position) {
            let value = floats.value(position);
            count[0] += 1;
            match value {
                value if value.is_nan() => count[1] += 1,
                f64::INFINITY => count[2] += 1,
                f64::NEG_INFINITY => count[3] += 1,
                value => finite = value,
            }
        }
        counts[position + 1] = count;
        totals[position + 1] = before + finite;
    }
    let window = |position: usize| {
        let start = layout.start[position];
        let first = (position + 1).saturating_sub(size).max(start);
        let before = if first == start { 0.0 } else { totals[first] };
        let count = |kind: usize| counts[position + 1][kind] - counts[first][kind];
        let sum = match (count(1), count(2), count(3)) {
            (0, 0, 0) => totals[position + 1] - before,
            (0, _, 0) => f64::INFINITY,
            (0, 0, _) => f64::NEG_INFINITY,
            _ => f64::NAN,
        };
        (sum, count(0))
    };
    (0..rows).map(window).collect()
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
            let totals = moving_totals(floats.as_primitive::<Float64Type>(), layout, call.offset);
            let averages: Float64Array = totals
                .into_iter()
                .map(|(sum, count)| (count > 0).then(|| sum / count as f64))
                .collect();
            Arc::new(averages)
        }
        WindowFn::DenseRank => {
            let mut rank = 0;
            let ranks = (0..rows).map(|position| {
                if layout.start[position] == position {
                    rank = 0;
                }
                rank += i64::from(layout.new_peer[position]);
                rank
            });
            Arc::new(Int64Array::from_iter_values(ranks))
        }
        WindowFn::PercentRank => {
            // The rows that come before the row's peers, as a share of the other rows.
            let mut before = 0;
            let shares = (0..rows).map(|position| {
                if layout.new_peer[position] {
                    before = position - layout.start[position];
                }
                match layout.end[position] - layout.start[position] - 1 {
                    0 => 0.0,
                    others => before as f64 / others as f64,
                }
            });
            Arc::new(Float64Array::from_iter_values(shares))
        }
        WindowFn::Ntile => {
            let groups = call.offset.max(1);
            let tiles = (0..rows).map(|position| {
                let size = layout.end[position] - layout.start[position];
                let index = position - layout.start[position];
                // The groups differ in size by at most one row, and the larger come first.
                let (small, larger) = (size / groups, size % groups);
                let tile = match index < larger * (small + 1) {
                    true => index / (small + 1),
                    false => larger + (index - larger * (small + 1)) / small.max(1),
                };
                (tile + 1) as i64
            });
            Arc::new(Int64Array::from_iter_values(tiles))
        }
        WindowFn::CumCount => {
            let mut count = 0;
            let counts = (0..rows).map(|position| {
                if layout.start[position] == position {
                    count = 0;
                }
                let counted = values
                    .as_ref()
                    .is_none_or(|values| values.is_valid(position));
                count += i64::from(counted);
                count
            });
            Arc::new(Int64Array::from_iter_values(counts))
        }
        WindowFn::CumMean => {
            let floats = cast(&argument()?, &ArrowType::Float64)?;
            let floats = floats.as_primitive::<Float64Type>();
            let (mut sum, mut count) = (0.0, 0u64);
            let means: Float64Array = (0..rows)
                .map(|position| {
                    if layout.start[position] == position {
                        (sum, count) = (0.0, 0);
                    }
                    if floats.is_valid(position) {
                        sum += floats.value(position);
                        count += 1;
                    }
                    (count > 0).then(|| sum / count as f64)
                })
                .collect();
            Arc::new(means)
        }
        WindowFn::CumMin | WindowFn::CumMax => {
            // Values of any type compare as their sort keys do; the result is the value at
            // the best position so far.
            let values = argument()?;
            let field = SortField::new(values.data_type().clone());
            let keys =
                RowConverter::new(vec![field])?.convert_columns(std::slice::from_ref(&values))?;
            let largest = call.func == WindowFn::CumMax;
            let mut best: Option<usize> = None;
            let chosen: UInt32Array = (0..rows)
                .map(|position| {
                    if layout.start[position] == position {
                        best = None;
                    }
                    if values.is_valid(position) {
                        let better = best.is_none_or(|best| match largest {
                            true => keys.row(position) > keys.row(best),
                            false => keys.row(position) < keys.row(best),
                        });
                        if better {
                            best = Some(position);
                        }
                    }
                    best.map(|best| best as u32)
                })
                .collect();
            take(&values, &chosen, None)?
        }
        WindowFn::MovingSum => {
            let values = argument()?;
            // Running totals let each window be a difference of two of them. Whole numbers
            // are totalled wide enough that only a window's own sum can overflow.
            let first = |position: usize| {
                (position + 1)
                    .saturating_sub(call.offset)
                    .max(layout.start[position])
            };
            let mut counts = Vec::with_capacity(rows + 1);
            counts.push(0u64);
            for position in 0..rows {
                counts.push(counts[position] + u64::from(values.is_valid(position)));
            }
            let some = |position: usize| counts[position + 1] > counts[first(position)];
            if values.data_type() == &ArrowType::Int64 {
                let ints = values.as_primitive::<Int64Type>();
                let mut totals = Vec::with_capacity(rows + 1);
                totals.push(0i128);
                for position in 0..rows {
                    let value = if ints.is_valid(position) {
                        ints.value(position)
                    } else {
                        0
                    };
                    totals.push(totals[position] + i128::from(value));
                }
                let mut overflowed = false;
                let sums: Int64Array = (0..rows)
                    .map(|position| {
                        let sum = totals[position + 1] - totals[first(position)];
                        let sum = i64::try_from(sum).unwrap_or_else(|_| {
                            overflowed = true;
                            0
                        });
                        some(position).then_some(sum)
                    })
                    .collect();
                if overflowed {
                    return Err(Error("integer overflow".into()));
                }
                Arc::new(sums)
            } else {
                let floats = values.as_primitive::<Float64Type>();
                let sums: Float64Array = moving_totals(floats, layout, call.offset)
                    .into_iter()
                    .map(|(sum, count)| (count > 0).then_some(sum))
                    .collect();
                Arc::new(sums)
            }
        }
        WindowFn::MovingMin | WindowFn::MovingMax => {
            let values = argument()?;
            let largest = call.func == WindowFn::MovingMax;
            if values.data_type() == &ArrowType::Int64 {
                let ints = values.as_primitive::<Int64Type>();
                Arc::new(moving_extreme(
                    ints,
                    layout,
                    call.offset,
                    |a, b| match largest {
                        true => a > b,
                        false => a < b,
                    },
                ))
            } else {
                // "Not a number" sorts above every number, as it does in `sort`.
                let floats = values.as_primitive::<Float64Type>();
                Arc::new(moving_extreme(
                    floats,
                    layout,
                    call.offset,
                    |a, b| match largest {
                        true => a.total_cmp(&b).is_gt(),
                        false => a.total_cmp(&b).is_lt(),
                    },
                ))
            }
        }
        WindowFn::Diff | WindowFn::PctChange => {
            let values = argument()?;
            let earlier = |position: usize| {
                let earlier = position.checked_sub(call.offset);
                earlier.filter(|earlier| *earlier >= layout.start[position])
            };
            if call.func == WindowFn::Diff && values.data_type() == &ArrowType::Int64 {
                let ints = values.as_primitive::<Int64Type>();
                let mut overflowed = false;
                let changes: Int64Array = (0..rows)
                    .map(|position| {
                        let earlier = earlier(position)?;
                        if ints.is_null(position) || ints.is_null(earlier) {
                            return None;
                        }
                        let change = ints.value(position).checked_sub(ints.value(earlier));
                        overflowed |= change.is_none();
                        change
                    })
                    .collect();
                if overflowed {
                    return Err(Error("integer overflow".into()));
                }
                Arc::new(changes)
            } else {
                let floats = cast(&values, &ArrowType::Float64)?;
                let floats = floats.as_primitive::<Float64Type>();
                let share = call.func == WindowFn::PctChange;
                let changes: Float64Array = (0..rows)
                    .map(|position| {
                        let earlier = earlier(position)?;
                        if floats.is_null(position) || floats.is_null(earlier) {
                            return None;
                        }
                        let (now, then) = (floats.value(position), floats.value(earlier));
                        Some(if share {
                            (now - then) / then
                        } else {
                            now - then
                        })
                    })
                    .collect();
                Arc::new(changes)
            }
        }
        WindowFn::FillForward | WindowFn::FillBackward => {
            let values = argument()?;
            // Each position takes the value at the nearest position that has one, looking
            // back, or looking ahead, and never past the ends of its partition.
            let forward = call.func == WindowFn::FillForward;
            let mut source: Vec<Option<u32>> = vec![None; rows];
            let mut nearest = None;
            let mut fill = |position: usize, slot: &mut Option<u32>| {
                let edge = match forward {
                    true => layout.start[position] == position,
                    false => layout.end[position] == position + 1,
                };
                if edge {
                    nearest = None;
                }
                if values.is_valid(position) {
                    nearest = Some(position as u32);
                }
                *slot = nearest;
            };
            match forward {
                true => source
                    .iter_mut()
                    .enumerate()
                    .for_each(|(at, slot)| fill(at, slot)),
                false => source
                    .iter_mut()
                    .enumerate()
                    .rev()
                    .for_each(|(at, slot)| fill(at, slot)),
            }
            take(&values, &UInt32Array::from(source), None)?
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
        let in_sorted_order = canonical(compute(call, &batch, &sorted, &layout)?);
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
