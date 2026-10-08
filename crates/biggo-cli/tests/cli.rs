use std::io::Write;
use std::process::{Command, Output, Stdio};

fn biggo(args: &[&str], stdin: &str) -> (String, String, Option<i32>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_biggo"))
        .args(args)
        .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let Output {
        status,
        stdout,
        stderr,
    } = child.wait_with_output().unwrap();
    let text = |bytes: Vec<u8>| String::from_utf8(bytes).unwrap();
    (text(stdout), text(stderr), status.code())
}

#[test]
fn run_prints_program_output() {
    let (stdout, stderr, code) = biggo(&["run", "testdata/run/functions.bgo"], "");
    let expected = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../testdata/run/functions.out"
    ))
    .unwrap();
    assert_eq!((stdout, stderr.as_str(), code), (expected, "", Some(0)));
}

#[test]
fn run_reports_a_runtime_error() {
    let (stdout, stderr, code) = biggo(&["run", "testdata/run/runtime_error.bgo"], "");
    assert_eq!(stdout, "before\n");
    assert_eq!(
        stderr,
        "error: division by zero\n --> testdata/run/runtime_error.bgo:4:3\n  |\n4 |   id % buckets\n  |   ^^^^^^^^^^^^\n"
    );
    assert_eq!(code, Some(1));
}

#[test]
fn run_reports_syntax_errors_without_running() {
    let (stdout, stderr, code) = biggo(&["run", "testdata/errors/syntax.bgo"], "");
    assert_eq!(stdout, "");
    assert!(
        stderr.ends_with("8 errors in testdata/errors/syntax.bgo\n"),
        "{stderr}"
    );
    assert_eq!(code, Some(1));
}

