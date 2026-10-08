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
        stderr.ends_with("12 errors in testdata/errors/syntax.bgo\n"),
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

#[test]
fn the_clock_can_be_set_for_a_run() {
    let dir = scratch("clock");
    let program = dir.join("clock.bgo");
    std::fs::write(&program, "print(today(), now(), weekday(today()))\n").unwrap();
    let run = |now: &str| {
        let output = Command::new(env!("CARGO_BIN_EXE_biggo"))
            .args(["run", program.to_str().unwrap()])
            .env("BIGGO_NOW", now)
            .output()
            .unwrap();
        (
            String::from_utf8(output.stdout).unwrap(),
            String::from_utf8(output.stderr).unwrap(),
        )
    };
    assert_eq!(
        run("2026-01-31T18:30:00").0,
        "2026-01-31 2026-01-31T18:30:00 6\n"
    );
    assert_eq!(run("2026-02-02").0, "2026-02-02 2026-02-02T00:00:00 1\n");
    let (stdout, stderr) = run("tomorrow");
    assert_eq!(stdout, "");
    assert!(
        stderr.starts_with("error: BIGGO_NOW is \"tomorrow\"; it has to be a datetime"),
        "{stderr}"
    );
}

/// Runs `biggo run` on a program with the engine held to a number of threads.
fn run_with_threads(program: &std::path::Path, threads: &str) -> (String, String, Option<i32>) {
    let output = Command::new(env!("CARGO_BIN_EXE_biggo"))
        .args(["run", program.to_str().unwrap()])
        .env("RAYON_NUM_THREADS", threads)
        .output()
        .unwrap();
    (
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
        output.status.code(),
    )
}

#[test]
fn a_failure_does_not_depend_on_the_number_of_threads() {
    let dir = scratch("threads");
    // A file of several pieces, with a value that is no number far into it and another
    // further on.
    let mut data = String::from("id,amount\n");
    for id in 0..400_000 {
        let amount = match id {
            300_000 => "first".to_string(),
            390_000 => "second".to_string(),
            id => (id % 97).to_string(),
        };
        data.push_str(&format!("{id},{amount}\n"));
    }
    std::fs::write(dir.join("amounts.csv"), data).unwrap();
    let source = "type Row = { id: int, amount: int }\nlet rows = read_csv<Row>(\"amounts.csv\")\n";

    // The first rows can be had without reading as far as the bad value.
    let early = dir.join("early.bgo");
    let query = "print(rows |> derive(twice = amount * 2) |> where(twice >= 0) |> take(5))\n";
    std::fs::write(&early, format!("{source}{query}")).unwrap();
    let one = run_with_threads(&early, "1");
    assert_eq!((one.1.as_str(), one.2), ("", Some(0)), "{}", one.1);
    assert_eq!(run_with_threads(&early, "8"), one);
    assert_eq!(run_with_threads(&early, "3"), one);

    // Reading all of it fails at the first of the two, whichever was decoded first.
    let all = dir.join("all.bgo");
    std::fs::write(
        &all,
        format!("{source}print(rows |> agg(total = sum(amount)))\n"),
    )
    .unwrap();
    let one = run_with_threads(&all, "1");
    assert!(
        one.1
            .contains("amounts.csv, line 300002: cannot read 'first' as an int"),
        "{}",
        one.1
    );
    assert_eq!(run_with_threads(&all, "8"), one);
    assert_eq!(run_with_threads(&all, "3"), one);
}

