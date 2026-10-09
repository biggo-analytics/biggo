//! Reads and writes Excel workbooks. A sheet is small next to the files the other readers
//! are made for, and its cells come in no order that could be cut into pieces, so a sheet is
//! read whole, on one core.

use std::path::Path;
use std::sync::Arc;

use arrow::array::{
    Array, ArrayRef, AsArray, Float64Array, RecordBatch, StringArray, StringBuilder,
};
use arrow::datatypes::{
    DataType as ArrowType, Date32Type, Decimal128Type, DurationMicrosecondType, Float64Type,
    Int64Type, TimestampMicrosecondType,
};
use biggo_plan::{DataType, Field, Plan, Scan, scalar, shown_path};
use calamine::{Data, Range, Reader, open_workbook_auto};
use rust_xlsxwriter::{Format, Workbook};

use crate::scan::{BATCH_ROWS, Piece, Shape};
use crate::{Draft, Error, Result, execute, optimize};

/// The rows of a sheet, and so the rows of a table that fits in one with its header.
const SHEET_ROWS: usize = 1_048_576;

/// The days from the day Excel counts from, 30 December 1899, to 1 January 1970.
const EPOCH_DAYS: f64 = 25_569.0;

const DAY_SECONDS: f64 = 86_400.0;

/// The whole numbers that Excel, which keeps every number as a float, holds exactly.
const EXACT: i64 = 1 << 53;

/// The row and the column of a cell that is named as Excel names it, counted from 0: `B7` is
/// the second column of the seventh row.
fn cell_at(name: &str) -> Option<(u32, u32)> {
    let digits = name.find(|c: char| c.is_ascii_digit())?;
    let (letters, row) = name.split_at(digits);
    if letters.is_empty() || letters.len() > 3 {
        return None;
    }
    let mut column = 0u32;
    for letter in letters.chars() {
        let place = u32::from(letter.to_ascii_uppercase()).checked_sub(u32::from('A'))?;
        column = column * 26 + (place < 26).then_some(place + 1)?;
    }
    let row: u32 = row.parse().ok().filter(|&row| row > 0)?;
    Some((row - 1, column - 1))
}

/// The two corners of a range written as `B3:F200`.
fn corners(range: &str) -> Result<((u32, u32), (u32, u32))> {
    let parsed = range
        .split_once(':')
        .and_then(|(from, to)| Some((cell_at(from.trim())?, cell_at(to.trim())?)));
    match parsed {
        Some((from, to)) if from.0 <= to.0 && from.1 <= to.1 => Ok((from, to)),
        _ => Err(Error(format!(
            "`range` names the first and the last cell of a block, as in \"B3:F200\"; \
             found {range:?}"
        ))),
    }
}

/// The cells of the sheet that a scan reads, within the range it names if it names one.
/// `shown` is the name of the file in messages.
pub(crate) fn open(
    path: &Path,
    shown: &str,
    sheet: Option<&str>,
    range: Option<&str>,
) -> Result<(String, Range<Data>)> {
    // Opened first as any file, for the same message as the other readers give.
    crate::open(path, shown)?;
    let mut workbook = open_workbook_auto(path).map_err(|err| match err {
        calamine::Error::Msg(_) => Error(format!(
            "{shown} is not a workbook by its name; those that can be read are \
             .xlsx, .xlsm, .xlsb, .xls and .ods"
        )),
        err => Error(format!("cannot read {shown} as a workbook: {err}")),
    })?;
    let names = workbook.sheet_names();
    let name = match sheet {
        Some(sheet) if names.iter().any(|name| name == sheet) => sheet.to_string(),
        Some(sheet) => {
            return Err(Error(format!(
                "{shown} has no sheet {sheet:?}; its sheets are {}",
                names.join(", ")
            )));
        }
        None => match names.first() {
            Some(first) => first.clone(),
            None => return Err(Error(format!("{shown} has no sheets"))),
        },
    };
    let cells = workbook
        .worksheet_range(&name)
        .map_err(|err| Error(format!("cannot read sheet {name:?} of {shown}: {err}")))?;
    let cells = match range {
        Some(range) => {
            let (from, to) = corners(range)?;
            cells.range(from, to)
        }
        None => cells,
    };
    Ok((name, cells))
}

