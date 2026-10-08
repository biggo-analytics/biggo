//! Rewrites a plan into an equivalent one that does less work: constants are folded, filters
//! move toward the scans, limits shrink sorts and scans, and unused columns are never read.

use std::collections::BTreeSet;
use std::sync::Arc;

use biggo_syntax::ast::BinaryOp;

use crate::expr::{Expr, ExprKind, SortKey};
use crate::plan::{Join, JoinColumn, JoinKind, Plan, Scan, schema_of};
use crate::schema::{ColType, DataType, Field, Scalar, Schema};

/// Computes the value of an expression that reads no columns, or `None` if evaluating it fails.
pub type Fold<'a> = &'a dyn Fn(&Expr) -> Option<Scalar>;

pub fn optimize(plan: &Arc<Plan>, fold: Fold) -> Arc<Plan> {
    let plan = simplify_plan(plan, fold);
    let plan = push_filters(&plan, Vec::new());
    let plan = push_limits(&plan);
    let required: BTreeSet<Arc<str>> = plan.schema().names().cloned().collect();
    let pruned = prune(&plan, &required);
    merge_projects(&restore_schema(pruned, &plan.schema()))
}

type Named = Vec<(Arc<str>, Expr)>;

fn map_named(columns: &Named, f: &mut impl FnMut(&Expr) -> Expr) -> Named {
    let mapped = columns.iter().map(|(name, expr)| (name.clone(), f(expr)));
    mapped.collect()
}

fn map_keys(keys: &[SortKey], f: &mut impl FnMut(&Expr) -> Expr) -> Vec<SortKey> {
    let mapped = keys.iter().map(|key| SortKey {
        expr: f(&key.expr),
        descending: key.descending,
    });
    mapped.collect()
}

/// Rebuilds `plan` with `input` applied to each input and `expr` to each expression.
fn map_plan(
    plan: &Plan,
    input: &mut impl FnMut(&Arc<Plan>) -> Arc<Plan>,
    expr: &mut impl FnMut(&Expr) -> Expr,
) -> Plan {
    match plan {
        Plan::Scan(scan) => Plan::Scan(Scan {
            filters: scan.filters.iter().map(expr).collect(),
            ..scan.clone()
        }),
        Plan::Memory(memory) => Plan::Memory(memory.clone()),
        Plan::Filter {
            input: source,
            predicate,
        } => Plan::Filter {
            input: input(source),
            predicate: expr(predicate),
        },
        Plan::Project {
            input: source,
            columns,
            schema,
        } => Plan::Project {
            input: input(source),
            columns: map_named(columns, expr),
            schema: schema.clone(),
        },
        Plan::Sort {
            input: source,
            keys,
            fetch,
        } => Plan::Sort {
            input: input(source),
            keys: map_keys(keys, expr),
            fetch: *fetch,
        },
        Plan::Limit {
            input: source,
            skip,
            fetch,
        } => Plan::Limit {
            input: input(source),
            skip: *skip,
            fetch: *fetch,
        },
        Plan::Group {
            input: source,
            keys,
        } => Plan::Group {
            input: input(source),
            keys: map_named(keys, expr),
        },
        Plan::Aggregate {
            input: source,
            keys,
            aggs,
            schema,
        } => {
            let mut aggs = aggs.clone();
            for (_, call) in &mut aggs {
                call.arg = call.arg.as_ref().map(&mut *expr);
                call.arg2 = call.arg2.as_ref().map(&mut *expr);
            }
            Plan::Aggregate {
                input: input(source),
                keys: map_named(keys, expr),
                aggs,
                schema: schema.clone(),
            }
        }
        Plan::Join(join) => Plan::Join(Join {
            left: input(&join.left),
            right: input(&join.right),
            on: join.on.iter().map(|(l, r)| (expr(l), expr(r))).collect(),
            ..join.clone()
        }),
        Plan::Window {
            input: source,
            partition,
            order,
            funcs,
            schema,
        } => {
            let mut funcs = funcs.clone();
            for (_, call) in &mut funcs {
                call.arg = call.arg.as_ref().map(&mut *expr);
            }
            Plan::Window {
                input: input(source),
                partition: partition.iter().map(&mut *expr).collect(),
                order: map_keys(order, expr),
                funcs,
                schema: schema.clone(),
            }
        }
        Plan::Union { inputs } => Plan::Union {
            inputs: inputs.iter().map(input).collect(),
        },
        Plan::Unpivot {
            input: source,
            columns,
            name,
            value,
            schema,
        } => Plan::Unpivot {
            input: input(source),
            columns: columns.clone(),
            name: name.clone(),
            value: value.clone(),
            schema: schema.clone(),
        },
        Plan::Explode {
            input: source,
            column,
            separator,
        } => Plan::Explode {
            input: input(source),
            column: column.clone(),
            separator: separator.clone(),
        },
    }
}