#[test]
fn a_file_is_replaced_only_by_a_whole_result() {
    let dir = scratch("replace");
    let data = dir.join("stock.csv");
    let original = "item,count\nbolt,5\nnut,many\nscrew,7\n";
    std::fs::write(&data, original).unwrap();
    let files = || {
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };

    // A query that fails partway leaves the file it was to replace as it was.
    let failing = dir.join("failing.bgo");
    let source = "type Row = { item: string, count: string }\n\
                  let stock = read_csv<Row>(\"stock.csv\")\n\
                  write_csv(stock |> derive(n = to_int(count)), \"stock.csv\")\n";
    std::fs::write(&failing, source).unwrap();
    let (stdout, stderr, code) = run_with_threads(&failing, "2");
    assert_eq!((stdout.as_str(), code), ("", Some(1)));
    assert!(
        stderr.contains("cannot convert 'many' to an int"),
        "{stderr}"
    );
    assert_eq!(std::fs::read_to_string(&data).unwrap(), original);
    assert_eq!(files(), ["failing.bgo", "stock.csv"]);

    // A query can replace the file that it reads.
    let trimming = dir.join("trimming.bgo");
    let source = "type Row = { item: string, count: string }\n\
                  let stock = read_csv<Row>(\"stock.csv\")\n\
                  write_csv(stock |> where(count != \"many\"), \"stock.csv\")\n\
                  write_json(read_csv<Row>(\"stock.csv\"), \"stock.json\")\n\
                  write_parquet(read_csv<Row>(\"stock.csv\"), \"stock.parquet\")\n\
                  print(count(read_parquet<Row>(\"stock.parquet\")), count(read_json<Row>(\"stock.json\")))\n";
    std::fs::write(&trimming, source).unwrap();
    let (stdout, stderr, code) = run_with_threads(&trimming, "2");
    assert_eq!(
        (stdout.as_str(), stderr.as_str(), code),
        ("2 2\n", "", Some(0))
    );
    assert_eq!(
        std::fs::read_to_string(&data).unwrap(),
        "item,count\nbolt,5\nscrew,7\n"
    );
    assert_eq!(
        files(),
        [
            "failing.bgo",
            "stock.csv",
            "stock.json",
            "stock.parquet",
            "trimming.bgo"
        ]
    );
}

#[test]
fn a_program_can_come_from_the_command_line_or_standard_input() {
    let (stdout, stderr, code) = biggo(&["run", "-e", "print(1 + 1, args())", "a", "b"], "");
    assert_eq!(stdout, "2 [\"a\", \"b\"]\n");
    assert_eq!((stderr.as_str(), code), ("", Some(0)));

    let (stdout, stderr, code) = biggo(&["run", "-", "x"], "print(args(), 6 * 7)\n");
    assert_eq!(stdout, "[\"x\"] 42\n");
    assert_eq!((stderr.as_str(), code), ("", Some(0)));

    // Errors name where the program came from.
    let (stdout, stderr, code) = biggo(&["run", "-e", "print(nope)"], "");
    assert!(stderr.contains(" --> <command line>:1:7"), "{stderr}");
    assert_eq!((stdout.as_str(), code), ("", Some(1)));
    let (_, stderr, code) = biggo(&["check", "-"], "let x: int = \"a\"\n");
    assert!(stderr.contains(" --> <stdin>:1:14"), "{stderr}");
    assert!(stderr.ends_with("1 error in <stdin>\n"), "{stderr}");
    assert_eq!(code, Some(1));

    // Paths in such a program start from the directory biggo runs in.
    let program = "print(read_csv<{ qty: int }>(\"testdata/run/data/sales.csv\") |> count())";
    let (stdout, stderr, code) = biggo(&["run", "-e", program], "");
    assert_eq!(
        (stdout.as_str(), stderr.as_str(), code),
        ("10\n", "", Some(0))
    );

    let (_, stderr, code) = biggo(&["run", "-e"], "");
    assert!(
        stderr.starts_with("biggo run: expected a program after `-e`\n"),
        "{stderr}"
    );
    assert_eq!(code, Some(2));
}

