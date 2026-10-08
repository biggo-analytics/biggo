# The type system

biggo checks the types of the whole program before the program runs (static typing). Every
expression has a type that is known at compile time, including tables, whose type is the list of
columns with their types. As a result:

- A mistyped column name, a string added to a number, or a forgotten null check is caught before
  any data is read.
- The engine knows the type of every column in advance, so it can choose the fastest way to compute
  without checking at run time.

Most of the time you do not write types yourself: the compiler infers them from values. What you do
have to write are function parameters and the row types of data files.

## All types

| Type | Values | Can be a column |
| --- | --- | --- |
| `int` | 64-bit integer | ✓ |
| `float` | 64-bit floating-point number | ✓ |
| `bool` | `true` / `false` | ✓ |
| `string` | UTF-8 text | ✓ |
| `date` | a date | ✓ |
| `datetime` | a date and time, with microsecond precision, no time zone | ✓ |
| `duration` | a span of time, with microsecond precision | ✓ |
| `decimal` | exact decimal number with 6 decimal places, 38 digits in total | ✓ |
| `T?` | a value of type `T`, or null | ✓ |
| `list<T>` | a sequence of values of type `T` | |
| `map<K, V>` | key-value pairs | |
| `{ a: T, b: U }` | record: a value with named fields | |
| `table<{ a: T, ... }>` | a table with those columns | |
| `fn(T, U) -> R` | a function | |

There are also two types that you cannot write in a program but that appear in error messages:
`unit` (the type of an expression that has no value, such as `print(...)`) and *grouped table* (the
result of `group`, which must be passed on to `agg`).

## Type aliases

`type Name = type` gives a type a name so that you can reuse it. You can declare an alias only at
the top level of a file.

```biggo
type Sale = { date: date, region: string, product: string, qty: int, price: float? }
type Sales = table<Sale>
type Score = float?

fn top(sales: Sales, n: int) -> Sales {
  sales |> sort(desc(qty)) |> take(n)
}

let sales = read_csv<Sale>("data/sales.csv")
print(top(sales, 2))
```

```text output
+------------+--------+---------+-----+-------+
| date       | region | product | qty | price |
+------------+--------+---------+-----+-------+
| 2026-03-01 | south  | widget  | 12  | 2.5   |
| 2026-01-05 | north  | widget  | 10  | 2.5   |
+------------+--------+---------+-----+-------+
```

An alias is only a name: `Sale` and `{ date: date, ... }` written out in full are the same type.
A record type that is used as a row type (in `read_csv<...>` or `table<...>`) must have only fields
that can be columns.

## Numbers: int, float, decimal

```biggo
print(9223372036854775807, -9223372036854775807 - 1)   // the limits of int
print(0.1 + 0.2, 1e308 * 10, 0 / 0)                    // float: has inf and NaN
print(0.1d + 0.2d, 1d / 3, 2d / 3)                     // decimal: exact, rounded at the 6th place
```

```text output
9223372036854775807 -9223372036854775808
0.30000000000000004 inf NaN
0.3 0.333333 0.666667
```

| | `int` | `float` | `decimal` |
| --- | --- | --- | --- |
| Stored as | 64-bit integer | 64-bit IEEE 754 | 128-bit integer × 10⁻⁶ |
| Precision | exact | about 15–17 digits | exact to 6 places after the point |
| Out of range | error `integer overflow` | `inf` | error `decimal overflow` |
| Division by zero | `/` gives `inf`, `%` is an error | `inf` or `NaN` | error `division by zero` |
| Good for | counts, ids | measurements, statistics | money |

Use `decimal` when totals must be exact down to the last cent. Use `float` when speed matters
more and an error on the order of 10⁻¹⁵ is acceptable. `float` is faster because the CPU computes
it directly.

A `decimal` literal can have at most 6 places after the point and 32 digits before it. Results of
`*` and `/` that have more places than that are rounded half away from zero at the 6th place.

## Dates and times: date, datetime, duration

