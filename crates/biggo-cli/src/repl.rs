use std::io::{self, BufRead, IsTerminal, Write};

use biggo_eval::Session;

const NAME: &str = "<repl>";

/// Reads entries from standard input and runs each one, printing its value. Definitions
/// stay available to later entries, and an error only ends the entry it happens in.
///
/// An entry is one line, or several when the line stops midway, as with an open `(` or `{`.
/// Prompts are shown only on a terminal, so piped input gives clean output.
pub fn repl() -> u8 {
    let stdin = io::stdin();
    let interactive = stdin.is_terminal();
    let mut session = Session::new(io::stdout());
    if interactive {
        let version = env!("CARGO_PKG_VERSION");
        println!("biggo {version} (Ctrl-D to exit)");
    }

    let mut lines = stdin.lock().lines();
    let mut entry = String::new();
    loop {
        if interactive {
            print!("{}", if entry.is_empty() { ">> " } else { ".. " });
            let _ = io::stdout().flush();
        }
        let line = match lines.next() {
            Some(Ok(line)) => Some(line),
            Some(Err(err)) => {
                eprintln!("biggo: cannot read input: {err}");
                return 1;
            }
            None => None,
        };
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
            if let Err(failure) = session.run(NAME, &entry, true) {
                // Once standard output is gone there is no one left to talk to.
                if failure.io().is_some() {
                    return 0;
                }
                eprint!("{}", failure.render());
            }
        }
        entry.clear();
        if line.is_none() {
            if interactive {
                println!();
            }
            return 0;
        }
    }
}
