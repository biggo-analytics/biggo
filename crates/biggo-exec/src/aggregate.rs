//! Hash aggregation. Batches are aggregated on all cores into partial results, which are then
//! merged; groups come out in the order their first row appears in the input.

use std::hash::BuildHasher;
use std::sync::Arc;

use arrow::array::{
    Array, ArrayRef, ArrowPrimitiveType, AsArray, Date32Array, Decimal128Array, Float64Array,
    Int64Array, PrimitiveArray, RecordBatch, StringArray, UInt32Array,
};
use arrow::compute::cast;
use arrow::datatypes::{DataType as ArrowType, Decimal128Type, Float64Type, Int64Type};
use arrow::row::{RowConverter, SortField};
use biggo_plan::{AggCall, AggFn, ColType, DataType, Expr, ExprKind, Extra, Scalar, Schema};
use hashbrown::{DefaultHashBuilder, HashSet, HashTable};
use rayon::prelude::*;

use crate::convert::{arrow_type, batch as make_batch, decimals, scalar_at, scalars_to_array};
use crate::expr::{canonical, eval_array};
use crate::{BatchIter, Error, Result};

/// The distinct keys seen so far, each with a dense id in order of first appearance.
#[derive(Default)]
struct Groups {
    table: HashTable<u32>,
    hasher: DefaultHashBuilder,
    /// The keys, end to end, in the row format of `arrow::row`.
    data: Vec<u8>,
    ends: Vec<usize>,
}

fn key_of<'a>(data: &'a [u8], ends: &[usize], id: u32) -> &'a [u8] {
    let id = id as usize;
    let start = if id == 0 { 0 } else { ends[id - 1] };
    &data[start..ends[id]]
}

impl Groups {
    fn len(&self) -> usize {
        self.ends.len()
    }

    fn key(&self, id: u32) -> &[u8] {
        key_of(&self.data, &self.ends, id)
    }

    /// The id of `key`, which is added if it is new.
    fn intern(&mut self, key: &[u8]) -> u32 {
        let hash = self.hasher.hash_one(key);
        let (data, ends) = (&self.data, &self.ends);
        if let Some(&id) = self.table.find(hash, |&id| key_of(data, ends, id) == key) {
            return id;
        }
        let id = self.ends.len() as u32;
        self.data.extend_from_slice(key);
        self.ends.push(self.data.len());
        let (data, ends, hasher) = (&self.data, &self.ends, &self.hasher);
        self.table
            .insert_unique(hash, id, |&id| hasher.hash_one(key_of(data, ends, id)));
        id
    }
}

/// The running state of one aggregate function, one slot per group.
pub enum Acc {
    Count(Vec<i64>),
    /// The sums are kept wider than an int, so that whether a sum overflows depends on the
    /// sum alone, and not on how the rows that make it up were divided among cores.
    SumInt {
        sums: Vec<i128>,
        seen: Vec<bool>,
    },
    SumFloat {
        sums: Vec<f64>,
        seen: Vec<bool>,
    },
    Mean {
        sums: Vec<f64>,
        counts: Vec<u64>,
    },
    /// Count, mean, and sum of squared distances from the mean, kept up to date per value.
    /// The standard deviation and the variance, of a sample or of a population, follow
    /// from them; `func` says which is wanted.
    Stddev {
        func: AggFn,
        counts: Vec<f64>,
        means: Vec<f64>,
        squares: Vec<f64>,
    },
    /// Minimum or maximum of ints, or of dates as their day numbers.
    ExtremeInt {
        best: Vec<i64>,
        seen: Vec<bool>,
        max: bool,
    },
    ExtremeFloat {
        best: Vec<f64>,
        seen: Vec<bool>,
        max: bool,
    },
    ExtremeStr {
        best: Vec<Option<String>>,
        max: bool,
    },
    /// The value in the first or last row of each group.
    Edge {
        values: Vec<Scalar>,
        seen: Vec<bool>,
        last: bool,
    },
    Median(Vec<Vec<f64>>),
    /// The values of each group, of which the one a share of the way up is found at the end.
    Quantile {
        all: Vec<Vec<f64>>,
        share: f64,
    },
    Product {
        products: Vec<f64>,
        seen: Vec<bool>,
    },
    /// The strings of each group, joined as they come.
    Strings {
        texts: Vec<String>,
        seen: Vec<bool>,
        separator: Arc<str>,
    },
    /// The value of one column in the row where another is largest or smallest. `keys` holds
    /// the best of that other column so far, in a form that compares as its values do.
    ArgExtreme {
        keys: Vec<Vec<u8>>,
        values: Vec<Scalar>,
        seen: Vec<bool>,
        max: bool,
        converter: Arc<RowConverter>,
    },
    SumDecimal {
        sums: Vec<i128>,
        seen: Vec<bool>,
    },
    ExtremeDecimal {
        best: Vec<i128>,
        seen: Vec<bool>,
        max: bool,
    },
    /// What relates two columns `x` and `y`: the number of rows where both have a value,
    /// their means, and the sums of products of their distances from the means. Correlation,
    /// covariance, and the least-squares line all follow from these.
    Pair {
        func: AggFn,
        counts: Vec<f64>,
        mean_x: Vec<f64>,
        mean_y: Vec<f64>,
        cross: Vec<f64>,
        square_x: Vec<f64>,
        square_y: Vec<f64>,
    },
    Distinct {
        sets: Vec<HashSet<Box<[u8]>>>,
        converter: Arc<RowConverter>,
    },
    /// The values of each group, as whole numbers, of which the distinct ones are counted at
    /// the end. Gathering them costs less than keeping a set per group, and the counting
    /// then runs on all cores.
    DistinctFixed(Vec<Vec<i64>>),
}

