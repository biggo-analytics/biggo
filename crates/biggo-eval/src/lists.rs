//! The functions of lists and maps that need nothing but their arguments.

use std::cmp::Ordering;
use std::rc::Rc;
use std::sync::Arc;

use biggo_plan::scalar;
use biggo_types::hir::ListOp;

use crate::value::{Decimal, Key, Map, Record, Value};

/// How two values of one type order. Floats go by `scalar::float_cmp`, as they do in a
/// table, and a null comes after every value.
fn order(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Greater,
        (_, Value::Null) => Ordering::Less,
        (Value::Int(a), Value::Int(b)) => a.cmp(b),
        (Value::Float(a), Value::Float(b)) => scalar::float_cmp(*a, *b),
        (Value::Int(a), Value::Float(b)) => scalar::float_cmp(*a as f64, *b),
        (Value::Float(a), Value::Int(b)) => scalar::float_cmp(*a, *b as f64),
        (Value::Decimal(a), Value::Decimal(b)) => a.scaled().cmp(&b.scaled()),
        (Value::Str(a), Value::Str(b)) => a.cmp(b),
        (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
        (Value::Date(a), Value::Date(b)) => a.cmp(b),
        (Value::DateTime(a), Value::DateTime(b)) => a.cmp(b),
        (Value::Duration(a), Value::Duration(b)) => a.cmp(b),
        // The checker gives these functions values of one ordered type.
        _ => Ordering::Equal,
    }
}

/// Like `order`, with the largest value first. A null still comes last.
fn order_down(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Null, _) | (_, Value::Null) => order(a, b),
        _ => order(b, a),
    }
}

fn list(value: &Value) -> &[Value] {
    match value {
        Value::List(items) => items,
        other => unreachable!("the checker passes a list here, not {other}"),
    }
}

fn int(value: &Value) -> i64 {
    match value {
        Value::Int(number) => *number,
        other => unreachable!("the checker passes an int here, not {other}"),
    }
}

fn map(value: &Value) -> &Map {
    match value {
        Value::Map(map) => map,
        other => unreachable!("the checker passes a map here, not {other}"),
    }
}

/// The part of a list from `start`, counted from the end when negative, for `length` items.
/// What lies outside the list is left out.
fn slice(items: &[Value], start: i64, length: i64) -> &[Value] {
    let count = items.len() as i64;
    let start = match start < 0 {
        true => count.saturating_add(start).max(0),
        false => start.min(count),
    };
    let end = start.saturating_add(length.max(0)).min(count);
    &items[start as usize..end as usize]
}

fn sum(items: &[Value]) -> Result<Value, String> {
    let mut values = items.iter().filter(|item| !matches!(item, Value::Null));
    // The first value says what is being added; an empty list of whole numbers sums to 0.
    Ok(match values.next() {
        None => match items.is_empty() {
            true => Value::Int(0),
            false => Value::Null,
        },
        Some(Value::Float(first)) => {
            let rest = values.map(|value| match value {
                Value::Float(value) => *value,
                Value::Int(value) => *value as f64,
                _ => 0.0,
            });
            Value::Float(rest.fold(*first, |total, value| total + value))
        }
        Some(Value::Decimal(first)) => {
            let mut total = first.scaled();
            for value in values {
                if let Value::Decimal(value) = value {
                    total = total
                        .checked_add(value.scaled())
                        .filter(|total| total.unsigned_abs() < 10u128.pow(38))
                        .ok_or("decimal overflow")?;
                }
            }
            Value::Decimal(Decimal::new(total))
        }
        Some(Value::Duration(first)) => {
            let mut total = *first;
            for value in values {
                if let Value::Duration(value) = value {
                    total = total.checked_add(*value).ok_or("duration overflow")?;
                }
            }
            Value::Duration(total)
        }
        Some(first) => {
            let mut total = int(first);
            for value in values {
                total = total.checked_add(int(value)).ok_or("integer overflow")?;
            }
            Value::Int(total)
        }
    })
}

