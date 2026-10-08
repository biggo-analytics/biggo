//! Reads CSV and Parquet files. A file is split into pieces that are decoded on all cores, a
//! few at a time, so that a reader who wants only the first rows does not pay for the rest.

use std::collections::VecDeque;
use std::fs::File;
use std::io::Cursor;
use std::ops::Range;
use std::sync::Arc;

use arrow::array::{Array, ArrayRef, AsArray, DurationMicrosecondArray, RecordBatch, StringArray};
use arrow::compute::{CastOptions, cast_with_options, filter_record_batch};
use arrow::csv::ReaderBuilder;
use arrow::datatypes::{
    DataType as ArrowType, Field as ArrowField, Float64Type, Schema as ArrowSchema, SchemaRef,
};
use biggo_plan::Date;
use biggo_plan::{DataType, Expr, Field, Format, Scalar, Scan, Schema, scalar};
use memmap2::Mmap;
use parquet::arrow::ProjectionMask;
use parquet::arrow::arrow_reader::{
    ArrowReaderMetadata, ArrowReaderOptions, ParquetRecordBatchReaderBuilder,
};
use rayon::prelude::*;

use crate::convert::{arrow_type, batch as make_batch, scalars_to_array};
use crate::expr::eval_array;
use crate::{BatchIter, Error, Result};

/// Rows per batch. Large enough that per-batch overhead is small, small enough that a batch
/// of a few columns stays in the cache.
pub const BATCH_ROWS: usize = 32_768;

/// The size of the pieces a CSV file is cut into. It does not depend on the number of cores,
/// so that a query sees the same batches, and gives the same result, on every machine.
const PIECE_BYTES: usize = 2 << 20;

pub fn scan(scan: &Scan) -> Result<BatchIter> {
    let shape = Arc::new(Shape::new(scan));
    let pieces: Vec<Piece> = match scan.format {
        Format::Csv => csv_pieces(scan, &shape)?,
        Format::Parquet => parquet_pieces(scan, &shape)?,
        Format::Json => json_pieces(scan, &shape)?,
        Format::Sqlite => sqlite_pieces(scan, &shape)?,
    };
    Ok(Box::new(ScanIter {
        pieces: pieces.into_iter(),
        ready: VecDeque::new(),
        remaining: scan.limit,
        // With a limit, the first piece often has every row that is wanted.
        group: if scan.limit.is_some() {
            1
        } else {
            rayon::current_num_threads()
        },
    }))
}

/// A part of a file that can be decoded independently of the others.
type Piece = Box<dyn FnOnce() -> Result<Vec<RecordBatch>> + Send>;

struct ScanIter {
    pieces: std::vec::IntoIter<Piece>,
    ready: VecDeque<RecordBatch>,
    remaining: Option<usize>,
    group: usize,
}

impl Iterator for ScanIter {
    type Item = Result<RecordBatch>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.remaining == Some(0) {
                return None;
            }
            if let Some(mut batch) = self.ready.pop_front() {
                if let Some(remaining) = &mut self.remaining {
                    batch = batch.slice(0, batch.num_rows().min(*remaining));
                    *remaining -= batch.num_rows();
                }
                return Some(Ok(batch));
            }
            let group: Vec<Piece> = self.pieces.by_ref().take(self.group).collect();
            if group.is_empty() {
                return None;
            }
            self.group = rayon::current_num_threads();
            let decoded: Result<Vec<Vec<RecordBatch>>> =
                group.into_par_iter().map(|piece| piece()).collect();
            match decoded {
                Ok(batches) => {
                    let batches = batches.into_iter().flatten();
                    self.ready
                        .extend(batches.filter(|batch| batch.num_rows() > 0));
                }
                Err(err) => {
                    self.pieces = Vec::new().into_iter();
                    return Some(Err(err));
                }
            }
        }
    }
}

