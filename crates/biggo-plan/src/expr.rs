use std::fmt;
use std::sync::Arc;

use biggo_syntax::ast::BinaryOp;

use crate::schema::{ColType, Name, Scalar};

/// An expression over the columns of one table, evaluated a whole column at a time.
#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub ty: ColType,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    Column(Arc<str>),
    Literal(Scalar),
    /// A value computed by the surrounding program, filled in when the plan is built.
    Param(usize),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    If(Box<Expr>, Box<Expr>, Box<Expr>),
    /// Converts its operand to this expression's type.
    Cast(Box<Expr>),
    Call(ScalarFn, Vec<Expr>),
}

impl Expr {
    pub fn new(kind: ExprKind, ty: ColType) -> Self {
        Self { kind, ty }
    }

    pub fn column(name: impl Into<Arc<str>>, ty: ColType) -> Self {
        Self::new(ExprKind::Column(name.into()), ty)
    }

    pub fn literal(value: Scalar, ty: ColType) -> Self {
        Self::new(ExprKind::Literal(value), ty)
    }

    pub fn as_column(&self) -> Option<&Arc<str>> {
        match &self.kind {
            ExprKind::Column(name) => Some(name),
            _ => None,
        }
    }

    /// Rebuilds the expression with `f` applied to each direct operand.
    pub fn map_children(&self, f: &mut impl FnMut(&Expr) -> Expr) -> Expr {
        let mut boxed = |expr: &Expr| Box::new(f(expr));
        let kind = match &self.kind {
            ExprKind::Column(_) | ExprKind::Literal(_) | ExprKind::Param(_) => self.kind.clone(),
            ExprKind::Neg(operand) => ExprKind::Neg(boxed(operand)),
            ExprKind::Not(operand) => ExprKind::Not(boxed(operand)),
            ExprKind::Binary(op, left, right) => ExprKind::Binary(*op, boxed(left), boxed(right)),
            ExprKind::If(cond, then, otherwise) => {
                ExprKind::If(boxed(cond), boxed(then), boxed(otherwise))
            }
            ExprKind::Cast(operand) => ExprKind::Cast(boxed(operand)),
            ExprKind::Call(func, args) => ExprKind::Call(*func, args.iter().map(f).collect()),
        };
        Expr::new(kind, self.ty)
    }

    pub fn for_each_child(&self, f: &mut impl FnMut(&Expr)) {
        match &self.kind {
            ExprKind::Column(_) | ExprKind::Literal(_) | ExprKind::Param(_) => {}
            ExprKind::Neg(operand) | ExprKind::Not(operand) | ExprKind::Cast(operand) => f(operand),
            ExprKind::Binary(_, left, right) => {
                f(left);
                f(right);
            }
            ExprKind::If(cond, then, otherwise) => {
                f(cond);
                f(then);
                f(otherwise);
            }
            ExprKind::Call(_, args) => args.iter().for_each(f),
        }
    }

    /// Calls `f` with the name of every column the expression reads.
    pub fn for_each_column(&self, f: &mut impl FnMut(&Arc<str>)) {
        match &self.kind {
            ExprKind::Column(name) => f(name),
            _ => self.for_each_child(&mut |child| child.for_each_column(f)),
        }
    }

    pub fn reads_any(&self, mut is_wanted: impl FnMut(&str) -> bool) -> bool {
        let mut found = false;
        self.for_each_column(&mut |name| found |= is_wanted(name));
        found
    }

    /// Replaces each column for which `replacement` returns an expression.
    pub fn substitute(&self, replacement: &impl Fn(&str) -> Option<Expr>) -> Expr {
        match &self.kind {
            ExprKind::Column(name) => replacement(name).unwrap_or_else(|| self.clone()),
            _ => self.map_children(&mut |child| child.substitute(replacement)),
        }
    }

    /// Replaces every parameter with its value.
    pub fn bind(&self, params: &[Scalar]) -> Expr {
        match &self.kind {
            ExprKind::Param(index) => Expr::literal(params[*index].clone(), self.ty),
            _ => self.map_children(&mut |child| child.bind(params)),
        }
    }

