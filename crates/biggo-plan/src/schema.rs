use std::fmt;
use std::sync::Arc;

use biggo_syntax::ast::Date;
use biggo_syntax::scalar;

/// The type of the values stored in a column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DataType {
    Int,
    Float,
    Bool,
    Str,
    Date,
    /// A date and a time of day, without a time zone.
    DateTime,
    /// A length of time.
    Duration,
    /// An exact number with six digits after the point.
    Decimal,
}

impl DataType {
    pub fn name(self) -> &'static str {
        match self {
            DataType::Int => "int",
            DataType::Float => "float",
            DataType::Bool => "bool",
            DataType::Str => "string",
            DataType::Date => "date",
            DataType::DateTime => "datetime",
            DataType::Duration => "duration",
            DataType::Decimal => "decimal",
        }
    }

    /// The name with its article, for messages: "an int", "a date".
    pub fn with_article(self) -> &'static str {
        match self {
            DataType::Int => "an int",
            DataType::Float => "a float",
            DataType::Bool => "a bool",
            DataType::Str => "a string",
            DataType::Date => "a date",
            DataType::DateTime => "a datetime",
            DataType::Duration => "a duration",
            DataType::Decimal => "a decimal",
        }
    }
}

/// The type of a column or of a column expression.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ColType {
    pub dtype: DataType,
    pub nullable: bool,
}

impl ColType {
    pub const fn new(dtype: DataType, nullable: bool) -> Self {
        Self { dtype, nullable }
    }

    pub const fn required(dtype: DataType) -> Self {
        Self::new(dtype, false)
    }

    pub const fn nullable(dtype: DataType) -> Self {
        Self::new(dtype, true)
    }
}

impl fmt::Display for ColType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.dtype.name())?;
        if self.nullable {
            f.write_str("?")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub name: Arc<str>,
    pub ty: ColType,
}

impl Field {
    pub fn new(name: impl Into<Arc<str>>, ty: ColType) -> Self {
        Self {
            name: name.into(),
            ty,
        }
    }
}

/// The columns of a table, in order. Column names are unique.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Schema {
    pub fields: Vec<Field>,
}

impl Schema {
    pub fn new(fields: Vec<Field>) -> Self {
        Self { fields }
    }

    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.fields.iter().position(|field| &*field.name == name)
    }

    pub fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|field| &*field.name == name)
    }

    pub fn names(&self) -> impl Iterator<Item = &Arc<str>> {
        self.fields.iter().map(|field| &field.name)
    }
}

impl fmt::Display for Schema {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("{")?;
        for (i, field) in self.fields.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{}: {}", Name(&field.name), field.ty)?;
        }
        f.write_str("}")
    }
}

/// Formats a column name as it is written in source: in backticks unless it is a plain word.
pub struct Name<'a>(pub &'a str);

impl fmt::Display for Name<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut chars = self.0.chars();
        let plain = chars.next().is_some_and(|c| c == '_' || c.is_alphabetic())
            && chars.all(|c| c == '_' || c.is_alphanumeric() || !c.is_ascii());
        let keyword = matches!(
            self.0,
            "let"
                | "match"
                | "import"
                | "type"
                | "fn"
                | "if"
                | "else"
                | "and"
                | "or"
                | "not"
                | "true"
                | "false"
                | "null"
        );
        if plain && !keyword {
            f.write_str(self.0)
        } else {
            write!(f, "`{}`", self.0)
        }
    }
}

/// A single value of a column type.
#[derive(Clone, Debug, PartialEq)]
pub enum Scalar {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Arc<str>),
    /// Days since 1970-01-01.
    Date(i32),
    /// Microseconds since 1970-01-01T00:00:00.
    DateTime(i64),
    /// Microseconds.
    Duration(i64),
    /// The value times 10 to the power `scalar::DECIMAL_SCALE`.
    Decimal(i128),
    /// The values that `in` looks through. No column holds one.
    List(Arc<[Scalar]>),
}

impl fmt::Display for Scalar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Scalar::Null => f.write_str("null"),
            Scalar::Bool(value) => write!(f, "{value}"),
            Scalar::Int(value) => write!(f, "{value}"),
            Scalar::Float(value) => write!(f, "{value:?}"),
            Scalar::Str(value) => write!(f, "{value:?}"),
            Scalar::Date(days) => match Date::from_days(*days) {
                Some(date) => write!(f, "{date}"),
                None => write!(f, "date({days})"),
            },
            Scalar::DateTime(micros) => f.write_str(&scalar::format_datetime(*micros)),
            Scalar::Duration(micros) => f.write_str(&scalar::format_duration(*micros)),
            Scalar::Decimal(value) => f.write_str(&scalar::format_decimal(*value)),
            Scalar::List(items) => {
                f.write_str("[")?;
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{item}")?;
                }
                f.write_str("]")
            }
        }
    }
}

/// The columns of the table that `describe` returns.
pub fn describe_schema() -> Schema {
    let float = ColType::nullable(DataType::Float);
    Schema::new(vec![
        Field::new("column", ColType::required(DataType::Str)),
        Field::new("type", ColType::required(DataType::Str)),
        Field::new("count", ColType::required(DataType::Int)),
        Field::new("nulls", ColType::required(DataType::Int)),
        Field::new("mean", float),
        Field::new("stddev", float),
        Field::new("min", float),
        Field::new("median", float),
        Field::new("max", float),
    ])
}

/// The columns of the table that `histogram` returns.
pub fn histogram_schema() -> Schema {
    Schema::new(vec![
        Field::new("bin_start", ColType::required(DataType::Float)),
        Field::new("bin_end", ColType::required(DataType::Float)),
        Field::new("count", ColType::required(DataType::Int)),
    ])
}
