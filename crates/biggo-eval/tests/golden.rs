use std::fs;
use std::path::{Path, PathBuf};

use biggo_eval::Session;
use biggo_syntax::shown_path;

fn testdata() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata")
}

/// The programs in a directory of `testdata/`, in a stable order.
fn programs(dir: &str) -> Vec<PathBuf> {
    let mut paths: Vec<_> = fs::read_dir(testdata().join(dir))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "bgo"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no .bgo files under testdata/{dir}");
    paths
}

/// Checks and runs `source` as if it were the file at `path`. Returns everything it printed,
/// followed by its errors if it has any. In explain mode queries print their plans instead of
/// running.
fn run(path: &Path, source: &str, explain: bool) -> String {
    let root = testdata();
    let name = shown_path(path.strip_prefix(&root).unwrap());
    let mut session = Session::new(Vec::new());
    // Paths in a program are relative to the program's own directory.
    session.vm().set_base_dir(path.parent().unwrap());
    session.vm().set_explain(explain);
    let result = session.run(&name, source, false);
    let mut output = String::from_utf8(std::mem::take(session.vm().output())).unwrap();
    if let Err(failure) = result {
        output.push_str(&failure.render());
    }
    output
}

/// Compares `actual` with the snapshot file, or rewrites the file when `BIGGO_BLESS` is set.
fn snapshot(path: &Path, actual: &str, failures: &mut Vec<String>) {
    if std::env::var_os("BIGGO_BLESS").is_some() {
        fs::write(path, actual).unwrap();
    } else if fs::read_to_string(path).ok().as_deref() != Some(actual) {
        let name = shown_path(path.strip_prefix(testdata()).unwrap());
        failures.push(format!("{name} does not match; got:\n{actual}"));
    }
}

fn report(failures: Vec<String>) {
    assert!(
        failures.is_empty(),
        "{}\nrerun with BIGGO_BLESS=1 to accept the new output",
        failures.join("\n")
    );
}

/// The output of each program in `testdata/run/`, and of the sample `testdata/sales.bgo`.
#[test]
fn run_snapshots() {
    let mut failures = Vec::new();
    let mut paths = programs("run");
    paths.push(testdata().join("sales.bgo"));
    for path in paths {
        let source = fs::read_to_string(&path).unwrap();
        let actual = run(&path, &source, false);
        snapshot(&path.with_extension("out"), &actual, &mut failures);
    }
    report(failures);
}

/// The type errors of each program in `testdata/check/`.
#[test]
fn check_snapshots() {
    let mut failures = Vec::new();
    for path in programs("check") {
        let source = fs::read_to_string(&path).unwrap();
        let actual = run(&path, &source, false);
        snapshot(&path.with_extension("out"), &actual, &mut failures);
    }
    report(failures);
}

/// The plans of the sample program.
#[test]
fn explain_snapshot() {
    let mut failures = Vec::new();
    let path = testdata().join("sales.bgo");
    let source = fs::read_to_string(&path).unwrap();
    snapshot(
        &path.with_extension("plan"),
        &run(&path, &source, true),
        &mut failures,
    );
    report(failures);
}

/// Runs every prefix of each program, and each program with one character removed. Such a
/// program may well fail, but nothing may panic. Programs that touch files are only planned,
/// so that a mangled path cannot write anywhere.
#[test]
fn survives_mangled_programs() {
    for path in programs("run").into_iter().chain(programs("check")) {
        let source = fs::read_to_string(&path).unwrap();
        let plan_only = source.contains("read_") || source.contains("write_");
        for (at, c) in source.char_indices() {
            let rest = &source[at + c.len_utf8()..];
            run(&path, &source[..at], plan_only);
            run(&path, &format!("{}{rest}", &source[..at]), plan_only);
        }
    }
}
