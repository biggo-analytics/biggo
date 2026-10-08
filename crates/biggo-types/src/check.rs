use std::sync::Arc;

use biggo_plan::{self as plan, Field, ScalarFn, Schema, TableOp, scalar};
use biggo_syntax::ast::{
    self, Ast, BinaryOp, ExprId, Ident, LambdaParam, MatchArm, Pattern, StmtKind, TypeExpr,
    TypeKind, UnaryOp,
};
use biggo_syntax::{Diagnostic, Interner, Span, Symbol};

use crate::hir::{Arg, Callee, Conversion, Expr, ExprKind, Function, Program, Stmt, TableExpr};
use crate::ty::{FnType, Param, Type};
use crate::verbs::{ColumnScope, Mode, is_builtin_name};

/// Type-checks modules one after another. The names a module defines at its top level stay
/// visible to the modules checked after it, which is how a REPL session accumulates.
#[derive(Default)]
pub struct Checker {
    /// Top-level names in definition order; a later entry shadows an earlier one.
    scope: Vec<(Symbol, Top)>,
    functions: Vec<Signature>,
    aliases: Vec<(Symbol, Type)>,
    globals: u32,
}

#[derive(Clone)]
enum Top {
    Global(u32, Type),
    Function(u32),
}

struct Signature {
    name: Arc<str>,
    params: Vec<Param>,
    /// Unknown until the body has been checked, if the declaration does not say.
    ret: Option<Type>,
    /// Whether this is a function written as a value, `fn(x) { ... }`.
    lambda: bool,
    /// The body of a function declared with `fn` at the top of a file, once it has been
    /// checked: what a call of the function on columns is replaced by.
    body: Option<Arc<Expr>>,
}

/// What the place where a lambda is written says about its type.
#[derive(Clone)]
pub(crate) struct FnHint {
    pub params: Vec<Type>,
    /// The type its body must have, where that is fixed.
    pub ret: Option<Type>,
}

/// A module that passed the type checker.
pub struct Checked {
    pub program: Program,
    /// The type of each expression of the module, indexed by `ExprId::index`.
    pub types: Vec<Option<Type>>,
}

/// The names that can be written at one place of a module, for an editor to offer there.
#[derive(Debug, PartialEq)]
pub struct Names {
    pub place: Place,
    /// Each name once, as its nearest declaration has it, the nearest names first.
    pub names: Vec<Named>,
}

/// What is written at a place of a module.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Place {
    /// An expression. It can also call a built-in function.
    Expr,
    /// The function of a call, which can also be a built-in one.
    Callee,
    /// One of the names and nothing else: a column where a table operation takes the name of
    /// one, or a field after the `.` of a record.
    Member,
    /// A type. The built-in types are not among the names.
    Type,
}

/// A name that can be written at a place of a module, with its type there.
#[derive(Clone, Debug, PartialEq)]
pub struct Named {
    pub name: Arc<str>,
    pub kind: NameKind,
    pub ty: Type,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NameKind {
    /// A column of the table that the enclosing table operation works on.
    Column,
    /// A field of the record before the `.`.
    Field,
    Variable,
    /// A function, or a variable that holds one.
    Function,
    /// A type declared with `type`.
    Type,
}

impl Checker {
    pub fn new() -> Self {
        Self::default()
    }

    /// How much the session holds, to go back to when a module does not join it.
    fn mark(&self) -> (usize, usize, usize, u32) {
        (
            self.scope.len(),
            self.functions.len(),
            self.aliases.len(),
            self.globals,
        )
    }

    fn rewind(&mut self, mark: (usize, usize, usize, u32)) {
        self.scope.truncate(mark.0);
        self.functions.truncate(mark.1);
        self.aliases.truncate(mark.2);
        self.globals = mark.3;
    }

    /// Checks `ast` and lowers it to HIR. On success its top-level definitions join the
    /// session; on failure the checker is left exactly as it was.
    pub fn check(
        &mut self,
        ast: &Ast,
        interner: &mut Interner,
    ) -> Result<Checked, Vec<Diagnostic>> {
        let saved = self.mark();
        let first_function = self.functions.len() as u32;
        let mut cx = Cx::new(self, ast, interner);
        let (main, result) = cx.module();
        let Cx {
            mut diags,
            functions,
            types,
            ..
        } = cx;
        if !diags.is_empty() {
            self.rewind(saved);
            diags.sort_by_key(|diag| diag.span.start);
            return Err(diags);
        }
        let functions = functions
            .into_iter()
            .map(|function| function.expect("a module without errors has checked every function"));
        let program = Program {
            functions: functions.collect(),
            first_function,
            main,
            globals: self.globals,
            result,
        };
        Ok(Checked { program, types })
    }

    /// The type of the one expression that `ast` consists of, with nothing joining the
    /// session. `None` if `ast` is anything else.
    pub fn type_of(
        &mut self,
        ast: &Ast,
        interner: &mut Interner,
    ) -> Result<Option<Type>, Vec<Diagnostic>> {
        let [stmt] = &ast.stmts[..] else {
            return Ok(None);
        };
        let StmtKind::Expr(expr) = stmt.kind else {
            return Ok(None);
        };
        let saved = self.mark();
        let checked = self.check(ast, interner);
        self.rewind(saved);
        Ok(checked?.types[expr.index()].clone())
    }

    /// The names that can be written at byte `offset` of `ast`, if a name, a field, or a type
    /// is written there. The module need not pass, and the checker is left exactly as it was.
    pub fn names_at(&mut self, ast: &Ast, interner: &mut Interner, offset: u32) -> Option<Names> {
        let saved = self.mark();
        let mut cx = Cx::new(self, ast, interner);
        cx.asked = Some(offset);
        cx.module();
        let names = cx.names;
        self.rewind(saved);
        names
    }
}

/// What is known about a function while its body is being checked.
#[derive(Default)]
struct Frame {
    /// The variables of each enclosing block, innermost last.
    scopes: Vec<Vec<(Symbol, u32, Type)>>,
    locals: u32,
    /// Variables of enclosing functions that the body uses, each with the expression that
    /// reads it where this function is created.
    captures: Vec<(Symbol, Expr)>,
    /// The name and id of a local function, which can call itself.
    this: Option<(Symbol, u32)>,
}

/// An argument of a call as written: `value` or `name = value`. The left side of a `|>` is the
/// first argument.
#[derive(Clone, Copy)]
pub(crate) struct ArgSrc {
    pub name: Option<Ident>,
    pub value: ExprId,
}

pub(crate) struct Call<'c> {
    pub span: Span,
    pub name: &'c str,
    pub args: &'c [ArgSrc],
    pub type_args: &'c [TypeExpr],
}

/// The state of checking one module.
pub(crate) struct Cx<'a> {
    checker: &'a mut Checker,
    pub(crate) ast: &'a Ast,
    pub(crate) interner: &'a mut Interner,
    diags: Vec<Diagnostic>,
    functions: Vec<Option<Function>>,
    first_function: u32,
    frames: Vec<Frame>,
    /// The tables whose columns are in scope: one entry per enclosing table operation.
    pub(crate) columns: Vec<ColumnScope>,
    types: Vec<Option<Type>>,
    /// An argument that was checked to see which function a call means, kept for the
    /// function that then checks the call, so that it is not checked twice.
    pub(crate) peeked: Option<(ExprId, Expr)>,
    /// The function whose body, written out for a call on columns, is being turned into a
    /// part of a query.
    pub(crate) inlined: Option<Arc<str>>,
    /// The place whose names an editor asks for, as a byte offset.
    asked: Option<u32>,
    /// What can be written at that place, once the checker has come to it.
    pub(crate) names: Option<Names>,
}

const MICROS_PER_DAY: i64 = 86_400_000_000;

#[derive(Clone, Copy, PartialEq)]
enum Base {
    Null,
    Int,
    Float,
    Bool,
    Str,
    Date,
    DateTime,
    Duration,
    Decimal,
}

fn base(ty: &Type) -> Option<(Base, bool)> {
    let (inner, nullable) = ty.split_null();
    let base = match inner {
        Type::Null => Base::Null,
        Type::Int => Base::Int,
        Type::Float => Base::Float,
        Type::Bool => Base::Bool,
        Type::Str => Base::Str,
        Type::Date => Base::Date,
        Type::DateTime => Base::DateTime,
        Type::Duration => Base::Duration,
        Type::Decimal => Base::Decimal,
        _ => return None,
    };
    Some((base, nullable))
}

fn base_type(base: Base) -> Type {
    match base {
        Base::Null => Type::Null,
        Base::Int => Type::Int,
        Base::Float => Type::Float,
        Base::Bool => Type::Bool,
        Base::Str => Type::Str,
        Base::Date => Type::Date,
        Base::DateTime => Type::DateTime,
        Base::Duration => Type::Duration,
        Base::Decimal => Type::Decimal,
    }
}

/// Whether a list, a record, or a map can hold values of this type: they hold data, not
/// tables or functions.
pub(crate) fn storable(ty: &Type) -> bool {
    !matches!(
        ty,
        Type::Unit | Type::Table(_) | Type::Grouped(_) | Type::Fn(_)
    )
}

/// How a value of one type becomes a value of a wider one.
#[derive(Clone, Copy)]
enum Widening {
    ToFloat,
    Convert(Conversion),
}

/// The conversion from `from` to `to`, neither of them nullable, if `to` is the wider type.
fn widening(from: &Type, to: &Type) -> Option<Widening> {
    Some(match (from, to) {
        (Type::Int, Type::Float) => Widening::ToFloat,
        (Type::Int, Type::Decimal) => Widening::Convert(Conversion::IntToDecimal),
        (Type::Decimal, Type::Float) => Widening::Convert(Conversion::DecimalToFloat),
        (Type::Date, Type::DateTime) => Widening::Convert(Conversion::DateToDateTime),
        _ => return None,
    })
}

