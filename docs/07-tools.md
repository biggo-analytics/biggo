# Tools

Everything is in a single executable named `biggo`: the runner, the checker, the REPL, the
formatter, the test runner, the executable builder, and the language server for editors.

```text
usage: biggo <command> [args]

commands:
  run <file> [<arg>...]    run a program; `args()` gives it the arguments
  run -e <code> [<arg>...] run a program given on the command line
  repl                     evaluate code interactively
  check <file>             report the syntax and type errors of a program
  explain <file> [<arg>...]  show the query plans of a program without running them
  test [<path>...]         run the tests in the `*_test.bgo` files under the paths
  fmt [--check] <file>...  format programs in place, or list those that need it
  help <function>          describe a built-in function

`-` in place of a file is standard input. `fmt -` writes the formatted program to standard
output.
  build <file> [-o <out>]  make a standalone executable of a program
  lsp                      serve an editor over the Language Server Protocol
  parse <file>             print the syntax tree of a program
  version                  print the version
```

| Exit code | Meaning |
| --- | --- |
| 0 | success |
| 1 | the program has an error (syntax, type, or at run time), a test fails, or `fmt --check` finds a file that is not formatted yet |
| 2 | the command is used incorrectly |

Program output goes to standard output. Errors go to standard error.

## biggo run

```sh
biggo run report.bgo
```

Runs a program. Before the program runs, biggo checks the syntax and types of the whole file (and
of the files it `import`s). If it finds errors, it reports all of them and runs nothing at all. So
a program with a type error can never run halfway and leave a half-written file behind.

Paths to data files in a program are relative to the folder of the `.bgo` file, not the folder you
run the command from.

Whatever follows the file name is passed to the program, which reads it with `args()`:

```sh
biggo run report.bgo 2026-01 north
```

```biggo
let given = args()
let month = if len(given) > 0 { given[0] } else { "2026-01" }
let region = if len(given) > 1 { given[1] } else { "all regions" }
print("report for", month, "in", region)
```

```text output
report for 2026-01 in all regions
```

- `args()` is a `list<string>`: here it is `["2026-01", "north"]`. Run without arguments, as the
  example above is, it is the empty list. Use `to_int`, `to_float` or `to_date` to turn an
  argument into another type.
- Nothing after the file name is taken as an option of `biggo` itself:
  `biggo run report.bgo --help` passes `--help` to the program.
- Asking for an argument that was not given is an error at run time
  (`index 0 is out of range for a list of 0 items`), so look at `len(args())` first.
- `biggo explain` and an executable made by `biggo build` take arguments the same way. In the REPL
  and under `biggo test`, `args()` is empty.

A short program can be given on the command line with `-e`, and a program can come from
standard input with `-` in place of the file. What follows is passed to the program in both
cases:

```sh
biggo run -e 'print(read_csv<{ region: string, qty: int }>("sales.csv") |> count_by(region))'
biggo run -e 'print(to_int(args()[0]) * 2)' 21
cat report.bgo | biggo run - 2026-01 north
```

Paths in such a program are relative to the folder you run the command from. `biggo explain`
and `biggo check` take `-` the same way, and `biggo explain` takes `-e`.

The number of threads the engine uses is set with `RAYON_NUM_THREADS` (the default is the total
number of cores):

```sh
RAYON_NUM_THREADS=2 biggo run report.bgo
```

`today()` and `now()` read the clock of the machine. To run a program as of another moment, for a
report that should come out the same on a later day, set `BIGGO_NOW`:

```sh
BIGGO_NOW=2026-01-31T18:30:00 biggo run report.bgo
```

## biggo check

```sh
biggo check bad.bgo
```

Checks syntax and types without running the program. It does not touch data files, so it is fast
and works even when the data is not ready yet. It reports every error it finds in a single pass:

```text
error: cannot apply `>` to int and string
 --> bad.bgo:3:22
  |
3 | print(sales |> where(qty > "5"))
  |                      ^^^^^^^^^

error: the table has no column `amount`; its columns are region, qty
 --> bad.bgo:4:23
  |
4 | print(sales |> select(amount))
  |                       ^^^^^^

2 errors in bad.bgo
```

It fits well in CI or in a pre-commit hook, together with `biggo fmt --check`.

## biggo explain

```sh
biggo explain report.bgo
```

Runs the program in a mode where **no query is actually run**: at every point where the program
would print, write, count, or fetch the rows of a table, it prints the plan of that query instead,
both the plan as written (`plan`) and the optimized plan (`optimized plan`).
Code that is not about tables runs as usual. Use it to see whether:

- a filter is pushed down into the step that reads the file (`Scan ... where ...`)
- only the columns that are needed are read (the list after `Scan`)
- `sort` + `take` are combined into a top-n (`Sort: qty desc (first 3)`)

