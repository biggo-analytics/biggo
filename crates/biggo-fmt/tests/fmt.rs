use std::fs;
use std::path::{Path, PathBuf};

use biggo_fmt::format;
use biggo_syntax::{Interner, parse};

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
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    collect(&root.join("testdata"), &mut files);
    collect(&root.join("bench"), &mut files);
    files.sort();
    files
}

/// The syntax tree and the comments of `source`.
fn meaning(source: &str) -> (String, Vec<String>) {
    let mut interner = Interner::new();
    let parsed = parse(source, &mut interner);
    assert!(
        parsed.diagnostics.is_empty(),
        "{:?}\n{source}",
        parsed.diagnostics
    );
    let comments = parsed
        .comments
        .iter()
        .map(|span| source[span.range()].trim_end().to_string());
    (parsed.ast.dump(&interner), comments.collect())
}

/// Formatting must keep the program and its comments, and formatting again must change
/// nothing. Every test program that parses is put through it.
#[test]
fn formatting_keeps_meaning_and_is_stable() {
    let mut checked = 0;
    for path in test_files() {
        let source = fs::read_to_string(&path).unwrap();
        let Ok(formatted) = format(&source) else {
            continue;
        };
        let name = path.display();
        assert_eq!(
            meaning(&source),
            meaning(&formatted),
            "{name} changed meaning:\n{formatted}"
        );
        assert_eq!(
            format(&formatted).unwrap(),
            formatted,
            "{name} is not stable"
        );
        for line in formatted.lines() {
            assert_eq!(line, line.trim_end(), "{name} has trailing spaces");
        }
        checked += 1;
    }
    assert!(checked >= 10, "only {checked} files were formatted");
}

fn check(source: &str, expected: &str) {
    assert_eq!(format(source).unwrap(), expected, "formatting {source:?}");
    assert_eq!(format(expected).unwrap(), expected, "formatting it again");
}

#[test]
fn spacing_and_parentheses() {
    check("let   x=1+2*3", "let x = 1 + 2 * 3\n");
    check("let x = (1+2)*3", "let x = (1 + 2) * 3\n");
    check("let x = ((1))+(2*3)", "let x = 1 + (2 * 3)\n");
    check("let x = (1 + 2)", "let x = 1 + 2\n");
    check("f((a + b), (c))", "f(a + b, c)\n");
    check("f(-a + b)", "f(-a + b)\n");
    check("let x = not (a > b)", "let x = not (a > b)\n");
    check(
        "let x = (if a { b } else { c }) ?? d",
        "let x = (if a { b } else { c }) ?? d\n",
    );
    check("let x = a-(b-c)", "let x = a - (b - c)\n");
    check("let x = -(a+b)", "let x = -(a + b)\n");
    check("let x = not (a and b) or c", "let x = not (a and b) or c\n");
    check("let x = (a ?? b) ?? c", "let x = (a ?? b) ?? c\n");
    check("let x = (a < b) == c", "let x = (a < b) == c\n");
    check("let n = (t |> count()) + 1", "let n = (t |> count()) + 1\n");
    check("f( a ,b=1 )", "f(a, b = 1)\n");
    check(
        "let xs:list<int>=[ 1,2 ,3]",
        "let xs: list<int> = [1, 2, 3]\n",
    );
    check(
        "type T={a:int,b:string?}",
        "type T = { a: int, b: string? }\n",
    );
    check(
        "t |> select(`order date`, x = `if`)",
        "t |> select(`order date`, x = `if`)\n",
    );
    check("", "");
}

#[test]
fn functions_and_blocks() {
    check(
        "fn f(a:int,b:int)->int{a+b}",
        "fn f(a: int, b: int) -> int { a + b }\n",
    );
    check("fn f() {}", "fn f() {}\n");
    check(
        "fn f(a: int) -> int {\nlet b = a\n\n\n  b + 1 }",
        "fn f(a: int) -> int {\n  let b = a\n\n  b + 1\n}\n",
    );
    check(
        "let x = if a { 1 } else if b {\n 2 } else { 3 }",
        "let x = if a { 1 } else if b {\n  2\n} else { 3 }\n",
    );
}

#[test]
fn pipelines() {
    check("a|>f()|>g(1)", "a |> f() |> g(1)\n");
    // A pipeline that was written over several lines stays that way.
    check(
        "let t = a\n|> f()\n  |> g(1)",
        "let t = a\n  |> f()\n  |> g(1)\n",
    );
    let long = "let top = sales |> where(quantity > 0 and order_date >= @2026-01-01) |> derive(revenue = quantity * price) |> take(10)";
    check(
        long,
        "let top = sales\n  |> where(quantity > 0 and order_date >= @2026-01-01)\n  \
         |> derive(revenue = quantity * price)\n  |> take(10)\n",
    );
}