/// Applies a widening conversion; the result has type `ty`. A literal is converted here.
fn widen(expr: Expr, how: Widening, ty: Type) -> Expr {
    let span = expr.span;
    let kind = match (how, expr.kind) {
        (Widening::ToFloat, ExprKind::Int(value)) => ExprKind::Float(value as f64),
        (Widening::Convert(Conversion::IntToDecimal), ExprKind::Int(value)) => {
            ExprKind::Decimal(scalar::decimal_from_int(value))
        }
        (Widening::Convert(Conversion::DecimalToFloat), ExprKind::Decimal(value)) => {
            ExprKind::Float(scalar::decimal_to_float(value))
        }
        (Widening::Convert(Conversion::DateToDateTime), ExprKind::Date(days)) => {
            ExprKind::DateTime(i64::from(days) * MICROS_PER_DAY)
        }
        (Widening::ToFloat, kind) => ExprKind::ToFloat(Box::new(Expr::new(kind, expr.ty, span))),
        (Widening::Convert(conversion), kind) => {
            ExprKind::Convert(conversion, Box::new(Expr::new(kind, expr.ty, span)))
        }
    };
    Expr::new(kind, ty, span)
}

/// The type that values of both types convert to, if there is one.
pub(crate) fn unify(a: &Type, b: &Type) -> Option<Type> {
    if a == b {
        return Some(a.clone());
    }
    match (a, b) {
        (Type::Error, other) | (other, Type::Error) => Some(other.clone()),
        (Type::Null, other) | (other, Type::Null) => {
            storable(other).then(|| other.clone().or_null())
        }
        (Type::Nullable(inner), other) | (other, Type::Nullable(inner)) => {
            unify(inner, other.split_null().0).map(Type::or_null)
        }
        // An empty list literal has no element type of its own.
        (Type::List(element), Type::List(_)) if **element == Type::Null => Some(b.clone()),
        (Type::List(_), Type::List(element)) if **element == Type::Null => Some(a.clone()),
        (Type::List(x), Type::List(y)) => Some(Type::List(Box::new(unify(x, y)?))),
        (Type::Map(key_x, value_x), Type::Map(key_y, value_y)) => Some(Type::Map(
            Box::new(unify(key_x, key_y)?),
            Box::new(unify(value_x, value_y)?),
        )),
        (Type::Record(x), Type::Record(y)) if x.len() == y.len() => {
            let mut fields = Vec::with_capacity(x.len());
            for ((name, left), (other, right)) in x.iter().zip(y.iter()) {
                if name != other {
                    return None;
                }
                fields.push((name.clone(), unify(left, right)?));
            }
            Some(Type::Record(Arc::new(fields)))
        }
        _ if widening(a, b).is_some() => Some(b.clone()),
        _ if widening(b, a).is_some() => Some(a.clone()),
        _ => None,
    }
}

/// Gives `expr` the type `ty`, which its value already fits. The expressions that supply the
/// value of a block or an `if` get it too, so that a `null` among them knows its type.
fn retype(expr: &mut Expr, ty: &Type) {
    expr.ty = ty.clone();
    match &mut expr.kind {
        ExprKind::Block(_, Some(value)) => retype(value, ty),
        ExprKind::If(_, then, Some(otherwise)) => {
            retype(then, ty);
            retype(otherwise, ty);
        }
        _ => {}
    }
}

/// Whether every value of type `from` is, just as it is, a value of type `to`: the types
/// differ only in that `to` allows null in more places. Values never change after they are
/// made, so a list of ints can stand in for a list of ints that may be null.
fn fits(from: &Type, to: &Type) -> bool {
    if from == to {
        return true;
    }
    match (from, to) {
        (Type::Null, Type::Nullable(_)) => true,
        (Type::Nullable(from), Type::Nullable(to)) => fits(from, to),
        (from, Type::Nullable(to)) => fits(from, to),
        (Type::List(from), Type::List(to)) => fits(from, to),
        (Type::Map(from_key, from), Type::Map(to_key, to)) => from_key == to_key && fits(from, to),
        (Type::Record(from), Type::Record(to)) => {
            let fields = from.iter().zip(to.iter());
            from.len() == to.len()
                && fields
                    .into_iter()
                    .all(|(f, t)| f.0 == t.0 && fits(&f.1, &t.1))
        }
        _ => false,
    }
}

/// The columns of `wanted` as they are taken from a table with the columns of `have`, which
/// must have every one of them, of the same type; where `wanted` allows null, `have` need
/// not. An error says which column stands in the way.
pub(crate) fn narrowed(
    have: &Schema,
    wanted: &Schema,
) -> Result<Vec<(Arc<str>, plan::Expr)>, String> {
    let mut columns = Vec::with_capacity(wanted.fields.len());
    for field in &wanted.fields {
        let Some(held) = have.field(&field.name) else {
            return Err(format!("the table has no column `{}`", field.name));
        };
        let fits = held.ty.dtype == field.ty.dtype && (field.ty.nullable || !held.ty.nullable);
        if !fits {
            return Err(format!(
                "column `{}` is {}, where {} is wanted",
                field.name, held.ty, field.ty
            ));
        }
        columns.push((
            field.name.clone(),
            plan::Expr::column(field.name.clone(), held.ty),
        ));
    }
    Ok(columns)
}

/// Converts `expr` to type `to`, where a value of its type may stand in for one: an int for a
/// float or a decimal, a date for a datetime, a `T` or `null` for a `T?`, a table for one
/// with fewer columns. A list, record, or map that is written out converts part by part.
/// Gives the expression back if it may not.
pub(crate) fn coerce(mut expr: Expr, to: &Type) -> Result<Expr, Expr> {
    if expr.ty == *to || expr.ty.is_error() || to.is_error() {
        return Ok(expr);
    }
    let (to_inner, to_null) = to.split_null();
    let (from_inner, from_null) = expr.ty.split_null();
    if (to_null || !from_null)
        && let Some(how) = widening(from_inner, to_inner)
    {
        return Ok(widen(expr, how, to.clone()));
    }
    if fits(&expr.ty, to) {
        retype(&mut expr, to);
        return Ok(expr);
    }
    // A table with more columns than are wanted stands in as a selection of those.
    if let (Type::Table(have), Type::Table(wanted)) = (&expr.ty, to)
        && let Ok(columns) = narrowed(have, wanted)
    {
        let span = expr.span;
        let op = TableOp::Project {
            columns,
            schema: wanted.clone(),
        };
        let table = TableExpr {
            op,
            inputs: vec![expr],
            params: Vec::new(),
        };
        return Ok(Expr::new(
            ExprKind::Table(Box::new(table)),
            to.clone(),
            span,
        ));
    }
    match (to, &expr.ty) {
        (Type::Nullable(inner), from) if !matches!(from, Type::Nullable(_)) => {
            let mut expr = coerce(expr, inner)?;
            retype(&mut expr, to);
            Ok(expr)
        }
        _ => coerce_parts(expr, to),
    }
}

/// Converts the parts of a list, record, or map written out in place, or of the value of a
/// block or an `if`. On failure some parts may have been converted; the caller reports the
/// error, so the expression is not used.
fn coerce_parts(expr: Expr, to: &Type) -> Result<Expr, Expr> {
    let Expr { kind, ty, span } = expr;
    let mut failed = false;
    let mut part = |part: Expr, to: &Type| {
        coerce(part, to).unwrap_or_else(|part| {
            failed = true;
            part
        })
    };
    let kind = match (kind, to.split_null().0) {
        (ExprKind::List(items), Type::List(element)) => {
            ExprKind::List(items.into_iter().map(|item| part(item, element)).collect())
        }
        (ExprKind::Record(names, values), Type::Record(fields))
            if names.len() == fields.len()
                && names
                    .iter()
                    .zip(fields.iter())
                    .all(|(name, field)| *name == field.0) =>
        {
            let values = values.into_iter().zip(fields.iter());
            ExprKind::Record(
                names,
                values.map(|(value, field)| part(value, &field.1)).collect(),
            )
        }
        (ExprKind::Map(entries), Type::Map(key, value)) => {
            let entries = entries.into_iter();
            ExprKind::Map(
                entries
                    .map(|(k, v)| (part(k, key), part(v, value)))
                    .collect(),
            )
        }
        // `{}` is a map without entries where a map is wanted.
        (ExprKind::Block(stmts, None), Type::Map(..)) if stmts.is_empty() => {
            ExprKind::Map(Vec::new())
        }
        (ExprKind::Block(stmts, Some(value)), _) => {
            ExprKind::Block(stmts, Some(Box::new(part(*value, to))))
        }
        (ExprKind::If(cond, then, Some(otherwise)), _) => {
            let then = Box::new(part(*then, to));
            ExprKind::If(cond, then, Some(Box::new(part(*otherwise, to))))
        }
        (kind, _) => return Err(Expr::new(kind, ty, span)),
    };
    match failed {
        true => Err(Expr::new(kind, ty, span)),
        false => Ok(Expr::new(kind, to.clone(), span)),
    }
}

impl<'a> Cx<'a> {
    fn new(checker: &'a mut Checker, ast: &'a Ast, interner: &'a mut Interner) -> Self {
        let first_function = checker.functions.len() as u32;
        Cx {
            checker,
            ast,
            interner,
            diags: Vec::new(),
            functions: Vec::new(),
            first_function,
            frames: vec![Frame::default()],
            columns: Vec::new(),
            types: vec![None; ast.expr_count()],
            peeked: None,
            inlined: None,
            asked: None,
            names: None,
        }
    }

    /// The name of a declared function, and its body if that has been checked.
    pub(crate) fn function_signature(&self, id: u32) -> (Arc<str>, Option<Arc<Expr>>) {
        let signature = &self.checker.functions[id as usize];
        (signature.name.clone(), signature.body.clone())
    }

