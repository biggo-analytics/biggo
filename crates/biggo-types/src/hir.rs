//! The checked form of a program. Every name is resolved to where its value lives, every
//! conversion is explicit, call arguments are matched to parameters, and each table operation
//! carries the plan node it builds.

use std::sync::Arc;

use biggo_plan::{AggFn, ScalarFn, TableOp, WindowFn};
use biggo_syntax::Span;
use biggo_syntax::ast::BinaryOp;

use crate::ty::Type;

/// One checked module: a source file, or a single entry typed at the REPL.
#[derive(Debug)]
pub struct Program {
    /// The functions this module declares. Function ids are shared by all modules of a
    /// session, and these take the ids from `first_function` on.
    pub functions: Vec<Function>,
    pub first_function: u32,
    /// The top-level statements.
    pub main: Function,
    /// The number of global slots in use once this module has run.
    pub globals: u32,
    /// The type of the module's value: that of its last statement.
    pub result: Type,
}

#[derive(Debug)]
pub struct Function {
    pub name: Arc<str>,
    /// Parameters are the first locals, in order.
    pub params: u32,
    pub locals: u32,
    pub body: Expr,
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub ty: Type,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Unit,
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Arc<str>),
    /// Days since 1970-01-01.
    Date(i32),
    /// Microseconds since 1970-01-01T00:00:00.
    DateTime(i64),
    /// The value times 10 to the power `scalar::DECIMAL_SCALE`.
    Decimal(i128),

    Local(u32),
    /// A variable of an enclosing function, copied into this function when it was created.
    Capture(u32),
    /// The local function that is running, for recursion.
    SelfFn,
    Global(u32, Arc<str>),
    /// A top-level function used as a value.
    Function(u32),

    /// A column of the table that the enclosing table operation reads. Like `Agg` and
    /// `Window`, it only occurs while that operation is being checked, never in a `Program`.
    Column(Arc<str>),
    /// An aggregate and its arguments: none, one column, or two.
    Agg(AggFn, Vec<Expr>),
    Window(WindowFn, Option<Box<Expr>>, usize),

    List(Vec<Expr>),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    /// Without an else branch the value is unit, whatever the then branch computes.
    If(Box<Expr>, Box<Expr>, Option<Box<Expr>>),
    /// Statements and the value of the block; unit if there is none.
    Block(Vec<Stmt>, Option<Box<Expr>>),
    /// Arguments are in the order they are evaluated.
    Call(Callee, Vec<Arg>),
    /// Converts an int, or a nullable int, to float.
    ToFloat(Box<Expr>),
    /// Another conversion that the types call for. Null stays null.
    Convert(Conversion, Box<Expr>),
    /// A record: its field names and their values, in the order of its type.
    Record(Arc<[Arc<str>]>, Vec<Expr>),
    /// The field of a record at the given position.
    Field(Box<Expr>, u32),
    Map(Vec<(Expr, Expr)>),
    /// An element of a list, or the value of a key in a map, which is null if absent.
    Index(Box<Expr>, Box<Expr>),
    /// A local function and the values it captures.
    Closure(u32, Vec<Expr>),
    Builtin(Builtin, Vec<Expr>),
    Scalar(ScalarFn, Vec<Expr>),
    Table(Box<TableExpr>),
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conversion {
    IntToDecimal,
    DecimalToFloat,
    /// A date becomes the datetime at its midnight.
    DateToDateTime,
}

#[derive(Clone, Debug)]
pub struct Arg {
    pub value: Expr,
    /// The index of the parameter that receives the value.
    pub param: u32,
}

#[derive(Clone, Debug)]
pub enum Callee {
    Function(u32),
    Value(Box<Expr>),
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Let(u32, Expr),
    Global(u32, Expr),
    Expr(Expr),
}