fn simplify_plan(plan: &Arc<Plan>, fold: Fold) -> Arc<Plan> {
    let simplified = map_plan(plan, &mut |input| simplify_plan(input, fold), &mut |expr| {
        simplify(expr, fold)
    });
    match simplified {
        // A filter that keeps every row does nothing.
        Plan::Filter { input, predicate } if is_literal(&predicate, true) => input,
        other => Arc::new(other),
    }
}

fn is_literal(expr: &Expr, value: bool) -> bool {
    matches!(&expr.kind, ExprKind::Literal(Scalar::Bool(b)) if *b == value)
}

/// Folds constant subexpressions and removes logical operands that cannot change the result.
fn simplify(expr: &Expr, fold: Fold) -> Expr {
    let expr = expr.map_children(&mut |child| simplify(child, fold));
    if let ExprKind::Binary(op @ (BinaryOp::And | BinaryOp::Or), left, right) = &expr.kind {
        // `true` is the neutral operand of `and` and `false` that of `or`; the other value
        // settles the result. This holds for null operands as well.
        let neutral = *op == BinaryOp::And;
        for (operand, other) in [(left, right), (right, left)] {
            if is_literal(operand, neutral) {
                return Expr::new(other.kind.clone(), expr.ty);
            }
            if is_literal(operand, !neutral) {
                return Expr::literal(Scalar::Bool(!neutral), expr.ty);
            }
        }
    }
    let mut constant = !matches!(
        expr.kind,
        ExprKind::Literal(_) | ExprKind::Column(_) | ExprKind::Param(_)
    );
    expr.for_each_child(&mut |child| constant &= matches!(child.kind, ExprKind::Literal(_)));
    match constant.then(|| fold(&expr)).flatten() {
        Some(value) => Expr::literal(value, expr.ty),
        None => expr,
    }
}

fn conjuncts(expr: Expr, out: &mut Vec<Expr>) {
    match expr.kind {
        ExprKind::Binary(BinaryOp::And, left, right) => {
            conjuncts(*left, out);
            conjuncts(*right, out);
        }
        _ => out.push(expr),
    }
}

fn and_all(mut predicates: Vec<Expr>) -> Option<Expr> {
    let mut result = predicates.pop()?;
    while let Some(predicate) = predicates.pop() {
        let nullable = predicate.ty.nullable || result.ty.nullable;
        let ty = ColType::new(DataType::Bool, nullable);
        let kind = ExprKind::Binary(BinaryOp::And, Box::new(predicate), Box::new(result));
        result = Expr::new(kind, ty);
    }
    Some(result)
}

fn filtered(plan: Plan, predicates: Vec<Expr>) -> Arc<Plan> {
    let plan = Arc::new(plan);
    match and_all(predicates) {
        Some(predicate) => Arc::new(Plan::Filter {
            input: plan,
            predicate,
        }),
        None => plan,
    }
}

/// Splits `predicates` into those that `rewrite` can express over an input and the rest.
fn split(predicates: Vec<Expr>, rewrite: impl Fn(&Expr) -> Option<Expr>) -> (Vec<Expr>, Vec<Expr>) {
    let mut pushed = Vec::new();
    let mut kept = Vec::new();
    for predicate in predicates {
        match rewrite(&predicate) {
            Some(rewritten) => pushed.push(rewritten),
            None => kept.push(predicate),
        }
    }
    (pushed, kept)
}