/// What a scan makes of the columns it decodes: it checks them against their declared
/// types, drops the rows that fail the scan's filters, and keeps the columns asked for.
struct Shape {
    file: String,
    format: Format,
    /// The declared columns that are read: those produced and those the filters need.
    read: Schema,
    output: Schema,
    filters: Vec<Expr>,
}

impl Shape {
    fn new(scan: &Scan) -> Self {
        let wanted = |name: &str| {
            scan.schema.field(name).is_some()
                || scan
                    .filters
                    .iter()
                    .any(|filter| filter.reads_any(|column| column == name))
        };
        let read = scan
            .declared
            .fields
            .iter()
            .filter(|field| wanted(&field.name));
        Self {
            file: scan.display_path.to_string(),
            format: scan.format,
            read: Schema::new(read.cloned().collect()),
            output: (*scan.schema).clone(),
            filters: scan.filters.clone(),
        }
    }

    /// Turns a decoded batch, whose columns are those of `read` in order, into output.
    fn finish(&self, decoded: &RecordBatch) -> Result<RecordBatch> {
        let rows = decoded.num_rows();
        let mut columns = Vec::with_capacity(self.read.fields.len());
        for (field, column) in self.read.fields.iter().zip(decoded.columns()) {
            let mut column = column.clone();
            let ty = arrow_type(field.ty.dtype);
            let number = matches!(
                column.data_type(),
                ArrowType::Int64 | ArrowType::Int32 | ArrowType::Float64 | ArrowType::Float32
            );
            if field.ty.dtype == DataType::Duration && number {
                column = seconds_to_durations(&column).map_err(|err| {
                    Error(format!(
                        "column `{}` of {}: {}",
                        field.name, self.file, err.0
                    ))
                })?;
            } else if column.data_type() != &ty {
                let options = CastOptions {
                    safe: false,
                    ..CastOptions::default()
                };
                column = cast_with_options(&column, &ty, &options).map_err(|err| {
                    let problem = match quoted_value(&err.to_string()) {
                        Some(value) => {
                            format!("cannot read '{value}' as {}", field.ty.dtype.with_article())
                        }
                        None => format!(
                            "holds {}, which cannot be read as {}",
                            column.data_type(),
                            field.ty.dtype.with_article()
                        ),
                    };
                    Error(format!(
                        "column `{}` of {}: {problem}",
                        field.name, self.file
                    ))
                })?;
            }
            if column.null_count() > 0 && !field.ty.nullable {
                // An empty CSV field reads as null, but it is a fine value for a string.
                if self.format == Format::Csv && field.ty.dtype == DataType::Str {
                    let strings = column.as_string::<i32>().iter();
                    let filled: StringArray = strings.map(|s| Some(s.unwrap_or(""))).collect();
                    column = Arc::new(filled);
                } else {
                    return Err(Error(format!(
                        "column `{0}` of {1} has missing values, but is declared `{2}`; \
                         declare it `{2}?`",
                        field.name,
                        self.file,
                        field.ty.dtype.name()
                    )));
                }
            }
            columns.push(column);
        }
        let mut batch = make_batch(&self.read, columns, rows)?;
        for filter in &self.filters {
            let keep = eval_array(filter, &batch)?;
            batch = filter_record_batch(&batch, keep.as_boolean())?;
        }
        let output = self.output.fields.iter().map(|field| {
            let index = self
                .read
                .index_of(&field.name)
                .expect("output columns are read");
            batch.column(index).clone()
        });
        let output: Vec<ArrayRef> = output.collect();
        make_batch(&self.output, output, batch.num_rows())
    }

    fn missing_column(&self, name: &str, available: &[String]) -> Error {
        Error(format!(
            "{} has no column `{name}`; its columns are {}",
            self.file,
            available.join(", ")
        ))
    }
}

/// The type a column is decoded as from a text file. A duration is written there as a number
/// of seconds, which `Shape::finish` converts.
fn file_type(dtype: DataType) -> ArrowType {
    match dtype {
        DataType::Duration => ArrowType::Float64,
        other => arrow_type(other),
    }
}