    /// Whether `span` is the place whose names are asked for.
    pub(crate) fn asked_at(&self, span: Span) -> bool {
        self.asked
            .is_some_and(|at| span.start <= at && at <= span.end)
    }

    /// Notes what the expression at `span` can name, if that is the place asked for: the
    /// columns of the table operation it is in, the variables and functions in reach from the
    /// innermost block outward, and the declared types.
    fn note_reach(&mut self, span: Span, place: Place) {
        if !self.asked_at(span) || self.names.is_some() {
            return;
        }
        let value = |name: Arc<str>, ty: &Type| Named {
            name,
            kind: match ty {
                Type::Fn(_) => NameKind::Function,
                _ => NameKind::Variable,
            },
            ty: ty.clone(),
        };
        // The result of a function is not known before its body has been checked.
        let function = |name: Arc<str>, signature: &Signature| Named {
            name,
            kind: NameKind::Function,
            ty: Type::Fn(Arc::new(FnType {
                params: signature.params.clone(),
                ret: signature.ret.clone().unwrap_or(Type::Error),
            })),
        };
        let mut names = Vec::new();
        if let Some(scope) = self.columns.last() {
            names.extend(scope.schema.fields.iter().map(|field| Named {
                name: field.name.clone(),
                kind: NameKind::Column,
                ty: Type::from_col(field.ty),
            }));
        }
        for frame in self.frames.iter().rev() {
            let locals = frame
                .scopes
                .iter()
                .rev()
                .flat_map(|scope| scope.iter().rev());
            names.extend(locals.map(|(name, _, ty)| value(self.text(*name), ty)));
            if let Some((name, id)) = frame.this {
                names.push(function(
                    self.text(name),
                    &self.checker.functions[id as usize],
                ));
            }
        }
        for (name, top) in self.checker.scope.iter().rev() {
            names.push(match top {
                Top::Global(_, ty) => value(self.text(*name), ty),
                Top::Function(id) => {
                    function(self.text(*name), &self.checker.functions[*id as usize])
                }
            });
        }
        names.extend(self.declared_types());
        if place == Place::Callee {
            names.retain(|named| named.kind == NameKind::Function);
        }
        self.offer(place, names);
    }

    /// Looks into the argument of `call` that holds the place asked for, when the check of
    /// the call did not come to it: a call with too few arguments is given up as a whole. An
    /// argument of an operation on a table is taken as an expression over its columns.
    fn look_into(&mut self, call: &Call) {
        let holds = |arg: &&ArgSrc| self.asked_at(self.ast.span(arg.value));
        let found = call.args.iter().find(holds);
        let Some(arg) = found.filter(|_| self.names.is_none()) else {
            return;
        };
        let table = call.args[0].value;
        let schema = match &self.types[table.index()] {
            Some(Type::Table(schema)) => Some(schema.clone()),
            Some(Type::Grouped(grouped)) => Some(grouped.input.clone()),
            _ => None,
        };
        let scope = schema
            .filter(|_| arg.value != table)
            .map(|schema| ColumnScope {
                schema,
                mode: Mode::Row,
            });
        let scoped = scope.is_some();
        self.columns.extend(scope);
        self.expr(arg.value);
        if scoped {
            self.columns.pop();
        }
    }

    /// The types declared with `type`, the latest declaration first.
    fn declared_types(&self) -> Vec<Named> {
        let aliases = self.checker.aliases.iter().rev();
        aliases
            .map(|(name, ty)| Named {
                name: self.text(*name),
                kind: NameKind::Type,
                ty: ty.clone(),
            })
            .collect()
    }

    /// Keeps `names`, nearest first, as what can be written at the place asked for. A name
    /// means what its nearest declaration says; a type may share its name with a value.
    pub(crate) fn offer(&mut self, place: Place, mut names: Vec<Named>) {
        let mut seen = Vec::new();
        names.retain(|named| {
            let key = (named.name.clone(), named.kind == NameKind::Type);
            let first = !seen.contains(&key);
            seen.push(key);
            first
        });
        self.names = Some(Names { place, names });
    }

    fn frame(&mut self) -> &mut Frame {
        self.frames
            .last_mut()
            .expect("the module frame is always there")
    }

    /// Reports an error and returns an expression that stands in for the faulty one.
    pub(crate) fn error(&mut self, span: Span, message: impl Into<String>) -> Expr {
        self.diags.push(Diagnostic::new(span, message));
        Expr::error(span)
    }

    pub(crate) fn text(&self, name: Symbol) -> Arc<str> {
        self.interner.resolve(name).into()
    }

    fn declare_local(&mut self, name: Symbol, ty: Type) -> u32 {
        let frame = self.frame();
        let local = frame.locals;
        frame.locals += 1;
        let scope = frame
            .scopes
            .last_mut()
            .expect("locals are declared in a block");
        scope.push((name, local, ty));
        local
    }

    /// A variable of the function being checked that no name refers to.
    fn fresh_local(&mut self) -> u32 {
        let frame = self.frame();
        frame.locals += 1;
        frame.locals - 1
    }

    /// Converts `expr` to `ty`, the type it was unified to. Only a value written out in place
    /// can change the type of what it holds, so anything else must have the type already.
    pub(crate) fn converted(&mut self, expr: Expr, ty: &Type) -> Expr {
        coerce(expr, ty).unwrap_or_else(|expr| {
            let message = format!("expected {ty}, found {}", expr.ty);
            self.error(expr.span, message)
        })
    }

    fn signature_type(&mut self, function: u32, span: Option<Span>) -> Type {
        let signature = &self.checker.functions[function as usize];
        match &signature.ret {
            Some(ret) => Type::Fn(Arc::new(FnType {
                params: signature.params.clone(),
                ret: ret.clone(),
            })),
            None => {
                if let Some(span) = span {
                    let message = format!(
                        "the return type of `{}` is not known here; declare it with `-> type`",
                        signature.name
                    );
                    self.error(span, message);
                }
                Type::Error
            }
        }
    }

    /// Finds the variable or function that `name` refers to. With a `span`, problems with the
    /// name are reported there.
    pub(crate) fn resolve(&mut self, name: Symbol, span: Option<Span>) -> Option<(ExprKind, Type)> {
        self.resolve_in(self.frames.len() - 1, name, span)
    }

    fn resolve_in(
        &mut self,
        depth: usize,
        name: Symbol,
        span: Option<Span>,
    ) -> Option<(ExprKind, Type)> {
        let frame = &self.frames[depth];
        let local = frame.scopes.iter().rev().find_map(|scope| {
            let found = scope.iter().rev().find(|(local, ..)| *local == name);
            found.map(|(_, id, ty)| (ExprKind::Local(*id), ty.clone()))
        });
        if local.is_some() {
            return local;
        }
        if let Some((_, function)) = frame.this.filter(|(this, _)| *this == name) {
            return Some((ExprKind::SelfFn, self.signature_type(function, span)));
        }
        if let Some(index) = frame
            .captures
            .iter()
            .position(|(captured, _)| *captured == name)
        {
            let ty = frame.captures[index].1.ty.clone();
            return Some((ExprKind::Capture(index as u32), ty));
        }
        if depth == 0 {
            let top = self
                .checker
                .scope
                .iter()
                .rev()
                .find(|(top, _)| *top == name);
            return Some(match top?.1.clone() {
                Top::Global(id, ty) => (ExprKind::Global(id, self.text(name)), ty),
                Top::Function(id) => (ExprKind::Function(id), self.signature_type(id, span)),
            });
        }
        let (kind, ty) = self.resolve_in(depth - 1, name, span)?;
        // Globals and top-level functions are reachable from anywhere; a variable of an
        // enclosing function has to be copied in.
        if matches!(kind, ExprKind::Global(..) | ExprKind::Function(_)) {
            return Some((kind, ty));
        }
        let read = Expr::new(kind, ty.clone(), span.unwrap_or(Span::new(0, 0)));
        let captures = &mut self.frames[depth].captures;
        captures.push((name, read));
        Some((ExprKind::Capture(captures.len() as u32 - 1), ty))
    }

    fn module(&mut self) -> (Function, Type) {
        let ast = self.ast;
        for stmt in &ast.stmts {
            if let StmtKind::Type { name, ty } = &stmt.kind {
                self.declare_alias(*name, ty);
            }
        }
        // Functions can be called above their declaration, so their signatures come first.
        let mut declared = Vec::new();
        for stmt in &ast.stmts {
            if let StmtKind::Fn {
                name, params, ret, ..
            } = &stmt.kind
            {
                let id = self.declare_function(*name, params, ret.as_ref());
                if declared.iter().any(|(other, _)| *other == name.name) {
                    let message =
                        format!("`{}` is declared twice in this file", self.text(name.name));
                    self.error(name.span, message);
                }
                declared.push((name.name, id));
                self.checker.scope.push((name.name, Top::Function(id)));
            }
        }

        self.frame().scopes.push(Vec::new());
        let mut stmts = Vec::new();
        let mut value = None;
        let mut next_function = declared.iter();
        let mut imports_done = false;
        for (index, stmt) in ast.stmts.iter().enumerate() {
            imports_done |= !matches!(stmt.kind, StmtKind::Import { .. });
            match &stmt.kind {
                StmtKind::Type { .. } => {}
                // The files a module imports are checked before it; their definitions are
                // already among the top-level names.
                StmtKind::Import { .. } => {
                    if imports_done {
                        let message = "imports come before everything else in a file";
                        self.error(stmt.span, message);
                    }
                }
                StmtKind::Fn { params, body, .. } => {
                    let (_, id) = *next_function.next().expect("declared in the first pass");
                    let names: Vec<Symbol> = params.iter().map(|param| param.name.name).collect();
                    self.function_body(id, &names, *body, None);
                }
                StmtKind::Let { name, ty, value } => {
                    if declared.iter().any(|(function, _)| *function == name.name) {
                        let text = self.text(name.name);
                        let message = format!("`{text}` is already a function in this file");
                        self.error(name.span, message);
                    }
                    let value = self.let_value(ty.as_ref(), *value);
                    let id = self.checker.globals;
                    self.checker.globals += 1;
                    let top = Top::Global(id, value.ty.clone());
                    self.checker.scope.push((name.name, top));
                    stmts.push(Stmt::Global(id, value));
                }
                StmtKind::Expr(expr) => {
                    let expr = self.expr(*expr);
                    if index + 1 == ast.stmts.len() {
                        value = Some(Box::new(expr));
                    } else {
                        stmts.push(Stmt::Expr(expr));
                    }
                }
            }
        }
        let frame = self.frames.pop().expect("the module frame");
        let result = value.as_ref().map_or(Type::Unit, |value| value.ty.clone());
        let span = Span::new(0, 0);
        let main = Function {
            name: "<main>".into(),
            params: 0,
            locals: frame.locals,
            body: Expr::new(ExprKind::Block(stmts, value), result.clone(), span),
        };
        (main, result)
    }