#[test]
fn fmt_formats_standard_input_to_standard_output() {
    let (stdout, stderr, code) = biggo(&["fmt", "-"], "let  xs=[1,2 ,3]\nprint( xs )\n");
    assert_eq!(stdout, "let xs = [1, 2, 3]\nprint(xs)\n");
    assert_eq!((stderr.as_str(), code), ("", Some(0)));

    // Text that does not parse is reported, and nothing is written.
    let (stdout, stderr, code) = biggo(&["fmt", "-"], "let x = \n");
    assert!(stderr.contains(" --> <stdin>:1:"), "{stderr}");
    assert_eq!((stdout.as_str(), code), ("", Some(1)));
}

#[test]
fn help_describes_a_builtin_function() {
    let (stdout, stderr, code) = biggo(&["help", "starts_with"], "");
    assert!(
        stdout.starts_with("string:\n  starts_with(s, part)\n"),
        "{stdout}"
    );
    assert!(stdout.contains("    Returns: bool\n"), "{stdout}");
    assert_eq!((stderr.as_str(), code), ("", Some(0)));

    let (stdout, stderr, code) = biggo(&["help", "start"], "");
    assert!(
        stderr.starts_with("biggo help: there is no built-in function `start`;"),
        "{stderr}"
    );
    assert!(
        stderr.contains("starts_with") && stderr.contains("start_of_month"),
        "{stderr}"
    );
    assert_eq!((stdout.as_str(), code), ("", Some(1)));
}

/// `biggo help` knows every function that the checker does.
#[test]
fn help_has_an_entry_for_every_builtin_function() {
    for name in biggo_types::builtin_names() {
        let (stdout, stderr, code) = biggo(&["help", name], "");
        assert!(code == Some(0) && stdout.contains(name), "{name}: {stderr}");
    }
}