/// A date or a time of a cell as text: a date alone when the time is midnight.
fn moment(cell: &calamine::ExcelDateTime) -> Option<String> {
    let moment = cell.as_datetime()?;
    let day = moment.format("%Y-%m-%d");
    let time = moment.format("%H:%M:%S%.f").to_string();
    Some(match time == "00:00:00" {
        true => day.to_string(),
        false => format!("{day}T{time}"),
    })
}

/// A number as the text that reads back as the same number: whole numbers without `.0`.
fn number(value: f64) -> String {
    match value.fract() == 0.0 && value.abs() < 1e15 {
        true => format!("{value:.0}"),
        false => value.to_string(),
    }
}

/// The text of a cell, as the text of the same value in a CSV file would be. `None` for a
/// cell with nothing in it, or with an error such as `#N/A`.
pub(crate) fn text(cell: &Data) -> Option<String> {
    match cell {
        Data::Empty | Data::Error(_) => None,
        Data::String(text) => Some(text.clone()),
        Data::Int(value) => Some(value.to_string()),
        Data::Float(value) => Some(number(*value)),
        Data::Bool(value) => Some(value.to_string()),
        // A length of time is a number of seconds, as it is in the other formats.
        Data::DateTime(value) if value.is_duration() => Some(number(value.as_f64() * DAY_SECONDS)),
        Data::DateTime(value) => moment(value).or_else(|| Some(number(value.as_f64()))),
        Data::DateTimeIso(text) | Data::DurationIso(text) => Some(text.clone()),
    }
}

/// The seconds of a cell that holds a length of time: a number of them, or a time of day
/// or a duration as Excel keeps it, in days.
fn seconds(cell: &Data) -> Result<Option<f64>> {
    Ok(match cell {
        Data::Empty | Data::Error(_) => None,
        Data::Int(value) => Some(*value as f64),
        Data::Float(value) => Some(*value),
        Data::DateTime(value) => Some(value.as_f64() * DAY_SECONDS),
        other => {
            let text = text(other).unwrap_or_default();
            let parsed = text.trim().parse::<f64>();
            Some(parsed.map_err(|_| Error(format!("cannot read '{text}' as a duration")))?)
        }
    })
}

/// The names of the columns from the cells of a header. The spaces around a name, which
/// cannot be seen in a sheet, are not part of it.
pub(crate) fn header(cells: &[Data]) -> Vec<String> {
    let name = |cell: &Data| text(cell).unwrap_or_default().trim().to_string();
    cells.iter().map(name).collect()
}

/// The rows of a sheet that hold something, after the first `skip` of all its rows.
pub(crate) fn rows(cells: &Range<Data>, skip: usize) -> impl Iterator<Item = &[Data]> {
    let blank = |row: &&[Data]| row.iter().all(|cell| matches!(cell, Data::Empty));
    cells.rows().skip(skip).filter(move |row| !blank(row))
}

