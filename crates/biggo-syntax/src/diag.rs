use std::path::Path;

use crate::span::Span;

/// A path as messages and results show it: with `/` between its parts on every system, so
/// that a program reports and prints the same wherever it runs.
pub fn shown_path(path: impl AsRef<Path>) -> String {
    let path = path.as_ref().to_string_lossy();
    match std::path::MAIN_SEPARATOR {
        '/' => path.into_owned(),
        separator => path.replace(separator, "/"),
    }
}

/// A compile error attached to a source location.
#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    pub span: Span,
    pub message: String,
}

impl Diagnostic {
    pub fn new(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
        }
    }
}

/// A source file with the line table needed to render diagnostics.
pub struct SourceFile<'a> {
    name: &'a str,
    text: &'a str,
    line_starts: Vec<usize>,
}

impl<'a> SourceFile<'a> {
    pub fn new(name: &'a str, text: &'a str) -> Self {
        let line_starts = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(i, _)| i + 1))
            .collect();
        Self {
            name,
            text,
            line_starts,
        }
    }

    /// Formats `diag` with its source line and a caret underline. A span that covers several
    /// lines is underlined on its first line only.
    pub fn render(&self, diag: &Diagnostic) -> String {
        let start = (diag.span.start as usize).min(self.text.len());
        let line = self.line_starts.partition_point(|&s| s <= start) - 1;
        let line_start = self.line_starts[line];
        let line_text = self.text[line_start..].lines().next().unwrap_or("");
        let line_end = line_start + line_text.len();
        let start = start.min(line_end);
        let end = (diag.span.end as usize).clamp(start, line_end);

        let before = &self.text[line_start..start];
        // Keep tabs so the carets line up however the terminal expands them.
        let pad: String = before
            .chars()
            .map(|c| if c == '\t' { '\t' } else { ' ' })
            .collect();
        let carets = "^".repeat(self.text[start..end].chars().count().max(1));
        let number = (line + 1).to_string();
        let gutter = " ".repeat(number.len());
        format!(
            "error: {message}\n\
             {gutter}--> {name}:{number}:{column}\n\
             {gutter} |\n\
             {number} | {line_text}\n\
             {gutter} | {pad}{carets}\n",
            message = diag.message,
            name = self.name,
            column = before.chars().count() + 1,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_source_line_with_carets() {
        let text = "let x = 1\n\tlet total = (1 +\n";
        let file = SourceFile::new("demo.bgo", text);
        let diag = Diagnostic::new(Span::new(15, 20), "boom");
        assert_eq!(
            file.render(&diag),
            "error: boom\n --> demo.bgo:2:6\n  |\n2 | \tlet total = (1 +\n  | \t    ^^^^^\n"
        );
    }

    #[test]
    fn paths_are_shown_with_slashes_on_every_system() {
        let path: std::path::PathBuf = ["data", "logs", "2026-01.csv"].iter().collect();
        assert_eq!(shown_path(&path), "data/logs/2026-01.csv");
        assert_eq!(shown_path("lib/rates.bgo"), "lib/rates.bgo");
    }

    #[test]
    fn renders_position_at_end_of_file() {
        let file = SourceFile::new("demo.bgo", "let x =");
        let diag = Diagnostic::new(Span::new(7, 7), "boom");
        assert_eq!(
            file.render(&diag),
            "error: boom\n --> demo.bgo:1:8\n  |\n1 | let x =\n  |        ^\n"
        );
    }
}