/// A table operation: the plan node it builds, the tables it reads, and the scalar values its
/// column expressions refer to as parameters.
#[derive(Clone, Debug)]
pub struct TableExpr {
    pub op: TableOp,
    pub inputs: Vec<Expr>,
    pub params: Vec<Expr>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Builtin {
    Print,
    /// Shows a value as the REPL does: as a literal, or as a table. The checker never
    /// produces it; the REPL wraps the last expression of an entry in it.
    Echo,
    WriteCsv,
    WriteParquet,
    /// Runs the query and keeps the result in memory.
    Collect,
    /// The number of rows of a table.
    Count,
    Explain,
    WriteJson,
    /// Writes a table as a table of a SQLite database, in place of the one of that name.
    WriteSqlite,
    /// The rows of a table as a list of records.
    ToRows,
    /// The one row of a table that has exactly one, as a record.
    OnlyRow,
    /// A table whose rows are the records of a list.
    FromRows,
    /// A table of summary statistics for each column.
    Describe,
    /// Counts of a column's values in ranges of equal width.
    Histogram,
    /// The number of elements of a list or entries of a map.
    Len,
    /// The whole numbers from the first argument up to, not including, the second.
    Range,
    /// Applies a function to each element of a list.
    MapList,
    /// Keeps the elements of a list for which a function gives true.
    Filter,
    /// Calls a function with each element of a list, for what the function does.
    Each,
    /// Combines the elements of a list into one value, starting from an initial one.
    Fold,
    Keys,
    Values,
    /// A map with one entry added or replaced.
    Put,
    HasKey,
    /// Stops the program unless its argument is true.
    Assert,
    /// Stops the program unless its two arguments are equal.
    AssertEq,
    /// The arguments the program was started with, as a list of strings.
    Args,
    /// The pieces of a string between its separators, as a list.
    Split,
}

impl Expr {
    pub fn new(kind: ExprKind, ty: Type, span: Span) -> Self {
        Self { kind, ty, span }
    }

    pub fn error(span: Span) -> Self {
        Self::new(ExprKind::Error, Type::Error, span)
    }

    /// Calls `f` with each direct subexpression.
    pub fn for_each_child<'a>(&'a self, f: &mut impl FnMut(&'a Expr)) {
        let stmts = |stmts: &'a [Stmt], f: &mut dyn FnMut(&'a Expr)| {
            for stmt in stmts {
                match stmt {
                    Stmt::Let(_, value) | Stmt::Global(_, value) | Stmt::Expr(value) => f(value),
                }
            }
        };
        match &self.kind {
            ExprKind::Unit
            | ExprKind::Null
            | ExprKind::Bool(_)
            | ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Str(_)
            | ExprKind::Date(_)
            | ExprKind::DateTime(_)
            | ExprKind::Decimal(_)
            | ExprKind::Local(_)
            | ExprKind::Capture(_)
            | ExprKind::SelfFn
            | ExprKind::Global(..)
            | ExprKind::Function(_)
            | ExprKind::Column(_)
            | ExprKind::Error => {}
            ExprKind::Window(_, arg, _) => {
                if let Some(arg) = arg {
                    f(arg);
                }
            }
            ExprKind::Convert(_, operand) | ExprKind::Field(operand, _) => f(operand),
            ExprKind::Index(base, index) => {
                f(base);
                f(index);
            }
            ExprKind::Map(entries) => {
                for (key, value) in entries {
                    f(key);
                    f(value);
                }
            }
            ExprKind::List(items)
            | ExprKind::Agg(_, items)
            | ExprKind::Record(_, items)
            | ExprKind::Closure(_, items)
            | ExprKind::Builtin(_, items)
            | ExprKind::Scalar(_, items) => items.iter().for_each(f),
            ExprKind::Neg(operand) | ExprKind::Not(operand) | ExprKind::ToFloat(operand) => {
                f(operand)
            }
            ExprKind::Binary(_, left, right) => {
                f(left);
                f(right);
            }
            ExprKind::If(cond, then, otherwise) => {
                f(cond);
                f(then);
                if let Some(otherwise) = otherwise {
                    f(otherwise);
                }
            }
            ExprKind::Block(block, value) => {
                stmts(block, f);
                if let Some(value) = value {
                    f(value);
                }
            }
            ExprKind::Call(callee, args) => {
                if let Callee::Value(callee) = callee {
                    f(callee);
                }
                args.iter().for_each(|arg| f(&arg.value));
            }
            ExprKind::Table(table) => {
                table.inputs.iter().for_each(&mut *f);
                table.params.iter().for_each(f);
            }
        }
    }

    /// The first place, if any, where the expression reads a column or aggregates.
    pub fn find_column_use(&self) -> Option<&Expr> {
        if matches!(
            self.kind,
            ExprKind::Column(_) | ExprKind::Agg(..) | ExprKind::Window(..)
        ) {
            return Some(self);
        }
        let mut found = None;
        self.for_each_child(&mut |child| {
            if found.is_none() {
                found = child.find_column_use();
            }
        });
        found
    }
}