/// Rewrites a predicate over output columns into one over the columns they are copies of.
/// Fails if the predicate reads a column that `source` cannot trace to the input.
fn retarget(predicate: &Expr, source: impl Fn(&str) -> Option<Expr>) -> Option<Expr> {
    let mut traceable = true;
    predicate.for_each_column(&mut |name| traceable &= source(name).is_some());
    traceable.then(|| predicate.substitute(&source))
}

/// The expression a named column is computed from, when that is just a column or a constant.
fn copied_from(columns: &Named, name: &str) -> Option<Expr> {
    let (_, expr) = columns.iter().find(|(column, _)| &**column == name)?;
    matches!(expr.kind, ExprKind::Column(_) | ExprKind::Literal(_)).then(|| expr.clone())
}

/// Returns a plan for `plan` filtered by all of `predicates`, with each predicate applied as
/// early as it can be.
fn push_filters(plan: &Arc<Plan>, mut predicates: Vec<Expr>) -> Arc<Plan> {
    let none = Vec::new;
    match &**plan {
        Plan::Filter { input, predicate } => {
            conjuncts(predicate.clone(), &mut predicates);
            push_filters(input, predicates)
        }
        Plan::Scan(scan) => {
            let mut scan = scan.clone();
            scan.filters.extend(predicates);
            Arc::new(Plan::Scan(scan))
        }
        Plan::Memory(_) => filtered(Plan::Memory(memory_of(plan)), predicates),
        Plan::Project {
            input,
            columns,
            schema,
        } => {
            let (pushed, kept) = split(predicates, |predicate| {
                retarget(predicate, |name| copied_from(columns, name))
            });
            let project = Plan::Project {
                input: push_filters(input, pushed),
                columns: columns.clone(),
                schema: schema.clone(),
            };
            filtered(project, kept)
        }
        // Filtering first is the same as filtering after, unless the sort also cuts rows off.
        Plan::Sort {
            input,
            keys,
            fetch: None,
        } => Arc::new(Plan::Sort {
            input: push_filters(input, predicates),
            keys: keys.clone(),
            fetch: None,
        }),
        Plan::Aggregate {
            input,
            keys,
            aggs,
            schema,
        } => {
            // A condition on the grouping columns selects whole groups.
            let (pushed, kept) = split(predicates, |predicate| {
                retarget(predicate, |name| copied_from(keys, name))
            });
            let aggregate = Plan::Aggregate {
                input: push_filters(input, pushed),
                keys: keys.clone(),
                aggs: aggs.clone(),
                schema: schema.clone(),
            };
            filtered(aggregate, kept)
        }
        Plan::Window {
            input,
            partition,
            order,
            funcs,
            schema,
        } => {
            // A condition on the partitioning columns selects whole partitions.
            let partitions_on = |name: &str| {
                let key = partition
                    .iter()
                    .find(|key| key.as_column().is_some_and(|column| &**column == name));
                key.cloned()
            };
            let (pushed, kept) = split(predicates, |p| retarget(p, partitions_on));
            let window = Plan::Window {
                input: push_filters(input, pushed),
                partition: partition.clone(),
                order: order.clone(),
                funcs: funcs.clone(),
                schema: schema.clone(),
            };
            filtered(window, kept)
        }
        Plan::Join(join) => {
            let side = |want_left: bool| {
                let schema = if want_left {
                    join.left.schema()
                } else {
                    join.right.schema()
                };
                move |name: &str| -> Option<Expr> {
                    let (_, column) = join.columns.iter().find(|(out, _)| &**out == name)?;
                    let input_name = match (column, want_left) {
                        (JoinColumn::Left(left), true) => left,
                        (JoinColumn::Right(right), false) => right,
                        // An inner join only keeps rows whose keys are equal on both sides.
                        (JoinColumn::Key(left, _), true) => left,
                        (JoinColumn::Key(_, right), false) if join.kind == JoinKind::Inner => right,
                        _ => return None,
                    };
                    let field = schema.field(input_name)?;
                    Some(Expr::column(input_name.clone(), field.ty))
                }
            };
            // Rows of the left side survive every join kind but a full one unchanged, so a
            // condition on them can run first. The same is true of the right side only in an
            // inner join; elsewhere a right-side condition must still see the null rows.
            let (to_left, rest) = match join.kind {
                JoinKind::Full => (none(), predicates),
                _ => split(predicates, |p| retarget(p, side(true))),
            };
            let (to_right, kept) = match join.kind {
                JoinKind::Inner => split(rest, |p| retarget(p, side(false))),
                _ => (none(), rest),
            };
            let joined = Plan::Join(Join {
                left: push_filters(&join.left, to_left),
                right: push_filters(&join.right, to_right),
                ..join.clone()
            });
            filtered(joined, kept)
        }
        Plan::Union { inputs } => {
            let inputs = inputs
                .iter()
                .map(|input| push_filters(input, predicates.clone()));
            Arc::new(Plan::Union {
                inputs: inputs.collect(),
            })
        }
        Plan::Sort { .. }
        | Plan::Limit { .. }
        | Plan::Group { .. }
        | Plan::Unpivot { .. }
        | Plan::Explode { .. } => {
            let rebuilt = map_plan(
                plan,
                &mut |input| push_filters(input, none()),
                &mut Expr::clone,
            );
            filtered(rebuilt, predicates)
        }
    }
}

