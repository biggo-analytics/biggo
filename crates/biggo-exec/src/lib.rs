//! Runs query plans on Arrow record batches: column vectors, vectorized expressions, and the
//! operators of the plan.

mod aggregate;
mod convert;
mod expr;
mod join;
mod ops;
mod scan;
mod window;

use std::fmt;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{AsArray, Float64Array, RecordBatch, RecordBatchOptions};
use arrow::datatypes::{
    DataType as ArrowType, DurationMicrosecondType, Field as ArrowField, Float64Type,
    Schema as ArrowSchema,
};
use arrow::error::ArrowError;
use arrow::util::display::FormatOptions;
use arrow::util::pretty::pretty_format_batches_with_options;
use biggo_plan::{
    AggCall, AggFn, ColType, DataType, Expr, ExprKind, Field, Memory, Plan, Scalar, ScalarFn,
    Schema, describe_schema, histogram_schema,
};
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::errors::ParquetError;
use parquet::file::properties::WriterProperties;

/// An error that stops a query.
#[derive(Clone, Debug, PartialEq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<ArrowError> for Error {
    fn from(err: ArrowError) -> Self {
        match err {
            ArrowError::DivideByZero => Error("division by zero".into()),
            ArrowError::ArithmeticOverflow(_) => Error("integer overflow".into()),
            ArrowError::ComputeError(message) => Error(message),
            other => Error(other.to_string()),
        }
    }
}

