# Working with tables

The table is the main data type in biggo. This page covers every table operation.
All examples on this page use this `sales` table (the file [`data/sales.csv`](data/sales.csv)):

```biggo prelude
type Sale = { date: date, region: string, product: string, qty: int, price: float? }
let sales = read_csv<Sale>("data/sales.csv")
```

```biggo
print(sales)
```

```text output
+------------+--------+---------+-----+-------+
| date       | region | product | qty | price |
+------------+--------+---------+-----+-------+
| 2026-01-05 | north  | widget  | 10  | 2.5   |
| 2026-01-17 | north  | gadget  | 3   | 10.0  |
| 2026-01-20 | south  | widget  | 0   | 2.5   |
| 2026-02-02 | south  | gadget  | 5   | null  |
| 2026-02-14 | north  | widget  | 7   | 2.5   |
| 2026-02-20 | east   | gizmo   | 2   | 99.9  |
| 2025-12-30 | east   | widget  | 4   | 2.5   |
| 2026-03-01 | south  | widget  | 12  | 2.5   |
| 2026-03-09 | east   | gadget  | 1   | 10.0  |
| 2026-03-15 | north  | gizmo   | 1   | 100.0 |
+------------+--------+---------+-----+-------+
```

## A table is a query that has not run yet

`read_csv` does not read the file, and `where` does not filter anything right away. Each operation
only *extends the plan*. A variable that holds a table therefore holds a plan, not data. The plan
runs when the program needs an actual result, that is, when you call:

| Function | What it does |
| --- | --- |
| `print(t)` | Runs the plan and prints the first 50 rows |
| `write_csv` `write_parquet` `write_json` `write_sql` | Runs the plan and writes the whole result |
| `count(t)` | Runs the plan and gives the number of rows (`int`) |
| `collect(t)` | Runs the plan and keeps the result in memory, giving a new table |
| `to_rows(t)` | Runs the plan and gives a `list` of records |
| `describe(t)` `histogram(t, ...)` `linreg(t, ...)` | Run the plan to compute statistics |
| `assert_eq(t1, t2)` | Runs both tables and compares them |

Before a plan runs, the whole plan is optimized: it filters as early as possible, reads only the
columns that are used, and stops reading once it has enough rows. You can see this with `explain(t)`
or the `biggo explain` command.

```biggo explain
sales
  |> derive(revenue = qty * (price ?? 0.0))
  |> where(region == "north")
  |> select(product, revenue)
  |> take(2)
  |> print()
```

```text output
plan:
  Limit: take 2
    Project: product, revenue
      Filter: region == "north"
        Project: date, region, product, qty, price, revenue = float(qty) * (price ?? 0.0)
          Scan csv "data/sales.csv": date, region, product, qty, price
optimized plan:
  Project: product, revenue = float(qty) * (price ?? 0.0)
    Limit: take 2
      Scan csv "data/sales.csv": product, qty, price where region == "north" limit 2
```

One consequence is that **a table used twice is computed twice**. If an intermediate table is
expensive to compute and is used in several places, `collect` it first:

```biggo
let large = collect(sales |> where(qty > 5))      // the file is read once, here
print(count(large), large |> agg(units = sum(qty)))
```

```text output
3
+-------+
| units |
+-------+
| 29    |
+-------+
```

## Column expressions

Inside the arguments of operations such as `where`, `derive`, and `agg`, you can use column names
like variables. An expression like this is computed *a whole column at a time* by the engine, not
row by row by the interpreter. What you can use in a column expression:

- Column names, literals, and values from the program (variables, function results)
- All operators: arithmetic, comparison, `and`, `or`, `not`, `??`
- `if ... else ...` (the `else` is required) and `match`
- Built-in functions that work on scalar values: `round`, `upper`, `year`, `to_int`, `is_null`, and so on
  ([full list](06-builtins.md#scalar-functions))

```biggo
let min_qty = 4                                   // a value from the program can be used in an expression
fn tax_rate() -> float { 0.07 }

sales
  |> where(qty >= min_qty and region != "east")
  |> derive(
    revenue = qty * (price ?? 0.0),
    label = upper(region) + "-" + product,
    size = match qty { 4 | 5 => "small", _ => "large" },
  )
  |> derive(taxed = round(revenue * (1 + tax_rate()), 2))   // a column you just created can be used
  |> select(date, label, size, revenue, taxed)
  |> print()
```

```text output
+------------+--------------+-------+---------+-------+
| date       | label        | size  | revenue | taxed |
+------------+--------------+-------+---------+-------+
| 2026-01-05 | NORTH-widget | large | 25.0    | 26.75 |
| 2026-02-02 | SOUTH-gadget | small | 0.0     | 0.0   |
| 2026-02-14 | NORTH-widget | large | 17.5    | 18.73 |
| 2026-03-01 | SOUTH-widget | large | 30.0    | 32.1  |
+------------+--------------+-------+---------+-------+
```

A part of an expression that does not touch any column (such as `1 + tax_rate()`) is computed
**once**, when the plan is built, and its value is embedded in the plan. It is not called again for
every row.

What you cannot do: pass a column to a user-defined function.

```biggo error
fn double(n: int) -> int { n * 2 }
print(sales |> derive(twice = double(qty)))
```

```text output
error: a user-defined function cannot take a column; only operators and built-in functions work on columns
 --> example.bgo:2:38
  |
2 | print(sales |> derive(twice = double(qty)))
  |                                      ^^^
```

This is because a user-defined function works on one value at a time in the interpreter, which is
hundreds of times slower than the engine. The language therefore does not let you write a slow
pipeline by accident. If you want something reusable, write a function that *takes and returns a
table* instead (see [Functions that take and return tables](#functions-that-take-and-return-tables)).

Other rules worth knowing:

- A variable and a column with the same name is a compile error
  (`is both a column of this table and a variable`). Rename the variable.
- null works the same way as it does outside tables: `price > 50` gives null for a row where `price` is null
- A column whose name contains a space or matches a keyword is wrapped in backticks: `` `order date` ``
- An expression that can fail at run time (such as `%` by zero) is computed only for the rows that
  actually need it: `if n != 0 { 10 % n } else { 0 }` is always safe

## Selecting rows

```biggo
// where: keeps the rows where the condition is true (rows where the condition is null are dropped)
print(sales |> where(price > 50) |> select(product, price))
print(sales |> where(is_null(price)))

// take / skip: the first n rows / skip the first n rows
print(sales |> sort(desc(qty)) |> skip(1) |> take(2) |> select(product, qty))

// distinct: removes duplicate rows, by the whole row or only by the given columns
print(sales |> distinct(region))
print(sales |> select(region, product) |> distinct() |> count())
```

```text output
+---------+-------+
| product | price |
+---------+-------+
| gizmo   | 99.9  |
| gizmo   | 100.0 |
+---------+-------+
+------------+--------+---------+-----+-------+
| date       | region | product | qty | price |
+------------+--------+---------+-----+-------+
| 2026-02-02 | south  | gadget  | 5   | null  |
+------------+--------+---------+-----+-------+
+---------+-----+
| product | qty |
+---------+-----+
| widget  | 10  |
| widget  | 7   |
+---------+-----+
+--------+
| region |
+--------+
| north  |
| south  |
| east   |
+--------+
8
```

| Operation | Result |
| --- | --- |
| `where(cond)` | Rows where `cond` is `true`; `cond` is `bool` or `bool?` |
| `take(n)` | The first `n` rows |
| `skip(n)` | Every row except the first `n` |
| `distinct()` | The unique rows, in order of first appearance |
| `distinct(a, b)` | The unique values of columns `a`, `b` (the result has only the given columns) |

The `n` of `take`/`skip` can be a program expression (`take(limit * 2)`), but it must not be negative.

## Selecting and creating columns

```biggo
print(sales |> select(product, qty) |> take(2))
// select can create new columns with name = expression
print(sales |> select(product, year = year(date), total = qty * (price ?? 0.0)) |> take(2))
print(sales |> drop(date, price) |> rename(quantity = qty, area = region) |> take(2))
// derive adds a column, or replaces the column that has the same name
print(sales |> derive(qty = qty * 10, big = qty >= 5) |> take(2))
```

```text output
+---------+-----+
| product | qty |
+---------+-----+
| widget  | 10  |
| gadget  | 3   |
+---------+-----+
+---------+------+-------+
| product | year | total |
+---------+------+-------+
| widget  | 2026 | 25.0  |
| gadget  | 2026 | 30.0  |
+---------+------+-------+
+-------+---------+----------+
| area  | product | quantity |
+-------+---------+----------+
| north | widget  | 10       |
| north | gadget  | 3        |
+-------+---------+----------+
+------------+--------+---------+-----+-------+------+
| date       | region | product | qty | price | big  |
+------------+--------+---------+-----+-------+------+
| 2026-01-05 | north  | widget  | 100 | 2.5   | true |
| 2026-01-17 | north  | gadget  | 30  | 10.0  | true |
+------------+--------+---------+-----+-------+------+
```

| Operation | Result |
| --- | --- |
| `select(a, b, c = expr)` | Only the given columns, in the order written |
| `drop(a, b)` | Every column except the given ones |
| `rename(new = old)` | Renames the column; its position stays the same |
| `derive(c = expr)` | Every existing column, plus the new columns at the end; a name that matches an existing column replaces it |

Within a single `derive`, a later column can use a column declared before it. In the example above,
`big = qty >= 5` sees the `qty` that has already been multiplied by 10.

## Sorting

```biggo
print(sales |> sort(region, desc(qty)) |> select(region, product, qty) |> take(5))
print(sales |> sort(desc(price), product) |> select(product, price) |> skip(7))
```

```text output
+--------+---------+-----+
| region | product | qty |
+--------+---------+-----+
| east   | widget  | 4   |
| east   | gizmo   | 2   |
| east   | gadget  | 1   |
| north  | widget  | 10  |
| north  | widget  | 7   |
+--------+---------+-----+
+---------+-------+
| product | price |
+---------+-------+
| widget  | 2.5   |
| widget  | 2.5   |
| gadget  | null  |
+---------+-------+
```

- `sort(a, b)` sorts by `a` first, then looks at `b` when the values are equal. The default is ascending.
- `desc(x)` reverses the order to descending, and `asc(x)` lets you state ascending explicitly
- A key can be an expression: `sort(desc(qty * price))`
- **null always comes last**, with both `asc` and `desc`
- Rows with equal keys keep their original order (stable sort)
- `sort` followed by `take(n)` is combined into a top-n: the whole table does not have to be sorted

## Summarizing by group: group and agg

`group(...)` splits the rows into groups by the values of columns, then `agg(...)` reduces each group to one row.

```biggo
sales
  |> group(region)
  |> agg(
    orders = count(),            // number of rows
    priced = count(price),       // number of non-null values
    units = sum(qty),
    avg_qty = mean(qty),
    high = max(price),
    products = count_distinct(product),
  )
  |> sort(region)
  |> print()
```

```text output
+--------+--------+--------+-------+--------------------+-------+----------+
| region | orders | priced | units | avg_qty            | high  | products |
+--------+--------+--------+-------+--------------------+-------+----------+
| east   | 3      | 3      | 7     | 2.3333333333333335 | 99.9  | 3        |
| north  | 4      | 4      | 21    | 5.25               | 100.0 | 3        |
| south  | 3      | 2      | 17    | 5.666666666666667  | 2.5   | 2        |
+--------+--------+--------+-------+--------------------+-------+----------+
```

The available aggregates:

| Function | Result | Accepts |
| --- | --- | --- |
| `count()` | Number of rows in the group | — |
| `count(x)` | Number of non-null values | Any type |
| `count_distinct(x)` | Number of distinct values (null is not counted) | Any type |
| `sum(x)` | Sum | Numbers, `duration` |
| `mean(x)` | Mean (`float`) | Numbers |
| `median(x)` | Median (`float`) | Numbers |
| `stddev(x)` | Sample standard deviation (divides by n−1) | Numbers |
| `min(x)` `max(x)` | Smallest/largest value | Any type except `bool` |
| `first(x)` `last(x)` | Value of the first/last row of the group | Any type |
| `corr(y, x)` `cov(y, x)` | Correlation, covariance | Numbers |
| `slope(y, x)` `intercept(y, x)` | Linear regression line | Numbers |

Every aggregate skips null (except `count()`, which counts rows). When there are no values to
aggregate at all, `count` gives `0` and the others give null, except for `sum` of a non-nullable
column on an empty table, which gives `0`.

The argument of an aggregate can be an expression, and the result of an aggregate can be used in further computation:

```biggo
sales
  |> group(month = month(date), large = qty >= 5)      // group keys can be computed
  |> agg(
    per_order = sum(qty) / count(),
    revenue = round(sum(qty * (price ?? 0.0)), 1),
  )
  |> sort(month, large)
  |> print()
```

```text output
+-------+-------+-----------+---------+
| month | large | per_order | revenue |
+-------+-------+-----------+---------+
| 1     | false | 1.5       | 30.0    |
| 1     | true  | 10.0      | 25.0    |
| 2     | false | 2.0       | 199.8   |
| 2     | true  | 6.0       | 17.5    |
| 3     | false | 1.0       | 110.0   |
| 3     | true  | 12.0      | 30.0    |
| 12    | false | 4.0       | 10.0    |
+-------+-------+-----------+---------+
```

`agg` without `group` aggregates the whole table into a single row (you always get one row, even when the table is empty):

```biggo
print(sales |> agg(rows = count(), total = sum(qty), best = max(price)))
print(sales |> where(qty > 1000) |> agg(rows = count(), total = sum(qty), best = max(price)))
```

```text output
+------+-------+-------+
| rows | total | best  |
+------+-------+-------+
| 10   | 45    | 100.0 |
+------+-------+-------+
+------+-------+------+
| rows | total | best |
+------+-------+------+
| 0    | 0     | null |
+------+-------+------+
```

Rules of `agg`:

- Every argument must be `name = expression`, and the expression must be an aggregate
  or be computed from aggregates and the group's columns
- A column that is not a group key must be inside an aggregate: `agg(x = qty)` is an error
- The result has the group's columns followed by the columns declared in `agg`
- **The order of the groups in the result is the order in which each group first appears in the data**.
  If you want a different order, use `sort`.
- The result of `group` (before any `agg`) can be used only with `agg` and `pivot`, but you can keep
  it in a variable and apply `agg` to it in several ways

## join

`join` matches the rows of two tables where the keys are equal.

```biggo
type Order = { order_id: int, customer_id: int?, amount: float }
type Customer = { customer_id: int, name: string, city: string }
let orders = read_csv<Order>("data/orders.csv")
let customers = read_csv<Customer>("data/customers.csv")
print(orders)
print(customers)

// inner join (the default): only the rows that have a match
print(orders |> join(customers, on = customer_id) |> sort(order_id))
// left join: every row of the left table; the right side is null when there is no match
print(orders |> join(customers, on = customer_id, how = "left") |> sort(order_id))
// semi / anti: rows of the left table that have / do not have a match in the right table
print(customers |> join(orders, on = customer_id, how = "anti"))
```

```text output
+----------+-------------+--------+
| order_id | customer_id | amount |
+----------+-------------+--------+
| 1        | 10          | 25.0   |
| 2        | 11          | 10.5   |
| 3        | 10          | 4.0    |
| 4        | 13          | 99.0   |
| 5        | null        | 1.0    |
+----------+-------------+--------+
+-------------+------+------------+
| customer_id | name | city       |
+-------------+------+------------+
| 10          | Ann  | Bangkok    |
| 11          | Bob  | Chiang Mai |
| 12          | Cho  | Phuket     |
+-------------+------+------------+
+----------+-------------+--------+------+------------+
| order_id | customer_id | amount | name | city       |
+----------+-------------+--------+------+------------+
| 1        | 10          | 25.0   | Ann  | Bangkok    |
| 2        | 11          | 10.5   | Bob  | Chiang Mai |
| 3        | 10          | 4.0    | Ann  | Bangkok    |
+----------+-------------+--------+------+------------+
+----------+-------------+--------+------+------------+
| order_id | customer_id | amount | name | city       |
+----------+-------------+--------+------+------------+
| 1        | 10          | 25.0   | Ann  | Bangkok    |
| 2        | 11          | 10.5   | Bob  | Chiang Mai |
| 3        | 10          | 4.0    | Ann  | Bangkok    |
| 4        | 13          | 99.0   | null | null       |
| 5        | null        | 1.0    | null | null       |
+----------+-------------+--------+------+------------+
+-------------+------+--------+
| customer_id | name | city   |
+-------------+------+--------+
| 12          | Cho  | Phuket |
+-------------+------+--------+
```

| `how` | Rows in the result |
| --- | --- |
| `"inner"` (the default) | Pairs whose keys match |
| `"left"` | Every row of the left table; the right-side columns are null when there is no match |
| `"right"` | Every row of the right table; the left-side columns are null when there is no match |
| `"full"` | Every row of both tables |
| `"semi"` | Rows of the left table that have a match (only the left table's columns, no repeated rows) |
| `"anti"` | Rows of the left table that have no match |

Specifying the key:

- `on = key` when the column has the same name in both tables. The result has a single key column.
- `on = [a, b]` when the key has several columns
- `left_on = a, right_on = b` when the names differ. The result keeps both columns.

```biggo
type Order = { order_id: int, customer_id: int?, amount: float }
type Customer = { customer_id: int, name: string, city: string }
let orders = read_csv<Order>("data/orders.csv")
let people = read_csv<Customer>("data/customers.csv") |> rename(id = customer_id)

orders
  |> join(people, left_on = customer_id, right_on = id)
  |> select(order_id, id, name, amount)
  |> sort(order_id)
  |> print()

// total per city, including customers that have no order yet
people
  |> join(orders, left_on = id, right_on = customer_id, how = "left")
  |> group(city)
  |> agg(orders = count(order_id), total = sum(amount))
  |> sort(city)
  |> print()
```

```text output
+----------+----+------+--------+
| order_id | id | name | amount |
+----------+----+------+--------+
| 1        | 10 | Ann  | 25.0   |
| 2        | 11 | Bob  | 10.5   |
| 3        | 10 | Ann  | 4.0    |
+----------+----+------+--------+
+------------+--------+-------+
| city       | orders | total |
+------------+--------+-------+
| Bangkok    | 2      | 29.0  |
| Chiang Mai | 1      | 10.5  |
| Phuket     | 0      | null  |
+------------+--------+-------+
```

Rules of `join`:

- A null key matches nothing (including another null)
- The keys on both sides must have the same type (for `int` and `float`, convert one side first)
- A non-key column with the same name in both tables is a compile error: `rename` or `drop` it on one side first
- The type of the result reflects `how`: in a left join, every right-side column becomes `T?`
- The engine builds a hash table from the right table and then scans the left table: when you can,
  **put the smaller table on the right**

## window

`window` computes a value for each row from the other rows in the same "window", without collapsing rows.
`by` splits the table into partitions, and `order` sets the order within a partition.

```biggo
type Visit = { day: date, site: string, visits: int? }
let log = read_csv<Visit>("data/visits.csv")

log
  |> window(
    by = site,
    order = day,
    n = row_number(),
    prev = lag(visits),
    running = cumsum(visits),
    avg2 = moving_avg(visits, 2),
    site_total = sum(visits),
    share = round(visits / sum(visits), 2),
  )
  |> print()
```

```text output
+------------+------+--------+---+------+---------+------+------------+-------+
| day        | site | visits | n | prev | running | avg2 | site_total | share |
+------------+------+--------+---+------+---------+------+------------+-------+
| 2026-01-01 | a    | 10     | 1 | null | 10      | 10.0 | 45         | 0.22  |
| 2026-01-02 | a    | 15     | 2 | 10   | 25      | 12.5 | 45         | 0.33  |
| 2026-01-03 | a    | null   | 3 | 15   | 25      | 15.0 | 45         | null  |
| 2026-01-04 | a    | 20     | 4 | null | 45      | 20.0 | 45         | 0.44  |
| 2026-01-01 | b    | 7      | 1 | null | 7       | 7.0  | 17         | 0.41  |
| 2026-01-02 | b    | 7      | 2 | 7    | 14      | 7.0  | 17         | 0.41  |
| 2026-01-03 | b    | 3      | 3 | 7    | 17      | 5.0  | 17         | 0.18  |
+------------+------+--------+---+------+---------+------+------------+-------+
```

| Function | Value for each row |
| --- | --- |
| `row_number()` | Position of the row in its partition, starting from 1 |
| `rank()` | Rank by `order`; equal rows get the same rank, then the next number is skipped |
| `lag(x)`, `lag(x, n)` | Value of `x` in the row `n` rows before (default 1); null if there is none |
| `lead(x)`, `lead(x, n)` | Value of `x` in the row `n` rows after |
| `cumsum(x)` | Running total from the start of the partition up to this row |
| `moving_avg(x, n)` | Mean of this row and the `n − 1` rows before it |
| `sum` `mean` `min` `max` `count` etc. | The aggregate of the whole partition, repeated on every row |

- `by` and `order` take a single column or a list: `by = [site, region]`, `order = [desc(visits), day]`
- Without `by`, the whole table is one partition
- The result has every existing column plus the declared columns.
  **The number and order of rows are the same as in the source table**: `order` only sets the order
  used for the computation. If you want the result sorted, add a `sort` afterwards.
- The `n` of `lag`/`lead`/`moving_avg` must be written as a literal number

A common pattern, top-n per group:

```biggo
sales
  |> window(by = region, order = desc(qty), place = row_number())
  |> where(place <= 2)
  |> select(region, place, product, qty)
  |> sort(region, place)
  |> print()
```

```text output
+--------+-------+---------+-----+
| region | place | product | qty |
+--------+-------+---------+-----+
| east   | 1     | widget  | 4   |
| east   | 2     | gizmo   | 2   |
| north  | 1     | widget  | 10  |
| north  | 2     | widget  | 7   |
| south  | 1     | widget  | 12  |
| south  | 2     | gadget  | 5   |
+--------+-------+---------+-----+
```

## union

`union(a, b, ...)` appends the rows of tables that have the same columns (names, types, order). It does not remove duplicate rows.

```biggo
let north = sales |> where(region == "north") |> select(product, qty)
let east = sales |> where(region == "east") |> select(product, qty)
print(union(north, east))
print(union(north, east) |> distinct(product))
```

```text output
+---------+-----+
| product | qty |
+---------+-----+
| widget  | 10  |
| gadget  | 3   |
| widget  | 7   |
| gizmo   | 1   |
| gizmo   | 2   |
| widget  | 4   |
| gadget  | 1   |
+---------+-----+
+---------+
| product |
+---------+
| widget  |
| gadget  |
| gizmo   |
+---------+
```

## Reshaping: pivot, unpivot, explode

### pivot: rows → columns

`pivot(column, [value...], aggregate)` creates one new column for each value given.
Each column holds the aggregate of the rows where `column` has that value. The `group` before
`pivot` determines what the rows of the result are.

```biggo
let wide = sales
  |> group(product)
  |> pivot(region, ["north", "south", "east"], sum(qty))
  |> sort(product)
print(wide)

// several aggregates must be named; the column name is value_name
sales
  |> group(month = month(date))
  |> pivot(product, ["widget", "gadget"], orders = count(), best = max(price))
  |> sort(month)
  |> print()
```

```text output
+---------+-------+-------+------+
| product | north | south | east |
+---------+-------+-------+------+
| gadget  | 3     | 5     | 1    |
| gizmo   | 1     | null  | 2    |
| widget  | 17    | 12    | 4    |
+---------+-------+-------+------+
+-------+---------------+---------------+-------------+-------------+
| month | widget_orders | gadget_orders | widget_best | gadget_best |
+-------+---------------+---------------+-------------+-------------+
| 1     | 2             | 1             | 2.5         | 10.0        |
| 2     | 1             | 1             | 2.5         | null        |
| 3     | 1             | 1             | 2.5         | 10.0        |
| 12    | 1             | 0             | 2.5         | null        |
+-------+---------------+---------------+-------------+-------------+
```

You have to list the values that become columns yourself, because the type of the table (its list of
columns) must be known at compile time. A value that is not in the list is skipped. A value that is
in the list but has no data gives null (or `0` for `count`). You can find the list of values with
`distinct(region)`. `first` and `last` cannot be used in `pivot`.

### unpivot: columns → rows

`unpivot(a, b, ...)` does the reverse: each row becomes several rows, one for each column given.
You get two new columns: the name of the original column, and its value.

```biggo
let wide = sales |> group(product) |> pivot(region, ["north", "south"], sum(qty))
print(wide |> unpivot(north, south, names = "region", values = "qty") |> sort(product, region))
```

```text output
+---------+--------+------+
| product | region | qty  |
+---------+--------+------+
| gadget  | north  | 3    |
| gadget  | south  | 5    |
| gizmo   | north  | 1    |
| gizmo   | south  | null |
| widget  | north  | 17   |
| widget  | south  | 12   |
+---------+--------+------+
```

- The given columns must have the same type
- `names` and `values` name the new columns (the defaults are `"name"` and `"value"`). They must be written as literal strings.
- Columns that are not given are kept, and their values are repeated in every row that is produced

### explode: one row → many rows

`explode(column)` splits a string that holds several values separated by `,` into one row per value,
and trims the whitespace around each value. A second argument changes the separator.

```biggo
let people = from_rows([
  { name: "Ann", langs: "en, th,ja" },
  { name: "Bo", langs: "th" },
  { name: "Cy", langs: null },
])
print(people |> explode(langs))
print(people |> explode(langs) |> group(langs) |> agg(speakers = count()) |> sort(langs))
print(from_rows([{ path: "usr/local/bin" }]) |> explode(path, "/"))
```

```text output
+------+-------+
| name | langs |
+------+-------+
| Ann  | en    |
| Ann  | th    |
| Ann  | ja    |
| Bo   | th    |
| Cy   | null  |
+------+-------+
+-------+----------+
| langs | speakers |
+-------+----------+
| en    | 1        |
| ja    | 1        |
| th    | 2        |
| null  | 1        |
+-------+----------+
+-------+
| path  |
+-------+
| usr   |
| local |
| bin   |
+-------+
```

A row whose value is null stays as one row whose value is null.

## Statistics

```biggo
let points = from_rows([
  { x: 1, y: 2.1 },
  { x: 2, y: 3.9 },
  { x: 3, y: 6.2 },
  { x: 4, y: 7.8 },
  { x: 5, y: 10.1 },
])

// correlation and the regression line y = slope * x + intercept
print(points |> agg(r = corr(y, x), slope = slope(y, x), intercept = intercept(y, x)))

// linreg gives a record for the regression line, including r²
let line = linreg(points, y, x)
print(line)
print((line.slope ?? 0.0) * 6 + (line.intercept ?? 0.0))      // predict y at x = 6
```

```text output
+--------------------+--------------------+---------------------+
| r                  | slope              | intercept           |
+--------------------+--------------------+---------------------+
| 0.9986517555689657 | 1.9899999999999998 | 0.05000000000000071 |
+--------------------+--------------------+---------------------+
{slope: 1.9899999999999998, intercept: 0.05000000000000071, r2: 0.9973053289009772}
11.989999999999998
```

- `corr(y, x)` is the Pearson correlation coefficient; `cov(y, x)` is the sample covariance
- `slope(y, x)` and `intercept(y, x)` are the least-squares line: **the dependent variable comes first**
- Rows where `x` or `y` is null are skipped. If fewer than 2 rows remain, or `x` has no variation, the result is null.
- All four are aggregates, so you can use them with `group` to find the relationship for each group separately
- `linreg(t, y, x)` gives a record `{ slope, intercept, r2 }`. Every field is `float?`.

`describe` summarizes every column in a single pass over the data, and `histogram` counts the distribution of a numeric column:

```biggo
print(describe(sales))
print(histogram(sales, qty, bins = 4))
```

```text output
+---------+--------+-------+-------+--------------------+-------------------+------+--------+-------+
| column  | type   | count | nulls | mean               | stddev            | min  | median | max   |
+---------+--------+-------+-------+--------------------+-------------------+------+--------+-------+
| date    | date   | 10    | 0     | null               | null              | null | null   | null  |
| region  | string | 10    | 0     | null               | null              | null | null   | null  |
| product | string | 10    | 0     | null               | null              | null | null   | null  |
| qty     | int    | 10    | 0     | 4.5                | 4.034572812303401 | 0.0  | 3.5    | 12.0  |
| price   | float? | 9     | 1     | 25.822222222222223 | 42.14584136595739 | 2.5  | 2.5    | 100.0 |
+---------+--------+-------+-------+--------------------+-------------------+------+--------+-------+
+-----------+---------+-------+
| bin_start | bin_end | count |
+-----------+---------+-------+
| 0.0       | 3.0     | 4     |
| 3.0       | 6.0     | 3     |
| 6.0       | 9.0     | 1     |
| 9.0       | 12.0    | 2     |
+-----------+---------+-------+
```

- `describe(t)` gives a table with one row per column: the type, the number of values, the number of nulls,
  and for numeric columns the mean, standard deviation, minimum, median, and maximum
- `histogram(t, column)` divides the range between the minimum and the maximum into 10 bins of equal
  width (`bins = n` changes the number), then counts the values in each bin. The maximum falls in
  the last bin. null is not counted.
- Both return an ordinary table, which you can pass on to `where` or `sort`, or write to a file

## Tables and lists of records

`to_rows` pulls the result of a query out as a `list` of records for use in ordinary code,
and `from_rows` creates a table from a `list` of records.

```biggo
let top = to_rows(sales |> sort(desc(qty)) |> take(2) |> select(product, qty))
print(top)
print(top[0].product, map(top, fn(row) { row.qty * 2 }))

each(top, fn(row) {
  print(row.product, "sold", row.qty)
})

let cities = from_rows([
  { city: "Bangkok", people: 10.5 },
  { city: "Chiang Mai", people: null },
])
print(cities |> where(not is_null(people)))

// an empty list has no row type, so you have to state it yourself
print(from_rows<{ id: int, note: string? }>([]))
```

```text output
[{product: "widget", qty: 12}, {product: "widget", qty: 10}]
widget [24, 20]
widget sold 12
widget sold 10
+---------+--------+
| city    | people |
+---------+--------+
| Bangkok | 10.5   |
+---------+--------+
+----+------+
| id | note |
+----+------+
+----+------+
```

`to_rows` loads every row into memory as interpreter values, so it suits small results
(summaries, top-n, config values). Work on large amounts of data should stay in the form of table
operations for as long as possible. An example of using the result of one query as a value in another query:

```biggo
let average = to_rows(sales |> agg(m = mean(qty)))[0].m ?? 0.0
print(average)
print(sales |> where(qty > average) |> select(product, qty))
```

```text output
4.5
+---------+-----+
| product | qty |
+---------+-----+
| widget  | 10  |
| gadget  | 5   |
| widget  | 7   |
| widget  | 12  |
+---------+-----+
```

## Functions that take and return tables

You write a reusable step as an ordinary function that takes and returns a table, and then use it
in a pipeline like a built-in function.

```biggo
type Priced = table<{ product: string, qty: int, price: float? }>
type Revenue = table<{ product: string, revenue: float }>

fn revenue_by_product(t: Priced, min_qty: int) -> Revenue {
  t
    |> where(qty >= min_qty)
    |> group(product)
    |> agg(revenue = sum(qty * (price ?? 0.0)))
}

sales
  |> select(product, qty, price)
  |> revenue_by_product(2)
  |> sort(desc(revenue))
  |> print()
```

```text output
+---------+---------+
| product | revenue |
+---------+---------+
| gizmo   | 199.8   |
| widget  | 82.5    |
| gadget  | 30.0    |
+---------+---------+
```

A function like this does not slow anything down: it is called once to *build the plan*, and then
the whole plan is optimized together, as if you had written the steps directly one after another.

## Row order and reproducible results

The engine runs on several cores, but **the result is the same every time, no matter how many cores the machine has**:

- Operations that do not sort (`where` `select` `derive` `join` ...) keep the row order of the source data
- `group` gives the groups in order of first appearance
- `sort` is a stable sort
- Sums of `float` are added in a fixed order, so every digit of the result is the same, no matter how many threads are used
- An error is part of the result: a query fails on any number of threads or on none, and with the
  same message. A query that stops early, such as `take(5)`, does not fail for a bad value in a
  row that it never needed, however far ahead other cores had read
- A sum of `int` overflows only if the sum itself does not fit, whatever the order of the rows

The number of threads is set with the environment variable `RAYON_NUM_THREADS` (the default is the number of cores).