pub(crate) fn pieces(scan: &Scan, shape: &Arc<Shape>) -> Result<Vec<Piece>> {
    let (scan, shape) = (scan.clone(), shape.clone());
    let piece = move || {
        let options = &scan.read;
        let (sheet, cells) = open(
            &scan.path,
            &shape.file,
            options.sheet.as_deref(),
            options.range.as_deref(),
        )?;
        let mut rows = rows(&cells, options.skip);
        let width = cells.width();
        let names: Vec<String> = match options.header {
            true => match rows.next() {
                Some(first) => header(first),
                None => {
                    return Err(Error(format!(
                        "sheet {sheet:?} of {} has no rows to read",
                        shape.file
                    )));
                }
            },
            // Without a header the columns are the declared ones, in their order.
            false => {
                let declared = scan.declared.fields.iter();
                let in_file =
                    declared.filter(|field| options.file_column.as_deref() != Some(&*field.name));
                let names: Vec<String> = in_file.map(|field| field.name.to_string()).collect();
                if width < names.len() {
                    return Err(Error(format!(
                        "sheet {sheet:?} of {} has {width} columns, but its row type has {}",
                        shape.file,
                        names.len()
                    )));
                }
                names
            }
        };
        let mut places = Vec::with_capacity(shape.read.fields.len());
        for field in &shape.read.fields {
            let place = names.iter().position(|name| *name == *field.name);
            places.push(place.ok_or_else(|| shape.missing_column(&field.name, &names))?);
        }

        let rows: Vec<&[Data]> = rows.collect();
        let mut batches = Vec::new();
        for rows in rows.chunks(BATCH_ROWS) {
            let columns = shape
                .read
                .fields
                .iter()
                .zip(&places)
                .map(|(field, &place)| {
                    let cells = rows
                        .iter()
                        .map(|row| row.get(place).unwrap_or(&Data::Empty));
                    let column = column(field, cells, &options.nulls).map_err(|err| {
                        Error(format!(
                            "column `{}` of {}: {}",
                            field.name, shape.file, err.0
                        ))
                    })?;
                    Ok((&*field.name, column, true))
                });
            let columns: Result<Vec<_>> = columns.collect();
            let batch = RecordBatch::try_from_iter_with_nullable(columns?)?;
            batches.push(shape.finish(&batch)?);
        }
        Ok(batches)
    };
    Ok(vec![Box::new(piece)])
}

/// The cells of one column as an array that `Shape::finish` turns into the declared type:
/// seconds for a length of time, and for every other type the text of the cells.
fn column<'c>(
    field: &Field,
    cells: impl Iterator<Item = &'c Data>,
    nulls: &[Arc<str>],
) -> Result<ArrayRef> {
    if field.ty.dtype == DataType::Duration {
        let seconds: Result<Float64Array> = cells.map(seconds).collect();
        return Ok(Arc::new(seconds?));
    }
    // In a column of strings that cannot be null, a text that means null is text.
    let kept = field.ty.dtype == DataType::Str && !field.ty.nullable;
    let mut texts = StringBuilder::new();
    for cell in cells {
        let text = text(cell).filter(|text| {
            let null = text.is_empty() || nulls.iter().any(|null| **null == **text);
            kept || !null
        });
        // Text around a number or a date is typed by hand, and often has a space after it.
        match (text, field.ty.dtype) {
            (Some(text), DataType::Str) => texts.append_value(text),
            (Some(text), _) => texts.append_value(text.trim()),
            (None, _) => texts.append_null(),
        }
    }
    Ok(Arc::new(texts.finish()))
}

