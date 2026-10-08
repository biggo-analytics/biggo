//! Hash join. The right input is loaded into a hash table keyed by its join columns; the
//! batches of the left input then look their rows up in it, several batches at a time.

use std::hash::BuildHasher;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use arrow::array::{Array, ArrayRef, BooleanArray, RecordBatch, UInt32Array, new_null_array};
use arrow::compute::{filter_record_batch, take};
use arrow::row::{RowConverter, Rows, SortField};
use biggo_plan::{Expr, Join, JoinColumn, JoinKind, Schema};
use hashbrown::{DefaultHashBuilder, HashTable};

use crate::convert::{arrow_type, batch as make_batch};
use crate::expr::eval_array;
use crate::ops::{concat, par_map};
use crate::{BatchIter, Error, Result};

const END: u32 = u32::MAX;

/// The right side of a join, indexed by key.
struct Table {
    batch: RecordBatch,
    converter: RowConverter,
    keys: Rows,
    hasher: DefaultHashBuilder,
    /// For each distinct key, the first right row that has it.
    heads: HashTable<u32>,
    /// For each right row, the next row with the same key, or `END`.
    next: Vec<u32>,
    /// Which right rows have found a partner; a full join reports the rest at the end.
    matched: Vec<AtomicBool>,
}

/// Evaluates the key expressions for `batch`: the key of each row in comparable form, and
/// whether the row has a null key, which matches nothing.
fn keys_of<'e>(
    converter: &RowConverter,
    exprs: impl Iterator<Item = &'e Expr>,
    batch: &RecordBatch,
) -> Result<(Rows, Vec<bool>)> {
    let mut columns = Vec::new();
    let mut null = vec![false; batch.num_rows()];
    for expr in exprs {
        let column = eval_array(expr, batch)?;
        if let Some(nulls) = column.nulls() {
            null.iter_mut()
                .enumerate()
                .for_each(|(row, null)| *null |= nulls.is_null(row));
        }
        columns.push(column);
    }
    Ok((converter.convert_columns(&columns)?, null))
}

impl Table {
    fn build(batch: RecordBatch, join: &Join) -> Result<Table> {
        let fields = join
            .on
            .iter()
            .map(|(_, right)| SortField::new(arrow_type(right.ty.dtype)));
        let converter = RowConverter::new(fields.collect())?;
        let (keys, null) = keys_of(&converter, join.on.iter().map(|(_, right)| right), &batch)?;
        let hasher = DefaultHashBuilder::default();
        let rows = batch.num_rows();
        let mut heads: HashTable<u32> = HashTable::new();
        let mut next = vec![END; rows];
        // Going backwards makes each chain list its rows in input order.
        for row in (0..rows as u32).rev() {
            if null[row as usize] {
                continue;
            }
            let key = keys.row(row as usize);
            let hash = hasher.hash_one(key.as_ref());
            match heads.find_mut(hash, |&head| keys.row(head as usize) == key) {
                Some(head) => {
                    next[row as usize] = *head;
                    *head = row;
                }
                None => {
                    heads.insert_unique(hash, row, |&head| {
                        hasher.hash_one(keys.row(head as usize).as_ref())
                    });
                }
            }
        }
        let matched = (0..rows).map(|_| AtomicBool::new(false)).collect();
        Ok(Table {
            batch,
            converter,
            keys,
            hasher,
            heads,
            next,
            matched,
        })
    }

    /// The right rows that have the key `key`, in input order.
    fn matches<'t>(&'t self, key: &[u8]) -> impl Iterator<Item = u32> + 't {
        let hash = self.hasher.hash_one(key);
        let head = self
            .heads
            .find(hash, |&head| self.keys.row(head as usize).as_ref() == key);
        let mut row = head.copied().unwrap_or(END);
        std::iter::from_fn(move || {
            (row != END).then(|| {
                let current = row;
                row = self.next[current as usize];
                current
            })
        })
    }
}

/// Assembles output columns from left and right rows that are already lined up. A side that
/// has no row for a position holds nulls there.
fn assemble(
    join: &Join,
    left: impl Fn(&str) -> Result<ArrayRef>,
    right: impl Fn(&str) -> Result<ArrayRef>,
    key_from_right: bool,
    rows: usize,
) -> Result<RecordBatch> {
    let mut columns = Vec::with_capacity(join.columns.len());
    for (_, source) in &join.columns {
        columns.push(match source {
            JoinColumn::Left(name) => left(name)?,
            JoinColumn::Right(name) => right(name)?,
            JoinColumn::Key(left_name, right_name) => match key_from_right {
                false => left(left_name)?,
                true => right(right_name)?,
            },
        });
    }
    make_batch(&join.schema, columns, rows)
}