impl From<ParquetError> for Error {
    fn from(err: ParquetError) -> Self {
        Error(err.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// The output of an operator, a batch at a time.
pub type BatchIter = Box<dyn Iterator<Item = Result<RecordBatch>> + Send>;

/// Starts running `plan` as it is. Rows are produced as they are pulled, so a consumer that
/// stops early saves the work for the rest; sorts, joins, and aggregations do their work
/// before the first row.
pub fn execute(plan: &Plan) -> Result<BatchIter> {
    Ok(match plan {
        Plan::Scan(scan) => scan::scan(scan)?,
        Plan::Memory(memory) => {
            let batches = memory.data.downcast_ref::<Vec<RecordBatch>>();
            let batches =
                batches.ok_or_else(|| Error("internal error: foreign table data".into()))?;
            Box::new(batches.clone().into_iter().map(Ok))
        }
        Plan::Filter { input, predicate } => ops::filter(execute(input)?, predicate.clone()),
        Plan::Project {
            input,
            columns,
            schema,
        } => ops::project(execute(input)?, columns.clone(), schema.clone()),
        Plan::Sort { input, keys, fetch } => {
            ops::sort(execute(input)?, keys, *fetch, &input.schema())?
        }
        Plan::Limit { input, skip, fetch } => ops::limit(execute(input)?, *skip, *fetch),
        Plan::Group { .. } => {
            return Err(Error(
                "a grouped table has no rows of its own; call `agg` on it".into(),
            ));
        }
        Plan::Aggregate {
            input,
            keys,
            aggs,
            schema,
        } => aggregate::aggregate(execute(input)?, keys, aggs, schema)?,
        Plan::Join(join) => join::join(execute(&join.left)?, execute(&join.right)?, join)?,
        Plan::Window {
            input,
            partition,
            order,
            funcs,
            schema,
        } => {
            let rows = execute(input)?;
            window::window(rows, partition, order, funcs, &input.schema(), schema)?
        }
        Plan::Union { inputs } => {
            let mut parts = Vec::with_capacity(inputs.len());
            for input in inputs {
                parts.push(execute(input)?);
            }
            Box::new(parts.into_iter().flatten())
        }
        Plan::Unpivot {
            input,
            columns,
            schema,
            ..
        } => ops::unpivot(execute(input)?, columns.clone(), schema.clone()),
        Plan::Explode {
            input,
            column,
            separator,
        } => ops::explode(execute(input)?, column.clone(), separator.clone()),
    })
}

/// Rewrites `plan` into a cheaper equivalent.
pub fn optimize(plan: &Arc<Plan>) -> Arc<Plan> {
    biggo_plan::optimize(plan, &fold)
}

/// Optimizes and runs `plan`, and gathers its rows.
pub fn collect(plan: &Arc<Plan>) -> Result<Vec<RecordBatch>> {
    execute(&optimize(plan))?.collect()
}

fn memory(schema: Arc<Schema>, batches: Vec<RecordBatch>) -> Plan {
    Plan::Memory(Memory {
        schema,
        rows: batches.iter().map(RecordBatch::num_rows).sum(),
        data: Arc::new(batches),
    })
}

/// Runs `plan` and wraps its rows as a table held in memory.
pub fn materialize(plan: &Arc<Plan>) -> Result<Plan> {
    Ok(memory(plan.schema(), collect(plan)?))
}

pub fn count(plan: &Arc<Plan>) -> Result<usize> {
    let batches = execute(&optimize(plan))?;
    batches.map(|batch| Ok(batch?.num_rows())).sum()
}

/// Runs `plan` and returns its rows as lists of values, in column order.
pub fn to_rows(plan: &Arc<Plan>) -> Result<Vec<Vec<Scalar>>> {
    let mut rows = Vec::new();
    for batch in collect(plan)? {
        for row in 0..batch.num_rows() {
            let values = batch
                .columns()
                .iter()
                .map(|column| convert::scalar_at(column, row));
            rows.push(values.collect::<Result<Vec<_>>>()?);
        }
    }
    Ok(rows)
}

/// Builds a table in memory from rows of values, each in the column order of `schema`.
pub fn from_rows(schema: &Arc<Schema>, rows: &[Vec<Scalar>]) -> Result<Plan> {
    let mut columns = Vec::with_capacity(schema.fields.len());
    for (index, field) in schema.fields.iter().enumerate() {
        let values = rows.iter().map(|row| row[index].clone());
        columns.push(convert::scalars_to_array(values, field.ty.dtype)?);
    }
    let batch = convert::batch(schema, columns, rows.len())?;
    Ok(memory(schema.clone(), vec![batch]))
}

/// Replaces the columns whose values Arrow would print differently from the rest of the
/// language, decimals, datetimes, and durations, by their text.
fn printable(batch: &RecordBatch) -> Result<RecordBatch> {
    let schema = batch.schema();
    let mut fields = Vec::with_capacity(batch.num_columns());
    let mut columns = Vec::with_capacity(batch.num_columns());
    for (field, column) in schema.fields().iter().zip(batch.columns()) {
        match field.data_type() {
            ArrowType::Decimal128(..) | ArrowType::Duration(_) | ArrowType::Timestamp(..) => {
                fields.push(ArrowField::new(field.name(), ArrowType::Utf8, true));
                columns.push(expr::to_strings(column)?);
            }
            _ => {
                fields.push(field.as_ref().clone());
                columns.push(column.clone());
            }
        }
    }
    let options = RecordBatchOptions::new().with_row_count(Some(batch.num_rows()));
    let schema = Arc::new(ArrowSchema::new(fields));
    Ok(RecordBatch::try_new_with_options(
        schema, columns, &options,
    )?)
}

/// Runs `plan` and draws its first `max_rows` rows as a text table.
pub fn format_table(plan: &Arc<Plan>, max_rows: usize) -> Result<String> {
    let preview = Arc::new(Plan::Limit {
        input: plan.clone(),
        skip: 0,
        fetch: Some(max_rows.saturating_add(1)),
    });
    let mut batches = collect(&preview)?;
    let rows: usize = batches.iter().map(RecordBatch::num_rows).sum();
    if rows > max_rows {
        let all = arrow::compute::concat_batches(&batches[0].schema(), &batches)?;
        batches = vec![all.slice(0, max_rows)];
    }
    if batches.is_empty() {
        let schema = convert::arrow_schema(&plan.schema());
        batches.push(RecordBatch::new_empty(schema));
    }
    let batches = batches.iter().map(printable).collect::<Result<Vec<_>>>()?;
    let options = FormatOptions::default().with_null("null");
    let mut text = pretty_format_batches_with_options(&batches, &options)?.to_string();
    text.push('\n');
    if rows > max_rows {
        text.push_str(&format!("... only the first {max_rows} rows are shown\n"));
    }
    Ok(text)
}

fn create(path: &Path) -> Result<File> {
    File::create(path).map_err(|err| Error(format!("cannot write {}: {err}", path.display())))
}

/// Gives the columns of a batch the form they have in a text file, which is the form the
/// readers take back: a decimal as its digits, a duration as a number of seconds.
fn for_text_file(batch: &RecordBatch) -> Result<RecordBatch> {
    let schema = batch.schema();
    let mut fields = Vec::with_capacity(batch.num_columns());
    let mut columns = Vec::with_capacity(batch.num_columns());
    for (field, column) in schema.fields().iter().zip(batch.columns()) {
        match field.data_type() {
            ArrowType::Decimal128(..) => {
                fields.push(ArrowField::new(field.name(), ArrowType::Utf8, true));
                columns.push(expr::to_strings(column)?);
            }
            ArrowType::Duration(_) => {
                let micros = column.as_primitive::<DurationMicrosecondType>();
                let seconds: Float64Array = micros.unary(|micros| micros as f64 / 1e6);
                fields.push(ArrowField::new(field.name(), ArrowType::Float64, true));
                columns.push(Arc::new(seconds));
            }
            _ => {
                fields.push(field.as_ref().clone());
                columns.push(column.clone());
            }
        }
    }
    let options = RecordBatchOptions::new().with_row_count(Some(batch.num_rows()));
    let schema = Arc::new(ArrowSchema::new(fields));
    Ok(RecordBatch::try_new_with_options(
        schema, columns, &options,
    )?)
}

pub fn write_csv(plan: &Arc<Plan>, path: &Path) -> Result<()> {
    let batches = execute(&optimize(plan))?;
    let mut writer = arrow::csv::WriterBuilder::new()
        .with_header(true)
        .build(create(path)?);
    let mut wrote = false;
    for batch in batches {
        writer.write(&for_text_file(&batch?)?)?;
        wrote = true;
    }
    // The header is written with the first batch, so an empty result still needs one.
    if !wrote {
        let empty = RecordBatch::new_empty(convert::arrow_schema(&plan.schema()));
        writer.write(&for_text_file(&empty)?)?;
    }
    Ok(())
}

/// Writes one JSON object per row, a row per line.
pub fn write_json(plan: &Arc<Plan>, path: &Path) -> Result<()> {
    let mut writer = arrow::json::LineDelimitedWriter::new(create(path)?);
    for batch in execute(&optimize(plan))? {
        writer.write_batches(&[&for_text_file(&batch?)?])?;
    }
    writer.finish()?;
    Ok(())
}

pub fn write_parquet(plan: &Arc<Plan>, path: &Path) -> Result<()> {
    let schema = convert::arrow_schema(&plan.schema());
    let properties = WriterProperties::builder()
        .set_compression(Compression::SNAPPY)
        // Row groups are the units a reader can work on in parallel.
        .set_max_row_group_row_count(Some(128 << 10))
        .build();
    let mut writer = ArrowWriter::try_new(create(path)?, schema.clone(), Some(properties))?;
    for batch in execute(&optimize(plan))? {
        // The file declares which columns may hold nulls; batches are looser about it.
        let batch = batch?;
        writer.write(&RecordBatch::try_new(
            schema.clone(),
            batch.columns().to_vec(),
        )?)?;
    }
    writer.close()?;
    Ok(())
}

fn sql_name(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Writes the rows of `plan` as a table of a SQLite database, in place of the table of that
/// name if the database has one; the database is created if it does not exist. Dates,
/// datetimes, and decimals are stored as text, and durations as seconds.
pub fn write_sqlite(plan: &Arc<Plan>, path: &Path, table: &str) -> Result<()> {
    use rusqlite::types::Value;
    let fail = |err: rusqlite::Error| Error(format!("{}: {err}", path.display()));
    let schema = plan.schema();
    let columns = schema.fields.iter().map(|field| {
        let kind = match field.ty.dtype {
            DataType::Int | DataType::Bool => "INTEGER",
            // A duration is stored as its length in seconds.
            DataType::Float | DataType::Duration => "REAL",
            _ => "TEXT",
        };
        format!("{} {kind}", sql_name(&field.name))
    });
    let columns: Vec<String> = columns.collect();
    // The query may read the very table it replaces, so it runs to its end first.
    let batches = collect(plan)?;
    let mut connection = rusqlite::Connection::open(path).map_err(fail)?;
    let transaction = connection.transaction().map_err(fail)?;
    let drop = format!("DROP TABLE IF EXISTS {}", sql_name(table));
    transaction.execute(&drop, []).map_err(fail)?;
    let create = format!("CREATE TABLE {} ({})", sql_name(table), columns.join(", "));
    transaction.execute(&create, []).map_err(fail)?;
    {
        let slots = vec!["?"; schema.fields.len()].join(", ");
        let insert = format!("INSERT INTO {} VALUES ({slots})", sql_name(table));
        let mut insert = transaction.prepare(&insert).map_err(fail)?;
        for batch in batches {
            for row in 0..batch.num_rows() {
                let mut values = Vec::with_capacity(batch.num_columns());
                for column in batch.columns() {
                    values.push(match convert::scalar_at(column, row)? {
                        Scalar::Null => Value::Null,
                        Scalar::Bool(value) => Value::Integer(i64::from(value)),
                        Scalar::Int(value) => Value::Integer(value),
                        Scalar::Duration(micros) => Value::Real(micros as f64 / 1e6),
                        Scalar::Float(value) => Value::Real(value),
                        Scalar::Str(value) => Value::Text(value.to_string()),
                        other => Value::Text(other.to_string()),
                    });
                }
                insert
                    .execute(rusqlite::params_from_iter(values))
                    .map_err(fail)?;
            }
        }
    }
    transaction.commit().map_err(fail)
}

fn float_of(column: Expr) -> Expr {
    let nullable = ColType::nullable(DataType::Float);
    match column.ty.dtype {
        DataType::Float => column,
        _ => Expr::new(ExprKind::Cast(Box::new(column)), nullable),
    }
}

fn agg(func: AggFn, arg: Option<Expr>, ty: ColType) -> AggCall {
    AggCall {
        func,
        arg,
        arg2: None,
        ty,
    }
}

/// Runs `aggs` over all of `plan` as one group and returns the single row of results.
fn aggregate_all(plan: &Arc<Plan>, aggs: Vec<(Arc<str>, AggCall)>) -> Result<Vec<Scalar>> {
    let fields = aggs
        .iter()
        .map(|(name, call)| Field::new(name.clone(), call.ty));
    let schema = Arc::new(Schema::new(fields.collect()));
    let aggregate = Arc::new(Plan::Aggregate {
        input: plan.clone(),
        keys: Vec::new(),
        aggs,
        schema,
    });
    let mut rows = to_rows(&aggregate)?;
    rows.pop()
        .ok_or_else(|| Error("internal error: an aggregate without a row".into()))
}

/// Summarizes each column of `plan`: how many values and nulls it has and, for numbers, their
/// mean, spread, and range. One pass over the data computes all of it.
pub fn describe(plan: &Arc<Plan>) -> Result<Plan> {
    const STATS: [AggFn; 5] = [
        AggFn::Mean,
        AggFn::Stddev,
        AggFn::Min,
        AggFn::Median,
        AggFn::Max,
    ];
    let float = ColType::nullable(DataType::Float);
    let int = ColType::required(DataType::Int);
    let schema = plan.schema();
    let numeric = |field: &Field| {
        matches!(
            field.ty.dtype,
            DataType::Int | DataType::Float | DataType::Decimal
        )
    };
    let mut aggs: Vec<(Arc<str>, AggCall)> = vec![("rows".into(), agg(AggFn::Count, None, int))];
    for (index, field) in schema.fields.iter().enumerate() {
        let column = Expr::column(field.name.clone(), field.ty);
        let count = agg(AggFn::Count, Some(column.clone()), int);
        aggs.push((format!("count{index}").into(), count));
        if numeric(field) {
            for func in STATS {
                let name = format!("{}{index}", func.name());
                let call = agg(func, Some(float_of(column.clone())), float);
                aggs.push((name.into(), call));
            }
        }
    }
    let mut results = aggregate_all(plan, aggs)?.into_iter();
    let Some(Scalar::Int(total)) = results.next() else {
        return Err(Error("internal error: no row count".into()));
    };
    let mut rows = Vec::with_capacity(schema.fields.len());
    for field in &schema.fields {
        let Some(Scalar::Int(count)) = results.next() else {
            return Err(Error("internal error: no value count".into()));
        };
        let mut row = vec![
            Scalar::Str(field.name.clone()),
            Scalar::Str(field.ty.to_string().into()),
            Scalar::Int(count),
            Scalar::Int(total - count),
        ];
        match numeric(field) {
            true => row.extend(results.by_ref().take(STATS.len())),
            false => row.extend(std::iter::repeat_n(Scalar::Null, STATS.len())),
        }
        rows.push(row);
    }
    from_rows(&Arc::new(describe_schema()), &rows)
}

/// Counts the values of a numeric column in `bins` ranges of equal width between its
/// smallest and largest value. Nulls are left out.
pub fn histogram(plan: &Arc<Plan>, column: &str, bins: usize) -> Result<Plan> {
    let schema = plan.schema();
    let field = schema.field(column);
    let field = field.ok_or_else(|| Error(format!("internal error: no column `{column}`")))?;
    let value = float_of(Expr::column(field.name.clone(), field.ty));
    let float = ColType::nullable(DataType::Float);
    let range = aggregate_all(
        plan,
        vec![
            ("min".into(), agg(AggFn::Min, Some(value.clone()), float)),
            ("max".into(), agg(AggFn::Max, Some(value.clone()), float)),
        ],
    )?;
    let output = Arc::new(histogram_schema());
    let (Scalar::Float(min), Scalar::Float(max)) = (&range[0], &range[1]) else {
        return from_rows(&output, &[]);
    };
    let (min, max) = (*min, *max);
    // When every value is the same there is nothing to divide: one bin holds them all.
    let bins = if min == max { 1 } else { bins.max(1) };
    let width = (max - min) / bins as f64;

    let values = Arc::new(Plan::Project {
        input: plan.clone(),
        columns: vec![("value".into(), value.clone())],
        schema: Arc::new(Schema::new(vec![Field::new("value", value.ty)])),
    });
    let mut counts = vec![0i64; bins];
    for batch in execute(&optimize(&values))? {
        let batch = batch?;
        let floats = batch.column(0).as_primitive::<Float64Type>();
        for value in floats.iter().flatten() {
            // The largest value belongs to the last bin, not to one past it.
            let bin = match width > 0.0 {
                true => ((value - min) / width) as usize,
                false => 0,
            };
            counts[bin.min(bins - 1)] += 1;
        }
    }
    let rows = counts.into_iter().enumerate().map(|(bin, count)| {
        let start = min + width * bin as f64;
        let end = match bin + 1 == bins {
            true => max,
            false => min + width * (bin + 1) as f64,
        };
        vec![Scalar::Float(start), Scalar::Float(end), Scalar::Int(count)]
    });
    from_rows(&output, &rows.collect::<Vec<_>>())
}

/// A batch of one row and no columns, to evaluate expressions that read no column.
fn unit_batch() -> RecordBatch {
    let options = RecordBatchOptions::new().with_row_count(Some(1));
    RecordBatch::try_new_with_options(Arc::new(ArrowSchema::empty()), Vec::new(), &options)
        .expect("a batch without columns is always valid")
}

/// The value of an expression that reads no column, or `None` if evaluating it fails.
pub fn fold(expr: &Expr) -> Option<Scalar> {
    match expr::eval(expr, &unit_batch()).ok()? {
        expr::Col::Scalar(value) => convert::scalar_at(&value, 0).ok(),
        expr::Col::Array(_) => None,
    }
}

/// Applies a scalar function to single values. `args` gives each argument with its type.
pub fn call_scalar(func: ScalarFn, args: &[(Scalar, ColType)], ty: ColType) -> Result<Scalar> {
    let args = args
        .iter()
        .map(|(value, ty)| Expr::literal(value.clone(), *ty));
    let call = Expr::new(ExprKind::Call(func, args.collect()), ty);
    let value = expr::eval(&call, &unit_batch())?.into_array(1)?;
    convert::scalar_at(&value, 0)
}