#[test]
fn repl_has_commands_of_its_own() {
    let session = "\
let t = from_rows([{ a: 1, b: \"x\" }])
:type t |> where(a > 0) |> select(b)
:type 1 + 2.5
:type fn(n: int) { [n] }
:help ends_with
:type let y = 1
:type
:type nope
:wat
:help nonesuch
y
:quit
print(\"not reached\")
";
    let (stdout, stderr, code) = biggo(&["repl"], session);
    let shown = "\
table<{b: string}>
float
fn(n: int) -> list<int>
string:
  ends_with(s, part)
";
    assert!(stdout.starts_with(shown), "{stdout}");
    assert!(!stdout.contains("not reached"), "{stdout}");
    let complaints = "\
`:type` takes one expression
`:type` takes an expression, as in `:type 1 + 2`
error: undefined name `nope`
 --> <repl>:1:1
  |
1 | nope
  | ^^^^

unknown command `:wat`; `:help` lists the commands
there is no built-in function `nonesuch`
error: undefined name `y`
";
    // Asking for a type defines nothing: `y` is still unknown afterwards.
    assert!(stderr.starts_with(complaints), "{stderr}");
    assert_eq!(code, Some(0));
}

/// Runs a one-line program from the root of the project and gives what it wrote as an error.
fn failure_of(program: &str) -> String {
    let (stdout, stderr, code) = biggo(&["run", "-e", program], "");
    assert_eq!(
        (stdout.as_str(), code),
        ("", Some(1)),
        "{program}: {stderr}"
    );
    stderr.lines().next().unwrap_or("").to_string()
}

#[test]
fn files_that_do_not_fit_their_options_are_explained() {
    let data = "testdata/run/data";
    let cases = [
        (
            format!("print(read_csv<{{ id: int }}>(\"{data}/no_header.csv\", skip = 9))"),
            format!(
                "error: {data}/no_header.csv has nothing after the 9 lines that `skip` passes over"
            ),
        ),
        (
            format!(
                "let lines = 0 - 1\nprint(read_csv<{{ id: int }}>(\"{data}/sales.csv\", skip = lines))"
            ),
            "error: `skip` cannot be negative, found -1".to_string(),
        ),
        (
            format!(
                "print(read_csv<{{ a: int, b: string, c: int, d: float?, e: string, f: int }}>\
                 (\"{data}/no_header.csv\", header = false))"
            ),
            format!("error: {data}/no_header.csv has 5 columns, but its row type has 6"),
        ),
        (
            format!(
                "print(read_csv<{{ a: string }}>(\"{data}/report_title.csv\", header = false))"
            ),
            format!(
                "error: {data}/report_title.csv, line 4: the row has 5 fields, but the first row has 1"
            ),
        ),
        (
            format!("print(read_json<{{ id: int }}>(\"{data}/bad_array.json\"))"),
            format!(
                "error: {data}/bad_array.json: every item must be one JSON object, but one is 5"
            ),
        ),
        (
            format!("print(read_json<{{ id: int }}>(\"{data}/open_array.json\"))"),
            format!(
                "error: {data}/open_array.json starts a JSON array with `[`, but does not end with `]`"
            ),
        ),
        (
            format!("print(read_parquet<{{ id: int }}>(\"{data}/*.parquet\"))"),
            format!("error: no file matches {data}/*.parquet"),
        ),
        (
            format!("print(read_excel<{{ id: int }}>(\"{data}/book.xlsx\", sheet = \"Nope\"))"),
            format!("error: {data}/book.xlsx has no sheet \"Nope\"; its sheets are Sales, Plain, สาขา, Block"),
        ),
        (
            format!("print(read_excel<{{ id: int }}>(\"{data}/book.xlsx\", range = \"B:F\"))"),
            "error: `range` names the first and the last cell of a block, as in \"B3:F200\"; found \"B:F\"".to_string(),
        ),
        (
            format!("print(read_excel<{{ id: int }}>(\"{data}/missing.xlsx\"))"),
            format!("error: cannot open {data}/missing.xlsx: No such file or directory (os error 2)"),
        ),
        (
            format!("print(read_excel<{{ id: int }}>(\"{data}/sales.csv\"))"),
            format!(
                "error: {data}/sales.csv is not a workbook by its name; those that can be read are \
                 .xlsx, .xlsm, .xlsb, .xls and .ods"
            ),
        ),
        (
            format!("print(read_excel<{{ id: int, branch: int }}>(\"{data}/book.xlsx\", skip = 3))"),
            format!("error: column `branch` of {data}/book.xlsx: cannot read 'north' as an int"),
        ),
        (
            format!("print(read_excel<{{ id: int, units: int }}>(\"{data}/book.xlsx\", skip = 3))"),
            format!(
                "error: column `units` of {data}/book.xlsx has missing values, \
                 but is declared `int`; declare it `int?`"
            ),
        ),
        (
            format!(
                "print(read_excel<{{ a: int, b: string, c: int, d: int }}>\
                 (\"{data}/book.xlsx\", sheet = \"Plain\", header = false))"
            ),
            format!("error: sheet \"Plain\" of {data}/book.xlsx has 3 columns, but its row type has 4"),
        ),
        (
            format!("print(read_excel<{{ id: int }}>(\"{data}/book.xlsx\", skip = 99))"),
            format!("error: sheet \"Sales\" of {data}/book.xlsx has no rows to read"),
        ),
        (
            "write_excel(from_rows([{ a: 1 }]), \"target/never.xlsx\", sheet = \"a/b\")".to_string(),
            "error: a sheet cannot be named \"a/b\": a name has 1 to 31 characters, and none of [ ] : * ? / \\"
                .to_string(),
        ),
    ];
    for (program, expected) in cases {
        assert_eq!(failure_of(&program), expected, "{program}");
    }
}

#[test]
fn a_program_reads_the_environment() {
    let output = Command::new(env!("CARGO_BIN_EXE_biggo"))
        .args([
            "run",
            "-e",
            "print(env(\"BIGGO_TEST_REGION\"), env(\"BIGGO_TEST_UNSET\"))",
        ])
        .env("BIGGO_TEST_REGION", "north east")
        .env_remove("BIGGO_TEST_UNSET")
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&output.stdout), "north east null\n");
    assert!(output.status.success());
}

