//! The syntax tree. Expressions live in an arena owned by `Ast` and refer to each other by
//! `ExprId`, so later passes can attach facts to them in side tables indexed by id.

use std::fmt;
use std::sync::Arc;

use crate::intern::Symbol;
use crate::span::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ExprId(u32);

impl ExprId {
    /// A dense index, usable as a key into a table of per-expression data.
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Debug, Default)]
pub struct Ast {
    /// Top-level statements in source order.
    pub stmts: Vec<Stmt>,
    exprs: Vec<Expr>,
    spans: Vec<Span>,
}

impl Ast {
    pub fn expr(&self, id: ExprId) -> &Expr {
        &self.exprs[id.0 as usize]
    }

    pub fn span(&self, id: ExprId) -> Span {
        self.spans[id.0 as usize]
    }

    pub fn expr_count(&self) -> usize {
        self.exprs.len()
    }

    /// The id of every expression of the tree.
    pub fn expr_ids(&self) -> impl Iterator<Item = ExprId> {
        (0..self.exprs.len() as u32).map(ExprId)
    }

    pub(crate) fn alloc(&mut self, expr: Expr, span: Span) -> ExprId {
        let id = ExprId(self.exprs.len() as u32);
        self.exprs.push(expr);
        self.spans.push(span);
        id
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ident {
    pub name: Symbol,
    pub span: Span,
}

#[derive(Debug)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Debug)]
pub enum StmtKind {
    Let {
        name: Ident,
        ty: Option<TypeExpr>,
        value: ExprId,
    },
    Type {
        name: Ident,
        ty: TypeExpr,
    },
    Fn {
        name: Ident,
        params: Vec<Param>,
        ret: Option<TypeExpr>,
        /// Always a `Block`.
        body: ExprId,
    },
    /// `import "path"`: the definitions of another file.
    Import {
        path: Arc<str>,
    },
    Expr(ExprId),
}

#[derive(Debug)]
pub struct Param {
    pub name: Ident,
    pub ty: TypeExpr,
    /// The value the parameter has in a call that does not give it one.
    pub default: Option<ExprId>,
}

#[derive(Debug)]
pub enum Expr {
    Int(i64),
    Float(f64),
    Str(Arc<str>),
    Bool(bool),
    Null,
    Date(Date),
    /// Microseconds since 1970-01-01T00:00:00.
    DateTime(i64),
    /// The value times 10 to the power `scalar::DECIMAL_SCALE`.
    Decimal(i128),
    Name(Symbol),
    List(Vec<ExprId>),
    /// `{ name: value, ... }`
    Record(Vec<(Ident, ExprId)>),
    /// `{ ...base, name: value }`: the fields of a record, with some of them given anew
    /// and others added.
    Update {
        base: ExprId,
        fields: Vec<(Ident, ExprId)>,
    },
    /// `{ "key": value, ... }`
    Map(Vec<(ExprId, ExprId)>),
    /// `base[index]`
    Index {
        base: ExprId,
        index: ExprId,
    },
    /// `fn(x: int) -> int { ... }`; the body is always a `Block`.
    Lambda {
        params: Vec<LambdaParam>,
        ret: Option<TypeExpr>,
        body: ExprId,
    },
    Match {
        scrutinee: ExprId,
        arms: Vec<MatchArm>,
    },
    Unary {
        op: UnaryOp,
        operand: ExprId,
    },
    Binary {
        op: BinaryOp,
        lhs: ExprId,
        rhs: ExprId,
    },
    /// `lhs |> rhs`, where `rhs` is always a `Call` that takes `lhs` as its first argument.
    Pipe {
        lhs: ExprId,
        rhs: ExprId,
    },
    Call {
        callee: ExprId,
        type_args: Vec<TypeExpr>,
        args: Vec<Arg>,
    },
    Field {
        base: ExprId,
        name: Ident,
    },
    If {
        cond: ExprId,
        /// Always a `Block`.
        then_branch: ExprId,
        /// A `Block`, or another `If` for `else if`.
        else_branch: Option<ExprId>,
    },
    /// The value of a block is its last statement, when that is an expression.
    Block(Vec<Stmt>),
}

/// A parameter of a lambda; its type may be left to the context.
#[derive(Debug)]
pub struct LambdaParam {
    pub name: Ident,
    pub ty: Option<TypeExpr>,
}

/// `pattern | pattern => body`
#[derive(Debug)]
pub struct MatchArm {
    pub patterns: Vec<Pattern>,
    pub body: ExprId,
}

#[derive(Debug)]
pub enum Pattern {
    /// `_`, which matches anything.
    Wildcard(Span),
    /// A literal to compare with.
    Value(ExprId),
}

/// A call argument, named when written as `name = value`.
#[derive(Debug)]
pub struct Arg {
    pub name: Option<Ident>,
    pub value: ExprId,
}

/// A calendar date that is known to exist. Dates order chronologically.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date {
    pub year: u16,
    pub month: u8,
    pub day: u8,
}

impl Date {
    /// Parses `YYYY-MM-DD`; `None` unless it is a real calendar date.
    pub fn parse(text: &str) -> Option<Date> {
        let bytes = text.as_bytes();
        if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
            return None;
        }
        let number = |digits: &[u8]| -> Option<u16> {
            digits.iter().try_fold(0, |value, digit| {
                digit
                    .is_ascii_digit()
                    .then(|| value * 10 + u16::from(digit - b'0'))
            })
        };
        let year = number(&bytes[..4])?;
        let month = number(&bytes[5..7])? as u8;
        let day = number(&bytes[8..])? as u8;
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let days_in_month = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if leap => 29,
            2 => 28,
            _ => return None,
        };
        (1..=days_in_month)
            .contains(&day)
            .then_some(Date { year, month, day })
    }

    /// Days since 1970-01-01, the representation used for date columns.
    pub fn to_days(self) -> i32 {
        let (year, month, day) = (
            i64::from(self.year),
            i64::from(self.month),
            i64::from(self.day),
        );
        // Count years from March so that the leap day is the last day of the year.
        let year = if month <= 2 { year - 1 } else { year };
        let era = year.div_euclid(400);
        let year_of_era = year.rem_euclid(400);
        let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
        let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
        (era * 146_097 + day_of_era - 719_468) as i32
    }

    /// The inverse of `to_days`; `None` if the year does not fit in four digits.
    pub fn from_days(days: i32) -> Option<Date> {
        let shifted = i64::from(days) + 719_468;
        let era = shifted.div_euclid(146_097);
        let day_of_era = shifted.rem_euclid(146_097);
        let year_of_era =
            (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let shifted_month = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
        let month = if shifted_month < 10 {
            shifted_month + 3
        } else {
            shifted_month - 9
        };
        let year = year_of_era + era * 400 + i64::from(month <= 2);
        let year = u16::try_from(year).ok().filter(|year| *year <= 9999)?;
        Some(Date {
            year,
            month: month as u8,
            day: day as u8,
        })
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    /// `a ?? b`: `a` unless it is null, then `b`.
    Coalesce,
    /// `a in list`: whether a list has a value equal to `a`.
    In,
    /// `a not in list`
    NotIn,
}

impl BinaryOp {
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::Rem => "%",
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::And => "and",
            Self::Or => "or",
            Self::Coalesce => "??",
            Self::In => "in",
            Self::NotIn => "not in",
        }
    }

    pub fn is_comparison(self) -> bool {
        matches!(
            self,
            Self::Eq
                | Self::Ne
                | Self::Lt
                | Self::Le
                | Self::Gt
                | Self::Ge
                | Self::In
                | Self::NotIn
        )
    }
}