/// Reads numbers as lengths of time in seconds.
fn seconds_to_durations(column: &ArrayRef) -> Result<ArrayRef> {
    let options = CastOptions::default();
    let seconds = cast_with_options(column, &ArrowType::Float64, &options)?;
    let seconds = seconds.as_primitive::<Float64Type>();
    let micros: DurationMicrosecondArray = seconds.try_unary(|seconds| {
        let micros = (seconds * 1e6).round();
        match micros.is_finite() && micros.abs() < 9.2e18 {
            true => Ok(micros as i64),
            false => Err(arrow::error::ArrowError::ComputeError(format!(
                "{seconds} seconds is too long for a duration"
            ))),
        }
    })?;
    Ok(Arc::new(micros))
}

/// The value that an Arrow message quotes as the one it could not convert.
pub(crate) fn quoted_value(message: &str) -> Option<&str> {
    let (_, rest) = message.split_once(['\'', '"'])?;
    let (value, _) = rest.rsplit_once(['\'', '"'])?;
    Some(value)
}

fn open(scan: &Scan) -> Result<File> {
    File::open(&scan.path).map_err(|err| Error(format!("cannot open {}: {err}", scan.display_path)))
}

/// The end of the row that starts at `start`: the first line break outside quotes.
fn row_end(data: &[u8], start: usize, mut quoted: bool) -> usize {
    let mut pos = start;
    while pos < data.len() {
        match data[pos] {
            b'"' => quoted = !quoted,
            b'\n' if !quoted => return pos + 1,
            _ => {}
        }
        pos += 1;
    }
    data.len()
}

fn header_names(line: &[u8]) -> Vec<String> {
    let line = String::from_utf8_lossy(line);
    let line = line
        .trim_end_matches(['\r', '\n'])
        .trim_start_matches('\u{feff}');
    let mut names = Vec::new();
    let mut name = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                chars.next();
                name.push('"');
            }
            '"' => quoted = !quoted,
            ',' if !quoted => names.push(std::mem::take(&mut name)),
            _ => name.push(c),
        }
    }
    names.push(name);
    names
}

/// Cuts `data[start..]` into ranges of whole rows of about `target` bytes each.
fn split_rows(data: &[u8], start: usize, target: usize) -> Vec<Range<usize>> {
    let body = &data[start..];
    let cuts: Vec<usize> = (0..body.len().div_ceil(target))
        .map(|i| i * target)
        .collect();
    // A line break inside a quoted field does not end a row. Whether a position is inside
    // quotes depends on how many quotes come before it, which is counted on all cores.
    let quotes: Vec<usize> = cuts
        .par_iter()
        .map(|&cut| {
            let part = &body[cut..(cut + target).min(body.len())];
            part.iter().filter(|&&byte| byte == b'"').count()
        })
        .collect();
    let mut ranges = Vec::with_capacity(cuts.len());
    let mut row_start = 0;
    let mut quotes_before = 0;
    for (index, &cut) in cuts.iter().enumerate().skip(1) {
        quotes_before += quotes[index - 1];
        if cut < row_start {
            continue;
        }
        let end = row_end(body, cut, quotes_before % 2 == 1);
        // `row_end` started in the middle of a row, so it found where that row stops.
        if end > row_start && end < body.len() {
            ranges.push(start + row_start..start + end);
            row_start = end;
        }
    }
    if row_start < body.len() {
        ranges.push(start + row_start..data.len());
    }
    ranges
}