/// Whether the values of a type can be told apart by a whole number each.
fn fixed_width(dtype: DataType) -> bool {
    use DataType::*;
    matches!(dtype, Int | Float | Bool | Date | DateTime | Duration)
}

/// A whole number for each value, the same for two values exactly when they are equal.
fn distinct_keys(values: &ArrayRef) -> Result<ArrayRef> {
    Ok(match values.data_type() {
        ArrowType::Int64 => values.clone(),
        ArrowType::Float64 => {
            let floats = values.as_primitive::<Float64Type>();
            // Zero has two signs and "not a number" many bit patterns; each is one value.
            let keys: Int64Array = floats.unary(|value| {
                if value == 0.0 {
                    0
                } else if value.is_nan() {
                    i64::MAX
                } else {
                    value.to_bits() as i64
                }
            });
            Arc::new(keys)
        }
        _ => cast(values, &ArrowType::Int64)?,
    })
}

fn overflow() -> Error {
    Error("integer overflow".into())
}

/// Calls `f` with the group and value of every non-null element.
fn each_valid<T: ArrowPrimitiveType>(
    values: &PrimitiveArray<T>,
    groups: &[u32],
    mut f: impl FnMut(usize, T::Native),
) {
    let pairs = groups.iter().zip(values.values());
    match values.nulls() {
        None => pairs.for_each(|(&group, &value)| f(group as usize, value)),
        Some(nulls) => {
            for (row, (&group, &value)) in pairs.enumerate() {
                if nulls.is_valid(row) {
                    f(group as usize, value);
                }
            }
        }
    }
}

fn as_floats(values: &ArrayRef) -> Result<ArrayRef> {
    Ok(cast(values, &ArrowType::Float64)?)
}

impl Acc {
    pub fn new(call: &AggCall) -> Result<Acc> {
        let arg = call.arg.as_ref().map(|arg| arg.ty.dtype);
        Ok(match (call.func, arg) {
            (AggFn::Count, _) => Acc::Count(Vec::new()),
            (AggFn::Sum, Some(DataType::Decimal)) => Acc::SumDecimal {
                sums: Vec::new(),
                seen: Vec::new(),
            },
            (AggFn::Corr | AggFn::Cov | AggFn::Slope | AggFn::Intercept, _) => Acc::Pair {
                func: call.func,
                counts: Vec::new(),
                mean_x: Vec::new(),
                mean_y: Vec::new(),
                cross: Vec::new(),
                square_x: Vec::new(),
                square_y: Vec::new(),
            },
            // A duration is summed as its microseconds.
            (AggFn::Sum, Some(DataType::Int | DataType::Duration)) => Acc::SumInt {
                sums: Vec::new(),
                seen: Vec::new(),
            },
            (AggFn::Sum, _) => Acc::SumFloat {
                sums: Vec::new(),
                seen: Vec::new(),
            },
            (AggFn::Mean, _) => Acc::Mean {
                sums: Vec::new(),
                counts: Vec::new(),
            },
            (AggFn::Stddev | AggFn::Variance | AggFn::StddevPop | AggFn::VariancePop, _) => {
                Acc::Stddev {
                    func: call.func,
                    counts: Vec::new(),
                    means: Vec::new(),
                    squares: Vec::new(),
                }
            }
            (AggFn::Quantile, _) => {
                let share = match call.arg2.as_ref().map(|share| &share.kind) {
                    Some(ExprKind::Literal(Scalar::Float(share))) => *share,
                    Some(ExprKind::Literal(Scalar::Int(share))) => *share as f64,
                    _ => return Err(Error("internal error: `quantile` without a share".into())),
                };
                if !(0.0..=1.0).contains(&share) {
                    return Err(Error(format!(
                        "the share of `quantile` is from 0 to 1, not {share}"
                    )));
                }
                Acc::Quantile {
                    all: Vec::new(),
                    share,
                }
            }
            (AggFn::Product, _) => Acc::Product {
                products: Vec::new(),
                seen: Vec::new(),
            },
            (AggFn::StringAgg, _) => {
                let separator = match call.arg2.as_ref().map(|separator| &separator.kind) {
                    Some(ExprKind::Literal(Scalar::Str(separator))) => separator.clone(),
                    _ => {
                        return Err(Error(
                            "internal error: `string_agg` without a separator".into(),
                        ));
                    }
                };
                Acc::Strings {
                    texts: Vec::new(),
                    seen: Vec::new(),
                    separator,
                }
            }
            (AggFn::ArgMax | AggFn::ArgMin, _) => {
                let Some(by) = &call.arg2 else {
                    return Err(Error(format!(
                        "internal error: `{}` without a column to go by",
                        call.func.name()
                    )));
                };
                let field = SortField::new(arrow_type(by.ty.dtype));
                Acc::ArgExtreme {
                    keys: Vec::new(),
                    values: Vec::new(),
                    seen: Vec::new(),
                    max: call.func == AggFn::ArgMax,
                    converter: Arc::new(RowConverter::new(vec![field])?),
                }
            }
            (AggFn::Min | AggFn::Max, Some(dtype)) => {
                let max = call.func == AggFn::Max;
                match dtype {
                    DataType::Float => Acc::ExtremeFloat {
                        best: Vec::new(),
                        seen: Vec::new(),
                        max,
                    },
                    DataType::Decimal => Acc::ExtremeDecimal {
                        best: Vec::new(),
                        seen: Vec::new(),
                        max,
                    },
                    DataType::Str => Acc::ExtremeStr {
                        best: Vec::new(),
                        max,
                    },
                    _ => Acc::ExtremeInt {
                        best: Vec::new(),
                        seen: Vec::new(),
                        max,
                    },
                }
            }
            (AggFn::First | AggFn::Last, _) => Acc::Edge {
                values: Vec::new(),
                seen: Vec::new(),
                last: call.func == AggFn::Last,
            },
            (AggFn::Median, _) => Acc::Median(Vec::new()),
            (AggFn::CountDistinct, Some(dtype)) if fixed_width(dtype) => {
                Acc::DistinctFixed(Vec::new())
            }
            (AggFn::CountDistinct, Some(dtype)) => {
                let field = SortField::new(arrow_type(dtype));
                Acc::Distinct {
                    sets: Vec::new(),
                    converter: Arc::new(RowConverter::new(vec![field])?),
                }
            }
            (func, None) => {
                return Err(Error(format!(
                    "internal error: `{}` without an argument",
                    func.name()
                )));
            }
        })
    }

