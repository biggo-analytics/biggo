use crate::{Failure, Session};

struct Repl(Session<Vec<u8>>);

impl Repl {
    fn new() -> Self {
        Repl(Session::new(Vec::new()))
    }

    /// Runs `source`; returns the `repr` of its value, or the message of its first error.
    fn run(&mut self, source: &str) -> Result<String, String> {
        match self.0.run("test.bgo", source, false) {
            Ok(value) => Ok(value.repr().to_string()),
            Err(Failure::Static(errors)) => Err(errors.diagnostics[0].message.clone()),
            Err(Failure::Runtime(error)) => Err(error.message.clone()),
        }
    }

    fn output(&mut self) -> String {
        String::from_utf8(std::mem::take(self.0.vm().output())).unwrap()
    }
}

fn eval(source: &str) -> String {
    match Repl::new().run(source) {
        Ok(value) => value,
        Err(message) => panic!("{source:?} failed: {message}"),
    }
}

fn fails(source: &str) -> String {
    match Repl::new().run(source) {
        Ok(value) => panic!("{source:?} should fail, but gave {value}"),
        Err(message) => message,
    }
}

fn ok(value: &str) -> Result<String, String> {
    Ok(value.to_string())
}

#[test]
fn integer_arithmetic() {
    assert_eq!(eval("1 + 2 * 3"), "7");
    assert_eq!(eval("10 - 4 - 3"), "3");
    assert_eq!(eval("-(2 + 3)"), "-5");
    assert_eq!(eval("-7 % 3"), "-1");
    assert_eq!(eval("3000000000 * 2"), "6000000000");
    assert_eq!(fails("7 % 0"), "division by zero");
    assert_eq!(fails("9223372036854775807 + 1"), "integer overflow");
    assert_eq!(fails("-9223372036854775807 - 2"), "integer overflow");
    assert_eq!(fails("9223372036854775807 * 2"), "integer overflow");
}

#[test]
fn division_is_always_float() {
    assert_eq!(eval("7 / 2"), "3.5");
    assert_eq!(eval("6 / 3"), "2.0");
    assert_eq!(eval("1 / 0"), "inf");
}

#[test]
fn ints_widen_to_floats() {
    assert_eq!(eval("1 + 2.5"), "3.5");
    assert_eq!(eval("2 * 3.0"), "6.0");
    assert_eq!(eval("7.5 % 2"), "1.5");
    assert_eq!(eval("-1.5"), "-1.5");
    assert_eq!(eval("1 == 1.0"), "true");
    assert_eq!(eval("1 < 1.5"), "true");
    assert_eq!(eval("let x: float = 1\nx"), "1.0");
    assert_eq!(eval("let xs: list<float> = [1, 2]\nxs"), "[1.0, 2.0]");
    assert_eq!(eval("let n = 3\nlet x: float? = n\nx"), "3.0");
    assert_eq!(eval("fn half(x: float) -> float { x / 2 }\nhalf(3)"), "1.5");
    assert_eq!(eval("if true { 1 } else { 2.5 }"), "1.0");
}

