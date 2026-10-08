//! Works out the columns of a file from what is in it, so that the row type a program needs
//! can be written for the person instead of by them.

use std::fs::File;
use std::ops::Range;
use std::path::Path;

use arrow::datatypes::DataType as ArrowType;
use biggo_plan::dates::make_date;
use biggo_plan::{ColType, DataType, Field, Format};
use encoding_rs::{Encoding, UTF_16BE, UTF_16LE};
use memmap2::Mmap;
use parquet::arrow::arrow_reader::{ArrowReaderMetadata, ArrowReaderOptions};

use crate::scan::{array_items, header_names, line_end, outer_commas, row_end};
use crate::{Error, Result};

/// The rows that the types of a text file are worked out from.
pub const SAMPLE_ROWS: usize = 10_000;

/// The bytes of a JSON array that are looked through for its first items.
const SAMPLE_BYTES: usize = 16 << 20;

/// What is known of a file before it is looked at.
#[derive(Default)]
pub struct Known {
    /// For a CSV file: its delimiter, if it is not to be found out.
    pub delimiter: Option<u8>,
    /// For a CSV file: its encoding, when it is not UTF-8.
    pub encoding: Option<&'static Encoding>,
    /// For a CSV file: the lines before the header.
    pub skip: usize,
    /// For a CSV file: that its first line is a row, not the names of the columns.
    pub no_header: bool,
    /// For a database: the table.
    pub table: Option<String>,
}

/// The columns of a file, as far as they could be worked out.
#[derive(Debug)]
pub struct Guess {
    pub format: Format,
    pub fields: Vec<Field>,
    /// For a CSV file: its delimiter.
    pub delimiter: u8,
    /// For a CSV file: its encoding, if the file itself says it is not UTF-8.
    pub encoding: Option<&'static Encoding>,
    /// The rows that were looked at, if that may not be all of them.
    pub sampled: Option<usize>,
    /// What the reader of the result should know: columns left out, and columns whose
    /// values could be read as more than the type they were given.
    pub notes: Vec<String>,
}

/// What the values of a column seen so far can all be read as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// No value yet.
    Unknown,
    Bool,
    Int,
    Float,
    Date,
    DateTime,
    Str,
}

impl Kind {
    /// What values of both kinds can be read as.
    fn with(self, other: Kind) -> Kind {
        use Kind::*;
        match (self, other) {
            (Unknown, kind) | (kind, Unknown) => kind,
            (a, b) if a == b => a,
            (Int, Float) | (Float, Int) => Float,
            (Date, DateTime) | (DateTime, Date) => DateTime,
            _ => Str,
        }
    }

    fn dtype(self) -> DataType {
        match self {
            Kind::Bool => DataType::Bool,
            Kind::Int => DataType::Int,
            Kind::Float => DataType::Float,
            Kind::Date => DataType::Date,
            Kind::DateTime => DataType::DateTime,
            Kind::Unknown | Kind::Str => DataType::Str,
        }
    }
}

/// The digits of `text` as a number, if it is nothing but `count` digits.
fn digits(text: &str, count: usize) -> Option<i64> {
    let plain = text.len() == count && text.bytes().all(|byte| byte.is_ascii_digit());
    plain.then(|| text.parse().ok()).flatten()
}