fn column(batch: &RecordBatch, name: &str) -> Result<ArrayRef> {
    let column = batch.column_by_name(name).cloned();
    column.ok_or_else(|| Error(format!("internal error: no column `{name}` to join")))
}

fn probe(table: &Table, join: &Join, batch: &RecordBatch) -> Result<RecordBatch> {
    let left_keys = join.on.iter().map(|(left, _)| left);
    let (keys, null) = keys_of(&table.converter, left_keys, batch)?;
    let rows = batch.num_rows();

    if matches!(join.kind, JoinKind::Semi | JoinKind::Anti) {
        let wanted = join.kind == JoinKind::Semi;
        let keep: BooleanArray = (0..rows)
            .map(|row| {
                let found = !null[row] && table.matches(keys.row(row).as_ref()).next().is_some();
                Some(found == wanted)
            })
            .collect();
        let kept = filter_record_batch(batch, &keep)?;
        let no_right = |_: &str| {
            Err(Error(
                "internal error: a semi join has no right columns".into(),
            ))
        };
        return assemble(
            join,
            |name| column(&kept, name),
            no_right,
            false,
            kept.num_rows(),
        );
    }

    let keeps_unmatched = matches!(join.kind, JoinKind::Left | JoinKind::Full);
    let track = join.kind == JoinKind::Full;
    let mut left_rows: Vec<u32> = Vec::with_capacity(rows);
    let mut right_rows: Vec<Option<u32>> = Vec::with_capacity(rows);
    for (row, null) in null.iter().enumerate() {
        let before = left_rows.len();
        if !null {
            for partner in table.matches(keys.row(row).as_ref()) {
                left_rows.push(row as u32);
                right_rows.push(Some(partner));
                if track {
                    table.matched[partner as usize].store(true, Ordering::Relaxed);
                }
            }
        }
        if keeps_unmatched && left_rows.len() == before {
            left_rows.push(row as u32);
            right_rows.push(None);
        }
    }
    let count = left_rows.len();
    let left_rows = UInt32Array::from(left_rows);
    let right_rows = UInt32Array::from(right_rows);
    assemble(
        join,
        |name| Ok(take(&column(batch, name)?, &left_rows, None)?),
        |name| Ok(take(&column(&table.batch, name)?, &right_rows, None)?),
        false,
        count,
    )
}

/// The right rows of a full join that matched no left row, with nulls for the left columns.
fn unmatched(table: &Table, join: &Join, left_schema: &Schema) -> Result<RecordBatch> {
    let rows = table.matched.iter().enumerate();
    let rows = rows.filter(|(_, matched)| !matched.load(Ordering::Relaxed));
    let rows: UInt32Array = rows.map(|(row, _)| row as u32).collect::<Vec<_>>().into();
    let count = rows.len();
    let null_left = |name: &str| {
        let field = left_schema.field(name);
        let field = field.ok_or_else(|| Error(format!("internal error: no column `{name}`")))?;
        Ok(new_null_array(&arrow_type(field.ty.dtype), count))
    };
    assemble(
        join,
        null_left,
        |name| Ok(take(&column(&table.batch, name)?, &rows, None)?),
        true,
        count,
    )
}

pub fn join(left: BatchIter, right: BatchIter, join: &Join) -> Result<BatchIter> {
    let right = concat(right, &join.right.schema())?;
    let table = Arc::new(Table::build(right, join)?);
    let join = Arc::new(join.clone());
    let probed = {
        let (table, join) = (table.clone(), join.clone());
        par_map(left, move |batch| probe(&table, &join, &batch))
    };
    if join.kind != JoinKind::Full {
        return Ok(probed);
    }
    // The leftover right rows are only known once every left batch has been probed, which
    // is the case when the chained iterator gets to them.
    let left_schema = join.left.schema();
    let rest = std::iter::once_with(move || unmatched(&table, &join, &left_schema));
    let rest = rest.filter(|batch| batch.as_ref().map_or(true, |batch| batch.num_rows() > 0));
    Ok(Box::new(probed.chain(rest)))
}