Example output is in [Getting started](01-getting-started.md#see-what-the-engine-will-do).
Inside a program, you can call `explain(table)` to print the plan of a single table.

## biggo repl

```text
$ biggo repl
biggo 0.1.0 (:help for commands, Ctrl-D to exit)
>> type Sale = { region: string, qty: int }
>> let sales = read_csv<Sale>("docs/data/sales.csv")
>> sales |> group(region) |> agg(units = sum(qty))
+--------+-------+
| region | units |
+--------+-------+
| north  | 21    |
| south  | 17    |
| east   | 7     |
+--------+-------+
>> sales |> where(qty > "many")
error: cannot apply `>` to int and string
 --> <repl>:1:16
  |
1 | sales |> where(qty > "many")
  |                ^^^^^^^^^^^^
>> count(sales)
10
```

- Each entry is type-checked and then run immediately. The value of an expression is shown in
  literal form (strings have `"`).
- A `let`, `fn`, `type`, or `import` that you enter stays available until the session ends.
- An entry that is not finished (an unclosed bracket, a trailing operator) shows the `..` prompt
  and waits for the next line. Enter a blank line to submit it even though it is not finished.
- An error does not end the session. What you declared earlier is still there.
- File paths are relative to the folder where you started the REPL.
- It accepts input from a pipe: `echo 'print(1 + 1)' | biggo repl` (no prompt).
- On a terminal the line can be edited with the arrow keys, Home and End, and Up and Down bring
  back earlier entries, those of earlier sessions too (they are kept in `~/.biggo_history`).
  Ctrl-C drops the entry being typed, and Ctrl-D leaves.

A line that starts with `:` is a command to the REPL itself:

| Command | What it does |
| --- | --- |
| `:help` | Lists these commands |
| `:help <name>` | What a built-in function does, as [`biggo help`](#biggo-help) prints it |
| `:type <expr>` | The type of an expression, without running it; for a table, its columns |
| `:quit` | Leaves, as Ctrl-D does |

```text
>> :type sales |> group(region) |> agg(units = sum(qty))
table<{region: string, units: int}>
>> :help starts_with
string:
  starts_with(s, part)
    Returns: bool
    `s` starts with `part`
```

## biggo fmt

```sh
biggo fmt report.bgo other.bgo     # format the files and overwrite them
biggo fmt --check *.bgo            # change no files, only print the names of files not yet formatted (exit 1 if any)
biggo fmt - < draft.bgo            # format standard input and print the result, which is what editors call
```

Formats code into one standard style. There are no options to set. Before and after:

```text
type Sale={region:string,qty:int}
let   big=read_csv<Sale>("sales.csv")|>where(qty>5)   // large orders
   |>group(region)|>agg(units=sum(qty),orders=count())
print( big )
```

```text
type Sale = { region: string, qty: int }
let big = read_csv<Sale>("sales.csv")
  |> where(qty > 5)  // large orders
  |> group(region)
  |> agg(units = sum(qty), orders = count())
print(big)
```

The style rules:

- Indentation is 2 spaces. Lines are at most 100 characters long.
- A pipeline that does not fit on one line, or in which the author put a newline, is laid out one
  step per line.
- Arguments that do not fit on one line are laid out one per line, with a trailing `,`.
- A multi-line lambda that is the last argument starts on the same line as the call:
  `each(xs, fn(x) {`
- Every comment is kept. A single blank line between statements is kept (several are collapsed
  into one).
- Unnecessary parentheses are removed, except those the author put around a compound expression
  for clarity.
- Literals are kept as written (`1_000`, `1.50d`).

The formatter guarantees two things, which are tested against every example program in the
project: the meaning of the program does not change (the syntax tree and the comments stay the
same), and formatting again gives the same result.
A file with a syntax error is reported and left untouched.

## biggo test

```sh
biggo test                  # every *_test.bgo file under the current folder
biggo test tests/ lib/      # under the given folders
biggo test pricing_test.bgo # a single file
```

Tests are written in biggo itself:

- A **test file** is a file whose name ends with `_test.bgo`.
- A **test** is a top-level function whose name starts with `test_` and that has no parameters.
- A test passes when it runs to the end without an error. Use `assert` and `assert_eq` to check
  results.

Suppose you have a file `pricing.bgo`:

```biggo check
fn discount(amount: float, percent: float) -> float {
  amount * (1 - percent / 100)
}
```

The test file `pricing_test.bgo` next to it:

```biggo fragment
import "pricing.bgo"

fn test_no_discount() {
  assert_eq(discount(200, 0), 200.0)
}

fn test_half_price() {
  assert_eq(discount(200, 50), 100.0)
}

fn test_rounding() {
  print("discounted:", discount(19.99, 15))
  assert_eq(round(discount(19.99, 15), 2), 17.0)
}
```

```text
$ biggo test
ok    pricing_test.bgo::test_no_discount
ok    pricing_test.bgo::test_half_price
FAIL  pricing_test.bgo::test_rounding
      error: assertion failed: the values differ
        left:  16.99
        right: 17.0
        --> pricing_test.bgo:13:3
         |
      13 |   assert_eq(round(discount(19.99, 15), 2), 17.0)
         |   ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
      output:
      discounted: 16.9915

2 passed, 1 failed
```

How it works:

1. Each test file is first run once as a whole (to declare its functions and variables). If this
   step fails, the whole file counts as one failed test.
2. Then the `test_...` functions are called one at a time, in the order they are declared. A test
   that fails does not stop the other tests.
3. What a test prints is captured, and shown only when that test fails.
4. A test file with no `test_...` function counts as one passing test if it runs to the end
   (use this to write script-style tests that have `assert` at the top level).
5. Folders whose names start with `.`, and the folders `target` and `node_modules`, are skipped.

The exit code is 1 if any test fails.

### assert and assert_eq

```biggo
assert(1 + 1 == 2)
assert(len([1, 2]) == 2, "must have two")          // the message shown on failure
assert_eq([1.0, 2.0], [1, 2])                      // deep structural comparison, converts like ==
assert_eq({ name: "a", tags: ["x"] }, { name: "a", tags: ["x"] })
assert_eq(put({ "a": 1 }, "b", 2), { "b": 2, "a": 1 })   // maps ignore the order of keys

// tables: the same columns, and the same rows in order
let totals = from_rows([{ k: "a", v: 1 }, { k: "a", v: 2 }, { k: "b", v: 5 }])
  |> group(k)
  |> agg(total = sum(v))
assert_eq(totals, from_rows([{ k: "a", total: 3 }, { k: "b", total: 5 }]))
print("all passed")
```

```text output
all passed
```

When an assertion fails, the error shows both values or, for tables, the first row that differs:

```biggo error
let got = from_rows([{ id: 1, name: "a" }, { id: 2, name: "b" }])
assert_eq(got |> sort(desc(id)), got)
```

```text output
error: assertion failed: row 1 differs
  left:  {id: 2, name: "b"}
  right: {id: 1, name: "a"}
 --> example.bgo:2:1
  |
2 | assert_eq(got |> sort(desc(id)), got)
  | ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
```

`assert_eq` compares `float` values exactly: you should `round` computed values before comparing
them. Comparing tables runs the queries on both sides and loads every row, so it suits test-sized
tables.

## biggo build

```sh
biggo build report.bgo             # produces an executable named report
biggo build report.bgo -o bin/app  # choose the name yourself
./report
./report 2026-01 north             # its arguments reach the program as args()
```

Builds an executable that runs the program on its own, with no need for `biggo` or the `.bgo` file
on the target machine.

- The executable is a copy of `biggo` with the source of the program **and of every file it
  `import`s** embedded inside. So its size is the same as `biggo` itself (about 22 MB), no matter
  how small the program is.
- The program is type-checked first. If it has errors, nothing is built.
- At run time, the source is compiled to bytecode again every time, which takes milliseconds.
- Paths to data files in the executable are relative to **the folder you run it from** (there is
  no `.bgo` file to be relative to anymore).
- The whole command line of the executable goes to the program, as `args()`.
- The total size of the embedded source is limited to about 256 KiB.
- On macOS, the executable is ad-hoc signed with `codesign` (requires the Xcode Command Line Tools).
- The executable works on the same operating system and CPU as the `biggo` that built it (no
  cross-compilation).

## biggo infer

```text
$ biggo infer data/sales.csv
type Sales = {
  date: date,
  region: string,
  product: string,
  qty: int,
  price: float?,
}

let sales = read_csv<Sales>("data/sales.csv")
```

Reads a data file and prints the row type that fits it, with the call that reads the file, to
copy into a program. It saves typing the type of every column, and it is the quickest way to see
what is in a file.

| File | What is looked at |
| --- | --- |
| `.csv`, `.tsv`, `.txt` | The header and the first 10,000 rows. The delimiter is found among `,` `;` tab and `\|` |
| `.json`, `.jsonl`, `.ndjson` | The first 10,000 objects, of lines or of one array |
| `.parquet` | The schema in the file, which is exact |
| `.db`, `.sqlite`, `.sqlite3` | The declared columns of the table named after the file, as in `biggo infer shop.db orders` |

How a column of a text file gets its type:

- `int`, `float`, `bool`, `date` or `datetime` if every value in the sample can be read as it;
  otherwise `string`. A column of whole numbers and fractions is `float`.
- A number that starts with a zero, such as a postal code `01000` or a telephone number, is a
  `string`: as a number it would lose the zero.
- A column with an empty field in the sample is nullable (`int?`).
- A name that is not a plain word is written in backticks: `` `Order ID`: int ``.

What cannot be worked out is said in comments above the type: a column without a name, a second
column of the same name, a JSON field that holds a list, and a column of dates written another
way, with the call that reads them:

```text
$ biggo infer orders.csv
// `paid on` holds dates written like 05/01/2569: parse_date(`paid on`, "%d/%m/%Y", era = "buddhist") reads them
type Orders = {
  `Order ID`: int,
  zip: string,
  `paid on`: string,
}

let orders = read_csv<Orders>("orders.csv", delimiter = ";")
```

| Option | Meaning |
| --- | --- |
| `--delimiter <d>` | The delimiter of a CSV file, when it should not be found out; `tab` or `\t` for a tab |
| `--encoding <e>` | The encoding of a CSV file that is not UTF-8, such as `tis-620`. A UTF-16 file with a byte order mark is recognized without it |
| `--skip <n>` | The lines before the header |
| `--no-header` | The first line is a row. The columns are named `column_1`, `column_2`, and so on; rename them in the type |

The options that the file needs appear in the `read_csv` call that is printed.

The types are a starting point, to read and correct: a column that is whole numbers in its
first 10,000 rows and has a fraction later fails when the program reads that row, with a message
that names the line and the column. Money read as `float` may be better as `decimal`, and a
column that is null in a later row needs a `?`.

## biggo help

```text
$ biggo help substring
string:
  substring(s, start), substring(s, start, length)
    Returns: string
    The part of `s` that starts at position `start` and runs to the end, or for `length` characters

The full reference, with examples: https://github.com/biggo-analytics/biggo/blob/main/docs/06-builtins.md
```

Prints what the [built-in reference](06-builtins.md) says about a function: how it is called,
what it takes and returns, and what it does. A name with several meanings, such as `sum` (an
aggregate, and a function of lists), has an entry for each. A name that is not a function gets
the names close to it:

```text
$ biggo help substr
biggo help: there is no built-in function `substr`; close to it: substring
```

The entries are the tables of the reference, built into the executable, so they are the ones
for the version of biggo that you run.

## biggo parse

```sh
biggo parse report.bgo
```

Prints the syntax tree of a program as an S-expression. Use it to check how the parser reads code
(operator precedence, where a statement ends). For example,
`let total = [1, 2] |> map(fn(n) { n * 2 })` gives:

```text
(let total (|> (list 1 2) (call map (lambda (n) (block (* n 2))))))
```

## Language server and editors

`biggo lsp` is a language server that speaks the Language Server Protocol over standard
input/output. Any editor that supports LSP can use it. Features:

| Feature | Details |
| --- | --- |
| diagnostics | syntax and type errors as you type, from the same checker as `biggo check` |
| hover | the type of the expression under the cursor (for a table, the list of columns at that point in the pipeline) |
| formatting | formats the whole file with the same formatter as `biggo fmt` |

A file that you `import` is read first from the content that is open in the editor (even if it is
not saved yet). If it is not open, it is read from disk.
Errors in an imported file are shown on the `import` line of the file you are editing.

Not available yet: autocomplete, go to definition, rename.

### VS Code

The extension is in the [`editors/vscode`](../editors/vscode) folder. It provides syntax
highlighting, bracket matching, every language server feature above, and the commands
**biggo: Run File**, **biggo: Explain Query Plans of File**, **biggo: Run Tests**.

```sh
cd editors/vscode
npm install
npx vsce package --allow-missing-repository --skip-license
code --install-extension biggo-0.1.0.vsix
```

The extension runs `biggo` from `PATH`, or from the path set in the `biggo.path` setting.

### Other editors

Neovim 0.11 or later:

```lua
vim.filetype.add({ extension = { bgo = "biggo" } })
vim.lsp.config("biggo", {
  cmd = { "biggo", "lsp" },
  filetypes = { "biggo" },
  root_markers = { ".git" },
})
vim.lsp.enable("biggo")
```

Helix (`~/.config/helix/languages.toml`):

```toml
[language-server.biggo]
command = "biggo"
args = ["lsp"]

[[language]]
name = "biggo"
scope = "source.biggo"
file-types = ["bgo"]
comment-token = "//"
indent = { tab-width = 2, unit = "  " }
language-servers = ["biggo"]
```

> Test status: the language server is tested automatically with real LSP messages (`cargo test -p biggo-lsp`),
> and the VS Code grammar is tested with the same engine that VS Code uses (`vscode-textmate`).
> The extension packages into a `.vsix` successfully, but it **has not been run in a real VS Code yet**.
> The Neovim/Helix settings above are written from each editor's documentation and have not been run yet.