    fn declare_alias(&mut self, name: Ident, ty: &TypeExpr) {
        let text = self.text(name.name);
        if matches!(
            &*text,
            "int"
                | "float"
                | "bool"
                | "string"
                | "date"
                | "datetime"
                | "duration"
                | "decimal"
                | "list"
                | "map"
                | "table"
        ) {
            self.error(name.span, format!("`{text}` is a built-in type"));
            return;
        }
        let ty = self.resolve_type(ty);
        self.checker.aliases.push((name.name, ty));
    }

    fn declare_function(
        &mut self,
        name: Ident,
        params: &[ast::Param],
        ret: Option<&TypeExpr>,
    ) -> u32 {
        let text = self.text(name.name);
        if is_builtin_name(&text) {
            let message = format!("`{text}` is a built-in function; choose another name");
            self.error(name.span, message);
        }
        let mut signature = Signature {
            name: text,
            params: Vec::new(),
            ret: ret.map(|ret| self.resolve_type(ret)),
            lambda: false,
            body: None,
        };
        for param in params {
            if signature.params.iter().any(|p| p.name == param.name.name) {
                let message = format!(
                    "parameter `{}` is declared twice",
                    self.text(param.name.name)
                );
                self.error(param.name.span, message);
            }
            signature.params.push(Param {
                name: param.name.name,
                text: self.text(param.name.name),
                ty: self.resolve_type(&param.ty),
            });
        }
        self.checker.functions.push(signature);
        self.checker.functions.len() as u32 - 1
    }

    /// Checks the body of a declared function and stores its HIR. `params` are the names of
    /// its parameters and `this` names a local function. Returns the values the function
    /// captures, as read where it is declared.
    fn function_body(
        &mut self,
        id: u32,
        params: &[Symbol],
        body: ExprId,
        this: Option<Symbol>,
    ) -> Vec<Expr> {
        let signature = &self.checker.functions[id as usize];
        let name = signature.name.clone();
        let lambda = signature.lambda;
        let declared_ret = signature.ret.clone();
        let locals = params.iter().zip(&signature.params).enumerate();
        let locals = locals.map(|(local, (param, typed))| (*param, local as u32, typed.ty.clone()));
        self.frames.push(Frame {
            scopes: vec![locals.collect()],
            locals: params.len() as u32,
            captures: Vec::new(),
            this: this.map(|this| (this, id)),
        });
        // The column of a table operation is not a variable of functions declared inside it.
        let columns = std::mem::take(&mut self.columns);
        // A function that returns a function may end in a lambda, which takes its types
        // from the declaration.
        let mut body = match &declared_ret {
            Some(ret) => self.expr_expecting(body, ret),
            None => self.expr(body),
        };
        self.columns = columns;
        let frame = self.frames.pop().expect("pushed above");

        match declared_ret {
            Some(ret) => {
                body = coerce(body, &ret).unwrap_or_else(|body| {
                    let message = match lambda {
                        true => format!(
                            "this function must return {ret}, but its body has type {}",
                            body.ty
                        ),
                        false => format!(
                            "`{name}` is declared to return {ret}, but its body has type {}",
                            body.ty
                        ),
                    };
                    self.error(body.span, message);
                    body
                });
            }
            None => self.checker.functions[id as usize].ret = Some(body.ty.clone()),
        }
        if !lambda && this.is_none() {
            self.checker.functions[id as usize].body = Some(Arc::new(body.clone()));
        }
        let function = Function {
            name,
            params: params.len() as u32,
            locals: frame.locals,
            body,
        };
        let index = (id - self.first_function) as usize;
        if self.functions.len() <= index {
            self.functions.resize_with(index + 1, || None);
        }
        self.functions[index] = Some(function);
        frame.captures.into_iter().map(|(_, read)| read).collect()
    }

    /// Checks a function written as a value. `hint` gives the types that the place it is
    /// written in expects.
    fn lambda(
        &mut self,
        span: Span,
        params: &[LambdaParam],
        ret: Option<&TypeExpr>,
        body: ExprId,
        hint: Option<FnHint>,
    ) -> Expr {
        let fits = hint
            .as_ref()
            .is_none_or(|hint| hint.params.len() == params.len());
        if let Some(hint) = hint.as_ref().filter(|_| !fits) {
            let count = |count: usize| match count {
                1 => "1 parameter".to_string(),
                count => format!("{count} parameters"),
            };
            let message = format!(
                "this function has {}, but one with {} is needed here",
                count(params.len()),
                count(hint.params.len())
            );
            self.error(span, message);
        }
        let mut typed: Vec<Param> = Vec::with_capacity(params.len());
        for (index, param) in params.iter().enumerate() {
            let text = self.text(param.name.name);
            if typed.iter().any(|p| p.name == param.name.name) {
                let message = format!("parameter `{text}` is declared twice");
                self.error(param.name.span, message);
            }
            let hinted = hint.as_ref().and_then(|hint| hint.params.get(index));
            let ty = match (&param.ty, hinted) {
                (Some(ty), _) => self.resolve_type(ty),
                (None, Some(ty)) => ty.clone(),
                (None, None) => {
                    if fits {
                        let message = format!(
                            "cannot tell the type of `{text}` here; write it, as in `{text}: int`"
                        );
                        self.error(param.name.span, message);
                    }
                    Type::Error
                }
            };
            typed.push(Param {
                name: param.name.name,
                text,
                ty,
            });
        }
        let ret = match ret {
            Some(ret) => Some(self.resolve_type(ret)),
            None => hint.and_then(|hint| hint.ret),
        };
        self.checker.functions.push(Signature {
            name: "lambda".into(),
            params: typed,
            ret,
            lambda: true,
            body: None,
        });
        let id = self.checker.functions.len() as u32 - 1;
        let names: Vec<Symbol> = params.iter().map(|param| param.name.name).collect();
        let captures = self.function_body(id, &names, body, None);
        // A parameter without a type has been reported; the lambda is not reported again
        // wherever its type does not fit.
        let signature = &self.checker.functions[id as usize];
        if signature.params.iter().any(|param| param.ty.is_error()) {
            return Expr::error(span);
        }
        let ty = self.signature_type(id, None);
        Expr::new(ExprKind::Closure(id, captures), ty, span)
    }

    /// Checks the value of a `let` against its annotation, if it has one.
    fn let_value(&mut self, annotation: Option<&TypeExpr>, value: ExprId) -> Expr {
        let Some(annotation) = annotation else {
            let value = self.expr(value);
            if value.ty == Type::Null {
                let message = "cannot tell which type this `null` has; \
                               annotate the variable, as in `let x: int? = null`";
                return self.error(value.span, message);
            }
            return value;
        };
        let ty = self.resolve_type(annotation);
        let value = self.expr_expecting(value, &ty);
        coerce(value, &ty).unwrap_or_else(|value| {
            let message = format!("expected {ty}, found {}", value.ty);
            self.error(value.span, message)
        })
    }