#[test]
fn infer_writes_the_row_type_of_a_file() {
    let data = "testdata/run/data";
    let (stdout, stderr, code) = biggo(
        &["infer", &format!("{data}/report_title.csv"), "--skip", "3"],
        "",
    );
    let expected = "\
type ReportTitle = {
  id: int,
  region: string,
  units: string?,
  price: string,
  note: string?,
}

let report_title = read_csv<ReportTitle>(\"testdata/run/data/report_title.csv\", skip = 3)
";
    assert_eq!(
        (stdout.as_str(), stderr.as_str(), code),
        (expected, "", Some(0))
    );

    // The delimiter is found out, and so is an encoding that the file itself gives away.
    let (stdout, _, code) = biggo(&["infer", &format!("{data}/people.tsv")], "");
    assert!(
        stdout
            .ends_with("read_csv<People>(\"testdata/run/data/people.tsv\", delimiter = \"\\t\")\n"),
        "{stdout}"
    );
    assert_eq!(code, Some(0));
    let (stdout, _, _) = biggo(&["infer", &format!("{data}/amounts.csv")], "");
    assert!(stdout.contains("delimiter = \";\""), "{stdout}");
    let (stdout, _, _) = biggo(&["infer", &format!("{data}/utf16.csv")], "");
    assert!(stdout.contains("encoding = \"utf-16le\""), "{stdout}");

    // Any other encoding has to be named.
    let (stdout, stderr, code) = biggo(&["infer", &format!("{data}/thai_tis620.csv")], "");
    assert!(
        stderr.contains("is not UTF-8 text; if it is in another encoding, name it"),
        "{stderr}"
    );
    assert_eq!((stdout.as_str(), code), ("", Some(1)));
    let (stdout, _, code) = biggo(
        &[
            "infer",
            &format!("{data}/thai_tis620.csv"),
            "--encoding",
            "tis-620",
        ],
        "",
    );
    assert!(
        stdout.contains("  ชื่อ: string,\n") && stdout.contains("encoding = \"windows-874\""),
        "{stdout}"
    );
    assert_eq!(code, Some(0));

    let (stdout, _, code) = biggo(
        &["infer", &format!("{data}/no_header.csv"), "--no-header"],
        "",
    );
    assert!(
        stdout.contains("  column_1: int,\n  column_2: string,\n"),
        "{stdout}"
    );
    assert!(
        stdout.contains("  column_4: float?,\n") && stdout.contains(", header = false)"),
        "{stdout}"
    );
    assert_eq!(code, Some(0));

    // JSON, as lines and as one array; a field that holds a list cannot be a column.
    let (stdout, _, code) = biggo(&["infer", &format!("{data}/items.json")], "");
    let expected = "\
// `tags` holds lists or objects, which a column cannot, and is left out
type Items = {
  id: int,
  name: string,
  score: float?,
  joined: date?,
}

let items = read_json<Items>(\"testdata/run/data/items.json\")
";
    assert_eq!((stdout.as_str(), code), (expected, Some(0)));

    let (_, stderr, code) = biggo(&["infer", "README.md"], "");
    assert!(
        stderr.starts_with("biggo infer: cannot tell what kind of file README.md is"),
        "{stderr}"
    );
    assert_eq!(code, Some(1));
    let (_, stderr, code) = biggo(&["infer"], "");
    assert!(
        stderr.starts_with("biggo infer: expected one file\n\nusage: biggo infer"),
        "{stderr}"
    );
    assert_eq!(code, Some(2));
}

/// What `infer` prints is a program: the type it writes reads the file it was made from.
#[test]
fn what_infer_writes_runs() {
    let dir = scratch("infer");
    let table = "from_rows([{ id: 1, at: @2026-01-05, price: 2.5d, ok: true, note: \"a\" }, \
                 { id: 2, at: @2026-01-06, price: 0.75d, ok: false, note: \"b\" }])";
    let write = format!(
        "let t = {table}\nwrite_parquet(t, \"t.parquet\")\nwrite_sql(t, \"shop.db\", \"orders\")\n\
         write_csv(t, \"plain.csv\")\nwrite_json(t, \"lines.json\")\nwrite_excel(t, \"book.xlsx\")\n"
    );
    std::fs::write(dir.join("write.bgo"), write).unwrap();
    let path = |name: &str| dir.join(name).to_str().unwrap().to_string();
    let (_, stderr, code) = biggo(&["run", &path("write.bgo")], "");
    assert_eq!((stderr.as_str(), code), ("", Some(0)));

    let sources: [&[&str]; 5] = [
        &["t.parquet"],
        &["shop.db", "orders"],
        &["plain.csv"],
        &["lines.json"],
        &["book.xlsx"],
    ];
    for source in sources {
        let output = Command::new(env!("CARGO_BIN_EXE_biggo"))
            .arg("infer")
            .args(source)
            .current_dir(&dir)
            .output()
            .unwrap();
        let program = String::from_utf8(output.stdout).unwrap();
        assert!(
            output.status.success(),
            "{source:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        // A database keeps a truth value as a number, and says that any column may be null.
        let (id, ok) = match source.len() {
            2 => ("  id: int?,\n", "  ok: int?,\n"),
            _ => ("  id: int,\n", "  ok: bool,\n"),
        };
        assert!(program.contains(id) && program.contains(ok), "{program}");
        assert!(program.contains("  at: date"), "{program}");
        // The last line names the table; print how many of its rows are of the fifth.
        let name = program.lines().last().unwrap().split(' ').nth(1).unwrap();
        let program = format!("{program}print({name} |> where(day(at) == 5) |> count())\n");
        let ran = Command::new(env!("CARGO_BIN_EXE_biggo"))
            .args(["run", "-"])
            .current_dir(&dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                child.stdin.take().unwrap().write_all(program.as_bytes())?;
                child.wait_with_output()
            })
            .unwrap();
        let shown = (
            String::from_utf8_lossy(&ran.stdout),
            String::from_utf8_lossy(&ran.stderr),
        );
        assert_eq!(
            (shown.0.as_ref(), shown.1.as_ref()),
            ("1\n", ""),
            "{source:?}:\n{program}"
        );
    }

    // A database is described a table at a time.
    let output = Command::new(env!("CARGO_BIN_EXE_biggo"))
        .args(["infer", "shop.db"])
        .current_dir(&dir)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(
            "a database needs the name of a table after it; in shop.db, its tables are orders"
        ),
        "{stderr}"
    );
}