fn memory_of(plan: &Plan) -> crate::plan::Memory {
    match plan {
        Plan::Memory(memory) => memory.clone(),
        _ => unreachable!("only called for memory tables"),
    }
}

/// Moves limits below projections, and tells sorts and scans how many rows are wanted.
fn push_limits(plan: &Arc<Plan>) -> Arc<Plan> {
    let Plan::Limit { input, skip, fetch } = &**plan else {
        let rebuilt = map_plan(plan, &mut push_limits, &mut Expr::clone);
        return match rebuilt {
            // Only the outer of two adjacent sorts decides the order.
            Plan::Sort { input, keys, fetch } => match &*input {
                Plan::Sort {
                    input: inner,
                    fetch: None,
                    ..
                } => Arc::new(Plan::Sort {
                    input: inner.clone(),
                    keys,
                    fetch,
                }),
                _ => Arc::new(Plan::Sort { input, keys, fetch }),
            },
            other => Arc::new(other),
        };
    };
    let (skip, fetch) = (*skip, *fetch);
    // The rows a limit needs from its input: those it skips plus those it keeps.
    let needed = fetch.map(|fetch| skip.saturating_add(fetch));
    let limit = |input: Arc<Plan>| Arc::new(Plan::Limit { input, skip, fetch });
    match &**input {
        // A projection keeps rows as they are, so limiting first saves computing the rest.
        Plan::Project {
            input: source,
            columns,
            schema,
        } => {
            let limited = push_limits(&limit(source.clone()));
            Arc::new(Plan::Project {
                input: limited,
                columns: columns.clone(),
                schema: schema.clone(),
            })
        }
        Plan::Limit {
            input: source,
            skip: inner_skip,
            fetch: inner_fetch,
        } => {
            let inner_left = inner_fetch.map(|inner| inner.saturating_sub(skip));
            let fetch = match (inner_left, fetch) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            push_limits(&Arc::new(Plan::Limit {
                input: source.clone(),
                skip: inner_skip.saturating_add(skip),
                fetch,
            }))
        }
        Plan::Sort {
            input: source,
            keys,
            fetch: sort_fetch,
        } if needed.is_some() => {
            let fetch = match (*sort_fetch, needed) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            limit(Arc::new(Plan::Sort {
                input: push_limits(source),
                keys: keys.clone(),
                fetch,
            }))
        }
        Plan::Scan(scan) if needed.is_some() => {
            let mut scan = scan.clone();
            scan.limit = match (scan.limit, needed) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            limit(Arc::new(Plan::Scan(scan)))
        }
        _ => limit(push_limits(input)),
    }
}

type Columns = BTreeSet<Arc<str>>;

fn read_by(exprs: impl IntoIterator<Item = impl std::borrow::Borrow<Expr>>, into: &mut Columns) {
    for expr in exprs {
        expr.borrow().for_each_column(&mut |name| {
            into.insert(name.clone());
        });
    }
}

