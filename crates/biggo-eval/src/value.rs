use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;
use std::sync::Arc;

use biggo_plan::{Plan, Scalar, scalar};
use biggo_syntax::ast::Date;

/// A runtime value. Cloning is cheap: everything bigger than a number is reference-counted.
#[derive(Clone, Debug)]
pub enum Value {
    /// What an expression with no result produces, such as a call to `print`.
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
    /// A length of time in microseconds.
    Duration(i64),
    Decimal(Decimal),
    List(Rc<[Value]>),
    Record(Rc<Record>),
    Map(Rc<Map>),
    Fn(Rc<Closure>),
    /// A table is a query that has not run yet; printing or writing it runs it.
    Table(Arc<Plan>),
}

/// An exact number: its value times 10 to the power `scalar::DECIMAL_SCALE`. It is kept as
/// bytes so that it does not make every `Value` as wide and as strictly aligned as an `i128`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decimal([u8; 16]);

impl Decimal {
    pub fn new(scaled: i128) -> Self {
        Self(scaled.to_ne_bytes())
    }

    pub fn scaled(self) -> i128 {
        i128::from_ne_bytes(self.0)
    }
}

/// A value with named fields. The names are shared by every record of one type.
#[derive(Debug)]
pub struct Record {
    pub names: Arc<[Arc<str>]>,
    pub values: Vec<Value>,
}

/// What a map can be keyed by.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Str(Arc<str>),
    Int(i64),
    Bool(bool),
    Date(i32),
}

impl Key {
    pub fn from_value(value: &Value) -> Option<Key> {
        Some(match value {
            Value::Str(text) => Key::Str(text.clone()),
            Value::Int(number) => Key::Int(*number),
            Value::Bool(flag) => Key::Bool(*flag),
            Value::Date(days) => Key::Date(*days),
            _ => return None,
        })
    }

    pub fn to_value(&self) -> Value {
        match self {
            Key::Str(text) => Value::Str(text.clone()),
            Key::Int(number) => Value::Int(*number),
            Key::Bool(flag) => Value::Bool(*flag),
            Key::Date(days) => Value::Date(*days),
        }
    }
}

/// A map that remembers the order its keys were first put in.
#[derive(Clone, Debug, Default)]
pub struct Map {
    entries: Vec<(Key, Value)>,
    index: HashMap<Key, usize>,
}

impl Map {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, key: &Key) -> Option<&Value> {
        self.index
            .get(key)
            .map(|&position| &self.entries[position].1)
    }

    /// Sets the value of `key`, which keeps its place if the map already has it.
    pub fn insert(&mut self, key: Key, value: Value) {
        match self.index.get(&key) {
            Some(&position) => self.entries[position].1 = value,
            None => {
                self.index.insert(key.clone(), self.entries.len());
                self.entries.push((key, value));
            }
        }
    }

    pub fn entries(&self) -> &[(Key, Value)] {
        &self.entries
    }
}

/// A function together with the values it captured where it was declared.
#[derive(Debug)]
pub struct Closure {
    pub(crate) function: u32,
    pub(crate) name: Arc<str>,
    pub(crate) captures: Vec<Value>,
}

