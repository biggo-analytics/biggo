# biggo

biggo is a programming language for data analytics, with a compiler and an engine written in Rust.
A program reads data in as tables, transforms them with pipelines, and prints or writes the result.
Everything is type-checked before the program runs, including the name and type of every column,
and queries run on a columnar engine that uses every core of the machine.

**Status:** version 0.1.0, the first release. It works as documented, but the language can still
change, and it has been tested on macOS (Apple Silicon) only. See the
[known limitations](#known-limitations) and the [roadmap](ROADMAP.md).

```biggo
type Sale = { date: date, region: string, product: string, qty: int, price: float? }

read_csv<Sale>("docs/data/sales.csv")
  |> where(qty > 0 and date >= @2026-01-01)
  |> derive(revenue = qty * (price ?? 0.0))
  |> group(region, month = month(date))
  |> agg(total = sum(revenue), orders = count())
  |> sort(desc(total))
  |> take(3)
  |> print()
```

```text output
+--------+-------+-------+--------+
| region | month | total | orders |
+--------+-------+-------+--------+
| east   | 2     | 199.8 | 1      |
| north  | 3     | 100.0 | 1      |
| north  | 1     | 55.0  | 2      |
+--------+-------+-------+--------+
```

## Highlights

- **Errors are caught before the program runs.** The whole language is statically typed: a
  misspelled column name, a mismatched type, or a null you forgot to handle is a compile error
  with a location and a hint, before a single row of data is read.
- **Fast.** A query over a 5-million-row CSV file takes about 0.2 seconds on an 8-core laptop,
  in the same range as DuckDB and Polars ([Performance](#performance) shows where it is ahead
  and where it is behind). It computes column by column on Apache Arrow, has an optimizer, and
  runs every stage in parallel.
- **Reproducible results.** The result is identical bit for bit, however many threads are used.
- **A small but complete language.** Functions, lambdas, `match`, records, maps, lists, `import`,
  `decimal` for money, `datetime` and `duration` for time, regular expressions, and program
  arguments.
- **Reads and writes several formats.** CSV (any delimiter, encodings such as TIS-620, files
  with titles or without a header), Parquet, JSON, SQLite, and many files at once by a pattern.
  `biggo infer` writes the row type of a file for you.
- **All the tools in one executable.** A runner, a REPL, a formatter, a test runner, a language
  server, and a builder of standalone executables. A VS Code extension is in this repository.

## Getting started

You need Rust (tested with 1.99) and the system's C compiler.

```sh
git clone https://github.com/biggo-analytics/biggo.git && cd biggo
cargo build --release
cp target/release/biggo ~/.local/bin/      # or anywhere on your PATH

biggo run hello.bgo          # run a program
biggo repl                   # try things one line at a time
biggo check hello.bgo        # type-check only
```

Releases, starting with 0.2.0, come with executables built for macOS, Linux and Windows, so
that Rust is not needed. This puts the latest one in `~/.local/bin`, after checking it against
the checksums of the release:

```sh
curl -fsSL https://raw.githubusercontent.com/biggo-analytics/biggo/main/install.sh | sh
```

Read on: [Getting started](docs/01-getting-started.md)

## The language at a glance

Ordinary code: functions, lambdas, `match`, records and maps:

```biggo
fn tier(score: int?) -> string {
  match score {
    null => "no score"
    100 => "full marks"
    _ => if (score ?? 0) >= 50 { "pass" } else { "fail" }
  }
}

let students = [
  { name: "Ann", score: 100 },
  { name: "Bo", score: 45 },
  { name: "Cy", score: null },
]
each(students, fn(s) { print(s.name, tier(s.score)) })

let passed = filter(students, fn(s) { (s.score ?? 0) >= 50 })
print(map(passed, fn(s) { s.name }), len(passed))
```

```text output
Ann full marks
Bo fail
Cy no score
["Ann"] 1
```

Money and time: `decimal` has no floating-point error, and subtracting one `datetime` from
another gives a `duration`:

```biggo
print(0.1 + 0.2, 0.1d + 0.2d)

let start = @2026-01-31T22:30:00
let stop = @2026-02-01T01:15:30
print(stop - start, start + hours(36), to_date(stop))
```

```text output
0.30000000000000004 0.3
02:45:30 2026-02-02T10:30:00 2026-02-01
```

Tables: joins, windows, pivots and statistics:

```biggo
type Sale = { date: date, region: string, product: string, qty: int, price: float? }
let sales = read_csv<Sale>("docs/data/sales.csv")

// units per product, one column per region
print(sales |> group(product) |> pivot(region, ["north", "south", "east"], sum(qty)) |> sort(product))

// the best-selling product of each region
sales
  |> window(by = region, order = desc(qty), place = row_number())
  |> where(place == 1)
  |> select(region, product, qty)
  |> sort(region)
  |> print()

print(describe(sales |> select(qty, price)))
```

```text output
+---------+-------+-------+------+
| product | north | south | east |
+---------+-------+-------+------+
| gadget  | 3     | 5     | 1    |
| gizmo   | 1     | null  | 2    |
| widget  | 17    | 12    | 4    |
+---------+-------+-------+------+
+--------+---------+-----+
| region | product | qty |
+--------+---------+-----+
| east   | widget  | 4   |
| north  | widget  | 10  |
| south  | widget  | 12  |
+--------+---------+-----+
+--------+--------+-------+-------+--------------------+-------------------+-----+--------+-------+
| column | type   | count | nulls | mean               | stddev            | min | median | max   |
+--------+--------+-------+-------+--------------------+-------------------+-----+--------+-------+
| qty    | int    | 10    | 0     | 4.5                | 4.034572812303401 | 0.0 | 3.5    | 12.0  |
| price  | float? | 9     | 1     | 25.822222222222223 | 42.14584136595739 | 2.5 | 2.5    | 100.0 |
+--------+--------+-------+-------+--------------------+-------------------+-----+--------+-------+
```

The type of a table is tracked through the whole pipeline:

```biggo error
type Sale = { date: date, region: string, product: string, qty: int, price: float? }
read_csv<Sale>("docs/data/sales.csv")
  |> group(region)
  |> agg(units = sum(qty))
  |> where(product == "widget")
  |> print()
```

```text output
error: `product` is a built-in function, which can only be called; to pass it along, wrap it in a function: `fn(x) { product(x) }`
 --> example.bgo:5:12
  |
5 |   |> where(product == "widget")
  |            ^^^^^^^
```

After `agg` the table has only `region` and `units`. The compiler knows this, and says so before
anything runs.

Tests are written in the same language (`biggo test` runs every `test_...` function in the
`*_test.bgo` files):

```biggo
fn total(amounts: list<float>) -> float { fold(amounts, 0.0, fn(sum, x) { sum + x }) }

fn test_total() {
  assert_eq(total([1.5, 2.5]), 4.0)
  assert_eq(total([]), 0.0)
}
test_total()
print("ok")
```

```text output
ok
```

## Tools

| Command | What it does |
| --- | --- |
| `biggo run file.bgo` | Runs a program; `biggo run -e '...'` runs one given on the command line |
| `biggo repl` | Interactive mode, with line editing, history, `:type` and `:help` |
| `biggo check file.bgo` | Reports syntax and type errors without running the program |
| `biggo explain file.bgo` | Shows each query's plan before and after optimization, without running it |
| `biggo test [path]` | Runs the tests in `*_test.bgo` files |
| `biggo fmt [--check] files` | Formats code |
| `biggo build file.bgo -o app` | Builds a standalone executable |
| `biggo infer data.csv` | Prints the row type of a data file, so that it need not be typed |
| `biggo help substring` | What a built-in function does |
| `biggo lsp` | Language server for editors (errors as you type, hover, formatting) |

The VS Code extension is in [`editors/vscode`](editors/vscode).

## Performance

5 million rows on an Apple M1 Pro with 8 cores, in seconds. biggo's times include process
start-up; the others' do not.

| Query | biggo | DuckDB 1.5.6 | Polars 2.0.0 |
| --- | --- | --- | --- |
| Filter + group (CSV) | 0.21 | 0.27 | 0.15 |
| Group into 1 million groups (CSV) | 0.38 | 0.29 | 0.14 |
| Join + group (CSV) | 0.24 | 0.27 | 0.13 |
| Top 5 by a computed value (CSV) | 0.24 | 0.27 | 0.14 |
| Filter + group (Parquet) | 0.06 | 0.02 | 0.02 |
| Pivot (CSV) | 0.24 | 0.27 | 0.15 |
| Window over 1 million partitions (CSV) | 0.47 | 0.47 | 0.45 |
| Filter + group (JSON Lines) | 0.43 | 0.19 | 0.56 |
| Count distinct (CSV) | 0.22 | 0.30 | 0.19 |
| Decimal sum (CSV) | 0.18 | 0.25 | 0.14 |

The method, the detailed results, scaling with thread count, the speed of the VM, and the slow
spots are in [Performance](docs/09-performance.md).

## Documentation

| Page | Contents |
| --- | --- |
| [Getting started](docs/01-getting-started.md) | Building, a first program, the REPL |
| [The language](docs/02-language.md) | Syntax, variables, functions, lambdas, `if`, `match`, lists, records, maps, `import` |
| [The type system](docs/03-types.md) | Every type, nullable types, conversions, type inference |
| [Working with tables](docs/04-tables.md) | `where` `select` `group` `agg` `join` `window` `pivot`, statistics and more |
| [Data sources](docs/05-data-sources.md) | CSV, Parquet, JSON, SQLite |
| [Built-in reference](docs/06-builtins.md) | Every built-in function |
| [Tools](docs/07-tools.md) | Every command, `biggo test`, the formatter, editors |
| [Architecture](docs/08-architecture.md) | How the compiler and the engine work, and how to add a feature |
| [Performance](docs/09-performance.md) | Benchmarks, method, limitations |
| [Grammar](docs/10-grammar.md) | The formal grammar |
| [Roadmap](ROADMAP.md) | What comes next |

Every example program in the documentation is run by `cargo test`, and the output shown is
compared with the real output.

## Project layout

```text
crates/
  biggo-syntax/   lexer, parser, AST, diagnostics
  biggo-types/    type checker, HIR
  biggo-plan/     schemas, logical plans, optimizer
  biggo-exec/     execution engine on Apache Arrow
  biggo-eval/     bytecode compiler, virtual machine, sessions
  biggo-fmt/      formatter
  biggo-lsp/      language server
  biggo-cli/      the biggo command
ROADMAP.md        what comes next
docs/             documentation and sample data
editors/vscode/   VS Code extension
bench/            benchmarks, and scripts that compare with DuckDB / Polars / CPython
testdata/         sample programs and their expected output (golden files)
```

## Development

```sh
cargo test                     # unit tests, golden files, documentation, CLI
BIGGO_BLESS=1 cargo test       # rewrite the expected output (then read the diff)
cargo clippy --all-targets
cargo fmt
python3 bench/run.py           # benchmarks (build with --release and generate the data first)
```

## Known limitations

- The data of a query that sorts a whole table, uses `window`, is the right side of a `join`, or
  groups into many groups has to fit in RAM (nothing spills to disk yet).
- SQLite is the only database. A CSV file needs a header line. JSON has to be one object per
  line.
- There are no `for` or `while` loops (use `map`, `filter`, `fold`, `each`, or recursion), and a
  variable cannot be assigned a new value.
- A user-defined function cannot take a column: a column expression can use only operators and
  built-in functions.
- The language server has no autocomplete or go-to-definition yet. The VS Code extension has been
  packaged and its grammar tested, but it has not been run inside VS Code.
- Tested on macOS (Apple Silicon) only.

## Roadmap

What comes next, in the order it will be built and with no dates. [ROADMAP.md](ROADMAP.md) lists
every function and feature that is planned, with its priority.

| Release | Goal | Main work |
| --- | --- | --- |
| 0.2 | The standard library | `in`, math, safe conversion, date functions, quantiles and other aggregates, more window functions, `count_by`, sampling, list functions, number formatting; CI and prebuilt binaries |
| 0.3 | Data in and out | `biggo infer`, Excel, files without a header, many files with `*`, PostgreSQL and MySQL, Markdown and HTML output |
| 0.4 | A language with more reuse | User-defined functions on columns, functions that accept more than one table shape, generics, `import ... as` |
| 0.5 | A faster engine, and data larger than memory | Parquet row-group skipping, grouping into many groups, parallel `join`, spilling to disk |
| 1.0 | Stable | A frozen language, fuzzing, checking results against DuckDB |

Found a bug, or want to propose a feature? Open an issue at
<https://github.com/biggo-analytics/biggo/issues>.

## License

biggo is released under the [MIT License](LICENSE).
