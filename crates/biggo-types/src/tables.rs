//! Table operations that are written in terms of the others: the engine sees a filter, a
//! projection, an aggregation, a join or a sort, and needs to know nothing new.

use std::sync::Arc;

use biggo_plan::{
    self as plan, AggCall, AggFn, ColType, DataType, Field, JoinColumn, JoinKind, Scalar, ScalarFn,
    Schema, SortKey, TableOp, WindowCall, WindowFn,
};
use biggo_syntax::Span;
use biggo_syntax::ast::{self, BinaryOp};

use crate::check::{Call, Cx, coerce};
use crate::hir::{Expr, ExprKind};
use crate::ty::Type;
use crate::verbs::pass_through;

/// The largest number that `hash` gives, as a float, for turning one into a share.
const HASH_RANGE: f64 = 9.223372036854776e18;

fn column(field: &Field) -> plan::Expr {
    plan::Expr::column(field.name.clone(), field.ty)
}

fn call(func: ScalarFn, args: Vec<plan::Expr>, ty: ColType) -> plan::Expr {
    plan::Expr::new(plan::ExprKind::Call(func, args), ty)
}

fn binary(op: BinaryOp, left: plan::Expr, right: plan::Expr, ty: ColType) -> plan::Expr {
    plan::Expr::new(
        plan::ExprKind::Binary(op, Box::new(left), Box::new(right)),
        ty,
    )
}

fn truth() -> ColType {
    ColType::required(DataType::Bool)
}

/// A test that a column has a value.
fn present(field: &Field) -> plan::Expr {
    let missing = call(ScalarFn::IsNull, vec![column(field)], truth());
    plan::Expr::new(plan::ExprKind::Not(Box::new(missing)), truth())
}

/// A value of a type that stands in for null where two nulls are to count as equal.
fn blank(dtype: DataType) -> Scalar {
    match dtype {
        DataType::Int => Scalar::Int(0),
        DataType::Float => Scalar::Float(0.0),
        DataType::Bool => Scalar::Bool(false),
        DataType::Str => Scalar::Str("".into()),
        DataType::Date => Scalar::Date(0),
        DataType::DateTime => Scalar::DateTime(0),
        DataType::Duration => Scalar::Duration(0),
        DataType::Decimal => Scalar::Decimal(0),
    }
}

