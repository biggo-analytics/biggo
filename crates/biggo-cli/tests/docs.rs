//! The documentation is tested. Every program in `README.md` and `docs/*.md` is run, and
//! where the documentation shows what a program prints, that is compared with what it does
//! print. Set `BIGGO_BLESS=1` to write the real output into the documents instead.
//!
//! A program is a fenced block whose first word is `biggo`. More words after it say how
//! the block is treated:
//!
//! - `prelude`: the block is also run before every later program of the document.
//! - `continue`: the block goes on in the session of the program before it.
//! - `error`: the program must fail; without the word it must not.
//! - `check`: the program is type-checked but not run.
//! - `explain`: the program runs in explain mode, printing plans instead of results.
//! - `fragment`: the block is not a whole program and is left alone.
//!
//! A block marked `text output` right after a program holds what the program prints,
//! followed by its error if it ends in one. File paths in a program start from the
//! directory of its document.

use std::fs;
use std::path::{Path, PathBuf};

use biggo_eval::Session;

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

/// A fenced block of a document.
struct Block {
    /// The words after the opening fence.
    info: Vec<String>,
    /// The lines between the fences: where they start, and one past where they end.
    lines: (usize, usize),
    text: String,
}

fn blocks(document: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut open: Option<(Vec<String>, usize)> = None;
    for (number, line) in document.lines().enumerate() {
        let Some(info) = line.trim_start().strip_prefix("```") else {
            continue;
        };
        match open.take() {
            None => {
                let info = info.split_whitespace().map(str::to_string).collect();
                open = Some((info, number + 1));
            }
            Some((info, start)) => {
                let lines: Vec<&str> = document.lines().skip(start).take(number - start).collect();
                blocks.push(Block {
                    info,
                    lines: (start, number),
                    text: lines.join("\n") + "\n",
                });
            }
        }
    }
    assert!(open.is_none(), "a fenced block is not closed");
    blocks
}

fn session(dir: &Path, explain: bool) -> Session<Vec<u8>> {
    let mut session = Session::new(Vec::new());
    session.vm().set_base_dir(dir);
    session.vm().set_explain(explain);
    session
}

/// Runs the programs of one document. Returns what is wrong with it, and the text of the
/// document with the real output of every program written in.
fn check_document(path: &Path) -> (Vec<String>, String) {
    let name = path.strip_prefix(ROOT).unwrap().display().to_string();
    let dir = path.parent().unwrap();
    let document = fs::read_to_string(path).unwrap();
    let blocks = blocks(&document);
    let mut failures = Vec::new();
    // The new content of output blocks, by the lines they take up.
    let mut rewrites: Vec<((usize, usize), String)> = Vec::new();
    let mut prelude: Option<&str> = None;
    let mut previous: Option<Session<Vec<u8>>> = None;

    for (index, block) in blocks.iter().enumerate() {
        let has = |word: &str| block.info[1..].iter().any(|flag| flag == word);
        if block.info.first().map(String::as_str) != Some("biggo") || has("fragment") {
            continue;
        }
        let at = format!("{name}:{}", block.lines.0);
        let mut session = match (has("continue"), previous.take()) {
            (true, Some(session)) => session,
            (true, None) => panic!("{at}: no program to continue from"),
            (false, _) => {
                let mut session = session(dir, has("explain"));
                if let Some(prelude) = prelude
                    && let Err(failure) = session.run("prelude.bgo", prelude, false)
                {
                    failures.push(format!("{at}: the prelude fails:\n{}", failure.render()));
                }
                session.vm().output().clear();
                session
            }
        };
        if has("prelude") {
            prelude = Some(&block.text);
        }
        let failure = match has("check") {
            true => session
                .check("example.bgo", &block.text)
                .err()
                .map(|e| e.render()),
            false => session
                .run("example.bgo", &block.text, false)
                .err()
                .map(|f| f.render()),
        };
        let mut output = String::from_utf8(std::mem::take(session.vm().output())).unwrap();
        match (&failure, has("error")) {
            (Some(failure), false) => failures.push(format!("{at}: the program fails:\n{failure}")),
            (None, true) => failures.push(format!("{at}: the program should fail, but runs")),
            _ => {}
        }
        output.push_str(failure.as_deref().unwrap_or(""));
        previous = Some(session);

        let shown = blocks
            .get(index + 1)
            .filter(|next| next.info == ["text", "output"]);
        if let Some(shown) = shown {
            if shown.text.trim_end() != output.trim_end() {
                failures.push(format!(
                    "{at}: the output shown differs from the real output:\n{output}"
                ));
            }
            rewrites.push((shown.lines, output));
        }
    }

    let mut blessed = String::new();
    let mut lines = document.lines().enumerate();
    while let Some((number, line)) = lines.next() {
        match rewrites.iter().find(|(span, _)| span.0 == number) {
            Some(((start, end), output)) => {
                if !output.trim_end().is_empty() {
                    blessed.push_str(output.trim_end());
                    blessed.push('\n');
                }
                // Skip the old content; the line at `end` is the closing fence.
                for _ in *start + 1..*end {
                    lines.next();
                }
                if start != end {
                    continue;
                }
                blessed.push_str(line);
                blessed.push('\n');
            }
            None => {
                blessed.push_str(line);
                blessed.push('\n');
            }
        }
    }
    (failures, blessed)
}

fn documents() -> Vec<PathBuf> {
    let mut paths = vec![Path::new(ROOT).join("README.md")];
    let docs = fs::read_dir(Path::new(ROOT).join("docs")).unwrap();
    paths.extend(docs.map(|entry| entry.unwrap().path()));
    paths.retain(|path| path.extension().is_some_and(|ext| ext == "md") && path.is_file());
    paths.sort();
    paths
}

#[test]
fn the_programs_in_the_documentation_run_as_shown() {
    let bless = std::env::var_os("BIGGO_BLESS").is_some();
    let mut failures = Vec::new();
    let mut programs = 0;
    for path in documents() {
        let (found, blessed) = check_document(&path);
        programs += blocks(&blessed)
            .iter()
            .filter(|block| block.info.first().is_some_and(|lang| lang == "biggo"))
            .count();
        if bless {
            fs::write(&path, blessed).unwrap();
            // A program that fails when it should not is wrong whatever output is shown.
            failures.extend(
                found
                    .into_iter()
                    .filter(|failure| !failure.contains("differs")),
            );
        } else {
            failures.extend(found);
        }
    }
    assert!(
        failures.is_empty(),
        "{}\nrerun with BIGGO_BLESS=1 to write the real output into the documents",
        failures.join("\n\n")
    );
    // A sign that the documents were found and read, not a number to write towards.
    assert!(
        programs >= 80,
        "only {programs} programs in the documentation"
    );
}
