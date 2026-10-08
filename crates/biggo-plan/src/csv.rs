//! How a CSV file is laid out: what separates its fields, and how its text is encoded. And
//! what else a call that reads a file can say about it.

use std::fmt;
use std::sync::Arc;

use encoding_rs::{Encoding, UTF_8};

use crate::schema::Name;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CsvOptions {
    /// The byte between the fields of a row.
    pub delimiter: u8,
    /// The encoding of the file, when it is not UTF-8.
    pub encoding: Option<&'static Encoding>,
}

impl Default for CsvOptions {
    fn default() -> Self {
        CsvOptions {
            delimiter: b',',
            encoding: None,
        }
    }
}

impl CsvOptions {
    /// The delimiter that `text` names. It has to be one byte that cannot be taken for part
    /// of a field: the readers split a file into rows before they look for fields.
    pub fn delimiter(text: &str) -> Result<u8, String> {
        match text.as_bytes() {
            [byte] if byte.is_ascii() && !matches!(byte, b'"' | b'\n' | b'\r') => Ok(*byte),
            _ => Err(format!(
                "the delimiter must be one ASCII character other than a quote or a line break, \
                 such as \",\", \";\" or \"\\t\"; found {text:?}"
            )),
        }
    }

    /// The encoding that `label` names, or `None` for UTF-8.
    pub fn encoding(label: &str) -> Result<Option<&'static Encoding>, String> {
        match Encoding::for_label(label.as_bytes()) {
            Some(encoding) if encoding == UTF_8 => Ok(None),
            Some(encoding) => Ok(Some(encoding)),
            None => Err(format!(
                "unknown encoding {label:?}; some that are known are \"utf-8\", \"tis-620\", \
                 \"windows-874\", \"windows-1252\" and \"utf-16le\""
            )),
        }
    }

    /// The encoding as a writer can use it. Text cannot be written as UTF-16.
    pub fn output_encoding(label: &str) -> Result<Option<&'static Encoding>, String> {
        let encoding = Self::encoding(label)?;
        match encoding {
            Some(encoding) if encoding.output_encoding() != encoding => Err(format!(
                "cannot write a file as {}; it can only be read",
                encoding.name().to_ascii_lowercase()
            )),
            _ => Ok(encoding),
        }
    }
}

/// What a call that reads a file says about it, beyond the delimiter and the encoding. All
/// but `file_column` are for CSV files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadOptions {
    /// Whether the first line names the columns. If not, the columns of the file are the
    /// declared ones, in the order they are declared.
    pub header: bool,
    /// The lines to pass over before the header: a title, a date, a blank line.
    pub skip: usize,
    /// The texts that mean null, besides an empty field.
    pub nulls: Vec<Arc<str>>,
    /// The declared column that is not read from the file, but holds the path of the file
    /// that each row came from.
    pub file_column: Option<Arc<str>>,
    /// For a workbook: the sheet, when it is not the first.
    pub sheet: Option<Arc<str>>,
    /// For a workbook: the block of cells that holds the table, as in `B3:F200`, when it is
    /// not all of the sheet.
    pub range: Option<Arc<str>>,
}

impl Default for ReadOptions {
    fn default() -> Self {
        ReadOptions {
            header: true,
            skip: 0,
            nulls: Vec::new(),
            file_column: None,
            sheet: None,
            range: None,
        }
    }
}

/// Shows the options that differ from the default, each with a space before it.
impl fmt::Display for ReadOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(sheet) = &self.sheet {
            write!(f, " sheet {sheet:?}")?;
        }
        if let Some(range) = &self.range {
            write!(f, " range {range}")?;
        }
        if !self.header {
            f.write_str(" no header")?;
        }
        if self.skip > 0 {
            write!(f, " skip {}", self.skip)?;
        }
        if !self.nulls.is_empty() {
            write!(f, " nulls {:?}", self.nulls)?;
        }
        if let Some(column) = &self.file_column {
            write!(f, " file name in {}", Name(column))?;
        }
        Ok(())
    }
}

/// Shows the options that differ from the default, each with a space before it.
impl fmt::Display for CsvOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.delimiter != b',' {
            write!(f, " delimiter {:?}", char::from(self.delimiter))?;
        }
        if let Some(encoding) = self.encoding {
            write!(f, " encoding {}", encoding.name().to_ascii_lowercase())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_delimiter_is_one_plain_character() {
        assert_eq!(CsvOptions::delimiter("\t"), Ok(b'\t'));
        assert_eq!(CsvOptions::delimiter(";"), Ok(b';'));
        for bad in ["", "ab", "\"", "\n", "é"] {
            assert!(CsvOptions::delimiter(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn encodings_go_by_their_usual_names() {
        assert_eq!(CsvOptions::encoding("UTF-8"), Ok(None));
        let thai = CsvOptions::encoding("tis-620").unwrap().unwrap();
        assert_eq!(thai.name(), "windows-874");
        assert_eq!(CsvOptions::encoding("TIS-620"), Ok(Some(thai)));
        assert_eq!(CsvOptions::encoding("windows-874"), Ok(Some(thai)));
        assert!(CsvOptions::encoding("klingon").is_err());
        assert!(CsvOptions::output_encoding("tis-620").is_ok());
        assert_eq!(
            CsvOptions::output_encoding("utf-16le").unwrap_err(),
            "cannot write a file as utf-16le; it can only be read"
        );
        let options = CsvOptions {
            delimiter: b'\t',
            encoding: Some(thai),
        };
        assert_eq!(options.to_string(), " delimiter '\\t' encoding windows-874");
        assert_eq!(CsvOptions::default().to_string(), "");
    }

    #[test]
    fn read_options_show_what_is_not_the_default() {
        assert_eq!(ReadOptions::default().to_string(), "");
        let options = ReadOptions {
            header: false,
            skip: 2,
            nulls: vec!["NA".into(), "-".into()],
            file_column: Some("from file".into()),
            sheet: Some("2026".into()),
            range: Some("B3:F9".into()),
        };
        let shown = " sheet \"2026\" range B3:F9 no header skip 2 nulls [\"NA\", \"-\"] \
                     file name in `from file`";
        assert_eq!(options.to_string(), shown);
    }
}
