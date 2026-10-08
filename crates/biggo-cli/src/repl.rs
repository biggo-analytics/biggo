use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;

use biggo_eval::Session;
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;

use crate::help;

const NAME: &str = "<repl>";

const COMMANDS: &str = "\
:help            this list
:help <name>     what a built-in function does
:type <expr>     the type of an expression, without running it; for a table, its columns
:quit            leave (also Ctrl-D)
";

/// What asking for a line gave.
enum Line {
    Text(String),
    /// Ctrl-C, which drops the entry being typed.
    Cancelled,
    End,
}

/// Where the lines of a session come from.
enum Input {
    /// A terminal, where a line can be edited and the arrow keys bring back earlier entries,
    /// those of earlier sessions too.
    Terminal(Box<DefaultEditor>),
    /// A pipe or a file. No prompt is shown, so piped input gives clean output.
    Piped(io::Lines<io::StdinLock<'static>>),
}

/// The file that keeps entries from one session to the next.
fn history_file() -> Option<PathBuf> {
    Some(std::env::home_dir()?.join(".biggo_history"))
}

impl Input {
    fn new() -> io::Result<Self> {
        let stdin = io::stdin();
        if !stdin.is_terminal() {
            return Ok(Input::Piped(stdin.lock().lines()));
        }
        let mut editor = DefaultEditor::new().map_err(io::Error::other)?;
        if let Some(file) = history_file() {
            // There is no file before the first session ends.
            let _ = editor.load_history(&file);
        }
        Ok(Input::Terminal(Box::new(editor)))
    }

    fn line(&mut self, prompt: &str) -> io::Result<Line> {
        match self {
            Input::Terminal(editor) => match editor.readline(prompt) {
                Ok(line) => Ok(Line::Text(line)),
                Err(ReadlineError::Interrupted) => Ok(Line::Cancelled),
                Err(ReadlineError::Eof) => Ok(Line::End),
                Err(ReadlineError::Io(err)) => Err(err),
                Err(err) => Err(io::Error::other(err)),
            },
            Input::Piped(lines) => match lines.next() {
                Some(line) => line.map(Line::Text),
                None => Ok(Line::End),
            },
        }
    }

    /// Keeps a finished entry for the arrow keys to bring back.
    fn remember(&mut self, entry: &str) {
        if let Input::Terminal(editor) = self {
            let _ = editor.add_history_entry(entry.trim_end());
        }
    }

    /// Saves the entries for the next session. A session is no worse for failing to.
    fn close(&mut self) {
        if let (Input::Terminal(editor), Some(file)) = (self, history_file()) {
            let _ = editor.save_history(&file);
        }
    }
}

/// Runs a line that starts with `:`, which is for the REPL itself and not biggo code.
/// Returns whether the session goes on.
fn command<W: Write>(session: &mut Session<W>, line: &str) -> bool {
    let (word, rest) = match line.split_once(char::is_whitespace) {
        Some((word, rest)) => (word, rest.trim()),
        None => (line, ""),
    };
    match (word, rest) {
        (":quit" | ":q", "") => return false,
        (":help", "") => print!("{COMMANDS}"),
        (":help", name) => match help::entry(name) {
            Some(entry) => print!("{entry}"),
            None => eprintln!("{}", help::unknown(name)),
        },
        (":type", "") => eprintln!("`:type` takes an expression, as in `:type 1 + 2`"),
        (":type", source) => match session.type_of(NAME, source) {
            Ok(Some(ty)) => println!("{ty}"),
            Ok(None) => eprintln!("`:type` takes one expression"),
            Err(errors) => eprintln!("{}", errors.render()),
        },
        _ => eprintln!("unknown command `{word}`; `:help` lists the commands"),
    }
    true
}

/// Reads entries from standard input and runs each one, printing its value. Definitions
/// stay available to later entries, and an error only ends the entry it happens in.
///
/// An entry is one line, or several when the line stops midway, as with an open `(` or `{`.
/// A line that starts with `:` is a command to the REPL; `:help` lists them.
pub fn repl() -> u8 {
    let mut input = match Input::new() {
        Ok(input) => input,
        Err(err) => {
            eprintln!("biggo: cannot read input: {err}");
            return 1;
        }
    };
    let mut session = Session::new(io::stdout());
    if matches!(input, Input::Terminal(_)) {
        let version = env!("CARGO_PKG_VERSION");
        println!("biggo {version} (:help for commands, Ctrl-D to exit)");
    }

    let mut entry = String::new();
    let status = loop {
        let prompt = if entry.is_empty() { ">> " } else { ".. " };
        let line = match input.line(prompt) {
            Ok(Line::Text(line)) => Some(line),
            Ok(Line::Cancelled) => {
                entry.clear();
                continue;
            }
            Ok(Line::End) => None,
            Err(err) => {
                eprintln!("biggo: cannot read input: {err}");
                break 1;
            }
        };
        if let Some(line) = line.as_deref().map(str::trim)
            && entry.is_empty()
            && line.starts_with(':')
        {
            input.remember(line);
            if !command(&mut session, line) {
                break 0;
            }
            continue;
        }
        // A blank line, or the end of input, submits an unfinished entry as it is.
        let may_continue = line.as_deref().is_some_and(|line| !line.trim().is_empty());
        if let Some(line) = &line {
            entry.push_str(line);
            entry.push('\n');
        }

        if !entry.trim().is_empty() {
            if may_continue && session.incomplete(&entry) {
                continue;
            }
            input.remember(&entry);
            if let Err(failure) = session.run(NAME, &entry, true) {
                // Once standard output is gone there is no one left to talk to.
                if failure.io().is_some() {
                    break 0;
                }
                eprint!("{}", failure.render());
            }
        }
        entry.clear();
        if line.is_none() {
            break 0;
        }
    };
    input.close();
    status
}