/// Applies a function to its arguments, which the checker has typed.
pub fn list_op(op: ListOp, args: &[Value]) -> Result<Value, String> {
    let collected = |items: Vec<Value>| Value::List(items.into());
    Ok(match op {
        ListOp::Sort => {
            let mut items = list(&args[0]).to_vec();
            // The sort is stable, so items that compare equal keep their order.
            match &args[1] {
                Value::Bool(true) => items.sort_by(order_down),
                _ => items.sort_by(order),
            }
            collected(items)
        }
        ListOp::SortBy => {
            let (items, keys) = (list(&args[0]), list(&args[1]));
            let mut positions: Vec<usize> = (0..items.len()).collect();
            positions.sort_by(|a, b| order(&keys[*a], &keys[*b]));
            collected(positions.into_iter().map(|at| items[at].clone()).collect())
        }
        ListOp::Sum => {
            let items = list(&args[0]);
            match sum(items)? {
                // A list with nothing but nulls adds up to nothing of its own type.
                Value::Null => Value::Int(0),
                total => total,
            }
        }
        ListOp::Mean => {
            let numbers = list(&args[0]).iter().filter_map(|item| match item {
                Value::Int(value) => Some(*value as f64),
                Value::Float(value) => Some(*value),
                Value::Decimal(value) => Some(scalar::decimal_to_float(value.scaled())),
                _ => None,
            });
            let (mut total, mut count) = (0.0, 0u64);
            for number in numbers {
                total += number;
                count += 1;
            }
            match count {
                0 => Value::Null,
                count => Value::Float(total / count as f64),
            }
        }
        ListOp::Min | ListOp::Max => {
            let values = list(&args[0])
                .iter()
                .filter(|item| !matches!(item, Value::Null));
            // Of values that compare equal, the first is given.
            let best = values.reduce(|best, value| {
                let better = match op {
                    ListOp::Min => order(value, best).is_lt(),
                    _ => order(value, best).is_gt(),
                };
                if better { value } else { best }
            });
            best.cloned().unwrap_or(Value::Null)
        }
        ListOp::Contains => {
            let found = list(&args[0])
                .iter()
                .any(|item| order(item, &args[1]).is_eq());
            Value::Bool(found)
        }
        ListOp::IndexOf => {
            let at = list(&args[0])
                .iter()
                .position(|item| order(item, &args[1]).is_eq());
            at.map_or(Value::Null, |at| Value::Int(at as i64))
        }
        ListOp::Join => {
            let Value::Str(separator) = &args[1] else {
                unreachable!("the checker passes a string here");
            };
            let parts = list(&args[0]).iter().filter_map(|item| match item {
                Value::Str(text) => Some(&**text),
                _ => None,
            });
            Value::Str(parts.collect::<Vec<&str>>().join(separator).into())
        }
        ListOp::Reverse => collected(list(&args[0]).iter().rev().cloned().collect()),
        ListOp::Distinct => {
            let items = list(&args[0]);
            // The positions in sorted order bring equal items together; of each such run
            // the first to appear is kept, and what is kept stays in its order.
            let mut positions: Vec<usize> = (0..items.len()).collect();
            positions.sort_by(|a, b| order(&items[*a], &items[*b]));
            let mut kept = vec![false; items.len()];
            for (index, at) in positions.iter().enumerate() {
                let repeats = index > 0 && order(&items[positions[index - 1]], &items[*at]).is_eq();
                kept[*at] = !repeats;
            }
            let kept = items.iter().zip(kept).filter(|(_, kept)| *kept);
            collected(kept.map(|(item, _)| item.clone()).collect())
        }
        ListOp::Take => collected(slice(list(&args[0]), 0, int(&args[1])).to_vec()),
        ListOp::Skip => {
            let items = list(&args[0]);
            let skipped = int(&args[1]).clamp(0, items.len() as i64) as usize;
            collected(items[skipped..].to_vec())
        }
        ListOp::Slice => collected(slice(list(&args[0]), int(&args[1]), int(&args[2])).to_vec()),
        ListOp::First => list(&args[0]).first().cloned().unwrap_or(Value::Null),
        ListOp::Last => list(&args[0]).last().cloned().unwrap_or(Value::Null),
        ListOp::Flatten => {
            let inner = list(&args[0])
                .iter()
                .flat_map(|items| list(items).iter().cloned());
            collected(inner.collect())
        }
        ListOp::NotEmpty => Value::Bool(!list(&args[0]).is_empty()),
        ListOp::SameLength => Value::Bool(list(&args[0]).len() == list(&args[1]).len()),
        ListOp::Remove => {
            let gone = Key::from_value(&args[1]);
            let mut rest = Map::default();
            for (key, value) in map(&args[0]).entries() {
                if Some(key) != gone.as_ref() {
                    rest.insert(key.clone(), value.clone());
                }
            }
            Value::Map(Rc::new(rest))
        }
        ListOp::Merge => {
            // The entries of the second map replace those of the first with the same key.
            let mut merged = map(&args[0]).clone();
            for (key, value) in map(&args[1]).entries() {
                merged.insert(key.clone(), value.clone());
            }
            Value::Map(Rc::new(merged))
        }
        ListOp::Entries => {
            let names: Arc<[Arc<str>]> = Arc::new(["key".into(), "value".into()]);
            let entries = map(&args[0]).entries().iter().map(|(key, value)| {
                Value::Record(Rc::new(Record {
                    names: names.clone(),
                    values: vec![key.to_value(), value.clone()],
                }))
            });
            collected(entries.collect())
        }
    })
}
