//! Table schemas, column expressions, logical plans, and the plan optimizer.

mod csv;
pub mod dates;
mod expr;
mod optimize;
mod plan;
mod schema;
pub mod text;

pub use biggo_syntax::ast::{BinaryOp, Date};
pub use biggo_syntax::scalar;
pub use csv::CsvOptions;
pub use expr::{AggCall, AggFn, Expr, ExprKind, ScalarFn, SortKey, WindowCall, WindowFn};
pub use optimize::{Fold, optimize};
pub use plan::{Format, Join, JoinColumn, JoinKind, Memory, Plan, Scan, TableOp, schema_of};
pub use schema::{
    ColType, DataType, Field, Name, Scalar, Schema, describe_schema, histogram_schema,
};