```biggo
let start = @2026-01-31T22:30:00
let stop = @2026-02-01T01:15:30.5
let took = stop - start                        // datetime - datetime = duration
print(took, total_seconds(took))
print(start + hours(2), start - days(31))      // datetime ± duration = datetime
print(days(1) + hours(12), hours(1) == minutes(60), took > hours(2))
print(year(start), month(start), day(start), hour(start), minute(start), second(stop))

// a date can stand in for a datetime: it counts as 00:00 on that day
print(@2026-03-01 - @2026-02-01, @2026-02-01 + days(1), start > @2026-01-31)
print(to_date(start), to_datetime(@2026-05-05))
```

```text output
02:45:30.5 9930.5
2026-02-01T00:30:00 2025-12-31T22:30:00
1d 12:00:00 true true
2026 1 31 22 30 30
28d 00:00:00 2026-02-02T00:00:00 true
2026-01-31 2026-05-05T00:00:00
```

- `date` is a calendar day, from year 0000 to 9999.
- `datetime` is a date and time with no time zone ("the time you see on the clock"), with
  microsecond precision.
- `duration` is a span of time. It can be negative. It prints as `HH:MM:SS`, prefixed with `Nd`
  when it is longer than one day.

The arithmetic that is allowed:

| Expression | Result |
| --- | --- |
| `datetime - datetime`, `date - date` | `duration` |
| `datetime + duration`, `datetime - duration` | `datetime` |
| `date + duration`, `date - duration` | `datetime` |
| `duration + duration`, `duration - duration` | `duration` |
| comparing `date`/`datetime` with each other, `duration` with each other | `bool` |

You cannot multiply or divide a `duration` by a number directly. Go through the number of seconds:
`seconds(total_seconds(d) * 2)`. Create a duration with `days(n)` `hours(n)` `minutes(n)`
`seconds(n)`, which take an `int` or a `float`. There are no month or year units because their
length is not fixed.

## Nullable: `T?`