    pub(crate) fn resolve_type(&mut self, ty: &TypeExpr) -> Type {
        match &ty.kind {
            TypeKind::Named { name, args } => {
                let text = self.text(*name);
                // The span of the type takes in its arguments; the name comes first.
                let written = Span::new(ty.span.start, ty.span.start + text.len() as u32);
                if self.asked_at(written) && self.names.is_none() {
                    self.offer(Place::Type, self.declared_types());
                }
                let expected_args = match &*text {
                    "list" | "table" => 1,
                    "map" => 2,
                    _ => 0,
                };
                if args.len() != expected_args {
                    let message = match expected_args {
                        0 => format!("`{text}` takes no type arguments"),
                        1 => format!("`{text}` takes one type argument, as in `{text}<...>`"),
                        _ => "`map` takes two type arguments, as in `map<string, int>`".into(),
                    };
                    self.error(ty.span, message);
                    return Type::Error;
                }
                match &*text {
                    "int" => Type::Int,
                    "float" => Type::Float,
                    "bool" => Type::Bool,
                    "string" => Type::Str,
                    "date" => Type::Date,
                    "datetime" => Type::DateTime,
                    "duration" => Type::Duration,
                    "decimal" => Type::Decimal,
                    "list" => match self.resolve_type(&args[0]) {
                        Type::Error => Type::Error,
                        element if storable(&element) => Type::List(Box::new(element)),
                        element => {
                            let message = format!("a list cannot hold values of type {element}");
                            self.error(args[0].span, message);
                            Type::Error
                        }
                    },
                    "map" => {
                        let key = self.resolve_type(&args[0]);
                        let value = self.resolve_type(&args[1]);
                        if key.is_error() || value.is_error() {
                            return Type::Error;
                        }
                        if !Self::is_key(&key) {
                            let message = format!(
                                "the keys of a map are strings, ints, bools, or dates, not {key}"
                            );
                            self.error(args[0].span, message);
                            return Type::Error;
                        }
                        if !storable(&value) {
                            let message = format!("a map cannot hold values of type {value}");
                            self.error(args[1].span, message);
                            return Type::Error;
                        }
                        Type::Map(Box::new(key), Box::new(value))
                    }
                    "table" => {
                        let row = self.resolve_type(&args[0]);
                        self.row_schema(&row, args[0].span)
                            .map_or(Type::Error, Type::Table)
                    }
                    _ => {
                        let alias = self
                            .checker
                            .aliases
                            .iter()
                            .rev()
                            .find(|(alias, _)| alias == name);
                        match alias {
                            Some((_, ty)) => ty.clone(),
                            None => {
                                self.error(ty.span, format!("unknown type `{text}`"));
                                Type::Error
                            }
                        }
                    }
                }
            }
            TypeKind::Record(fields) => {
                let mut resolved: Vec<(Arc<str>, Type)> = Vec::with_capacity(fields.len());
                for field in fields {
                    let name = self.text(field.name.name);
                    if resolved.iter().any(|(other, _)| *other == name) {
                        self.error(field.name.span, format!("field `{name}` is declared twice"));
                    }
                    match self.resolve_type(&field.ty) {
                        Type::Error => {}
                        ty if storable(&ty) => resolved.push((name, ty)),
                        ty => {
                            let message = format!("a record cannot hold a value of type {ty}");
                            self.error(field.ty.span, message);
                        }
                    }
                }
                Type::Record(Arc::new(resolved))
            }
            TypeKind::Nullable(inner) => match self.resolve_type(inner) {
                Type::Error => Type::Error,
                inner @ Type::Nullable(_) => inner,
                inner if storable(&inner) => inner.or_null(),
                other => {
                    self.error(ty.span, format!("{other} cannot be made nullable"));
                    Type::Error
                }
            },
            TypeKind::Fn { params, ret } => {
                // The parameters of a function type have no names to pass arguments by.
                let name = self.interner.intern("");
                let mut typed = Vec::with_capacity(params.len());
                for param in params {
                    typed.push(Param {
                        name,
                        text: "".into(),
                        ty: self.resolve_type(param),
                    });
                }
                let ret = match ret {
                    Some(ret) => self.resolve_type(ret),
                    None => Type::Unit,
                };
                if ret.is_error() || typed.iter().any(|param| param.ty.is_error()) {
                    return Type::Error;
                }
                Type::Fn(Arc::new(FnType { params: typed, ret }))
            }
        }
    }

    /// Whether values of this type can be the keys of a map.
    pub(crate) fn is_key(ty: &Type) -> bool {
        matches!(ty, Type::Str | Type::Int | Type::Bool | Type::Date)
    }

    /// The columns of a table whose rows have type `row`, which must be a record of values
    /// that columns can hold. Reports what is wrong otherwise.
    pub(crate) fn row_schema(&mut self, row: &Type, span: Span) -> Option<Arc<biggo_plan::Schema>> {
        match row {
            Type::Error => None,
            Type::Table(schema) => Some(schema.clone()),
            _ => match row.columns() {
                Some(Ok(schema)) => Some(Arc::new(schema)),
                Some(Err((name, ty))) => {
                    let message = format!("column `{name}` cannot have type {ty}");
                    self.error(span, message);
                    None
                }
                None => {
                    let message = format!("expected a row type such as `{{id: int}}`, found {row}");
                    self.error(span, message);
                    None
                }
            },
        }
    }

    pub(crate) fn expr(&mut self, id: ExprId) -> Expr {
        self.expr_hinted(id, None)
    }

    /// Checks `id` where a value of type `expected` is wanted, which tells a lambda the types
    /// of its parameters. The caller still converts the result to that type.
    pub(crate) fn expr_expecting(&mut self, id: ExprId, expected: &Type) -> Expr {
        let hint = match expected {
            Type::Fn(function) => Some(FnHint {
                params: function.params.iter().map(|p| p.ty.clone()).collect(),
                ret: Some(function.ret.clone()),
            }),
            _ => None,
        };
        self.expr_hinted(id, hint)
    }

    pub(crate) fn expr_hinted(&mut self, id: ExprId, hint: Option<FnHint>) -> Expr {
        if let Some((peeked, expr)) = self.peeked.take()
            && peeked == id
        {
            return expr;
        }
        let ast = self.ast;
        let expr = match ast.expr(id) {
            ast::Expr::Lambda { params, ret, body } => {
                self.lambda(ast.span(id), params, ret.as_ref(), *body, hint)
            }
            // What is wanted of a block or an `if` is wanted of the value it gives.
            ast::Expr::Block(stmts) if hint.is_some() => self.block(ast.span(id), stmts, hint),
            ast::Expr::If {
                cond,
                then_branch,
                else_branch,
            } if hint.is_some() => {
                self.if_expr(ast.span(id), *cond, *then_branch, *else_branch, hint)
            }
            _ => self.expr_inner(id),
        };
        self.types[id.index()] = Some(expr.ty.clone());
        expr
    }

    fn expr_inner(&mut self, id: ExprId) -> Expr {
        let ast = self.ast;
        let span = ast.span(id);
        let literal = |kind, ty| Expr::new(kind, ty, span);
        match ast.expr(id) {
            ast::Expr::Int(value) => literal(ExprKind::Int(*value), Type::Int),
            ast::Expr::Float(value) => literal(ExprKind::Float(*value), Type::Float),
            ast::Expr::Str(value) => literal(ExprKind::Str(value.clone()), Type::Str),
            ast::Expr::Bool(value) => literal(ExprKind::Bool(*value), Type::Bool),
            ast::Expr::Null => literal(ExprKind::Null, Type::Null),
            ast::Expr::Date(date) => literal(ExprKind::Date(date.to_days()), Type::Date),
            ast::Expr::DateTime(micros) => literal(ExprKind::DateTime(*micros), Type::DateTime),
            ast::Expr::Decimal(value) => literal(ExprKind::Decimal(*value), Type::Decimal),
            ast::Expr::Name(name) => self.name(span, *name),
            ast::Expr::List(items) => self.list(span, items),
            ast::Expr::Record(fields) => self.record(span, fields),
            ast::Expr::Map(entries) => self.map(span, entries),
            ast::Expr::Index { base, index } => self.index(span, *base, *index),
            ast::Expr::Lambda { params, ret, body } => {
                self.lambda(span, params, ret.as_ref(), *body, None)
            }
            ast::Expr::Match { scrutinee, arms } => self.match_expr(span, *scrutinee, arms),
            ast::Expr::Unary { op, operand } => {
                let operand = self.expr(*operand);
                self.unary(span, *op, operand)
            }
            ast::Expr::Binary { op, lhs, rhs } => {
                let left = self.expr(*lhs);
                let right = self.expr(*rhs);
                self.binary(span, *op, left, right)
            }
            ast::Expr::Pipe { lhs, rhs } => self.call(*rhs, Some(*lhs)),
            ast::Expr::Call { .. } => self.call(id, None),
            ast::Expr::Field { base, name } => self.field(span, *base, *name),
            ast::Expr::If {
                cond,
                then_branch,
                else_branch,
            } => self.if_expr(span, *cond, *then_branch, *else_branch, None),
            ast::Expr::Block(stmts) => self.block(span, stmts, None),
        }
    }

    fn field(&mut self, span: Span, base: ExprId, name: Ident) -> Expr {
        let base = self.expr(base);
        let text = self.text(name.name);
        let (inner, nullable) = base.ty.split_null();
        if self.asked_at(name.span) && self.names.is_none() {
            // Anything but a record has no fields to name.
            let fields = match inner {
                Type::Record(fields) => &fields[..],
                _ => &[],
            };
            let fields = fields.iter().map(|(field, ty)| Named {
                name: field.clone(),
                kind: NameKind::Field,
                ty: ty.clone().with_null(nullable),
            });
            self.offer(Place::Member, fields.collect());
        }
        let fields = match inner {
            Type::Error => return Expr::error(span),
            Type::Record(fields) => fields.clone(),
            _ => {
                let message = format!("a value of type {} has no field `{text}`", base.ty);
                return self.error(name.span, message);
            }
        };
        let Some(index) = fields.iter().position(|(field, _)| *field == text) else {
            let names: Vec<&str> = fields.iter().map(|(field, _)| &**field).collect();
            let message = format!(
                "this record has no field `{text}`; its fields are {}",
                names.join(", ")
            );
            return self.error(name.span, message);
        };
        // A field of a record that is null is null.
        let ty = fields[index].1.clone().with_null(nullable);
        Expr::new(ExprKind::Field(Box::new(base), index as u32), ty, span)
    }

    fn record(&mut self, span: Span, fields: &[(Ident, ExprId)]) -> Expr {
        let mut names: Vec<Arc<str>> = Vec::with_capacity(fields.len());
        let mut values = Vec::with_capacity(fields.len());
        let mut failed = false;
        for (name, value) in fields {
            let text = self.text(name.name);
            if names.contains(&text) {
                self.error(name.span, format!("field `{text}` is given twice"));
                failed = true;
            }
            let value = self.expr(*value);
            if value.ty.is_error() {
                failed = true;
            } else if !storable(&value.ty) {
                let message = format!("a record cannot hold a value of type {}", value.ty);
                self.error(value.span, message);
                failed = true;
            }
            names.push(text);
            values.push(value);
        }
        if failed {
            return Expr::error(span);
        }
        let types = values.iter().map(|value| value.ty.clone());
        let ty = Type::Record(Arc::new(names.iter().cloned().zip(types).collect()));
        Expr::new(ExprKind::Record(names.into(), values), ty, span)
    }