#[test]
fn long_calls_break_one_argument_per_line() {
    let source = "t |> agg(orders = count(), priced = count(price), units = sum(qty), avg_qty = mean(qty), low = min(price))";
    check(
        source,
        "t\n  |> agg(\n    orders = count(),\n    priced = count(price),\n    units = sum(qty),\n    \
         avg_qty = mean(qty),\n    low = min(price),\n  )\n",
    );
}

#[test]
fn comments_stay_where_they_are() {
    check(
        "// top\n\n\nlet a = 1 // one\n// before b\nlet b = 2\n// end",
        "// top\n\nlet a = 1  // one\n// before b\nlet b = 2\n// end\n",
    );
    check(
        "let t = a // source\n  // why\n  |> f() // first\n  |> g()",
        "let t = a  // source\n  // why\n  |> f()  // first\n  |> g()\n",
    );
    check(
        "f(\n  a, // first\n  // about b\n  b,\n)",
        "f(\n  a,  // first\n  // about b\n  b,\n)\n",
    );
    check(
        "fn f() {\n  // nothing yet\n}",
        "fn f() {\n  // nothing yet\n}\n",
    );
    // A comment with no place of its own moves to the end of its statement.
    check("let x = 1 + // why\n  2", "let x = 1 + 2  // why\n");
}

#[test]
fn functions_as_values() {
    check(
        "let f=fn(x:int,y)->int{x+y}",
        "let f = fn(x: int, y) -> int { x + y }\n",
    );
    check(
        "let g : fn(int,int)->int = f\nlet h: fn() = fn(){print(1)}",
        "let g: fn(int, int) -> int = f\nlet h: fn() = fn() { print(1) }\n",
    );
    // A function written over several lines opens on the line of the call it ends.
    check(
        "each(xs,fn(x){\nprint(x)\n})",
        "each(xs, fn(x) {\n  print(x)\n})\n",
    );
    check(
        "xs |> fold(0, fn(sum, x) { let y = x * 2\n sum + y })",
        "xs\n  |> fold(0, fn(sum, x) {\n    let y = x * 2\n    sum + y\n  })\n",
    );
    // Not when what comes before it needs lines of its own.
    check(
        "each(\n  xs,\n  fn(x) {\n    print(x)\n  },\n)",
        "each(\n  xs,\n  fn(x) {\n    print(x)\n  },\n)\n",
    );
}

#[test]
fn match_records_and_maps() {
    check(
        "let s = match  x {\n  1|2 => \"low\"   // small\n  // negative\n  -1=>\"neg\",\n  _=>{ y }\n}",
        "let s = match x {\n  1 | 2 => \"low\"  // small\n  // negative\n  -1 => \"neg\"\n  _ => { y }\n}\n",
    );
    check(
        "match kind { \"a\" => 1, _ => 2 } + 1",
        "match kind {\n  \"a\" => 1\n  _ => 2\n} + 1\n",
    );
    check(
        "let r={name:\"a\",qty:1+2}\nlet m={\"a\":1,\"b\":2}\nlet v=xs[i+1].name[0]",
        "let r = { name: \"a\", qty: 1 + 2 }\nlet m = { \"a\": 1, \"b\": 2 }\nlet v = xs[i + 1].name[0]\n",
    );
    check(
        "let r = {\n  // who\n  name: \"a\", // first\n  qty: 1,\n}",
        "let r = {\n  // who\n  name: \"a\",  // first\n  qty: 1,\n}\n",
    );
    check(
        "import    \"lib/x.bgo\"   // shared\nlet d = 1.50d\nlet t = @2026-01-02T03:04:05",
        "import \"lib/x.bgo\"  // shared\nlet d = 1.50d\nlet t = @2026-01-02T03:04:05\n",
    );
}

#[test]
fn a_comment_after_a_call_stays_outside_it() {
    check(
        "let big=read(\"f.csv\")|>where(qty>5)   // large\n   |>group(region)",
        "let big = read(\"f.csv\")\n  |> where(qty > 5)  // large\n  |> group(region)\n",
    );
    check(
        "f(a, b) // after\ng(\n  a, // first\n  b,\n)",
        "f(a, b)  // after\ng(\n  a,  // first\n  b,\n)\n",
    );
}
