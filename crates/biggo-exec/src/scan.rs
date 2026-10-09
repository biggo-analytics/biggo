//! Reads files and databases. A file is split into pieces that are decoded on all cores, a
//! few at a time, so that a reader who wants only the first rows does not pay for the rest.

use std::collections::VecDeque;
use std::fs::File;
use std::io::Cursor;
use std::ops::{Deref, Range};
use std::sync::Arc;

use arrow::array::{Array, ArrayRef, AsArray, DurationMicrosecondArray, RecordBatch, StringArray};
use arrow::compute::{CastOptions, cast_with_options, filter_record_batch};
use arrow::csv::ReaderBuilder;
use arrow::datatypes::{
    DataType as ArrowType, Field as ArrowField, Float64Type, Schema as ArrowSchema, SchemaRef,
};
use biggo_plan::Date;
use biggo_plan::{
    ColType, DataType, Expr, Field, Format, Scalar, Scan, Schema, scalar, shown_path,
};
use memmap2::Mmap;
use parquet::arrow::ProjectionMask;
use parquet::arrow::arrow_reader::{
    ArrowReaderMetadata, ArrowReaderOptions, ParquetRecordBatchReaderBuilder,
};
use rayon::prelude::*;
use regex::Regex;

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
    let mut pieces: Vec<Piece> = Vec::new();
    for file in files(scan)? {
        let shape = Arc::new(Shape::new(&file));
        pieces.extend(match scan.format {
            Format::Csv => csv_pieces(&file, &shape)?,
            Format::Parquet => parquet_pieces(&file, &shape)?,
            Format::Json => json_pieces(&file, &shape)?,
            Format::Sqlite => sqlite_pieces(&file, &shape)?,
            Format::Excel => crate::excel::pieces(&file, &shape)?,
            Format::Postgres => crate::database::pieces(&file, &shape)?,
            Format::Mysql => crate::database::mysql_pieces(&file, &shape)?,
        });
    }
    Ok(Box::new(ScanIter {
        pieces: pieces.into_iter(),
        ready: VecDeque::new(),
        error: None,
        remaining: scan.limit,
        // With a limit, the first piece often has every row that is wanted.
        group: if scan.limit.is_some() {
            1
        } else {
            rayon::current_num_threads()
        },
    }))
}

/// The files that a scan reads, each as a scan of its own: the file at the path, or, when
/// there is none and the path has `*`, `?` or `[`, the files that it matches as a pattern.
/// They come in the order of their names, so that a query reads them the same way each time.
fn files(scan: &Scan) -> Result<Vec<Scan>> {
    let pattern = scan.path.to_string_lossy();
    let one = matches!(
        scan.format,
        Format::Sqlite | Format::Postgres | Format::Mysql
    ) || !pattern.contains(['*', '?', '['])
        || scan.path.exists();
    if one {
        return Ok(vec![scan.clone()]);
    }
    let shown = &*scan.display_path;
    let matches = glob::glob(&pattern)
        .map_err(|err| Error(format!("{shown:?} is not a pattern for files: {}", err.msg)))?;
    // A file is named as the program names the pattern: without the folder of the program.
    let folder = match shown_path(&*pattern).ends_with(shown) {
        true => pattern.len() - shown.len(),
        false => 0,
    };
    let mut found = Vec::new();
    for path in matches {
        let path =
            path.map_err(|err| Error(format!("cannot look for {shown}: {}", err.error())))?;
        if path.is_dir() {
            continue;
        }
        let display_path = match path.to_string_lossy().get(folder..) {
            Some(name) => shown_path(name).into(),
            None => shown_path(&path).into(),
        };
        found.push(Scan {
            path,
            display_path,
            ..scan.clone()
        });
    }
    match found.is_empty() {
        true => Err(Error(format!("no file matches {shown}"))),
        false => Ok(found),
    }
}

/// A part of a file that can be decoded independently of the others.
pub(crate) type Piece = Box<dyn FnOnce() -> Result<Vec<RecordBatch>> + Send>;