#[derive(Debug)]
pub struct TypeExpr {
    pub kind: TypeKind,
    pub span: Span,
}

#[derive(Debug)]
pub enum TypeKind {
    /// `int`, `Sale`, `list<int>`, `table<Sale>`
    Named { name: Symbol, args: Vec<TypeExpr> },
    /// `{ region: string, qty: int }`
    Record(Vec<FieldType>),
    /// `T?`
    Nullable(Box<TypeExpr>),
    /// `fn(int, int) -> int`; without `->` the function returns nothing.
    Fn {
        params: Vec<TypeExpr>,
        ret: Option<Box<TypeExpr>>,
    },
}

#[derive(Debug)]
pub struct FieldType {
    pub name: Ident,
    pub ty: TypeExpr,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_convert_to_days_and_back() {
        let date = |year, month, day| Date { year, month, day };
        assert_eq!(date(1970, 1, 1).to_days(), 0);
        assert_eq!(date(1969, 12, 31).to_days(), -1);
        assert_eq!(date(2000, 3, 1).to_days(), 11_017);
        assert_eq!(date(2026, 10, 8).to_days(), 20_734);
        // Every day from 0000-01-01 through 9999-12-31 round-trips and follows the last one.
        let (first, last) = (date(0, 1, 1).to_days(), date(9999, 12, 31).to_days());
        let mut previous = None;
        for days in first..=last {
            let converted = Date::from_days(days).unwrap();
            assert_eq!(converted.to_days(), days);
            assert!(previous < Some(converted));
            previous = Some(converted);
        }
        assert_eq!(Date::from_days(last + 1), None);
        assert_eq!(Date::from_days(first - 1), None);
    }
}