    fn map(&mut self, span: Span, entries: &[(ExprId, ExprId)]) -> Expr {
        let mut checked = Vec::with_capacity(entries.len());
        let (mut key_ty, mut value_ty): (Option<Type>, Option<Type>) = (None, None);
        for (key, value) in entries {
            let key = self.expr(*key);
            let value = self.expr(*value);
            if key.ty.is_error() || value.ty.is_error() {
                return Expr::error(span);
            }
            for (expr, ty, what) in [
                (&key, &mut key_ty, "keys"),
                (&value, &mut value_ty, "values"),
            ] {
                let unified = match ty.as_ref() {
                    Some(ty) => unify(ty, &expr.ty),
                    None => Some(expr.ty.clone()),
                };
                let Some(unified) = unified else {
                    let message = format!(
                        "the {what} of a map must have one type, found {} and {}",
                        ty.take().unwrap_or(Type::Null),
                        expr.ty
                    );
                    return self.error(expr.span, message);
                };
                *ty = Some(unified);
            }
            let twice = |(other, _): &(Expr, Expr)| match (&other.kind, &key.kind) {
                (ExprKind::Str(a), ExprKind::Str(b)) => a == b,
                (ExprKind::Int(a), ExprKind::Int(b)) => a == b,
                (ExprKind::Bool(a), ExprKind::Bool(b)) => a == b,
                (ExprKind::Date(a), ExprKind::Date(b)) => a == b,
                _ => false,
            };
            if checked.iter().any(twice) {
                return self.error(key.span, "this key is given twice");
            }
            checked.push((key, value));
        }
        let (Some(key_ty), Some(value_ty)) = (key_ty, value_ty) else {
            unreachable!("the parser only builds maps with entries");
        };
        if !Self::is_key(&key_ty) {
            let message =
                format!("the keys of a map are strings, ints, bools, or dates, not {key_ty}");
            return self.error(checked[0].0.span, message);
        }
        if !storable(&value_ty) {
            let message = format!("a map cannot hold values of type {value_ty}");
            return self.error(checked[0].1.span, message);
        }
        let mut converted = Vec::with_capacity(checked.len());
        for (key, value) in checked {
            let key = self.converted(key, &key_ty);
            converted.push((key, self.converted(value, &value_ty)));
        }
        let ty = Type::Map(Box::new(key_ty), Box::new(value_ty));
        Expr::new(ExprKind::Map(converted), ty, span)
    }

    fn index(&mut self, span: Span, base: ExprId, index: ExprId) -> Expr {
        let base = self.expr(base);
        let index = self.expr(index);
        if base.ty.is_error() || index.ty.is_error() {
            return Expr::error(span);
        }
        let (inner, nullable) = base.ty.split_null();
        let (index_ty, ty) = match inner {
            Type::List(element) => (Type::Int, (**element).clone().with_null(nullable)),
            // A key that the map does not have gives null.
            Type::Map(key, value) => ((**key).clone(), (**value).clone().or_null()),
            Type::Table(_) => {
                let message = "the rows of a table are not numbered; use `take` and `skip`, \
                               or `to_rows` for a list of its rows";
                return self.error(span, message);
            }
            other => {
                let message = format!("a value of type {other} cannot be indexed");
                return self.error(span, message);
            }
        };
        let index = match coerce(index, &index_ty) {
            Ok(index) => index,
            Err(index) => {
                let message = match inner {
                    Type::List(_) => format!("a list index must be an int, found {}", index.ty),
                    _ => format!("the keys of this map are {index_ty}, found {}", index.ty),
                };
                return self.error(index.span, message);
            }
        };
        Expr::new(ExprKind::Index(Box::new(base), Box::new(index)), ty, span)
    }

    /// Checks a `match` and lowers it to a chain of `if`s that test the arms in order.
    fn match_expr(&mut self, span: Span, scrutinee: ExprId, arms: &[MatchArm]) -> Expr {
        let ast = self.ast;
        let subject_span = ast.span(scrutinee);
        let subject = self.expr(scrutinee);
        let mut failed = subject.ty.is_error();
        if !failed && (base(&subject.ty).is_none() || subject.ty == Type::Null) {
            let message = format!("cannot match on a value of type {}", subject.ty);
            self.error(subject.span, message);
            failed = true;
        }
        let nullable = subject.ty.split_null().1;
        // The value is computed once and held in a variable, unless reading it again costs
        // nothing. A column is read again too: a variable cannot hold one.
        let cheap = matches!(
            subject.kind,
            ExprKind::Local(_)
                | ExprKind::Capture(_)
                | ExprKind::Global(..)
                | ExprKind::Column(_)
                | ExprKind::Bool(_)
                | ExprKind::Int(_)
                | ExprKind::Float(_)
                | ExprKind::Str(_)
                | ExprKind::Date(_)
                | ExprKind::DateTime(_)
                | ExprKind::Decimal(_)
        );
        let (binding, subject) = if failed || cheap || subject.find_column_use().is_some() {
            (None, subject)
        } else {
            let local = self.fresh_local();
            let read = Expr::new(ExprKind::Local(local), subject.ty.clone(), subject.span);
            (Some((local, subject)), read)
        };

        // What the arms without `_` cover, to tell whether a bool is matched in full.
        let (mut on_true, mut on_false, mut on_null) = (false, false, false);
        let mut catch_all = false;
        let mut checked: Vec<(Option<Expr>, Expr)> = Vec::with_capacity(arms.len());
        for arm in arms {
            if catch_all {
                let span = match arm.patterns.first() {
                    Some(Pattern::Wildcard(span)) => *span,
                    Some(Pattern::Value(value)) => ast.span(*value),
                    None => ast.span(arm.body),
                };
                self.error(
                    span,
                    "this arm is never reached; the `_` arm above takes everything",
                );
                failed = true;
            }
            let mut cond: Option<Expr> = None;
            for pattern in &arm.patterns {
                let value = match pattern {
                    Pattern::Wildcard(_) => {
                        catch_all = true;
                        continue;
                    }
                    Pattern::Value(value) => self.expr(*value),
                };
                if failed || value.ty.is_error() {
                    failed = true;
                    continue;
                }
                let pattern_span = value.span;
                let test = match value.kind {
                    ExprKind::Null => {
                        if !nullable {
                            let message = format!("a value of type {} is never null", subject.ty);
                            self.error(pattern_span, message);
                            failed = true;
                            continue;
                        }
                        on_null = true;
                        let kind = ExprKind::Scalar(ScalarFn::IsNull, vec![subject.clone()]);
                        Expr::new(kind, Type::Bool, pattern_span)
                    }
                    _ => {
                        on_true |= matches!(value.kind, ExprKind::Bool(true));
                        on_false |= matches!(value.kind, ExprKind::Bool(false));
                        // A null matches no value: only `null` and `_` take it.
                        match self.equals(pattern_span, subject.clone(), value) {
                            Some(test) => test,
                            None => {
                                failed = true;
                                continue;
                            }
                        }
                    }
                };
                cond = Some(match cond {
                    Some(cond) => {
                        let kind = ExprKind::Binary(BinaryOp::Or, Box::new(cond), Box::new(test));
                        Expr::new(kind, Type::Bool, pattern_span)
                    }
                    None => test,
                });
            }
            let body = self.expr(arm.body);
            failed |= body.ty.is_error();
            // An arm with `_` among its patterns takes everything.
            let cond = cond.filter(|_| !catch_all);
            checked.push((cond, body));
        }
        let whole_bool = subject.ty.split_null().0 == &Type::Bool
            && on_true
            && on_false
            && (on_null || !nullable);
        if !failed && !catch_all && !whole_bool {
            let message = "this `match` does not cover every value; add a `_ => ...` arm";
            self.error(subject_span, message);
            failed = true;
        }
        if failed {
            return Expr::error(span);
        }

        let mut ty = checked[0].1.ty.clone();
        for (_, body) in &checked[1..] {
            let Some(unified) = unify(&ty, &body.ty) else {
                let message = format!(
                    "the arms of a `match` must have one type, found {ty} and {}",
                    body.ty
                );
                return self.error(body.span, message);
            };
            ty = unified;
        }
        // The last arm is what is left when no other arm matches.
        let (_, last) = checked
            .pop()
            .expect("a match that covers every value has an arm");
        let mut value = self.converted(last, &ty);
        for (cond, body) in checked.into_iter().rev() {
            let cond = cond.expect("only the last arm takes everything");
            let body = self.converted(body, &ty);
            let kind = ExprKind::If(Box::new(cond), Box::new(body), Some(Box::new(value)));
            value = Expr::new(kind, ty.clone(), span);
        }
        match binding {
            Some((local, subject)) => {
                let kind = ExprKind::Block(vec![Stmt::Let(local, subject)], Some(Box::new(value)));
                Expr::new(kind, ty, span)
            }
            None => value,
        }
    }

    fn name(&mut self, span: Span, name: Symbol) -> Expr {
        self.note_reach(span, Place::Expr);
        if let Some(column) = self.column(name) {
            let variable = self.resolve(name, None);
            if variable.is_some_and(|(_, ty)| !matches!(ty, Type::Fn(_))) {
                let message = format!(
                    "`{}` is both a column of this table and a variable; rename the variable",
                    column.name
                );
                return self.error(span, message);
            }
            let ty = Type::from_col(column.ty);
            return Expr::new(ExprKind::Column(column.name), ty, span);
        }
        match self.resolve(name, Some(span)) {
            Some((kind, ty)) => Expr::new(kind, ty, span),
            None => {
                let text = self.text(name);
                if is_builtin_name(&text) {
                    let message = format!(
                        "`{text}` is a built-in function, which can only be called; \
                         to pass it along, wrap it in a function: `fn(x) {{ {text}(x) }}`"
                    );
                    return self.error(span, message);
                }
                let mut message = format!("undefined name `{text}`");
                if let Some(scope) = self.columns.last() {
                    let names: Vec<String> = scope.schema.names().map(|n| n.to_string()).collect();
                    message.push_str(&format!("; the table has columns {}", names.join(", ")));
                }
                self.error(span, message)
            }
        }
    }