#[test]
fn repl_evaluates_entries_and_keeps_definitions() {
    let session = "\
let x = 2
x * 21
\"hi\"
print(\"hi\")
fn add_one(n: int) -> int {
  n + 1
}
add_one(x)
[1, 2] |>
  print()
";
    let (stdout, stderr, code) = biggo(&["repl"], session);
    assert_eq!(stdout, "42\n\"hi\"\nhi\n3\n[1, 2]\n");
    assert_eq!((stderr.as_str(), code), ("", Some(0)));
}

#[test]
fn repl_continues_after_errors() {
    let session = "\
let x = 1
x +

missing
x % 0
x + 1";
    let (stdout, stderr, code) = biggo(&["repl"], session);
    assert_eq!(stdout, "2\n");
    assert_eq!(
        stderr,
        "\
error: expected an expression, found end of file
 --> <repl>:1:4
  |
1 | x +
  |    ^
error: undefined name `missing`
 --> <repl>:1:1
  |
1 | missing
  | ^^^^^^^
error: division by zero
 --> <repl>:1:1
  |
1 | x % 0
  | ^^^^^
"
    );
    assert_eq!(code, Some(0));
}

#[test]
fn rejects_bad_usage() {
    let (_, stderr, code) = biggo(&["run"], "");
    assert!(stderr.starts_with("biggo run: expected a file"), "{stderr}");
    assert_eq!(code, Some(2));
    let (_, stderr, code) = biggo(&["check", "a.bgo", "b.bgo"], "");
    assert!(
        stderr.starts_with("biggo check: expected exactly one file"),
        "{stderr}"
    );
    assert_eq!(code, Some(2));
    let (_, stderr, code) = biggo(&["frobnicate"], "");
    assert!(
        stderr.starts_with("biggo: unknown command `frobnicate`"),
        "{stderr}"
    );
    assert_eq!(code, Some(2));
}

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

fn snapshot(path: &str) -> String {
    std::fs::read_to_string(format!("{ROOT}/{path}")).unwrap()
}

/// A fresh directory for the files of one test.
fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("biggo-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn run_executes_table_pipelines() {
    let (stdout, stderr, code) = biggo(&["run", "testdata/sales.bgo"], "");
    assert_eq!(
        (stdout, stderr.as_str(), code),
        (snapshot("testdata/sales.out"), "", Some(0))
    );
}

#[test]
fn explain_prints_plans_without_running() {
    let (stdout, stderr, code) = biggo(&["explain", "testdata/sales.bgo"], "");
    assert_eq!(
        (stdout, stderr.as_str(), code),
        (snapshot("testdata/sales.plan"), "", Some(0))
    );
}

#[test]
fn check_reports_type_errors() {
    let (stdout, stderr, code) = biggo(&["check", "testdata/check/core.bgo"], "");
    assert_eq!(stdout, "");
    assert!(
        stderr.starts_with("error: expected int, found string\n"),
        "{stderr}"
    );
    assert!(
        stderr.ends_with("20 errors in testdata/check/core.bgo\n"),
        "{stderr}"
    );
    assert_eq!(code, Some(1));
    assert_eq!(
        biggo(&["check", "testdata/sales.bgo"], ""),
        (String::new(), String::new(), Some(0))
    );
}

#[test]
fn repl_shows_tables_and_types_stay_checked() {
    let session = "\
type Sale = { region: string, qty: int }
let sales = read_csv<Sale>(\"testdata/run/data/sales.csv\")
sales |> group(region) |> agg(units = sum(qty)) |> sort(region)
sales |> where(qty > \"many\")
sales |> count()
";
    let (stdout, stderr, code) = biggo(&["repl"], session);
    assert_eq!(
        stdout,
        "\
+--------+-------+
| region | units |
+--------+-------+
| east   | 7     |
| north  | 21    |
| south  | 17    |
+--------+-------+
10
"
    );
    assert!(
        stderr.starts_with("error: cannot apply `>` to int and string\n"),
        "{stderr}"
    );
    assert_eq!(code, Some(0));
}

#[test]
fn fmt_rewrites_files_and_check_lists_them() {
    let dir = scratch("fmt");
    let (messy, tidy, broken) = (
        dir.join("messy.bgo"),
        dir.join("tidy.bgo"),
        dir.join("broken.bgo"),
    );
    std::fs::write(&messy, "let   x=1\n// note\nlet y = [ 1,2 ]").unwrap();
    std::fs::write(&tidy, "let x = 1\n").unwrap();
    std::fs::write(&broken, "let x = (\n").unwrap();
    let (messy, tidy, broken) = (
        messy.to_str().unwrap(),
        tidy.to_str().unwrap(),
        broken.to_str().unwrap(),
    );

    let (stdout, _, code) = biggo(&["fmt", "--check", messy, tidy], "");
    assert_eq!((stdout, code), (format!("{messy}\n"), Some(1)));
    assert_eq!(
        std::fs::read_to_string(messy).unwrap(),
        "let   x=1\n// note\nlet y = [ 1,2 ]"
    );

    let (stdout, stderr, code) = biggo(&["fmt", messy, tidy], "");
    assert_eq!((stdout.as_str(), stderr.as_str(), code), ("", "", Some(0)));
    assert_eq!(
        std::fs::read_to_string(messy).unwrap(),
        "let x = 1\n// note\nlet y = [1, 2]\n"
    );
    assert_eq!(biggo(&["fmt", "--check", messy, tidy], "").2, Some(0));

    // A file with syntax errors is reported and left alone.
    let (_, stderr, code) = biggo(&["fmt", broken], "");
    assert!(
        stderr.starts_with("error: expected an expression, found end of file"),
        "{stderr}"
    );
    assert_eq!(code, Some(1));
    assert_eq!(std::fs::read_to_string(broken).unwrap(), "let x = (\n");
}

#[test]
fn build_makes_a_standalone_executable() {
    let dir = scratch("build");
    let app = dir.join("functions");
    let (stdout, stderr, code) = biggo(
        &[
            "build",
            "testdata/run/functions.bgo",
            "-o",
            app.to_str().unwrap(),
        ],
        "",
    );
    assert_eq!((stdout.as_str(), stderr.as_str(), code), ("", "", Some(0)));
    // The executable needs neither the source file nor a command.
    let output = Command::new(&app)
        .current_dir(&dir)
        .arg("ignored")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        snapshot("testdata/run/functions.out")
    );
    assert_eq!(output.status.code(), Some(0));

    // A program with errors is not built.
    let (_, stderr, code) = biggo(
        &[
            "build",
            "testdata/check/core.bgo",
            "-o",
            app.to_str().unwrap(),
        ],
        "",
    );
    assert!(
        stderr.ends_with("20 errors in testdata/check/core.bgo\n"),
        "{stderr}"
    );
    assert_eq!(code, Some(1));
}

#[test]
fn lsp_answers_an_editor() {
    let frame = |body: &str| format!("Content-Length: {}\r\n\r\n{body}", body.len());
    let input = frame(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#)
        + &frame(r#"{"jsonrpc":"2.0","method":"exit"}"#);
    let (stdout, stderr, code) = biggo(&["lsp"], &input);
    assert!(
        stdout.contains(r#""documentFormattingProvider":true"#),
        "{stdout}"
    );
    assert_eq!((stderr.as_str(), code), ("", Some(0)));
}

#[test]
fn test_runs_test_functions_and_reports_failures() {
    let (stdout, stderr, code) = biggo(&["test", "testdata/tests"], "");
    assert_eq!(
        stdout,
        "\
ok    testdata/tests/math_test.bgo::test_square
ok    testdata/tests/math_test.bgo::test_units
ok    testdata/tests/math_test.bgo::test_tables
ok    testdata/tests/script_test.bgo

4 passed, 0 failed
"
    );
    assert_eq!((stderr.as_str(), code), ("", Some(0)));

    // A failure shows the error and what the test printed; the other tests still run.
    let (stdout, _, code) = biggo(&["test", "testdata/tests_failing"], "");
    assert_eq!(
        stdout,
        "\
ok    testdata/tests_failing/broken_test.bgo::test_passes
FAIL  testdata/tests_failing/broken_test.bgo::test_fails
      error: assertion failed: the values differ
        left:  2
        right: 3
       --> testdata/tests_failing/broken_test.bgo:7:3
        |
      7 |   assert_eq(1 + 1, 3)
        |   ^^^^^^^^^^^^^^^^^^^
      output:
      checking the sum
FAIL  testdata/tests_failing/broken_test.bgo::test_needs_argument
      a test function takes no parameters
FAIL  testdata/tests_failing/script_test.bgo
      error: assertion failed: checked at the top level
       --> testdata/tests_failing/script_test.bgo:3:1
        |
      3 | assert(1 > 2, \"checked at the top level\")
        | ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
      output:
      loading

1 passed, 3 failed
"
    );
    assert_eq!(code, Some(1));

    // One file can be named, and a path that does not exist is a mistake in the command.
    let (stdout, _, code) = biggo(&["test", "testdata/tests/script_test.bgo"], "");
    assert!(stdout.ends_with("1 passed, 0 failed\n"), "{stdout}");
    assert_eq!(code, Some(0));
    let (_, stderr, code) = biggo(&["test", "testdata/nowhere"], "");
    assert_eq!(
        stderr,
        "biggo test: there is no file or directory `testdata/nowhere`\n"
    );
    assert_eq!(code, Some(2));
}

#[test]
fn imports_are_found_from_the_importing_file() {
    let (stdout, stderr, code) = biggo(&["run", "testdata/run/imports.bgo"], "");
    assert_eq!(
        (stdout, stderr.as_str(), code),
        (snapshot("testdata/run/imports.out"), "", Some(0))
    );

    // The errors of an imported file are shown in that file.
    let dir = scratch("imports");
    std::fs::create_dir(dir.join("lib")).unwrap();
    std::fs::write(
        dir.join("main.bgo"),
        "import \"lib/half.bgo\"\nprint(half)\n",
    )
    .unwrap();
    std::fs::write(dir.join("lib/half.bgo"), "let half: int = 0.5\n").unwrap();
    let main = dir.join("main.bgo");
    let lib = dir.join("lib/half.bgo");
    for command in ["check", "run"] {
        let (stdout, stderr, code) = biggo(&[command, main.to_str().unwrap()], "");
        assert_eq!(stdout, "");
        assert_eq!(
            stderr,
            format!(
                "error: expected int, found float\n --> {0}:1:17\n  |\n1 | let half: int = 0.5\n  \
                 |                 ^^^\n\n1 error in {0}\n",
                lib.display()
            )
        );
        assert_eq!(code, Some(1));
    }

    std::fs::write(dir.join("main.bgo"), "import \"missing.bgo\"\n").unwrap();
    let (_, stderr, code) = biggo(&["run", main.to_str().unwrap()], "");
    assert!(
        stderr.starts_with(&format!(
            "error: there is no file `{}`\n",
            dir.join("missing.bgo").display()
        )),
        "{stderr}"
    );
    assert_eq!(code, Some(1));
}

#[test]
fn build_takes_imported_files_along() {
    let dir = scratch("build-imports");
    let app = dir.join("imports");
    let (stdout, stderr, code) = biggo(
        &[
            "build",
            "testdata/run/imports.bgo",
            "-o",
            app.to_str().unwrap(),
        ],
        "",
    );
    assert_eq!((stdout.as_str(), stderr.as_str(), code), ("", "", Some(0)));
    // Where the executable runs there is no `lib/shapes.bgo` to read.
    let output = Command::new(&app).current_dir(&dir).output().unwrap();
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        snapshot("testdata/run/imports.out")
    );
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn run_executes_the_newer_features() {
    for name in ["lambdas", "match", "records", "reshape", "stats", "strings"] {
        let (stdout, stderr, _) = biggo(&["run", &format!("testdata/run/{name}.bgo")], "");
        // The snapshot holds the output followed by the error, if the program ends in one.
        let expected = snapshot(&format!("testdata/run/{name}.out"));
        let both = format!("{stdout}{stderr}");
        assert_eq!(both.replace("testdata/run/", "run/"), expected, "{name}");
    }
}

#[test]
fn a_program_sees_its_arguments() {
    let dir = scratch("args");
    let program = dir.join("report.bgo");
    std::fs::write(&program, "print(args())\nprint(len(args()))\n").unwrap();
    let path = program.to_str().unwrap();

    // Everything after the file is the program's, whatever it looks like.
    let given = ["2026-01", "two words", "--check", "-o"];
    let shown = "[\"2026-01\", \"two words\", \"--check\", \"-o\"]\n4\n";
    for command in ["run", "explain"] {
        let mut line = vec![command, path];
        line.extend(given);
        let (stdout, stderr, code) = biggo(&line, "");
        assert_eq!(
            (stdout.as_str(), stderr.as_str(), code),
            (shown, "", Some(0)),
            "{command}"
        );
    }
    let (stdout, stderr, code) = biggo(&["run", path], "");
    assert_eq!(
        (stdout.as_str(), stderr.as_str(), code),
        ("[]\n0\n", "", Some(0))
    );

    // A built program takes its whole command line.
    let app = dir.join("report");
    let (_, stderr, code) = biggo(&["build", path, "-o", app.to_str().unwrap()], "");
    assert_eq!((stderr.as_str(), code), ("", Some(0)));
    let output = Command::new(&app).args(given).output().unwrap();
    assert_eq!(String::from_utf8(output.stdout).unwrap(), shown);
    assert_eq!(output.status.code(), Some(0));
}