    fn resize(&mut self, groups: usize) {
        match self {
            Acc::Count(counts) => counts.resize(groups, 0),
            Acc::SumInt { sums, seen } => {
                sums.resize(groups, 0);
                seen.resize(groups, false);
            }
            Acc::SumFloat { sums, seen } => {
                sums.resize(groups, 0.0);
                seen.resize(groups, false);
            }
            Acc::Mean { sums, counts } => {
                sums.resize(groups, 0.0);
                counts.resize(groups, 0);
            }
            Acc::Stddev {
                counts,
                means,
                squares,
                ..
            } => {
                counts.resize(groups, 0.0);
                means.resize(groups, 0.0);
                squares.resize(groups, 0.0);
            }
            Acc::ExtremeInt { best, seen, .. } => {
                best.resize(groups, 0);
                seen.resize(groups, false);
            }
            Acc::ExtremeFloat { best, seen, .. } => {
                best.resize(groups, 0.0);
                seen.resize(groups, false);
            }
            Acc::ExtremeStr { best, .. } => best.resize(groups, None),
            Acc::Edge { values, seen, .. } => {
                values.resize(groups, Scalar::Null);
                seen.resize(groups, false);
            }
            Acc::Median(values) => values.resize(groups, Vec::new()),
            Acc::Quantile { all, .. } => all.resize(groups, Vec::new()),
            Acc::Product { products, seen } => {
                products.resize(groups, 1.0);
                seen.resize(groups, false);
            }
            Acc::Strings { texts, seen, .. } => {
                texts.resize(groups, String::new());
                seen.resize(groups, false);
            }
            Acc::ArgExtreme {
                keys, values, seen, ..
            } => {
                keys.resize(groups, Vec::new());
                values.resize(groups, Scalar::Null);
                seen.resize(groups, false);
            }
            Acc::SumDecimal { sums, seen } => {
                sums.resize(groups, 0);
                seen.resize(groups, false);
            }
            Acc::ExtremeDecimal { best, seen, .. } => {
                best.resize(groups, 0);
                seen.resize(groups, false);
            }
            Acc::Pair {
                counts,
                mean_x,
                mean_y,
                cross,
                square_x,
                square_y,
                ..
            } => {
                for sums in [counts, mean_x, mean_y, cross, square_x, square_y] {
                    sums.resize(groups, 0.0);
                }
            }
            Acc::Distinct { sets, .. } => sets.resize(groups, HashSet::new()),
            Acc::DistinctFixed(all) => all.resize(groups, Vec::new()),
        }
    }

