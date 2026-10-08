# Data sources: CSV, Parquet, JSON, Excel, SQLite, PostgreSQL, MySQL

biggo reads and writes files in 5 formats, and tables of a PostgreSQL or a MySQL server. All of
them follow the same principles:

- **Read**: `read_xxx<row type>(path)` returns a table. The program says which columns it wants and
  what their types are, and the engine checks them against the file at run time.
- **Write**: `write_xxx(table, path)` runs the query and writes the whole result. It replaces the
  file if it already exists, and creates the folder if it does not exist yet. The result is
  written under a temporary name beside the file and takes the file's name only when all of it
  is there: a query that fails leaves the old file as it was, and a query can replace the file
  that it reads.
- **path** is relative to the folder of the program file, not to where you run the command. So you
  can move a program, together with its data, and run it from anywhere.
- Reading is lazy: `read_xxx` does not open the file yet. The file is read when the query runs, and
  only the columns the query uses are read.

| Format | Read | Write | Reads in parallel on multiple cores | Skips unused columns |
| --- | --- | --- | --- | --- |
| CSV | `read_csv` | `write_csv` | ✓ | ✓ (values are not converted) |
| Parquet | `read_parquet` | `write_parquet` | ✓ | ✓ (not read from disk at all) |
| JSON (one object per line, or one array) | `read_json` | `write_json` | ✓ | ✓ |
| Excel | `read_excel` | `write_excel` | | ✓ (cells are not converted) |
| SQLite | `read_sql` | `write_sql` | | (you write it in the query yourself) |
| PostgreSQL | `read_sql` | `write_sql` | | ✓ (the server sends only those columns) |
| MySQL, MariaDB | `read_sql` | `write_sql` | | ✓ (the server sends only those columns) |

## The row type