/// Returns a plan that produces at least the `required` columns of `plan`, reading and
/// computing as little else as possible. It may produce other columns too, in any order.
fn prune(plan: &Arc<Plan>, required: &Columns) -> Arc<Plan> {
    match &**plan {
        Plan::Scan(scan) => {
            let mut fields: Vec<Field> = scan
                .schema
                .fields
                .iter()
                .filter(|field| required.contains(&field.name))
                .cloned()
                .collect();
            // Even a row count needs something to read.
            if fields.is_empty() {
                fields.extend(scan.schema.fields.first().cloned());
            }
            Arc::new(Plan::Scan(Scan {
                schema: Arc::new(Schema::new(fields)),
                ..scan.clone()
            }))
        }
        Plan::Memory(_) => plan.clone(),
        Plan::Filter { input, predicate } => {
            let mut needed = required.clone();
            read_by([predicate], &mut needed);
            Arc::new(Plan::Filter {
                input: prune(input, &needed),
                predicate: predicate.clone(),
            })
        }
        Plan::Project { input, columns, .. } => {
            let mut kept: Named = columns
                .iter()
                .filter(|(name, _)| required.contains(name))
                .cloned()
                .collect();
            if kept.is_empty() {
                kept.extend(columns.first().cloned());
            }
            let mut needed = Columns::new();
            read_by(kept.iter().map(|(_, expr)| expr), &mut needed);
            Arc::new(Plan::Project {
                input: prune(input, &needed),
                schema: Arc::new(schema_of(&kept)),
                columns: kept,
            })
        }
        Plan::Sort { input, keys, fetch } => {
            let mut needed = required.clone();
            read_by(keys.iter().map(|key| &key.expr), &mut needed);
            Arc::new(Plan::Sort {
                input: prune(input, &needed),
                keys: keys.clone(),
                fetch: *fetch,
            })
        }
        Plan::Limit { input, skip, fetch } => Arc::new(Plan::Limit {
            input: prune(input, required),
            skip: *skip,
            fetch: *fetch,
        }),
        Plan::Group { input, keys } => {
            let all: Columns = input.schema().names().cloned().collect();
            Arc::new(Plan::Group {
                input: prune(input, &all),
                keys: keys.clone(),
            })
        }
        Plan::Aggregate {
            input, keys, aggs, ..
        } => {
            let aggs: Vec<_> = aggs
                .iter()
                .filter(|(name, _)| required.contains(name))
                .cloned()
                .collect();
            let mut needed = Columns::new();
            read_by(keys.iter().map(|(_, expr)| expr), &mut needed);
            read_by(
                aggs.iter()
                    .flat_map(|(_, call)| call.arg.iter().chain(&call.arg2)),
                &mut needed,
            );
            let mut fields: Vec<Field> = keys
                .iter()
                .map(|(name, expr)| Field::new(name.clone(), expr.ty))
                .collect();
            fields.extend(
                aggs.iter()
                    .map(|(name, call)| Field::new(name.clone(), call.ty)),
            );
            Arc::new(Plan::Aggregate {
                input: prune(input, &needed),
                keys: keys.clone(),
                aggs,
                schema: Arc::new(Schema::new(fields)),
            })
        }
        Plan::Window {
            input,
            partition,
            order,
            funcs,
            ..
        } => {
            let funcs: Vec<_> = funcs
                .iter()
                .filter(|(name, _)| required.contains(name))
                .cloned()
                .collect();
            let input_schema = input.schema();
            let mut needed: Columns = required
                .iter()
                .filter(|name| input_schema.field(name).is_some())
                .cloned()
                .collect();
            read_by(partition, &mut needed);
            read_by(order.iter().map(|key| &key.expr), &mut needed);
            read_by(
                funcs.iter().filter_map(|(_, call)| call.arg.as_ref()),
                &mut needed,
            );
            let input = prune(input, &needed);
            let mut fields = input.schema().fields.clone();
            fields.extend(
                funcs
                    .iter()
                    .map(|(name, call)| Field::new(name.clone(), call.ty)),
            );
            Arc::new(Plan::Window {
                input,
                partition: partition.clone(),
                order: order.clone(),
                funcs,
                schema: Arc::new(Schema::new(fields)),
            })
        }
        Plan::Join(join) => {
            let columns: Vec<_> = join
                .columns
                .iter()
                .filter(|(name, _)| required.contains(name))
                .cloned()
                .collect();
            let (mut left, mut right) = (Columns::new(), Columns::new());
            read_by(join.on.iter().map(|(l, _)| l), &mut left);
            read_by(join.on.iter().map(|(_, r)| r), &mut right);
            for (_, column) in &columns {
                match column {
                    JoinColumn::Left(name) => drop(left.insert(name.clone())),
                    JoinColumn::Right(name) => drop(right.insert(name.clone())),
                    JoinColumn::Key(l, r) => {
                        left.insert(l.clone());
                        right.insert(r.clone());
                    }
                }
            }
            let fields = join
                .schema
                .fields
                .iter()
                .filter(|field| columns.iter().any(|(name, _)| *name == field.name));
            Arc::new(Plan::Join(Join {
                left: prune(&join.left, &left),
                right: prune(&join.right, &right),
                schema: Arc::new(Schema::new(fields.cloned().collect())),
                columns,
                ..join.clone()
            }))
        }
        // Reshaping reads the columns it was built for.
        Plan::Unpivot { input, .. } => {
            let all: Columns = input.schema().names().cloned().collect();
            let rebuilt = map_plan(
                plan,
                &mut |_| restore_schema(prune(input, &all), &input.schema()),
                &mut Expr::clone,
            );
            Arc::new(rebuilt)
        }
        Plan::Explode {
            input,
            column,
            separator,
        } => {
            let mut needed = required.clone();
            needed.insert(column.clone());
            Arc::new(Plan::Explode {
                input: prune(input, &needed),
                column: column.clone(),
                separator: separator.clone(),
            })
        }
        // The inputs of a union are matched by position, so each must keep its exact columns.
        Plan::Union { inputs } => {
            let inputs = inputs.iter().map(|input| {
                let all: Columns = input.schema().names().cloned().collect();
                restore_schema(prune(input, &all), &input.schema())
            });
            Arc::new(Plan::Union {
                inputs: inputs.collect(),
            })
        }
    }
}

