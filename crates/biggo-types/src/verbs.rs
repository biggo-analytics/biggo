//! Built-in functions: table operations, the scalar functions that also work on columns,
//! aggregates, and input/output. Table operations check their arguments against the columns of
//! their input and lower them to plan expressions.

use std::sync::Arc;

use biggo_plan::{
    self as plan, AggCall, AggFn, ColType, DataType, Field, Format, JoinColumn, JoinKind, Scalar,
    ScalarFn, Schema, SortKey, TableOp, WindowCall, WindowFn,
};
use biggo_syntax::Span;
use biggo_syntax::ast::{self, BinaryOp, Date, ExprId};

use crate::check::{ArgSrc, Call, Cx, FnHint, coerce};
use crate::hir::{Builtin, Expr, ExprKind, TableExpr};
use crate::ty::{Grouped, Type};

/// A table whose columns can be named by the expression being checked.
pub(crate) struct ColumnScope {
    pub schema: Arc<Schema>,
    pub mode: Mode,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Mode {
    /// The expression is computed for each row.
    Row,
    /// The expression is computed for each group and may call aggregate functions. `grouped`
    /// says whether there are group columns, which guarantees that a group has rows.
    Agg { grouped: bool },
    /// The expression is computed for each row and may call window functions.
    Window,
}

const VERBS: [&str; 48] = [
    "print",
    "read_csv",
    "read_parquet",
    "read_json",
    "read_sql",
    "write_csv",
    "write_parquet",
    "write_json",
    "write_sql",
    "collect",
    "explain",
    "describe",
    "histogram",
    "linreg",
    "pivot",
    "unpivot",
    "explode",
    "to_rows",
    "from_rows",
    "len",
    "range",
    "map",
    "filter",
    "fold",
    "each",
    "keys",
    "values",
    "put",
    "has_key",
    "assert",
    "assert_eq",
    "where",
    "select",
    "drop",
    "rename",
    "derive",
    "sort",
    "take",
    "skip",
    "distinct",
    "group",
    "agg",
    "join",
    "window",
    "union",
    "desc",
    "asc",
    "count",
];

/// The name of every built-in function, in alphabetical order, for the tools that list or
/// highlight them.
pub fn builtin_names() -> Vec<&'static str> {
    let scalars = ScalarFn::ALL.into_iter().map(ScalarFn::name);
    let aggregates = AggFn::ALL.into_iter().map(AggFn::name);
    let windows = [
        WindowFn::RowNumber,
        WindowFn::Rank,
        WindowFn::Lag,
        WindowFn::Lead,
        WindowFn::CumSum,
        WindowFn::MovingAvg,
    ];
    let windows = windows.into_iter().map(WindowFn::name);
    let mut names: Vec<&str> = VERBS.into_iter().chain(scalars).collect();
    names.extend(aggregates.chain(windows));
    names.sort_unstable();
    names.dedup();
    names
}

pub(crate) fn is_builtin_name(name: &str) -> bool {
    VERBS.contains(&name)
        || ScalarFn::from_name(name).is_some()
        || WindowFn::from_name(name).is_some()
}

/// A column expression while it is being lowered: either it reads no column, and the program
/// can compute it ahead of the query, or it is an expression over columns.
enum Lowered {
    Free(Expr),
    Col(plan::Expr),
}

/// Builds a node from its operands, which it takes one at a time, in order.
type Rebuild<Operand, Node> = Box<dyn FnOnce(&mut dyn FnMut() -> Box<Operand>) -> Node>;

/// Collects the aggregate or window calls taken out of the expressions of one operation.
struct Extract<'e> {
    /// Names that the new columns must not take.
    taken: &'e dyn Fn(&str) -> bool,
    /// The name for the call when it is the whole expression.
    direct: Option<Arc<str>>,
    aggs: &'e mut Vec<(Arc<str>, AggCall)>,
    windows: &'e mut Vec<(Arc<str>, WindowCall)>,
    params: &'e mut Vec<Expr>,
}

impl Extract<'_> {
    fn name(&mut self) -> Arc<str> {
        if let Some(direct) = self.direct.take() {
            return direct;
        }
        let used = |name: &str, extract: &Self| {
            (extract.taken)(name)
                || extract.aggs.iter().any(|(agg, _)| &**agg == name)
                || extract.windows.iter().any(|(window, _)| &**window == name)
        };
        let mut index = self.aggs.len() + self.windows.len();
        loop {
            let name = format!("_{index}");
            if !used(&name, self) {
                return name.into();
            }
            index += 1;
        }
    }
}

fn with_column(schema: &Schema, name: &Arc<str>, ty: ColType) -> Schema {
    let mut fields = schema.fields.clone();
    match fields.iter_mut().find(|field| field.name == *name) {
        Some(field) => field.ty = ty,
        None => fields.push(Field::new(name.clone(), ty)),
    }
    Schema::new(fields)
}

fn shift_params(expr: &plan::Expr, offset: usize) -> plan::Expr {
    match &expr.kind {
        plan::ExprKind::Param(index) => {
            plan::Expr::new(plan::ExprKind::Param(index + offset), expr.ty)
        }
        _ => expr.map_children(&mut |child| shift_params(child, offset)),
    }
}

fn pass_through(field: &Field) -> (Arc<str>, plan::Expr) {
    (
        field.name.clone(),
        plan::Expr::column(field.name.clone(), field.ty),
    )
}

/// The result type of an aggregate over values of type `arg`, and of type `second` for the
/// aggregates that relate two columns. `grouped` says that every group is known to have at
/// least one row.
fn agg_type(
    func: AggFn,
    arg: Option<ColType>,
    second: Option<ColType>,
    grouped: bool,
) -> Result<ColType, String> {
    use DataType::*;
    let name = func.name();
    let number = |ty: ColType| matches!(ty.dtype, Int | Float | Decimal);
    if func.is_pair() {
        return match (arg, second) {
            (Some(y), Some(x)) if number(y) && number(x) => Ok(ColType::nullable(Float)),
            (Some(y), Some(x)) => Err(format!("`{name}` needs numbers, found {y} and {x}")),
            _ => Err(format!("`{name}` takes two columns, as in `{name}(y, x)`")),
        };
    }
    let Some(arg) = arg else {
        return match func {
            AggFn::Count => Ok(ColType::required(Int)),
            _ => Err(format!(
                "`{name}` needs a column to work on, as in `{name}(x)`"
            )),
        };
    };
    let need_numbers = || match number(arg) {
        true => Ok(()),
        false => Err(format!("`{name}` needs numbers, found {arg}")),
    };
    // With no rows to aggregate there is no value to give.
    let maybe_empty = arg.nullable || !grouped;
    Ok(match func {
        AggFn::Count | AggFn::CountDistinct => ColType::required(Int),
        // Lengths of time add up too.
        AggFn::Sum if arg.dtype == Duration => arg,
        AggFn::Sum => {
            need_numbers()?;
            arg
        }
        AggFn::Mean | AggFn::Median => {
            need_numbers()?;
            ColType::new(Float, maybe_empty)
        }
        AggFn::Stddev => {
            need_numbers()?;
            ColType::nullable(Float)
        }
        AggFn::Min | AggFn::Max => {
            if arg.dtype == Bool {
                return Err(format!("`{name}` does not work on bool"));
            }
            ColType::new(arg.dtype, maybe_empty)
        }
        AggFn::First | AggFn::Last => ColType::new(arg.dtype, maybe_empty),
        AggFn::Corr | AggFn::Cov | AggFn::Slope | AggFn::Intercept => {
            unreachable!("handled above")
        }
    })
}