/// Needs a PostgreSQL server: set `BIGGO_TEST_POSTGRES` to the address of a database that
/// can be written to, such as `postgres://app:password@localhost:5432/shop`. Without it the
/// test passes without having tried anything.
#[test]
fn a_postgres_server_is_read_and_written() {
    let Ok(address) = std::env::var("BIGGO_TEST_POSTGRES") else {
        return;
    };
    let program = r#"
let db = env("BIGGO_TEST_POSTGRES") ?? ""
let people = from_rows([
  { id: 1, name: "Ann, \"A\"", score: 9.5, joined: @2026-01-05, at: @2026-01-05T09:30:00, ok: true, paid: 12.50d, took: minutes(2), note: "" },
  { id: 2, name: "สมหญิง", score: 0.0 / 0.0, joined: @2026-02-28, at: @2026-02-28T23:59:59, ok: false, paid: 0.000001d, took: seconds(1), note: "two\nlines" },
]) |> derive(maybe = if id > 1 { id * 10 } else { null })
write_sql(people, db, "biggo_test_people")

type Person = { id: int, name: string, score: float, joined: date, at: datetime, ok: bool, paid: decimal, took: duration, note: string?, maybe: int? }
let back = read_sql<Person>(db, "select * from biggo_test_people order by id")
assert_eq(back, people)
print(back |> where(ok) |> select(id, name, paid, note, maybe))

// A query that ends in a semicolon, one that ends in a comment, and the server stopping
// after a row.
print(read_sql<Person>(db, "select * from biggo_test_people order by id desc;") |> select(id, at) |> take(1))
print(read_sql<Person>(db, "select * from biggo_test_people where id = 1 -- the first") |> select(id) |> count())
type Made = { n: int, label: string, big: decimal, moment: datetime, flag: bool? }
print(read_sql<Made>(db, "select count(*) as n, 'x' || 'y' as label, 12345678901234567890.123456::numeric as big, timestamptz '2026-01-05 09:30:00+07' as moment, null::boolean as flag from biggo_test_people"))

// A table is replaced whole, and one that is read can be the one that is written.
write_sql(back |> where(id > 1), db, "public.biggo_test_people")
print(read_sql<{ id: int }>(db, "select id from biggo_test_people") |> count())
print(read_sql<{ nope: int }>(db, "select id from biggo_test_people"))
"#;
    let output = Command::new(env!("CARGO_BIN_EXE_biggo"))
        .args(["run", "-e", program])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let expected = "\
+----+----------+------+------+-------+
| id | name     | paid | note | maybe |
+----+----------+------+------+-------+
| 1  | Ann, \"A\" | 12.5 |      | null  |
+----+----------+------+------+-------+
+----+---------------------+
| id | at                  |
+----+---------------------+
| 2  | 2026-02-28T23:59:59 |
+----+---------------------+
1
+---+-------+-----------------------------+---------------------+------+
| n | label | big                         | moment              | flag |
+---+-------+-----------------------------+---------------------+------+
| 2 | xy    | 12345678901234567890.123456 | 2026-01-05T02:30:00 | null |
+---+-------+-----------------------------+---------------------+------+
1
";
    assert_eq!(stdout, expected, "{stderr}");
    // The address is named without its password.
    let (_, shown) = biggo_plan::source::server(&address).unwrap();
    let complaint = format!("error: {shown} has no column `nope`; its columns are id\n");
    assert!(stderr.starts_with(&complaint), "{stderr}");
    if let Some((_, rest)) = address.split_once("://")
        && let Some((login, _)) = rest.split_once('@')
        && let Some((_, password)) = login.split_once(':')
    {
        assert!(!stderr.contains(password), "the password is in a message");
    }
}

