//! Tables as text for people: Markdown for a document or a message, HTML for a page.

use std::path::Path;
use std::sync::Arc;

use arrow::array::{Array, AsArray};
use biggo_plan::{DataType, Plan, Schema, shown_path};

use crate::{Draft, Error, Result, execute, expr, optimize};

/// The rows of a table, each cell as text; a null is `None`. A float shows as it prints.
fn cells(plan: &Arc<Plan>) -> Result<Vec<Vec<Option<String>>>> {
    let mut rows = Vec::new();
    for batch in execute(&optimize(plan))? {
        let batch = batch?;
        let columns: Result<Vec<_>> = batch.columns().iter().map(expr::to_strings).collect();
        let columns = columns?;
        for row in 0..batch.num_rows() {
            let cell = |column: &arrow::array::ArrayRef| {
                let texts = column.as_string::<i32>();
                texts.is_valid(row).then(|| texts.value(row).to_string())
            };
            rows.push(columns.iter().map(cell).collect());
        }
    }
    Ok(rows)
}

/// Whether the values of a column are set to the right, as numbers are.
fn numeric(dtype: DataType) -> bool {
    matches!(dtype, DataType::Int | DataType::Float | DataType::Decimal)
}

/// Text as a cell of a Markdown table, where a bar would end the cell and a line break the
/// row.
fn markdown_cell(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace("\r\n", "<br>")
        .replace(['\n', '\r'], "<br>")
}

/// Runs `plan` and writes all its rows as a Markdown table. A null is an empty cell.
pub fn to_markdown(plan: &Arc<Plan>) -> Result<String> {
    let schema = plan.schema();
    let rows = cells(plan)?;
    let mut text = String::new();
    let row = |text: &mut String, cells: &mut dyn Iterator<Item = String>| {
        text.push('|');
        for cell in cells {
            text.push(' ');
            text.push_str(&cell);
            text.push_str(" |");
        }
        text.push('\n');
    };
    row(
        &mut text,
        &mut schema.names().map(|name| markdown_cell(name)),
    );
    let rules = schema
        .fields
        .iter()
        .map(|field| match numeric(field.ty.dtype) {
            true => "---:".to_string(),
            false => "---".to_string(),
        });
    row(&mut text, &mut { rules });
    for cells in &rows {
        let cells = cells
            .iter()
            .map(|cell| markdown_cell(cell.as_deref().unwrap_or("")));
        row(&mut text, &mut { cells });
    }
    Ok(text)
}

/// Text as it is written in HTML, where `<` and `&` would start something else.
fn html_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn html_table(schema: &Schema, rows: &[Vec<Option<String>>]) -> String {
    let side = |index: usize| match numeric(schema.fields[index].ty.dtype) {
        true => " class=\"n\"",
        false => "",
    };
    let mut text = String::from("<table>\n<thead>\n<tr>");
    for (index, name) in schema.names().enumerate() {
        text.push_str(&format!("<th{}>{}</th>", side(index), html_text(name)));
    }
    text.push_str("</tr>\n</thead>\n<tbody>\n");
    for cells in rows {
        text.push_str("<tr>");
        for (index, cell) in cells.iter().enumerate() {
            let cell = html_text(cell.as_deref().unwrap_or(""));
            text.push_str(&format!(
                "<td{}>{}</td>",
                side(index),
                cell.replace('\n', "<br>")
            ));
        }
        text.push_str("</tr>\n");
    }
    text.push_str("</tbody>\n</table>\n");
    text
}

/// Runs `plan` and writes all its rows as an HTML `<table>`, to put inside a page. Cells of
/// numbers have the class `n`. A null is an empty cell.
pub fn to_html(plan: &Arc<Plan>) -> Result<String> {
    Ok(html_table(&plan.schema(), &cells(plan)?))
}

/// How the page of `write_html` looks: a plain table that is easy to read, with numbers
/// set to the right in digits of one width.
const STYLE: &str = "\
body { font: 14px/1.4 system-ui, sans-serif; margin: 2rem; color: #1f2328; }
table { border-collapse: collapse; }
th, td { padding: 0.3rem 0.8rem; border-bottom: 1px solid #d8dee4; text-align: left; }
th { border-bottom: 2px solid #8c959f; }
.n { text-align: right; font-variant-numeric: tabular-nums; }
tbody tr:hover { background: #f6f8fa; }
";

fn write(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    let (mut file, draft) = Draft::create(path)?;
    file.write_all(text.as_bytes())
        .map_err(|err| Error(format!("cannot write {}: {err}", shown_path(path))))?;
    drop(file);
    draft.keep()
}

pub fn write_markdown(plan: &Arc<Plan>, path: &Path) -> Result<()> {
    write(path, &to_markdown(plan)?)
}

/// Writes a table as a page that a browser opens, titled with the name of the file.
pub fn write_html(plan: &Arc<Plan>, path: &Path) -> Result<()> {
    let title = path.file_stem().unwrap_or_default().to_string_lossy();
    let page = format!(
        "<!doctype html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n<title>{}</title>\n\
         <style>\n{STYLE}</style>\n</head>\n<body>\n{}</body>\n</html>\n",
        html_text(&title),
        to_html(plan)?
    );
    write(path, &page)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_made_safe_for_a_cell() {
        assert_eq!(markdown_cell("a|b"), "a\\|b");
        assert_eq!(markdown_cell("one\ntwo\r\nthree"), "one<br>two<br>three");
        assert_eq!(markdown_cell("back\\slash"), "back\\\\slash");
        assert_eq!(
            html_text("a < b & \"c\" > d"),
            "a &lt; b &amp; &quot;c&quot; &gt; d"
        );
    }

    #[test]
    fn numbers_are_set_to_the_right() {
        assert!(numeric(DataType::Int) && numeric(DataType::Decimal));
        assert!(!numeric(DataType::Str) && !numeric(DataType::Date));
    }
}
