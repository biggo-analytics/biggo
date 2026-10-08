//! `biggo test`: runs test files and reports which of their tests pass. A test file is one
//! whose name ends in `_test.bgo`; its tests are its top-level functions named `test_...`.

use std::path::{Path, PathBuf};

use biggo_eval::Session;
use biggo_syntax::Interner;
use biggo_syntax::ast::StmtKind;

const SUFFIX: &str = "_test.bgo";

/// Directories that hold no tests of the project itself.
const SKIPPED: [&str; 2] = ["target", "node_modules"];

#[derive(Default)]
struct Totals {
    passed: usize,
    failed: usize,
}

/// Runs the tests under `paths`, which are test files or directories to search for them; the
/// current directory if there are none. Returns the exit code.
pub fn test(paths: &[&str]) -> u8 {
    let mut files = Vec::new();
    let paths = if paths.is_empty() { &["."][..] } else { paths };
    for path in paths {
        let path = Path::new(path);
        if path.is_dir() {
            find(path, &mut files);
        } else if path.is_file() {
            files.push(path.to_path_buf());
        } else {
            eprintln!(
                "biggo test: there is no file or directory `{}`",
                path.display()
            );
            return 2;
        }
    }
    if files.is_empty() {
        println!("no test files found; they are the files named `*{SUFFIX}`");
        return 0;
    }
    let mut totals = Totals::default();
    for file in &files {
        run_file(file, &mut totals);
    }
    println!("\n{} passed, {} failed", totals.passed, totals.failed);
    u8::from(totals.failed > 0)
}

/// Collects the test files in `dir` and the directories below it, in name order.
fn find(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        let name = path.file_name().map(|name| name.to_string_lossy());
        let name = name.unwrap_or_default();
        if path.is_dir() {
            if !name.starts_with('.') && !SKIPPED.contains(&&*name) {
                find(&path, files);
            }
        } else if name.ends_with(SUFFIX) {
            // `./a_test.bgo` reads better as `a_test.bgo`.
            files.push(
                path.strip_prefix(".")
                    .map(Path::to_path_buf)
                    .unwrap_or(path),
            );
        }
    }
}

/// The names of the test functions of a file, in the order they are declared, each with
/// whether it can be called without arguments.
fn tests_of(source: &str) -> Vec<(String, bool)> {
    let mut interner = Interner::new();
    let parsed = biggo_syntax::parse(source, &mut interner);
    let mut tests = Vec::new();
    for stmt in &parsed.ast.stmts {
        if let StmtKind::Fn { name, params, .. } = &stmt.kind {
            let name = interner.resolve(name.name);
            if name.starts_with("test_") {
                tests.push((name.to_string(), params.is_empty()));
            }
        }
    }
    tests
}

fn indented(text: &str) -> String {
    let lines = text.lines().map(|line| format!("      {line}\n"));
    lines.collect::<String>().trim_end().to_string()
}

/// Reports a failure: what went wrong, then what the test printed before it did.
fn report(label: &str, error: &str, output: &[u8]) {
    println!("FAIL  {label}");
    println!("{}", indented(error));
    if !output.is_empty() {
        println!("      output:");
        println!("{}", indented(&String::from_utf8_lossy(output)));
    }
}

fn run_file(path: &Path, totals: &mut Totals) {
    let name = path.display().to_string();
    let source = match std::fs::read_to_string(path) {
        Ok(source) => source,
        Err(err) => {
            report(&name, &format!("cannot read the file: {err}"), &[]);
            totals.failed += 1;
            return;
        }
    };
    // The output of a test is shown only if the test fails.
    let mut session = Session::new(Vec::new());
    session
        .vm()
        .set_base_dir(path.parent().unwrap_or(Path::new("")));
    // The file itself runs first: it defines the tests, and may check things on its own.
    if let Err(failure) = session.run(&name, &source, false) {
        report(&name, &failure.render(), session.vm().output());
        totals.failed += 1;
        return;
    }
    let tests = tests_of(&source);
    if tests.is_empty() {
        println!("ok    {name}");
        totals.passed += 1;
    }
    for (test, callable) in tests {
        let label = format!("{name}::{test}");
        if !callable {
            report(&label, "a test function takes no parameters", &[]);
            totals.failed += 1;
            continue;
        }
        session.vm().output().clear();
        match session.run("<test>", &format!("{test}()"), false) {
            Ok(_) => {
                println!("ok    {label}");
                totals.passed += 1;
            }
            Err(failure) => {
                report(&label, &failure.render(), session.vm().output());
                totals.failed += 1;
            }
        }
    }
}