    /// Whether evaluating the expression can stop the program with an error, as integer
    /// overflow or `%` by zero do. Such an expression must not be evaluated for rows that the
    /// program would have skipped.
    pub fn can_fail(&self) -> bool {
        let fails_here = match &self.kind {
            // Exact arithmetic stops on overflow, and on division by zero.
            ExprKind::Binary(op, left, _) => {
                use crate::DataType::{Decimal, Int};
                use BinaryOp::*;
                let exact = matches!(left.ty.dtype, Int | Decimal);
                matches!(op, Rem)
                    || (exact && matches!(op, Add | Sub | Mul))
                    || (left.ty.dtype == Decimal && matches!(op, Div))
            }
            ExprKind::Neg(operand) => operand.ty.dtype == crate::DataType::Int,
            // The conversions the type checker inserts always succeed; those a program asks
            // for may meet a value that does not convert.
            ExprKind::Cast(_) => false,
            ExprKind::Call(func, _) => matches!(
                func,
                ScalarFn::ToInt
                    | ScalarFn::ToFloat
                    | ScalarFn::ToDecimal
                    | ScalarFn::ToDate
                    | ScalarFn::ToDateTime
                    | ScalarFn::Round
                    | ScalarFn::Days
                    | ScalarFn::Hours
                    | ScalarFn::Minutes
                    | ScalarFn::Seconds
            ),
            _ => false,
        };
        let mut fails = fails_here;
        self.for_each_child(&mut |child| fails |= child.can_fail());
        fails
    }
}

/// A function of single values that also applies to whole columns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScalarFn {
    IsNull,
    Abs,
    Round,
    Floor,
    Ceil,
    Sqrt,
    Lower,
    Upper,
    Trim,
    Length,
    Contains,
    StartsWith,
    EndsWith,
    Year,
    Month,
    Day,
    ToInt,
    ToFloat,
    ToString,
    Hour,
    Minute,
    Second,
    ToDate,
    ToDateTime,
    ToDecimal,
    /// A duration of the given number of days; `Hours`, `Minutes`, and `Seconds` likewise.
    Days,
    Hours,
    Minutes,
    Seconds,
    /// The length of a duration in seconds.
    TotalSeconds,
    /// Part of a string, by the position of its first character and its length.
    Substring,
    Replace,
    /// One of the pieces between the separators of a string, by position.
    SplitPart,
    PadLeft,
    PadRight,
    /// Where one string first occurs in another.
    IndexOf,
    /// Whether a regular expression matches anywhere in a string.
    RegexMatch,
    /// The first match of a regular expression, or one of its groups.
    RegexExtract,
    RegexReplace,
}

impl ScalarFn {
    pub const ALL: [ScalarFn; 39] = [
        ScalarFn::IsNull,
        ScalarFn::Abs,
        ScalarFn::Round,
        ScalarFn::Floor,
        ScalarFn::Ceil,
        ScalarFn::Sqrt,
        ScalarFn::Lower,
        ScalarFn::Upper,
        ScalarFn::Trim,
        ScalarFn::Length,
        ScalarFn::Contains,
        ScalarFn::StartsWith,
        ScalarFn::EndsWith,
        ScalarFn::Year,
        ScalarFn::Month,
        ScalarFn::Day,
        ScalarFn::ToInt,
        ScalarFn::ToFloat,
        ScalarFn::ToString,
        ScalarFn::Hour,
        ScalarFn::Minute,
        ScalarFn::Second,
        ScalarFn::ToDate,
        ScalarFn::ToDateTime,
        ScalarFn::ToDecimal,
        ScalarFn::Days,
        ScalarFn::Hours,
        ScalarFn::Minutes,
        ScalarFn::Seconds,
        ScalarFn::TotalSeconds,
        ScalarFn::Substring,
        ScalarFn::Replace,
        ScalarFn::SplitPart,
        ScalarFn::PadLeft,
        ScalarFn::PadRight,
        ScalarFn::IndexOf,
        ScalarFn::RegexMatch,
        ScalarFn::RegexExtract,
        ScalarFn::RegexReplace,
    ];

