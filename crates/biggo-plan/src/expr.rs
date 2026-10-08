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
            ExprKind::Call(func, _) => func.can_fail(),
            _ => false,
        };
        let mut fails = fails_here;
        self.for_each_child(&mut |child| fails |= child.can_fail());
        fails
    }
}

/// Declares a set of functions: each variant with the name a program calls it by.
macro_rules! functions {
    ($(#[$about:meta])* $set:ident { $($(#[$doc:meta])* $variant:ident => $name:literal,)* }) => {
        $(#[$about])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum $set {
            $($(#[$doc])* $variant,)*
        }

        impl $set {
            pub const ALL: &'static [$set] = &[$($set::$variant,)*];

            pub fn name(self) -> &'static str {
                match self {
                    $($set::$variant => $name,)*
                }
            }

            pub fn from_name(name: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|func| func.name() == name)
            }
        }
    };
}

functions! {
    /// A function of single values that also applies to whole columns.
    ScalarFn {
    IsNull => "is_null",
    Abs => "abs",
    Round => "round",
    Floor => "floor",
    Ceil => "ceil",
    Sqrt => "sqrt",
    Lower => "lower",
    Upper => "upper",
    Trim => "trim",
    Length => "length",
    Contains => "contains",
    StartsWith => "starts_with",
    EndsWith => "ends_with",
    Year => "year",
    Month => "month",
    Day => "day",
    ToInt => "to_int",
    ToFloat => "to_float",
    ToString => "to_string",
    Hour => "hour",
    Minute => "minute",
    Second => "second",
    ToDate => "to_date",
    ToDateTime => "to_datetime",
    ToDecimal => "to_decimal",
    /// A duration of the given number of days; `Hours`, `Minutes`, and `Seconds` likewise.
    Days => "days",
    Hours => "hours",
    Minutes => "minutes",
    Seconds => "seconds",
    /// The length of a duration in seconds.
    TotalSeconds => "total_seconds",
    /// Part of a string, by the position of its first character and its length.
    Substring => "substring",
    Replace => "replace",
    /// One of the pieces between the separators of a string, by position.
    SplitPart => "split_part",
    PadLeft => "pad_left",
    PadRight => "pad_right",
    /// Where one string first occurs in another.
    IndexOf => "index_of",
    /// Whether a regular expression matches anywhere in a string.
    RegexMatch => "regex_match",
    /// The first match of a regular expression, or one of its groups.
    RegexExtract => "regex_extract",
    RegexReplace => "regex_replace",
    /// The largest of two or more values in one row, and the smallest.
    Greatest => "greatest",
    Least => "least",
    /// Null if the two arguments are equal, otherwise the first.
    NullIf => "null_if",
    /// The conversions that give null for a value that does not convert.
    TryToInt => "try_to_int",
    TryToFloat => "try_to_float",
    TryToDecimal => "try_to_decimal",
    TryToDate => "try_to_date",
    TryToDateTime => "try_to_datetime",
    ToBool => "to_bool",
    TryToBool => "try_to_bool",
    Pow => "pow",
    Exp => "exp",
    Ln => "ln",
    Log10 => "log10",
    Log2 => "log2",
    /// The logarithm of the first argument to the base that the second gives.
    Log => "log",
    /// -1, 0 or 1, in the type of the argument.
    Sign => "sign",
    /// Division of whole numbers, dropping the remainder.
    Div => "div",
    /// A number without the digits past a position, which `round` would round at.
    Trunc => "trunc",
    IsNan => "is_nan",
    IsFinite => "is_finite",
    Sin => "sin",
    Cos => "cos",
    Tan => "tan",
    Asin => "asin",
    Acos => "acos",
    Atan => "atan",
    /// The angle of the point `(x, y)`, called as `atan2(y, x)`.
    Atan2 => "atan2",
    Degrees => "degrees",
    Radians => "radians",
    /// A number as people write it, with separators of thousands or a percent sign.
    ParseNumber => "parse_number",
    /// 1 for Monday to 7 for Sunday.
    Weekday => "weekday",
    /// The number of the ISO week.
    Week => "week",
    Quarter => "quarter",
    DayOfYear => "day_of_year",
    StartOfWeek => "start_of_week",
    StartOfMonth => "start_of_month",
    StartOfQuarter => "start_of_quarter",
    StartOfYear => "start_of_year",
    EndOfMonth => "end_of_month",
    AddDays => "add_days",
    AddMonths => "add_months",
    AddYears => "add_years",
    /// Whole units from the first moment to the second.
    DaysBetween => "days_between",
    MonthsBetween => "months_between",
    YearsBetween => "years_between",
    TotalDays => "total_days",
    TotalHours => "total_hours",
    TotalMinutes => "total_minutes",
    MakeDate => "make_date",
    MakeDateTime => "make_datetime",
    /// A date as text in a pattern such as `%d/%m/%Y`, and the other way round.
    FormatDate => "format_date",
    ParseDate => "parse_date",
    ParseDateTime => "parse_datetime",
    TryParseDate => "try_parse_date",
    TryParseDateTime => "try_parse_datetime",
    /// The start of the interval of a given length that a moment falls in.
    TimeBucket => "time_bucket",
    MonthName => "month_name",
    DayName => "day_name",
    IsWeekend => "is_weekend",
    /// Seconds since 1970-01-01T00:00:00, and the moment that many seconds after it.
    ToUnix => "to_unix",
    FromUnix => "from_unix",
    /// The fiscal year of a date, given the month that fiscal years start in.
    FiscalYear => "fiscal_year",
    /// The year of the Buddhist era.
    BuddhistYear => "buddhist_year",
    /// A number that is the same for the same value everywhere, and spread evenly.
    Hash => "hash",
    TrimLeft => "trim_left",
    TrimRight => "trim_right",
    /// The first characters of a string, and the last.
    Left => "left",
    Right => "right",
    Repeat => "repeat",
    Reverse => "reverse",
    /// Each word with its first letter in upper case.
    Title => "title",
    /// Values of any type joined as text, nulls left out.
    Concat => "concat",
    /// Whether a string fits a SQL pattern, in which `%` is any run of characters.
    Like => "like",
    RegexCount => "regex_count",
    /// A number as text with separators of thousands and a fixed count of decimals.
    FormatNumber => "format_number",
    FormatPercent => "format_percent",
    Sha256 => "sha256",
    Md5 => "md5",
    }
}

functions! {
    /// A function that reduces the rows of a group to one value.
    AggFn {
    Sum => "sum",
    Mean => "mean",
    Min => "min",
    Max => "max",
    /// The number of rows, or of non-null values when given an argument.
    Count => "count",
    CountDistinct => "count_distinct",
    First => "first",
    Last => "last",
    Median => "median",
    /// Sample standard deviation.
    Stddev => "stddev",
    /// Correlation of two columns.
    Corr => "corr",
    /// Sample covariance of two columns.
    Cov => "cov",
    /// The slope of the least-squares line through `(x, y)`, called as `slope(y, x)`.
    Slope => "slope",
    /// Where that line crosses `x = 0`.
    Intercept => "intercept",
    /// Sample variance, and the standard deviation and variance of a whole population.
    Variance => "variance",
    StddevPop => "stddev_pop",
    VariancePop => "variance_pop",
    /// The value below which a given share of the values lie.
    Quantile => "quantile",
    Product => "product",
    /// The values of a group joined into one string, with a separator between them.
    StringAgg => "string_agg",
    /// The value of one column in the row where another is largest, or smallest.
    ArgMax => "arg_max",
    ArgMin => "arg_min",
    }
}

impl ScalarFn {
    /// Whether the function can stop the program for some value, as `to_int` does for a
    /// string that is no number. A function counts as one that can unless it is listed here
    /// as safe: to forget a new function then costs a little speed, and never a wrong error.
    pub fn can_fail(self) -> bool {
        use ScalarFn::*;
        !matches!(
            self,
            IsNull
                | Floor
                | Ceil
                | Sqrt
                | Lower
                | Upper
                | Trim
                | Length
                | Contains
                | StartsWith
                | EndsWith
                | Year
                | Month
                | Day
                | Hour
                | Minute
                | Second
                | ToString
                | TotalSeconds
                | TotalMinutes
                | TotalHours
                | TotalDays
                | Substring
                | Replace
                | SplitPart
                | IndexOf
                | Greatest
                | Least
                | NullIf
                | TryToInt
                | TryToFloat
                | TryToDecimal
                | TryToDate
                | TryToDateTime
                | TryToBool
                | Pow
                | Exp
                | Ln
                | Log10
                | Log2
                | Log
                | Sign
                | Trunc
                | IsNan
                | IsFinite
                | Sin
                | Cos
                | Tan
                | Asin
                | Acos
                | Atan
                | Atan2
                | Degrees
                | Radians
                | ParseNumber
                | Weekday
                | DayName
                | IsWeekend
                | ToUnix
                | Hash
                | TrimLeft
                | TrimRight
                | Left
                | Right
                | Reverse
                | Title
                | Concat
                | FormatNumber
                | FormatPercent
                | Sha256
                | Md5
        )
    }
}

/// What an aggregate takes besides the column it works on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Extra {
    Nothing,
    /// A second column, read for every row.
    Column,
    /// A value that is the same for every row, such as a separator.
    Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AggCall {
    pub func: AggFn,
    pub arg: Option<Expr>,
    /// What the aggregate takes besides its column: see `AggFn::extra`.
    pub arg2: Option<Expr>,
    pub ty: ColType,
}

impl AggFn {
    /// Whether the aggregate relates two columns of numbers.
    pub fn is_pair(self) -> bool {
        matches!(
            self,
            AggFn::Corr | AggFn::Cov | AggFn::Slope | AggFn::Intercept
        )
    }

    pub fn extra(self) -> Extra {
        match self {
            AggFn::Corr
            | AggFn::Cov
            | AggFn::Slope
            | AggFn::Intercept
            | AggFn::ArgMax
            | AggFn::ArgMin => Extra::Column,
            AggFn::Quantile | AggFn::StringAgg => Extra::Value,
            _ => Extra::Nothing,
        }
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
    /// Mean of the row and the `offset - 1` rows before it, and likewise their sum, their
    /// smallest and their largest.
    MovingAvg,
    MovingSum,
    MovingMin,
    MovingMax,
    /// A rank that goes up by one from each set of equal rows to the next.
    DenseRank,
    /// The rank as a share of the way from the first row to the last.
    PercentRank,
    /// Which of `offset` groups of nearly equal size the row falls in.
    Ntile,
    /// The number of rows up to and including the row, or of values among them.
    CumCount,
    CumMin,
    CumMax,
    CumMean,
    /// The value less the value `offset` rows earlier, and that as a share of the earlier.
    Diff,
    PctChange,
    /// The value, or where it is null the last value before it, or the next after it.
    FillForward,
    FillBackward,
    /// An aggregate of the whole partition.
    Agg(AggFn),
}

impl WindowFn {
    /// The window functions that are not aggregates, each with its name.
    pub const OWN: &'static [(WindowFn, &'static str)] = &[
        (WindowFn::RowNumber, "row_number"),
        (WindowFn::Rank, "rank"),
        (WindowFn::Lag, "lag"),
        (WindowFn::Lead, "lead"),
        (WindowFn::CumSum, "cumsum"),
        (WindowFn::MovingAvg, "moving_avg"),
        (WindowFn::MovingSum, "moving_sum"),
        (WindowFn::MovingMin, "moving_min"),
        (WindowFn::MovingMax, "moving_max"),
        (WindowFn::DenseRank, "dense_rank"),
        (WindowFn::PercentRank, "percent_rank"),
        (WindowFn::Ntile, "ntile"),
        (WindowFn::CumCount, "cum_count"),
        (WindowFn::CumMin, "cum_min"),
        (WindowFn::CumMax, "cum_max"),
        (WindowFn::CumMean, "cum_mean"),
        (WindowFn::Diff, "diff"),
        (WindowFn::PctChange, "pct_change"),
        (WindowFn::FillForward, "fill_forward"),
        (WindowFn::FillBackward, "fill_backward"),
    ];

    pub fn name(self) -> &'static str {
        match self {
            WindowFn::Agg(func) => func.name(),
            own => {
                let listed = Self::OWN.iter().find(|(func, _)| *func == own);
                listed.map_or("window function", |(_, name)| name)
            }
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        let own = Self::OWN.iter().find(|(_, own)| *own == name);
        match own {
            Some((func, _)) => Some(*func),
            None => AggFn::from_name(name).map(WindowFn::Agg),
        }
    }

    /// Whether the function takes a second argument: a row distance or a window size.
    pub fn has_offset(self) -> bool {
        matches!(
            self,
            WindowFn::Lag
                | WindowFn::Lead
                | WindowFn::MovingAvg
                | WindowFn::MovingSum
                | WindowFn::MovingMin
                | WindowFn::MovingMax
                | WindowFn::Diff
                | WindowFn::PctChange
        )
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
    /// Whether nulls come before every value. They come after, unless this says otherwise.
    pub nulls_first: bool,
}

fn precedence(op: BinaryOp) -> u8 {
    use BinaryOp::*;
    match op {
        Or => 1,
        And => 2,
        Eq | Ne | Lt | Le | Gt | Ge | In | NotIn => 3,
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
        if self.func == WindowFn::Ntile {
            write!(f, "{}", self.offset)?;
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
        if self.nulls_first {
            f.write_str(" nulls first")?;
        }
        Ok(())
    }
}