impl Cx<'_> {
    /// `count_by(t, a, b)`: the number of rows for each value, the most frequent first.
    pub(crate) fn verb_count_by(&mut self, call: &Call) -> Expr {
        let Some((input, schema)) = self.table_arg(call, 0) else {
            return Expr::error(call.span);
        };
        if !self.all_positional(call, call.args) {
            return Expr::error(call.span);
        }
        let mut keys: Vec<Field> = Vec::new();
        for arg in &call.args[1..] {
            let Some(field) = self.column_name(call, &schema, arg.value) else {
                return Expr::error(call.span);
            };
            if &*field.name == "count" {
                let span = self.ast.span(arg.value);
                let message = "`count_by` names its result `count`; rename this column first";
                return self.error(span, message);
            }
            if !keys.iter().any(|key| key.name == field.name) {
                keys.push(field);
            }
        }
        if keys.is_empty() {
            let message = "`count_by` takes a table and the columns to count by";
            return self.error(call.span, message);
        }
        let whole = ColType::required(DataType::Int);
        let mut fields = keys.clone();
        fields.push(Field::new("count", whole));
        let counted = Arc::new(Schema::new(fields));
        let group = TableOp::Group {
            keys: keys.iter().map(pass_through).collect(),
        };
        let grouped = self.table(call.span, group, vec![input], Vec::new(), schema);
        let rows = AggCall {
            func: AggFn::Count,
            arg: None,
            arg2: None,
            ty: whole,
        };
        let aggregate = TableOp::Aggregate {
            aggs: vec![("count".into(), rows)],
            schema: counted.clone(),
        };
        let table = self.table(
            call.span,
            aggregate,
            vec![grouped],
            Vec::new(),
            counted.clone(),
        );
        // Values that are as frequent stay in the order they first appeared.
        let most = SortKey {
            expr: plan::Expr::column("count", whole),
            descending: true,
            nulls_first: false,
        };
        let sort = TableOp::Sort { keys: vec![most] };
        self.table(call.span, sort, vec![table], Vec::new(), counted)
    }

    /// The columns that a call names after its table, or every column if it names none.
    fn named_or_all(&mut self, call: &Call, schema: &Schema) -> Option<Vec<Field>> {
        if !self.all_positional(call, call.args) {
            return None;
        }
        if call.args.len() == 1 {
            return Some(schema.fields.clone());
        }
        let mut fields = Vec::new();
        for arg in &call.args[1..] {
            fields.push(self.column_name(call, schema, arg.value)?);
        }
        Some(fields)
    }

    /// `drop_nulls(t)` and `drop_nulls(t, a, b)`: without the rows that have a null in any
    /// column, or in those named. The columns that were looked at are not nullable after.
    pub(crate) fn verb_drop_nulls(&mut self, call: &Call) -> Expr {
        let Some((input, schema)) = self.table_arg(call, 0) else {
            return Expr::error(call.span);
        };
        let Some(looked_at) = self.named_or_all(call, &schema) else {
            return Expr::error(call.span);
        };
        let mut tests = looked_at
            .iter()
            .filter(|field| field.ty.nullable)
            .map(present);
        let Some(first) = tests.next() else {
            // No column that was named can hold a null.
            return input;
        };
        let predicate = tests.fold(first, |all, test| binary(BinaryOp::And, all, test, truth()));
        let filter = TableOp::Filter { predicate };
        let kept = self.table(call.span, filter, vec![input], Vec::new(), schema.clone());
        let columns = schema.fields.iter().map(|field| {
            let filled = looked_at.iter().any(|looked| looked.name == field.name);
            let ty = match filled {
                true => ColType::required(field.ty.dtype),
                false => field.ty,
            };
            (
                field.name.clone(),
                plan::Expr::column(field.name.clone(), ty),
            )
        });
        self.project(call.span, kept, columns.collect(), Vec::new())
    }

    /// `fill_nulls(t, a = 0, b = "")`: the nulls of each named column replaced by a value.
    pub(crate) fn verb_fill_nulls(&mut self, call: &Call) -> Expr {
        let Some((input, schema)) = self.table_arg(call, 0) else {
            return Expr::error(call.span);
        };
        if call.args.len() < 2 {
            let message = "`fill_nulls` takes a table and `column = value` for each column to fill";
            return self.error(call.span, message);
        }
        let mut params = Vec::new();
        let mut fills: Vec<(Arc<str>, plan::Expr)> = Vec::new();
        for arg in &call.args[1..] {
            let Some(name) = arg.name else {
                let span = self.ast.span(arg.value);
                let message = "`fill_nulls` takes `column = value`, as in `fill_nulls(t, qty = 0)`";
                return self.error(span, message);
            };
            let text = self.text(name.name);
            let Some(field) = schema.field(&text).cloned() else {
                let names: Vec<&str> = schema.names().map(|name| &**name).collect();
                let message = format!(
                    "the table has no column `{text}`; its columns are {}",
                    names.join(", ")
                );
                return self.error(name.span, message);
            };
            if fills.iter().any(|(filled, _)| *filled == field.name) {
                return self.error(name.span, format!("column `{text}` is filled twice"));
            }
            let filled = ColType::required(field.ty.dtype);
            let value = self.plain(arg.value);
            if value.ty.is_error() {
                return Expr::error(call.span);
            }
            let value = match coerce(value, &Type::from_col(filled)) {
                Ok(value) => value,
                Err(value) => {
                    let message = format!(
                        "column `{text}` is {}, so the value to fill it with must be {}, found {}",
                        Type::from_col(field.ty),
                        Type::from_col(filled),
                        value.ty
                    );
                    return self.error(value.span, message);
                }
            };
            let Some(value) = self.plan_expr(value, &mut params) else {
                return Expr::error(call.span);
            };
            let expr = match field.ty.nullable {
                true => binary(BinaryOp::Coalesce, column(&field), value, filled),
                false => column(&field),
            };
            fills.push((field.name, expr));
        }
        let columns = schema.fields.iter().map(|field| {
            let fill = fills.iter().find(|(name, _)| *name == field.name);
            match fill {
                Some(fill) => fill.clone(),
                None => pass_through(field),
            }
        });
        self.project(call.span, input, columns.collect(), params)
    }

    /// `columns(t)`: the names of the columns of a table, which are known without running it.
    pub(crate) fn verb_columns(&mut self, call: &Call) -> Expr {
        let Some((_, schema)) = self.table_arg(call, 0) else {
            return Expr::error(call.span);
        };
        if call.args.len() != 1 {
            return self.error(call.span, "`columns` takes only a table");
        }
        let name = |name: &Arc<str>| Expr::new(ExprKind::Str(name.clone()), Type::Str, call.span);
        let names = schema.names().map(name).collect();
        let ty = Type::List(Box::new(Type::Str));
        Expr::new(ExprKind::List(names), ty, call.span)
    }

    /// `intersect(a, b)` and `except(a, b)`: the distinct rows of `a` that are, or are not,
    /// also rows of `b`. Two rows are the same when every column is, nulls included.
    pub(crate) fn verb_set(&mut self, call: &Call) -> Expr {
        let name = call.name;
        if call.args.len() != 2 || !self.all_positional(call, call.args) {
            return self.error(call.span, format!("`{name}` takes two tables"));
        }
        let Some((left, left_schema)) = self.table_arg(call, 0) else {
            return Expr::error(call.span);
        };
        let Some((right, right_schema)) = self.table_arg(call, 1) else {
            return Expr::error(call.span);
        };
        let matching = left_schema.fields.len() == right_schema.fields.len()
            && left_schema.fields.iter().all(|field| {
                let other = right_schema.field(&field.name);
                other.is_some_and(|other| other.ty.dtype == field.ty.dtype)
            });
        if !matching {
            let message = format!(
                "the tables of `{name}` need the same columns, found {left_schema} and {right_schema}"
            );
            return self.error(right.span, message);
        }
        let left = self.distinct_rows(call.span, left, left_schema.clone());
        // A join leaves out null keys, where here two nulls are the same value. So each
        // column is compared in two parts: whether it is null, and its value or a stand-in.
        let mut on = Vec::new();
        for field in &left_schema.fields {
            let other = right_schema.field(&field.name).expect("checked above");
            if !field.ty.nullable && !other.ty.nullable {
                on.push((column(field), column(other)));
                continue;
            }
            let missing = |field: &Field| call_is_null(field);
            on.push((missing(field), missing(other)));
            let filled = ColType::required(field.ty.dtype);
            let stand_in = || plan::Expr::literal(blank(field.ty.dtype), filled);
            let valued = |field: &Field| match field.ty.nullable {
                true => binary(BinaryOp::Coalesce, column(field), stand_in(), filled),
                false => column(field),
            };
            on.push((valued(field), valued(other)));
        }
        let columns = left_schema.fields.iter();
        let columns =
            columns.map(|field| (field.name.clone(), JoinColumn::Left(field.name.clone())));
        let join = TableOp::Join {
            kind: if name == "intersect" {
                JoinKind::Semi
            } else {
                JoinKind::Anti
            },
            on,
            columns: columns.collect(),
            schema: left_schema.clone(),
        };
        self.table(call.span, join, vec![left, right], Vec::new(), left_schema)
    }

    /// The table with a column that numbers its rows from 1, under a name no column has.
    fn numbered(&mut self, span: Span, input: Expr, schema: &Schema) -> (Expr, Field) {
        let mut name = String::from("#row");
        while schema.field(&name).is_some() {
            name.push('#');
        }
        let number = Field::new(name.as_str(), ColType::required(DataType::Int));
        let mut fields = schema.fields.clone();
        fields.push(number.clone());
        let numbered = Arc::new(Schema::new(fields));
        let row_number = WindowCall {
            func: WindowFn::RowNumber,
            arg: None,
            offset: 1,
            ty: number.ty,
        };
        let window = TableOp::Window {
            partition: Vec::new(),
            order: Vec::new(),
            funcs: vec![(number.name.clone(), row_number)],
            schema: numbered.clone(),
        };
        let table = self.table(span, window, vec![input], Vec::new(), numbered);
        (table, number)
    }

    /// Sorts by one expression, keeps the first `count` rows, puts those back in the order
    /// of `number`, and drops that column.
    fn first_by(
        &mut self,
        span: Span,
        input: Expr,
        key: SortKey,
        key_params: Vec<Expr>,
        count: Expr,
        (schema, number): (&Arc<Schema>, &Field),
    ) -> Expr {
        let mut fields = schema.fields.clone();
        fields.push(number.clone());
        let numbered = Arc::new(Schema::new(fields));
        let sort = TableOp::Sort { keys: vec![key] };
        let sorted = self.table(span, sort, vec![input], key_params, numbered.clone());
        let taken = self.table(
            span,
            TableOp::Take,
            vec![sorted],
            vec![count],
            numbered.clone(),
        );
        let back = SortKey {
            expr: column(number),
            descending: false,
            nulls_first: false,
        };
        let restore = TableOp::Sort { keys: vec![back] };
        let restored = self.table(span, restore, vec![taken], Vec::new(), numbered);
        let columns = schema.fields.iter().map(pass_through).collect();
        self.project(span, restored, columns, Vec::new())
    }

    /// `tail(t, n)`: the last `n` rows.
    pub(crate) fn verb_tail(&mut self, call: &Call) -> Expr {
        let Some((input, schema)) = self.table_arg(call, 0) else {
            return Expr::error(call.span);
        };
        let [_, count] = call.args else {
            return self.error(call.span, "`tail` takes a table and a number of rows");
        };
        if !self.all_positional(call, call.args) {
            return Expr::error(call.span);
        }
        let Some(count) = self.scalar_arg(count.value, Type::Int, "the number of rows") else {
            return Expr::error(call.span);
        };
        let (numbered, number) = self.numbered(call.span, input, &schema);
        let last = SortKey {
            expr: column(&number),
            descending: true,
            nulls_first: false,
        };
        self.first_by(
            call.span,
            numbered,
            last,
            Vec::new(),
            count,
            (&schema, &number),
        )
    }

    /// `sample(t, n)` and `sample(t, fraction = 0.1)`, with `seed = 0`: rows picked at random.
    /// Each row gets a number from its position and the seed, scattered evenly, and the rows
    /// with the smallest numbers are the sample: the same rows every time for one seed.
    pub(crate) fn verb_sample(&mut self, call: &Call) -> Expr {
        let Some((input, schema)) = self.table_arg(call, 0) else {
            return Expr::error(call.span);
        };
        let (mut count, mut fraction, mut seed) = (None, None, None);
        for arg in &call.args[1..] {
            let slot = match arg.name.map(|name| (self.text(name.name), name.span)) {
                None => &mut count,
                Some((name, _)) if &*name == "fraction" => &mut fraction,
                Some((name, _)) if &*name == "seed" => &mut seed,
                Some((name, span)) => {
                    let message = format!(
                        "`sample` has no argument `{name}`; it takes `fraction` and `seed`"
                    );
                    return self.error(span, message);
                }
            };
            if slot.replace(arg.value).is_some() {
                let span = self.ast.span(arg.value);
                return self.error(span, "this is given twice");
            }
        }
        let usage = "`sample` takes a table and a number of rows, or `fraction = 0.1`";
        let seed = match seed {
            Some(seed) => match self.scalar_arg(seed, Type::Int, "the seed") {
                Some(seed) => seed,
                None => return Expr::error(call.span),
            },
            None => Expr::new(ExprKind::Int(0), Type::Int, call.span),
        };
        let whole = ColType::required(DataType::Int);
        let (numbered, number) = self.numbered(call.span, input, &schema);
        // The seed is the first value that the operation takes from the program.
        let scattered = call_hash(&number, plan::Expr::new(plan::ExprKind::Param(0), whole));
        match (count, fraction) {
            (Some(count), None) => {
                let Some(count) = self.scalar_arg(count, Type::Int, "the number of rows") else {
                    return Expr::error(call.span);
                };
                let smallest = SortKey {
                    expr: scattered,
                    descending: false,
                    nulls_first: false,
                };
                let chosen = (&schema, &number);
                self.first_by(call.span, numbered, smallest, vec![seed], count, chosen)
            }
            (None, Some(fraction)) => {
                if let ast::Expr::Float(share) = self.ast.expr(fraction)
                    && !(0.0..=1.0).contains(share)
                {
                    let span = self.ast.span(fraction);
                    let message = format!("the fraction of `sample` is from 0 to 1, not {share}");
                    return self.error(span, message);
                }
                let Some(fraction) = self.scalar_arg(fraction, Type::Float, "the fraction") else {
                    return Expr::error(call.span);
                };
                // A row is in when its number, as a share of the largest, is under the fraction.
                let float = ColType::required(DataType::Float);
                let as_float = plan::Expr::new(plan::ExprKind::Cast(Box::new(scattered)), float);
                let range = plan::Expr::literal(Scalar::Float(HASH_RANGE), float);
                let share = binary(BinaryOp::Div, as_float, range, float);
                let wanted = plan::Expr::new(plan::ExprKind::Param(1), float);
                let predicate = binary(BinaryOp::Lt, share, wanted, truth());
                let mut fields = schema.fields.clone();
                fields.push(number);
                let numbered_schema = Arc::new(Schema::new(fields));
                let filter = TableOp::Filter { predicate };
                let params = vec![seed, fraction];
                let kept = self.table(call.span, filter, vec![numbered], params, numbered_schema);
                let columns = schema.fields.iter().map(pass_through).collect();
                self.project(call.span, kept, columns, Vec::new())
            }
            _ => self.error(call.span, usage),
        }
    }
}

fn call_is_null(field: &Field) -> plan::Expr {
    call(ScalarFn::IsNull, vec![column(field)], truth())
}

/// The scattered number of a row: `hash` of its row number, with a seed.
fn call_hash(number: &Field, seed: plan::Expr) -> plan::Expr {
    let whole = ColType::required(DataType::Int);
    call(ScalarFn::Hash, vec![column(number), seed], whole)
}