struct ScanIter {
    pieces: std::vec::IntoIter<Piece>,
    ready: VecDeque<RecordBatch>,
    /// The error of a piece, to report once the batches of the pieces before it are given.
    error: Option<Error>,
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
            if let Some(err) = self.error.take() {
                return Some(Err(err));
            }
            let group: Vec<Piece> = self.pieces.by_ref().take(self.group).collect();
            if group.is_empty() {
                return None;
            }
            self.group = rayon::current_num_threads();
            // Several pieces are decoded at once, but the reader sees them one after the
            // other: a piece that fails stops the scan only when the reader gets to it,
            // however many cores were decoding ahead.
            let decoded: Vec<Result<Vec<RecordBatch>>> =
                group.into_par_iter().map(|piece| piece()).collect();
            for piece in decoded {
                match piece {
                    Ok(batches) => {
                        let filled = batches.into_iter().filter(|batch| batch.num_rows() > 0);
                        self.ready.extend(filled);
                    }
                    Err(err) => {
                        self.pieces = Vec::new().into_iter();
                        self.error = Some(err);
                        break;
                    }
                }
            }
        }
    }
}

/// What a scan makes of the columns it decodes: it checks them against their declared
/// types, drops the rows that fail the scan's filters, and keeps the columns asked for.
pub(crate) struct Shape {
    pub(crate) file: String,
    format: Format,
    /// The declared columns that are read from the file: those produced and those the
    /// filters need.
    pub(crate) read: Schema,
    /// The columns the filters see: those of `read`, and after them the column that holds
    /// the name of the file, if it is wanted.
    whole: Schema,
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
        let named = |field: &&Field| scan.read.file_column.as_deref() == Some(&*field.name);
        let declared = scan.declared.fields.iter();
        let (name, in_file): (Vec<&Field>, Vec<&Field>) = declared.partition(named);
        let mut read: Vec<Field> = in_file
            .iter()
            .copied()
            .filter(|f| wanted(&f.name))
            .cloned()
            .collect();
        // The rows of a file are counted by reading something, even if only its name is
        // wanted.
        if read.is_empty() {
            read.extend(in_file.first().copied().cloned());
        }
        let mut whole = read.clone();
        whole.extend(name.into_iter().filter(|f| wanted(&f.name)).cloned());
        Self {
            file: scan.display_path.to_string(),
            format: scan.format,
            read: Schema::new(read),
            whole: Schema::new(whole),
            output: (*scan.schema).clone(),
            filters: scan.filters.clone(),
        }
    }

    /// Turns a decoded batch, whose columns are those of `read` in order, into output.
    pub(crate) fn finish(&self, decoded: &RecordBatch) -> Result<RecordBatch> {
        let rows = decoded.num_rows();
        let mut columns = Vec::with_capacity(self.read.fields.len());
        for (field, column) in self.read.fields.iter().zip(decoded.columns()) {
            let mut column = column.clone();
            let ty = arrow_type(field.ty.dtype);
            let number = matches!(
                column.data_type(),
                ArrowType::Int64 | ArrowType::Int32 | ArrowType::Float64 | ArrowType::Float32
            );
            // A length of time is written as a number of seconds, which may come as text.
            if field.ty.dtype == DataType::Duration && column.data_type() == &ArrowType::Utf8 {
                let options = CastOptions {
                    safe: false,
                    ..CastOptions::default()
                };
                column =
                    cast_with_options(&column, &ArrowType::Float64, &options).map_err(|err| {
                        let problem = match quoted_value(&err.to_string()) {
                            Some(value) => format!("cannot read '{value}' as a duration"),
                            None => "holds text that is not a number of seconds".to_string(),
                        };
                        Error(format!(
                            "column `{}` of {}: {problem}",
                            field.name, self.file
                        ))
                    })?;
            }
            let number = number || column.data_type() == &ArrowType::Float64;
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
                // An empty field or cell reads as null, but it is a fine value for a string.
                let text = matches!(self.format, Format::Csv | Format::Excel);
                if text && field.ty.dtype == DataType::Str {
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
            // A file can hold negative zero and any "not a number"; a column holds one of each.
            columns.push(crate::expr::canonical(column));
        }
        if self.whole.fields.len() > self.read.fields.len() {
            let name: StringArray = std::iter::repeat_n(Some(self.file.as_str()), rows).collect();
            columns.push(Arc::new(name));
        }
        let mut batch = make_batch(&self.whole, columns, rows)?;
        for filter in &self.filters {
            let keep = eval_array(filter, &batch)?;
            batch = filter_record_batch(&batch, keep.as_boolean())?;
        }
        let output = self.output.fields.iter().map(|field| {
            let index = self
                .whole
                .index_of(&field.name)
                .expect("output columns are read");
            batch.column(index).clone()
        });
        let output: Vec<ArrayRef> = output.collect();
        make_batch(&self.output, output, batch.num_rows())
    }

    pub(crate) fn missing_column(&self, name: &str, available: &[String]) -> Error {
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
    crate::open(&scan.path, &scan.display_path)
}

/// The end of the row that starts at `start`: the first line break outside quotes.
pub(crate) fn row_end(data: &[u8], start: usize, mut quoted: bool) -> usize {
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

/// The end of the line that starts at `start`, whatever is in it.
pub(crate) fn line_end(data: &[u8], start: usize) -> usize {
    let line = data[start..].iter().position(|&byte| byte == b'\n');
    line.map_or(data.len(), |end| start + end + 1)
}

/// A pattern for the fields that are null when a file says which texts mean null: those
/// texts, and the empty field. `None` if the file names no texts.
fn null_texts(texts: &[Arc<str>]) -> Result<Option<Regex>> {
    if texts.is_empty() {
        return Ok(None);
    }
    let texts: Vec<String> = texts.iter().map(|text| regex::escape(text)).collect();
    let pattern = format!("^(?:{})?$", texts.join("|"));
    let pattern = Regex::new(&pattern).map_err(|err| Error(format!("`nulls`: {err}")))?;
    Ok(Some(pattern))
}

/// A pattern that no field matches, for reading a file with nothing taken as null.
fn never() -> Regex {
    Regex::new(r"[^\s\S]").expect("the pattern is valid")
}

pub(crate) fn header_names(line: &[u8], delimiter: u8) -> Vec<String> {
    let delimiter = char::from(delimiter);
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
            c if c == delimiter && !quoted => names.push(std::mem::take(&mut name)),
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
/// name of the column. `line_of` gives the line of the file that a row starts on, where rows
/// are counted from 0 as the reader counts them; `read` are the columns being read, and
/// `header` says whether the file has one.
fn csv_error(
    message: &str,
    file: &str,
    line_of: &dyn Fn(usize) -> usize,
    names: &[String],
    read: &Schema,
    header: bool,
) -> Error {
    let located = |value: &str, column: &str, row: &str| {
        let name = names.get(column.parse::<usize>().ok()?)?;
        let line = line_of(row.parse().ok()?);
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
        let line = line_of(row.parse::<usize>().ok()?.checked_sub(1)?);
        let fields = match found.trim() {
            "1" => "1 field".to_string(),
            found => format!("{found} fields"),
        };
        // The number of columns of a file is that of its first line.
        let first = if header { "header" } else { "first row" };
        Some(format!(
            "{file}, line {line}: the row has {fields}, but the {first} has {expected}"
        ))
    };
    if message.contains("invalid UTF-8") {
        return not_utf8(file);
    }
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
/// `each` is what holds one object in this file: a line, or an item of an array.
fn json_error(message: &str, file: &str, read: &Schema, each: &str) -> Error {
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
            "{file}: every {each} must be one JSON object, but one is {found}"
        ))
    };
    let decimal_error = || {
        let value = quoted_value(message.strip_prefix("Parser error: Invalid decimal format: ")?)?;
        Some(format!("{file}: cannot read \"{value}\" as a decimal"))
    };
    let truncated = || {
        message.starts_with("Truncated record").then(|| {
            format!(
                "{file}: a{} {each} is not a whole JSON object",
                if each == "item" { "n" } else { "" }
            )
        })
    };
    let rewritten = field_error()
        .or_else(decimal_error)
        .or_else(not_an_object)
        .or_else(truncated);
    Error(rewritten.unwrap_or_else(|| format!("{file}: {message}")))
}

/// The text of a CSV file as UTF-8: the file itself, or a copy made from another encoding.
enum Text {
    Mapped(Mmap),
    Decoded(Vec<u8>),
}

impl Deref for Text {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            Text::Mapped(data) => data,
            Text::Decoded(data) => data,
        }
    }
}