    pub fn name(self) -> &'static str {
        match self {
            ScalarFn::IsNull => "is_null",
            ScalarFn::Abs => "abs",
            ScalarFn::Round => "round",
            ScalarFn::Floor => "floor",
            ScalarFn::Ceil => "ceil",
            ScalarFn::Sqrt => "sqrt",
            ScalarFn::Lower => "lower",
            ScalarFn::Upper => "upper",
            ScalarFn::Trim => "trim",
            ScalarFn::Length => "length",
            ScalarFn::Contains => "contains",
            ScalarFn::StartsWith => "starts_with",
            ScalarFn::EndsWith => "ends_with",
            ScalarFn::Year => "year",
            ScalarFn::Month => "month",
            ScalarFn::Day => "day",
            ScalarFn::ToInt => "to_int",
            ScalarFn::ToFloat => "to_float",
            ScalarFn::ToString => "to_string",
            ScalarFn::Hour => "hour",
            ScalarFn::Minute => "minute",
            ScalarFn::Second => "second",
            ScalarFn::ToDate => "to_date",
            ScalarFn::ToDateTime => "to_datetime",
            ScalarFn::ToDecimal => "to_decimal",
            ScalarFn::Days => "days",
            ScalarFn::Hours => "hours",
            ScalarFn::Minutes => "minutes",
            ScalarFn::Seconds => "seconds",
            ScalarFn::TotalSeconds => "total_seconds",
            ScalarFn::Substring => "substring",
            ScalarFn::Replace => "replace",
            ScalarFn::SplitPart => "split_part",
            ScalarFn::PadLeft => "pad_left",
            ScalarFn::PadRight => "pad_right",
            ScalarFn::IndexOf => "index_of",
            ScalarFn::RegexMatch => "regex_match",
            ScalarFn::RegexExtract => "regex_extract",
            ScalarFn::RegexReplace => "regex_replace",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|func| func.name() == name)
    }
}

/// A function that reduces the rows of a group to one value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AggFn {
    Sum,
    Mean,
    Min,
    Max,
    /// The number of rows, or of non-null values when given an argument.
    Count,
    CountDistinct,
    First,
    Last,
    Median,
    /// Sample standard deviation.
    Stddev,
    /// Correlation of two columns.
    Corr,
    /// Sample covariance of two columns.
    Cov,
    /// The slope of the least-squares line through `(x, y)`, called as `slope(y, x)`.
    Slope,
    /// Where that line crosses `x = 0`.
    Intercept,
}

impl AggFn {
    pub const ALL: [AggFn; 14] = [
        AggFn::Sum,
        AggFn::Mean,
        AggFn::Min,
        AggFn::Max,
        AggFn::Count,
        AggFn::CountDistinct,
        AggFn::First,
        AggFn::Last,
        AggFn::Median,
        AggFn::Stddev,
        AggFn::Corr,
        AggFn::Cov,
        AggFn::Slope,
        AggFn::Intercept,
    ];

    pub fn name(self) -> &'static str {
        match self {
            AggFn::Sum => "sum",
            AggFn::Mean => "mean",
            AggFn::Min => "min",
            AggFn::Max => "max",
            AggFn::Count => "count",
            AggFn::CountDistinct => "count_distinct",
            AggFn::First => "first",
            AggFn::Last => "last",
            AggFn::Median => "median",
            AggFn::Stddev => "stddev",
            AggFn::Corr => "corr",
            AggFn::Cov => "cov",
            AggFn::Slope => "slope",
            AggFn::Intercept => "intercept",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|func| func.name() == name)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AggCall {
    pub func: AggFn,
    pub arg: Option<Expr>,
    /// The second column of the aggregates that relate two.
    pub arg2: Option<Expr>,
    pub ty: ColType,
}

impl AggFn {
    /// Whether the aggregate takes two columns.
    pub fn is_pair(self) -> bool {
        matches!(
            self,
            AggFn::Corr | AggFn::Cov | AggFn::Slope | AggFn::Intercept
        )
    }
}

/// A function computed for each row from the rows of its partition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowFn {
    RowNumber,
    Rank,
    /// The value `offset` rows earlier in the partition.
    Lag,
    /// The value `offset` rows later in the partition.
    Lead,
    /// Running total up to and including the row.
    CumSum,
    /// Mean of the row and the `offset - 1` rows before it.
    MovingAvg,
    /// An aggregate of the whole partition.
    Agg(AggFn),
}

