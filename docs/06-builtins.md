# Built-in reference

A list of all of biggo's built-in functions, organized by category. The explanatory detail is on
the other pages. This page is for quick lookup.

Notation in the tables: *number* means `int`, `float` or `decimal`; *moment* means `date` or
`datetime`; *any* means any type that can be a column. Every scalar function propagates null: if an
argument is null, the result is null (except `is_null`).

A built-in function can only be called. It cannot be used as a value (`map(xs, upper)` does not
work; write `map(xs, fn(s) { upper(s) })` instead), and you cannot give your own function the same
name as a built-in function.

## Scalar functions

These work both on ordinary values and in column expressions (which compute a whole column at once).

### null

| Function | Result |
| --- | --- |
| `is_null(x)` | `true` if `x` is null; always returns a non-null `bool` |
| `a ?? b` | (operator) `a` if it is not null, otherwise `b` |

### Numbers

| Function | Takes | Returns | Meaning |
| --- | --- | --- | --- |
| `abs(x)` | number | same type | Absolute value |
| `round(x)` | number | `float` (`decimal` if given a `decimal`) | Rounds to a whole number; halves round away from zero |
| `round(x, digits)` | number, `int` | same as above | Rounds to `digits` decimal places; a negative value rounds digits before the decimal point |
| `floor(x)` | number | `float` | Rounds down |
| `ceil(x)` | number | `float` | Rounds up |
| `sqrt(x)` | number | `float` | Square root (`NaN` if `x` is negative) |

```biggo
print(abs(-3), abs(-2.5), abs(-1.5d))
print(round(2.5), round(-2.5), round(3.14159, 2), round(1234.5, -2), round(2.345d, 2))
print(floor(2.7), ceil(2.1), floor(-2.7), sqrt(2), sqrt(-1))
```

```text output
3 2.5 1.5
3.0 -3.0 3.14 1200.0 2.35
2.0 3.0 -3.0 1.4142135623730951 NaN
```

The `digits` argument of `round` must be a program value, not a column.

### string

| Function | Returns | Meaning |
| --- | --- | --- |
| `length(s)` | `int` | Number of characters (Unicode code points) |
| `lower(s)` `upper(s)` | `string` | Lowercase / uppercase |
| `trim(s)` | `string` | Removes whitespace at both ends |
| `contains(s, part)` | `bool` | `s` has `part` inside it |
| `starts_with(s, part)` | `bool` | `s` starts with `part` |
| `ends_with(s, part)` | `bool` | `s` ends with `part` |
| `a + b` | `string` | (operator) Concatenates strings |

```biggo
print(length("biggo"), length("ภาษาไทย"), upper("Abc"), lower("Abc"), trim("  a b  ") + "|")
print(contains("biggo", "gg"), starts_with("biggo", "big"), ends_with("biggo", "x"))
```

```text output
5 7 ABC abc a b|
true true false
```

The second `length` call in the example takes Thai text, which has 7 characters. Search is literal
and case-sensitive. There are no regular expressions, substring extraction or replacement yet.

### Dates and times

| Function | Takes | Returns | Meaning |
| --- | --- | --- | --- |
| `year(d)` `month(d)` `day(d)` | moment | `int` | Year, month (1–12), day of the month |
| `hour(t)` `minute(t)` `second(t)` | moment | `int` | Hour (0–23), minute, second; for a `date` these are 0 |
| `days(n)` `hours(n)` `minutes(n)` `seconds(n)` | `int` or `float` | `duration` | A span of time `n` units long |
| `total_seconds(d)` | `duration` | `float` | Length in seconds |

```biggo
let t = @2026-12-25T18:30:45
print(year(t), month(t), day(t), hour(t), minute(t), second(t), hour(@2026-12-25))
print(days(2), hours(1.5), minutes(90), seconds(0.25), total_seconds(days(1)))
print(t + days(7), t - @2026-01-01, total_seconds(t - @2026-12-25) / 3600)
```

```text output
2026 12 25 18 30 45 0
2d 00:00:00 01:30:00 01:30:00 00:00:00.25 86400.0
2027-01-01T18:30:45 358d 18:30:45 18.5125
```

### Conversions

| Function | Takes | Returns |
| --- | --- | --- |
| `to_int(x)` | `int` `float` `decimal` `string` `bool` | `int` (the fraction is dropped) |
| `to_float(x)` | `int` `float` `decimal` `string` | `float` |
| `to_decimal(x)` | `int` `float` `decimal` `string` | `decimal` |
| `to_string(x)` | any | `string` |
| `to_date(x)` | `string` `date` `datetime` | `date` |
| `to_datetime(x)` | `string` `date` `datetime` | `datetime` |

