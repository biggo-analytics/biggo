//! The reference entry of a built-in function, for `biggo help <name>` and `:help <name>` in
//! the REPL. The entries are the rows of the tables in `docs/06-builtins.md`, which is built
//! into the executable, so the terminal and the documentation say the same thing.

const REFERENCE: &str = include_str!("../../../docs/06-builtins.md");

const MORE: &str = "https://github.com/biggo-analytics/biggo/blob/main/docs/06-builtins.md";

/// The cells of a line that is a row of a table.
fn cells(line: &str) -> Option<Vec<String>> {
    let row = line.trim().strip_prefix('|')?.strip_suffix('|')?;
    let mut cells = vec![String::new()];
    let mut chars = row.chars().peekable();
    while let Some(ch) = chars.next() {
        let cell = cells.last_mut().expect("there is always a cell being read");
        match ch {
            // A bar that belongs to the text of a cell is written `\|`.
            '\\' if chars.peek() == Some(&'|') => {
                cell.push('|');
                chars.next();
            }
            '|' => cells.push(String::new()),
            _ => cell.push(ch),
        }
    }
    Some(cells.iter().map(|cell| cell.trim().to_string()).collect())
}

/// The functions that the first cell of a row describes, as in "`trim(s)`, `trim(s, chars)`".
fn names(signatures: &str) -> impl Iterator<Item = &str> {
    let code = signatures.split('`').skip(1).step_by(2);
    code.filter_map(|code| {
        let word = |ch: char| ch.is_ascii_alphanumeric() || ch == '_';
        let end = code.find(|ch| !word(ch)).unwrap_or(code.len());
        // Not an operator such as `a ?? b`, nor an index such as `xs[i]`.
        let call = matches!(code[end..].chars().next(), None | Some('(' | '<'));
        (end > 0 && call).then(|| &code[..end])
    })
}

/// The rows of the reference that describe functions: the section each is in, the headings
/// of its table, and its cells.
fn rows() -> impl Iterator<Item = (&'static str, Vec<String>, Vec<String>)> {
    let mut section = "";
    let mut headings: Vec<String> = Vec::new();
    REFERENCE.lines().filter_map(move |line| {
        if let Some(title) = line.strip_prefix("## ").or(line.strip_prefix("### ")) {
            section = title;
        }
        let Some(row) = cells(line) else {
            headings.clear();
            return None;
        };
        if headings.is_empty() {
            headings = row;
            return None;
        }
        let described = names(&row[0]).next().is_some();
        described.then(|| (section, headings.clone(), row))
    })
}

/// What the reference says about the built-in function `name`, as text for a terminal.
/// `None` if there is no such function.
pub fn entry(name: &str) -> Option<String> {
    let mut text = String::new();
    let mut last = "";
    for (section, headings, row) in rows() {
        if !names(&row[0]).any(|found| found == name) {
            continue;
        }
        if section != last {
            text.push_str(&format!("{section}:\n"));
            last = section;
        }
        text.push_str(&format!("  {}\n", row[0].replace('`', "")));
        for (heading, cell) in headings.iter().zip(&row).skip(1) {
            match heading.as_str() {
                "Takes" | "Returns" => {
                    text.push_str(&format!("    {heading}: {}\n", cell.replace('`', "")));
                }
                _ => text.push_str(&format!("    {cell}\n")),
            }
        }
    }
    if text.is_empty() {
        return None;
    }
    text.push_str(&format!("\nThe full reference, with examples: {MORE}\n"));
    Some(text)
}

/// What to say when `name` is not a built-in function: the names that are close to it.
pub fn unknown(name: &str) -> String {
    let mut close: Vec<String> = Vec::new();
    for (_, _, row) in rows() {
        for found in names(&row[0]) {
            // The same start, or one name inside the other.
            let start = name.len().min(found.len()).min(3);
            let alike = found.contains(name)
                || name.contains(found)
                || (start == 3 && found.as_bytes()[..3] == name.as_bytes()[..3]);
            if alike && !close.iter().any(|name| name == found) {
                close.push(found.to_string());
            }
        }
    }
    close.sort_unstable();
    match close.is_empty() {
        true => format!("there is no built-in function `{name}`"),
        false => format!(
            "there is no built-in function `{name}`; close to it: {}",
            close.join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_split_into_cells() {
        let row = cells("| `a(x)` | `int` | one \\| two |").unwrap();
        assert_eq!(row, ["`a(x)`", "`int`", "one | two"]);
        assert_eq!(cells("not a row"), None);
    }

    #[test]
    fn a_cell_names_the_functions_it_describes() {
        let found: Vec<&str> = names("`trim(s)`, `trim(s, chars)`").collect();
        assert_eq!(found, ["trim", "trim"]);
        assert_eq!(
            names("`read_csv<T>(path)`").collect::<Vec<_>>(),
            ["read_csv"]
        );
        assert_eq!(names("`a ?? b`, `xs[i]`, `%Y`").count(), 0);
    }

    #[test]
    fn an_entry_has_the_signature_and_the_meaning() {
        let text = entry("substring").unwrap();
        let first = "string:\n  substring(s, start), substring(s, start, length)\n";
        assert!(text.starts_with(first), "{text}");
        assert!(text.contains("    Returns: string\n"), "{text}");
        // A name with several meanings has one entry for each.
        let text = entry("sum").unwrap();
        assert!(
            text.contains("Aggregate:\n") && text.contains("Lists and maps:\n"),
            "{text}"
        );
        assert_eq!(entry("nonesuch"), None);
    }

    #[test]
    fn an_unknown_name_gets_the_names_close_to_it() {
        assert!(unknown("substr").contains("substring"));
        assert_eq!(unknown("zzzz"), "there is no built-in function `zzzz`");
    }
}