    /// Adds the rows of a batch. `groups` gives the group of each row, and `values` the
    /// argument of the aggregate for each row, if it has one.
    pub fn update(
        &mut self,
        values: Option<&ArrayRef>,
        groups: &[u32],
        count: usize,
    ) -> Result<()> {
        self.resize(count);
        let Some(values) = values else {
            let Acc::Count(counts) = self else {
                return Err(Error("internal error: aggregate without values".into()));
            };
            groups.iter().for_each(|&group| counts[group as usize] += 1);
            return Ok(());
        };
        match self {
            Acc::Count(counts) => {
                for (row, &group) in groups.iter().enumerate() {
                    counts[group as usize] += i64::from(values.is_valid(row));
                }
            }
            Acc::SumInt { sums, seen } => {
                let ints = cast(values, &ArrowType::Int64)?;
                each_valid(ints.as_primitive::<Int64Type>(), groups, |group, value| {
                    sums[group] += i128::from(value);
                    seen[group] = true;
                });
            }
            Acc::SumFloat { sums, seen } => {
                each_valid(
                    values.as_primitive::<Float64Type>(),
                    groups,
                    |group, value| {
                        sums[group] += value;
                        seen[group] = true;
                    },
                );
            }
            Acc::Mean { sums, counts } => {
                let floats = as_floats(values)?;
                each_valid(
                    floats.as_primitive::<Float64Type>(),
                    groups,
                    |group, value| {
                        sums[group] += value;
                        counts[group] += 1;
                    },
                );
            }
            Acc::Stddev {
                counts,
                means,
                squares,
                ..
            } => {
                let floats = as_floats(values)?;
                each_valid(
                    floats.as_primitive::<Float64Type>(),
                    groups,
                    |group, value| {
                        counts[group] += 1.0;
                        let delta = value - means[group];
                        means[group] += delta / counts[group];
                        squares[group] += delta * (value - means[group]);
                    },
                );
            }
            Acc::ExtremeInt { best, seen, max } => {
                let ints = cast(values, &ArrowType::Int64)?;
                each_valid(ints.as_primitive::<Int64Type>(), groups, |group, value| {
                    let better = if *max {
                        value > best[group]
                    } else {
                        value < best[group]
                    };
                    if better || !seen[group] {
                        best[group] = value;
                        seen[group] = true;
                    }
                });
            }
            Acc::ExtremeFloat { best, seen, max } => {
                each_valid(
                    values.as_primitive::<Float64Type>(),
                    groups,
                    |group, value| {
                        // "Not a number" compares as the largest, as it sorts; taken as
                        // unordered, it would win or lose by where it stood among the rows.
                        let order = value.total_cmp(&best[group]);
                        let better = if *max { order.is_gt() } else { order.is_lt() };
                        if better || !seen[group] {
                            best[group] = value;
                            seen[group] = true;
                        }
                    },
                );
            }
            Acc::ExtremeStr { best, max } => {
                let strings = values.as_string::<i32>();
                for (row, &group) in groups.iter().enumerate() {
                    if strings.is_null(row) {
                        continue;
                    }
                    let value = strings.value(row);
                    let slot = &mut best[group as usize];
                    let better = match slot.as_deref() {
                        Some(best) => {
                            if *max {
                                value > best
                            } else {
                                value < best
                            }
                        }
                        None => true,
                    };
                    if better {
                        *slot = Some(value.to_string());
                    }
                }
            }
            Acc::Edge {
                values: edge,
                seen,
                last,
            } => {
                // Only one row of each group matters: find it before reading any value.
                const NONE: u32 = u32::MAX;
                let mut chosen = vec![NONE; count];
                for (row, &group) in groups.iter().enumerate() {
                    let slot = &mut chosen[group as usize];
                    if *last || *slot == NONE {
                        *slot = row as u32;
                    }
                }
                for (group, &row) in chosen.iter().enumerate() {
                    if row != NONE && (*last || !seen[group]) {
                        edge[group] = scalar_at(values, row as usize)?;
                        seen[group] = true;
                    }
                }
            }
            Acc::SumDecimal { sums, seen } => {
                let mut overflowed = false;
                each_valid(
                    values.as_primitive::<Decimal128Type>(),
                    groups,
                    |group, value| {
                        let (sum, wrapped) = sums[group].overflowing_add(value);
                        sums[group] = sum;
                        overflowed |= wrapped;
                        seen[group] = true;
                    },
                );
                if overflowed {
                    return Err(Error("decimal overflow".into()));
                }
            }
            Acc::ExtremeDecimal { best, seen, max } => {
                each_valid(
                    values.as_primitive::<Decimal128Type>(),
                    groups,
                    |group, value| {
                        let better = if *max {
                            value > best[group]
                        } else {
                            value < best[group]
                        };
                        if better || !seen[group] {
                            best[group] = value;
                            seen[group] = true;
                        }
                    },
                );
            }
            Acc::Pair { .. } => {
                return Err(Error(
                    "internal error: a pair aggregate needs two columns".into(),
                ));
            }
            Acc::Median(all) | Acc::Quantile { all, .. } => {
                let floats = as_floats(values)?;
                each_valid(
                    floats.as_primitive::<Float64Type>(),
                    groups,
                    |group, value| {
                        all[group].push(value);
                    },
                );
            }
            Acc::Product { products, seen } => {
                let floats = as_floats(values)?;
                each_valid(
                    floats.as_primitive::<Float64Type>(),
                    groups,
                    |group, value| {
                        products[group] *= value;
                        seen[group] = true;
                    },
                );
            }
            Acc::Strings {
                texts,
                seen,
                separator,
            } => {
                let strings = values.as_string::<i32>();
                for (row, &group) in groups.iter().enumerate() {
                    if strings.is_null(row) {
                        continue;
                    }
                    let group = group as usize;
                    if seen[group] {
                        texts[group].push_str(separator);
                    }
                    texts[group].push_str(strings.value(row));
                    seen[group] = true;
                }
            }
            Acc::ArgExtreme { .. } => {
                return Err(Error(
                    "internal error: an aggregate of two columns was given one".into(),
                ));
            }
            Acc::DistinctFixed(all) => {
                let keys = distinct_keys(values)?;
                each_valid(keys.as_primitive::<Int64Type>(), groups, |group, key| {
                    all[group].push(key);
                });
            }
            Acc::Distinct { sets, converter } => {
                let rows = converter.convert_columns(std::slice::from_ref(values))?;
                for (row, &group) in groups.iter().enumerate() {
                    if values.is_valid(row) {
                        let key = rows.row(row);
                        let set = &mut sets[group as usize];
                        if !set.contains(key.as_ref()) {
                            set.insert(key.as_ref().into());
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Adds the rows of a batch to an aggregate of two columns. For the line through the
    /// points, `first` is `y` and `second` is `x`. Rows that lack either value are skipped.
    fn update_pair(
        &mut self,
        first: &ArrayRef,
        second: &ArrayRef,
        groups: &[u32],
        count: usize,
    ) -> Result<()> {
        self.resize(count);
        if let Acc::ArgExtreme {
            keys,
            values,
            seen,
            max,
            converter,
        } = self
        {
            // `second` is the column to go by. A row where it is null is never the best.
            let by = converter.convert_columns(std::slice::from_ref(second))?;
            for (row, &group) in groups.iter().enumerate() {
                if second.is_null(row) {
                    continue;
                }
                let (group, key) = (group as usize, by.row(row));
                let key = key.as_ref();
                // An equal key does not replace: the first row with the best one is kept.
                let better = match (seen[group], *max) {
                    (false, _) => true,
                    (true, true) => key > keys[group].as_slice(),
                    (true, false) => key < keys[group].as_slice(),
                };
                if better {
                    keys[group].clear();
                    keys[group].extend_from_slice(key);
                    values[group] = scalar_at(first, row)?;
                    seen[group] = true;
                }
            }
            return Ok(());
        }
        let Acc::Pair {
            counts,
            mean_x,
            mean_y,
            cross,
            square_x,
            square_y,
            ..
        } = self
        else {
            return Err(Error("internal error: not a pair aggregate".into()));
        };
        let (ys, xs) = (as_floats(first)?, as_floats(second)?);
        let (ys, xs) = (
            ys.as_primitive::<Float64Type>(),
            xs.as_primitive::<Float64Type>(),
        );
        for (row, &group) in groups.iter().enumerate() {
            if xs.is_null(row) || ys.is_null(row) {
                continue;
            }
            let (group, x, y) = (group as usize, xs.value(row), ys.value(row));
            counts[group] += 1.0;
            let (dx, dy) = (x - mean_x[group], y - mean_y[group]);
            mean_x[group] += dx / counts[group];
            mean_y[group] += dy / counts[group];
            cross[group] += dx * (y - mean_y[group]);
            square_x[group] += dx * (x - mean_x[group]);
            square_y[group] += dy * (y - mean_y[group]);
        }
        Ok(())
    }

    /// Adds the state of `other`, which covers later rows. Group `g` of `other` is group
    /// `mapping[g]` here, unless that is `SKIP`, which leaves the group out.
    fn merge(&mut self, other: &Acc, mapping: &[u32], count: usize) -> Result<()> {
        self.resize(count);
        let pairs = || {
            let kept = mapping.iter().enumerate().filter(|(_, to)| **to != SKIP);
            kept.map(|(from, to)| (from, *to as usize))
        };
        match (self, other) {
            (Acc::Count(counts), Acc::Count(other)) => {
                pairs().for_each(|(from, to)| counts[to] += other[from]);
            }
            (
                Acc::SumInt { sums, seen },
                Acc::SumInt {
                    sums: other,
                    seen: other_seen,
                },
            ) => {
                for (from, to) in pairs() {
                    sums[to] += other[from];
                    seen[to] |= other_seen[from];
                }
            }
            (
                Acc::SumFloat { sums, seen },
                Acc::SumFloat {
                    sums: other,
                    seen: other_seen,
                },
            ) => {
                for (from, to) in pairs() {
                    sums[to] += other[from];
                    seen[to] |= other_seen[from];
                }
            }
            (
                Acc::Mean { sums, counts },
                Acc::Mean {
                    sums: other,
                    counts: other_counts,
                },
            ) => {
                for (from, to) in pairs() {
                    sums[to] += other[from];
                    counts[to] += other_counts[from];
                }
            }
            (
                Acc::Stddev {
                    counts,
                    means,
                    squares,
                    ..
                },
                Acc::Stddev {
                    counts: other_counts,
                    means: other_means,
                    squares: other_squares,
                    ..
                },
            ) => {
                for (from, to) in pairs() {
                    let (a, b) = (counts[to], other_counts[from]);
                    if b == 0.0 {
                        continue;
                    }
                    let delta = other_means[from] - means[to];
                    squares[to] += other_squares[from] + delta * delta * a * b / (a + b);
                    means[to] += delta * b / (a + b);
                    counts[to] = a + b;
                }
            }
            (
                Acc::ExtremeInt { best, seen, max },
                Acc::ExtremeInt {
                    best: other,
                    seen: other_seen,
                    ..
                },
            ) => {
                for (from, to) in pairs() {
                    let value = other[from];
                    let better = if *max {
                        value > best[to]
                    } else {
                        value < best[to]
                    };
                    if other_seen[from] && (better || !seen[to]) {
                        best[to] = value;
                        seen[to] = true;
                    }
                }
            }
            (
                Acc::ExtremeFloat { best, seen, max },
                Acc::ExtremeFloat {
                    best: other,
                    seen: other_seen,
                    ..
                },
            ) => {
                for (from, to) in pairs() {
                    let value = other[from];
                    let order = value.total_cmp(&best[to]);
                    let better = if *max { order.is_gt() } else { order.is_lt() };
                    if other_seen[from] && (better || !seen[to]) {
                        best[to] = value;
                        seen[to] = true;
                    }
                }
            }
            (Acc::ExtremeStr { best, max }, Acc::ExtremeStr { best: other, .. }) => {
                for (from, to) in pairs() {
                    let Some(value) = &other[from] else {
                        continue;
                    };
                    let better = match &best[to] {
                        Some(best) => {
                            if *max {
                                value > best
                            } else {
                                value < best
                            }
                        }
                        None => true,
                    };
                    if better {
                        best[to] = Some(value.clone());
                    }
                }
            }
            (
                Acc::Edge { values, seen, last },
                Acc::Edge {
                    values: other,
                    seen: other_seen,
                    ..
                },
            ) => {
                for (from, to) in pairs() {
                    if other_seen[from] && (*last || !seen[to]) {
                        values[to] = other[from].clone();
                        seen[to] = true;
                    }
                }
            }
            (Acc::Median(all), Acc::Median(other))
            | (Acc::Quantile { all, .. }, Acc::Quantile { all: other, .. }) => {
                pairs().for_each(|(from, to)| all[to].extend_from_slice(&other[from]));
            }
            (
                Acc::Product { products, seen },
                Acc::Product {
                    products: other,
                    seen: other_seen,
                },
            ) => {
                for (from, to) in pairs() {
                    products[to] *= other[from];
                    seen[to] |= other_seen[from];
                }
            }
            (
                Acc::Strings {
                    texts,
                    seen,
                    separator,
                },
                Acc::Strings {
                    texts: other,
                    seen: other_seen,
                    ..
                },
            ) => {
                // `other` covers later rows, so its strings go after these.
                for (from, to) in pairs() {
                    if !other_seen[from] {
                        continue;
                    }
                    if seen[to] {
                        texts[to].push_str(separator);
                    }
                    texts[to].push_str(&other[from]);
                    seen[to] = true;
                }
            }
            (
                Acc::ArgExtreme {
                    keys,
                    values,
                    seen,
                    max,
                    ..
                },
                Acc::ArgExtreme {
                    keys: other_keys,
                    values: other,
                    seen: other_seen,
                    ..
                },
            ) => {
                for (from, to) in pairs() {
                    if !other_seen[from] {
                        continue;
                    }
                    let better = match (seen[to], *max) {
                        (false, _) => true,
                        (true, true) => other_keys[from] > keys[to],
                        (true, false) => other_keys[from] < keys[to],
                    };
                    if better {
                        keys[to].clone_from(&other_keys[from]);
                        values[to] = other[from].clone();
                        seen[to] = true;
                    }
                }
            }
            (Acc::Distinct { sets, .. }, Acc::Distinct { sets: other, .. }) => {
                pairs().for_each(|(from, to)| sets[to].extend(other[from].iter().cloned()));
            }
            (Acc::DistinctFixed(all), Acc::DistinctFixed(other)) => {
                pairs().for_each(|(from, to)| all[to].extend_from_slice(&other[from]));
            }
            (
                Acc::SumDecimal { sums, seen },
                Acc::SumDecimal {
                    sums: other,
                    seen: other_seen,
                },
            ) => {
                for (from, to) in pairs() {
                    let sum = sums[to].checked_add(other[from]);
                    sums[to] = sum.ok_or_else(|| Error("decimal overflow".into()))?;
                    seen[to] |= other_seen[from];
                }
            }
            (
                Acc::ExtremeDecimal { best, seen, max },
                Acc::ExtremeDecimal {
                    best: other,
                    seen: other_seen,
                    ..
                },
            ) => {
                for (from, to) in pairs() {
                    let value = other[from];
                    let better = if *max {
                        value > best[to]
                    } else {
                        value < best[to]
                    };
                    if other_seen[from] && (better || !seen[to]) {
                        best[to] = value;
                        seen[to] = true;
                    }
                }
            }
            (
                Acc::Pair {
                    counts,
                    mean_x,
                    mean_y,
                    cross,
                    square_x,
                    square_y,
                    ..
                },
                Acc::Pair {
                    counts: other_counts,
                    mean_x: other_x,
                    mean_y: other_y,
                    cross: other_cross,
                    square_x: other_square_x,
                    square_y: other_square_y,
                    ..
                },
            ) => {
                for (from, to) in pairs() {
                    let (a, b) = (counts[to], other_counts[from]);
                    if b == 0.0 {
                        continue;
                    }
                    let (dx, dy) = (other_x[from] - mean_x[to], other_y[from] - mean_y[to]);
                    let weight = a * b / (a + b);
                    cross[to] += other_cross[from] + dx * dy * weight;
                    square_x[to] += other_square_x[from] + dx * dx * weight;
                    square_y[to] += other_square_y[from] + dy * dy * weight;
                    mean_x[to] += dx * b / (a + b);
                    mean_y[to] += dy * b / (a + b);
                    counts[to] = a + b;
                }
            }
            _ => return Err(Error("internal error: mismatched aggregate states".into())),
        }
        Ok(())
    }

    /// The result for each group. A group with no value to give is null where the result
    /// type allows it; a sum that the type says is never null is 0 there.
    pub fn finish(self, ty: ColType, count: usize) -> Result<ArrayRef> {
        let mut acc = self;
        acc.resize(count);
        let optional = |seen: bool| seen || !ty.nullable;
        Ok(match acc {
            Acc::Count(counts) => Arc::new(Int64Array::from(counts)),
            Acc::SumInt { sums, seen } => {
                let mut narrowed = Vec::with_capacity(sums.len());
                for (sum, seen) in sums.into_iter().zip(seen) {
                    let sum = i64::try_from(sum).map_err(|_| overflow())?;
                    narrowed.push(optional(seen).then_some(sum));
                }
                cast(&Int64Array::from(narrowed), &arrow_type(ty.dtype))?
            }
            Acc::SumFloat { sums, seen } => {
                let sums = sums.into_iter().zip(seen);
                Arc::new(
                    sums.map(|(sum, seen)| optional(seen).then_some(sum))
                        .collect::<Float64Array>(),
                )
            }
            Acc::Mean { sums, counts } => {
                let means = sums.into_iter().zip(counts);
                let means = means.map(|(sum, count)| (count > 0).then(|| sum / count as f64));
                Arc::new(means.collect::<Float64Array>())
            }
            Acc::Stddev {
                func,
                counts,
                squares,
                ..
            } => {
                // A sample of one value has no spread to speak of; a population of one has none.
                let population = matches!(func, AggFn::StddevPop | AggFn::VariancePop);
                let root = matches!(func, AggFn::Stddev | AggFn::StddevPop);
                let spreads = squares.into_iter().zip(counts).map(|(squares, count)| {
                    let divisor = if population { count } else { count - 1.0 };
                    let variance = (divisor > 0.0).then(|| squares / divisor)?;
                    Some(if root { variance.sqrt() } else { variance })
                });
                Arc::new(spreads.collect::<Float64Array>())
            }
            Acc::ExtremeInt { best, seen, .. } => {
                let best = best
                    .into_iter()
                    .zip(seen)
                    .map(|(best, seen)| seen.then_some(best));
                match ty.dtype {
                    DataType::Date => Arc::new(
                        best.map(|days| days.map(|days| days as i32))
                            .collect::<Date32Array>(),
                    ),
                    // Datetimes and durations were compared as their microseconds.
                    dtype => cast(&best.collect::<Int64Array>(), &arrow_type(dtype))?,
                }
            }
            Acc::ExtremeFloat { best, seen, .. } => {
                let best = best
                    .into_iter()
                    .zip(seen)
                    .map(|(best, seen)| seen.then_some(best));
                Arc::new(best.collect::<Float64Array>())
            }
            Acc::ExtremeStr { best, .. } => scalars_to_array(
                best.into_iter()
                    .map(|best| best.map_or(Scalar::Null, |s| Scalar::Str(s.into()))),
                ty.dtype,
            )?,
            Acc::Edge { values, .. } => scalars_to_array(values.into_iter(), ty.dtype)?,
            Acc::SumDecimal { sums, seen } => {
                let sums = sums.into_iter().zip(seen);
                decimals(
                    sums.map(|(sum, seen)| optional(seen).then_some(sum))
                        .collect::<Decimal128Array>(),
                )?
            }
            Acc::ExtremeDecimal { best, seen, .. } => {
                let best = best.into_iter().zip(seen);
                decimals(
                    best.map(|(best, seen)| seen.then_some(best))
                        .collect::<Decimal128Array>(),
                )?
            }
            Acc::Pair {
                func,
                counts,
                mean_x,
                mean_y,
                cross,
                square_x,
                square_y,
            } => {
                let results = (0..count).map(|group| {
                    let (n, cross, square_x) = (counts[group], cross[group], square_x[group]);
                    let value = match func {
                        AggFn::Cov if n >= 2.0 => cross / (n - 1.0),
                        AggFn::Corr if n >= 2.0 => cross / (square_x * square_y[group]).sqrt(),
                        AggFn::Slope if n >= 2.0 => cross / square_x,
                        AggFn::Intercept if n >= 2.0 => {
                            mean_y[group] - cross / square_x * mean_x[group]
                        }
                        _ => f64::NAN,
                    };
                    // No rows, or no spread to divide by: there is no answer.
                    value.is_finite().then_some(value)
                });
                Arc::new(results.collect::<Float64Array>())
            }
            Acc::Median(all) => {
                let medians = all.into_iter().map(|mut values| {
                    values.sort_unstable_by(f64::total_cmp);
                    let middle = values.len() / 2;
                    match values.len() {
                        0 => None,
                        len if len % 2 == 1 => Some(values[middle]),
                        _ => Some((values[middle - 1] + values[middle]) / 2.0),
                    }
                });
                Arc::new(medians.collect::<Float64Array>())
            }
            Acc::Quantile { all, share } => {
                let quantiles = all.into_iter().map(|mut values| {
                    values.sort_unstable_by(f64::total_cmp);
                    // The position a share of the way from the first value to the last,
                    // and between two values in proportion to how far it is past the lower.
                    let position = share * (values.len().checked_sub(1)? as f64);
                    let below = position.floor() as usize;
                    let above = (below + 1).min(values.len() - 1);
                    let past = position - below as f64;
                    Some(values[below] + (values[above] - values[below]) * past)
                });
                Arc::new(quantiles.collect::<Float64Array>())
            }
            Acc::Product { products, seen } => {
                let products = products.into_iter().zip(seen);
                let products: Float64Array = products
                    .map(|(product, seen)| optional(seen).then_some(product))
                    .collect();
                Arc::new(products)
            }
            Acc::Strings { texts, seen, .. } => {
                let texts = texts.into_iter().zip(seen);
                let texts: StringArray = texts.map(|(text, seen)| seen.then_some(text)).collect();
                Arc::new(texts)
            }
            Acc::ArgExtreme { values, .. } => scalars_to_array(values.into_iter(), ty.dtype)?,
            Acc::Distinct { sets, .. } => Arc::new(
                sets.iter()
                    .map(|set| set.len() as i64)
                    .collect::<Int64Array>(),
            ),
            Acc::DistinctFixed(all) => {
                let distinct = |mut keys: Vec<i64>| {
                    keys.sort_unstable();
                    keys.dedup();
                    keys.len() as i64
                };
                let counts: Vec<i64> = all.into_par_iter().map(distinct).collect();
                Arc::new(Int64Array::from(counts))
            }
        })
    }
}

/// Aggregates `values` within the groups given by `groups`, and returns one value per group.
pub fn aggregate_groups(
    call: &AggCall,
    values: Option<&ArrayRef>,
    groups: &[u32],
    count: usize,
) -> Result<ArrayRef> {
    let mut acc = Acc::new(call)?;
    acc.update(values, groups, count)?;
    Ok(canonical(acc.finish(call.ty, count)?))
}

type Named<T> = [(Arc<str>, T)];

/// The groups and aggregate states of some of the input.
struct State {
    converter: Option<Arc<RowConverter>>,
    groups: Groups,
    accs: Vec<Acc>,
}

impl State {
    fn new(converter: &Option<Arc<RowConverter>>, aggs: &Named<AggCall>) -> Result<State> {
        let accs = aggs.iter().map(|(_, call)| Acc::new(call));
        Ok(State {
            converter: converter.clone(),
            groups: Groups::default(),
            accs: accs.collect::<Result<_>>()?,
        })
    }

    fn update(
        &mut self,
        batch: &RecordBatch,
        keys: &Named<Expr>,
        aggs: &Named<AggCall>,
    ) -> Result<()> {
        let rows = batch.num_rows();
        let groups: Vec<u32> = match &self.converter {
            Some(converter) => {
                let mut columns = Vec::with_capacity(keys.len());
                for (_, key) in keys {
                    columns.push(eval_array(key, batch)?);
                }
                let key_rows = converter.convert_columns(&columns)?;
                let ids = (0..rows).map(|row| self.groups.intern(key_rows.row(row).as_ref()));
                ids.collect()
            }
            // Without keys the whole input is one group.
            None => vec![0; rows],
        };
        let count = self.count();
        for (acc, (_, call)) in self.accs.iter_mut().zip(aggs) {
            let values = match &call.arg {
                Some(arg) => Some(eval_array(arg, batch)?),
                None => None,
            };
            match (&values, &call.arg2) {
                (Some(first), Some(second)) if call.func.extra() == Extra::Column => {
                    let second = eval_array(second, batch)?;
                    acc.update_pair(first, &second, &groups, count)?;
                }
                _ => acc.update(values.as_ref(), &groups, count)?,
            }
        }
        Ok(())
    }

    fn count(&self) -> usize {
        match self.converter {
            Some(_) => self.groups.len(),
            None => 1,
        }
    }

    /// Merges in the groups of `other` that `wanted` selects. `other` covers rows that come
    /// after this state's rows. Returns the ids, in `other`, of the groups that are new here.
    fn merge(&mut self, other: &State, wanted: impl Fn(u32) -> bool) -> Result<Vec<u32>> {
        let mut added = Vec::new();
        let mapping: Vec<u32> = match self.converter {
            Some(_) => (0..other.groups.len() as u32)
                .map(|id| {
                    if !wanted(id) {
                        return SKIP;
                    }
                    let known = self.groups.len();
                    let group = self.groups.intern(other.groups.key(id));
                    if group as usize == known {
                        added.push(id);
                    }
                    group
                })
                .collect(),
            None => vec![0],
        };
        let count = self.count();
        for (acc, other) in self.accs.iter_mut().zip(&other.accs) {
            acc.merge(other, &mapping, count)?;
        }
        Ok(added)
    }

    /// The result: one row per group, in group order.
    fn finish(self, aggs: &Named<AggCall>, schema: &Schema) -> Result<RecordBatch> {
        let count = self.count();
        let mut columns: Vec<ArrayRef> = match &self.converter {
            Some(converter) => {
                let parser = converter.parser();
                let keys = (0..count as u32).map(|id| parser.parse(self.groups.key(id)));
                converter.convert_rows(keys)?
            }
            None => Vec::new(),
        };
        for (acc, (_, call)) in self.accs.into_iter().zip(aggs) {
            columns.push(canonical(acc.finish(call.ty, count)?));
        }
        make_batch(schema, columns, count)
    }
}

/// Marks a group that a merge leaves out.
const SKIP: u32 = u32::MAX;

/// How many batches are read before they are aggregated.
const WINDOW: usize = 64;

/// How many consecutive batches one core aggregates into a partial result.
const RUN: usize = 4;

/// With fewer groups than this in the partial results, merging them one after another is
/// faster than dividing the work.
const PARTITION_MIN: usize = 50_000;

pub fn aggregate(
    mut input: BatchIter,
    keys: &Named<Expr>,
    aggs: &Named<AggCall>,
    schema: &Schema,
) -> Result<BatchIter> {
    let converter = match keys {
        [] => None,
        _ => {
            let fields = keys
                .iter()
                .map(|(_, key)| SortField::new(arrow_type(key.ty.dtype)));
            Some(Arc::new(RowConverter::new(fields.collect())?))
        }
    };
    let new_state = || State::new(&converter, aggs);
    // The input is taken a window at a time, so that it is never all in memory at once. Its
    // batches are aggregated in runs, each on its own core. Runs have a fixed size: how the
    // rows are grouped into partial results, and with it the rounding of a floating-point
    // sum, must not depend on the machine.
    let mut runs: Vec<State> = Vec::new();
    loop {
        let batches: Vec<RecordBatch> = input.by_ref().take(WINDOW).collect::<Result<_>>()?;
        if batches.is_empty() {
            break;
        }
        let aggregated = batches.par_chunks(RUN).map(|run| -> Result<State> {
            let mut state = new_state()?;
            for batch in run {
                state.update(batch, keys, aggs)?;
            }
            Ok(state)
        });
        runs.extend(aggregated.collect::<Result<Vec<_>>>()?);
    }

    let groups: usize = runs.iter().map(|run| run.groups.len()).sum();
    let partitions = rayon::current_num_threads();
    let batch = if groups < PARTITION_MIN || partitions == 1 {
        let mut total = new_state()?;
        for run in &runs {
            total.merge(run, |_| true)?;
        }
        total.finish(aggs, schema)?
    } else {
        merge_partitioned(&runs, partitions, &new_state, aggs, schema)?
    };
    Ok(Box::new(std::iter::once(Ok(batch))))
}

/// Merges runs that hold many groups. The groups are divided by a hash of their key, and
/// each part is merged on its own core. A group is still merged from the runs in order, so
/// the result is what merging run after run gives; the rows are then put back in the order
/// in which the groups first appear in the input.
fn merge_partitioned(
    runs: &[State],
    partitions: usize,
    new_state: &(impl Fn() -> Result<State> + Sync),
    aggs: &Named<AggCall>,
    schema: &Schema,
) -> Result<RecordBatch> {
    let hasher = DefaultHashBuilder::default();
    let part_of: Vec<Vec<u32>> = runs
        .par_iter()
        .map(|run| {
            let ids = 0..run.groups.len() as u32;
            let part = |id| (hasher.hash_one(run.groups.key(id)) >> 32) as usize % partitions;
            ids.map(|id| part(id) as u32).collect()
        })
        .collect();

    // For each part: its rows, and for each row where its group first appears, as the number
    // of the run and the group's id in that run.
    let merged: Vec<(RecordBatch, Vec<u64>)> = (0..partitions as u32)
        .into_par_iter()
        .map(|part| -> Result<(RecordBatch, Vec<u64>)> {
            let mut total = new_state()?;
            let mut first_seen = Vec::new();
            for (index, run) in runs.iter().enumerate() {
                let added = total.merge(run, |id| part_of[index][id as usize] == part)?;
                first_seen.extend(added.iter().map(|id| (index as u64) << 32 | u64::from(*id)));
            }
            Ok((total.finish(aggs, schema)?, first_seen))
        })
        .collect::<Result<_>>()?;

    let batches: Vec<&RecordBatch> = merged.iter().map(|(batch, _)| batch).collect();
    let all = arrow::compute::concat_batches(&batches[0].schema(), batches)?;
    let mut order: Vec<(u64, u32)> = merged
        .iter()
        .flat_map(|(_, first_seen)| first_seen)
        .zip(0u32..)
        .map(|(first_seen, row)| (*first_seen, row))
        .collect();
    order.par_sort_unstable();
    let rows = UInt32Array::from_iter_values(order.into_iter().map(|(_, row)| row));
    Ok(arrow::compute::take_record_batch(&all, &rows)?)
}