`T?` means "a value of type `T`, or null". Every type that is data can have a `?` (functions and
tables cannot). How to use it is described in [The language](02-language.md#null). The type rules
in brief:

- A value of type `T` can always be used where a `T?` is expected. The reverse is not allowed: you
  must unwrap it with `??` first.
- Computing with a `T?` gives a type that has a `?`.
- `a ?? b` has the type of `b` (if `b` has no `?`, the result has no `?` either).
- `is_null(x)` always gives a `bool` with no `?`.
- A condition of `if` or `where` that is a `bool?`: `if` does not accept it, while `where` counts
  null as "does not pass".

```biggo
let maybe: int? = 5
let sure: int = maybe ?? 0
let doubled = maybe * 2          // int?
print(sure, doubled, is_null(doubled))
```

```text output
5 10 false
```

## list, map, record

```biggo
let numbers: list<int> = [1, 2, 3]
let scores: map<string, float> = { "ann": 9.5, "bo": 7 }
let user: { name: string, tags: list<string> } = { name: "ann", tags: ["a"] }
let nested = [{ id: 1, at: [@2026-01-01] }]
print(numbers, scores, user, nested)
```

```text output
[1, 2, 3] {"ann": 9.5, "bo": 7.0} {name: "ann", tags: ["a"]} [{id: 1, at: [@2026-01-01]}]
```

- The elements of a list, the values of a map, and the fields of a record can be data of any type,
  nested without limit, but they cannot hold functions or tables.
- A map key is a `string`, `int`, `bool`, or `date`.
- Two record types are the same type when their fields have the same names, the same order, and
  the same types.
- `[]` (the empty list) and `{}` as an empty map have no type of their own. The context has to
  supply one, for example `let xs: list<int> = []`.

## Table types

The type of a table is its schema: the names, types, and order of its columns.
Every table operation computes the type of its result at compile time.

```biggo
type Sale = { date: date, region: string, product: string, qty: int, price: float? }
let sales = read_csv<Sale>("data/sales.csv")

fn units(t: table<{ qty: int }>) -> int {
  to_rows(t |> agg(total = sum(qty)))[0].total
}

// pass a table whose columns match what the function declares
print(units(sales |> select(qty)))
```

```text output
45
```

A table that you pass to a function must have columns that match the parameter type *exactly*
(names, types, order), not just "at least these". So you often need to `select` before passing it:

```biggo error
type Sale = { date: date, region: string, product: string, qty: int, price: float? }
fn units(t: table<{ qty: int }>) -> int { count(t) }
print(units(read_csv<Sale>("data/sales.csv")))
```

```text output
error: `units` expects table<{qty: int}> for `t`, found table<{date: date, region: string, product: string, qty: int, price: float?}>
 --> example.bgo:3:13
  |
3 | print(units(read_csv<Sale>("data/sales.csv")))
  |             ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
```

Place the cursor on a variable in your editor to see the type of the table at that point in the
pipeline (see [Tools](07-tools.md#language-server-and-editors)).

## Function types

`fn(int, string) -> bool` is a function that takes an `int` and a `string` and gives a `bool`.
Without `->`, it is a function that has no value. Two functions have the same type when their
parameter types and result type match. Parameter names do not matter.

```biggo
let ops: map<string, int> = { "double": 2, "triple": 3 }
fn scale_by(factor: int) -> fn(int) -> int { fn(n) { n * factor } }
fn apply_all(n: int, f: fn(int) -> int, g: fn(int) -> int) -> int { g(f(n)) }
print(apply_all(5, scale_by(ops["double"] ?? 1), scale_by(ops["triple"] ?? 1)))
```

```text output
30
```

## Automatic conversions

The compiler converts values for you only when **no information is lost**. There are 4 directions:

| From | To | Example |
| --- | --- | --- |
| `int` | `float` | `1 + 2.5` → `3.5` |
| `int` | `decimal` | `1 + 2.5d` → `3.5d` |
| `decimal` | `float` | `1.5 + 2.5d` → `4.0` |
| `date` | `datetime` | `@2026-01-01 < @2026-01-01T12:00:00` |

And a value of type `T` can always be used as a `T?`. Conversion happens everywhere two types have
to agree: operators, function arguments, a `let` with a type, the branches of an `if`, the arms of
a `match`, the elements of a list.

```biggo
let x: float = 1                  // int -> float
let y: decimal = 2                // int -> decimal
let z: float = 2.5d               // decimal -> float
let w: datetime = @2026-01-01     // date -> datetime
print(x, y, z, w)
print([1, 2.5d], [1.5, 2.5d], if true { 1 } else { null })
print(1 + 2.5d, 1.5 + 2.5d, 2.5d / 2, 5 / 2, 7d % 2)
```

```text output
1.0 2 2.5 2026-01-01T00:00:00
[1d, 2.5d] [1.5, 2.5] 1
3.5 4.0 1.25 2.5 1
```

For the directions that can lose information (`float` → `int`, `string` → number, and so on), you
have to call a conversion function yourself:

```biggo error
let count: int = 2.5
```

```text output
error: expected int, found float
 --> example.bgo:1:18
  |
1 | let count: int = 2.5
  |                  ^^^
```

### Lists, records, and maps written out directly

A value that is *written out in place* (a literal) is converted piece by piece to fit the expected
type. A value that is already in a variable has a fixed type. It can be converted only in the way
that does not touch the data inside, which is adding `?`.

```biggo
type Point = { x: float, y: float }
let a: Point = { x: 1, y: 2 }             // literal: int becomes float, field by field
let ints = [1, 2, 3]
let optional: list<int?> = ints            // variable: adding ? is allowed
print(a, optional)
```

```text output
{x: 1.0, y: 2.0} [1, 2, 3]
```

```biggo error
let ints = [1, 2, 3]
let floats: list<float> = ints             // variable: int -> float for a whole list is not allowed
```

```text output
error: expected list<float>, found list<int>
 --> example.bgo:2:27
  |
2 | let floats: list<float> = ints             // variable: int -> float for a whole list is not allowed
  |                           ^^^^
```

The way to convert a whole list is `map(ints, fn(n) { to_float(n) })`.

## Conversion functions

```biggo
print(to_int(3.9), to_int(-3.9), to_int("42"), to_int(true), to_int(19.99d))
print(to_float("1.5"), to_float(2), to_float(2.50d))
print(to_decimal(0.1), to_decimal("2.50"), to_decimal(7))
print(to_string(1.5), to_string(@2026-01-02), to_string(true), to_string(1.50d))
print(to_date("2026-03-04"), to_date(@2026-03-04T23:59:59))
print(to_datetime("2026-03-04T05:06:07"), to_datetime("2026-03-04 05:06:07.25"), to_datetime(@2026-03-04))
```

```text output
3 -3 42 1 19
1.5 2.0 2.5
0.1 2.5 7
1.5 2026-01-02 true 1.5
2026-03-04 2026-03-04
2026-03-04T05:06:07 2026-03-04T05:06:07.25 2026-03-04T00:00:00
```

| Function | Accepts | Notes |
| --- | --- | --- |
| `to_int(x)` | `float` `decimal` `string` `bool` `int` | drops the fraction (toward zero), does not round; `true` becomes `1` |
| `to_float(x)` | `int` `decimal` `string` `float` | |
| `to_decimal(x)` | `int` `float` `string` `decimal` | more than 6 places is rounded |
| `to_string(x)` | every type that can be a column | the same text that `print` shows |
| `to_date(x)` | `string` `datetime` `date` | a string must be `YYYY-MM-DD`; a datetime drops its time |
| `to_datetime(x)` | `string` `date` `datetime` | a string is `YYYY-MM-DDTHH:MM:SS`, or with a space in place of `T`, and can have fractional seconds |

If a string cannot be converted, the program stops with an error that names the value (you do not
silently get null):

```biggo error
print(to_int("12 baht"))
```

```text output
error: cannot convert '12 baht' to an int
 --> example.bgo:1:7
  |
1 | print(to_int("12 baht"))
  |       ^^^^^^^^^^^^^^^^^
```

Every function passes null through: `to_int(x)` gives null when `x` is null. These functions work
on a whole column in the same way, for example `derive(amount = to_decimal(amount_text))`.

## Type inference

The compiler reads the program from top to bottom and knows the type of every expression from its
parts:

| What you write | Resulting type |
| --- | --- |
| `let x = expr` | the type of `expr` |
| `fn f(a: T) { body }` | the result is the type of the last expression in `body` |
| `[a, b, c]` | a `list` of the type that every element can convert to |
| `if c { a } else { b }`, the arms of a `match` | the type that every branch can convert to |
| `fn(x) { ... }` as an argument | the type of `x` comes from the parameter that receives the lambda |
| `t \|> derive(c = expr)` | the original table plus a column `c` with the type of `expr` |

Cases where the compiler asks you to state the type yourself:

- A bare `null`: with `let x = null`, it does not know what the null is a null of → `let x: int? = null`
- An empty list or an empty map: `let xs: list<int> = []`
- A recursive function: you must write `-> T`
- A lambda that is not in a context that gives the type of its parameter

## Comparison and equality

`==` and `!=` work on numbers, `string`, `bool`, `date`, `datetime`, and `duration`. `<` `<=` `>`
`>=` work on all of those types except `bool`. Both sides must be the same type or convertible to
each other according to the table above (`1 == 1.0` gives `true`; `1 == "1"` is a compile error).

Lists, records, and maps cannot be compared with `==`. In tests, use `assert_eq(a, b)`, which
compares the whole structure deeply, including two tables. See [Tools](07-tools.md#biggo-test).

Strings sort by UTF-8 byte order (uppercase letters come before lowercase letters; Thai text sorts
by character code, not in dictionary order).
