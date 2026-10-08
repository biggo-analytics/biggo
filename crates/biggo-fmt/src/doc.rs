//! A document is text with the places where a line may break. Rendering lays a group out on
//! one line if it fits in the page width and otherwise breaks all of its line breaks, group by
//! group from the outside in. This is the design of Wadler's "A prettier printer".

pub enum Doc {
    Text(String),
    /// A space, or a line break if the enclosing group is broken.
    Line,
    /// Nothing, or a line break if the enclosing group is broken.
    SoftLine,
    /// A line break, which also breaks every enclosing group.
    HardLine,
    /// A line break that leaves an empty line, breaking every enclosing group.
    BlankLine,
    /// Text for the end of the current line, wherever that turns out to be: a comment.
    LineSuffix(String),
    /// Text that only appears if the enclosing group is broken.
    IfBroken(&'static str),
    /// Breaks every enclosing group.
    BreakParent,
    Concat(Vec<Doc>),
    /// Indents the line breaks inside by one level.
    Indent(Box<Doc>),
    /// A unit that is either laid out on one line or broken.
    Group(Box<Doc>),
}

const INDENT: usize = 2;

pub fn text(text: impl Into<String>) -> Doc {
    Doc::Text(text.into())
}

pub fn concat(docs: impl IntoIterator<Item = Doc>) -> Doc {
    Doc::Concat(docs.into_iter().collect())
}

pub fn indent(doc: Doc) -> Doc {
    Doc::Indent(Box::new(doc))
}

pub fn group(doc: Doc) -> Doc {
    Doc::Group(Box::new(doc))
}

/// Puts `separator` between the docs.
pub fn join(docs: impl IntoIterator<Item = Doc>, separator: impl Fn() -> Doc) -> Doc {
    let mut joined = Vec::new();
    for doc in docs {
        if !joined.is_empty() {
            joined.push(separator());
        }
        joined.push(doc);
    }
    Doc::Concat(joined)
}

impl Doc {
    /// Whether the doc has a break that its enclosing groups cannot lay out flat.
    fn forces_break(&self) -> bool {
        match self {
            Doc::HardLine | Doc::BlankLine | Doc::LineSuffix(_) | Doc::BreakParent => true,
            Doc::Text(_) | Doc::Line | Doc::SoftLine | Doc::IfBroken(_) => false,
            Doc::Concat(docs) => docs.iter().any(Doc::forces_break),
            Doc::Indent(doc) | Doc::Group(doc) => doc.forces_break(),
        }
    }

    /// The width of the doc on one line, up to `limit`; `None` if it is wider.
    fn flat_width(&self, limit: usize) -> Option<usize> {
        Some(match self {
            Doc::Text(text) => text.chars().count(),
            Doc::Line => 1,
            Doc::SoftLine | Doc::IfBroken(_) | Doc::BreakParent | Doc::LineSuffix(_) => 0,
            Doc::HardLine | Doc::BlankLine => return None,
            Doc::Concat(docs) => {
                let mut width = 0;
                for doc in docs {
                    width += doc.flat_width(limit.checked_sub(width)?)?;
                }
                width
            }
            Doc::Indent(doc) | Doc::Group(doc) => doc.flat_width(limit)?,
        })
        .filter(|width| *width <= limit)
    }
}

struct Printer {
    out: String,
    width: usize,
    column: usize,
    /// Comments waiting for the end of the line.
    suffixes: Vec<String>,
}

impl Printer {
    fn newline(&mut self, level: usize) {
        for suffix in self.suffixes.drain(..) {
            self.out.push_str(&suffix);
        }
        // An indented line that stays empty would end in spaces; add them only with text.
        self.out.push('\n');
        self.column = level * INDENT;
    }

    fn write(&mut self, text: &str) {
        if self.out.ends_with('\n') || self.out.is_empty() {
            self.out.push_str(&" ".repeat(self.column));
        }
        self.out.push_str(text);
        self.column += text.chars().count();
    }

    fn print(&mut self, doc: &Doc, level: usize, broken: bool) {
        match doc {
            Doc::Text(text) => self.write(text),
            Doc::Line if !broken => self.write(" "),
            Doc::SoftLine if !broken => {}
            Doc::Line | Doc::SoftLine | Doc::HardLine => self.newline(level),
            Doc::BlankLine => {
                self.newline(level);
                self.newline(level);
            }
            Doc::LineSuffix(text) => self.suffixes.push(text.clone()),
            Doc::IfBroken(text) => {
                if broken {
                    self.write(text);
                }
            }
            Doc::BreakParent => {}
            Doc::Concat(docs) => docs.iter().for_each(|doc| self.print(doc, level, broken)),
            Doc::Indent(doc) => self.print(doc, level + 1, broken),
            Doc::Group(doc) => {
                let room = self.width.saturating_sub(self.column);
                let flat = !doc.forces_break() && doc.flat_width(room).is_some();
                self.print(doc, level, !flat);
            }
        }
    }
}

/// Lays `doc` out in lines of at most `width` columns where its breaks allow it.
pub fn render(doc: &Doc, width: usize) -> String {
    let mut printer = Printer {
        out: String::new(),
        width,
        column: 0,
        suffixes: Vec::new(),
    };
    printer.print(doc, 0, true);
    printer.newline(0);
    printer.out
}