fn is_date(text: &str) -> bool {
    let mut parts = text.splitn(3, '-');
    let (Some(year), Some(month), Some(day)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    match (digits(year, 4), digits(month, 2), digits(day, 2)) {
        (Some(year), Some(month), Some(day)) => make_date(year, month, day).is_some(),
        _ => false,
    }
}

/// A date, then `T` or a space, then a time with seconds and maybe a fraction of them.
fn is_datetime(text: &str) -> bool {
    let Some((date, time)) = text.split_once(['T', ' ']) else {
        return false;
    };
    let (time, fraction) = time.split_once('.').unwrap_or((time, "0"));
    let mut parts = time.splitn(3, ':');
    let (Some(hour), Some(minute), Some(second)) = (parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let in_range = |part: &str, below: i64| digits(part, 2).is_some_and(|value| value < below);
    let fraction = (1..=6).contains(&fraction.len()) && digits(fraction, fraction.len()).is_some();
    is_date(date) && in_range(hour, 24) && in_range(minute, 60) && in_range(second, 60) && fraction
}

/// What a text of a file can be read as. A number that starts with a zero, as a postal code
/// or a telephone number does, stays text: as a number it would lose the zero.
fn kind_of(text: &str) -> Kind {
    if text.eq_ignore_ascii_case("true") || text.eq_ignore_ascii_case("false") {
        return Kind::Bool;
    }
    let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
    let whole = unsigned.split(['.', 'e', 'E']).next().unwrap_or("");
    let padded = whole.len() > 1 && whole.starts_with('0');
    if !unsigned.is_empty() && unsigned.bytes().all(|byte| byte.is_ascii_digit()) {
        return match text.parse::<i64>() {
            Ok(_) if !padded => Kind::Int,
            _ => Kind::Str,
        };
    }
    let numeric =
        |byte: u8| byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b'.' | b'e' | b'E');
    let number = unsigned.starts_with(|first: char| first.is_ascii_digit() || first == '.')
        && text.bytes().all(numeric)
        && text.parse::<f64>().is_ok();
    if (number && !padded) || matches!(text, "NaN" | "inf" | "-inf") {
        return Kind::Float;
    }
    if is_date(text) {
        return Kind::Date;
    }
    if is_datetime(text) {
        return Kind::DateTime;
    }
    Kind::Str
}

/// What is known of one column while its values are looked at.
#[derive(Clone)]
struct Column {
    name: String,
    kind: Kind,
    missing: bool,
    /// A value that made the column text, to show what is in it.
    example: Option<String>,
    /// Whether every value so far is a date written with slashes, dots or dashes between a
    /// day, a month and a year of four digits; and the largest of each part.
    day_first: Option<[i64; 3]>,
}

impl Column {
    fn new(name: String) -> Self {
        Column {
            name,
            kind: Kind::Unknown,
            missing: false,
            example: None,
            day_first: Some([0; 3]),
        }
    }

    /// Takes note of a value, which can be read as `kind`.
    fn see(&mut self, text: &str, kind: Kind) {
        self.kind = self.kind.with(kind);
        if self.kind == Kind::Str && self.example.is_none() {
            self.example = Some(text.to_string());
        }
        self.day_first = self.day_first.and_then(|largest| {
            let parts: Vec<&str> = text.split(['/', '.', '-']).collect();
            let [first, second, year] = parts[..] else {
                return None;
            };
            let part = |text: &str| {
                let short = (1..=2).contains(&text.len());
                short.then(|| digits(text, text.len())).flatten()
            };
            let (first, second, year) = (part(first)?, part(second)?, digits(year, 4)?);
            Some([
                largest[0].max(first),
                largest[1].max(second),
                largest[2].max(year),
            ])
        });
    }

    fn missing(&mut self) {
        self.missing = true;
    }

    /// How a column of text that holds dates written another way can be read as dates.
    fn date_hint(&self) -> Option<String> {
        let [first, second, year] = self.day_first.filter(|_| self.kind == Kind::Str)?;
        let example = self.example.as_deref()?;
        let separator = example.chars().find(|c| !c.is_ascii_digit())?;
        // The part that goes above 12 is the day. When neither does, the day comes first,
        // as it does in most of the world.
        let order = match (first > 12, second > 12) {
            (false, true) => ["%m", "%d"],
            _ => ["%d", "%m"],
        };
        let pattern = format!("{}{separator}{}{separator}%Y", order[0], order[1]);
        // A year far ahead of today is counted in the Buddhist era.
        let era = if year > 2400 {
            ", era = \"buddhist\""
        } else {
            ""
        };
        let name = biggo_plan::Name(&self.name);
        Some(format!(
            "`{}` holds dates written like {example}: \
             parse_date({name}, \"{pattern}\"{era}) reads them",
            self.name
        ))
    }

    fn field(&self) -> Field {
        let nullable = self.missing || self.kind == Kind::Unknown;
        Field::new(&*self.name, ColType::new(self.kind.dtype(), nullable))
    }
}

/// The fields of the columns that can be declared, and notes on those that cannot: a row
/// type cannot have a column without a name, or two of one name.
fn declare(columns: &[Column], notes: &mut Vec<String>) -> Vec<Field> {
    let mut fields: Vec<Field> = Vec::new();
    for (index, column) in columns.iter().enumerate() {
        if column.name.is_empty() {
            notes.push(format!("column {} has no name, and is left out", index + 1));
        } else if fields.iter().any(|field| *field.name == *column.name) {
            notes.push(format!(
                "column {} is the second named `{}`, and is left out",
                index + 1,
                column.name
            ));
        } else {
            fields.push(column.field());
            notes.extend(column.date_hint());
        }
    }
    fields
}

fn map(path: &Path, shown: &str) -> Result<Mmap> {
    let file = File::open(path).map_err(|err| Error(format!("cannot open {shown}: {err}")))?;
    if file.metadata().map(|meta| meta.len() == 0).unwrap_or(false) {
        return Err(Error(format!("{shown} is empty")));
    }
    // SAFETY: as when a query reads the file, the mapping is only read.
    unsafe { Mmap::map(&file) }.map_err(|err| Error(format!("cannot read {shown}: {err}")))
}

/// The delimiter that cuts the first rows of a file into the same number of fields, and
/// into the most of them.
fn find_delimiter(rows: &[&[u8]]) -> u8 {
    let fields = |delimiter: u8| {
        let mut counts = rows.iter().map(|row| header_names(row, delimiter).len());
        let first = counts.next().unwrap_or(1);
        counts.all(|count| count == first).then_some(first)
    };
    let found = b",;\t|"
        .iter()
        .copied()
        .filter_map(|delimiter| Some((fields(delimiter)?, delimiter)))
        // The first of those with the most fields.
        .rev()
        .max_by_key(|(count, _)| *count);
    match found {
        Some((count, delimiter)) if count > 1 => delimiter,
        _ => b',',
    }
}

fn infer_csv(path: &Path, shown: &str, known: &Known) -> Result<Guess> {
    let mapped = map(path, shown)?;
    // A file that starts with the mark of UTF-16 says so itself.
    let marked = match mapped.get(..2) {
        Some([0xff, 0xfe]) => Some(UTF_16LE),
        Some([0xfe, 0xff]) => Some(UTF_16BE),
        _ => None,
    };
    let encoding = known.encoding.or(marked);
    let decoded;
    let data: &[u8] = match encoding {
        Some(encoding) => {
            let (text, _, malformed) = encoding.decode(&mapped);
            if malformed {
                let name = encoding.name().to_ascii_lowercase();
                return Err(Error(format!("{shown} is not {name} text")));
            }
            decoded = text.into_owned();
            decoded.as_bytes()
        }
        None => &mapped,
    };

    let mut start = 0;
    for _ in 0..known.skip {
        start = line_end(data, start);
    }
    let limit = SAMPLE_ROWS + usize::from(!known.no_header);
    let mut rows: Vec<Range<usize>> = Vec::new();
    while start < data.len() && rows.len() < limit {
        let end = row_end(data, start, false);
        rows.push(start..end);
        start = end;
    }
    let whole = start == data.len();
    if std::str::from_utf8(&data[..start]).is_err() {
        return Err(Error(format!(
            "{shown} is not UTF-8 text; if it is in another encoding, name it, \
             as in `--encoding tis-620`"
        )));
    }
    let Some(first) = rows.first() else {
        return Err(Error(format!(
            "{shown} has nothing after the {} lines that `--skip` passes over",
            known.skip
        )));
    };
    let delimiter = match known.delimiter {
        Some(delimiter) => delimiter,
        None => {
            let first: Vec<&[u8]> = rows.iter().take(20).map(|row| &data[row.clone()]).collect();
            find_delimiter(&first)
        }
    };
    let first = header_names(&data[first.clone()], delimiter);
    let (names, body) = match known.no_header {
        true => {
            let numbered = (1..=first.len()).map(|number| format!("column_{number}"));
            (numbered.collect(), &rows[..])
        }
        false => (first, &rows[1..]),
    };
    let mut columns: Vec<Column> = names.into_iter().map(Column::new).collect();
    for row in body {
        let values = header_names(&data[row.clone()], delimiter);
        // A line with nothing on it, as at the end of a file, is not a row.
        if values.len() == 1 && values[0].is_empty() && columns.len() > 1 {
            continue;
        }
        if values.len() != columns.len() {
            let line = data[..row.start].iter().filter(|&&byte| byte == b'\n');
            return Err(Error(format!(
                "{shown}, line {}: the row has {} fields, but the first row has {}; \
                 if the delimiter is not {:?}, name it with `--delimiter`",
                line.count() + 1,
                values.len(),
                columns.len(),
                char::from(delimiter)
            )));
        }
        for (column, value) in columns.iter_mut().zip(&values) {
            match value.is_empty() {
                true => column.missing(),
                false => column.see(value, kind_of(value)),
            }
        }
    }
    let mut notes = Vec::new();
    let fields = declare(&columns, &mut notes);
    Ok(Guess {
        format: Format::Csv,
        fields,
        delimiter,
        encoding: marked.filter(|_| known.encoding.is_none()),
        sampled: (!whole).then_some(SAMPLE_ROWS),
        notes,
    })
}

fn infer_json(path: &Path, shown: &str) -> Result<Guess> {
    let data = map(path, shown)?;
    let (items, whole): (Vec<&[u8]>, bool) = match array_items(&data, shown)? {
        Some(items) => {
            let looked = items.start..items.end.min(items.start + SAMPLE_BYTES);
            let text = &data[looked.clone()];
            let mut cuts = vec![0];
            outer_commas(text, |comma| cuts.push(comma + 1));
            // What follows the last comma is a whole item only if the array was looked
            // through to its end.
            let whole = looked.end == items.end;
            if whole {
                cuts.push(text.len() + 1);
            }
            let items = cuts.windows(2).map(|pair| &text[pair[0]..pair[1] - 1]);
            let items: Vec<&[u8]> = items.take(SAMPLE_ROWS + 1).collect();
            let whole = whole && items.len() <= SAMPLE_ROWS;
            (items, whole)
        }
        None => {
            let lines = data.split(|&byte| byte == b'\n');
            let lines: Vec<&[u8]> = lines.take(SAMPLE_ROWS + 1).collect();
            let whole = lines.len() <= SAMPLE_ROWS;
            (lines, whole)
        }
    };
    let mut columns: Vec<Column> = Vec::new();
    let mut nested: Vec<String> = Vec::new();
    let mut rows = 0;
    for item in items.iter().take(SAMPLE_ROWS) {
        if item.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let object: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(item)
            .map_err(|_| {
                let start: String = String::from_utf8_lossy(item).chars().take(60).collect();
                Error(format!(
                    "{shown}: every row must be one JSON object, but one is {}",
                    start.trim()
                ))
            })?;
        for (name, value) in &object {
            let at = match columns.iter().position(|column| column.name == *name) {
                Some(at) => at,
                None => {
                    let mut column = Column::new(name.clone());
                    // The objects before this one do not have the field.
                    column.missing = rows > 0;
                    columns.push(column);
                    columns.len() - 1
                }
            };
            let column = &mut columns[at];
            use serde_json::Value;
            match value {
                Value::Null => column.missing(),
                Value::Bool(_) => column.kind = column.kind.with(Kind::Bool),
                Value::Number(number) if number.is_i64() => {
                    column.kind = column.kind.with(Kind::Int);
                }
                Value::Number(_) => column.kind = column.kind.with(Kind::Float),
                // In JSON a number in quotes was written as text, and stays text.
                Value::String(text) => match kind_of(text) {
                    kind @ (Kind::Date | Kind::DateTime) => column.see(text, kind),
                    _ => column.see(text, Kind::Str),
                },
                Value::Array(_) | Value::Object(_) => {
                    if !nested.contains(name) {
                        nested.push(name.clone());
                    }
                }
            }
        }
        for column in &mut columns {
            if !object.contains_key(&column.name) {
                column.missing();
            }
        }
        rows += 1;
    }
    let mut notes = Vec::new();
    columns.retain(|column| !nested.contains(&column.name));
    for name in nested {
        notes.push(format!(
            "`{name}` holds lists or objects, which a column cannot, and is left out"
        ));
    }
    let fields = declare(&columns, &mut notes);
    Ok(Guess {
        format: Format::Json,
        fields,
        delimiter: b',',
        encoding: None,
        sampled: (!whole).then_some(SAMPLE_ROWS),
        notes,
    })
}

fn infer_parquet(path: &Path, shown: &str) -> Result<Guess> {
    let file = File::open(path).map_err(|err| Error(format!("cannot open {shown}: {err}")))?;
    let metadata = ArrowReaderMetadata::load(&file, ArrowReaderOptions::new())
        .map_err(|err| Error(format!("{shown} is not a Parquet file: {err}")))?;
    let mut notes = Vec::new();
    let mut fields = Vec::new();
    for field in metadata.schema().fields() {
        use ArrowType::*;
        let dtype = match field.data_type() {
            Int8 | Int16 | Int32 | Int64 | UInt8 | UInt16 | UInt32 | UInt64 => DataType::Int,
            Float16 | Float32 | Float64 => DataType::Float,
            Boolean => DataType::Bool,
            Utf8 | LargeUtf8 | Utf8View => DataType::Str,
            Date32 | Date64 => DataType::Date,
            Timestamp(..) => DataType::DateTime,
            Duration(_) => DataType::Duration,
            Decimal128(..) | Decimal256(..) => DataType::Decimal,
            other => {
                notes.push(format!(
                    "`{}` is of the Parquet type {other}, which a column cannot hold, \
                     and is left out",
                    field.name()
                ));
                continue;
            }
        };
        let ty = ColType::new(dtype, field.is_nullable());
        fields.push(Field::new(field.name().as_str(), ty));
    }
    Ok(Guess {
        format: Format::Parquet,
        fields,
        delimiter: b',',
        encoding: None,
        sampled: None,
        notes,
    })
}

/// The type that a column of a database is declared as, by the rules SQLite itself uses to
/// read a declaration, with the names of dates and truth values added.
fn declared_type(declared: &str) -> Option<DataType> {
    let declared = declared.to_ascii_uppercase();
    let has = |part: &str| declared.contains(part);
    Some(if has("INT") {
        DataType::Int
    } else if has("CHAR") || has("CLOB") || has("TEXT") {
        DataType::Str
    } else if has("REAL") || has("FLOA") || has("DOUB") {
        DataType::Float
    } else if has("BOOL") {
        DataType::Bool
    } else if has("DATETIME") || has("TIMESTAMP") {
        DataType::DateTime
    } else if has("DATE") {
        DataType::Date
    } else if has("DEC") || has("NUMERIC") {
        DataType::Decimal
    } else {
        return None;
    })
}

fn infer_sqlite(path: &Path, shown: &str, known: &Known) -> Result<Guess> {
    let fail = |err: rusqlite::Error| Error(format!("{shown}: {err}"));
    if !path.exists() {
        return Err(Error(format!("cannot open {shown}: there is no such file")));
    }
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY;
    let connection = rusqlite::Connection::open_with_flags(path, flags).map_err(fail)?;
    let tables: Vec<String> = {
        let query = "select name from sqlite_master \
                     where type in ('table', 'view') and name not like 'sqlite_%' order by name";
        let mut statement = connection.prepare(query).map_err(fail)?;
        let names = statement.query_map([], |row| row.get(0)).map_err(fail)?;
        names.collect::<rusqlite::Result<_>>().map_err(fail)?
    };
    let listed = match tables.is_empty() {
        true => "it has no tables".to_string(),
        false => format!("its tables are {}", tables.join(", ")),
    };
    let Some(table) = &known.table else {
        return Err(Error(format!(
            "a database needs the name of a table after it; in {shown}, {listed}"
        )));
    };
    if !tables.contains(table) {
        return Err(Error(format!("{shown} has no table `{table}`; {listed}")));
    }
    let quoted = table.replace('"', "\"\"");
    let mut columns: Vec<(String, Option<DataType>, bool)> = {
        let query = format!("pragma table_info(\"{quoted}\")");
        let mut statement = connection.prepare(&query).map_err(fail)?;
        let rows = statement
            .query_map([], |row| {
                let (name, declared): (String, String) = (row.get(1)?, row.get(2)?);
                let (required, key): (bool, i64) = (row.get(3)?, row.get(5)?);
                Ok((name, declared_type(&declared), !required && key == 0))
            })
            .map_err(fail)?;
        rows.collect::<rusqlite::Result<_>>().map_err(fail)?
    };
    // A column declared with no type, or one that is not known, goes by what it holds.
    let query = format!("select * from \"{quoted}\" limit {SAMPLE_ROWS}");
    let mut statement = connection.prepare(&query).map_err(fail)?;
    let mut held: Vec<Kind> = vec![Kind::Unknown; columns.len()];
    let mut rows = statement.query([]).map_err(fail)?;
    while let Some(row) = rows.next().map_err(fail)? {
        for (index, kind) in held.iter_mut().enumerate() {
            use rusqlite::types::ValueRef;
            let found = match row.get_ref(index).map_err(fail)? {
                ValueRef::Null => Kind::Unknown,
                ValueRef::Integer(_) => Kind::Int,
                ValueRef::Real(_) => Kind::Float,
                ValueRef::Text(text) => match std::str::from_utf8(text).map(kind_of) {
                    Ok(kind @ (Kind::Date | Kind::DateTime)) => kind,
                    _ => Kind::Str,
                },
                ValueRef::Blob(_) => Kind::Str,
            };
            *kind = kind.with(found);
        }
    }
    let fields = columns
        .drain(..)
        .zip(held)
        .map(|((name, declared, nullable), held)| {
            let dtype = match (declared, held) {
                // A database has no type for dates: they are kept as text.
                (Some(DataType::Str), Kind::Date | Kind::DateTime) => held.dtype(),
                (Some(declared), _) => declared,
                (None, held) => held.dtype(),
            };
            Field::new(&*name, ColType::new(dtype, nullable))
        });
    Ok(Guess {
        format: Format::Sqlite,
        fields: fields.collect(),
        delimiter: b',',
        encoding: None,
        sampled: None,
        notes: Vec::new(),
    })
}

/// The format that the name of a file says it is in.
pub fn format_of(path: &Path) -> Option<Format> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match extension.as_str() {
        "csv" | "tsv" | "txt" => Format::Csv,
        "parquet" | "pq" => Format::Parquet,
        "json" | "jsonl" | "ndjson" => Format::Json,
        "db" | "sqlite" | "sqlite3" => Format::Sqlite,
        _ => return None,
    })
}

