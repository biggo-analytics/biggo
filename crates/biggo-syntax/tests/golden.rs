use std::fs;
use std::path::{Path, PathBuf};

use biggo_syntax::{Interner, SourceFile, parse};

fn testdata() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata")
}

/// Every `.bgo` file under `testdata/`, in a stable order.
fn test_files() -> Vec<PathBuf> {
    fn collect(dir: &Path, files: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                collect(&path, files);
            } else if path.extension().is_some_and(|ext| ext == "bgo") {
                files.push(path);
            }
        }
    }
    let mut files = Vec::new();
    collect(&testdata(), &mut files);
    files.sort();
    assert!(!files.is_empty(), "no .bgo files under testdata/");
    files
}

/// The syntax tree of `source`, or its diagnostics when it does not parse.
fn render(name: &str, source: &str) -> String {
    let mut interner = Interner::new();
    let parsed = parse(source, &mut interner);
    if parsed.diagnostics.is_empty() {
        return parsed.ast.dump(&interner);
    }
    let file = SourceFile::new(name, source);
    let rendered: Vec<String> = parsed.diagnostics.iter().map(|d| file.render(d)).collect();
    rendered.join("\n")
}

/// Compares each test file with its `.syntax` snapshot. Set `BIGGO_BLESS=1` to rewrite the
/// snapshots.
#[test]
fn syntax_snapshots() {
    let root = testdata();
    let bless = std::env::var_os("BIGGO_BLESS").is_some();
    let mut failures = Vec::new();
    for path in test_files() {
        let name = path.strip_prefix(&root).unwrap().to_string_lossy();
        let actual = render(&name, &fs::read_to_string(&path).unwrap());
        let snapshot = path.with_extension("syntax");
        if bless {
            fs::write(&snapshot, &actual).unwrap();
        } else if fs::read_to_string(&snapshot).ok().as_deref() != Some(actual.as_str()) {
            failures.push(format!(
                "{name} does not match its snapshot; got:\n{actual}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{}\nrerun with BIGGO_BLESS=1 to accept the new output",
        failures.join("\n")
    );
}

/// Parses every prefix and suffix of each test file, and each file with one character removed.
/// Whatever the input, parsing and rendering the diagnostics must finish without panicking.
#[test]
fn survives_broken_input() {
    for path in test_files() {
        let source = fs::read_to_string(&path).unwrap();
        for (at, c) in source.char_indices() {
            let rest = &source[at + c.len_utf8()..];
            render("broken.bgo", &source[..at]);
            render("broken.bgo", &source[at..]);
            render("broken.bgo", &format!("{}{rest}", &source[..at]));
        }
    }
}
