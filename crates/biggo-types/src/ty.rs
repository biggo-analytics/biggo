use std::fmt;
use std::sync::Arc;

use biggo_plan::{ColType, DataType, Field, Schema};
use biggo_syntax::Symbol;

#[derive(Clone, Debug, PartialEq)]
pub enum Type {
    /// The type of an expression with no value, such as a call to `print`.
    Unit,
    /// The type of `null` before it is known which type's null it is.
    Null,
    Int,
    Float,
    Bool,
    Str,
    Date,
    DateTime,
    Duration,
    Decimal,
    /// `T?`. The inner type is never `Nullable`, `Null`, or `Unit`.
    Nullable(Box<Type>),
    List(Box<Type>),
    /// `map<K, V>`
    Map(Box<Type>, Box<Type>),
    /// `{a: int, b: string}`: a value with named fields, and the type of a table's rows.
    Record(Arc<Vec<(Arc<str>, Type)>>),
    Table(Arc<Schema>),
    /// The result of `group`, which only `agg` accepts.
    Grouped(Arc<Grouped>),
    Fn(Arc<FnType>),
    /// The type of an expression that gives no value because the program does not go on
    /// after it, such as a call to `fail`. It stands in for a value of any type.
    Never,
    /// The type of an expression that already has an error. It is accepted everywhere, so that
    /// one mistake is reported once.
    Error,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Grouped {
    pub input: Arc<Schema>,
    pub keys: Vec<Field>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FnType {
    pub params: Vec<Param>,
    pub ret: Type,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: Symbol,
    pub text: Arc<str>,
    pub ty: Type,
}

/// Two function types are the same if they take and return the same types, whatever their
/// parameters are called.
impl PartialEq for Param {
    fn eq(&self, other: &Self) -> bool {
        self.ty == other.ty
    }
}

impl Type {
    pub fn is_error(&self) -> bool {
        matches!(self, Type::Error)
    }

    /// `T?` for this type; types that already admit null stay as they are.
    pub fn or_null(self) -> Type {
        match self {
            Type::Nullable(_) | Type::Null | Type::Error | Type::Unit | Type::Never => self,
            other => Type::Nullable(Box::new(other)),
        }
    }

    /// The type without its `?`, and whether it had one. `null` counts as nullable.
    pub fn split_null(&self) -> (&Type, bool) {
        match self {
            Type::Nullable(inner) => (inner, true),
            Type::Null => (self, true),
            other => (other, false),
        }
    }

    pub fn with_null(self, nullable: bool) -> Type {
        if nullable { self.or_null() } else { self }
    }

    pub fn from_dtype(dtype: DataType) -> Type {
        match dtype {
            DataType::Int => Type::Int,
            DataType::Float => Type::Float,
            DataType::Bool => Type::Bool,
            DataType::Str => Type::Str,
            DataType::Date => Type::Date,
            DataType::DateTime => Type::DateTime,
            DataType::Duration => Type::Duration,
            DataType::Decimal => Type::Decimal,
        }
    }

    pub fn from_col(ty: ColType) -> Type {
        Type::from_dtype(ty.dtype).with_null(ty.nullable)
    }

    /// The column type that holds values of this type, if it is one a column can hold.
    pub fn to_col(&self) -> Option<ColType> {
        let (base, nullable) = self.split_null();
        let dtype = match base {
            Type::Int => DataType::Int,
            Type::Float => DataType::Float,
            Type::Bool => DataType::Bool,
            Type::Str => DataType::Str,
            Type::Date => DataType::Date,
            Type::DateTime => DataType::DateTime,
            Type::Duration => DataType::Duration,
            Type::Decimal => DataType::Decimal,
            _ => return None,
        };
        Some(ColType::new(dtype, nullable))
    }

    /// The type of the rows of a table with these columns.
    pub fn row_of(schema: &Schema) -> Type {
        let fields = schema.fields.iter();
        let fields = fields.map(|field| (field.name.clone(), Type::from_col(field.ty)));
        Type::Record(Arc::new(fields.collect()))
    }

    /// The columns of a table whose rows have this record type. Fails with the first field
    /// whose type a column cannot hold.
    pub fn columns(&self) -> Option<Result<Schema, (Arc<str>, Type)>> {
        let Type::Record(fields) = self else {
            return None;
        };
        let mut schema = Schema::default();
        for (name, ty) in fields.iter() {
            match ty.to_col() {
                Some(col) => schema.fields.push(Field::new(name.clone(), col)),
                None => return Some(Err((name.clone(), ty.clone()))),
            }
        }
        Some(Ok(schema))
    }
}

/// Formats the type as it is written in source, where it can be.
impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Unit => f.write_str("unit"),
            Type::Never => f.write_str("never"),
            Type::Null => f.write_str("null"),
            Type::Int => f.write_str("int"),
            Type::Float => f.write_str("float"),
            Type::Bool => f.write_str("bool"),
            Type::Str => f.write_str("string"),
            Type::Date => f.write_str("date"),
            Type::DateTime => f.write_str("datetime"),
            Type::Duration => f.write_str("duration"),
            Type::Decimal => f.write_str("decimal"),
            Type::Nullable(inner) => write!(f, "{inner}?"),
            Type::List(element) => write!(f, "list<{element}>"),
            Type::Map(key, value) => write!(f, "map<{key}, {value}>"),
            Type::Record(fields) => {
                f.write_str("{")?;
                for (i, (name, ty)) in fields.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{}: {ty}", biggo_plan::Name(name))?;
                }
                f.write_str("}")
            }
            Type::Table(schema) => write!(f, "table<{schema}>"),
            Type::Grouped(grouped) => {
                f.write_str("grouped table (by ")?;
                for (i, key) in grouped.keys.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{}", biggo_plan::Name(&key.name))?;
                }
                f.write_str(")")
            }
            Type::Fn(function) => {
                f.write_str("fn(")?;
                for (i, param) in function.params.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    match param.text.is_empty() {
                        true => write!(f, "{}", param.ty)?,
                        false => write!(f, "{}: {}", param.text, param.ty)?,
                    }
                }
                write!(f, ") -> {}", function.ret)
            }
            Type::Error => f.write_str("?"),
        }
    }
}