/// Rewrites a value error of the CSV reader in terms of the file: its line number and the
/// name of the column. `first_line` is the line of the first row the reader was given, and
/// `read` are the columns being read.
fn csv_error(
    message: &str,
    file: &str,
    first_line: usize,
    names: &[String],
    read: &Schema,
) -> Error {
    let located = |value: &str, column: &str, row: &str| {
        let name = names.get(column.parse::<usize>().ok()?)?;
        let line = first_line + row.parse::<usize>().ok()?;
        let ty = read.field(name)?.ty.dtype.with_article();
        Some(format!(
            "{file}, line {line}: cannot read '{value}' as {ty} for column `{name}`"
        ))
    };
    // Most types are reported with the value, the column, and the row.
    let value_error = || {
        let rest = message.strip_prefix("Parser error: Error while parsing value '")?;
        let (value, rest) = rest.split_once("' as type '")?;
        let (_, rest) = rest.split_once("' for column ")?;
        let (column, rest) = rest.split_once(" at line ")?;
        located(value, column, rest.split_once('.')?.0)
    };
    // A datetime is reported by another part of the reader, in other words.
    let column_error = || {
        let rest = message.strip_prefix("Parser error: Error parsing column ")?;
        let (column, rest) = rest.split_once(" at line ")?;
        let (row, rest) = rest.split_once(": ")?;
        located(quoted_value(rest)?, column, row)
    };
    // A decimal is reported with its value alone.
    let decimal_error = || {
        let value = quoted_value(message.strip_prefix("Parser error: Invalid decimal format: ")?)?;
        let decimals = read.fields.iter();
        let decimals: Vec<String> = decimals
            .filter(|field| field.ty.dtype == DataType::Decimal)
            .map(|field| format!("`{}`", field.name))
            .collect();
        let columns = match decimals.len() {
            1 => format!("column {}", decimals[0]),
            _ => format!("one of the columns {}", decimals.join(", ")),
        };
        Some(format!(
            "{file}: cannot read '{value}' as a decimal for {columns}"
        ))
    };
    // A row with more or fewer fields than the header is counted from 1.
    let field_count = || {
        let rest = message.strip_prefix("Csv error: incorrect number of fields for line ")?;
        let (row, rest) = rest.split_once(", expected ")?;
        let (expected, found) = rest.split_once(" got ")?;
        let line = first_line + row.parse::<usize>().ok()? - 1;
        let fields = match found.trim() {
            "1" => "1 field".to_string(),
            found => format!("{found} fields"),
        };
        Some(format!(
            "{file}, line {line}: the row has {fields}, but the header has {expected}"
        ))
    };
    let rewritten = value_error()
        .or_else(column_error)
        .or_else(decimal_error)
        .or_else(field_count);
    Error(rewritten.unwrap_or_else(|| format!("{file}: {message}")))
}

/// The start of a value, when all of it would drown a message.
fn shortened(value: &str) -> String {
    const SHOWN: usize = 60;
    match value.char_indices().nth(SHOWN) {
        Some((end, _)) => format!("{}...", &value[..end]),
        None => value.to_string(),
    }
}

/// Rewrites a value error of the JSON reader in terms of the file and the declared columns.
fn json_error(message: &str, file: &str, read: &Schema) -> Error {
    let message = message.strip_prefix("Json error: ").unwrap_or(message);
    let field_error = || {
        let rest = message.strip_prefix("whilst decoding field '")?;
        let (name, rest) = rest.split_once("': ")?;
        let ty = read.field(name)?.ty.dtype.with_article();
        // `failed to parse "x" as Int64`, or `expected boolean got "x"`.
        let value = match rest.strip_prefix("failed to parse ") {
            Some(rest) => rest.split_once(" as ")?.0,
            None => rest.split_once(" got ")?.1,
        };
        let value = shortened(value);
        Some(format!(
            "{file}: cannot read {value} as {ty} for field `{name}`"
        ))
    };
    let not_an_object = || {
        let found = shortened(message.strip_prefix("expected { got ")?);
        Some(format!(
            "{file}: every line must be one JSON object, but one is {found}"
        ))
    };
    let decimal_error = || {
        let value = quoted_value(message.strip_prefix("Parser error: Invalid decimal format: ")?)?;
        Some(format!("{file}: cannot read \"{value}\" as a decimal"))
    };
    let truncated = || {
        message
            .starts_with("Truncated record")
            .then(|| format!("{file}: a line is not a whole JSON object"))
    };
    let rewritten = field_error()
        .or_else(decimal_error)
        .or_else(not_an_object)
        .or_else(truncated);
    Error(rewritten.unwrap_or_else(|| format!("{file}: {message}")))
}