/// Combines a projection with the projection it reads from, and drops projections that only
/// pass their input through.
fn merge_projects(plan: &Arc<Plan>) -> Arc<Plan> {
    let rebuilt = map_plan(plan, &mut merge_projects, &mut Expr::clone);
    let Plan::Project {
        mut input,
        mut columns,
        schema,
    } = rebuilt
    else {
        return Arc::new(rebuilt);
    };
    if let Plan::Project {
        input: source,
        columns: inner,
        ..
    } = &*input
    {
        // Putting an inner expression where its column is used computes it once per use, so
        // a computed column that is used twice stays in a projection of its own.
        let uses = |name: &str| {
            let mut count = 0;
            for (_, expr) in &columns {
                expr.for_each_column(&mut |column| count += usize::from(&**column == name));
            }
            count
        };
        let cheap = |expr: &Expr| matches!(expr.kind, ExprKind::Column(_) | ExprKind::Literal(_));
        if inner
            .iter()
            .all(|(name, expr)| cheap(expr) || uses(name) <= 1)
        {
            let definition = |name: &str| {
                let found = inner.iter().find(|(column, _)| &**column == name);
                found.map(|(_, expr)| expr.clone())
            };
            columns = map_named(&columns, &mut |expr| expr.substitute(&definition));
            input = source.clone();
        }
    }
    let unchanged = input
        .schema()
        .names()
        .eq(columns.iter().map(|(name, _)| name))
        && columns
            .iter()
            .all(|(name, expr)| expr.as_column().is_some_and(|column| column == name));
    if unchanged {
        return input;
    }
    Arc::new(Plan::Project {
        input,
        columns,
        schema,
    })
}

/// Adds a projection if `plan` does not produce exactly the columns of `schema`, in order.
fn restore_schema(plan: Arc<Plan>, schema: &Arc<Schema>) -> Arc<Plan> {
    if plan.schema().names().eq(schema.names()) {
        return plan;
    }
    let columns = schema.fields.iter().map(|field| {
        (
            field.name.clone(),
            Expr::column(field.name.clone(), field.ty),
        )
    });
    Arc::new(Plan::Project {
        input: plan,
        columns: columns.collect(),
        schema: schema.clone(),
    })
}