/// Works out the columns of the file at `path`, which messages call `shown`.
pub fn infer(path: &Path, shown: &str, known: &Known) -> Result<Guess> {
    let Some(format) = format_of(path) else {
        return Err(Error(format!(
            "cannot tell what kind of file {shown} is from its name; the kinds are known by \
             .csv, .tsv, .txt, .json, .jsonl, .ndjson, .parquet, .db, .sqlite and .sqlite3"
        )));
    };
    let guess = match format {
        Format::Csv => infer_csv(path, shown, known)?,
        Format::Json => infer_json(path, shown)?,
        Format::Parquet => infer_parquet(path, shown)?,
        Format::Sqlite => infer_sqlite(path, shown, known)?,
    };
    if guess.fields.is_empty() {
        return Err(Error(format!("{shown} has no column that can be declared")));
    }
    Ok(guess)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_text_is_read_as_the_narrowest_type_that_holds_it() {
        let kinds = [
            ("12", Kind::Int),
            ("-7", Kind::Int),
            ("0", Kind::Int),
            ("1.5", Kind::Float),
            ("-0.25", Kind::Float),
            ("1e6", Kind::Float),
            (".5", Kind::Float),
            ("NaN", Kind::Float),
            ("true", Kind::Bool),
            ("FALSE", Kind::Bool),
            ("2026-01-31", Kind::Date),
            ("2026-01-31T08:30:00", Kind::DateTime),
            ("2026-01-31 08:30:00.25", Kind::DateTime),
            // Numbers that would lose a zero, or do not fit, and dates that do not exist.
            ("007", Kind::Str),
            ("0812345678", Kind::Str),
            ("00.5", Kind::Str),
            ("123456789012345678901", Kind::Str),
            ("2026-02-30", Kind::Str),
            ("2026-01-31T25:00:00", Kind::Str),
            ("31/01/2026", Kind::Str),
            ("1,000", Kind::Str),
            ("-", Kind::Str),
            ("e5", Kind::Str),
            ("north", Kind::Str),
        ];
        for (text, kind) in kinds {
            assert_eq!(kind_of(text), kind, "{text}");
        }
    }

    #[test]
    fn kinds_widen_as_values_are_seen() {
        assert_eq!(Kind::Unknown.with(Kind::Int), Kind::Int);
        assert_eq!(Kind::Int.with(Kind::Float), Kind::Float);
        assert_eq!(Kind::Date.with(Kind::DateTime), Kind::DateTime);
        assert_eq!(Kind::Int.with(Kind::Date), Kind::Str);
        assert_eq!(Kind::Bool.with(Kind::Int), Kind::Str);
    }

    #[test]
    fn the_delimiter_is_the_one_that_gives_even_rows() {
        let rows = |text: &'static str| text.split('\n').map(str::as_bytes).collect::<Vec<_>>();
        assert_eq!(find_delimiter(&rows("a,b,c\n1,2,3")), b',');
        assert_eq!(find_delimiter(&rows("a;b;c\n1,5;2;3")), b';');
        assert_eq!(find_delimiter(&rows("a\tb\n1\t2")), b'\t');
        assert_eq!(find_delimiter(&rows("a|b\n\"x,y\"|2")), b'|');
        assert_eq!(find_delimiter(&rows("only\none")), b',');
    }

    #[test]
    fn dates_written_another_way_get_a_hint() {
        let mut column = Column::new("paid on".to_string());
        ["05/01/2569", "31/12/2568"]
            .iter()
            .for_each(|text| column.see(text, kind_of(text)));
        let hint = column.date_hint().unwrap();
        let reads = "parse_date(`paid on`, \"%d/%m/%Y\", era = \"buddhist\") reads them";
        assert!(hint.ends_with(reads), "{hint}");

        let mut column = Column::new("day".to_string());
        ["1.25.2026", "12.31.2025"]
            .iter()
            .for_each(|text| column.see(text, kind_of(text)));
        assert!(column.date_hint().unwrap().contains("\"%m.%d.%Y\")"));

        let mut column = Column::new("code".to_string());
        ["05/01/2569", "n/a"]
            .iter()
            .for_each(|text| column.see(text, kind_of(text)));
        assert_eq!(column.date_hint(), None);
    }
}