fn csv_pieces(scan: &Scan, shape: &Arc<Shape>) -> Result<Vec<Piece>> {
    let file = open(scan)?;
    if file.metadata().map(|meta| meta.len() == 0).unwrap_or(false) {
        return Err(Error(format!("{} is empty", scan.display_path)));
    }
    // SAFETY: the mapping is only read. If another process truncates the file while the
    // query runs, the read fails with a signal instead of an error; that is the accepted
    // cost of not copying the file into memory.
    let data = Arc::new(
        unsafe { Mmap::map(&file) }
            .map_err(|err| Error(format!("cannot read {}: {err}", scan.display_path)))?,
    );
    let header_end = row_end(&data, 0, false);
    let names = header_names(&data[..header_end]);

    // The reader takes column types by position, so every column of the file needs one.
    // Those that are not read are declared as strings and left out of the projection.
    let mut fields: Vec<ArrowField> = (0..names.len())
        .map(|index| ArrowField::new(format!("#{index}"), ArrowType::Utf8, true))
        .collect();
    let mut projection = Vec::with_capacity(shape.read.fields.len());
    for field in &shape.read.fields {
        let Some(index) = names.iter().position(|name| *name == *field.name) else {
            return Err(shape.missing_column(&field.name, &names));
        };
        fields[index] = ArrowField::new(&*field.name, file_type(field.ty.dtype), true);
        projection.push(index);
    }
    let file_schema: SchemaRef = Arc::new(ArrowSchema::new(fields));

    let ranges = split_rows(&data, header_end, PIECE_BYTES);
    let names = Arc::new(names);
    let pieces = ranges.into_iter().map(|range| {
        let (data, shape, names) = (data.clone(), shape.clone(), names.clone());
        let (file_schema, projection) = (file_schema.clone(), projection.clone());
        Box::new(move || {
            let reader = ReaderBuilder::new(file_schema)
                .with_header(false)
                .with_batch_size(BATCH_ROWS)
                .with_projection(projection)
                .build(Cursor::new(&data[range.clone()]))?;
            let mut batches = Vec::new();
            for batch in reader {
                let batch = batch.map_err(|err| {
                    // The reader counts rows from the start of its piece, and columns by
                    // position; say where that is in the file.
                    let lines_before = data[..range.start].iter().filter(|&&byte| byte == b'\n');
                    csv_error(
                        &err.to_string(),
                        &shape.file,
                        lines_before.count() + 1,
                        &names,
                        &shape.read,
                    )
                })?;
                batches.push(shape.finish(&batch)?);
            }
            Ok(batches)
        }) as Piece
    });
    Ok(pieces.collect())
}