The type argument in `<...>` says which columns the program wants. You do not have to type it
out: [`biggo infer`](07-tools.md#biggo-infer) reads a file and prints its row type, with the
call that reads it.

```text
$ biggo infer sales.csv
type Sales = {
  date: date,
  region: string,
  product: string,
  qty: int,
  price: float?,
}

let sales = read_csv<Sales>("sales.csv")
```

A row type looks like this:

```biggo
type Sale = { date: date, region: string, product: string, qty: int, price: float? }

// declare only what you use; the order need not match the file; columns are matched by name
let slim = read_csv<{ qty: int, product: string }>("data/sales.csv")
print(slim |> take(2))
print(read_csv<Sale>("data/sales.csv") |> count())
```

```text output
+-----+---------+
| qty | product |
+-----+---------+
| 10  | widget  |
| 3   | gadget  |
+-----+---------+
10
```

- The file can have more columns than you declare. The extra ones are skipped.
- A column that you declare but the file does not have is an error at run time, with the list of
  columns the file has.
- A column declared as non-null (`qty: int`) when the file has missing values is an error that tells
  you to declare it as `int?`.
- A value that cannot be converted to the declared type is an error that gives the line and the
  column.

```biggo error
print(read_csv<{ product: string, qty: float, price: float }>("data/sales.csv"))
```

```text output
error: column `price` of data/sales.csv has missing values, but is declared `float`; declare it `float?`
 --> example.bgo:1:1
  |
1 | print(read_csv<{ product: string, qty: float, price: float }>("data/sales.csv"))
  | ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
```

```biggo error
print(read_csv<{ product: int }>("data/sales.csv"))
```

```text output
error: data/sales.csv, line 2: cannot read 'widget' as an int for column `product`
 --> example.bgo:1:1
  |
1 | print(read_csv<{ product: int }>("data/sales.csv"))
  | ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
```

These errors happen at run time, because the compiler does not open data files: the same program
therefore passes the check even when the file does not exist yet, and works with files that change
every day.

## How values look in files

A value of each type looks the same in every file format. So a file that biggo writes can always be
read back, and the same table written as CSV, JSON, Parquet or SQLite reads back with every value
unchanged.

| type | CSV | JSON | SQLite | Parquet |
| --- | --- | --- | --- | --- |
| `int` | `42` | `42` | `INTEGER` | `INT64` |
| `float` | `2.5` | `2.5` | `REAL` | `DOUBLE` |
| `bool` | `true` / `false` | `true` / `false` | `INTEGER` 1 / 0 | `BOOLEAN` |
| `string` | text | `"text"` | `TEXT` | `STRING` |
| `date` | `2026-01-31` | `"2026-01-31"` | `TEXT` | `DATE` |
| `datetime` | `2026-01-31T18:30:00` | `"2026-01-31T18:30:00"` | `TEXT` | `TIMESTAMP` (microseconds) |
| `decimal` | `19.99` | `"19.99"` | `TEXT` | `DECIMAL(38, 6)` |
| `duration` | number of seconds `5400.0` | number of seconds `5400.0` | `REAL` (seconds) | Arrow duration |
| null | empty field | field absent or `null` | `NULL` | null |

When reading, the reader accepts more forms than the ones it writes:

- `datetime`: a space can replace `T`, fractional seconds can have up to 6 digits, and a date alone
  means time 00:00. If a time zone follows (`Z`, `+07:00`), the value is converted to UTC and the
  zone is dropped. **Seconds are required** (`18:30:00`, not `18:30`).
- `decimal`: in JSON it can be a number or a string. More than 6 decimal places are rounded.
- `bool` in CSV: `true` / `false` in any letter case (`1`/`0` are not accepted).
- `duration`: any kind of number is read as a number of seconds.

## CSV

```biggo
type Sale = { date: date, region: string, product: string, qty: int, price: float? }
let sales = read_csv<Sale>("data/sales.csv")

let summary = sales
  |> group(region)
  |> agg(units = sum(qty), best = max(price), latest = max(date))
  |> sort(region)
write_csv(summary, "out/summary.csv")

print(read_csv<{ region: string, units: int, best: float?, latest: date }>("out/summary.csv"))
```

```text output
+--------+-------+-------+------------+
| region | units | best  | latest     |
+--------+-------+-------+------------+
| east   | 7     | 99.9  | 2026-03-09 |
| north  | 21    | 100.0 | 2026-03-15 |
| south  | 17    | 2.5   | 2026-03-01 |
+--------+-------+-------+------------+
```

The resulting file:

```text
region,units,best,latest
east,7,99.9,2026-03-09
north,21,100.0,2026-03-15
south,17,2.5,2026-03-01
```

Requirements for a CSV file that can be read:

- The first line is the column names (the header), unless you say otherwise with `header` or
  `skip` (see [below](#titles-no-header-and-other-words-for-null)).
- The separator is `,`, unless you name another with `delimiter` (see below).
- A value that contains `,`, a line break, or `"` must be inside `"..."`, and a `"` inside it is
  doubled as `""`.
- The encoding is UTF-8, unless you name another with `encoding` (see below). A BOM at the start
  of the file is skipped.
- An empty field is null, except in a non-nullable `string` column, where it is the empty string
  `""`. `nulls` names other texts that mean null.
- Every line must have the same number of fields as the header.

**Speed:** the file is mapped into memory and cut into 2 MiB chunks at line boundaries (counting `"`
characters so that a value containing a line break is not cut in the middle). Each chunk is parsed
on its own core. Columns the query does not use are not converted to values, and the `where` filter
is applied already inside each chunk.

### Other delimiters and encodings

`read_csv` and `write_csv` take two named arguments after the path:

| Argument | Default | Meaning |
| --- | --- | --- |
| `delimiter` | `","` | The character between the fields of a row: one ASCII character other than `"` or a line break, such as `"\t"`, `";"` or `"|"` |
| `encoding` | `"utf-8"` | The encoding of the file's text, by its usual name: `"tis-620"`, `"windows-874"`, `"windows-1252"`, `"iso-8859-1"`, `"shift_jis"`, `"gbk"`, `"utf-16le"` and so on |

```biggo
type Product = { code: string, name: string, price: float }
let products = read_csv<Product>("data/products.tsv", delimiter = "\t")
print(products)

// A file written by an older Thai system, in TIS-620.
type Customer = { id: int, name: string, city: string }
let customers = read_csv<Customer>("data/customers_tis620.csv", encoding = "tis-620")
print(customers |> where(id > 1))

// Written for a program that wants semicolons and TIS-620, then read back.
write_csv(customers, "out/customers.txt", delimiter = ";", encoding = "tis-620")
print(count(read_csv<Customer>("out/customers.txt", delimiter = ";", encoding = "tis-620")))
```

```text output
+------+---------------+-------+
| code | name          | price |
+------+---------------+-------+
| W-01 | widget        | 2.5   |
| G-07 | gadget, large | 10.0  |
| Z-99 | gizmo         | 99.9  |
+------+---------------+-------+
+----+-------+---------+
| id | name  | city    |
+----+-------+---------+
| 2  | สมหญิง | เชียงใหม่ |
| 3  | Anna  | ภูเก็ต    |
+----+-------+---------+
3
```

- Encodings go by the names of the [Encoding Standard](https://encoding.spec.whatwg.org/#names-and-labels),
  the ones web browsers know, in upper or lower case. `"tis-620"` is read as `"windows-874"`, the
  same encoding with a few more characters, and that is the name `biggo explain` shows.
- A file in another encoding is converted to UTF-8 in memory before it is read. The copy is as
  large as the file for ASCII text and up to three times as large for Thai text, and the
  conversion runs on one core. A UTF-8 file is read in place, with no copy.
- A byte order mark at the start of a file overrules the encoding you name, so a UTF-16 file with
  one is read correctly whether you say `"utf-16le"` or `"utf-16be"`.
- Nothing is replaced silently. Bytes that are not valid in the encoding stop the program
  (`x.csv is not windows-874 text`), and so does writing a character that the encoding does not
  have (`'ร' cannot be written as windows-1252`).
- A file can be read as UTF-16, but not written as it.
- An option that is written out as a string is checked before the program runs. One that the
  program computes is checked when the file is read or written.
- Whatever the delimiter, a line break ends a row and `"` quotes a value.

### Titles, no header, and other words for null

Files that come out of other systems are often not a plain table. Three more named arguments of
`read_csv` say how such a file is laid out:

| Argument | Default | Meaning |
| --- | --- | --- |
| `skip` | `0` | The number of lines to pass over before the header: a title, the date of printing, a blank line |
| `header` | `true` | Whether the first line (after those skipped) names the columns. With `header = false` it is a row, and the columns of the file are the columns of the row type, in the order they are declared |
| `nulls` | `[]` | Texts that mean null, besides an empty field, such as `["NA", "-", "n/a"]` |

The file [`data/report.csv`](data/report.csv) starts with three lines that are not part of the
table, and marks missing values in two ways:

```text
Branch report
printed 2026-02-01

branch,units,price,note
north,10,2.5,ok
south,NA,-,NA
east,5,NA,
```

```biggo
type Line = { branch: string, units: int?, price: float?, note: string }
let report = read_csv<Line>("data/report.csv", skip = 3, nulls = ["NA", "-"])
print(report)
print(report |> agg(units = sum(units), priced = count(price)))

// A file with no header: the first column is `id`, the second `branch`, the third `units`.
type Plain = { id: int, branch: string, units: int }
print(read_csv<Plain>("data/plain.csv", header = false) |> where(units > 5))
```

```text output
+--------+-------+-------+------+
| branch | units | price | note |
+--------+-------+-------+------+
| north  | 10    | 2.5   | ok   |
| south  | null  | null  | NA   |
| east   | 5     | null  |      |
+--------+-------+-------+------+
+-------+--------+
| units | priced |
+-------+--------+
| 15    | 1      |
+-------+--------+
+----+--------+-------+
| id | branch | units |
+----+--------+-------+
| 1  | north  | 10    |
| 2  | south  | 7     |
+----+--------+-------+
```

- `nulls` applies to every column that can hold null. In a column that cannot, the text is
  an error for a number or a date (`cannot read 'NA' as an int`), and stays as it is written in
  a `string` column: above, `note` is `string`, so it keeps `NA`. Declare the column `string?`
  to get null.
- The texts are compared exactly: `"NA"` does not match `na` or ` NA`.
- Without a header, a file may have more columns than the row type; those after the declared
  ones are not read. It may not have fewer.
- The lines that `skip` passes over are not looked at, so they may hold anything.

## Many files at once

A path with `*`, `?` or `[...]` in it is a pattern, and `read_csv`, `read_json` and
`read_parquet` read every file that matches it as one table. The files are read in the order of
their names, so the result is the same each time.

| Pattern | Matches |
| --- | --- |
| `*` | Any run of characters in a name: `logs/2026-*.csv` |
| `?` | Any one character: `logs/2026-0?.csv` |
| `[abc]`, `[0-9]` | One of the characters, or one in the range |
| `**` | Any number of folders: `logs/**/*.csv` |

The named argument `file_name` picks a `string` column of the row type that is not read from
the files: it is filled with the path of the file each row came from.

```biggo
type Hits = { day: date, hits: int, source: string }
let logs = read_csv<Hits>("data/logs/*.csv", file_name = "source")
print(logs)
print(logs |> group(source) |> agg(hits = sum(hits)) |> sort(source))
```

```text output
+------------+------+-----------------------+
| day        | hits | source                |
+------------+------+-----------------------+
| 2026-01-01 | 10   | data/logs/2026-01.csv |
| 2026-01-02 | 12   | data/logs/2026-01.csv |
| 2026-02-01 | 7    | data/logs/2026-02.csv |
+------------+------+-----------------------+
+-----------------------+------+
| source                | hits |
+-----------------------+------+
| data/logs/2026-01.csv | 22   |
| data/logs/2026-02.csv | 7    |
+-----------------------+------+
```

- Every file must have the declared columns. Each file is read with its own header, so the
  columns may be in another order from one file to the next.
- A pattern that matches no file is an error (`no file matches data/logs/*.csv`), since a table
  of no files is more likely a wrong path than what was meant.
- If a file exists whose name is exactly the path, that file is read, whatever characters its
  name has.
- The path in the `file_name` column is written the way the pattern is: relative to the program.
  `regex_extract(source, r"(\d{4}-\d{2})")` or `split_part` takes a part of it, such as a month.
- `file_name` also works with a path that is one file.

## Parquet

Parquet is a columnar format that is compressed and carries its own types. It is much faster than
CSV (in the [benchmark](09-performance.md), reading is about 3–4 times faster), because there is no
text to convert into values, and unused columns are skipped without being read from disk.

```biggo
type Sale = { date: date, region: string, product: string, qty: int, price: float? }
write_parquet(read_csv<Sale>("data/sales.csv"), "out/sales.parquet")

read_parquet<{ region: string, qty: int }>("out/sales.parquet")
  |> where(qty > 5)
  |> print()
```

```text output
+--------+-----+
| region | qty |
+--------+-----+
| north  | 10  |
| north  | 7   |
| south  | 12  |
+--------+-----+
```

- Columns are matched by name. The type in the file can differ from the declared type if it can be
  converted (for example, the file has `INT32` and you declare `int`).
- Files are written with Snappy compression and split into row groups of 131,072 rows each, which
  are the units that can be read in parallel.
- Files written by other tools (pandas, Polars, DuckDB, Spark) can be read, and vice versa.

A job that reads the same CSV file many times should convert it to Parquet once and then read from
the Parquet file.

## JSON

`read_json` reads files of JSON objects in either of two layouts: *JSON Lines* (NDJSON), with one
object per line, and one array of objects, `[{...}, {...}]`. It tells them apart by the first
character of the file. The example file [`data/events.json`](data/events.json):

```text
{"id": 1, "kind": "click", "at": "2026-01-05T09:00:00", "cost": "0.25", "ok": true}
{"id": 2, "kind": "view", "at": "2026-01-05T09:01:30", "cost": null, "ok": true}
{"id": 3, "kind": "click", "at": "2026-01-06T17:45:00", "cost": 1.5, "ok": false, "extra": "ignored"}

{"id": 4, "kind": "click", "at": "2026-01-07T08:00:00", "ok": true}
```

```biggo
type Event = { id: int, kind: string, at: datetime, cost: decimal?, ok: bool }
let events = read_json<Event>("data/events.json")
print(events)

write_json(events |> where(ok) |> select(id, day = to_date(at), cost), "out/ok.json")
print(read_json<{ id: int, day: date, cost: decimal? }>("out/ok.json"))
```

```text output
+----+-------+---------------------+------+-------+
| id | kind  | at                  | cost | ok    |
+----+-------+---------------------+------+-------+
| 1  | click | 2026-01-05T09:00:00 | 0.25 | true  |
| 2  | view  | 2026-01-05T09:01:30 | null | true  |
| 3  | click | 2026-01-06T17:45:00 | 1.5  | false |
| 4  | click | 2026-01-07T08:00:00 | null | true  |
+----+-------+---------------------+------+-------+
+----+------------+------+
| id | day        | cost |
+----+------------+------+
| 1  | 2026-01-05 | 0.25 |
| 2  | 2026-01-05 | null |
| 4  | 2026-01-07 | null |
+----+------------+------+
```

- A field that the object does not have, or whose value is `null`, is read as null.
- Fields that are not declared are skipped. Blank lines are skipped.
- Values must be scalar values: a field that is a nested object or array cannot be read as a column.
- `write_json` writes one object per line. Fields that are null are omitted.

A file that is one array, [`data/people.json`](data/people.json):

```text
[
  {"id": 1, "name": "Ann", "score": 9.5},
  {"id": 2, "name": "Bo", "score": null},
  {"id": 3, "name": "Cy", "score": 7, "joined": "2026-01-05"}
]
```

```biggo
type Person = { id: int, name: string, score: float?, joined: date? }
print(read_json<Person>("data/people.json") |> where(score > 5.0))
```

```text output
+----+------+-------+------------+
| id | name | score | joined     |
+----+------+-------+------------+
| 1  | Ann  | 9.5   | null       |
| 3  | Cy   | 7.0   | 2026-01-05 |
+----+------+-------+------------+
```

An array is cut into pieces between its objects and read on every core, like a file of lines. To
find the places to cut, the file is first passed over once on one core, which a file of lines
does not need.

## Excel

`read_excel` reads one sheet of a workbook: `.xlsx`, `.xlsm`, `.xlsb`, the older `.xls`, and
`.ods` from LibreOffice. The first row is the names of the columns, as in a CSV file.

The example workbook [`data/branches.xlsx`](data/branches.xlsx) has two sheets. `Sales` has a
title above its table:

```text
     A                 B       C       D            E
1    Sales by branch
2
3    branch            units   price   sold on      paid
4    north             10      2.5     2026-01-05   TRUE
5    south                     7.25    2026-01-06   FALSE
6    east              5       n/a     2026-02-28   TRUE
```

```biggo
type Sale = { branch: string, units: int?, price: float?, `sold on`: date, paid: bool }
let sales = read_excel<Sale>("data/branches.xlsx", skip = 2, nulls = ["n/a"])
print(sales)

// Another sheet, joined to the first.
type Target = { branch: string, target: int }
let targets = read_excel<Target>("data/branches.xlsx", sheet = "Targets")
let report = sales
  |> join(targets, on = branch)
  |> select(branch, units, target, reached = (units ?? 0) >= target)
print(report)

write_excel(report, "out/report.xlsx", sheet = "Report")
print(count(read_excel<{ branch: string, reached: bool }>("out/report.xlsx")))
```

```text output
+--------+-------+-------+------------+-------+
| branch | units | price | sold on    | paid  |
+--------+-------+-------+------------+-------+
| north  | 10    | 2.5   | 2026-01-05 | true  |
| south  | null  | 7.25  | 2026-01-06 | false |
| east   | 5     | null  | 2026-02-28 | true  |
+--------+-------+-------+------------+-------+
+--------+-------+--------+---------+
| branch | units | target | reached |
+--------+-------+--------+---------+
| north  | 10    | 8      | true    |
| south  | null  | 6      | false   |
| east   | 5     | 9      | false   |
+--------+-------+--------+---------+
3
```

The named arguments of `read_excel`:

| Argument | Default | Meaning |
| --- | --- | --- |
| `sheet` | the first sheet | The name of the sheet |
| `range` | all of the sheet | The block of cells that holds the table, by its first and last cell: `"B3:F200"` |
| `skip` | `0` | The number of rows to pass over before the header |
| `header` | `true` | Whether the first row names the columns; with `header = false` they are the columns of the row type, in order |
| `nulls` | `[]` | Texts that mean null, besides an empty cell |
| `file_name` | | The column that takes the path of the file, as for [many files at once](#many-files-at-once) |

How cells become values:

- A cell holds a number, text, a truth value or a date, whatever the column is declared as. It
  is read the way the same value written in a CSV file would be: the number 12 fits `int`,
  `float`, `decimal` and `string`; the number 12.5 does not fit `int`
  (`cannot read '12.5' as an int`).
- A number typed as text, such as `'007` or a `5` with a space after it, is read as a number in
  a column of numbers, and stays text in a `string` column, where the zeros are kept.
- A date cell is read as a `date`, and as a `datetime` when it has a time of day. A date written
  as text, `2026-03-01`, is read too.
- A `duration` is a number of seconds, as in the other formats, or a cell that Excel formats as
  a length of time (`[h]:mm:ss`).
- An empty cell is null, and so is a cell that shows an error such as `#N/A` or `#DIV/0!`.
- A formula is read as its result, the one that Excel saved in the file.
- The spaces around the name of a column in the header, which cannot be seen in a sheet, are not
  part of the name.
- Rows that are wholly empty are passed over, wherever they are.

`write_excel(t, path)` writes a workbook with one sheet, named `Sheet1` unless `sheet` names it:
the header in bold, numbers as numbers, dates and times as dates that Excel can sort and filter,
and nulls as empty cells.

- A sheet holds 1,048,575 rows under its header. A larger table is an error; write it as CSV or
  Parquet.
- Excel keeps every number as a float with 15 digits. A `decimal` is written as such a number,
  and an `int` beyond 9,007,199,254,740,992 as text, so that no digit of it changes.
- A sheet is read whole into memory, on one core. For a table of millions of rows, a CSV or a
  Parquet file is read many times faster.

## SQLite

`read_sql<T>(path, query)` runs a query on a SQLite database file and returns the result as a table.
`write_sql(table, path, name)` writes a table to the database under the name `name`.

```biggo
type Sale = { date: date, region: string, product: string, qty: int, price: float? }
let sales = read_csv<Sale>("data/sales.csv")

// write two tables to one database (the file is created if it does not exist yet)
write_sql(sales, "out/shop.db", "sales")
write_sql(sales |> group(product) |> agg(units = sum(qty)), "out/shop.db", "product_units")

// the query is full SQLite SQL; the result's column names must match the declared ones
type Row = { region: string, day: date, revenue: float }
read_sql<Row>(
  "out/shop.db",
  "select region, date as day, qty * price as revenue from sales where price is not null order by revenue desc limit 3",
)
  |> print()

// the query result can be used in a pipeline like any other table
read_sql<{ product: string, units: int }>("out/shop.db", "select * from product_units")
  |> where(units > 5)
  |> sort(desc(units))
  |> print()
```

```text output
+--------+------------+---------+
| region | day        | revenue |
+--------+------------+---------+
| east   | 2026-02-20 | 199.8   |
| north  | 2026-03-15 | 100.0   |
| north  | 2026-01-17 | 30.0    |
+--------+------------+---------+
+---------+-------+
| product | units |
+---------+-------+
| widget  | 33    |
| gadget  | 9     |
+---------+-------+
```

- SQLite itself is embedded in `biggo`, so there is nothing extra to install.
- The database is opened read-only during `read_sql`.
- Result columns are matched by name. Use `as` in the SQL to give them names that match the declared
  type.
- Values are converted to the declared type: `TEXT` can be converted to `date`, `datetime`,
  `decimal` or a number, and `INTEGER` to `bool` (0 is `false`) or `float`.
- `write_sql` **replaces** the whole table of that name (other tables in the database are not
  touched). The write happens in a single transaction: if it fails partway, the old table is still
  there.
- A `where` that follows `read_sql` runs in biggo and is not pushed into the SQL: if the table is
  large, filter in the query.
- Reading from SQLite runs on a single thread.

## PostgreSQL

When the first argument of `read_sql` or `write_sql` is an address that starts with
`postgres://` (or `postgresql://`), it names a database on a PostgreSQL server in place of a
file:

```text
postgres://user:password@host:port/database
```

```biggo check
// The password is not written in the program: it comes from the environment.
let password = env("SHOP_PASSWORD") ?? ""
let shop = "postgres://report:" + password + "@db.example.com:5432/shop"

type Order = { id: int, customer: string, total: decimal, placed: datetime, paid: bool }
let orders = read_sql<Order>(shop, "select id, customer, total, placed, paid from orders")

let daily = orders
  |> where(paid)
  |> group(day = to_date(placed))
  |> agg(orders = count(), revenue = sum(total))
print(daily |> sort(desc(day)) |> take(7))

// The result as a table of the database, in the schema `reports`.
write_sql(daily, shop, "reports.daily_revenue")
```

- The query is PostgreSQL's SQL, and its result columns are matched to the row type by name.
  PostgreSQL writes names in lower case unless they are quoted, so `select OrderId` gives a
  column named `orderid`.
- Values are read as the declared type: `integer` and `bigint` as `int`, `numeric` as `decimal`
  or `float`, `text` and `varchar` as `string`, `boolean` as `bool`, `date` and `timestamp` as
  `date` and `datetime`. A `timestamptz` is read as its moment in UTC, since a `datetime` has no
  time zone. Any column can be read as `string`.
- Only the columns the program uses are sent by the server, and `take(n)` right after `read_sql`
  stops it after `n` rows. A `where` in biggo is checked after the rows arrive: to filter a large
  table, filter in the query.
- `write_sql` **replaces** the table of that name, in one transaction: the table changes when
  every row is in it, or not at all. `"schema.table"` names a table in a schema. Columns are
  made as `bigint`, `double precision`, `boolean`, `text`, `date`, `timestamp` and
  `numeric(38, 6)`; a `duration` is stored as its seconds.
- Rows cross the connection with PostgreSQL's `COPY`, which is the fastest way in and out of
  the server. The result of a query is held in memory while it is read, on one thread.
- A password with characters such as `@`, `:` or `/` is written with percent signs in an
  address: `p@ss` is `p%40ss`.
- In messages and in `explain`, an address is shown without its password:
  `postgres://report:***@db.example.com:5432/shop`.

How the connection is protected is said with `sslmode`, in the words PostgreSQL's own tools use,
after a `?` at the end of the address:

| `sslmode` | Meaning |
| --- | --- |
| (not given), `prefer` | Encrypted if the server can, and plain if it cannot |
| `require` | Encrypted, or the connection fails |
| `verify-full`, `verify-ca` | Encrypted, and the certificate of the server must be one this machine trusts, made out for the host in the address |
| `disable` | Not encrypted |

As with PostgreSQL's tools, `prefer` and `require` encrypt the connection without checking who
the server is. Use `verify-full` across a network that is not your own:
`postgres://report:...@db.example.com/shop?sslmode=verify-full`.

## MySQL and MariaDB

An address that starts with `mysql://` (or `mariadb://`) names a database on a MySQL or MariaDB
server, and works with `read_sql` and `write_sql` the way a PostgreSQL address does:

```biggo check
let password = env("SHOP_PASSWORD") ?? ""
let shop = "mysql://report:" + password + "@db.example.com:3306/shop"

type Order = { id: int, customer: string, total: decimal, placed: datetime, paid: bool }
let orders = read_sql<Order>(shop, "select id, customer, total, placed, paid from orders")
print(orders |> where(paid) |> group(customer) |> agg(spent = sum(total)) |> sort(desc(spent)) |> take(10))

write_sql(orders |> where(not paid), shop, "unpaid_orders")
```

- The query is MySQL's SQL. Values are read as the declared type, as for PostgreSQL: a
  `TINYINT(1)` or `BOOLEAN` column holds 1 and 0, which a `bool` reads.
- Only the columns the program uses are sent, and `take(n)` right after `read_sql` stops the
  server after `n` rows.
- `write_sql` **replaces** the table of that name. MySQL cannot undo the making of a table, so
  the rows are written to a new table that takes the name, in one step, when every row is in it;
  a query that fails leaves the old table as it was. Columns are made as `BIGINT`, `DOUBLE`,
  `BOOLEAN`, `LONGTEXT`, `DATE`, `DATETIME(6)` and `DECIMAL(38, 6)`.
- A MySQL table cannot hold a `float` that is not a number (`NaN`) or is infinite: writing one
  is an error that names the column.
- The password is hidden in messages, and percent signs write the characters that an address
  cannot hold, as for PostgreSQL.

How the connection is protected is said with `ssl-mode`, in MySQL's own words (in upper or lower
case), after a `?` at the end of the address:

| `ssl-mode` | Meaning |
| --- | --- |
| (not given), `PREFERRED` | Encrypted if the server can, and plain if it cannot |
| `REQUIRED` | Encrypted, or the connection fails |
| `VERIFY_CA`, `VERIFY_IDENTITY` | Encrypted, and the certificate of the server must be signed by a public authority; with `VERIFY_IDENTITY` it must also be made out for the host in the address |
| `DISABLED` | Not encrypted |

## Common errors

| Message | Cause |
| --- | --- |
| `cannot open x.csv: No such file or directory` | Wrong path: paths are relative to the folder of the program file |
| `x.csv has no column `c`; its columns are a, b` | The column name does not match the header (upper and lower case are treated as different) |
| `column `c` of x.csv has missing values, but is declared `int`; declare it `int?`` | There are missing values in a column that is not declared nullable |
| `x.csv, line 12: cannot read 'abc' as an int for column `c`` | The value on that line is not of the declared type |
| `x.csv is not UTF-8 text; if it is in another encoding, name it, as in `encoding = "tis-620"`` | The file is in another encoding, such as TIS-620 or Windows-1252 |
| `x.csv has no column `id`; its columns are id;name; if the file separates its columns with another character, name it, as in `delimiter = ";"`` | The file uses another delimiter than `,`, so the whole header was read as one column |
| `x.csv, line 12: the row has 3 fields, but the header has 5` | The number of fields differs from the header. This usually comes from a `,` or `"` in a value that is not wrapped in `"..."` |
| `x.json: cannot read "abc" as an int for field `c`` | The field's value is not of the declared type |
| `x.json: every line must be one JSON object, but one is 5` | A line (or, in an array, an item) is a number, a string or a list instead of an object |
| `x.json starts a JSON array with `[`, but does not end with `]`` | The file is cut short, or has something after the array |
| `no file matches logs/*.csv` | A pattern that matches nothing: check the folder, which is relative to the program |
| `x.csv has 3 columns, but its row type has 5` | With `header = false`, the file has fewer columns than the row type declares |
| `x.csv has nothing after the 9 lines that `skip` passes over` | `skip` is larger than the file |
| `x.xlsx has no sheet "2026"; its sheets are Sales, Targets` | The name of the sheet is not exact: upper and lower case, and spaces, matter |
| `column `units` of x.xlsx: cannot read 'north' as an int` | A cell of the column holds something else than the declared type, often because the table starts lower in the sheet: see `skip` and `range` |
| `x.db: no such table: t` | A message straight from SQLite |
| `cannot connect to postgres://app:***@db/shop: password authentication failed for user "app"` | What the PostgreSQL server said. `Connection refused` means no server answers at that host and port |
| `postgres://app:***@db/shop: relation "orders" does not exist` | A message straight from PostgreSQL, here for a table that is not there or is in another schema |

Errors from data point to the program line that *runs* the query (such as `print`), not the line
where `read_csv` is written, because the file is read when the query runs.