    fn list(&mut self, span: Span, items: &[ExprId]) -> Expr {
        let items: Vec<Expr> = items.iter().map(|item| self.expr(*item)).collect();
        let mut element: Option<Type> = None;
        for item in &items {
            if item.ty.is_error() {
                return Expr::error(span);
            }
            let unified = match &element {
                Some(element) => unify(element, &item.ty),
                None => Some(item.ty.clone()),
            };
            let Some(unified) = unified else {
                let message = format!(
                    "the elements of a list must have one type, found {} and {}",
                    element.unwrap_or(Type::Null),
                    item.ty
                );
                return self.error(item.span, message);
            };
            element = Some(unified);
        }
        // An empty list takes its element type from where it is used.
        let element = element.unwrap_or(Type::Null);
        if !storable(&element) {
            let message = format!("a list cannot hold values of type {element}");
            return self.error(span, message);
        }
        let mut converted = Vec::with_capacity(items.len());
        for item in items {
            converted.push(self.converted(item, &element));
        }
        let ty = Type::List(Box::new(element));
        Expr::new(ExprKind::List(converted), ty, span)
    }

    fn unary(&mut self, span: Span, op: UnaryOp, operand: Expr) -> Expr {
        if operand.ty.is_error() {
            return Expr::error(span);
        }
        let operand_base = base(&operand.ty).map(|(base, _)| base);
        let (accepted, kind): (bool, fn(Box<Expr>) -> ExprKind) = match op {
            UnaryOp::Neg => (
                matches!(
                    operand_base,
                    Some(Base::Int | Base::Float | Base::Decimal | Base::Null)
                ),
                ExprKind::Neg,
            ),
            UnaryOp::Not => (
                matches!(operand_base, Some(Base::Bool | Base::Null)),
                ExprKind::Not,
            ),
        };
        if !accepted {
            let message = match op {
                UnaryOp::Neg => format!("cannot negate {}", operand.ty),
                UnaryOp::Not => format!("`not` expects a bool, found {}", operand.ty),
            };
            return self.error(span, message);
        }
        let ty = operand.ty.clone();
        Expr::new(kind(Box::new(operand)), ty, span)
    }

    pub(crate) fn binary(&mut self, span: Span, op: BinaryOp, left: Expr, right: Expr) -> Expr {
        use BinaryOp::*;
        if left.ty.is_error() || right.ty.is_error() {
            return Expr::error(span);
        }
        if op == Coalesce {
            return self.coalesce(span, left, right);
        }
        if matches!(op, In | NotIn) {
            return self.membership(span, op, left, right);
        }
        let mismatch = |cx: &mut Self| {
            let message = format!(
                "cannot apply `{}` to {} and {}",
                op.symbol(),
                left.ty,
                right.ty
            );
            cx.error(span, message)
        };
        // Two lists join end to end.
        if op == Add && matches!((&left.ty, &right.ty), (Type::List(_), Type::List(_))) {
            let Some(ty) = unify(&left.ty, &right.ty) else {
                return mismatch(self);
            };
            let left = self.converted(left, &ty);
            let right = self.converted(right, &ty);
            let kind = ExprKind::Binary(op, Box::new(left), Box::new(right));
            return Expr::new(kind, ty, span);
        }
        let (Some((left_base, left_null)), Some((right_base, right_null))) =
            (base(&left.ty), base(&right.ty))
        else {
            return mismatch(self);
        };
        // Numbers from the narrowest type to the widest.
        let rank = |base| match base {
            Base::Int => Some(0),
            Base::Decimal => Some(1),
            Base::Float => Some(2),
            _ => None,
        };
        // The wider of two number types. A `null` takes the type of the other operand.
        let wider = |left: Base, right: Base| match (left, right) {
            (Base::Null, other) | (other, Base::Null) => rank(other).map(|_| other),
            (left, right) => Some(if rank(left)? >= rank(right)? {
                left
            } else {
                right
            }),
        };
        let moment = |base| matches!(base, Base::Date | Base::DateTime);
        // The types the operands convert to, and the type of the result.
        let (left_to, right_to, result) = match op {
            Add | Sub | Mul | Div | Rem => match (left_base, right_base) {
                (Base::Null, Base::Null) => (Base::Null, Base::Null, Base::Null),
                (Base::Str | Base::Null, Base::Str | Base::Null) if op == Add => {
                    (Base::Str, Base::Str, Base::Str)
                }
                // The time between two moments, and a moment a length of time away.
                (l, r) if op == Sub && moment(l) && moment(r) => {
                    (Base::DateTime, Base::DateTime, Base::Duration)
                }
                (l, Base::Duration) if matches!(op, Add | Sub) && moment(l) => {
                    (Base::DateTime, Base::Duration, Base::DateTime)
                }
                (Base::Duration, r) if op == Add && moment(r) => {
                    (Base::Duration, Base::DateTime, Base::DateTime)
                }
                (Base::Duration, Base::Duration) if matches!(op, Add | Sub) => {
                    (Base::Duration, Base::Duration, Base::Duration)
                }
                (l, r) => match wider(l, r) {
                    // Dividing whole numbers gives a float.
                    Some(Base::Int) if op == Div => (Base::Float, Base::Float, Base::Float),
                    Some(number) => (number, number, number),
                    None => return mismatch(self),
                },
            },
            Eq | Ne | Lt | Le | Gt | Ge => {
                if left_base == Base::Null || right_base == Base::Null {
                    let message = "comparing with `null` always gives null; \
                                   use `is_null(...)` to test for null";
                    return self.error(span, message);
                }
                let operand = match (left_base, right_base) {
                    (l, r) if rank(l).is_some() && rank(r).is_some() => {
                        wider(l, r).expect("both are numbers")
                    }
                    (l, r) if moment(l) && moment(r) => {
                        if l == r {
                            l
                        } else {
                            Base::DateTime
                        }
                    }
                    (Base::Str, Base::Str) => Base::Str,
                    (Base::Duration, Base::Duration) => Base::Duration,
                    (Base::Bool, Base::Bool) if matches!(op, Eq | Ne) => Base::Bool,
                    _ => return mismatch(self),
                };
                (operand, operand, Base::Bool)
            }
            And | Or => {
                let boolean = |base| matches!(base, Base::Bool | Base::Null);
                if !boolean(left_base) || !boolean(right_base) {
                    return mismatch(self);
                }
                (Base::Bool, Base::Bool, Base::Bool)
            }
            Coalesce | In | NotIn => unreachable!("handled above"),
        };
        let convert = |expr: Expr, to: Base, nullable: bool| {
            let ty = base_type(to).with_null(nullable);
            coerce(expr, &ty).unwrap_or_else(|expr| expr)
        };
        let left = convert(left, left_to, left_null);
        let right = convert(right, right_to, right_null);
        let ty = base_type(result).with_null(left_null || right_null);
        Expr::new(
            ExprKind::Binary(op, Box::new(left), Box::new(right)),
            ty,
            span,
        )
    }

    /// `value in list` and `value not in list`: whether the list has an item equal to the
    /// value. The value and the items are compared as `==` compares them.
    fn membership(&mut self, span: Span, op: BinaryOp, left: Expr, right: Expr) -> Expr {
        let Type::List(item) = &right.ty else {
            let message = format!(
                "`{0}` needs a list on its right, as in `x {0} [1, 2]`, found {1}",
                op.symbol(),
                right.ty
            );
            return self.error(right.span, message);
        };
        let mismatch = |cx: &mut Self, left: &Expr, right: &Expr| {
            let message = format!("cannot look for {} in {}", left.ty, right.ty);
            cx.error(span, message)
        };
        let (Some((value_base, value_null)), Some((item_base, item_null))) =
            (base(&left.ty), base(item))
        else {
            return mismatch(self, &left, &right);
        };
        let number = |base| matches!(base, Base::Int | Base::Decimal | Base::Float);
        let moment = |base| matches!(base, Base::Date | Base::DateTime);
        // The type the value and the items are compared as.
        let operand = match (value_base, item_base) {
            (Base::Null, _) => {
                let message = "looking for `null` always gives null; \
                               use `is_null(...)` to test for null";
                return self.error(left.span, message);
            }
            // A list with no items, or with nothing but nulls.
            (value, Base::Null) => value,
            (value, item) if value == item => value,
            (value, item) if number(value) && number(item) => {
                let rank = |base| match base {
                    Base::Int => 0,
                    Base::Decimal => 1,
                    _ => 2,
                };
                if rank(value) >= rank(item) {
                    value
                } else {
                    item
                }
            }
            (value, item) if moment(value) && moment(item) => Base::DateTime,
            _ => return mismatch(self, &left, &right),
        };
        let value_ty = base_type(operand).with_null(value_null);
        let list_ty = Type::List(Box::new(base_type(operand).with_null(item_null)));
        // A list that is written out converts item by item; a list in a variable has to
        // have the right type already.
        let (left, right) = match (coerce(left, &value_ty), coerce(right, &list_ty)) {
            (Ok(left), Ok(right)) => (left, right),
            (Ok(left) | Err(left), Ok(right) | Err(right)) => {
                return mismatch(self, &left, &right);
            }
        };
        let ty = Type::Bool.with_null(value_null || item_null);
        let kind = ExprKind::Binary(BinaryOp::In, Box::new(left), Box::new(right));
        let found = Expr::new(kind, ty.clone(), span);
        match op {
            BinaryOp::NotIn => Expr::new(ExprKind::Not(Box::new(found)), ty, span),
            _ => found,
        }
    }

    fn coalesce(&mut self, span: Span, left: Expr, right: Expr) -> Expr {
        let (right_inner, right_null) = right.ty.split_null();
        let unified = unify(left.ty.split_null().0, right_inner);
        let inner = match unified.as_ref().map(|ty| ty.split_null().0) {
            Some(inner) if storable(inner) => inner.clone(),
            _ => {
                let message = format!(
                    "`??` needs a value and a fallback of one type, found {} and {}",
                    left.ty, right.ty
                );
                return self.error(span, message);
            }
        };
        let ty = inner.clone().with_null(right_null);
        let left = self.converted(left, &inner.or_null());
        let right = self.converted(right, &ty);
        let kind = ExprKind::Binary(BinaryOp::Coalesce, Box::new(left), Box::new(right));
        Expr::new(kind, ty, span)
    }