fn parquet_pieces(scan: &Scan, shape: &Arc<Shape>) -> Result<Vec<Piece>> {
    // The footer describes the whole file. It is read once and shared by the pieces.
    let metadata = ArrowReaderMetadata::load(&open(scan)?, ArrowReaderOptions::new())?;
    let available: Vec<String> = metadata
        .schema()
        .fields()
        .iter()
        .map(|field| field.name().clone())
        .collect();
    for field in &shape.read.fields {
        if !available.iter().any(|name| *name == *field.name) {
            return Err(shape.missing_column(&field.name, &available));
        }
    }
    let names: Arc<Vec<String>> = Arc::new(
        shape
            .read
            .fields
            .iter()
            .map(|f| f.name.to_string())
            .collect(),
    );
    let row_groups = metadata.metadata().num_row_groups();
    let path = scan.path.clone();
    let pieces = (0..row_groups).map(|row_group| {
        let (shape, path, names, metadata) =
            (shape.clone(), path.clone(), names.clone(), metadata.clone());
        Box::new(move || {
            let file = File::open(&path)
                .map_err(|err| Error(format!("cannot open {}: {err}", shape.file)))?;
            let builder = ParquetRecordBatchReaderBuilder::new_with_metadata(file, metadata);
            let columns = names.iter().map(String::as_str);
            let mask = ProjectionMask::columns(builder.parquet_schema(), columns);
            let reader = builder
                .with_projection(mask)
                .with_row_groups(vec![row_group])
                .with_batch_size(BATCH_ROWS)
                .build()?;
            let mut batches = Vec::new();
            for batch in reader {
                // The file stores its columns in its own order.
                let batch = batch?;
                let mut columns = Vec::with_capacity(names.len());
                for name in names.iter() {
                    let column = batch.column_by_name(name).cloned();
                    let column = column
                        .ok_or_else(|| Error(format!("{} has no column `{name}`", shape.file)))?;
                    columns.push((name.as_str(), column, true));
                }
                let batch = RecordBatch::try_from_iter_with_nullable(columns)?;
                batches.push(shape.finish(&batch)?);
            }
            Ok(batches)
        }) as Piece
    });
    Ok(pieces.collect())
}

/// Cuts `data` into ranges of whole lines of about `target` bytes each.
fn split_lines(data: &[u8], target: usize) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < data.len() {
        let from = (start + target).min(data.len());
        let end = data[from..]
            .iter()
            .position(|&byte| byte == b'\n')
            .map_or(data.len(), |newline| from + newline + 1);
        ranges.push(start..end);
        start = end;
    }
    ranges
}

/// A file of JSON objects, one per line. A field that an object lacks reads as null.
fn json_pieces(scan: &Scan, shape: &Arc<Shape>) -> Result<Vec<Piece>> {
    let file = open(scan)?;
    if file.metadata().map(|meta| meta.len() == 0).unwrap_or(false) {
        return Ok(Vec::new());
    }
    // SAFETY: as for CSV files, the mapping is only read.
    let data = Arc::new(
        unsafe { Mmap::map(&file) }
            .map_err(|err| Error(format!("cannot read {}: {err}", scan.display_path)))?,
    );
    let fields = shape
        .read
        .fields
        .iter()
        .map(|field| ArrowField::new(&*field.name, file_type(field.ty.dtype), true));
    let schema: SchemaRef = Arc::new(ArrowSchema::new(fields.collect::<Vec<_>>()));
    let pieces = split_lines(&data, PIECE_BYTES).into_iter().map(|range| {
        let (data, shape, schema) = (data.clone(), shape.clone(), schema.clone());
        Box::new(move || {
            let fail = |err: arrow::error::ArrowError| {
                json_error(&err.to_string(), &shape.file, &shape.read)
            };
            let reader = arrow::json::ReaderBuilder::new(schema)
                .with_batch_size(BATCH_ROWS)
                .build(Cursor::new(&data[range]))
                .map_err(fail)?;
            let mut batches = Vec::new();
            for batch in reader {
                batches.push(shape.finish(&batch.map_err(fail)?)?);
            }
            Ok(batches)
        }) as Piece
    });
    Ok(pieces.collect())
}