impl Cx<'_> {
    /// Checks a call to a built-in function. Returns `None` if `call.name` is not one.
    pub(crate) fn builtin_call(&mut self, call: &Call) -> Option<Expr> {
        let typed = matches!(
            call.name,
            "read_csv" | "read_parquet" | "read_json" | "read_sql" | "from_rows"
        );
        if !typed
            && is_builtin_name(call.name)
            && let Some(first) = call.type_args.first()
        {
            let message = format!("`{}` does not take type arguments", call.name);
            return Some(self.error(first.span, message));
        }
        if let Some(result) = self.value_builtin(call) {
            return Some(result);
        }
        Some(match call.name {
            "print" => self.print(call),
            "read_csv" => self.read(call, Format::Csv),
            "read_parquet" => self.read(call, Format::Parquet),
            "read_json" => self.read(call, Format::Json),
            "read_sql" => self.read(call, Format::Sqlite),
            "write_csv" => self.write(call, Builtin::WriteCsv),
            "write_parquet" => self.write(call, Builtin::WriteParquet),
            "write_json" => self.write(call, Builtin::WriteJson),
            "write_sql" => self.write(call, Builtin::WriteSqlite),
            "collect" => self.table_builtin(call, Builtin::Collect),
            "explain" => self.table_builtin(call, Builtin::Explain),
            "describe" => self.table_builtin(call, Builtin::Describe),
            "histogram" => self.verb_histogram(call),
            "linreg" => self.verb_linreg(call),
            "pivot" => self.verb_pivot(call),
            "unpivot" => self.verb_unpivot(call),
            "explode" => self.verb_explode(call),
            "where" => self.verb_where(call),
            "select" => self.verb_select(call),
            "drop" => self.verb_drop(call),
            "rename" => self.verb_rename(call),
            "derive" => self.verb_derive(call),
            "sort" => self.verb_sort(call),
            "take" => self.verb_limit(call, TableOp::Take),
            "skip" => self.verb_limit(call, TableOp::Skip),
            "distinct" => self.verb_distinct(call),
            "group" => self.verb_group(call),
            "agg" => self.verb_agg(call),
            "join" => self.verb_join(call),
            "window" => self.verb_window(call),
            "union" => self.verb_union(call),
            "desc" | "asc" => {
                let message = format!(
                    "`{0}` marks a sort key, as in `sort({0}(x))`; it is not a function",
                    call.name
                );
                self.error(call.span, message)
            }
            name => {
                if let Some(func) = ScalarFn::from_name(name) {
                    return Some(self.scalar_call(func, call));
                }
                let func = WindowFn::from_name(name)?;
                match (self.columns.last().map(|scope| scope.mode), func) {
                    (Some(Mode::Agg { grouped }), WindowFn::Agg(agg)) => {
                        self.agg_call(agg, grouped, call)
                    }
                    (Some(Mode::Window), func) => self.window_call(func, call),
                    _ if name == "count" => self.table_builtin(call, Builtin::Count),
                    _ => {
                        let message = format!("`{name}` can only be used inside `agg` or `window`");
                        self.error(call.span, message)
                    }
                }
            }
        })
    }

    pub(crate) fn table(
        &self,
        span: Span,
        op: TableOp,
        inputs: Vec<Expr>,
        params: Vec<Expr>,
        schema: Arc<Schema>,
    ) -> Expr {
        let table = TableExpr { op, inputs, params };
        Expr::new(ExprKind::Table(Box::new(table)), Type::Table(schema), span)
    }

    /// Checks `value` as ordinary code, outside the columns of any table operation.
    pub(crate) fn plain(&mut self, value: ExprId) -> Expr {
        self.plain_hinted(value, None)
    }

    /// Like `plain`, for an argument that may be a lambda whose types follow from `hint`.
    pub(crate) fn plain_hinted(&mut self, value: ExprId, hint: Option<FnHint>) -> Expr {
        let columns = std::mem::take(&mut self.columns);
        let expr = self.expr_hinted(value, hint);
        self.columns = columns;
        expr
    }

    /// Checks a scalar argument such as a row count or a file path.
    pub(crate) fn scalar_arg(&mut self, value: ExprId, expected: Type, what: &str) -> Option<Expr> {
        let expr = self.plain(value);
        if expr.ty.is_error() {
            return None;
        }
        match coerce(expr, &expected) {
            Ok(expr) => Some(expr),
            Err(expr) => {
                let message = format!("{what} must be {expected}, found {}", expr.ty);
                self.error(expr.span, message);
                None
            }
        }
    }

    /// Checks the argument at `index`, which must be a table, and returns it with its columns.
    pub(crate) fn table_arg(&mut self, call: &Call, index: usize) -> Option<(Expr, Arc<Schema>)> {
        let name = call.name;
        let Some(arg) = call.args.get(index).filter(|arg| arg.name.is_none()) else {
            let message =
                format!("`{name}` needs a table to work on, as in `table |> {name}(...)`");
            self.error(call.span, message);
            return None;
        };
        let expr = self.plain(arg.value);
        match &expr.ty {
            Type::Table(schema) => {
                let schema = schema.clone();
                Some((expr, schema))
            }
            Type::Error => None,
            Type::Grouped(_) => {
                let message = format!("this table is grouped; call `agg` before `{name}`");
                self.error(expr.span, message);
                None
            }
            other => {
                let message = format!("`{name}` works on a table, found {other}");
                self.error(expr.span, message);
                None
            }
        }
    }

    /// Reports the named arguments among `args`; returns whether there were none.
    pub(crate) fn all_positional(&mut self, call: &Call, args: &[ArgSrc]) -> bool {
        let mut ok = true;
        for name in args.iter().filter_map(|arg| arg.name) {
            let message = format!("`{}` does not take named arguments", call.name);
            self.error(name.span, message);
            ok = false;
        }
        ok
    }

    /// Checks `value` as an expression over the columns of `schema` and lowers it to a plan
    /// expression. Values it takes from the program are appended to `params`.
    fn column_expr(
        &mut self,
        schema: &Arc<Schema>,
        value: ExprId,
        params: &mut Vec<Expr>,
    ) -> Option<plan::Expr> {
        let expr = self.checked_in(schema, Mode::Row, value)?;
        self.plan_expr(expr, params)
    }

    fn checked_in(&mut self, schema: &Arc<Schema>, mode: Mode, value: ExprId) -> Option<Expr> {
        self.columns.push(ColumnScope {
            schema: schema.clone(),
            mode,
        });
        let expr = self.expr(value);
        self.columns.pop();
        (!expr.ty.is_error()).then_some(expr)
    }

    /// The column that `value` names, which must be nothing more than a column name.
    fn column_name(&mut self, call: &Call, schema: &Schema, value: ExprId) -> Option<Field> {
        let span = self.ast.span(value);
        let ast::Expr::Name(name) = self.ast.expr(value) else {
            let message = format!(
                "`{}` takes column names here; write `name = expression` to compute a column",
                call.name
            );
            self.error(span, message);
            return None;
        };
        let text = self.text(*name);
        let field = schema.field(&text).cloned();
        if field.is_none() {
            let names: Vec<&str> = schema.names().map(|name| &**name).collect();
            let message = format!(
                "the table has no column `{text}`; its columns are {}",
                names.join(", ")
            );
            self.error(span, message);
        }
        field
    }

    fn plan_expr(&mut self, expr: Expr, params: &mut Vec<Expr>) -> Option<plan::Expr> {
        let lowered = self.lower(expr, params)?;
        self.finish(lowered, params)
    }

    /// Turns a lowered expression into a plan expression. One that reads no column becomes a
    /// constant, or a parameter whose value the program computes before the query runs.
    fn finish(&mut self, lowered: Lowered, params: &mut Vec<Expr>) -> Option<plan::Expr> {
        let expr = match lowered {
            Lowered::Col(expr) => return Some(expr),
            Lowered::Free(expr) => expr,
        };
        let Some(ty) = expr.ty.to_col() else {
            let message = match expr.ty {
                Type::Null => "cannot tell which type this `null` has".to_string(),
                _ => format!("a {} cannot be used in a column expression", expr.ty),
            };
            self.error(expr.span, message);
            return None;
        };
        let constant = match &expr.kind {
            ExprKind::Null => Scalar::Null,
            ExprKind::Bool(value) => Scalar::Bool(*value),
            ExprKind::Int(value) => Scalar::Int(*value),
            ExprKind::Float(value) => Scalar::Float(*value),
            ExprKind::Str(value) => Scalar::Str(value.clone()),
            ExprKind::Date(value) => Scalar::Date(*value),
            ExprKind::DateTime(value) => Scalar::DateTime(*value),
            ExprKind::Decimal(value) => Scalar::Decimal(*value),
            _ => {
                params.push(expr);
                return Some(plan::Expr::new(plan::ExprKind::Param(params.len() - 1), ty));
            }
        };
        Some(plan::Expr::literal(constant, ty))
    }

    fn col_type(&mut self, ty: &Type, span: Span) -> Option<ColType> {
        let col = ty.to_col();
        if col.is_none() {
            let message = format!("a column expression cannot have type {ty}");
            self.error(span, message);
        }
        col
    }

    /// Lowers `operands`. If none of them reads a column they come back unchanged; otherwise
    /// all of them become plan expressions.
    fn lower_all(
        &mut self,
        operands: Vec<Expr>,
        params: &mut Vec<Expr>,
    ) -> Option<Result<Vec<Expr>, Vec<plan::Expr>>> {
        let mut lowered = Vec::with_capacity(operands.len());
        for operand in operands {
            lowered.push(self.lower(operand, params)?);
        }
        if lowered
            .iter()
            .all(|operand| matches!(operand, Lowered::Free(_)))
        {
            let free = lowered.into_iter().map(|operand| match operand {
                Lowered::Free(expr) => expr,
                Lowered::Col(_) => unreachable!("just checked"),
            });
            return Some(Ok(free.collect()));
        }
        let mut cols = Vec::with_capacity(lowered.len());
        for operand in lowered {
            cols.push(self.finish(operand, params)?);
        }
        Some(Err(cols))
    }

    fn lower(&mut self, expr: Expr, params: &mut Vec<Expr>) -> Option<Lowered> {
        let Expr { kind, ty, span } = expr;
        // Each operator node is rebuilt from its lowered operands, as the same kind of node
        // it was or as the matching plan node.
        let (operands, free, col): (
            Vec<Expr>,
            Rebuild<Expr, ExprKind>,
            Rebuild<plan::Expr, plan::ExprKind>,
        ) = match kind {
            ExprKind::Column(name) => {
                let col_ty = self.col_type(&ty, span)?;
                return Some(Lowered::Col(plan::Expr::column(name, col_ty)));
            }
            ExprKind::Neg(operand) => (
                vec![*operand],
                Box::new(|next| ExprKind::Neg(next())),
                Box::new(|next| plan::ExprKind::Neg(next())),
            ),
            ExprKind::Not(operand) => (
                vec![*operand],
                Box::new(|next| ExprKind::Not(next())),
                Box::new(|next| plan::ExprKind::Not(next())),
            ),
            ExprKind::ToFloat(operand) => (
                vec![*operand],
                Box::new(|next| ExprKind::ToFloat(next())),
                Box::new(|next| plan::ExprKind::Cast(next())),
            ),
            ExprKind::Convert(conversion, operand) => (
                vec![*operand],
                Box::new(move |next| ExprKind::Convert(conversion, next())),
                Box::new(|next| plan::ExprKind::Cast(next())),
            ),
            ExprKind::Binary(op, left, right) => (
                vec![*left, *right],
                Box::new(move |next| ExprKind::Binary(op, next(), next())),
                Box::new(move |next| plan::ExprKind::Binary(op, next(), next())),
            ),
            ExprKind::If(cond, then, Some(otherwise)) => (
                vec![*cond, *then, *otherwise],
                Box::new(|next| ExprKind::If(next(), next(), Some(next()))),
                Box::new(|next| plan::ExprKind::If(next(), next(), next())),
            ),
            ExprKind::Scalar(func, args) => {
                return match self.lower_all(args, params)? {
                    Ok(args) => {
                        let kind = ExprKind::Scalar(func, args);
                        Some(Lowered::Free(Expr::new(kind, ty, span)))
                    }
                    Err(args) => {
                        let digits = args.get(1).filter(|_| func == ScalarFn::Round);
                        if digits.is_some_and(|d| d.reads_any(|_| true)) {
                            let message =
                                "the number of digits of `round` cannot depend on a column";
                            self.error(span, message);
                            return None;
                        }
                        let col_ty = self.col_type(&ty, span)?;
                        let kind = plan::ExprKind::Call(func, args);
                        Some(Lowered::Col(plan::Expr::new(kind, col_ty)))
                    }
                };
            }
            ExprKind::Block(stmts, Some(value)) if stmts.is_empty() => {
                return self.lower(*value, params);
            }
            kind => {
                let expr = Expr::new(kind, ty, span);
                let Some(column_use) = expr.find_column_use() else {
                    return Some(Lowered::Free(expr));
                };
                let message = match (&expr.kind, &column_use.kind) {
                    (_, ExprKind::Agg(..) | ExprKind::Window(..)) => {
                        "an aggregate cannot be used here"
                    }
                    (ExprKind::If(..), _) => "an `if` over columns needs an `else` branch",
                    (ExprKind::Call(..), _) => {
                        "a user-defined function cannot take a column; \
                         only operators and built-in functions work on columns"
                    }
                    (ExprKind::List(_), _) => "a list cannot be built from columns",
                    (ExprKind::Record(..) | ExprKind::Map(_), _) => {
                        "a record or a map cannot be built from columns"
                    }
                    (ExprKind::Closure(..), _) => "a function cannot use a column",
                    _ => "a column cannot be used in this kind of expression",
                };
                self.error(column_use.span, message);
                return None;
            }
        };
        Some(match self.lower_all(operands, params)? {
            Ok(operands) => {
                let mut operands = operands.into_iter();
                let kind = free(&mut || Box::new(operands.next().expect("one per operand")));
                Lowered::Free(Expr::new(kind, ty, span))
            }
            Err(operands) => {
                let mut operands = operands.into_iter();
                let kind = col(&mut || Box::new(operands.next().expect("one per operand")));
                Lowered::Col(plan::Expr::new(kind, self.col_type(&ty, span)?))
            }
        })
    }

    /// Replaces the aggregate and window calls in `expr` with references to new columns,
    /// recording each call in `extract`. What is left is an expression over those columns.
    fn extract(&mut self, expr: Expr, extract: &mut Extract) -> Option<Expr> {
        let Expr { kind, ty, span } = expr;
        let mut recur = |cx: &mut Self, operand: Box<Expr>| -> Option<Box<Expr>> {
            Some(Box::new(cx.extract(*operand, extract)?))
        };
        let kind = match kind {
            ExprKind::Agg(func, args) => {
                let mut planned = Vec::with_capacity(args.len());
                for arg in args {
                    planned.push(self.plan_expr(arg, extract.params)?);
                }
                let mut planned = planned.into_iter();
                let call = AggCall {
                    func,
                    arg: planned.next(),
                    arg2: planned.next(),
                    ty: self.col_type(&ty, span)?,
                };
                let name = extract.name();
                extract.aggs.push((name.clone(), call));
                ExprKind::Column(name)
            }
            ExprKind::Window(func, arg, offset) => {
                let arg = match arg {
                    Some(arg) => Some(self.plan_expr(*arg, extract.params)?),
                    None => None,
                };
                let call = WindowCall {
                    func,
                    arg,
                    offset,
                    ty: self.col_type(&ty, span)?,
                };
                let name = extract.name();
                extract.windows.push((name.clone(), call));
                ExprKind::Column(name)
            }
            ExprKind::Neg(operand) => ExprKind::Neg(recur(self, operand)?),
            ExprKind::Not(operand) => ExprKind::Not(recur(self, operand)?),
            ExprKind::ToFloat(operand) => ExprKind::ToFloat(recur(self, operand)?),
            ExprKind::Convert(conversion, operand) => {
                ExprKind::Convert(conversion, recur(self, operand)?)
            }
            ExprKind::Binary(op, left, right) => {
                let left = recur(self, left)?;
                ExprKind::Binary(op, left, recur(self, right)?)
            }
            ExprKind::If(cond, then, Some(otherwise)) => {
                let cond = recur(self, cond)?;
                let then = recur(self, then)?;
                ExprKind::If(cond, then, Some(recur(self, otherwise)?))
            }
            ExprKind::Scalar(func, args) => {
                let mut extracted = Vec::with_capacity(args.len());
                for arg in args {
                    extracted.push(*recur(self, Box::new(arg))?);
                }
                ExprKind::Scalar(func, extracted)
            }
            ExprKind::Block(stmts, Some(value)) if stmts.is_empty() => {
                return self.extract(*value, extract);
            }
            other => other,
        };
        Some(Expr::new(kind, ty, span))
    }

    fn print(&mut self, call: &Call) -> Expr {
        let ok = self.all_positional(call, call.args);
        let args: Vec<Expr> = call.args.iter().map(|arg| self.plain(arg.value)).collect();
        if !ok || args.iter().any(|arg| arg.ty.is_error()) {
            return Expr::error(call.span);
        }
        if let Some(grouped) = args.iter().find(|arg| matches!(arg.ty, Type::Grouped(_))) {
            return self.error(
                grouped.span,
                "a grouped table has no rows to print; call `agg` first",
            );
        }
        Expr::new(
            ExprKind::Builtin(Builtin::Print, args),
            Type::Unit,
            call.span,
        )
    }

    fn read(&mut self, call: &Call, format: Format) -> Expr {
        let name = call.name;
        let [row_type] = call.type_args else {
            let example = match format {
                Format::Sqlite => "(\"app.db\", \"select id from users\")",
                _ => "(\"file\")",
            };
            let message = format!(
                "`{name}` needs the row type of the data, as in `{name}<{{id: int}}>{example}`"
            );
            return self.error(call.span, message);
        };
        let row = self.resolve_type(row_type);
        let Some(schema) = self.row_schema(&row, row_type.span) else {
            return Expr::error(call.span);
        };
        if schema.fields.is_empty() {
            return self.error(row_type.span, "the row type needs at least one column");
        }
        if !self.all_positional(call, call.args) {
            return Expr::error(call.span);
        }
        let texts: &[(ExprId, &str)] = match (format, call.args) {
            (Format::Sqlite, [path, query]) => &[
                (path.value, "the database path"),
                (query.value, "the query"),
            ],
            (Format::Sqlite, _) => {
                let message = "`read_sql` takes the path of a SQLite database and a query";
                return self.error(call.span, message);
            }
            (_, [path]) => &[(path.value, "the file path")],
            _ => {
                let message = format!("`{name}` takes one argument, the file path");
                return self.error(call.span, message);
            }
        };
        let mut params = Vec::with_capacity(texts.len());
        for (value, what) in texts {
            match self.scalar_arg(*value, Type::Str, what) {
                Some(param) => params.push(param),
                None => return Expr::error(call.span),
            }
        }
        let op = TableOp::Read {
            format,
            schema: schema.clone(),
        };
        self.table(call.span, op, Vec::new(), params, schema)
    }

    fn write(&mut self, call: &Call, builtin: Builtin) -> Expr {
        let Some((table, _)) = self.table_arg(call, 0) else {
            return Expr::error(call.span);
        };
        let texts: &[(ExprId, &str)] = match (builtin, call.args) {
            (Builtin::WriteSqlite, [_, path, name]) => &[
                (path.value, "the database path"),
                (name.value, "the table name"),
            ],
            (Builtin::WriteSqlite, _) => {
                let message = "`write_sql` takes a table, the path of a SQLite database, \
                               and the name to store the table under";
                return self.error(call.span, message);
            }
            (_, [_, path]) => &[(path.value, "the file path")],
            _ => {
                let message = format!("`{}` takes a table and a file path", call.name);
                return self.error(call.span, message);
            }
        };
        if !self.all_positional(call, call.args) {
            return Expr::error(call.span);
        }
        let mut args = vec![table];
        for (value, what) in texts {
            match self.scalar_arg(*value, Type::Str, what) {
                Some(arg) => args.push(arg),
                None => return Expr::error(call.span),
            }
        }
        Expr::new(ExprKind::Builtin(builtin, args), Type::Unit, call.span)
    }

    fn table_builtin(&mut self, call: &Call, builtin: Builtin) -> Expr {
        let Some((table, schema)) = self.table_arg(call, 0) else {
            return Expr::error(call.span);
        };
        if call.args.len() != 1 {
            let message = format!("`{}` takes only a table", call.name);
            return self.error(call.span, message);
        }
        let ty = match builtin {
            Builtin::Collect => Type::Table(schema),
            Builtin::Describe => Type::Table(Arc::new(plan::describe_schema())),
            Builtin::Count => Type::Int,
            _ => Type::Unit,
        };
        Expr::new(ExprKind::Builtin(builtin, vec![table]), ty, call.span)
    }

    /// `histogram(t, column)` or `histogram(t, column, bins)`: how the values of a numeric
    /// column spread over ranges of equal width.
    fn verb_histogram(&mut self, call: &Call) -> Expr {
        let Some((table, schema)) = self.table_arg(call, 0) else {
            return Expr::error(call.span);
        };
        let (column, bins) = match call.args {
            [_, column] if column.name.is_none() => (column, None),
            [_, column, bins] if column.name.is_none() => (column, Some(bins)),
            _ => {
                let message = "`histogram` takes a table, a column, and optionally `bins = n`";
                return self.error(call.span, message);
            }
        };
        let Some(field) = self.column_name(call, &schema, column.value) else {
            return Expr::error(call.span);
        };
        if !is_number(field.ty) {
            let span = self.ast.span(column.value);
            let message = format!(
                "`histogram` needs a column of numbers, but `{}` is {}",
                field.name, field.ty
            );
            return self.error(span, message);
        }
        let bins = match bins {
            Some(bins) => {
                if let Some(name) = bins.name.filter(|name| &*self.text(name.name) != "bins") {
                    let message = "`histogram` has no such argument; it takes `bins = n`";
                    return self.error(name.span, message);
                }
                match self.scalar_arg(bins.value, Type::Int, "the number of bins") {
                    Some(bins) => bins,
                    None => return Expr::error(call.span),
                }
            }
            None => Expr::new(ExprKind::Int(10), Type::Int, call.span),
        };
        let name = Expr::new(ExprKind::Str(field.name), Type::Str, call.span);
        let ty = Type::Table(Arc::new(plan::histogram_schema()));
        let kind = ExprKind::Builtin(Builtin::Histogram, vec![table, name, bins]);
        Expr::new(kind, ty, call.span)
    }

    /// `linreg(t, y, x)`: the least-squares line `y = slope * x + intercept` through the rows
    /// of a table, as a record.
    fn verb_linreg(&mut self, call: &Call) -> Expr {
        let Some((input, schema)) = self.table_arg(call, 0) else {
            return Expr::error(call.span);
        };
        let [_, y, x] = call.args else {
            let message = "`linreg` takes a table and two columns, as in `linreg(t, y, x)`";
            return self.error(call.span, message);
        };
        if !self.all_positional(call, call.args) {
            return Expr::error(call.span);
        }
        let mut params = Vec::new();
        let mut columns = Vec::with_capacity(2);
        for arg in [y, x] {
            let Some(expr) = self.column_expr(&schema, arg.value, &mut params) else {
                return Expr::error(call.span);
            };
            if !is_number(expr.ty) {
                let span = self.ast.span(arg.value);
                let message = format!("`linreg` needs numbers, found {}", expr.ty);
                return self.error(span, message);
            }
            columns.push(expr);
        }
        let float = ColType::nullable(DataType::Float);
        let pair = |name: &str, func: AggFn| {
            let call = AggCall {
                func,
                arg: Some(columns[0].clone()),
                arg2: Some(columns[1].clone()),
                ty: float,
            };
            (Arc::<str>::from(name), call)
        };
        let aggs = vec![
            pair("slope", AggFn::Slope),
            pair("intercept", AggFn::Intercept),
            pair("r", AggFn::Corr),
        ];
        let fields = aggs.iter().map(|(name, _)| Field::new(name.clone(), float));
        let schema = Arc::new(Schema::new(fields.collect()));
        let op = TableOp::Aggregate {
            aggs,
            schema: schema.clone(),
        };
        let fitted = self.table(call.span, op, vec![input], params, schema);
        let r = plan::Expr::column("r", float);
        let squared = plan::ExprKind::Binary(BinaryOp::Mul, Box::new(r.clone()), Box::new(r));
        let columns = vec![
            pass_through(&Field::new("slope", float)),
            pass_through(&Field::new("intercept", float)),
            ("r2".into(), plan::Expr::new(squared, float)),
        ];
        let line = self.project(call.span, fitted, columns, Vec::new());
        let Type::Table(schema) = &line.ty else {
            unreachable!("a projection is a table");
        };
        // Aggregating a whole table gives exactly one row.
        let row = Type::row_of(schema);
        Expr::new(
            ExprKind::Builtin(Builtin::OnlyRow, vec![line]),
            row,
            call.span,
        )
    }

    /// The text of an argument that must be written as a string literal.
    fn literal_text(&mut self, value: ExprId, what: &str) -> Option<Arc<str>> {
        match self.ast.expr(value) {
            ast::Expr::Str(text) if !text.is_empty() => Some(text.clone()),
            _ => {
                let span = self.ast.span(value);
                self.error(
                    span,
                    format!("{what} must be written as a string, not empty"),
                );
                None
            }
        }
    }

    /// `unpivot(t, a, b, ...)`: one row per input row and listed column, holding the name of
    /// the column and its value.
    fn verb_unpivot(&mut self, call: &Call) -> Expr {
        let Some((input, schema)) = self.table_arg(call, 0) else {
            return Expr::error(call.span);
        };
        let (mut name, mut value): (Arc<str>, Arc<str>) = ("name".into(), "value".into());
        let mut columns: Vec<Field> = Vec::new();
        for arg in &call.args[1..] {
            let Some(option) = arg.name else {
                let Some(field) = self.column_name(call, &schema, arg.value) else {
                    return Expr::error(call.span);
                };
                if columns.iter().any(|column| column.name == field.name) {
                    let span = self.ast.span(arg.value);
                    return self.error(span, format!("column `{}` is listed twice", field.name));
                }
                columns.push(field);
                continue;
            };
            let target = match &*self.text(option.name) {
                "names" => &mut name,
                "values" => &mut value,
                other => {
                    let message = format!(
                        "`unpivot` has no argument `{other}`; it takes columns, \
                         `names = \"...\"`, and `values = \"...\"`"
                    );
                    return self.error(option.span, message);
                }
            };
            match self.literal_text(arg.value, "the name of a new column") {
                Some(text) => *target = text,
                None => return Expr::error(call.span),
            }
        }
        let Some(first) = columns.first() else {
            let message = "`unpivot` needs the columns to turn into rows, as in `unpivot(a, b)`";
            return self.error(call.span, message);
        };
        if let Some(other) = columns.iter().find(|c| c.ty.dtype != first.ty.dtype) {
            let message = format!(
                "the columns of `unpivot` need one type, but `{}` is {} and `{}` is {}; \
                 convert one first",
                first.name,
                first.ty.dtype.name(),
                other.name,
                other.ty.dtype.name()
            );
            return self.error(call.span, message);
        }
        let kept = schema.fields.iter();
        let kept = kept.filter(|field| !columns.iter().any(|column| column.name == field.name));
        let mut fields: Vec<Field> = kept.cloned().collect();
        let taken = [&name, &value]
            .into_iter()
            .find(|new| fields.iter().any(|field| field.name == **new));
        if let Some(taken) = taken {
            let message = format!(
                "the table already has a column `{taken}`; \
                 choose other names with `names = \"...\"` and `values = \"...\"`"
            );
            return self.error(call.span, message);
        }
        if name == value {
            return self.error(call.span, "the two new columns need different names");
        }
        let nullable = columns.iter().any(|column| column.ty.nullable);
        fields.push(Field::new(name.clone(), ColType::required(DataType::Str)));
        fields.push(Field::new(
            value.clone(),
            ColType::new(first.ty.dtype, nullable),
        ));
        let schema = Arc::new(Schema::new(fields));
        let op = TableOp::Unpivot {
            columns: columns.into_iter().map(|column| column.name).collect(),
            name,
            value,
            schema: schema.clone(),
        };
        self.table(call.span, op, vec![input], Vec::new(), schema)
    }

    /// `explode(t, column)` or `explode(t, column, ";")`: one row per part of a string that
    /// lists several values.
    fn verb_explode(&mut self, call: &Call) -> Expr {
        let Some((input, schema)) = self.table_arg(call, 0) else {
            return Expr::error(call.span);
        };
        let (column, separator) = match call.args {
            [_, column] => (column, None),
            [_, column, separator] => (column, Some(separator)),
            _ => {
                let message =
                    "`explode` takes a column and, optionally, the text that separates its parts";
                return self.error(call.span, message);
            }
        };
        if !self.all_positional(call, call.args) {
            return Expr::error(call.span);
        }
        let Some(field) = self.column_name(call, &schema, column.value) else {
            return Expr::error(call.span);
        };
        if field.ty.dtype != DataType::Str {
            let span = self.ast.span(column.value);
            let message = format!(
                "`explode` splits a column of strings, but `{}` is {}",
                field.name, field.ty
            );
            return self.error(span, message);
        }
        let separator = match separator {
            Some(separator) => match self.literal_text(separator.value, "the separator") {
                Some(separator) => separator,
                None => return Expr::error(call.span),
            },
            None => ",".into(),
        };
        let op = TableOp::Explode {
            column: field.name,
            separator,
        };
        self.table(call.span, op, vec![input], Vec::new(), schema)
    }

    /// `pivot(t, column, ["a", "b"], sum(x))`: one output column per listed value of
    /// `column`, holding the aggregate of the rows that have that value. Grouping the table
    /// first gives one row per group.
    fn verb_pivot(&mut self, call: &Call) -> Expr {
        let usage = "`pivot` takes a column, the list of its values that become columns, \
                     and an aggregate, as in `pivot(region, [\"north\", \"south\"], sum(amount))`";
        let Some(first) = call.args.first().filter(|arg| arg.name.is_none()) else {
            return self.error(call.span, usage);
        };
        let input = self.plain(first.value);
        let (schema, keys) = match &input.ty {
            Type::Grouped(grouped) => (grouped.input.clone(), grouped.keys.clone()),
            Type::Table(schema) => (schema.clone(), Vec::new()),
            Type::Error => return Expr::error(call.span),
            other => {
                let message = format!("`pivot` works on a table or a grouped table, found {other}");
                return self.error(input.span, message);
            }
        };
        let [_, column, values, outputs @ ..] = call.args else {
            return self.error(call.span, usage);
        };
        if column.name.is_some() || values.name.is_some() || outputs.is_empty() {
            return self.error(call.span, usage);
        }
        let Some(field) = self.column_name(call, &schema, column.value) else {
            return Expr::error(call.span);
        };
        let ast::Expr::List(items) = self.ast.expr(values.value) else {
            let span = self.ast.span(values.value);
            return self.error(span, "the values of `pivot` must be written out as a list");
        };
        if items.is_empty() {
            let span = self.ast.span(values.value);
            return self.error(span, "`pivot` needs at least one value");
        }

        // Each listed value gives the name of its column and the test that picks its rows.
        let subject = Expr::new(
            ExprKind::Column(field.name.clone()),
            Type::from_col(field.ty),
            self.ast.span(column.value),
        );
        let mut picks: Vec<(Arc<str>, Expr)> = Vec::with_capacity(items.len());
        for item in items {
            let value = self.plain(*item);
            let text: Arc<str> = match &value.kind {
                ExprKind::Str(text) => text.clone(),
                ExprKind::Int(number) => number.to_string().into(),
                ExprKind::Bool(flag) => flag.to_string().into(),
                ExprKind::Date(days) => match Date::from_days(*days) {
                    Some(date) => date.to_string().into(),
                    None => return Expr::error(call.span),
                },
                ExprKind::Error => return Expr::error(call.span),
                _ => {
                    let message = "a value of `pivot` must be written as a string, a whole \
                                   number, a bool, or a date";
                    return self.error(value.span, message);
                }
            };
            let Some(test) = self.equals(value.span, subject.clone(), value) else {
                return Expr::error(call.span);
            };
            picks.push((text, test));
        }

        let mode = Mode::Agg {
            grouped: !keys.is_empty(),
        };
        let unnamed = outputs.iter().filter(|arg| arg.name.is_none()).count();
        if unnamed > 1 || (unnamed == 1 && outputs.len() > 1) {
            let message = "with several aggregates, `pivot` needs a name for each, \
                           as in `total = sum(amount)`";
            return self.error(call.span, message);
        }
        let mut aggs: Vec<(Arc<str>, AggCall)> = Vec::new();
        let mut params = Vec::new();
        for output in outputs {
            let Some(expr) = self.checked_in(&schema, mode, output.value) else {
                return Expr::error(call.span);
            };
            let ExprKind::Agg(func, args) = expr.kind else {
                let message = "`pivot` takes aggregates such as `sum(amount)`";
                return self.error(expr.span, message);
            };
            if matches!(func, AggFn::First | AggFn::Last) {
                let message = format!("`{}` cannot be used in `pivot`", func.name());
                return self.error(expr.span, message);
            }
            let suffix = output.name.map(|name| self.text(name.name));
            for (text, test) in &picks {
                // The aggregate sees the value only in the rows the test picks; in the
                // others it sees null, which every aggregate leaves out.
                let mut args = args.clone().into_iter();
                let counted = Expr::new(ExprKind::Bool(true), Type::Bool, expr.span);
                let value = args.next().unwrap_or(counted);
                let picked_ty = value.ty.clone().or_null();
                let nothing = Expr::new(ExprKind::Null, picked_ty.clone(), value.span);
                let span = value.span;
                let kind = ExprKind::If(
                    Box::new(test.clone()),
                    Box::new(coerce(value, &picked_ty).unwrap_or_else(|value| value)),
                    Some(Box::new(nothing)),
                );
                let picked = Expr::new(kind, picked_ty.clone(), span);
                let second = args.next();

                let Some(picked_col) = self.col_type(&picked_ty, span) else {
                    return Expr::error(call.span);
                };
                let second_col = second.as_ref().and_then(|second| second.ty.to_col());
                let ty = match agg_type(func, Some(picked_col), second_col, !keys.is_empty()) {
                    Ok(ty) => ty,
                    Err(message) => return self.error(expr.span, message),
                };
                let Some(arg) = self.plan_expr(picked, &mut params) else {
                    return Expr::error(call.span);
                };
                let arg2 = match second {
                    Some(second) => match self.plan_expr(second, &mut params) {
                        Some(second) => Some(second),
                        None => return Expr::error(call.span),
                    },
                    None => None,
                };
                let name: Arc<str> = match &suffix {
                    Some(suffix) => format!("{text}_{suffix}").into(),
                    None => text.clone(),
                };
                let exists = keys.iter().any(|key| key.name == name)
                    || aggs.iter().any(|(other, _)| *other == name);
                if exists {
                    let message = format!("the result would have two columns called `{name}`");
                    return self.error(call.span, message);
                }
                let call = AggCall {
                    func,
                    arg: Some(arg),
                    arg2,
                    ty,
                };
                aggs.push((name, call));
            }
        }

        let mut fields = keys;
        fields.extend(
            aggs.iter()
                .map(|(name, agg)| Field::new(name.clone(), agg.ty)),
        );
        let schema = Arc::new(Schema::new(fields));
        let op = TableOp::Aggregate {
            aggs,
            schema: schema.clone(),
        };
        self.table(call.span, op, vec![input], params, schema)
    }

    fn verb_where(&mut self, call: &Call) -> Expr {
        let Some((input, schema)) = self.table_arg(call, 0) else {
            return Expr::error(call.span);
        };
        let [_, condition] = call.args else {
            return self.error(call.span, "`where` takes one condition");
        };
        if !self.all_positional(call, call.args) {
            return Expr::error(call.span);
        }
        let mut params = Vec::new();
        let Some(predicate) = self.column_expr(&schema, condition.value, &mut params) else {
            return Expr::error(call.span);
        };
        if predicate.ty.dtype != DataType::Bool {
            let span = self.ast.span(condition.value);
            let message = format!(
                "the condition of `where` must be a bool, found {}",
                predicate.ty
            );
            return self.error(span, message);
        }
        self.table(
            call.span,
            TableOp::Filter { predicate },
            vec![input],
            params,
            schema,
        )
    }

    /// Builds a projection of `input` with the given output columns.
    fn project(
        &self,
        span: Span,
        input: Expr,
        columns: Vec<(Arc<str>, plan::Expr)>,
        params: Vec<Expr>,
    ) -> Expr {
        let schema = Arc::new(plan::schema_of(&columns));
        let op = TableOp::Project {
            columns,
            schema: schema.clone(),
        };
        self.table(span, op, vec![input], params, schema)
    }

    fn verb_select(&mut self, call: &Call) -> Expr {
        let Some((input, schema)) = self.table_arg(call, 0) else {
            return Expr::error(call.span);
        };
        let mut columns: Vec<(Arc<str>, plan::Expr)> = Vec::new();
        let mut params = Vec::new();
        let mut failed = false;
        for arg in &call.args[1..] {
            let column = match arg.name {
                None => self
                    .column_name(call, &schema, arg.value)
                    .map(|f| pass_through(&f)),
                Some(name) => {
                    let expr = self.column_expr(&schema, arg.value, &mut params);
                    expr.map(|expr| (self.text(name.name), expr))
                }
            };
            let Some(column) = column else {
                failed = true;
                continue;
            };
            if columns.iter().any(|(name, _)| *name == column.0) {
                let span = self.ast.span(arg.value);
                self.error(span, format!("column `{}` is selected twice", column.0));
                failed = true;
            }
            columns.push(column);
        }
        if failed {
            return Expr::error(call.span);
        }
        if columns.is_empty() {
            return self.error(call.span, "`select` needs at least one column");
        }
        self.project(call.span, input, columns, params)
    }

    fn verb_drop(&mut self, call: &Call) -> Expr {
        let Some((input, schema)) = self.table_arg(call, 0) else {
            return Expr::error(call.span);
        };
        let mut dropped = Vec::new();
        for arg in &call.args[1..] {
            if arg.name.is_some() {
                self.all_positional(call, &[*arg]);
            } else if let Some(field) = self.column_name(call, &schema, arg.value) {
                dropped.push(field.name);
                continue;
            }
            return Expr::error(call.span);
        }
        let kept = schema
            .fields
            .iter()
            .filter(|field| !dropped.contains(&field.name));
        let columns: Vec<_> = kept.map(pass_through).collect();
        if columns.is_empty() {
            return self.error(call.span, "`drop` cannot remove every column");
        }
        self.project(call.span, input, columns, Vec::new())
    }

    fn verb_rename(&mut self, call: &Call) -> Expr {
        let Some((input, schema)) = self.table_arg(call, 0) else {
            return Expr::error(call.span);
        };
        let mut columns: Vec<_> = schema.fields.iter().map(pass_through).collect();
        for arg in &call.args[1..] {
            let Some(new_name) = arg.name else {
                let span = self.ast.span(arg.value);
                return self.error(span, "`rename` takes `new_name = old_name`");
            };
            let Some(old) = self.column_name(call, &schema, arg.value) else {
                return Expr::error(call.span);
            };
            let index = schema.index_of(&old.name).expect("column_name found it");
            columns[index].0 = self.text(new_name.name);
        }
        for (index, (name, _)) in columns.iter().enumerate() {
            if columns[..index].iter().any(|(earlier, _)| earlier == name) {
                let message = format!("after renaming, two columns are called `{name}`");
                return self.error(call.span, message);
            }
        }
        self.project(call.span, input, columns, Vec::new())
    }

    fn verb_derive(&mut self, call: &Call) -> Expr {
        let Some((mut input, mut schema)) = self.table_arg(call, 0) else {
            return Expr::error(call.span);
        };
        // The columns of one projection are computed from its input, so a column that uses
        // one defined earlier in the same `derive` starts a new projection.
        let mut input_schema = schema.clone();
        let mut pending: Vec<(Arc<str>, plan::Expr)> = Vec::new();
        let mut params = Vec::new();
        let mut failed = false;
        for arg in &call.args[1..] {
            let Some(name) = arg.name else {
                let span = self.ast.span(arg.value);
                self.error(span, "`derive` takes `name = expression`");
                failed = true;
                continue;
            };
            let name = self.text(name.name);
            let mut own_params = Vec::new();
            let Some(mut expr) = self.column_expr(&schema, arg.value, &mut own_params) else {
                failed = true;
                continue;
            };
            if expr.reads_any(|column| pending.iter().any(|(defined, _)| &**defined == column)) {
                let columns = Self::with_derived(&input_schema, std::mem::take(&mut pending));
                input = self.project(call.span, input, columns, std::mem::take(&mut params));
                input_schema = schema.clone();
            }
            expr = shift_params(&expr, params.len());
            params.extend(own_params);
            schema = Arc::new(with_column(&schema, &name, expr.ty));
            pending.retain(|(defined, _)| *defined != name);
            pending.push((name, expr));
        }
        if failed {
            return Expr::error(call.span);
        }
        if pending.is_empty() {
            return self.error(call.span, "`derive` needs at least one `name = expression`");
        }
        let columns = Self::with_derived(&input_schema, pending);
        self.project(call.span, input, columns, params)
    }

    /// The columns of `input` with `derived` replacing the ones of the same name and the rest
    /// appended.
    fn with_derived(
        input: &Schema,
        derived: Vec<(Arc<str>, plan::Expr)>,
    ) -> Vec<(Arc<str>, plan::Expr)> {
        let mut columns: Vec<_> = input.fields.iter().map(pass_through).collect();
        for (name, expr) in derived {
            match columns.iter_mut().find(|(existing, _)| *existing == name) {
                Some(column) => column.1 = expr,
                None => columns.push((name, expr)),
            }
        }
        columns
    }

    /// Checks a sort key: an expression, optionally wrapped in `desc(...)` or `asc(...)`.
    fn sort_key(
        &mut self,
        schema: &Arc<Schema>,
        value: ExprId,
        params: &mut Vec<Expr>,
    ) -> Option<SortKey> {
        let ast = self.ast;
        let mut key = (value, false);
        if let ast::Expr::Call {
            callee,
            type_args,
            args,
        } = ast.expr(value)
            && let ast::Expr::Name(name) = ast.expr(*callee)
            && let [arg] = &args[..]
            && arg.name.is_none()
            && type_args.is_empty()
        {
            match self.interner.resolve(*name) {
                "desc" => key = (arg.value, true),
                "asc" => key = (arg.value, false),
                _ => {}
            }
        }
        let expr = self.column_expr(schema, key.0, params)?;
        Some(SortKey {
            expr,
            descending: key.1,
        })
    }

    fn verb_sort(&mut self, call: &Call) -> Expr {
        let Some((input, schema)) = self.table_arg(call, 0) else {
            return Expr::error(call.span);
        };
        if !self.all_positional(call, call.args) {
            return Expr::error(call.span);
        }
        let mut params = Vec::new();
        let mut keys = Vec::new();
        for arg in &call.args[1..] {
            match self.sort_key(&schema, arg.value, &mut params) {
                Some(key) => keys.push(key),
                None => return Expr::error(call.span),
            }
        }
        if keys.is_empty() {
            return self.error(call.span, "`sort` needs at least one key");
        }
        self.table(
            call.span,
            TableOp::Sort { keys },
            vec![input],
            params,
            schema,
        )
    }

    fn verb_limit(&mut self, call: &Call, op: TableOp) -> Expr {
        let Some((input, schema)) = self.table_arg(call, 0) else {
            return Expr::error(call.span);
        };
        let [_, count] = call.args else {
            let message = format!("`{}` takes the number of rows", call.name);
            return self.error(call.span, message);
        };
        if !self.all_positional(call, call.args) {
            return Expr::error(call.span);
        }
        let Some(count) = self.scalar_arg(count.value, Type::Int, "the number of rows") else {
            return Expr::error(call.span);
        };
        self.table(call.span, op, vec![input], vec![count], schema)
    }

    /// Groups `input` by all of its columns and keeps one row per group.
    fn distinct_rows(&self, span: Span, input: Expr, schema: Arc<Schema>) -> Expr {
        let keys = schema.fields.iter().map(pass_through).collect();
        let grouped = self.table(
            span,
            TableOp::Group { keys },
            vec![input],
            Vec::new(),
            schema.clone(),
        );
        let op = TableOp::Aggregate {
            aggs: Vec::new(),
            schema: schema.clone(),
        };
        self.table(span, op, vec![grouped], Vec::new(), schema)
    }

    fn verb_distinct(&mut self, call: &Call) -> Expr {
        let Some((input, schema)) = self.table_arg(call, 0) else {
            return Expr::error(call.span);
        };
        if !self.all_positional(call, call.args) {
            return Expr::error(call.span);
        }
        if call.args.len() == 1 {
            return self.distinct_rows(call.span, input, schema);
        }
        let mut columns: Vec<(Arc<str>, plan::Expr)> = Vec::new();
        for arg in &call.args[1..] {
            let Some(field) = self.column_name(call, &schema, arg.value) else {
                return Expr::error(call.span);
            };
            if !columns.iter().any(|(name, _)| *name == field.name) {
                columns.push(pass_through(&field));
            }
        }
        let selected = self.project(call.span, input, columns, Vec::new());
        let Type::Table(schema) = &selected.ty else {
            unreachable!("a projection is a table");
        };
        let schema = schema.clone();
        self.distinct_rows(call.span, selected, schema)
    }

    fn verb_group(&mut self, call: &Call) -> Expr {
        let Some((input, schema)) = self.table_arg(call, 0) else {
            return Expr::error(call.span);
        };
        let mut keys: Vec<(Arc<str>, plan::Expr)> = Vec::new();
        let mut params = Vec::new();
        for arg in &call.args[1..] {
            let key = match arg.name {
                None => self
                    .column_name(call, &schema, arg.value)
                    .map(|f| pass_through(&f)),
                Some(name) => {
                    let expr = self.column_expr(&schema, arg.value, &mut params);
                    expr.map(|expr| (self.text(name.name), expr))
                }
            };
            let Some(key) = key else {
                return Expr::error(call.span);
            };
            if keys.iter().any(|(name, _)| *name == key.0) {
                let span = self.ast.span(arg.value);
                return self.error(span, format!("`{}` is a group column twice", key.0));
            }
            keys.push(key);
        }
        if keys.is_empty() {
            let message = "`group` needs at least one column; \
                           to aggregate the whole table, call `agg` on it directly";
            return self.error(call.span, message);
        }
        let grouped = Grouped {
            input: schema,
            keys: plan::schema_of(&keys).fields,
        };
        let table = TableExpr {
            op: TableOp::Group { keys },
            inputs: vec![input],
            params,
        };
        let ty = Type::Grouped(Arc::new(grouped));
        Expr::new(ExprKind::Table(Box::new(table)), ty, call.span)
    }

    fn verb_agg(&mut self, call: &Call) -> Expr {
        let Some(first) = call.args.first().filter(|arg| arg.name.is_none()) else {
            let message =
                "`agg` needs a table or a grouped table, as in `t |> group(k) |> agg(...)`";
            return self.error(call.span, message);
        };
        let input = self.plain(first.value);
        let (input_schema, keys) = match &input.ty {
            Type::Grouped(grouped) => (grouped.input.clone(), grouped.keys.clone()),
            Type::Table(schema) => (schema.clone(), Vec::new()),
            Type::Error => return Expr::error(call.span),
            other => {
                let message = format!("`agg` works on a table or a grouped table, found {other}");
                return self.error(input.span, message);
            }
        };
        // Outside an aggregate call an expression may name the group columns, including the
        // computed ones that the input table does not have.
        let mut scope = (*input_schema).clone();
        for key in &keys {
            if scope.field(&key.name).is_none() {
                scope.fields.push(key.clone());
            }
        }
        let scope = Arc::new(scope);
        let mode = Mode::Agg {
            grouped: !keys.is_empty(),
        };
        let reserved = self.reserved_names(&scope, &call.args[1..]);
        let taken = |name: &str| reserved.iter().any(|reserved| &**reserved == name);

        let (mut aggs, mut no_windows, mut params) = (Vec::new(), Vec::new(), Vec::new());
        let mut outputs: Vec<(Arc<str>, Expr)> = Vec::new();
        let mut failed = false;
        for arg in &call.args[1..] {
            let Some(name) = arg.name else {
                let span = self.ast.span(arg.value);
                self.error(
                    span,
                    "`agg` takes `name = aggregate`, as in `total = sum(x)`",
                );
                failed = true;
                continue;
            };
            let text = self.text(name.name);
            let exists = keys.iter().any(|key| key.name == text)
                || outputs.iter().any(|(output, _)| *output == text);
            if exists {
                self.error(
                    name.span,
                    format!("the result already has a column `{text}`"),
                );
                failed = true;
                continue;
            }
            let Some(expr) = self.checked_in(&scope, mode, arg.value) else {
                failed = true;
                continue;
            };
            let mut extract = Extract {
                taken: &taken,
                direct: matches!(expr.kind, ExprKind::Agg(..)).then(|| text.clone()),
                aggs: &mut aggs,
                windows: &mut no_windows,
                params: &mut params,
            };
            match self.extract(expr, &mut extract) {
                Some(expr) => outputs.push((text, expr)),
                None => failed = true,
            }
        }
        if failed {
            return Expr::error(call.span);
        }
        if outputs.is_empty() {
            return self.error(call.span, "`agg` needs at least one `name = aggregate`");
        }
        let reads_computed_key = |agg: &(Arc<str>, AggCall)| {
            let mut args = agg.1.arg.iter().chain(&agg.1.arg2);
            args.any(|arg| arg.reads_any(|column| input_schema.field(column).is_none()))
        };
        if aggs.iter().any(reads_computed_key) {
            let message = "a group column computed in `group` cannot be used inside an aggregate";
            return self.error(call.span, message);
        }

        let mut fields = keys.clone();
        fields.extend(
            aggs.iter()
                .map(|(name, agg)| Field::new(name.clone(), agg.ty)),
        );
        let schema = Arc::new(Schema::new(fields));
        let op = TableOp::Aggregate {
            aggs,
            schema: schema.clone(),
        };
        let aggregate = self.table(call.span, op, vec![input], params, schema.clone());
        let computed = |(name, expr): &(Arc<str>, Expr)| !matches!(&expr.kind, ExprKind::Column(column) if column == name);
        if !outputs.iter().any(computed) {
            return aggregate;
        }

        // Some outputs compute with aggregates, as in `sum(x) / count()`: a projection over
        // the aggregation does that.
        let mut columns: Vec<_> = keys.iter().map(pass_through).collect();
        let mut post_params = Vec::new();
        for (name, expr) in outputs {
            if let Some(stray) = find_column(&expr, &|column| schema.field(column).is_none()) {
                let message =
                    "a column must be aggregated, as in `sum(x)`, unless it is a group column";
                return self.error(stray.span, message);
            }
            match self.plan_expr(expr, &mut post_params) {
                Some(expr) => columns.push((name, expr)),
                None => return Expr::error(call.span),
            }
        }
        self.project(call.span, aggregate, columns, post_params)
    }

    /// The names that a generated column must avoid: the columns in scope and the outputs.
    fn reserved_names(&self, scope: &Schema, outputs: &[ArgSrc]) -> Vec<Arc<str>> {
        let outputs = outputs.iter().filter_map(|arg| arg.name);
        let outputs = outputs.map(|name| self.text(name.name));
        scope.names().cloned().chain(outputs).collect()
    }

    /// Checks a call to an aggregate function inside `agg`.
    fn agg_call(&mut self, func: AggFn, grouped: bool, call: &Call) -> Expr {
        if !self.all_positional(call, call.args) {
            return Expr::error(call.span);
        }
        let schema = self
            .columns
            .last()
            .expect("called in a column scope")
            .schema
            .clone();
        let wanted = if func.is_pair() { 2 } else { 1 };
        if call.args.len() > wanted {
            let message = match func.is_pair() {
                true => format!("`{0}` takes two columns, as in `{0}(y, x)`", call.name),
                false => format!("`{}` takes one column", call.name),
            };
            return self.error(call.span, message);
        }
        let mut args = Vec::with_capacity(call.args.len());
        let mut types = Vec::with_capacity(call.args.len());
        for arg in call.args {
            let Some(arg) = self.checked_in(&schema, Mode::Row, arg.value) else {
                return Expr::error(call.span);
            };
            let Some(ty) = self.col_type(&arg.ty, arg.span) else {
                return Expr::error(call.span);
            };
            types.push(ty);
            args.push(arg);
        }
        match agg_type(func, types.first().copied(), types.get(1).copied(), grouped) {
            Ok(ty) => Expr::new(ExprKind::Agg(func, args), Type::from_col(ty), call.span),
            Err(message) => self.error(call.span, message),
        }
    }

    /// Checks a call to a window function inside `window`.
    fn window_call(&mut self, func: WindowFn, call: &Call) -> Expr {
        if !self.all_positional(call, call.args) {
            return Expr::error(call.span);
        }
        let name = call.name;
        if matches!(func, WindowFn::Agg(agg) if agg.is_pair()) {
            let message = format!("`{name}` works in `agg`, not in `window`");
            return self.error(call.span, message);
        }
        let schema = self
            .columns
            .last()
            .expect("called in a column scope")
            .schema
            .clone();
        let (value, rows) = match (func, call.args) {
            (WindowFn::RowNumber | WindowFn::Rank, []) => (None, None),
            (WindowFn::RowNumber | WindowFn::Rank, _) => {
                return self.error(call.span, format!("`{name}` takes no arguments"));
            }
            (WindowFn::Agg(AggFn::Count), []) => (None, None),
            (WindowFn::Lag | WindowFn::Lead | WindowFn::CumSum | WindowFn::Agg(_), [value]) => {
                (Some(value), None)
            }
            (WindowFn::Lag | WindowFn::Lead | WindowFn::MovingAvg, [value, rows]) => {
                (Some(value), Some(rows))
            }
            (WindowFn::MovingAvg, _) => {
                let message = "`moving_avg` takes a column and a number of rows";
                return self.error(call.span, message);
            }
            _ => return self.error(call.span, format!("`{name}` takes one column")),
        };
        let offset = match rows {
            None => 1,
            Some(rows) => match self.ast.expr(rows.value) {
                ast::Expr::Int(rows) if *rows >= 1 => *rows as usize,
                _ => {
                    let span = self.ast.span(rows.value);
                    let message =
                        "the number of rows must be written as a whole number of at least 1";
                    return self.error(span, message);
                }
            },
        };
        let arg = match value {
            Some(value) => match self.checked_in(&schema, Mode::Row, value.value) {
                Some(arg) => Some(arg),
                None => return Expr::error(call.span),
            },
            None => None,
        };
        let arg_ty = match &arg {
            Some(arg) => match self.col_type(&arg.ty, arg.span) {
                Some(ty) => Some(ty),
                None => return Expr::error(call.span),
            },
            None => None,
        };
        let numeric = arg_ty.is_some_and(|ty| matches!(ty.dtype, DataType::Int | DataType::Float));
        let ty = match (func, arg_ty) {
            (WindowFn::RowNumber | WindowFn::Rank, _) => Ok(ColType::required(DataType::Int)),
            (WindowFn::Lag | WindowFn::Lead, Some(arg)) => Ok(ColType::nullable(arg.dtype)),
            (WindowFn::CumSum, Some(arg)) if numeric => Ok(arg),
            (WindowFn::MovingAvg, Some(arg)) if numeric => {
                Ok(ColType::new(DataType::Float, arg.nullable))
            }
            (WindowFn::CumSum | WindowFn::MovingAvg, Some(arg)) => {
                Err(format!("`{name}` needs numbers, found {arg}"))
            }
            // A partition always has at least one row.
            (WindowFn::Agg(agg), arg) => agg_type(agg, arg, None, true),
            _ => Err(format!("`{name}` needs a column")),
        };
        match ty {
            Ok(ty) => {
                let kind = ExprKind::Window(func, arg.map(Box::new), offset);
                Expr::new(kind, Type::from_col(ty), call.span)
            }
            Err(message) => self.error(call.span, message),
        }
    }

    /// The expressions of an argument that is one item or a list of them.
    fn one_or_many(&self, value: ExprId) -> Vec<ExprId> {
        match self.ast.expr(value) {
            ast::Expr::List(items) => items.clone(),
            _ => vec![value],
        }
    }

    fn verb_window(&mut self, call: &Call) -> Expr {
        let Some((input, schema)) = self.table_arg(call, 0) else {
            return Expr::error(call.span);
        };
        let (mut partition, mut order, mut params) = (Vec::new(), Vec::new(), Vec::new());
        let mut output_args = Vec::new();
        for arg in &call.args[1..] {
            let Some(name) = arg.name else {
                let span = self.ast.span(arg.value);
                let message = "`window` takes `by = ...`, `order = ...`, and `name = function`";
                return self.error(span, message);
            };
            match &*self.text(name.name) {
                "by" => {
                    for value in self.one_or_many(arg.value) {
                        match self.column_expr(&schema, value, &mut params) {
                            Some(expr) => partition.push(expr),
                            None => return Expr::error(call.span),
                        }
                    }
                }
                "order" => {
                    for value in self.one_or_many(arg.value) {
                        match self.sort_key(&schema, value, &mut params) {
                            Some(key) => order.push(key),
                            None => return Expr::error(call.span),
                        }
                    }
                }
                _ => output_args.push(*arg),
            }
        }
        let reserved = self.reserved_names(&schema, &output_args);
        let taken = |name: &str| reserved.iter().any(|reserved| &**reserved == name);

        let (mut no_aggs, mut windows) = (Vec::new(), Vec::new());
        let mut outputs: Vec<(Arc<str>, Expr)> = Vec::new();
        for arg in &output_args {
            let name = arg.name.expect("only named arguments were kept");
            let text = self.text(name.name);
            let exists =
                schema.field(&text).is_some() || outputs.iter().any(|(output, _)| *output == text);
            if exists {
                let message = format!("the table already has a column `{text}`");
                return self.error(name.span, message);
            }
            let Some(expr) = self.checked_in(&schema, Mode::Window, arg.value) else {
                return Expr::error(call.span);
            };
            let mut extract = Extract {
                taken: &taken,
                direct: matches!(expr.kind, ExprKind::Window(..)).then(|| text.clone()),
                aggs: &mut no_aggs,
                windows: &mut windows,
                params: &mut params,
            };
            match self.extract(expr, &mut extract) {
                Some(expr) => outputs.push((text, expr)),
                None => return Expr::error(call.span),
            }
        }
        if windows.is_empty() {
            let message = "`window` needs at least one window function, as in `n = row_number()`";
            return self.error(call.span, message);
        }

        let mut fields = schema.fields.clone();
        fields.extend(
            windows
                .iter()
                .map(|(name, call)| Field::new(name.clone(), call.ty)),
        );
        let window_schema = Arc::new(Schema::new(fields));
        let op = TableOp::Window {
            partition,
            order,
            funcs: windows,
            schema: window_schema.clone(),
        };
        let window = self.table(call.span, op, vec![input], params, window_schema);
        let computed = |(name, expr): &(Arc<str>, Expr)| !matches!(&expr.kind, ExprKind::Column(column) if column == name);
        if !outputs.iter().any(computed) {
            return window;
        }
        let mut columns: Vec<_> = schema.fields.iter().map(pass_through).collect();
        let mut post_params = Vec::new();
        for (name, expr) in outputs {
            match self.plan_expr(expr, &mut post_params) {
                Some(expr) => columns.push((name, expr)),
                None => return Expr::error(call.span),
            }
        }
        self.project(call.span, window, columns, post_params)
    }

    /// The key columns named by a join argument: one name or a list of names.
    fn key_columns(&mut self, call: &Call, schema: &Schema, value: ExprId) -> Option<Vec<Field>> {
        let mut keys = Vec::new();
        for value in self.one_or_many(value) {
            keys.push(self.column_name(call, schema, value)?);
        }
        Some(keys)
    }

    fn verb_join(&mut self, call: &Call) -> Expr {
        let Some((left, left_schema)) = self.table_arg(call, 0) else {
            return Expr::error(call.span);
        };
        let Some((right, right_schema)) = self.table_arg(call, 1) else {
            return Expr::error(call.span);
        };
        let (mut on, mut left_on, mut right_on) = (None, None, None);
        let mut how = "inner";
        for arg in &call.args[2..] {
            let Some(name) = arg.name else {
                let span = self.ast.span(arg.value);
                let message =
                    "after its two tables, `join` takes `on`, `left_on`, `right_on`, and `how`";
                return self.error(span, message);
            };
            match &*self.text(name.name) {
                "on" => on = Some(arg.value),
                "left_on" => left_on = Some(arg.value),
                "right_on" => right_on = Some(arg.value),
                "how" => {
                    how = match self.ast.expr(arg.value) {
                        ast::Expr::Str(how) => match &**how {
                            "inner" => "inner",
                            "left" => "left",
                            "right" => "right",
                            "full" => "full",
                            "semi" => "semi",
                            "anti" => "anti",
                            _ => "",
                        },
                        _ => "",
                    };
                    if how.is_empty() {
                        let span = self.ast.span(arg.value);
                        let message = "`how` must be one of \"inner\", \"left\", \"right\", \
                                       \"full\", \"semi\", or \"anti\"";
                        return self.error(span, message);
                    }
                }
                other => {
                    let message = format!("`join` has no argument `{other}`");
                    return self.error(name.span, message);
                }
            }
        }
        let shared = on.is_some();
        let (left_keys, right_keys) = match (on, left_on, right_on) {
            (Some(on), None, None) => (
                self.key_columns(call, &left_schema, on),
                self.key_columns(call, &right_schema, on),
            ),
            (None, Some(left_on), Some(right_on)) => (
                self.key_columns(call, &left_schema, left_on),
                self.key_columns(call, &right_schema, right_on),
            ),
            _ => {
                let message = "`join` needs `on = key`, or both `left_on` and `right_on`";
                return self.error(call.span, message);
            }
        };
        let (Some(left_keys), Some(right_keys)) = (left_keys, right_keys) else {
            return Expr::error(call.span);
        };
        if left_keys.len() != right_keys.len() || left_keys.is_empty() {
            let message = "`left_on` and `right_on` must name the same number of columns";
            return self.error(call.span, message);
        }
        for (l, r) in left_keys.iter().zip(&right_keys) {
            if l.ty.dtype != r.ty.dtype {
                let message = format!(
                    "key `{}` is {} on the left but `{}` is {} on the right; convert one first",
                    l.name,
                    l.ty.dtype.name(),
                    r.name,
                    r.ty.dtype.name()
                );
                return self.error(call.span, message);
            }
        }

        // A right join runs as a left join with its inputs exchanged.
        let swap = how == "right";
        let kind = match how {
            "inner" => JoinKind::Inner,
            "left" | "right" => JoinKind::Left,
            "full" => JoinKind::Full,
            "semi" => JoinKind::Semi,
            _ => JoinKind::Anti,
        };
        let keeps_right = !matches!(kind, JoinKind::Semi | JoinKind::Anti);
        let from_left = |name: &Arc<str>| match swap {
            false => JoinColumn::Left(name.clone()),
            true => JoinColumn::Right(name.clone()),
        };
        let from_right = |name: &Arc<str>| match swap {
            false => JoinColumn::Right(name.clone()),
            true => JoinColumn::Left(name.clone()),
        };
        let mut columns = Vec::new();
        let mut fields = Vec::new();
        for field in &left_schema.fields {
            let key = left_keys.iter().position(|key| key.name == field.name);
            match key.filter(|_| shared && keeps_right) {
                // A shared key is one column: it holds whichever side has the row.
                Some(key) => {
                    let (l, r) = (&left_keys[key], &right_keys[key]);
                    let nullable = match how {
                        "right" => r.ty.nullable,
                        "full" => l.ty.nullable || r.ty.nullable,
                        _ => l.ty.nullable,
                    };
                    let column = match swap {
                        false => JoinColumn::Key(l.name.clone(), r.name.clone()),
                        true => JoinColumn::Key(r.name.clone(), l.name.clone()),
                    };
                    columns.push((field.name.clone(), column));
                    fields.push(Field::new(
                        field.name.clone(),
                        ColType::new(l.ty.dtype, nullable),
                    ));
                }
                None => {
                    let nullable = field.ty.nullable || matches!(how, "right" | "full");
                    columns.push((field.name.clone(), from_left(&field.name)));
                    fields.push(Field::new(
                        field.name.clone(),
                        ColType::new(field.ty.dtype, nullable),
                    ));
                }
            }
        }
        if keeps_right {
            for field in &right_schema.fields {
                if shared && right_keys.iter().any(|key| key.name == field.name) {
                    continue;
                }
                if left_schema.field(&field.name).is_some() {
                    let message = format!(
                        "both tables have a column `{}`; rename or drop one before the join",
                        field.name
                    );
                    return self.error(call.span, message);
                }
                let nullable = field.ty.nullable || matches!(how, "left" | "full");
                columns.push((field.name.clone(), from_right(&field.name)));
                fields.push(Field::new(
                    field.name.clone(),
                    ColType::new(field.ty.dtype, nullable),
                ));
            }
        }
        let key_expr = |field: &Field| plan::Expr::column(field.name.clone(), field.ty);
        let on = left_keys.iter().zip(&right_keys).map(|(l, r)| match swap {
            false => (key_expr(l), key_expr(r)),
            true => (key_expr(r), key_expr(l)),
        });
        let schema = Arc::new(Schema::new(fields));
        let op = TableOp::Join {
            kind,
            on: on.collect(),
            columns,
            schema: schema.clone(),
        };
        let inputs = if swap {
            vec![right, left]
        } else {
            vec![left, right]
        };
        self.table(call.span, op, inputs, Vec::new(), schema)
    }

    fn verb_union(&mut self, call: &Call) -> Expr {
        if call.args.len() < 2 || !self.all_positional(call, call.args) {
            return self.error(call.span, "`union` takes two or more tables");
        }
        let mut inputs = Vec::new();
        let mut fields: Vec<Field> = Vec::new();
        for index in 0..call.args.len() {
            let Some((input, schema)) = self.table_arg(call, index) else {
                return Expr::error(call.span);
            };
            if index == 0 {
                fields = schema.fields.clone();
            } else {
                let same = fields.len() == schema.fields.len()
                    && fields
                        .iter()
                        .zip(&schema.fields)
                        .all(|(a, b)| a.name == b.name && a.ty.dtype == b.ty.dtype);
                if !same {
                    let message = format!(
                        "the tables of a `union` need the same columns, found {} and {schema}",
                        Schema::new(fields)
                    );
                    return self.error(input.span, message);
                }
                for (field, other) in fields.iter_mut().zip(&schema.fields) {
                    field.ty.nullable |= other.ty.nullable;
                }
            }
            inputs.push(input);
        }
        self.table(
            call.span,
            TableOp::Union,
            inputs,
            Vec::new(),
            Arc::new(Schema::new(fields)),
        )
    }

    /// A test that is true where `subject` equals `value`. Where either is null the test is
    /// false: a null equals nothing.
    pub(crate) fn equals(&mut self, span: Span, subject: Expr, value: Expr) -> Option<Expr> {
        let equal = self.binary(span, BinaryOp::Eq, subject, value);
        match &equal.ty {
            Type::Bool => Some(equal),
            Type::Error => None,
            _ => {
                let no = Expr::new(ExprKind::Bool(false), Type::Bool, span);
                let kind = ExprKind::Binary(BinaryOp::Coalesce, Box::new(equal), Box::new(no));
                Some(Expr::new(kind, Type::Bool, span))
            }
        }
    }

    /// Checks a call to a function that works on single values and on columns alike.
    fn scalar_call(&mut self, func: ScalarFn, call: &Call) -> Expr {
        use DataType::*;
        #[derive(Clone, Copy, PartialEq)]
        enum Want {
            Any,
            /// An int, a float, or a decimal, which stays what it is.
            Number,
            /// A float; an int or a decimal is converted to one.
            Float,
            /// A float or a decimal; an int is converted to a float.
            Fraction,
            Str,
            /// A date or a datetime.
            Day,
            /// A datetime; a date is converted to one.
            Moment,
            Duration,
            /// An int or a float.
            Amount,
            /// An int that is not null.
            Count,
        }
        let name = call.name;
        if !self.all_positional(call, call.args) {
            return Expr::error(call.span);
        }
        let mut args: Vec<Expr> = call.args.iter().map(|arg| self.expr(arg.value)).collect();
        if args.iter().any(|arg| arg.ty.is_error()) {
            return Expr::error(call.span);
        }
        let wants: &[Want] = match func {
            ScalarFn::IsNull
            | ScalarFn::ToString
            | ScalarFn::ToInt
            | ScalarFn::ToFloat
            | ScalarFn::ToDecimal
            | ScalarFn::ToDate
            | ScalarFn::ToDateTime => &[Want::Any],
            ScalarFn::Abs => &[Want::Number],
            ScalarFn::Round if args.len() == 2 => &[Want::Fraction, Want::Count],
            ScalarFn::Round => &[Want::Fraction],
            ScalarFn::Floor | ScalarFn::Ceil | ScalarFn::Sqrt => &[Want::Float],
            ScalarFn::Lower | ScalarFn::Upper | ScalarFn::Trim | ScalarFn::Length => &[Want::Str],
            ScalarFn::Contains | ScalarFn::StartsWith | ScalarFn::EndsWith => {
                &[Want::Str, Want::Str]
            }
            ScalarFn::Year | ScalarFn::Month | ScalarFn::Day => &[Want::Day],
            ScalarFn::Hour | ScalarFn::Minute | ScalarFn::Second => &[Want::Moment],
            ScalarFn::Days | ScalarFn::Hours | ScalarFn::Minutes | ScalarFn::Seconds => {
                &[Want::Amount]
            }
            ScalarFn::TotalSeconds => &[Want::Duration],
        };
        if args.len() != wants.len() {
            let plural = if wants.len() == 1 { "" } else { "s" };
            let message = format!("`{name}` takes {} argument{plural}", wants.len());
            return self.error(call.span, message);
        }
        let mut nullable = false;
        let mut first = Int;
        for (index, (arg, want)) in args.iter_mut().zip(wants).enumerate() {
            let Some(ty) = arg.ty.to_col() else {
                let message = match arg.ty {
                    Type::Null => format!("cannot tell which type this `null` has for `{name}`"),
                    _ => format!("`{name}` cannot work on {}", arg.ty),
                };
                return self.error(arg.span, message);
            };
            let number = matches!(ty.dtype, Int | Float | Decimal);
            let accepted = match want {
                Want::Any => match func {
                    ScalarFn::ToInt => !matches!(ty.dtype, Date | DateTime | Duration),
                    ScalarFn::ToFloat | ScalarFn::ToDecimal => number || ty.dtype == Str,
                    ScalarFn::ToDate | ScalarFn::ToDateTime => {
                        matches!(ty.dtype, Str | Date | DateTime)
                    }
                    _ => true,
                },
                Want::Number | Want::Float | Want::Fraction => number,
                Want::Str => ty.dtype == Str,
                Want::Day | Want::Moment => matches!(ty.dtype, Date | DateTime),
                Want::Duration => ty.dtype == Duration,
                Want::Amount => matches!(ty.dtype, Int | Float),
                Want::Count => ty == ColType::required(Int),
            };
            if !accepted {
                let expected = match want {
                    Want::Number | Want::Float | Want::Fraction | Want::Amount => "a number",
                    Want::Str => "a string",
                    Want::Day => "a date or a datetime",
                    Want::Moment => "a datetime",
                    Want::Duration => "a duration",
                    Want::Count => "an int",
                    Want::Any => {
                        let message = format!("`{name}` cannot convert a value of type {}", arg.ty);
                        return self.error(arg.span, message);
                    }
                };
                let message = format!("`{name}` expects {expected}, found {}", arg.ty);
                return self.error(arg.span, message);
            }
            let converted = match (want, ty.dtype) {
                (Want::Float, Int | Decimal) | (Want::Fraction, Int) => Some(Float),
                (Want::Moment, Date) => Some(DateTime),
                _ => None,
            };
            let dtype = converted.unwrap_or(ty.dtype);
            if converted.is_some() {
                let old = std::mem::replace(arg, Expr::error(arg.span));
                let to = Type::from_col(ColType::new(dtype, ty.nullable));
                *arg = coerce(old, &to).unwrap_or_else(|old| old);
            }
            if index == 0 {
                first = dtype;
            }
            nullable |= ty.nullable;
        }
        let dtype = match func {
            ScalarFn::IsNull | ScalarFn::Contains | ScalarFn::StartsWith | ScalarFn::EndsWith => {
                Bool
            }
            // A decimal stays exact when rounded.
            ScalarFn::Abs | ScalarFn::Round => first,
            ScalarFn::Floor
            | ScalarFn::Ceil
            | ScalarFn::Sqrt
            | ScalarFn::ToFloat
            | ScalarFn::TotalSeconds => Float,
            ScalarFn::Lower | ScalarFn::Upper | ScalarFn::Trim | ScalarFn::ToString => Str,
            ScalarFn::Length
            | ScalarFn::Year
            | ScalarFn::Month
            | ScalarFn::Day
            | ScalarFn::Hour
            | ScalarFn::Minute
            | ScalarFn::Second
            | ScalarFn::ToInt => Int,
            ScalarFn::ToDate => Date,
            ScalarFn::ToDateTime => DateTime,
            ScalarFn::ToDecimal => Decimal,
            ScalarFn::Days | ScalarFn::Hours | ScalarFn::Minutes | ScalarFn::Seconds => Duration,
        };
        let nullable = nullable && func != ScalarFn::IsNull;
        let ty = Type::from_col(ColType::new(dtype, nullable));
        Expr::new(ExprKind::Scalar(func, args), ty, call.span)
    }
}

fn is_number(ty: ColType) -> bool {
    matches!(
        ty.dtype,
        DataType::Int | DataType::Float | DataType::Decimal
    )
}

/// The first column read by `expr` whose name satisfies `wanted`.
fn find_column<'e>(expr: &'e Expr, wanted: &dyn Fn(&str) -> bool) -> Option<&'e Expr> {
    if matches!(&expr.kind, ExprKind::Column(name) if wanted(name)) {
        return Some(expr);
    }
    let mut found = None;
    expr.for_each_child(&mut |child| {
        if found.is_none() {
            found = find_column(child, wanted);
        }
    });
    found
}
