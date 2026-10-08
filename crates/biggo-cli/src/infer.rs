//! `biggo infer`: prints the row type of a file, worked out from what is in it, and the
//! call that reads the file with it, to copy into a program.

use std::path::Path;

use biggo_exec::infer::{Guess, Known, infer};
use biggo_plan::{CsvOptions, Format, Name};

const USAGE: &str = "\
usage: biggo infer <file> [<table>] [--delimiter <d>] [--encoding <e>] [--skip <n>] [--no-header]

  <table>           for a SQLite database, the table to describe; for a workbook, the sheet
  --delimiter <d>   the delimiter of a CSV file, when it is not to be found out
  --encoding <e>    the encoding of a CSV file that is not UTF-8, such as tis-620
  --skip <n>        the lines before the header of a CSV file or a sheet
  --no-header       the first line is a row, not the names of the columns
";

/// What the command line asks for: the file, and what is known of it.
fn request<'a>(args: &[&'a str]) -> Result<(&'a str, Known), String> {
    let mut known = Known::default();
    let mut names = Vec::new();
    let mut args = args.iter().copied();
    while let Some(arg) = args.next() {
        let mut value = |what: &str| args.next().ok_or(format!("`{arg}` needs {what} after it"));
        match arg {
            "--delimiter" => {
                // A tab is hard to type on a command line.
                let delimiter = match value("a delimiter")? {
                    "\\t" | "tab" => "\t",
                    other => other,
                };
                known.delimiter = Some(CsvOptions::delimiter(delimiter)?);
            }
            "--encoding" => known.encoding = CsvOptions::encoding(value("an encoding")?)?,
            "--skip" => {
                let lines = value("a number of lines")?;
                let lines = lines.parse();
                known.skip = lines.map_err(|_| "`--skip` needs a number of lines after it")?;
            }
            "--no-header" => known.no_header = true,
            option if option.starts_with("--") => return Err(format!("unknown option `{option}`")),
            name => names.push(name),
        }
    }
    match names[..] {
        [file] => Ok((file, known)),
        [file, table] => {
            known.table = Some(table.to_string());
            Ok((file, known))
        }
        _ => Err("expected one file".to_string()),
    }
}