A string that cannot be converted stops the program with an error. Details and examples are in
[The type system](03-types.md#conversion-functions).

## Aggregate

Used inside `agg(...)`, `pivot(...)` and (except the last four) inside `window(...)`.
They reduce the values of a group to a single value, and skip nulls.

| Function | Takes | Returns |
| --- | --- | --- |
| `count()` | — | `int` number of rows |
| `count(x)` | any | `int` number of non-null values |
| `count_distinct(x)` | any | `int` number of distinct values |
| `sum(x)` | number, `duration` | same type |
| `mean(x)` | number | `float` |
| `median(x)` | number | `float` |
| `stddev(x)` | number | `float?` sample standard deviation; null if there are fewer than 2 values |
| `min(x)` `max(x)` | any except `bool` | same type |
| `first(x)` `last(x)` | any | same type; the value of the first / last row of the group |
| `corr(y, x)` | number, number | `float?` Pearson correlation |
| `cov(y, x)` | number, number | `float?` sample covariance |
| `slope(y, x)` | number, number | `float?` slope of the least-squares line |
| `intercept(y, x)` | number, number | `float?` y-intercept of the same line |

```biggo
let t = from_rows([
  { g: "a", x: 1, y: 2.0 },
  { g: "a", x: 2, y: 4.5 },
  { g: "a", x: 4, y: 7.5 },
  { g: "b", x: 5, y: null },
])
t
  |> group(g)
  |> agg(
    n = count(),
    ys = count(y),
    total = sum(x),
    avg = mean(y),
    mid = median(x),
    spread = round(stddev(x), 3),
    low = min(y),
    start = first(x),
    r = round(corr(y, x), 3),
    line = round(slope(y, x), 3),
  )
  |> print()
```

```text output
+---+---+----+-------+-------------------+-----+--------+------+-------+-------+-------+
| g | n | ys | total | avg               | mid | spread | low  | start | r     | line  |
+---+---+----+-------+-------------------+-----+--------+------+-------+-------+-------+
| a | 3 | 3  | 7     | 4.666666666666667 | 2.0 | 1.528  | 2.0  | 1     | 0.991 | 1.786 |
| b | 1 | 0  | 5     | null              | 5.0 | null   | null | 5     | null  | null  |
+---+---+----+-------+-------------------+-----+--------+------+-------+-------+-------+
```

Details: [group and agg](04-tables.md#summarizing-by-group-group-and-agg), [statistics](04-tables.md#statistics)

## Window function

Used only inside `window(...)`. They give one value per row.

| Function | Returns | Meaning |
| --- | --- | --- |
| `row_number()` | `int` | Position of the row in its partition, starting at 1 |
| `rank()` | `int` | Rank by `order`; equal values get the same rank |
| `lag(x)`, `lag(x, n)` | type of `x` (nullable) | The value `n` rows earlier |
| `lead(x)`, `lead(x, n)` | type of `x` (nullable) | The value `n` rows later |
| `cumsum(x)` | type of `x` | Cumulative sum (`int` or `float`) |
| `moving_avg(x, n)` | `float` | Average of the latest `n` rows |
| any aggregate | same as the aggregate | Its value over the whole partition |

Details: [window](04-tables.md#window)

## Table operations

Each one takes a table as its first argument, so it works with `|>`, and returns a table (unless
noted).

| Operation | Result |
| --- | --- |
| `where(t, cond)` | Rows where `cond` is true |
| `select(t, a, b = expr, ...)` | Only the columns named or computed |
| `drop(t, a, ...)` | Every column except the ones named |
| `rename(t, new = old, ...)` | Renames columns |
| `derive(t, c = expr, ...)` | Adds or replaces columns |
| `sort(t, key, desc(key), asc(key), ...)` | Sorts rows; nulls go last |
| `take(t, n)` | The first `n` rows |
| `skip(t, n)` | Skips the first `n` rows |
| `distinct(t)`, `distinct(t, a, ...)` | Unique rows or values |
| `group(t, a, k = expr, ...)` | A grouped table, to pass on to `agg` or `pivot` |
| `agg(t, name = aggregate, ...)` | One row per group |
| `join(a, b, on = k, how = "inner")` | Matches rows of two tables; `how`: `inner` `left` `right` `full` `semi` `anti` |
| `window(t, by = k, order = k, name = fn, ...)` | Adds columns computed from neighboring rows |
| `union(a, b, ...)` | Appends the rows of several tables |
| `pivot(t, column, [values], aggregate)` | The values of `column` become columns |
| `unpivot(t, a, b, names = "name", values = "value")` | Columns become rows |
| `explode(t, column)`, `explode(t, column, sep)` | Splits a string that holds several values into several rows |
| `collect(t)` | Runs the query and keeps the result in memory |
| `count(t)` | Number of rows (`int`) |

`desc` and `asc` are not functions. They are markers on the keys of `sort` and on the `order` of
`window`.

Details: [Working with tables](04-tables.md)

## Reading and writing data

| Function | Meaning |
| --- | --- |
| `read_csv<T>(path)` | A table from a CSV file |
| `read_parquet<T>(path)` | A table from a Parquet file |
| `read_json<T>(path)` | A table from a JSON Lines file |
| `read_sql<T>(path, query)` | The result of a query on a SQLite database |
| `write_csv(t, path)` | Writes a table as CSV |
| `write_parquet(t, path)` | Writes a table as Parquet |
| `write_json(t, path)` | Writes a table as JSON Lines |
| `write_sql(t, path, name)` | Writes a table to a SQLite database, as the table named `name` |

`T` is the row type, such as `{ id: int, name: string? }`. Details: [Data sources](05-data-sources.md)

## Lists and maps

| Function | Returns | Meaning |
| --- | --- | --- |
| `len(x)` | `int` | Number of elements in a list, or number of keys in a map |
| `range(n)` | `list<int>` | `0` to `n − 1` |
| `range(a, b)` | `list<int>` | `a` to `b − 1` |
| `map(xs, f)` | `list<U>` | `f(x)` for each element |
| `filter(xs, f)` | `list<T>` | The elements for which `f(x)` is `true` |
| `fold(xs, start, f)` | type of `start` | Accumulates a value with `f(acc, x)` |
| `each(xs, f)` | — | Calls `f(x)` on each element in turn; returns no value |
| `keys(m)` | `list<K>` | The keys, in the order they were inserted |
| `values(m)` | `list<V>` | The values, in the same order |
| `put(m, k, v)` | `map<K, V>` | A new map in which `k` has the value `v` |
| `has_key(m, k)` | `bool` | The map has the key `k` |
| `xs[i]`, `m[k]`, `xs + ys` | | (operators) Indexing and list concatenation |
| `to_rows(t)` | `list<{...}>` | The rows of a table as a list of records |
| `from_rows(xs)`, `from_rows<T>(xs)` | table | A table from a list of records |

```biggo
let m = { "a": 1, "b": 2 }
print(len([1, 2, 3]), len(m), range(3), range(2, 5))
print(map([1, 2], fn(x) { x * 10 }), filter([1, 2, 3], fn(x) { x != 2 }))
print(fold(["a", "b"], "", fn(s, x) { s + x }), keys(m), values(m))
print(put(m, "c", 3), has_key(m, "c"), m["a"], m["zz"])
print(to_rows(from_rows([{ id: 1 }, { id: 2 }])))
```

```text output
3 2 [0, 1, 2] [2, 3, 4]
[10, 20] [1, 3]
ab ["a", "b"] [1, 2]
{"a": 1, "b": 2, "c": 3} false 1 null
[{id: 1}, {id: 2}]
```

Details: [list](02-language.md#list), [map](02-language.md#map),
[tables and lists of records](04-tables.md#tables-and-lists-of-records)

## Summaries and statistics

| Function | Returns | Meaning |
| --- | --- | --- |
| `describe(t)` | table | Summary statistics for every column |
| `histogram(t, column)`, `histogram(t, column, bins = n)` | table | Number of values in each interval |
| `linreg(t, y, x)` | `{ slope, intercept, r2 }` | Linear regression line of `y` on `x` |

Details: [statistics](04-tables.md#statistics)

## Output and checks

| Function | Meaning |
| --- | --- |
| `print(a, b, ...)` | Prints the values separated by spaces, then a newline; a table is run and its first 50 rows are printed |
| `explain(t)` | Prints the query's plan, both before and after it is optimized, without running it |
| `assert(cond)`, `assert(cond, message)` | Stops the program with an error if `cond` is not `true` |
| `assert_eq(a, b)` | Stops the program if the two values are not equal; it can compare lists, records, maps and tables |

```biggo
print("total:", 42, [1, 2], { ok: true })
assert(1 + 1 == 2, "arithmetic is broken")
assert_eq(map([1, 2], fn(n) { n * 2 }), [2, 4])
assert_eq(from_rows([{ id: 1 }]) |> derive(twice = id * 2), from_rows([{ id: 1, twice: 2 }]))
explain(from_rows([{ id: 1 }, { id: 2 }]) |> where(id > 1) |> select(id))
```

```text output
total: 42 [1, 2] {ok: true}
plan:
  Project: id
    Filter: id > 1
      Table: 2 rows
optimized plan:
  Filter: id > 1
    Table: 2 rows
```

`print` shows strings and dates bare when they are top-level values, and as literals
(with `"` or `@`) when they are inside a list, record or map. Details of `assert`:
[biggo test](07-tools.md#biggo-test)