/// What to say of a CSV file that was read as UTF-8 and is not.
fn not_utf8(file: &str) -> Error {
    Error(format!(
        "{file} is not UTF-8 text; if it is in another encoding, name it, \
         as in `encoding = \"tis-620\"`"
    ))
}

fn csv_pieces(scan: &Scan, shape: &Arc<Shape>) -> Result<Vec<Piece>> {
    let file = open(scan)?;
    if file.metadata().map(|meta| meta.len() == 0).unwrap_or(false) {
        return Err(Error(format!("{} is empty", scan.display_path)));
    }
    // SAFETY: the mapping is only read. If another process truncates the file while the
    // query runs, the read fails with a signal instead of an error; that is the accepted
    // cost of not copying the file into memory.
    let mapped = unsafe { Mmap::map(&file) }
        .map_err(|err| Error(format!("cannot read {}: {err}", scan.display_path)))?;
    let data = Arc::new(match scan.csv.encoding {
        None => Text::Mapped(mapped),
        // The readers take UTF-8, so a file in another encoding is converted whole first.
        // A byte order mark at the start of the file overrules the encoding that was named.
        Some(encoding) => {
            let (text, _, malformed) = encoding.decode(&mapped);
            if malformed {
                return Err(Error(format!(
                    "{} is not {} text",
                    scan.display_path,
                    encoding.name().to_ascii_lowercase()
                )));
            }
            Text::Decoded(text.into_owned().into_bytes())
        }
    });
    let delimiter = scan.csv.delimiter;
    // The lines before the table are not rows of it, so a quote in them opens nothing.
    let mut start = 0;
    for _ in 0..scan.read.skip {
        start = line_end(&data, start);
    }
    if start == data.len() {
        return Err(Error(format!(
            "{} has nothing after the {} lines that `skip` passes over",
            scan.display_path, scan.read.skip
        )));
    }
    let first_end = row_end(&data, start, false);
    if std::str::from_utf8(&data[start..first_end]).is_err() {
        return Err(not_utf8(&scan.display_path));
    }
    let first = header_names(&data[start..first_end], delimiter);
    let (names, body) = match scan.read.header {
        true => (first, first_end),
        // Without a header the columns are the declared ones, in the order they are
        // declared, and the first line is a row.
        false => {
            let declared = scan.declared.fields.iter();
            let in_file =
                declared.filter(|field| scan.read.file_column.as_deref() != Some(&*field.name));
            let mut names: Vec<String> = in_file.map(|field| field.name.to_string()).collect();
            if first.len() < names.len() {
                let columns = match first.len() {
                    1 => "1 column".to_string(),
                    count => format!("{count} columns"),
                };
                return Err(Error(format!(
                    "{} has {columns}, but its row type has {}",
                    scan.display_path,
                    names.len()
                )));
            }
            // Columns after the declared ones are not read.
            names.extend((names.len()..first.len()).map(|index| format!("#{index}")));
            (names, start)
        }
    };

    // The reader takes column types by position, so every column of the file needs one.
    // Those that are not read are declared as strings and left out of the projection.
    let mut fields: Vec<ArrowField> = (0..names.len())
        .map(|index| ArrowField::new(format!("#{index}"), ArrowType::Utf8, true))
        .collect();
    let mut projection = Vec::with_capacity(shape.read.fields.len());
    for field in &shape.read.fields {
        let Some(index) = names.iter().position(|name| *name == *field.name) else {
            let Error(mut message) = shape.missing_column(&field.name, &names);
            // A header that is one long column was probably cut at the wrong character.
            let other = [('\t', "\\t"), (';', ";"), ('|', "|")];
            let other = other
                .iter()
                .find(|(c, _)| names.len() == 1 && names[0].contains(*c));
            if let Some((_, written)) = other.filter(|_| delimiter == b',') {
                message.push_str(&format!(
                    "; if the file separates its columns with another character, name it, \
                     as in `delimiter = \"{written}\"`"
                ));
            }
            return Err(Error(message));
        };
        fields[index] = ArrowField::new(&*field.name, file_type(field.ty.dtype), true);
        projection.push(index);
    }
    let file_schema: SchemaRef = Arc::new(ArrowSchema::new(fields));

    let ranges = split_rows(&data, body, PIECE_BYTES);
    let names = Arc::new(names);
    let nulls = null_texts(&scan.read.nulls)?;
    // Where the texts that mean null are text all the same: the columns of strings that
    // cannot be null, where an empty field is an empty string too.
    let kept: Vec<usize> = match nulls {
        Some(_) => (0..shape.read.fields.len())
            .filter(|&index| shape.read.fields[index].ty == ColType::new(DataType::Str, false))
            .collect(),
        None => Vec::new(),
    };
    let pieces = ranges.into_iter().map(|range| {
        let (data, shape, names) = (data.clone(), shape.clone(), names.clone());
        let (file_schema, projection) = (file_schema.clone(), projection.clone());
        let (nulls, kept) = (nulls.clone(), kept.clone());
        let header = scan.read.header;
        Box::new(move || {
            let reader = |nulls: Option<Regex>, projection: Vec<usize>| {
                let builder = ReaderBuilder::new(file_schema.clone())
                    .with_header(false)
                    .with_delimiter(delimiter)
                    .with_batch_size(BATCH_ROWS)
                    .with_projection(projection);
                let builder = match nulls {
                    Some(nulls) => builder.with_null_regex(nulls),
                    None => builder,
                };
                builder.build(Cursor::new(&data[range.clone()]))
            };
            // A second reading of the columns of `kept`, in which nothing is null. It is
            // made only if one of them turns out to hold a text that means null.
            let mut as_written: Option<Vec<RecordBatch>> = None;
            let mut batches = Vec::new();
            for (number, batch) in reader(nulls.clone(), projection.clone())?.enumerate() {
                let batch = batch.map_err(|err| {
                    // The reader counts rows from the start of its piece, passing over
                    // the lines with nothing on them, and columns by position; say where
                    // that is in the file.
                    let line_of = |row: usize| {
                        let (mut start, mut rows) = (range.start, 0);
                        while start < range.end {
                            let end = row_end(&data, start, false);
                            let blank = data[start..end].iter().all(|byte| b"\r\n".contains(byte));
                            if !blank && rows == row {
                                break;
                            }
                            rows += usize::from(!blank);
                            start = end;
                        }
                        data[..start].iter().filter(|&&byte| byte == b'\n').count() + 1
                    };
                    csv_error(
                        &err.to_string(),
                        &shape.file,
                        &line_of,
                        &names,
                        &shape.read,
                        header,
                    )
                })?;
                let mut columns = batch.columns().to_vec();
                for (place, &index) in kept.iter().enumerate() {
                    if columns[index].null_count() == 0 {
                        continue;
                    }
                    if as_written.is_none() {
                        let columns: Vec<usize> = kept.iter().map(|&i| projection[i]).collect();
                        let batches: std::result::Result<_, _> =
                            reader(Some(never()), columns)?.collect();
                        as_written = Some(batches?);
                    }
                    let written = as_written.as_ref().expect("it was just read");
                    columns[index] = written[number].column(place).clone();
                }
                let batch = RecordBatch::try_new(batch.schema(), columns)?;
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
            let file = crate::open(&path, &shape.file)?;
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

/// Calls `found` with the place of each comma of `text` that is outside every string,
/// object and array: in the items of a JSON array, the commas between the items.
pub(crate) fn outer_commas(text: &[u8], mut found: impl FnMut(usize)) {
    let (mut depth, mut quoted, mut escaped) = (0usize, false, false);
    for (at, &byte) in text.iter().enumerate() {
        if quoted {
            match byte {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => quoted = false,
                _ => {}
            }
            continue;
        }
        match byte {
            b'"' => quoted = true,
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => found(at),
            _ => {}
        }
    }
}

/// If `data` is one JSON array, the range of what is between its brackets.
pub(crate) fn array_items(data: &[u8], file: &str) -> Result<Option<Range<usize>>> {
    let space = |byte: &u8| byte.is_ascii_whitespace();
    let Some(open) = data.iter().position(|byte| !space(byte)) else {
        return Ok(None);
    };
    if data[open] != b'[' {
        return Ok(None);
    }
    match data.iter().rposition(|byte| !space(byte)) {
        Some(close) if close > open && data[close] == b']' => Ok(Some(open + 1..close)),
        _ => Err(Error(format!(
            "{file} starts a JSON array with `[`, but does not end with `]`"
        ))),
    }
}

/// Cuts the items of a JSON array into ranges of whole items of about `target` bytes each.
fn split_items(data: &[u8], items: Range<usize>, target: usize) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0;
    outer_commas(&data[items.clone()], |comma| {
        if comma - start >= target {
            ranges.push(items.start + start..items.start + comma);
            start = comma + 1;
        }
    });
    ranges.push(items.start + start..items.end);
    ranges
}

/// A file of JSON objects: one per line, or the items of one array. A field that an object
/// lacks reads as null.
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
    let items = array_items(&data, &scan.display_path)?;
    let ranges = match &items {
        Some(items) => split_items(&data, items.clone(), PIECE_BYTES),
        None => split_lines(&data, PIECE_BYTES),
    };
    let array = items.is_some();
    let pieces = ranges.into_iter().map(move |range| {
        let (data, shape, schema) = (data.clone(), shape.clone(), schema.clone());
        Box::new(move || {
            let each = if array { "item" } else { "line" };
            let fail = |err: arrow::error::ArrowError| {
                json_error(&err.to_string(), &shape.file, &shape.read, each)
            };
            // The reader takes objects one after another, so in a copy of the items of an
            // array the commas between them are blanked.
            let text: std::borrow::Cow<[u8]> = match array {
                true => {
                    let mut text = data[range].to_vec();
                    let mut commas = Vec::new();
                    outer_commas(&text, |comma| commas.push(comma));
                    commas.into_iter().for_each(|comma| text[comma] = b' ');
                    // An item that is not an object, such as a number, ends at a space.
                    text.push(b'\n');
                    text.into()
                }
                false => (&data[range]).into(),
            };
            let reader = arrow::json::ReaderBuilder::new(schema)
                .with_batch_size(BATCH_ROWS)
                .build(Cursor::new(text))
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
            csv_error(message, "sales.csv", &|row| 10 + row, &names, &read, true).0,
            "sales.csv, line 13: cannot read 'north' as an int for column `region`"
        );
        let message = "Parser error: Error parsing column 0 at line 2: Parser error: \
                       Error parsing timestamp from 'soon': error parsing date";
        assert_eq!(
            csv_error(message, "sales.csv", &|row| 10 + row, &names, &read, true).0,
            "sales.csv, line 12: cannot read 'soon' as a datetime for column `date`"
        );
        let message = "Csv error: incorrect number of fields for line 2, expected 2 got 3";
        assert_eq!(
            csv_error(message, "sales.csv", &|row| 10 + row, &names, &read, true).0,
            "sales.csv, line 11: the row has 3 fields, but the header has 2"
        );
        assert_eq!(
            csv_error(
                "something else",
                "sales.csv",
                &|row| 1 + row,
                &names,
                &read,
                true
            )
            .0,
            "sales.csv: something else"
        );
        assert_eq!(
            json_error(
                "Json error: expected { got [1, 2]",
                "sales.json",
                &read,
                "line"
            )
            .0,
            "sales.json: every line must be one JSON object, but one is [1, 2]"
        );
        let message = "Json error: whilst decoding field 'region': failed to parse \"x\" as Int64";
        assert_eq!(
            json_error(message, "sales.json", &read, "line").0,
            "sales.json: cannot read \"x\" as an int for field `region`"
        );
    }

    #[test]
    fn reads_header_names() {
        assert_eq!(
            header_names(b"a,b c,\"d,e\",\"f\"\"g\"\r\n", b','),
            ["a", "b c", "d,e", "f\"g"]
        );
        assert_eq!(
            header_names("\u{feff}id,ชื่อ\n".as_bytes(), b','),
            ["id", "ชื่อ"]
        );
        assert_eq!(header_names(b"a,b\t\"c\td\"\n", b'\t'), ["a,b", "c\td"]);
    }
}
