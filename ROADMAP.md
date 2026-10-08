# biggo roadmap

This document says what biggo can do today, what it lacks, and what comes next, in order.

- The current version is **0.1.0**, the first release. Until 1.0, the language and the commands
  can change without staying compatible with earlier versions.
- Items are listed in order of priority. There are **no dates**, and the order can change with
  the problems people actually run into.
- Every item is something the code does **not** do today. What exists is described in the
  [README](README.md) and in [docs](docs).

## Contents

- [Where things stand](#where-things-stand-010)
- [Principles](#principles)
- [0.2: Easy to install, works everywhere](#02-easy-to-install-works-everywhere)
- [0.3: More data sources](#03-more-data-sources)
- [0.4: A faster engine, and data larger than memory](#04-a-faster-engine-and-data-larger-than-memory)
- [0.5: A language with more reuse](#05-a-language-with-more-reuse)
- [Tooling](#tooling-alongside-every-release)
- [1.0: Stable](#10-stable)
- [Not planned for now](#not-planned-for-now)
- [Feedback](#feedback)

## Where things stand (0.1.0)

| Area | What exists |
| --- | --- |
| Language | Static typing throughout, functions, lambdas, `match`, lists, records, maps, `import` |
| Types | `int` `float` `bool` `string` `date` `datetime` `duration` `decimal`, and a nullable form of each |
| Tables | `where` `select` `derive` `group` + `agg` `join` (6 kinds) `sort` `window` `pivot` `unpivot` `explode` `union` `distinct`, and statistics (`corr` `linreg` `describe` `histogram`) |
| Data sources | CSV, Parquet, JSON Lines, SQLite, for both reading and writing |
| Engine | Column-at-a-time execution on Apache Arrow, a rule-based optimizer, every stage in parallel, the same result for any number of threads |
| Tools | `run` `repl` `check` `explain` `test` `fmt` `build` `lsp`, and a VS Code extension |
| Testing | 125 automated tests, one of which runs all 94 example programs in the documentation and compares their output with what the documentation shows |

The main limitations of this release, which the plan below comes from:

- It has been tested on macOS (Apple Silicon) only. There is no CI and there are no prebuilt
  binaries: you build it yourself with Rust.
- The VS Code extension has been packaged and its grammar tested, but it has never been run
  inside VS Code.
- SQLite is the only database, and CSV files can only be comma-separated.
- The data of a `sort`, a `window`, the right side of a `join`, or a `group` with many groups has
  to fit in RAM.
- Some work is slower than in other engines: reading Parquet is 2.6 times slower than DuckDB and
  Polars, grouping into 1 million groups is 2.7 times slower than Polars, and reading JSON is 2.2
  times slower than DuckDB (the numbers and the method are in
  [docs/09-performance.md](docs/09-performance.md)).
- A user-defined function cannot be used on a column, and there are few string functions.

## Principles

No new work may break any of these. When two of them conflict, the one listed first wins.

1. **Errors are caught before the program runs.** The name and type of every column must be
   checkable at compile time. A feature that makes the type of a table known only at run time
   does not go into the core language.
2. **Results are reproducible.** The same query on the same data gives the same result, bit for
   bit, for any number of threads.
3. **Engine speed comes before convenience in writing the engine**, when the two pull apart.
4. **The documentation is true.** Every example in it is run by `cargo test`, so a new feature
   always arrives with documentation and tests.
5. **One file does it all.** The runner, the formatter, the test runner and the language server
   are one executable, with nothing else to install.

## 0.2: Easy to install, works everywhere

**Goal:** someone without Rust can install biggo and use it on their own data files, on Linux,
macOS and Windows.

Installation and compatibility

- [ ] CI on GitHub Actions: run `cargo test`, `cargo clippy` and `cargo fmt --check` on Linux,
      macOS and Windows for every push and pull request
- [ ] Find and fix behavior that differs between systems, such as Windows paths and files with
      CRLF line endings
- [ ] Prebuilt binaries in GitHub Releases for macOS (arm64, x86_64), Linux (x86_64, arm64) and
      Windows (x86_64), with an install script and a Homebrew formula
- [ ] The VS Code extension: try it in VS Code, fix what turns up, then publish it to the
      Marketplace and Open VSX
- [ ] Try the Neovim and Helix settings written in [docs/07-tools.md](docs/07-tools.md) and make
      them work

Gaps you hit right away with real data

- [ ] Programs can read command-line arguments and environment variables, so one script works
      for many files and many periods
- [ ] String functions: substring, replace, split, pad, find a position, and regular expressions
      (match, extract, replace)
- [ ] CSV options: other separators (tab, `;`, `|`), files with no header line, choosing which
      text means null, and encodings other than UTF-8 (such as TIS-620 / Windows-874, which is
      common in Thai data)
- [ ] JSON files that are one array (`[{...}, {...}]`), in addition to JSON Lines
- [ ] `biggo infer data.csv`: read a sample of the data and print a `type` declaration to copy,
      instead of typing the type of every column by hand

A proposed form (it may change):

```text
// report.bgo
let month = args()[0]                       // biggo run report.bgo -- 2026-01
let sales = read_csv<Sale>("sales.tsv", delimiter = "\t")
sales |> where(starts_with(to_string(date), month)) |> print()
```

**Done when:** CI passes on all three systems, biggo installs with one command and no Rust, and
the extension installs from the Marketplace.

## 0.3: More data sources

**Goal:** read data from where it actually lives, without exporting it to CSV first.

- [ ] PostgreSQL and MySQL through `read_sql` / `write_sql` with a connection URL, where the
      password comes from an environment variable and is never written in the program
- [ ] Push biggo's `where`, `select` and `take` into the SQL so the database does that work.
      This includes SQLite, where today the filter runs after the rows have been read out
- [ ] Excel (`.xlsx`), for both reading and writing
- [ ] Many files in one call, such as `read_csv<T>("logs/2026-*.csv")`, and partitioned Parquet
      directories
- [ ] Compressed files: `.csv.gz`, `.json.gz`, `.zst`
- [ ] Reading from `https://` and S3-style object storage, fetching only the byte ranges of a
      Parquet file that the query uses
- [ ] Arrow IPC (Feather), to hand data to Python and R without converting it
- [ ] Nested columns: let `list<T>` and records be columns, to read JSON and Parquet with nested
      structure (today a column can only have a basic type, and `explode` works on a string of
      separated values)

**Done when:** every new source has a round-trip test (write, read back, and get the same values
for every data type), as the four current formats do, and a documentation page with examples
that run.

## 0.4: A faster engine, and data larger than memory

**Goal:** close the gap with DuckDB and Polars where biggo clearly loses today, and stop failing
when the data is larger than memory.

| Work | Today (5 million rows, M1 Pro) | Target |
| --- | --- | --- |
| Parquet: skip row groups using their min/max statistics, and decode only the rows that pass the filter | 2.6 times slower than DuckDB and Polars | Within 1.5 times |
| `group` with many groups: a fast path when the key is a single `int` or `string`, instead of encoding the key as bytes every time | 2.7 times slower than Polars | Within 1.5 times |
| JSON: biggo's own reader in place of Arrow's general one | 2.2 times slower than DuckDB | Within 1.5 times |
| VM: make function and lambda calls cheaper | 1.0 to 1.5 times slower than CPython | Not slower than CPython on any of the three benchmark programs |

- [ ] `join`: build the hash table in parallel, and pick the smaller side as the build side
      automatically (today it is built on one thread, and the author has to put the small table
      on the right)
- [ ] An optimizer that uses statistics: row counts and min/max from file metadata, to choose the
      order of joins
- [ ] Data larger than RAM: an external `sort`, a `group` and a `join` that spill to disk past a
      limit, and a setting for the memory limit
- [ ] A table that is used more than once reads its file once, automatically (today you have to
      call `collect` yourself)
- [ ] SQLite: read in parallel by splitting the rowid range
- [ ] Wider benchmarks: the TPC-H queries, data larger than RAM, measurements on Linux, and a run
      in CI to catch speed regressions

Everything in this release has to keep principle 2. Each fast path gets a test that confirms it
gives the same result as the plain path for every value, as the parallel sort and the VM's
scalar functions already have.

**Done when:** the numbers in the table reach their targets when measured with `bench/run.py`
and `bench/compare.py`, and a query that sorts data twice the size of RAM runs to the end.

## 0.5: A language with more reuse

**Goal:** move repeated logic into functions and imported files, without losing type checking
and without getting slower.

- [ ] **User-defined functions on columns.** Today `derive(m = margin(price, cost))` is a compile
      error, because a column expression can use only operators and built-in functions. The plan
      is to inline the function's body into the query plan at compile time, so it is as fast as
      writing the expression out.
- [ ] **Functions that take a table with "at least" the named columns.** Today a parameter of
      type `table<{region: string, qty: int}>` rejects a table that has extra columns, so you
      cannot write a pipeline once and reuse it across tables.
- [ ] User-defined generic functions, such as `fn first<T>(xs: list<T>) -> T?`
- [ ] Named imports: `import "lib/geo.bgo" as geo`, then `geo.area(...)` (today the names of
      every file share one namespace)
- [ ] A fuller `match`: ranges of values, guards, and taking a record's fields apart (today a
      pattern can only be a literal or `_`)
- [ ] String interpolation
- [ ] Time zones for `datetime`, and a `decimal` with a chosen number of decimal places (today it
      is fixed at 6)
- [ ] User-defined aggregates
- [ ] A `pivot` that finds the column values at run time, for the REPL, where the result is
      printed at once and not passed on (in a program the values still have to be listed, by
      principle 1)

**Done when:** the documentation examples that repeat one expression in several places can be
rewritten with a function, and the existing benchmarks are no slower.

## Tooling (alongside every release)

- [ ] Language server: autocomplete (including the column names available at that point in a
      pipeline, which the type checker already knows), go to definition, rename, and showing the
      type of the table between the stages of a pipeline
- [ ] REPL: line editing with the arrow keys, and recalling earlier input
- [ ] `biggo test`: choose tests by name, and run several files at once
- [ ] Charts: functions that write a chart from a table as an SVG or HTML file
- [ ] A Jupyter kernel, to use biggo in notebooks
- [ ] A web playground (compiled to WebAssembly), to try the language without installing it

## 1.0: Stable

**Goal:** use biggo for routine work without worrying that the next version breaks existing
programs.

- [ ] Freeze the language: a program that passes `biggo check` in 1.0 must pass, and give the
      same result, in every 1.x release
- [ ] A deprecation policy: anything to be removed gets a warning at least one release ahead
- [ ] Fuzzing of the lexer, the parser and the type checker, and comparing the results of
      randomly generated queries with DuckDB, to find wrong results
- [ ] A security review of the parts that touch the outside world: database connections, file
      paths, and the executables that `biggo build` produces
- [ ] A code for each kind of error, with a page that explains the cause and the fix

## Not planned for now

None of these is rejected for good, but none will be done until there is a real use that cannot
be met with what exists.

- **Running across several machines.** biggo aims to use one machine fully.
- **`for` / `while` loops and variables that can be reassigned.** The language uses `map`,
  `filter`, `fold`, `each` and recursion, and every value is immutable, which keeps type checking
  and parallel execution simple.
- **A JIT for the VM.** Heavy work belongs in tables, which the engine already runs fast. The VM
  is there to put pipelines together.
- **Being a general-purpose language**, for writing web servers or programs with a user
  interface, for example.

## Feedback

Found a bug, or want a feature? Open an issue at <https://github.com/biggo-analytics/biggo/issues>.

- **Bugs:** attach the shortest `.bgo` program that shows the problem, a few rows of sample data,
  what you got, and what you expected.
- **Features:** describe the work you are doing and where you got stuck, rather than proposing a
  syntax, and say which part of this roadmap it belongs to, if any.

If you want to change the code yourself, [docs/08-architecture.md](docs/08-architecture.md)
explains how the compiler and the engine are built, and the steps for adding a built-in function
or a table operation.