/// Converts a value of a SQLite row to the declared type of its column.
fn sql_value(value: rusqlite::types::ValueRef, field: &Field) -> Result<Scalar> {
    use rusqlite::types::ValueRef;
    let text = |bytes| std::str::from_utf8(bytes).ok();
    let converted = match (value, field.ty.dtype) {
        (ValueRef::Null, _) => Some(Scalar::Null),
        (ValueRef::Integer(value), DataType::Int) => Some(Scalar::Int(value)),
        (ValueRef::Integer(value), DataType::Float) => Some(Scalar::Float(value as f64)),
        (ValueRef::Integer(value), DataType::Bool) => Some(Scalar::Bool(value != 0)),
        // A number is a length of time in seconds.
        (ValueRef::Integer(value), DataType::Duration) => {
            value.checked_mul(1_000_000).map(Scalar::Duration)
        }
        (ValueRef::Real(value), DataType::Duration) => {
            let micros = (value * 1e6).round();
            (micros.abs() < 9.2e18).then_some(Scalar::Duration(micros as i64))
        }
        (ValueRef::Integer(value), DataType::Decimal) => {
            Some(Scalar::Decimal(scalar::decimal_from_int(value)))
        }
        (ValueRef::Integer(value), DataType::Str) => Some(Scalar::Str(value.to_string().into())),
        (ValueRef::Real(value), DataType::Float) => Some(Scalar::Float(value)),
        (ValueRef::Real(value), DataType::Decimal) => {
            scalar::decimal_from_float(value).map(Scalar::Decimal)
        }
        (ValueRef::Real(value), DataType::Str) => Some(Scalar::Str(value.to_string().into())),
        (ValueRef::Text(bytes), dtype) => text(bytes).and_then(|text| match dtype {
            DataType::Str => Some(Scalar::Str(text.into())),
            DataType::Int => text.trim().parse().ok().map(Scalar::Int),
            DataType::Float => text.trim().parse().ok().map(Scalar::Float),
            DataType::Bool => match text.trim() {
                "true" | "1" => Some(Scalar::Bool(true)),
                "false" | "0" => Some(Scalar::Bool(false)),
                _ => None,
            },
            DataType::Date => Date::parse(text.get(..10)?).map(|date| Scalar::Date(date.to_days())),
            DataType::DateTime => scalar::parse_datetime(text)
                .or_else(|| scalar::parse_datetime(&format!("{text}T00:00")))
                .map(Scalar::DateTime),
            DataType::Decimal => scalar::parse_decimal(text).map(Scalar::Decimal),
            DataType::Duration => None,
        }),
        _ => None,
    };
    converted.ok_or_else(|| {
        let shown = match value {
            ValueRef::Text(bytes) => format!("'{}'", String::from_utf8_lossy(bytes)),
            ValueRef::Integer(value) => value.to_string(),
            ValueRef::Real(value) => value.to_string(),
            _ => "a blob".to_string(),
        };
        Error(format!(
            "cannot read {shown} as {} for column `{}`",
            field.ty.dtype.with_article(),
            field.name
        ))
    })
}