    fn if_expr(
        &mut self,
        span: Span,
        cond: ExprId,
        then_branch: ExprId,
        else_branch: Option<ExprId>,
        hint: Option<FnHint>,
    ) -> Expr {
        let cond = self.expr(cond);
        let cond = match &cond.ty {
            Type::Bool | Type::Error => cond,
            ty if base(ty).is_some_and(|(base, _)| matches!(base, Base::Bool | Base::Null)) => {
                let message = "this condition can be null; say what null means, \
                               for example with `?? false`";
                self.error(cond.span, message)
            }
            ty => {
                let message = format!("a condition must be a bool, found {ty}");
                self.error(cond.span, message)
            }
        };
        let then = self.expr_hinted(then_branch, hint.clone());
        let Some(else_branch) = else_branch else {
            let kind = ExprKind::If(Box::new(cond), Box::new(then), None);
            return Expr::new(kind, Type::Unit, span);
        };
        let otherwise = self.expr_hinted(else_branch, hint);
        let Some(ty) = unify(&then.ty, &otherwise.ty) else {
            let message = format!(
                "the branches of an `if` must have one type, found {} and {}",
                then.ty, otherwise.ty
            );
            return self.error(span, message);
        };
        let then = self.converted(then, &ty);
        let otherwise = self.converted(otherwise, &ty);
        let kind = ExprKind::If(Box::new(cond), Box::new(then), Some(Box::new(otherwise)));
        Expr::new(kind, ty, span)
    }

    fn block(&mut self, span: Span, stmts: &[ast::Stmt], mut hint: Option<FnHint>) -> Expr {
        self.frame().scopes.push(Vec::new());
        let mut checked = Vec::new();
        let mut value = None;
        for (index, stmt) in stmts.iter().enumerate() {
            match &stmt.kind {
                StmtKind::Expr(expr) => {
                    if index + 1 == stmts.len() {
                        let expr = self.expr_hinted(*expr, hint.take());
                        value = Some(Box::new(expr));
                    } else {
                        checked.push(Stmt::Expr(self.expr(*expr)));
                    }
                }
                StmtKind::Let { name, ty, value } => {
                    let value = self.let_value(ty.as_ref(), *value);
                    let local = self.declare_local(name.name, value.ty.clone());
                    checked.push(Stmt::Let(local, value));
                }
                StmtKind::Fn {
                    name,
                    params,
                    ret,
                    body,
                } => {
                    let id = self.declare_function(*name, params, ret.as_ref());
                    let names: Vec<Symbol> = params.iter().map(|param| param.name.name).collect();
                    let captures = self.function_body(id, &names, *body, Some(name.name));
                    // Outside its body the function is an ordinary variable holding a closure.
                    let ty = self.signature_type(id, None);
                    let closure = Expr::new(ExprKind::Closure(id, captures), ty.clone(), stmt.span);
                    let local = self.declare_local(name.name, ty);
                    checked.push(Stmt::Let(local, closure));
                }
                StmtKind::Type { name, .. } => {
                    self.error(name.span, "types can only be declared at the top level");
                }
                StmtKind::Import { .. } => {
                    self.error(stmt.span, "a file can only be imported at the top level");
                }
            }
        }
        self.frame().scopes.pop();
        let ty = match &value {
            Some(value) => value.ty.clone(),
            None => Type::Unit,
        };
        Expr::new(ExprKind::Block(checked, value), ty, span)
    }

    /// Checks the call `id`. `piped` is the left side of a `|>`, which becomes the first
    /// argument.
    fn call(&mut self, id: ExprId, piped: Option<ExprId>) -> Expr {
        let ast = self.ast;
        let ast::Expr::Call {
            callee,
            type_args,
            args,
        } = ast.expr(id)
        else {
            unreachable!("the parser only builds pipes into calls");
        };
        let span = match piped {
            Some(piped) => ast.span(piped).to(ast.span(id)),
            None => ast.span(id),
        };
        let piped = piped.map(|value| ArgSrc { name: None, value });
        let written = args.iter().map(|arg| ArgSrc {
            name: arg.name,
            value: arg.value,
        });
        let args: Vec<ArgSrc> = piped.into_iter().chain(written).collect();
        let callee_span = ast.span(*callee);

        let (callee_kind, callee_ty, name) = match ast.expr(*callee) {
            ast::Expr::Name(name) => {
                self.note_reach(callee_span, Place::Callee);
                let text = self.text(*name);
                // A user function comes first; any other meaning of the name gives way to a
                // built-in function, so a variable or column may share a built-in's name.
                let resolved = match self.resolve(*name, None) {
                    Some((_, Type::Fn(_) | Type::Error)) => self.resolve(*name, Some(callee_span)),
                    other => other,
                };
                let call = Call {
                    span,
                    name: &text,
                    args: &args,
                    type_args,
                };
                match resolved {
                    Some((kind, ty @ (Type::Fn(_) | Type::Error))) => (kind, ty, text),
                    other => {
                        let builtin = self.builtin_call(&call);
                        self.look_into(&call);
                        if let Some(result) = builtin {
                            return result;
                        }
                        let message = match other {
                            Some((_, ty)) => {
                                format!("`{text}` is not a function; it has type {ty}")
                            }
                            None => format!("undefined function `{text}`"),
                        };
                        return self.error(callee_span, message);
                    }
                }
            }
            _ => {
                let callee = self.expr(*callee);
                (callee.kind, callee.ty, "this function".into())
            }
        };
        let function = match callee_ty {
            Type::Fn(function) => function,
            Type::Error => {
                // Still check the arguments, so that their own errors are reported.
                args.iter().for_each(|arg| drop(self.expr(arg.value)));
                return Expr::error(span);
            }
            other => {
                let message = format!("cannot call a value of type {other}");
                return self.error(callee_span, message);
            }
        };
        let callee = match callee_kind {
            ExprKind::Function(id) => Callee::Function(id),
            kind => {
                let ty = Type::Fn(function.clone());
                Callee::Value(Box::new(Expr::new(kind, ty, callee_span)))
            }
        };
        let call = Call {
            span,
            name: &name,
            args: &args,
            type_args,
        };
        self.call_function(callee, &function, &call)
    }

    /// Matches the arguments of a call to the parameters of a user function.
    fn call_function(&mut self, callee: Callee, function: &FnType, call: &Call) -> Expr {
        let name = call.name;
        let mut failed = false;
        if let Some(first) = call.type_args.first() {
            self.error(first.span, format!("`{name}` does not take type arguments"));
            failed = true;
        }
        let params = &function.params;
        let mut filled = vec![false; params.len()];
        let mut args = Vec::new();
        let mut positional = 0;
        // What a parameter is called in a message; those of a function type have no names.
        let label = |index: usize| match params[index].text.is_empty() {
            true => format!("argument {}", index + 1),
            false => format!("`{}`", params[index].text),
        };
        let mut named = false;
        for arg in call.args {
            if arg.name.is_none() && named {
                let span = self.ast.span(arg.value);
                self.error(
                    span,
                    "positional arguments must come before named arguments",
                );
                failed = true;
            }
            named |= arg.name.is_some();
            // Which parameter the argument is for comes first: it tells a lambda the types
            // of its own parameters.
            let target = match arg.name {
                Some(arg_name) => params.iter().position(|p| p.name == arg_name.name),
                None => {
                    positional += 1;
                    Some(positional - 1)
                }
            };
            let value = match target.and_then(|index| params.get(index)) {
                Some(param) => self.expr_expecting(arg.value, &param.ty),
                None => self.expr(arg.value),
            };
            let (index, span) = match (arg.name, target) {
                (Some(arg_name), Some(index)) => (index, arg_name.span),
                (Some(arg_name), None) => {
                    let text = self.text(arg_name.name);
                    let message = format!("`{name}` has no parameter named `{text}`");
                    self.error(arg_name.span, message);
                    failed = true;
                    continue;
                }
                (None, target) => (target.expect("counted above"), value.span),
            };
            let Some(param) = params.get(index) else {
                let message = format!("too many arguments: `{name}` takes {}", params.len());
                self.error(span, message);
                failed = true;
                continue;
            };
            if std::mem::replace(&mut filled[index], true) {
                let message = format!("argument {} is given more than once", label(index));
                self.error(span, message);
                failed = true;
                continue;
            }
            match coerce(value, &param.ty) {
                Ok(value) => args.push(Arg {
                    value,
                    param: index as u32,
                }),
                Err(value) => {
                    // Of two long table types, what matters is the column that differs.
                    let message = match (&value.ty, &param.ty) {
                        (Type::Table(have), Type::Table(wanted)) => format!(
                            "`{name}` cannot take this table for {}: {}",
                            label(index),
                            narrowed(have, wanted).err().unwrap_or_default()
                        ),
                        _ => format!(
                            "`{name}` expects {} for {}, found {}",
                            param.ty,
                            label(index),
                            value.ty
                        ),
                    };
                    self.error(value.span, message);
                    failed = true;
                }
            }
        }
        if let Some(missing) = filled.iter().position(|filled| !filled)
            && !failed
        {
            let message = format!("missing argument {} in call to `{name}`", label(missing));
            return self.error(call.span, message);
        }
        if failed {
            return Expr::error(call.span);
        }
        Expr::new(
            ExprKind::Call(callee, args),
            function.ret.clone(),
            call.span,
        )
    }

    /// The column that `name` refers to inside the table operation being checked, if any.
    fn column(&self, name: Symbol) -> Option<Field> {
        let scope = self.columns.last()?;
        scope.schema.field(self.interner.resolve(name)).cloned()
    }
}
