# Data sources: CSV, Parquet, JSON, SQLite

biggo reads and writes data in 4 formats. All of them follow the same principles:

- **Read**: `read_xxx<row type>(path)` returns a table. The program says which columns it wants and
  what their types are, and the engine checks them against the file at run time.
- **Write**: `write_xxx(table, path)` runs the query and writes the whole result. It overwrites the
  file if it already exists, and creates the folder if it does not exist yet.
- **path** is relative to the folder of the program file, not to where you run the command. So you
  can move a program, together with its data, and run it from anywhere.
- Reading is lazy: `read_xxx` does not open the file yet. The file is read when the query runs, and
  only the columns the query uses are read.

| Format | Read | Write | Reads in parallel on multiple cores | Skips unused columns |
| --- | --- | --- | --- | --- |
| CSV | `read_csv` | `write_csv` | ✓ | ✓ (values are not converted) |
| Parquet | `read_parquet` | `write_parquet` | ✓ | ✓ (not read from disk at all) |
| JSON (one object per line) | `read_json` | `write_json` | ✓ | ✓ |
| SQLite | `read_sql` | `write_sql` | | (you write it in the query yourself) |

## The row type

The type argument in `<...>` says which columns the program wants:

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

- The first line is always the column names (the header).
- The separator is `,` only.
- A value that contains `,`, a line break, or `"` must be inside `"..."`, and a `"` inside it is
  doubled as `""`.
- The encoding is UTF-8; a BOM at the start of the file is skipped.
- An empty field is null, except in a non-nullable `string` column, where it is the empty string
  `""`.
- Every line must have the same number of fields as the header.

**Speed:** the file is mapped into memory and cut into 2 MiB chunks at line boundaries (counting `"`
characters so that a value containing a line break is not cut in the middle). Each chunk is parsed
on its own core. Columns the query does not use are not converted to values, and the `where` filter
is applied already inside each chunk.

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

`read_json` reads *JSON Lines* (NDJSON) files: one object per line.
The example file [`data/events.json`](data/events.json):

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
- A file that is one big array (`[{...}, {...}]`) is not JSON Lines and cannot be read.
  Convert it first with a tool such as `jq -c '.[]'`.
- `write_json` writes one object per line. Fields that are null are omitted.

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

- This works with SQLite files only (SQLite itself is embedded in `biggo`, so there is nothing extra
  to install). Server databases such as PostgreSQL or MySQL are not supported yet.
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

## Common errors

| Message | Cause |
| --- | --- |
| `cannot open x.csv: No such file or directory` | Wrong path: paths are relative to the folder of the program file |
| `x.csv has no column `c`; its columns are a, b` | The column name does not match the header (upper and lower case are treated as different) |
| `column `c` of x.csv has missing values, but is declared `int`; declare it `int?`` | There are missing values in a column that is not declared nullable |
| `x.csv, line 12: cannot read 'abc' as an int for column `c`` | The value on that line is not of the declared type |
| `x.csv, line 12: the row has 3 fields, but the header has 5` | The number of fields differs from the header. This usually comes from a `,` or `"` in a value that is not wrapped in `"..."` |
| `x.json: cannot read "abc" as an int for field `c`` | The field's value is not of the declared type |
| `x.json: every line must be one JSON object` | The file is not JSON Lines |
| `x.db: no such table: t` | A message straight from SQLite |

Errors from data point to the program line that *runs* the query (such as `print`), not the line
where `read_csv` is written, because the file is read when the query runs.