/// Writes a table as a workbook with one sheet, with the names of the columns in its
/// first row.
pub fn write_excel(plan: &Arc<Plan>, path: &Path, sheet: &str) -> Result<()> {
    let refuse = |err: rust_xlsxwriter::XlsxError| {
        Error(format!("cannot write {}: {err}", shown_path(path)))
    };
    let schema = plan.schema();
    let mut workbook = Workbook::new();
    let page = workbook.add_worksheet();
    page.set_name(sheet).map_err(|_| {
        Error(format!(
            "a sheet cannot be named {sheet:?}: a name has 1 to 31 characters, \
             and none of [ ] : * ? / \\"
        ))
    })?;
    let bold = Format::new().set_bold();
    let day = Format::new().set_num_format("yyyy-mm-dd");
    let moment = Format::new().set_num_format("yyyy-mm-dd hh:mm:ss");
    for (place, field) in schema.fields.iter().enumerate() {
        page.write_string_with_format(0, place as u16, &*field.name, &bold)
            .map_err(refuse)?;
    }

    let mut row = 1usize;
    for batch in execute(&optimize(plan))? {
        let batch = batch?;
        if row + batch.num_rows() > SHEET_ROWS {
            return Err(Error(format!(
                "a sheet holds {} rows under its header, and the table has more",
                SHEET_ROWS - 1
            )));
        }
        for (place, column) in batch.columns().iter().enumerate() {
            let place = place as u16;
            for index in (0..column.len()).filter(|&index| column.is_valid(index)) {
                let at = (row + index) as u32;
                match column.data_type() {
                    ArrowType::Int64 => {
                        let value = column.as_primitive::<Int64Type>().value(index);
                        // A number too large to be kept exactly is written as text.
                        match value.abs() <= EXACT {
                            true => page.write_number(at, place, value as f64),
                            false => page.write_string(at, place, value.to_string()),
                        }
                    }
                    ArrowType::Float64 => {
                        let value = column.as_primitive::<Float64Type>().value(index);
                        // A sheet has no cell for a number that is not finite.
                        match value.is_finite() {
                            true => page.write_number(at, place, value),
                            false => page.write_string(at, place, value.to_string()),
                        }
                    }
                    ArrowType::Boolean => {
                        page.write_boolean(at, place, column.as_boolean().value(index))
                    }
                    ArrowType::Date32 => {
                        let days = column.as_primitive::<Date32Type>().value(index);
                        let serial = f64::from(days) + EPOCH_DAYS;
                        page.write_number_with_format(at, place, serial, &day)
                    }
                    ArrowType::Timestamp(..) => {
                        let micros = column
                            .as_primitive::<TimestampMicrosecondType>()
                            .value(index);
                        let serial = micros as f64 / (DAY_SECONDS * 1e6) + EPOCH_DAYS;
                        page.write_number_with_format(at, place, serial, &moment)
                    }
                    ArrowType::Duration(_) => {
                        let micros = column
                            .as_primitive::<DurationMicrosecondType>()
                            .value(index);
                        page.write_number(at, place, micros as f64 / 1e6)
                    }
                    ArrowType::Decimal128(..) => {
                        let value = column.as_primitive::<Decimal128Type>().value(index);
                        let scale = 10f64.powi(scalar::DECIMAL_SCALE as i32);
                        page.write_number(at, place, value as f64 / scale)
                    }
                    _ => {
                        let strings: &StringArray = column.as_string();
                        page.write_string(at, place, strings.value(index))
                    }
                }
                .map_err(refuse)?;
            }
        }
        row += batch.num_rows();
    }
    let (file, draft) = Draft::create(path)?;
    workbook.save_to_writer(file).map_err(refuse)?;
    draft.keep()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_have_names() {
        let cells = [
            ("A1", (0, 0)),
            ("B7", (6, 1)),
            ("b7", (6, 1)),
            ("AA10", (9, 26)),
            ("ZZ1", (0, 701)),
            ("XFD1048576", (1_048_575, 16_383)),
        ];
        for (name, place) in cells {
            assert_eq!(cell_at(name), Some(place), "{name}");
        }
        for bad in ["", "7", "B", "B0", "7B", "ABCD1", "B-1", "é1"] {
            assert_eq!(cell_at(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_range_has_two_corners() {
        assert_eq!(corners("B3:F200").unwrap(), ((2, 1), (199, 5)));
        assert_eq!(corners(" a1 : a1 ").unwrap(), ((0, 0), (0, 0)));
        for bad in ["B3", "F200:B3", "B3:", "B:F", "B3:F200:G1"] {
            assert!(corners(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn numbers_are_written_as_they_read() {
        assert_eq!(number(12.0), "12");
        assert_eq!(number(-3.0), "-3");
        assert_eq!(number(0.1), "0.1");
        assert_eq!(number(1e20), "100000000000000000000");
        assert_eq!(text(&Data::Float(7.0)).as_deref(), Some("7"));
        assert_eq!(text(&Data::Bool(true)).as_deref(), Some("true"));
        assert_eq!(text(&Data::Empty), None);
    }
}