#[test]
fn comparisons() {
    assert_eq!(eval("1 < 2"), "true");
    assert_eq!(eval("2 <= 1"), "false");
    assert_eq!(eval("3 != 3"), "false");
    assert_eq!(eval(r#""a" < "b""#), "true");
    assert_eq!(eval(r#""ab" == "ab""#), "true");
    assert_eq!(eval("@2026-01-31 < @2026-02-01"), "true");
    assert_eq!(eval("@2026-01-01 >= @2026-01-01"), "true");
    assert_eq!(eval("true != false"), "true");
}

#[test]
fn strings_concatenate() {
    assert_eq!(eval(r#""data" + "lake""#), r#""datalake""#);
}

#[test]
fn operators_reject_mismatched_types() {
    assert_eq!(fails(r#"1 + "a""#), "cannot apply `+` to int and string");
    assert_eq!(fails(r#"1 < "a""#), "cannot apply `<` to int and string");
    assert_eq!(fails("true < false"), "cannot apply `<` to bool and bool");
    assert_eq!(
        fails("[1] == [1]"),
        "cannot apply `==` to list<int> and list<int>"
    );
    assert_eq!(fails("@2026-01-01 - 1"), "cannot apply `-` to date and int");
    assert_eq!(fails("1 and true"), "cannot apply `and` to int and bool");
    assert_eq!(fails(r#"-"a""#), "cannot negate string");
    assert_eq!(fails("not 1"), "`not` expects a bool, found int");
}

#[test]
fn null_propagates_through_operators() {
    let maybe = "let x: int? = null\n";
    assert_eq!(eval("null + 1"), "null");
    assert_eq!(eval("1 / null"), "null");
    assert_eq!(eval(&format!("{maybe}x < 1")), "null");
    assert_eq!(eval(&format!("{maybe}x == x")), "null");
    assert_eq!(eval(&format!("{maybe}-x")), "null");
    assert_eq!(eval("not null"), "null");
    assert_eq!(
        fails(&format!("{maybe}x == null")),
        "comparing with `null` always gives null; use `is_null(...)` to test for null"
    );
    assert_eq!(eval(&format!("{maybe}is_null(x)")), "true");
    assert_eq!(eval("is_null(1)"), "false");
}

#[test]
fn coalesce_replaces_null() {
    assert_eq!(eval("null ?? 3"), "3");
    assert_eq!(eval("2 ?? 3"), "2");
    assert_eq!(eval("null ?? null ?? 4"), "4");
    assert_eq!(eval("null ?? null"), "null");
    assert_eq!(eval("let x: int? = null\nx ?? 2.5"), "2.5");
    assert_eq!(
        fails(r#"1 ?? "a""#),
        "`??` needs a value and a fallback of one type, found int and string"
    );
}

#[test]
fn logic_is_three_valued() {
    let cases = [
        ("true and true", "true"),
        ("true and false", "false"),
        ("false and true", "false"),
        ("true and null", "null"),
        ("null and true", "null"),
        ("null and false", "false"),
        ("false and null", "false"),
        ("null and null", "null"),
        ("false or false", "false"),
        ("false or true", "true"),
        ("true or false", "true"),
        ("false or null", "null"),
        ("null or false", "null"),
        ("null or true", "true"),
        ("true or null", "true"),
        ("null or null", "null"),
        ("not true", "false"),
    ];
    for (source, expected) in cases {
        assert_eq!(eval(source), expected, "{source}");
    }
}

#[test]
fn right_operand_is_skipped_when_it_cannot_matter() {
    assert_eq!(eval("false and 1 % 0 == 0"), "false");
    assert_eq!(eval("true or 1 % 0 == 0"), "true");
    assert_eq!(eval("1 ?? 1 % 0"), "1");
    assert_eq!(fails("null and 1 % 0 == 0"), "division by zero");
    assert_eq!(fails("true and 1 % 0 == 0"), "division by zero");
}

#[test]
fn if_chooses_a_branch() {
    assert_eq!(eval(r#"if 1 < 2 { "yes" } else { "no" }"#), r#""yes""#);
    assert_eq!(eval("if false { 1 } else if false { 2 } else { 3 }"), "3");
    assert_eq!(eval("if false { 1 }"), "()");
    assert_eq!(eval("if true { 1 }"), "()");
    assert_eq!(fails("if 1 { 2 }"), "a condition must be a bool, found int");
    assert_eq!(
        fails("let c: bool? = null\nif c { 2 }"),
        "this condition can be null; say what null means, for example with `?? false`"
    );
    assert_eq!(
        fails(r#"if true { 1 } else { "a" }"#),
        "the branches of an `if` must have one type, found int and string"
    );
}

#[test]
fn let_and_block_scopes() {
    assert_eq!(eval("let x = 1"), "()");
    assert_eq!(eval("let x = 1\nlet x = x + 1\nx"), "2");
    assert_eq!(
        eval("let x = 1\nlet y = {\n  let x = 10\n  x + 1\n}\nx + y"),
        "12"
    );
    assert_eq!(
        eval("let a = {\n  let b = 2\n  let c = b * 3\n  [b, c]\n}\na"),
        "[2, 6]"
    );
    assert_eq!(fails("y"), "undefined name `y`");
    assert_eq!(fails("{ let inner = 1 }\ninner"), "undefined name `inner`");
    assert_eq!(fails(r#"let x: int = "a""#), "expected int, found string");
    assert_eq!(
        fails("let x = null"),
        "cannot tell which type this `null` has; annotate the variable, as in `let x: int? = null`"
    );
}

#[test]
fn lists_and_strings_print_as_literals() {
    assert_eq!(eval("[1, 2.5, null]"), "[1.0, 2.5, null]");
    assert_eq!(eval("[[true], []]"), "[[true], []]");
    assert_eq!(eval(r#"["a", "b"]"#), r#"["a", "b"]"#);
    assert_eq!(eval("[@2026-01-01]"), "[@2026-01-01]");
    assert_eq!(
        fails(r#"[1, "a"]"#),
        "the elements of a list must have one type, found int and string"
    );
    let thai = r#""ร้าน \"A\"\n""#;
    assert_eq!(eval(thai), thai);
}

#[test]
fn calls_user_functions() {
    let sub = "fn sub(a: int, b: int) -> int { a - b }\n";
    assert_eq!(eval(&format!("{sub}sub(10, 1)")), "9");
    assert_eq!(eval(&format!("{sub}sub(b = 1, a = 10)")), "9");
    assert_eq!(eval(&format!("{sub}sub(10, b = 1)")), "9");
    assert_eq!(eval(&format!("{sub}10 |> sub(1) |> sub(b = 2)")), "7");
    assert_eq!(eval("fn nothing() {}\nnothing()"), "()");
    // Arguments are evaluated as written, even when named ones go to other parameters.
    let mut repl = Repl::new();
    let source = "fn show(n: int) -> int {\n  print(n)\n  n\n}\n\
                  fn sub(a: int, b: int) -> int { a - b }\n\
                  sub(b = show(1), a = show(10))";
    assert_eq!(repl.run(source), ok("9"));
    assert_eq!(repl.output(), "1\n10\n");
}

#[test]
fn functions_recurse_and_are_hoisted() {
    let fib = "fn fib(n: int) -> int { if n < 2 { n } else { fib(n - 1) + fib(n - 2) } }";
    assert_eq!(eval(&format!("{fib}\nfib(15)")), "610");
    assert_eq!(
        eval("let r = twice(4)\nfn twice(n: int) -> int { n * 2 }\nr"),
        "8"
    );
    let parity = "fn is_even(n: int) -> bool { if n == 0 { true } else { is_odd(n - 1) } }\n\
                  fn is_odd(n: int) -> bool { if n == 0 { false } else { is_even(n - 1) } }";
    assert_eq!(eval(&format!("{parity}\nis_even(10)")), "true");
}

#[test]
fn return_types_are_inferred_when_possible() {
    assert_eq!(eval("fn double(n: int) { n * 2 }\ndouble(4)"), "8");
    let unknown = "the return type of `f` is not known here; declare it with `-> type`";
    assert_eq!(fails("fn f(n: int) { f(n) }"), unknown);
    assert_eq!(fails("let x = f(1)\nfn f(n: int) { n }"), unknown);
    assert_eq!(
        fails(r#"fn g() -> int { "a" }"#),
        "`g` is declared to return int, but its body has type string"
    );
}

#[test]
fn functions_are_values() {
    let inc = "fn inc(n: int) -> int { n + 1 }\n";
    assert_eq!(eval(&format!("{inc}let f = inc\nf(1)")), "2");
    assert_eq!(eval(&format!("{inc}let f = inc\nf(n = 4)")), "5");
    assert_eq!(eval(&format!("{inc}inc")), "<fn inc>");
    // A built-in function is not a value, but a function that calls it is.
    assert_eq!(
        fails("let show = print"),
        "`print` is a built-in function, which can only be called; \
         to pass it along, wrap it in a function: `fn(x) { print(x) }`"
    );
    assert_eq!(eval("map([\"a\"], fn(s) { upper(s) })"), "[\"A\"]");
    assert_eq!(fails("nowhere"), "undefined name `nowhere`");
}

#[test]
fn reports_bad_calls() {
    let f = "fn f(a: int, b: int) -> int { a + b }\n";
    let fails_with_f = |call: &str| fails(&format!("{f}{call}"));
    assert_eq!(fails_with_f("f(1)"), "missing argument `b` in call to `f`");
    assert_eq!(
        fails_with_f("f(1, 2, 3)"),
        "too many arguments: `f` takes 2"
    );
    assert_eq!(
        fails_with_f("1 |> f(2, 3)"),
        "too many arguments: `f` takes 2"
    );
    assert_eq!(
        fails_with_f("f(1, c = 2)"),
        "`f` has no parameter named `c`"
    );
    assert_eq!(
        fails_with_f("f(1, a = 2)"),
        "argument `a` is given more than once"
    );
    assert_eq!(
        fails_with_f("f(1, b = 2, b = 3)"),
        "argument `b` is given more than once"
    );
    assert_eq!(
        fails_with_f("f<int>(1, 2)"),
        "`f` does not take type arguments"
    );
    assert_eq!(
        fails_with_f(r#"f("x", 2)"#),
        "`f` expects int for `a`, found string"
    );
    assert_eq!(
        fails("let n = 5\nn(1)"),
        "`n` is not a function; it has type int"
    );
    assert_eq!(fails("nope(1)"), "undefined function `nope`");
    assert_eq!(
        fails("print(x = 1)"),
        "`print` does not take named arguments"
    );
    assert_eq!(
        fails("fn none() {}\n1 |> none()"),
        "too many arguments: `none` takes 0"
    );
    assert_eq!(
        fails("let x = 1\nx.y"),
        "a value of type int has no field `y`"
    );
    assert_eq!(
        fails("fn where(x: int) -> int { x }"),
        "`where` is a built-in function; choose another name"
    );
}

#[test]
fn scoping_is_lexical() {
    let caller_locals_are_invisible = "let x = 1\n\
        fn get() -> int { x }\n\
        fn shadow() -> int {\n  let x = 2\n  get()\n}\n\
        shadow()";
    assert_eq!(eval(caller_locals_are_invisible), "1");
    assert_eq!(
        eval("let x = 1\nfn id(x: int) -> int { x }\nid(5) + x"),
        "6"
    );
    assert_eq!(
        fails("fn outer() { fn inner() {} }\ninner()"),
        "undefined function `inner`"
    );
    // A function sees the variables defined above it, not those that come later.
    assert_eq!(
        fails("fn f() -> int { later }\nlet later = 1\nf()"),
        "undefined name `later`"
    );
    assert_eq!(
        fails("let a = f()\nlet b = 1\nfn f() -> int { b }"),
        "`b` is used before it has a value"
    );
}

#[test]
fn nested_functions_capture_their_scope() {
    let captures = "fn make(n: int) -> int {\n\
        \x20 let base = n * 10\n\
        \x20 fn add(m: int) -> int { base + m + n }\n\
        \x20 add(1)\n\
        }\n\
        make(2)";
    assert_eq!(eval(captures), "23");

    // A capture is the value at the declaration; a later `let` does not change it.
    let by_value = "fn outer() -> int {\n\
        \x20 let a = 1\n\
        \x20 fn get() -> int { a }\n\
        \x20 let a = 2\n\
        \x20 get() + a\n\
        }\n\
        outer()";
    assert_eq!(eval(by_value), "3");

    let recursion = "fn sum_to(n: int) -> int {\n\
        \x20 fn go(i: int, acc: int) -> int {\n\
        \x20   if i > n { acc } else { go(i + 1, acc + i) }\n\
        \x20 }\n\
        \x20 go(1, 0)\n\
        }\n\
        sum_to(10)";
    assert_eq!(eval(recursion), "55");

    let two_levels = "fn a(x: int) -> int {\n\
        \x20 fn b(y: int) -> int {\n\
        \x20   fn c(z: int) -> int { x + y + z }\n\
        \x20   c(3)\n\
        \x20 }\n\
        \x20 b(2)\n\
        }\n\
        a(1)";
    assert_eq!(eval(two_levels), "6");
}

#[test]
fn scalar_functions_work_on_single_values() {
    assert_eq!(eval(r#"lower("ABC") + upper("def")"#), r#""abcDEF""#);
    assert_eq!(eval(r#"length("ร้าน")"#), "4");
    assert_eq!(eval(r#"contains("data lake", "lake")"#), "true");
    assert_eq!(eval(r#"starts_with("data", "lake")"#), "false");
    assert_eq!(eval(r#"trim("  x ")"#), r#""x""#);
    assert_eq!(
        eval("year(@2026-10-08) * 10000 + month(@2026-10-08) * 100 + day(@2026-10-08)"),
        "20261008"
    );
    assert_eq!(eval("round(2.567, 2)"), "2.57");
    assert_eq!(eval("round(2.5)"), "3.0");
    assert_eq!(eval("floor(2.5) + ceil(2.5) + sqrt(16)"), "9.0");
    assert_eq!(eval("abs(-3)"), "3");
    assert_eq!(eval("abs(-3.5)"), "3.5");
    assert_eq!(eval("to_int(3.9)"), "3");
    assert_eq!(eval("to_int(-3.9)"), "-3");
    assert_eq!(eval(r#"to_int("42") + 1"#), "43");
    assert_eq!(eval(r#"to_float("1.5")"#), "1.5");
    assert_eq!(
        eval("to_string(42) + to_string(1.5) + to_string(@2026-01-01)"),
        r#""421.52026-01-01""#
    );
    assert_eq!(eval("let x: string? = null\nlower(x)"), "null");
    assert!(Repl::new().run(r#"to_int("x")"#).is_err());
    assert_eq!(fails("lower(1)"), "`lower` expects a string, found int");
    assert_eq!(
        fails("sum(1)"),
        "`sum` can only be used inside `agg` or `window`"
    );
}

#[test]
fn print_writes_a_line() {
    let mut repl = Repl::new();
    let source = "print(\"a\", 1, 2.0, [1.5, 2.5], null, @2026-01-01)\nprint()\nprint(\"ร้าน\")";
    assert_eq!(repl.run(source), ok("()"));
    assert_eq!(repl.output(), "a 1 2.0 [1.5, 2.5] null 2026-01-01\n\nร้าน\n");
}

#[test]
fn definitions_persist_and_later_ones_shadow() {
    let mut repl = Repl::new();
    assert_eq!(
        repl.run("let x = 2\nfn double(n: int) -> int { n * x }"),
        ok("()")
    );
    assert_eq!(repl.run("double(21)"), ok("42"));
    // A new `x` is a new variable; `double` keeps the one it was checked against.
    assert_eq!(repl.run("let x = \"three\""), ok("()"));
    assert_eq!(repl.run("double(21)"), ok("42"));
    assert_eq!(repl.run("x"), ok("\"three\""));
    assert_eq!(repl.run("fn double(n: int) -> int { n * 2 + 1 }"), ok("()"));
    assert_eq!(repl.run("double(1)"), ok("3"));
}

#[test]
fn an_error_leaves_the_session_usable() {
    let mut repl = Repl::new();
    let failing = "let kept = 1\n\
        fn f(n: int) -> int {\n  if true {\n    let y = n\n    y % 0\n  } else { 0 }\n}\n\
        f(1)";
    assert_eq!(repl.run(failing), Err("division by zero".to_string()));
    assert_eq!(
        repl.run("kept + f(0 - 1) * 0"),
        Err("division by zero".to_string())
    );
    assert_eq!(repl.run("kept + 1"), ok("2"));
    // An entry that does not check defines nothing.
    assert_eq!(
        repl.run("let fresh = 1\nlet bad = fresh + \"a\""),
        Err("cannot apply `+` to int and string".to_string())
    );
    assert_eq!(repl.run("fresh"), Err("undefined name `fresh`".to_string()));
}

#[test]
fn errors_point_into_the_module_that_failed() {
    let mut repl = Repl::new();
    repl.run("fn bad(n: int) -> int {\n  n % 0\n}").unwrap();
    let Err(failure) = repl.0.run("later.bgo", "bad(7)", false) else {
        panic!("expected a runtime error");
    };
    assert_eq!(
        failure.render(),
        "error: division by zero\n --> test.bgo:2:3\n  |\n2 |   n % 0\n  |   ^^^^^\n"
    );
}

#[test]
fn deep_recursion_works_up_to_a_limit() {
    let count = "fn depth(n: int) -> int { if n == 0 { 0 } else { 1 + depth(n - 1) } }";
    assert_eq!(eval(&format!("{count}\ndepth(50000)")), "50000");
    assert_eq!(
        fails("fn forever(n: int) -> int { forever(n + 1) }\nforever(0)"),
        "stack overflow: the program recurses too deeply"
    );
}

#[test]
fn echo_prints_the_last_value() {
    let mut repl = Repl(Session::new(Vec::new()));
    for entry in [
        "let x = 2",
        "x * 21",
        "\"hi\"",
        "print(\"hi\")",
        "[1.5, 2.5]",
    ] {
        repl.0.run("<repl>", entry, true).unwrap();
    }
    assert_eq!(repl.output(), "42\n\"hi\"\nhi\n[1.5, 2.5]\n");
}

#[test]
fn lambdas_and_the_loops_that_call_them() {
    assert_eq!(eval("let f = fn(x: int) { x + 1 }\nf(2)"), "3");
    assert_eq!(eval("map([1, 2, 3], fn(x) { x * 2 })"), "[2, 4, 6]");
    assert_eq!(
        eval("filter(range(10), fn(x) { x % 3 == 0 })"),
        "[0, 3, 6, 9]"
    );
    assert_eq!(eval("fold([1, 2, 3], 10, fn(sum, x) { sum + x })"), "16");
    assert_eq!(eval("let k = 5\nmap([1], fn(x) { x + k })"), "[6]");
    assert_eq!(eval("map(range(0), fn(x) { x })"), "[]");
    assert_eq!(eval("range(5, 2)"), "[]");
    assert_eq!(eval("range(-2, 1)"), "[-2, -1, 0]");
    let compose = "fn compose(f: fn(int) -> int, g: fn(int) -> int) -> fn(int) -> int {\n  \
                   fn(x) { g(f(x)) }\n}\n";
    assert_eq!(
        eval(&format!(
            "{compose}compose(fn(x) {{ x + 1 }}, fn(x) {{ x * 10 }})(2)"
        )),
        "30"
    );
    assert_eq!(
        fails("range(100000000)"),
        "`range` would make a list of 100000000 numbers; the most is 10000000"
    );
    assert_eq!(eval("each([1, 2], fn(x) { x })"), "()");
}

#[test]
fn an_error_inside_a_loop_leaves_the_session_usable() {
    let mut repl = Repl::new();
    assert_eq!(
        repl.run("map([1, 0], fn(x) { map([x], fn(y) { 1 % y }) })"),
        Err("division by zero".to_string())
    );
    assert_eq!(repl.run("map([1, 2], fn(x) { x + 1 })"), ok("[2, 3]"));
}

#[test]
fn match_tests_its_arms_in_order() {
    assert_eq!(
        eval("match 2 { 1 => \"a\", 2 | 3 => \"b\", _ => \"c\" }"),
        "\"b\""
    );
    assert_eq!(
        eval("match 9 { 1 => \"a\", 2 | 3 => \"b\", _ => \"c\" }"),
        "\"c\""
    );
    assert_eq!(
        eval("let x: int? = null\nmatch x { 1 => 1, null => 0, _ => 2 }"),
        "0"
    );
    // A null matches no value: it falls through to `_`.
    assert_eq!(eval("let x: int? = null\nmatch x { 1 => 1, _ => 2 }"), "2");
    assert_eq!(eval("match 1 + 1 { 2 => 1.5, _ => 2 }"), "1.5");
    assert_eq!(eval("match false { true => 1, false => 2 }"), "2");
}

#[test]
fn records_and_maps() {
    assert_eq!(eval("{ a: 1, b: \"x\" }"), "{a: 1, b: \"x\"}");
    assert_eq!(eval("let r = { a: 1, b: { c: [1, 2] } }\nr.b.c[1]"), "2");
    assert_eq!(eval("{ \"k\": 1 }[\"k\"]"), "1");
    assert_eq!(eval("{ \"k\": 1 }[\"other\"]"), "null");
    assert_eq!(eval("{ 1: \"a\", 2: \"b\" }[2]"), "\"b\"");
    assert_eq!(eval("keys(put({ true: 1 }, false, 2))"), "[true, false]");
    assert_eq!(
        eval("let m: map<string, float> = {}\nput(m, \"a\", 1)"),
        "{\"a\": 1.0}"
    );
    assert_eq!(
        eval("let m: map<date, int> = { @2026-01-01: 1 }\nm[@2026-01-01]"),
        "1"
    );
    assert_eq!(eval("[10, 20, 30][-1]"), "30");
    assert_eq!(eval("[1] + [2.5]"), "[1.0, 2.5]");
    assert_eq!(
        fails("[10, 20][2]"),
        "index 2 is out of range for a list of 2 items"
    );
    assert_eq!(
        fails("[10, 20][-3]"),
        "index -3 is out of range for a list of 2 items"
    );
    // A variable keeps its type; only what is written out converts part by part.
    assert_eq!(
        fails("let r = { a: 1 }\nlet s: { a: float } = r"),
        "expected {a: float}, found {a: int}"
    );
    // A value fits a type that only allows null in more places.
    assert_eq!(
        eval("let xs = [1, 2]\nlet ys: list<int?> = xs\nys"),
        "[1, 2]"
    );
    assert_eq!(
        eval("let r = { a: 1, b: [\"x\"] }\nlet s: { a: int?, b: list<string?> }? = r\ns"),
        "{a: 1, b: [\"x\"]}"
    );
    assert_eq!(
        eval("fn head(xs: list<int?>) -> int? { xs[0] }\nhead(range(2, 4))"),
        "2"
    );
    assert_eq!(
        fails("let xs: list<int?> = [1]\nlet ys: list<int> = xs"),
        "expected list<int>, found list<int?>"
    );
}

#[test]
fn decimals_are_exact() {
    assert_eq!(eval("0.1d + 0.2d"), "0.3d");
    assert_eq!(eval("1.10d * 3"), "3.3d");
    assert_eq!(eval("1d / 3"), "0.333333d");
    assert_eq!(eval("2d / 3"), "0.666667d");
    assert_eq!(eval("7.5d % 2"), "1.5d");
    assert_eq!(eval("1.5d + 1.5"), "3.0");
    assert_eq!(eval("1.5d < 2"), "true");
    assert_eq!(eval("-(1.5d)"), "-1.5d");
    assert_eq!(eval("let x: decimal? = null\nx + 1"), "null");
    assert_eq!(fails("1d % 0"), "division by zero");
    assert_eq!(
        fails("99999999999999999999999999999999d * 10"),
        "decimal overflow"
    );
}

#[test]
fn datetimes_and_durations() {
    assert_eq!(
        eval("@2026-01-01T10:00:00 + hours(26)"),
        "@2026-01-02T12:00:00"
    );
    assert_eq!(
        eval("@2026-01-02T00:00:00 - @2026-01-01T12:00:00"),
        "12:00:00"
    );
    assert_eq!(eval("@2026-01-02 - @2026-01-01"), "1d 00:00:00");
    assert_eq!(eval("@2026-01-01 + minutes(1.5)"), "@2026-01-01T00:01:30");
    assert_eq!(eval("days(2) - hours(1) > days(1)"), "true");
    assert_eq!(eval("@2026-01-01T00:00:00 == @2026-01-01"), "true");
    assert_eq!(eval("total_seconds(minutes(2))"), "120.0");
    assert_eq!(eval("hour(@2026-01-01T23:59:00)"), "23");
    assert_eq!(eval("to_date(@2026-01-01T23:59:00)"), "@2026-01-01");
}

fn provide(repl: &mut Repl, path: &str, source: &str) {
    repl.0
        .provide(std::path::Path::new(path), source.to_string());
}

#[test]
fn an_imported_file_is_loaded_once() {
    let mut repl = Repl::new();
    provide(
        &mut repl,
        "lib/a.bgo",
        "import \"b.bgo\"\nlet a = b + 1\nprint(\"a\")",
    );
    provide(
        &mut repl,
        "lib/b.bgo",
        "let b = 1\nfn twice(x: int) -> int { x * 2 }\nprint(\"b\")",
    );
    let main = "import \"lib/a.bgo\"\nimport \"lib/../lib/b.bgo\"\ntwice(a + b)";
    assert_eq!(repl.run(main), ok("6"));
    assert_eq!(repl.output(), "b\na\n");
    assert_eq!(repl.run("import \"lib/a.bgo\"\na"), ok("2"));
    assert_eq!(repl.output(), "");
}

#[test]
fn import_errors() {
    assert_eq!(
        fails("import \"nowhere.bgo\""),
        "there is no file `nowhere.bgo`"
    );
    assert_eq!(
        fails("let x = 1\nimport \"nowhere.bgo\""),
        "there is no file `nowhere.bgo`"
    );

    let mut repl = Repl::new();
    provide(&mut repl, "a.bgo", "import \"b.bgo\"");
    provide(&mut repl, "b.bgo", "import \"a.bgo\"");
    assert_eq!(
        repl.run("import \"a.bgo\""),
        Err(
            "`a.bgo` is in the middle of being imported; files cannot import each other in a \
             circle"
                .to_string()
        )
    );

    provide(&mut repl, "late.bgo", "let x = 1");
    assert_eq!(
        repl.run("let y = 2\nimport \"late.bgo\""),
        Err("imports come before everything else in a file".to_string())
    );

    // The errors of an imported file are its own, and say where it was imported.
    provide(&mut repl, "lib/bad.bgo", "let x: int = \"text\"");
    provide(&mut repl, "via.bgo", "import \"lib/bad.bgo\"");
    let Err(Failure::Static(errors)) =
        repl.0
            .run("main.bgo", "let z = 0\n\nimport \"via.bgo\"", false)
    else {
        panic!("the import should fail");
    };
    assert_eq!(errors.name, "lib/bad.bgo");
    assert_eq!(errors.diagnostics[0].message, "expected int, found string");
    assert_eq!(errors.import.map(|span| span.start), Some(11));
    // A file that failed is not remembered as loaded.
    provide(&mut repl, "lib/bad.bgo", "let fixed = 7");
    assert_eq!(repl.run("import \"via.bgo\"\nfixed"), ok("7"));
}

#[test]
fn assertions() {
    assert_eq!(eval("assert(true)"), "()");
    assert_eq!(fails("assert(1 > 2)"), "assertion failed");
    assert_eq!(fails("assert(1 > 2, \"no\")"), "assertion failed: no");
    assert_eq!(eval("assert_eq([1.0, 2.0], [1, 2])"), "()");
    assert_eq!(
        fails("assert_eq({ a: 1 }, { a: 2 })"),
        "assertion failed: the values differ\n  left:  {a: 1}\n  right: {a: 2}"
    );
    assert_eq!(
        fails("assert_eq(from_rows([{ a: 1 }, { a: 2 }]), from_rows([{ a: 1 }]))"),
        "assertion failed: the left table has 2 rows but the right one has 1"
    );
}

#[test]
fn named_arguments_follow_positional_ones_in_function_calls() {
    let f = "fn f(a: int, b: int) -> int { a - b }\n";
    assert_eq!(eval(&format!("{f}f(5, b = 1)")), "4");
    assert_eq!(
        fails(&format!("{f}f(b = 1, 5)")),
        "positional arguments must come before named arguments"
    );
    // The columns of a table operation come in the order they are written.
    assert_eq!(
        eval("to_rows(from_rows([{ a: 1, c: 3 }]) |> select(a, b = a + 1, c))"),
        "[{a: 1, b: 2, c: 3}]"
    );
}

/// Values are copied all the time, so they must stay small: a decimal is kept as bytes for
/// that reason, where an `i128` would widen every value.
#[test]
fn values_are_three_words_wide() {
    assert_eq!(std::mem::size_of::<crate::Value>(), 24);
}

/// The virtual machine computes the common scalar functions itself; the engine computes all
/// of them. Both must give the same value, or the same error, for the same call.
#[test]
fn scalar_functions_agree_with_the_engine() {
    let prelude = "let n: int? = null\nlet s: string? = null\nlet x: float? = null\n";
    let calls = [
        "is_null(1)",
        "is_null(n)",
        "is_null(s)",
        "to_string(42)",
        "to_string(-7)",
        "to_string(\"x\")",
        "to_string(true)",
        "to_string(@2026-01-02)",
        "to_string(@2026-01-02T03:04:05.5)",
        "to_string(hours(1.5))",
        "to_string(1.50d)",
        "to_string(1.5)",
        "to_string(1e21)",
        "to_string(n)",
        "to_int(3.9)",
        "to_int(-3.9)",
        "to_int(true)",
        "to_int(19.99d)",
        "to_int(-19.99d)",
        "to_int(7)",
        "to_int(1e30)",
        "to_int(-1e30)",
        "to_int(0.0 / 0.0)",
        "to_int(\"42\")",
        "to_int(\"4x\")",
        "to_int(x)",
        "to_float(2)",
        "to_float(2.5d)",
        "to_float(1.5)",
        "to_float(\"1e3\")",
        "to_decimal(3)",
        "to_decimal(1.5d)",
        "to_decimal(0.1)",
        "abs(-3)",
        "abs(-2.5)",
        "abs(-9223372036854775807 - 1)",
        "abs(-1.5d)",
        "abs(n)",
        "floor(2.7)",
        "floor(-2.7)",
        "floor(3)",
        "ceil(2.1)",
        "sqrt(2)",
        "sqrt(-1)",
        "sqrt(2.25d)",
        "round(2.5)",
        "round(-2.5)",
        "round(0.5)",
        "round(3.14159, 2)",
        "round(1234.5, -2)",
        "round(2, 1)",
        "round(2.345d, 2)",
        "round(1e300, 400)",
        "round(0.5, -400)",
        "round(x, 2)",
        "length(\"ภาษาไทย\")",
        "length(\"\")",
        "length(s)",
        "lower(\"AbC ÄÖ\")",
        "upper(\"abc ß\")",
        "trim(\"  a b \\t\")",
        "upper(s)",
        "contains(\"biggo\", \"gg\")",
        "contains(\"biggo\", \"\")",
        "contains(\"biggo\", \"x\")",
        "contains(s, \"a\")",
        "starts_with(\"biggo\", \"big\")",
        "starts_with(\"biggo\", \"go\")",
        "ends_with(\"biggo\", \"go\")",
        "ends_with(\"a\", \"ab\")",
        "substring(\"hello\", 1)",
        "substring(\"hello\", 1, 3)",
        "substring(\"hello\", -3)",
        "substring(\"hello\", -9, 2)",
        "substring(\"hello\", 9)",
        "substring(\"hello\", 2, -1)",
        "substring(\"ภาษาไทย\", 2, 3)",
        "substring(\"hello\", -9223372036854775807 - 1, 9223372036854775807)",
        "substring(s, 1)",
        "substring(\"hello\", n)",
        "substring(\"hello\", 1, n)",
        "replace(\"a-b-c\", \"-\", \"+\")",
        "replace(\"abc\", \"\", \"x\")",
        "replace(\"aaa\", \"a\", \"\")",
        "replace(s, \"a\", \"b\")",
        "replace(\"abc\", \"a\", s)",
        "split_part(\"a,b,c\", \",\", 0)",
        "split_part(\"a,b,c\", \",\", -1)",
        "split_part(\"a,b,c\", \",\", 3)",
        "split_part(\"a,b,c\", \",\", -4)",
        "split_part(\"abc\", \"\", 0)",
        "split_part(\"abc\", \"\", 1)",
        "split_part(\"a,b\", \",\", n)",
        "pad_left(\"7\", 3, \"0\")",
        "pad_left(\"7\", 3)",
        "pad_right(\"ab\", 5, \"xyz\")",
        "pad_right(\"hello\", 3)",
        "pad_left(\"a\", 4, \"\")",
        "pad_left(\"é\", 3, \"ü\")",
        "pad_left(\"a\", 2000000)",
        "pad_right(\"a\", n)",
        "pad_left(\"a\", 3, s)",
        "index_of(\"hello\", \"l\")",
        "index_of(\"hello\", \"z\")",
        "index_of(\"größe\", \"ß\")",
        "index_of(\"abc\", \"\")",
        "index_of(s, \"a\")",
        "regex_match(\"order-123\", \"[0-9]+\")",
        "regex_match(\"order\", \"^[0-9]+$\")",
        "regex_match(s, \"a\")",
        "regex_match(\"a\", s)",
        "regex_extract(\"order-123\", \"([a-z]+)-([0-9]+)\", 2)",
        "regex_extract(\"order-123\", \"[0-9]+\")",
        "regex_extract(\"order\", \"[0-9]+\")",
        "regex_extract(\"ab\", \"(a)|(b)\", 2)",
        "regex_replace(\"a1b22\", \"[0-9]+\", \"#\")",
        "regex_replace(\"2026-01\", \"(\\\\d+)-(\\\\d+)\", \"$2/$1\")",
        "regex_replace(\"abc\", \"b\", s)",
        "pow(2, 10)",
        "pow(2.5, -1.5)",
        "pow(-8, 1.0 / 3)",
        "pow(0, 0)",
        "pow(n, 2)",
        "exp(1)",
        "exp(710)",
        "ln(0)",
        "ln(-1)",
        "ln(2.718281828)",
        "log10(1000)",
        "log10(0.001)",
        "log2(1024)",
        "log(81, 3)",
        "log(8, 0.5)",
        "log(x, 2)",
        "sin(1)",
        "cos(1)",
        "tan(1)",
        "asin(0.5)",
        "asin(2)",
        "acos(0.5)",
        "atan(1)",
        "atan2(1, -1)",
        "atan2(0, 0)",
        "degrees(3.14159)",
        "radians(180)",
        "is_nan(0.0 / 0.0)",
        "is_nan(1)",
        "is_nan(x)",
        "is_finite(1.0 / 0.0)",
        "is_finite(2)",
        "sign(-5)",
        "sign(0)",
        "sign(2.5)",
        "sign(0.0)",
        "sign(-0.0)",
        "sign(0.0 / 0.0)",
        "sign(-1.5d)",
        "sign(0d)",
        "sign(n)",
        "div(7, 2)",
        "div(-7, 2)",
        "div(7, -2)",
        "div(1, 0)",
        "div(-9223372036854775807 - 1, -1)",
        "div(n, 2)",
        "div(2, n)",
        "trunc(2.789)",
        "trunc(-2.789)",
        "trunc(-2.789, 1)",
        "trunc(1234.5, -2)",
        "trunc(2.789d, 2)",
        "trunc(-2.789d)",
        "trunc(5, 1)",
        "trunc(1e300, 400)",
        "trunc(x, 1)",
        "greatest(1, 5, 3)",
        "least(1, 5, 3)",
        "greatest(1, 2.5)",
        "least(\"b\", \"a\")",
        "greatest(@2026-01-01, @2026-03-01)",
        "greatest(@2026-01-01, @2026-01-01T10:00:00)",
        "greatest(1.5d, 2)",
        "least(0.0 / 0.0, 1.0)",
        "greatest(0.0 / 0.0, 1.0)",
        "greatest(1, n)",
        "least(n, 1, 2)",
        "greatest(hours(1), minutes(90))",
        "null_if(5, 5)",
        "null_if(5, 6)",
        "null_if(\"N/A\", \"N/A\")",
        "null_if(5, n)",
        "null_if(n, 5)",
        "null_if(-999.0, -999)",
        "null_if(1.50d, 1.5d)",
        "null_if(@2026-01-01, @2026-01-01)",
        "try_to_int(\"42\")",
        "try_to_int(\"4x\")",
        "try_to_int(\" 42\")",
        "try_to_int(3.9)",
        "try_to_int(-3.9)",
        "try_to_int(1e30)",
        "try_to_int(0.0 / 0.0)",
        "try_to_int(19.99d)",
        "try_to_int(true)",
        "try_to_int(s)",
        "try_to_float(\"1e3\")",
        "try_to_float(\"abc\")",
        "try_to_float(2)",
        "try_to_float(2.5d)",
        "try_to_decimal(\"1.25\")",
        "try_to_decimal(\"x\")",
        "try_to_decimal(0.1)",
        "try_to_decimal(1e40)",
        "try_to_date(\"2026-01-05\")",
        "try_to_date(\"2026-02-30\")",
        "try_to_date(\"05/01/2026\")",
        "try_to_date(@2026-01-05T10:00:00)",
        "try_to_datetime(\"2026-01-05 10:00:00\")",
        "try_to_datetime(\"nope\")",
        "try_to_datetime(@2026-01-05)",
        "to_bool(\"Yes\")",
        "to_bool(\"no\")",
        "to_bool(0)",
        "to_bool(1)",
        "to_bool(true)",
        "to_bool(\"maybe\")",
        "to_bool(2)",
        "to_bool(s)",
        "try_to_bool(\"maybe\")",
        "try_to_bool(2)",
        "try_to_bool(\" T \")",
        "try_to_bool(false)",
        "parse_number(\"1,234.50\")",
        "parse_number(\"45%\")",
        "parse_number(\"(120)\")",
        "parse_number(\"abc\")",
        "parse_number(\"\")",
        "parse_number(s)",
        "between(5, 1, 10)",
        "between(1, 1, 10)",
        "between(11, 1, 10)",
        "between(2.5, 1, 3)",
        "between(n, 1, 2)",
        "clamp(15, 0, 10)",
        "clamp(-3.5, 0, 10)",
        "clamp(n, 0, 1)",
        "pi()",
        "3 in [1, 2, 3]",
        "3 not in [1, 2, 3]",
        "1.5 in [1, 2]",
        "n in [1, 2]",
        "\"a\" in [\"a\", \"b\"]",
        "@2026-01-02 in [@2026-01-01]",
        "2 in [1.0, 2.0]",
        "1 in []",
        "year(@2026-12-25)",
        "month(@2026-12-25)",
        "day(@2026-12-25)",
        "year(@0001-01-01)",
        "year(@2026-12-25T10:00:00)",
        "hour(@2026-12-25T10:11:12)",
        "hour(@2026-12-25)",
        "days(2)",
        "hours(3)",
        "minutes(-90)",
        "seconds(1)",
        "days(1.5)",
        "days(9223372036854775807)",
        "days(n)",
        "total_seconds(minutes(2))",
        "total_seconds(seconds(0.25))",
        "to_date(\"2026-03-04\")",
        "to_datetime(@2026-03-04)",
    ];
    for call in calls {
        let run = |own: bool| {
            let mut repl = Repl::new();
            repl.0.vm().set_own_scalars(own);
            repl.run(&format!("{prelude}{call}"))
        };
        assert_eq!(run(true), run(false), "{call}");
    }
}
