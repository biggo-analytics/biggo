# biggo roadmap

This document says what biggo can do today, lists everything it still needs, and gives the order
in which that will be built.

- The current version is **0.1.0**, the first release. Until 1.0, the language and the commands
  can change without staying compatible with earlier versions.
- Work is listed in the order it will be done. There are **no dates**, and the order can change
  with the problems people actually run into.
- An item that is ticked is done. Every other item is something the code does **not** do today.
  What exists is described in the [README](README.md) and in [docs](docs).
- The names and signatures in the [catalog](#catalog) are proposals. They can change when the
  feature is built, and the documentation of the built feature is what counts.

## Contents

- [Where things stand](#where-things-stand-010)
- [Principles](#principles)
- [Releases at a glance](#releases-at-a-glance)
- [Order of work](#order-of-work)
- [Catalog](#catalog)
  - [1. Expressions, math and safe conversion](#1-expressions-math-and-safe-conversion)
  - [2. Dates and times](#2-dates-and-times)
  - [3. Strings and formatting](#3-strings-and-formatting)
  - [4. Aggregates](#4-aggregates)
  - [5. Window functions](#5-window-functions)
  - [6. Table operations](#6-table-operations)
  - [7. Lists, maps and records](#7-lists-maps-and-records)
  - [8. Getting data in and out](#8-getting-data-in-and-out)
  - [9. Output and reports](#9-output-and-reports)
  - [10. Language](#10-language)
  - [11. Tooling](#11-tooling)
  - [12. Engine](#12-engine)
  - [13. Stability](#13-stability)
- [Not planned for now](#not-planned-for-now)
- [Feedback](#feedback)

## Where things stand (0.1.0)

| Area | What exists |
| --- | --- |
| Language | Static typing throughout, functions, lambdas, `match`, lists, records, maps, `import`, string functions and regular expressions, program arguments |
| Types | `int` `float` `bool` `string` `date` `datetime` `duration` `decimal`, and a nullable form of each |
| Tables | `where` `select` `derive` `group` + `agg` `join` (6 kinds) `sort` `window` `pivot` `unpivot` `explode` `union` `distinct`, and statistics (`corr` `linreg` `describe` `histogram`) |
| Data sources | CSV (any delimiter, and encodings other than UTF-8), Parquet, JSON Lines, SQLite, for both reading and writing |
| Engine | Column-at-a-time execution on Apache Arrow, a rule-based optimizer, every stage in parallel, the same result for any number of threads |
| Tools | `run` `repl` `check` `explain` `test` `fmt` `build` `lsp`, and a VS Code extension |
| Testing | 133 automated tests, one of which runs all 99 example programs in the documentation and compares their output with what the documentation shows |

The main limitations of this release, which the plan below comes from:

- The function library is thin next to SQL, pandas or Polars: there is no `in`, no power or
  logarithm, little for dates beyond their parts, no quantile, few window functions, and one
  value that cannot be converted stops the whole query.
- Every file needs its row type written by hand, and Excel files cannot be read.
- A user-defined function cannot be used on a column.
- It has been tested on macOS (Apple Silicon) only. There is no CI and there are no prebuilt
  binaries: you build it yourself with Rust.
- The VS Code extension has been packaged and its grammar tested, but it has never been run
  inside VS Code.
- SQLite is the only database.
- The data of a `sort`, a `window`, the right side of a `join`, or a `group` with many groups has
  to fit in RAM.
- Some work is slower than in other engines: reading Parquet is 2.6 times slower than DuckDB and
  Polars, grouping into 1 million groups is 2.7 times slower than Polars, and reading JSON is 2.2
  times slower than DuckDB (the numbers and the method are in
  [docs/09-performance.md](docs/09-performance.md)).

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

## Releases at a glance

| Release | Goal | What it contains |
| --- | --- | --- |
| 0.2 | **The standard library.** What an analyst expects a data tool to have built in | Catalog sections 1 to 7, plus CI and prebuilt binaries from section 11 |
| 0.3 | **Data in and out.** Read data where it lives, and hand results on | Sections 8 and 9 |
| 0.4 | **A language with more reuse.** Write logic once, use it on any table | Section 10 |
| 0.5 | **A faster engine, and data larger than memory** | Section 12 |
| 1.0 | **Stable.** Programs keep working from one version to the next | Section 13 |

Tooling (section 11) moves along with every release.

The engine comes after the library and the language on purpose. Its speed is already in the range
of DuckDB on most of the queries measured, while a missing function stops people on their first
day.

## Order of work

Each package is built whole, in this order. A package is done when every function in it has a
type rule, gives the same answer on a column as on a single value, handles null, has an entry in
[docs/06-builtins.md](docs/06-builtins.md) with an example that runs, has its mistakes covered in
`testdata/check`, and leaves the existing benchmarks no slower.

| # | Package | Catalog | Why here |
| --- | --- | --- | --- |
| 1 | Expressions, math, safe conversion, raw strings | [1](#1-expressions-math-and-safe-conversion) | The smallest pieces, used by everything after. `try_to_int` and its family end the "one bad value stops the query" problem |
| 2 | Dates and times | [2](#2-dates-and-times) | Almost every real data set is grouped by period, or holds dates that are not written the ISO way |
| 3 | Aggregates | [4](#4-aggregates) | Quantiles, conditional counts and joined strings are in every report |
| 4 | Window functions | [5](#5-window-functions) | Growth, running totals and gap filling for time series |
| 5 | Table operations | [6](#6-table-operations) | `count_by`, sampling, set operations, checks on data quality |
| 6 | Strings and formatting | [3](#3-strings-and-formatting) | Numbers formatted for people, and the string functions that SQL and spreadsheets have |
| 7 | Lists, maps and records | [7](#7-lists-maps-and-records) | Ordinary code catches up with what tables can do |
| 8 | CI, prebuilt binaries, the extension tried in VS Code | [11](#11-tooling) | Completes 0.2. It can be done at any point, since nothing else depends on it |
| 9 | `biggo infer`, the remaining CSV and JSON layouts, many files, `env` | [8](#8-getting-data-in-and-out) | Removes the first obstacle of a new user: writing every column type by hand |
| 10 | Excel | [8](#8-getting-data-in-and-out) | Where most analysts' data is |
| 11 | Output: more rows, Markdown, HTML | [9](#9-output-and-reports) | Results that go into a document or a message |
| 12 | PostgreSQL and MySQL | [8](#8-getting-data-in-and-out) | Completes 0.3 |
| 13 | User-defined functions on columns, tables with extra columns | [10](#10-language) | The largest limit of the language today |
| 14 | The rest of the language | [10](#10-language) | Completes 0.4 |
| 15 | Engine speed and spilling to disk | [12](#12-engine) | Completes 0.5 |
| 16 | Stability | [13](#13-stability) | Completes 1.0 |

## Catalog

Everything biggo should have, by area. Each table lists what is missing. The line above it lists
what exists, for context.

Priority: **Must** is something every data tool has, and whose absence people hit in their first
week. **Should** is commonly expected. **Later** is useful but not part of the baseline.

Every scalar function works on a single value and on a column, and gives null for a null argument
unless its row says otherwise.

### 1. Expressions, math and safe conversion

Exists: `+ - * / %`, comparisons, `in` and `not in`, `and` `or` `not`, `??`, `if`, `match`,
`is_null`, `abs` `round` `trunc` `floor` `ceil` `sqrt` `sign` `div` `pow` `exp` `ln` `log10` `log2`
`log`, `greatest` `least` `clamp` `between`, `is_nan` `is_finite`, `pi` and the trigonometric
functions, `to_int` `to_float` `to_decimal` `to_string` `to_date` `to_datetime` `to_bool` and the
`try_` form of each, `parse_number`, `null_if`, raw strings.

| Feature | Result | What it does | Priority |
| --- | --- | --- | --- |
| `recode(x, mapping)`, `recode(x, mapping, default)` | value type of the map | Replaces codes by labels with a map: `recode(sex, { "M": "male", "F": "female" })` | Should |
| `cut(x, edges)`, `cut(x, edges, labels)` | `string?` | The range a number falls in, for age groups, price bands and the like: `cut(age, [0, 18, 65])` | Should |
| interpolated strings: `f"total: {x}"` | `string` | Values written into text without `+` and `to_string` | Should |

- [x] `in` and `not in`, `greatest` and `least`, `null_if`, the `try_` conversions and `to_bool`
- [x] `pow`, `exp`, `ln`, `log10`, `log2`, `log`, `sign` (which keeps the type of its argument),
      `div`, `trunc`, `clamp`, `between`, `is_nan`, `is_finite`, `pi` and trigonometry
- [x] `parse_number`, and raw strings such as `r"\d+"`
- [x] `hash`, a stable number for a value

### 2. Dates and times

Exists: date and datetime literals, `year` `month` `day` `hour` `minute` `second`, `to_date`
`to_datetime`, `days` `hours` `minutes` `seconds`, `total_seconds` `total_minutes` `total_hours`
`total_days`, adding and subtracting dates and durations, `today` `now`, `weekday` `week` `quarter`
`day_of_year` `month_name` `day_name` `is_weekend`, `start_of_week` `start_of_month`
`start_of_quarter` `start_of_year` `end_of_month`, `add_days` `add_months` `add_years`,
`days_between` `months_between` `years_between`, `make_date` `make_datetime`, `format_date`
`parse_date` `parse_datetime` and their `try_` forms (with Buddhist-era years), `time_bucket`,
`to_unix` `from_unix`, `fiscal_year`, `buddhist_year`.

| Feature | Result | What it does | Priority |
| --- | --- | --- | --- |
| time zones: `to_zone(t, "Asia/Bangkok")`, zones kept when reading | | Today a zone in a file is converted to UTC and dropped | Later |
| `add_business_days(d, n)`, `business_days_between(a, b)`, with a table of holidays | | Working-day arithmetic | Later |

- [x] `today` and `now`, the parts of a date (`weekday`, `week`, `quarter`, `day_of_year`, names),
      the first and last days of periods, and `time_bucket`
- [x] Calendar arithmetic: `add_days`, `add_months`, `add_years`, and the `_between` functions
- [x] `make_date`, `make_datetime`, `format_date`, `parse_date`, `parse_datetime` and the `try_`
      forms, with `era = "buddhist"` for Thai dates; `fiscal_year`, `to_unix`, `from_unix`

### 3. Strings and formatting

Exists: `length` `lower` `upper` `title` `trim` `trim_left` `trim_right` `contains` `starts_with`
`ends_with` `index_of` `like` `substring` `left` `right` `replace` `split` `split_part` `pad_left`
`pad_right` `repeat` `reverse` `concat` `format_number` `format_percent` `sha256` `md5`
`regex_match` `regex_extract` `regex_replace` `regex_count`, and `+`. Tables print with their
columns lined up by display width, for Thai and other scripts too.

| Feature | Result | What it does | Priority |
| --- | --- | --- | --- |
| `json_get(s, path)` | `string?` | A value out of JSON text that sits in a string column | Should |
| `levenshtein(a, b)`, `similarity(a, b)` | `int`, `float` | How alike two strings are, for matching names that are spelled differently | Later |
| `normalize(s)`, `remove_accents(s)` | `string` | One Unicode form for text that looks the same | Later |

- [x] `substring`, `replace`, `split`, `split_part`, `pad_left`, `pad_right`, `index_of`
- [x] Regular expressions: `regex_match`, `regex_extract`, `regex_replace`, `regex_count`
- [x] `format_number`, `format_percent`, `concat`, `like`
- [x] `trim_left`, `trim_right`, trimming other characters, `left`, `right`, `repeat`, `reverse`,
      `title`
- [x] `sha256`, `md5`

### 4. Aggregates

Exists: `sum` `mean` `min` `max` `count` `count_distinct` `first` `last` `median` `quantile`
`stddev` `variance` `stddev_pop` `variance_pop` `product` `count_if` `any` `all` `count_null`
`string_agg` `arg_max` `arg_min` `weighted_mean` `corr` `cov` `slope` `intercept`.

| Feature | Result | What it does | Priority |
| --- | --- | --- | --- |
| `mode(x)` | type of `x` | The most frequent value | Should |
| `skewness(x)`, `kurtosis(x)` | `float?` | The shape of a distribution | Later |
| `approx_count_distinct(x)` | `int` | A fast estimate for very many distinct values | Later |

- [x] `quantile`, `variance`, `stddev_pop`, `variance_pop`, `product`
- [x] `count_if`, `any`, `all`, `count_null`, `string_agg`
- [x] `arg_max`, `arg_min`, `weighted_mean`

### 5. Window functions

Exists: `row_number` `rank` `dense_rank` `percent_rank` `ntile`, `lag` `lead` `diff` `pct_change`,
`fill_forward` `fill_backward`, `cumsum` `cum_count` `cum_min` `cum_max` `cum_mean`, `moving_avg`
`moving_sum` `moving_min` `moving_max`, and every aggregate of one column over a whole partition.

| Feature | Result | What it does | Priority |
| --- | --- | --- | --- |
| `moving_stddev(x, n)` | `float?` | Volatility over the last `n` rows | Should |
| windows by time: `moving_avg(x, over = days(7))` | | The last 7 days, however many rows that is | Should |
| `cume_dist()` | `float` | The share of rows at or below this one | Later |
| `ewm_mean(x, alpha)` | `float?` | An exponentially weighted mean | Later |

- [x] `diff`, `pct_change`, `fill_forward`, `fill_backward`
- [x] `cum_count`, `cum_min`, `cum_max`, `cum_mean`, `moving_sum`, `moving_min`, `moving_max`
- [x] `dense_rank`, `percent_rank`, `ntile`

### 6. Table operations

Exists: `where` `select` `drop` `rename` `derive` `sort` (with nulls first or last) `take` `skip`
`tail` `sample` `distinct` `count_by` `drop_nulls` `fill_nulls` `group` + `agg` `join` (inner, left,
right, full, semi, anti, cross) `window` `union` `intersect` `except` `pivot` `unpivot` `explode`
`collect` `count` `columns` `describe` `histogram` `linreg` `to_rows` `from_rows`. A column that
numbers the rows is `window(n = row_number())`.

| Feature | Result | What it does | Priority |
| --- | --- | --- | --- |
| totals: `agg(..., totals = true)` | table | A row of subtotals for each group and a grand total | Should |
| `date_range(first, last)`, `complete(t, a, b)` | table | Every day of a period, and every combination of values, so that gaps in data show as rows | Should |
| checks: `assert_unique(t, a, ...)`, `assert_no_nulls(t, a, ...)` | | Stops the program when the data breaks an assumption the query relies on | Should |
| joins on a range or an inequality, and the nearest earlier row (`asof`) | table | Prices in force on a date, the reading before an event | Later |
| `select` by pattern or by type | table | `select(matching("^q[0-9]"))`, worked out at compile time | Later |
| `transpose(t)` | table | Rows as columns | Later |

- [x] `count_by`, `sample` (the same rows for the same seed), `tail`
- [x] Cross joins, `intersect` and `except`, and `union` by column name
- [x] `drop_nulls`, `fill_nulls`, `sort(..., nulls = "first")`, `columns`

### 7. Lists, maps and records

Exists: `len` `range` `map` `filter` `fold` `each` `split`, `xs[i]`, `xs + ys`, `keys` `values`
`put` `has_key`, `m[k]`, `r.field`.

- [x] `sort(xs)`, `sort(xs, desc = true)`, `sort_by(xs, f)`
- [x] `sum(xs)`, `min(xs)`, `max(xs)`, `mean(xs)`
- [x] `contains(xs, x)`, `index_of(xs, x)`, `join(xs, separator)`
- [x] `reverse(xs)`, `distinct(xs)`, `take(xs, n)`, `skip(xs, n)`, `slice(xs, start, length)`
- [x] `first(xs)`, `last(xs)`, `find(xs, f)`, `any(xs, f)`, `all(xs, f)`, `count(xs, f)`
- [x] `flatten(xss)`
- [x] `remove(m, k)`, `merge(a, b)`, `entries(m)`

These take the names that tables and strings already use, and which one a call means shows in
its first argument.

| Feature | Result | What it does | Priority |
| --- | --- | --- | --- |
| record update: `{ ...r, a: 1 }` | record | A copy of a record with some fields changed | Should |
| `zip(xs, ys)`, `group_by(xs, f)` | | Pairs of two lists, and a list split into a map | Later |

### 8. Getting data in and out

Exists: `read_csv` and `write_csv` (with `delimiter` and `encoding`), `read_parquet`
`write_parquet`, `read_json` `write_json` (JSON Lines), `read_sql` `write_sql` (SQLite), `args()`.

| Feature | What it does | Priority |
| --- | --- | --- |
| `biggo infer data.csv` | Reads a sample of a file and prints the `type` declaration for it, to copy into a program, instead of typing the type of every column by hand | Must |
| `read_csv(..., header = false)` | Files with no header line. Columns are taken in the order of the row type | Must |
| `read_csv(..., nulls = ["NA", "-"])` | Which text means null | Must |
| `read_csv(..., skip = 3)` | Lines to pass over before the header, for exports that start with a title | Must |
| JSON files that are one array | `[{...}, {...}]`, in addition to JSON Lines | Must |
| many files in one call | `read_csv<T>("logs/2026-*.csv")`, with the file name available as a column | Must |
| `read_excel<T>(path)`, with `sheet`, `skip` and `range` | Excel workbooks, where most analysts' data is | Must |
| `env(name)` | An environment variable as a `string?`, for paths and passwords that do not belong in a program | Must |
| `write_excel(t, path, sheet = "...")` | A result as a workbook | Should |
| `read_csv(..., date_format = "%d/%m/%Y", decimal = ",")` | Dates and numbers in the form the file uses, read straight into `date` and `float` columns | Should |
| `read_csv(..., on_error = "null")` | A value that does not fit its column becomes null, and the number of such values is reported, instead of the first one stopping the query | Should |
| compressed files | `.csv.gz`, `.json.gz`, `.zst` | Should |
| standard input and output | `read_csv<T>("-")` and `write_csv(t, "-")`, to use biggo between other commands | Should |
| PostgreSQL and MySQL | Through `read_sql` and `write_sql` with a connection URL, where the password comes from `env` | Should |
| pushdown | biggo's `where`, `select` and `take` become part of the SQL, so the database does that work. This includes SQLite, where today the filter runs after the rows are read out | Should |
| appending | `write_csv(t, path, append = true)`, `write_sql(t, path, name, mode = "append")` | Should |
| files as values | `exists(path)`, `list_files(pattern)`, `read_text(path)`, `write_text(path, s)` | Should |
| nested JSON | A field inside an object, named by its path in the row type | Should |
| SQL Server | Through `read_sql` | Later |
| partitioned Parquet | Reading and writing a directory split by the values of a column | Later |
| `https://` and S3 | Reading only the byte ranges of a Parquet file that the query uses | Later |
| Arrow IPC (Feather) | Handing data to Python and R without converting it | Later |
| nested columns | `list<T>` and records as columns. Today a column can only have a basic type | Later |

- [x] Programs can read their command-line arguments with `args()`
- [x] CSV files with another delimiter (tab, `;`, `|`) and in encodings other than UTF-8 (such as
      TIS-620 / Windows-874), for both reading and writing

### 9. Output and reports

Exists: `print` (the first 50 rows of a table), `explain`, the four `write_` functions.

| Feature | What it does | Priority |
| --- | --- | --- |
| `print(t, rows = 200)` | More or fewer rows than 50 | Must |
| `to_markdown(t)`, `write_markdown(t, path)` | A table for a document, an issue or a chat message | Should |
| `write_html(t, path)` | A table as a page, with numbers aligned | Should |
| number display | How many decimal places a `float` column prints with | Should |
| charts | Functions that write a chart from a table as an SVG or HTML file | Later |
| a Jupyter kernel | biggo in notebooks | Later |

### 10. Language

Exists: functions, lambdas, `if`, `match` on literals, lists, records, maps, `import`, named
arguments, type inference for lambdas.

| Feature | What it does | Priority |
| --- | --- | --- |
| user-defined functions on columns | Today `derive(m = margin(price, cost))` is a compile error, because a column expression can use only operators and built-in functions. The function's body is inlined into the query plan at compile time, so it is as fast as writing the expression out | Must |
| functions that take a table with "at least" the named columns | Today a parameter of type `table<{region: string, qty: int}>` rejects a table that has extra columns, so a pipeline cannot be written once and reused across tables | Must |
| default values for parameters | `fn top(t: table<Sale>, n: int = 10)` | Should |
| user-defined generic functions | `fn first<T>(xs: list<T>) -> T?` | Should |
| named imports | `import "lib/geo.bgo" as geo`, then `geo.area(...)`. Today the names of every file share one namespace | Should |
| a fuller `match` | Ranges of values, guards, and taking a record's fields apart. Today a pattern can only be a literal or `_` | Should |
| `fail(message)` | Stops the program with a message of its own | Should |
| documentation comments | A comment above a function, shown when the editor hovers over a call | Should |
| a `decimal` with a chosen number of decimal places | Today it is fixed at 6 | Later |
| user-defined aggregates | An aggregate written in biggo | Later |
| a `pivot` that finds its column values at run time | For the REPL, where the result is printed at once and not passed on. In a program the values still have to be listed, by principle 1 | Later |

### 11. Tooling

Exists: `run` `repl` `check` `explain` `test` `fmt` `build` `lsp` `parse`, and a VS Code extension
with a grammar.

| Feature | What it does | Priority |
| --- | --- | --- |
| CI on GitHub Actions | `cargo test`, `cargo clippy` and `cargo fmt --check` on Linux, macOS and Windows for every push and pull request, and fixes for what differs between systems | Must |
| prebuilt binaries | In GitHub Releases for macOS (arm64, x86_64), Linux (x86_64, arm64) and Windows (x86_64), with an install script and a Homebrew formula | Must |
| the VS Code extension, verified | Tried in VS Code, fixed, and published to the Marketplace and Open VSX | Must |
| REPL: line editing | Arrow keys and earlier input | Must |
| `biggo help substring` | The reference entry of a built-in function in the terminal, and `:help` in the REPL | Should |
| `biggo run -e '...'` | A program given on the command line or on standard input, for one-off questions | Should |
| language server: more | Go to definition, rename, the arguments of the function being typed, the outline of a file, and the type of the table between the stages of a pipeline | Should |
| REPL: `:type x`, `:schema t` | The type of a value and the columns of a table without printing them | Should |
| `biggo test` | Tests chosen by name, several files at once, and the time each took | Should |
| `biggo explain --timings` | Runs the query and shows the rows and the time of each operator | Should |
| `--threads n` and a memory limit | As options, beside the `RAYON_NUM_THREADS` variable | Should |
| `biggo fmt -` | Formats standard input, which editors other than VS Code use | Should |
| Neovim and Helix | The settings in [docs/07-tools.md](docs/07-tools.md) tried and made to work | Should |
| documentation: a cookbook | "How do I ..." recipes, and tables that put SQL and pandas beside the biggo for the same thing | Should |
| a changelog, a contributing guide, issue templates | What changed in each release, and how to take part | Should |
| a web playground | The language compiled to WebAssembly, to try it without installing | Later |

- [x] Language server: autocomplete of the columns available at that point in a pipeline, the
      fields of a record, the names in scope, the built-in functions and the keywords, also on
      a line that is still being typed

### 12. Engine

The methods chosen for this work, and the reasons for them, are set out in
[docs/design/engine.md](docs/design/engine.md): push-based pipelines over morsels, partitioned
aggregation and joins with typed keys, spilling to Arrow IPC files under one memory budget, and
one connector interface for every data source.

| Work | Today (5 million rows, M1 Pro) | Target |
| --- | --- | --- |
| Parquet: skip row groups using their min/max statistics, and decode only the rows that pass the filter | 2.6 times slower than DuckDB and Polars | Within 1.5 times |
| `group` with many groups: a fast path when the key is a single `int` or `string`, instead of encoding the key as bytes every time | 2.7 times slower than Polars | Within 1.5 times |
| JSON: biggo's own reader in place of Arrow's general one | 2.2 times slower than DuckDB | Within 1.5 times |
| VM: make function and lambda calls cheaper | 1.0 to 1.5 times slower than CPython | Not slower than CPython on any of the three benchmark programs |

- [ ] `sort` followed by `take`: keep only the best rows instead of sorting the whole table
- [ ] `join`: build the hash table in parallel, and pick the smaller side as the build side
      automatically (today it is built on one thread, and the author has to put the small table
      on the right)
- [ ] An optimizer that uses statistics: row counts and min/max from file metadata, to choose the
      order of joins
- [ ] Data larger than RAM: an external `sort`, a `group` and a `join` that spill to disk past a
      limit, and a setting for the memory limit
- [ ] A table that is used more than once reads its file once, automatically (today you have to
      call `collect` yourself)
- [ ] Files in another encoding than UTF-8 converted in pieces on all cores, instead of whole on
      one
- [ ] SQLite: read in parallel by splitting the rowid range
- [ ] A CSV file with a stray quote in an unquoted field is cut into pieces as fast as any other
      (today every cut after the quote scans on to the next quote, and the file is decoded on
      one core)
- [ ] Tables of more than 4 billion rows, and more than 2 GiB of text in one column of a sort
- [ ] Wider benchmarks: the TPC-H queries, data larger than RAM, measurements on Linux, and a run
      in CI to catch speed regressions

Everything here has to keep principle 2. Each fast path gets a test that confirms it gives the
same result as the plain path for every value, as the parallel sort and the VM's scalar functions
already have.

### 13. Stability

- [ ] Freeze the language: a program that passes `biggo check` in 1.0 must pass, and give the
      same result, in every 1.x release
- [ ] A rule for adding built-in functions after 1.0. Today a new built-in takes its name away
      from every program that used it for a function of its own
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
- **Random numbers without a seed.** They would break principle 2. `sample` takes a seed.
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
