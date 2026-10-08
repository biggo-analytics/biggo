# Getting started with biggo

biggo is a language for data analysis. A program is a single file (`.bgo`) that reads data in as tables,
transforms the tables with a pipeline (`|>`), and then prints or writes the result. The language checks
all types before the program runs, including the name and type of every column, so if you mistype a
column name, you find out before a single row of data has been read.

This page takes you from building biggo to running your first program.

## What you need

- A current version of Rust, which you can install from <https://rustup.rs> (the project uses edition 2024
  and is developed and tested with Rust 1.99)
- The system C compiler (used to build the SQLite that is bundled with biggo). On macOS, this is the
  Xcode Command Line Tools

You do not need to install a database or any other library. Everything is linked into a single executable.

## Build

```sh
git clone https://github.com/biggo-analytics/biggo.git && cd biggo
cargo build --release
```

This produces the file `target/release/biggo`, about 22 MB in size. You can copy it straight into your
`PATH`:

```sh
cp target/release/biggo ~/.local/bin/
biggo version
```

Always use `--release` when you measure speed or use biggo for real work. A debug build is tens of
times slower.

## Your first program

Create the file `hello.bgo`:

```biggo
let name = "biggo"
print("hello,", name)
print(1 + 2 * 3, 7 / 2, [1, 2, 3])
```

Then run `biggo run hello.bgo`. You get:

```text output
hello, biggo
7 3.5 [1, 2, 3]
```

`print` takes any number of values and separates them with a space when it prints. `/` always produces
a float (`7 / 2` is `3.5`).

## Your first data analysis program

The examples in this documentation use the file [`data/sales.csv`](data/sales.csv), which has 10 rows:

```text
date,region,product,qty,price
2026-01-05,north,widget,10,2.5
2026-01-17,north,gadget,3,10.0
2026-01-20,south,widget,0,2.5
2026-02-02,south,gadget,5,
...
```

This program computes the total sales of each region:

```biggo
// Declares which columns each row of the file has, and their types
type Sale = { date: date, region: string, product: string, qty: int, price: float? }

let sales = read_csv<Sale>("data/sales.csv")

sales
  |> where(qty > 0)
  |> derive(revenue = qty * (price ?? 0.0))
  |> group(region)
  |> agg(total = sum(revenue), orders = count())
  |> sort(desc(total))
  |> print()
```

```text output
+--------+-------+--------+
| region | total | orders |
+--------+-------+--------+
| east   | 219.8 | 3      |
| north  | 172.5 | 4      |
| south  | 30.0  | 2      |
+--------+-------+--------+
```

You can read it from top to bottom:

1. `type Sale = {...}` declares the row type. `price: float?` means this column can be empty (the file
   has a row with no price)
2. `read_csv<Sale>(...)` gives a table whose columns follow `Sale`. The file path is relative to the
   location of the program file, not to the directory where you run the command
3. `a |> f(b)` is `f(a, b)`. The table on the left is passed as the first argument of the function on
   the right
4. `where` selects rows, `derive` adds columns, `group` + `agg` summarize by group, `sort` sorts
5. `price ?? 0.0` means "use `price` if it has a value, and use `0.0` if it is null"

A table in biggo is *an instruction that has not run yet* (lazy). The whole pipeline is combined into
a single plan, optimized, and run only when it reaches `print`: it reads only the columns that are used,
filters rows while reading the file, and uses every core of the machine.

## Errors are caught before the program runs

Try mistyping a column name:

```biggo error
type Sale = { date: date, region: string, product: string, qty: int, price: float? }
let sales = read_csv<Sale>("data/sales.csv")
print(sales |> where(quantity > 0))
```

```text output
error: undefined name `quantity`; the table has columns date, region, product, qty, price
 --> example.bgo:3:22
  |
3 | print(sales |> where(quantity > 0))
  |                      ^^^^^^^^
```

This error comes from the type checker. The data file has not been opened at all. If you only want to
check a program without running it, use `biggo check hello.bgo`.

## Try things line by line with the REPL

`biggo repl` opens an interactive mode. Type an expression and you see its value immediately. Variables
and functions that you declare stay available until you exit:

```text
$ biggo repl
biggo 0.1.0 (Ctrl-D to exit)
>> let x = 2
>> x * 21
42
>> fn double(n: int) -> int { n * 2 }
>> [1, 2, 3] |> map(double)
[2, 4, 6]
>> [x, x + 1] |>
..   print()
[2, 3]
```

If a line is not finished (for example, a parenthesis is still open, or the line ends with `|>`), the
REPL waits for the next line automatically.

## See what the engine will do

`biggo explain hello.bgo` prints the plan of every query instead of running it, both the plan as written
and the optimized plan:

```biggo explain
type Sale = { date: date, region: string, product: string, qty: int, price: float? }
read_csv<Sale>("data/sales.csv")
  |> where(qty > 0)
  |> group(region)
  |> agg(units = sum(qty))
  |> print()
```

```text output
plan:
  Aggregate: by region; units = sum(qty)
    Filter: qty > 0
      Scan csv "data/sales.csv": date, region, product, qty, price
optimized plan:
  Aggregate: by region; units = sum(qty)
    Scan csv "data/sales.csv": region, qty where qty > 0
```

The optimized plan reads only 2 of the 5 columns and applies the `qty > 0` filter in the file-reading
step itself.

## Where to go next

| To learn about | Read |
| --- | --- |
| Syntax, variables, functions, `if`, `match`, lambdas | [The language](02-language.md) |
| All data types and the type conversion rules | [The type system](03-types.md) |
| `where`, `group`, `join`, `window`, `pivot`, and so on | [Working with tables](04-tables.md) |
| CSV, Parquet, JSON, SQLite | [Data sources](05-data-sources.md) |
| Every built-in function | [Built-in reference](06-builtins.md) |
| `run`, `check`, `fmt`, `test`, `build`, `lsp`, and editors | [Tools](07-tools.md) |
| How the compiler works | [Architecture](08-architecture.md) |
| Benchmark results and how they are measured | [Performance](09-performance.md) |
| The formal grammar | [Grammar](10-grammar.md) |