/// The words of a file name: `2026 sales-north.csv` has `2026`, `sales` and `north`.
fn words(path: &str) -> Vec<String> {
    let stem = Path::new(path).file_stem().and_then(|stem| stem.to_str());
    let words = stem.unwrap_or("").split(|c: char| !c.is_alphanumeric());
    words
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// A name for the row type and one for the table, from the name of the file or of the
/// table of a database: `Sales` and `sales`.
fn names(source: &str) -> (String, String) {
    let words = words(source);
    let starts_plain = words
        .first()
        .and_then(|word| word.chars().next())
        .is_some_and(char::is_alphabetic);
    if !starts_plain {
        return ("Row".to_string(), "data".to_string());
    }
    let capital = |word: &String| {
        let mut letters = word.chars();
        let first = letters
            .next()
            .map(|first| first.to_uppercase().collect::<String>());
        first.unwrap_or_default() + letters.as_str()
    };
    let ty: String = words.iter().map(capital).collect();
    // A table named as a keyword or a built-in function would get in its way.
    let table = words.join("_");
    let taken = Name(&table).to_string() != table || crate::help::entry(&table).is_some();
    match taken {
        true => (ty, "data".to_string()),
        false => (ty, table),
    }
}

/// The program text for a guess: notes, the row type, and the call that reads the file.
fn render(file: &str, known: &Known, guess: &Guess) -> String {
    let source = known.table.as_deref().unwrap_or(file);
    let (ty, table) = names(source);
    let mut text = String::new();
    if let Some(rows) = guess.sampled {
        text.push_str(&format!(
            "// The types are those of the first {rows} rows; later rows may not fit them.\n"
        ));
    }
    for note in &guess.notes {
        text.push_str(&format!("// {note}\n"));
    }
    if !guess.sheets.is_empty() {
        let sheets: Vec<String> = guess
            .sheets
            .iter()
            .map(|sheet| format!("{sheet:?}"))
            .collect();
        text.push_str(&format!(
            "// The other sheets of the workbook: {}\n",
            sheets.join(", ")
        ));
    }
    text.push_str(&format!("type {ty} = {{\n"));
    for field in &guess.fields {
        text.push_str(&format!("  {}: {},\n", Name(&field.name), field.ty));
    }
    text.push_str("}\n\n");

    let mut call = format!("{file:?}");
    match guess.format {
        Format::Csv => {
            if guess.delimiter != b',' {
                call.push_str(&format!(
                    ", delimiter = {:?}",
                    char::from(guess.delimiter).to_string()
                ));
            }
            if let Some(encoding) = known.encoding.or(guess.encoding) {
                let name = encoding.name().to_ascii_lowercase();
                call.push_str(&format!(", encoding = {name:?}"));
            }
            if known.no_header {
                call.push_str(", header = false");
            }
            if known.skip > 0 {
                call.push_str(&format!(", skip = {}", known.skip));
            }
        }
        Format::Sqlite => call.push_str(&format!(", {:?}", format!("select * from {source}"))),
        Format::Excel => {
            if let Some(sheet) = &known.table {
                call.push_str(&format!(", sheet = {sheet:?}"));
            }
            if known.no_header {
                call.push_str(", header = false");
            }
            if known.skip > 0 {
                call.push_str(&format!(", skip = {}", known.skip));
            }
        }
        Format::Json | Format::Parquet | Format::Postgres | Format::Mysql => {}
    }
    let read = match guess.format {
        Format::Excel => "read_excel",
        Format::Csv => "read_csv",
        Format::Json => "read_json",
        Format::Parquet => "read_parquet",
        Format::Sqlite | Format::Postgres | Format::Mysql => "read_sql",
    };
    text.push_str(&format!("let {table} = {read}<{ty}>({call})\n"));
    text
}

pub fn command(args: &[&str]) -> u8 {
    let (file, known) = match request(args) {
        Ok(request) => request,
        Err(message) => {
            eprintln!("biggo infer: {message}\n\n{USAGE}");
            return 2;
        }
    };
    match infer(Path::new(file), file, &known) {
        Ok(guess) => {
            print!("{}", render(file, &known, &guess));
            0
        }
        Err(err) => {
            eprintln!("biggo infer: {}", err.0);
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_come_from_the_file() {
        let named = |path| names(path);
        assert_eq!(named("data/sales.csv"), ("Sales".into(), "sales".into()));
        assert_eq!(
            named("monthly-sales 2026.tsv"),
            ("MonthlySales2026".into(), "monthly_sales_2026".into())
        );
        assert_eq!(named("2026.csv"), ("Row".into(), "data".into()));
        // `count` and `type` mean something already.
        assert_eq!(named("type.csv"), ("Type".into(), "data".into()));
        assert_eq!(named("count.csv"), ("Count".into(), "data".into()));
        assert_eq!(named("ยอดขาย.csv"), ("ยอดขาย".into(), "ยอดขาย".into()));
    }

    #[test]
    fn the_command_line_says_what_is_known() {
        let (file, known) =
            request(&["a.csv", "--delimiter", "tab", "--skip", "2", "--no-header"]).unwrap();
        assert_eq!(
            (file, known.delimiter, known.skip, known.no_header),
            ("a.csv", Some(b'\t'), 2, true)
        );
        let (file, known) = request(&["shop.db", "orders"]).unwrap();
        assert_eq!((file, known.table.as_deref()), ("shop.db", Some("orders")));
        assert_eq!(request(&[]).err().unwrap(), "expected one file");
        assert_eq!(
            request(&["a.csv", "--skip"]).err().unwrap(),
            "`--skip` needs a number of lines after it"
        );
        assert_eq!(
            request(&["a.csv", "--wat"]).err().unwrap(),
            "unknown option `--wat`"
        );
        assert!(request(&["a.csv", "--encoding", "klingon"]).is_err());
    }
}