#[test]
fn the_address_of_a_server_is_shown_without_its_password() {
    let program = "\
let shop = \"postgres://report:hunter2@localhost:1/shop?sslmode=disable\"
let orders = read_sql<{ id: int, total: decimal }>(shop, \"select id, total, placed from orders\")
explain(orders |> where(id > 5) |> select(total) |> take(3))
print(orders)
";
    let (stdout, stderr, code) = biggo(&["run", "-e", program], "");
    let plan = "\
plan:
  Limit: take 3
    Project: total
      Filter: id > 5
        Scan postgres \"postgres://report:***@localhost:1/shop?sslmode=disable\" \
\"select id, total, placed from orders\": id, total
";
    assert!(stdout.starts_with(plan), "{stdout}");
    // Nothing listens there, and the message says where without saying the password.
    let complaint = "error: cannot connect to postgres://report:***@localhost:1/shop?sslmode=disable: \
                     error connecting to server: ";
    assert!(stderr.starts_with(complaint), "{stderr}");
    assert!(!stdout.contains("hunter2") && !stderr.contains("hunter2"));
    assert_eq!(code, Some(1));

    let (_, stderr, _) = biggo(
        &[
            "run",
            "-e",
            "let db = \"postgres://u:hunter2@local host/db\"\nwrite_sql(from_rows([{ a: 1 }]), db, \"t\")",
        ],
        "",
    );
    assert!(!stderr.contains("hunter2"), "{stderr}");
    assert!(
        stderr.starts_with("error: cannot connect to postgres://u:***@local host/db"),
        "{stderr}"
    );
}