/// The rows of a query on a SQLite database, read by one piece.
fn sqlite_pieces(scan: &Scan, shape: &Arc<Shape>) -> Result<Vec<Piece>> {
    let Some(query) = scan.query.clone() else {
        return Err(Error(
            "internal error: a database scan without a query".into(),
        ));
    };
    let (path, shape) = (scan.path.clone(), shape.clone());
    let piece = move || {
        let fail = |err: rusqlite::Error| Error(format!("{}: {err}", shape.file));
        let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY;
        let connection = rusqlite::Connection::open_with_flags(&path, flags).map_err(fail)?;
        let mut statement = connection.prepare(&query).map_err(fail)?;
        let names: Vec<String> = statement
            .column_names()
            .iter()
            .map(|n| n.to_string())
            .collect();
        let fields = &shape.read.fields;
        let mut positions = Vec::with_capacity(fields.len());
        for field in fields {
            let position = names.iter().position(|name| *name == *field.name);
            positions.push(position.ok_or_else(|| shape.missing_column(&field.name, &names))?);
        }

        let mut batches = Vec::new();
        let mut columns: Vec<Vec<Scalar>> = vec![Vec::new(); fields.len()];
        let mut flush = |columns: &mut Vec<Vec<Scalar>>| -> Result<()> {
            let mut arrays = Vec::with_capacity(fields.len());
            for (field, values) in fields.iter().zip(columns.iter_mut()) {
                let values = std::mem::take(values).into_iter();
                arrays.push((
                    &*field.name,
                    scalars_to_array(values, field.ty.dtype)?,
                    true,
                ));
            }
            if !arrays.is_empty() {
                let batch = RecordBatch::try_from_iter_with_nullable(arrays)?;
                batches.push(shape.finish(&batch)?);
            }
            Ok(())
        };
        let mut rows = statement.query([]).map_err(fail)?;
        while let Some(row) = rows.next().map_err(fail)? {
            for ((field, position), column) in fields.iter().zip(&positions).zip(&mut columns) {
                let value = row.get_ref(*position).map_err(fail)?;
                let value = sql_value(value, field);
                column.push(value.map_err(|err| Error(format!("{}: {}", shape.file, err.0)))?);
            }
            if columns
                .first()
                .is_some_and(|column| column.len() == BATCH_ROWS)
            {
                flush(&mut columns)?;
            }
        }
        if columns.first().is_some_and(|column| !column.is_empty()) {
            flush(&mut columns)?;
        }
        Ok(batches)
    };
    Ok(vec![Box::new(piece)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_at_row_boundaries_only() {
        let data = b"h1,h2\na,\"x\ny\"\nb,2\nc,\"say \"\"hi\"\"\n\"\nd,4\n";
        let start = row_end(data, 0, false);
        for target in 1..data.len() {
            let ranges = split_rows(data, start, target);
            let rows: Vec<&[u8]> = ranges.iter().map(|range| &data[range.clone()]).collect();
            assert_eq!(rows.concat(), &data[start..], "target {target}");
            for row in rows {
                let quotes = row.iter().filter(|&&byte| byte == b'"').count();
                assert!(
                    quotes % 2 == 0 && row.ends_with(b"\n"),
                    "target {target}: {row:?}"
                );
            }
        }
    }

    #[test]
    fn rewrites_reader_errors() {
        use biggo_plan::ColType;
        let names = ["date".to_string(), "region".to_string()];
        let read = Schema::new(vec![
            Field::new("date", ColType::required(DataType::DateTime)),
            Field::new("region", ColType::required(DataType::Int)),
        ]);
        let message = "Parser error: Error while parsing value 'north' as type 'Int64' \
                       for column 1 at line 3. Row data: '[2026-01-05,north]'";
        assert_eq!(
            csv_error(message, "sales.csv", 10, &names, &read).0,
            "sales.csv, line 13: cannot read 'north' as an int for column `region`"
        );
        let message = "Parser error: Error parsing column 0 at line 2: Parser error: \
                       Error parsing timestamp from 'soon': error parsing date";
        assert_eq!(
            csv_error(message, "sales.csv", 10, &names, &read).0,
            "sales.csv, line 12: cannot read 'soon' as a datetime for column `date`"
        );
        let message = "Csv error: incorrect number of fields for line 2, expected 2 got 3";
        assert_eq!(
            csv_error(message, "sales.csv", 10, &names, &read).0,
            "sales.csv, line 11: the row has 3 fields, but the header has 2"
        );
        assert_eq!(
            csv_error("something else", "sales.csv", 1, &names, &read).0,
            "sales.csv: something else"
        );
        assert_eq!(
            json_error("Json error: expected { got [1, 2]", "sales.json", &read).0,
            "sales.json: every line must be one JSON object, but one is [1, 2]"
        );
        let message = "Json error: whilst decoding field 'region': failed to parse \"x\" as Int64";
        assert_eq!(
            json_error(message, "sales.json", &read).0,
            "sales.json: cannot read \"x\" as an int for field `region`"
        );
    }

    #[test]
    fn reads_header_names() {
        assert_eq!(
            header_names(b"a,b c,\"d,e\",\"f\"\"g\"\r\n"),
            ["a", "b c", "d,e", "f\"g"]
        );
        assert_eq!(header_names("\u{feff}id,ชื่อ\n".as_bytes()), ["id", "ชื่อ"]);
    }
}