impl Value {
    /// Formats the value as it would be written in source code, where it has a literal form:
    /// strings are quoted and dates start with `@`.
    pub fn repr(&self) -> Repr<'_> {
        Repr(self)
    }

    pub(crate) fn from_scalar(scalar: Scalar) -> Value {
        match scalar {
            Scalar::Null => Value::Null,
            Scalar::Bool(value) => Value::Bool(value),
            Scalar::Int(value) => Value::Int(value),
            Scalar::Float(value) => Value::Float(value),
            Scalar::Str(value) => Value::Str(value),
            Scalar::Date(value) => Value::Date(value),
            Scalar::DateTime(value) => Value::DateTime(value),
            Scalar::Duration(value) => Value::Duration(value),
            Scalar::Decimal(value) => Value::Decimal(Decimal::new(value)),
            Scalar::List(items) => {
                Value::List(items.iter().cloned().map(Value::from_scalar).collect())
            }
        }
    }

    pub(crate) fn to_scalar(&self) -> Option<Scalar> {
        Some(match self {
            Value::Null => Scalar::Null,
            Value::Bool(value) => Scalar::Bool(*value),
            Value::Int(value) => Scalar::Int(*value),
            Value::Float(value) => Scalar::Float(*value),
            Value::Str(value) => Scalar::Str(value.clone()),
            Value::Date(value) => Scalar::Date(*value),
            Value::DateTime(value) => Scalar::DateTime(*value),
            Value::Duration(value) => Scalar::Duration(*value),
            Value::Decimal(value) => Scalar::Decimal(value.scaled()),
            Value::List(items) => {
                let items: Option<Vec<Scalar>> = items.iter().map(Value::to_scalar).collect();
                Scalar::List(items?.into())
            }
            _ => return None,
        })
    }

    /// Whether two values of one type are the same. Unlike `==` in a program, this holds for
    /// two nulls, and for two floats that are both not a number.
    pub fn same(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Unit, Value::Unit) | (Value::Null, Value::Null) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b || (a.is_nan() && b.is_nan()),
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Date(a), Value::Date(b)) => a == b,
            (Value::DateTime(a), Value::DateTime(b)) => a == b,
            (Value::Duration(a), Value::Duration(b)) => a == b,
            (Value::Decimal(a), Value::Decimal(b)) => a == b,
            (Value::List(a), Value::List(b)) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| x.same(y))
            }
            (Value::Record(a), Value::Record(b)) => {
                a.values.len() == b.values.len()
                    && a.values.iter().zip(&b.values).all(|(x, y)| x.same(y))
            }
            (Value::Map(a), Value::Map(b)) => {
                let has = |(key, value): &(Key, Value)| b.get(key).is_some_and(|v| v.same(value));
                a.len() == b.len() && a.entries().iter().all(has)
            }
            (Value::Fn(a), Value::Fn(b)) => Rc::ptr_eq(a, b),
            (Value::Table(a), Value::Table(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

fn write_date(f: &mut fmt::Formatter<'_>, days: i32) -> fmt::Result {
    match Date::from_days(days) {
        Some(date) => write!(f, "{date}"),
        None => write!(f, "<day {days}>"),
    }
}

/// Formats the value for output: like `repr`, except that a value on its own is printed
/// without the marks that set its literal apart: a string bare, a date without its `@`.
impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Str(text) => f.write_str(text),
            Value::Date(days) => write_date(f, *days),
            Value::DateTime(micros) => f.write_str(&scalar::format_datetime(*micros)),
            Value::Decimal(value) => f.write_str(&scalar::format_decimal(value.scaled())),
            other => other.repr().fmt(f),
        }
    }
}

pub struct Repr<'a>(&'a Value);

impl fmt::Display for Repr<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Value::Unit => f.write_str("()"),
            Value::Null => f.write_str("null"),
            Value::Bool(value) => write!(f, "{value}"),
            Value::Int(value) => write!(f, "{value}"),
            // `{:?}` keeps the `.0` on whole numbers, so floats stay distinct from ints.
            Value::Float(value) => write!(f, "{value:?}"),
            Value::Str(text) => write_quoted(f, text),
            Value::Date(days) => {
                f.write_str("@")?;
                write_date(f, *days)
            }
            Value::DateTime(micros) => write!(f, "@{}", scalar::format_datetime(*micros)),
            Value::Duration(micros) => f.write_str(&scalar::format_duration(*micros)),
            Value::Decimal(value) => write!(f, "{}d", scalar::format_decimal(value.scaled())),
            Value::List(items) => {
                f.write_str("[")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    item.repr().fmt(f)?;
                }
                f.write_str("]")
            }
            Value::Record(record) => {
                f.write_str("{")?;
                for (i, (name, value)) in record.names.iter().zip(&record.values).enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{}: {}", biggo_plan::Name(name), value.repr())?;
                }
                f.write_str("}")
            }
            Value::Map(map) => {
                f.write_str("{")?;
                for (i, (key, value)) in map.entries().iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{}: {}", key.to_value().repr(), value.repr())?;
                }
                f.write_str("}")
            }
            Value::Fn(closure) => write!(f, "<fn {}>", closure.name),
            Value::Table(plan) => write!(f, "<table {}>", plan.schema()),
        }
    }
}

/// Writes `text` as a string literal, escaping only what the lexer requires. Unlike `{:?}`, this
/// leaves combining marks alone, so Thai text stays readable.
fn write_quoted(f: &mut fmt::Formatter<'_>, text: &str) -> fmt::Result {
    f.write_str("\"")?;
    for c in text.chars() {
        match c {
            '"' => f.write_str("\\\"")?,
            '\\' => f.write_str("\\\\")?,
            '\n' => f.write_str("\\n")?,
            '\t' => f.write_str("\\t")?,
            '\r' => f.write_str("\\r")?,
            '\0' => f.write_str("\\0")?,
            _ => fmt::Write::write_char(f, c)?,
        }
    }
    f.write_str("\"")
}