impl WindowFn {
    pub fn name(self) -> &'static str {
        match self {
            WindowFn::RowNumber => "row_number",
            WindowFn::Rank => "rank",
            WindowFn::Lag => "lag",
            WindowFn::Lead => "lead",
            WindowFn::CumSum => "cumsum",
            WindowFn::MovingAvg => "moving_avg",
            WindowFn::Agg(func) => func.name(),
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "row_number" => WindowFn::RowNumber,
            "rank" => WindowFn::Rank,
            "lag" => WindowFn::Lag,
            "lead" => WindowFn::Lead,
            "cumsum" => WindowFn::CumSum,
            "moving_avg" => WindowFn::MovingAvg,
            _ => WindowFn::Agg(AggFn::from_name(name)?),
        })
    }

    /// Whether the function takes a second argument: a row distance or a window size.
    pub fn has_offset(self) -> bool {
        matches!(self, WindowFn::Lag | WindowFn::Lead | WindowFn::MovingAvg)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WindowCall {
    pub func: WindowFn,
    pub arg: Option<Expr>,
    pub offset: usize,
    pub ty: ColType,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SortKey {
    pub expr: Expr,
    pub descending: bool,
}

fn precedence(op: BinaryOp) -> u8 {
    use BinaryOp::*;
    match op {
        Or => 1,
        And => 2,
        Eq | Ne | Lt | Le | Gt | Ge => 3,
        Coalesce => 4,
        Add | Sub => 5,
        Mul | Div | Rem => 6,
    }
}

impl Expr {
    fn fmt_operand(&self, f: &mut fmt::Formatter<'_>, parent: u8, right: bool) -> fmt::Result {
        let needs_parens = match &self.kind {
            // Operators group to the left, except `??`.
            ExprKind::Binary(op, ..) => {
                let own = precedence(*op);
                own < parent || (own == parent && right != (*op == BinaryOp::Coalesce))
            }
            ExprKind::If(..) => true,
            _ => false,
        };
        if needs_parens {
            write!(f, "({self})")
        } else {
            write!(f, "{self}")
        }
    }
}

/// Formats the expression in source syntax.
impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            ExprKind::Column(name) => write!(f, "{}", Name(name)),
            ExprKind::Literal(value) => write!(f, "{value}"),
            ExprKind::Param(index) => write!(f, "${index}"),
            ExprKind::Neg(operand) => {
                f.write_str("-")?;
                operand.fmt_operand(f, 7, false)
            }
            ExprKind::Not(operand) => {
                f.write_str("not ")?;
                operand.fmt_operand(f, 3, false)
            }
            ExprKind::Binary(op, left, right) => {
                let own = precedence(*op);
                left.fmt_operand(f, own, false)?;
                write!(f, " {} ", op.symbol())?;
                right.fmt_operand(f, own, true)
            }
            ExprKind::If(cond, then, otherwise) => {
                write!(f, "if {cond} {{ {then} }} else {{ {otherwise} }}")
            }
            ExprKind::Cast(operand) => write!(f, "{}({operand})", self.ty.dtype.name()),
            ExprKind::Call(func, args) => {
                write!(f, "{}(", func.name())?;
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{arg}")?;
                }
                f.write_str(")")
            }
        }
    }
}

impl fmt::Display for AggCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.arg, &self.arg2) {
            (Some(arg), Some(arg2)) => write!(f, "{}({arg}, {arg2})", self.func.name()),
            (Some(arg), None) => write!(f, "{}({arg})", self.func.name()),
            _ => write!(f, "{}()", self.func.name()),
        }
    }
}

impl fmt::Display for WindowCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}(", self.func.name())?;
        if let Some(arg) = &self.arg {
            write!(f, "{arg}")?;
            if self.func.has_offset() {
                write!(f, ", {}", self.offset)?;
            }
        }
        f.write_str(")")
    }
}

impl fmt::Display for SortKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.expr)?;
        if self.descending {
            f.write_str(" desc")?;
        }
        Ok(())
    }
}
