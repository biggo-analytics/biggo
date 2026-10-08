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
| `trunc(x)`, `trunc(x, digits)` | number, `int` | like `round` | Cuts the digits off without rounding |
| `sign(x)` | number | same type | -1, 0 or 1 |
| `div(a, b)` | `int`, `int` | `int` | Whole-number division, dropping the remainder. `div(a, 0)` is an error |
| `pow(x, y)` | numbers | `float` | `x` to the power `y` |
| `exp(x)` | number | `float` | `e` to the power `x` |
| `ln(x)`, `log10(x)`, `log2(x)` | number | `float` | Logarithm to the base `e`, 10 and 2 |
| `log(x, base)` | numbers | `float` | Logarithm to any base |
| `greatest(a, b, ...)`, `least(a, b, ...)` | two or more values of one type | that type | The largest and the smallest of the values in one row. They also take strings, dates and durations |
| `clamp(x, low, high)` | values of one type | that type | `x` brought into the range: `least(greatest(x, low), high)` |
| `between(x, low, high)` | values of one type | `bool` | `x >= low and x <= high` |
| `is_nan(x)`, `is_finite(x)` | number | `bool` | Whether a float is "not a number", and whether it is neither that nor infinite |
| `pi()` | | `float` | 3.141592653589793 |
| `sin(x)` `cos(x)` `tan(x)` | number | `float` | Trigonometric functions of an angle in radians |
| `asin(x)` `acos(x)` `atan(x)` | number | `float` | Their inverses, which give radians |
| `atan2(y, x)` | numbers | `float` | The angle of the point (`x`, `y`), in radians |
| `degrees(x)`, `radians(x)` | number | `float` | Radians as degrees, and degrees as radians |

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

The `digits` argument of `round` and of `trunc` must be a program value, not a column.

```biggo
print(pow(2, 10), exp(0), ln(1), log10(1000), log(81, 3), sign(-5), sign(2.5))
print(div(7, 2), 7 % 2, div(-7, 2), -7 % 2, trunc(2.789), trunc(-2.789, 1), round(-2.789, 1))
print(greatest(1, 5, 3), least(1, 2.5), least("b", "a"), clamp(15, 0, 10), between(5, 1, 10))
print(is_nan(0.0 / 0.0), is_finite(1.0 / 0.0), round(pi(), 5), round(sin(pi() / 2), 3), round(degrees(pi()), 1))
```

```text output
1024.0 1.0 0.0 3.0 4.0 -1 1.0
3 1 -3 -1 2.0 -2.7 -2.8
5 1.0 a 10 true
true false 3.14159 1.0 180.0
```

- `div` and `%` belong together: `div(a, b) * b + a % b` is `a`. Both round toward zero, so
  `div(-7, 2)` is `-3` and `-7 % 2` is `-1`.
- The logarithm of zero is `-inf`, and of a negative number `NaN`, as `sqrt(-1)` is. No math
  function stops the program for a value outside its domain.
- `greatest` and `least` give null as soon as one value is null. Use `??` on a value that may be
  null: `greatest(a ?? 0, b ?? 0)`.
- `max` and `min` work down a column, in `agg`; `greatest` and `least` work across the values of
  one row.

### string

| Function | Returns | Meaning |
| --- | --- | --- |
| `length(s)` | `int` | Number of characters (Unicode code points) |
| `lower(s)` `upper(s)` | `string` | Lowercase / uppercase |
| `trim(s)` | `string` | Removes whitespace at both ends |
| `contains(s, part)` | `bool` | `s` has `part` inside it |
| `starts_with(s, part)` | `bool` | `s` starts with `part` |
| `ends_with(s, part)` | `bool` | `s` ends with `part` |
| `index_of(s, part)` | `int?` | Position of the first `part` in `s`, counted from 0; null if `s` does not have it |
| `substring(s, start)`, `substring(s, start, length)` | `string` | The part of `s` that starts at position `start` and runs to the end, or for `length` characters |
| `replace(s, from, to)` | `string` | `s` with every `from` replaced by `to` |
| `split_part(s, separator, n)` | `string?` | Piece number `n` of `s` when it is cut at every `separator`, counted from 0; null if there are not that many pieces |
| `pad_left(s, width)`, `pad_left(s, width, fill)` | `string` | `s` made `width` characters long by putting `fill` in front of it (a space, unless `fill` is given) |
| `pad_right(s, width)`, `pad_right(s, width, fill)` | `string` | The same, with `fill` put after it |
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
and case-sensitive.

```biggo
print(substring("2026-01-05", 0, 4), substring("2026-01-05", 5), substring("2026-01-05", -2))
print(replace("a-b-c", "-", "/"), split_part("a-b-c", "-", 1), split_part("a-b-c", "-", -1))
print(pad_left("7", 3, "0"), pad_right("ab", 5) + "|", index_of("hello", "l"), index_of("hello", "z"))
```

```text output
2026 01-05 05
a/b/c b c
007 ab   | 2 null
```

- Positions count characters from 0. A negative `start` or `n` counts from the end: `-1` is the
  last character, or the last piece.
- A position outside the string is not an error. `substring` gives what there is, so
  `substring("abc", 1, 99)` is `"bc"` and `substring("abc", 5)` is `""`; `split_part` gives null.
- An empty `from` or `separator` matches nothing: `replace` gives `s` back, and `s` is its own
  only piece.
- Padding never cuts: a string that is already `width` characters long comes back as it is. A
  `fill` of several characters is repeated and cut to fit, and `width` can be at most 1,000,000.
- `split(s, separator)` gives all the pieces at once, as a list. It is under
  [Lists and maps](#lists-and-maps), because a column cannot hold a list.

### Regular expressions

| Function | Returns | Meaning |
| --- | --- | --- |
| `regex_match(s, pattern)` | `bool` | Whether `pattern` matches anywhere in `s` |
| `regex_extract(s, pattern)`, `regex_extract(s, pattern, group)` | `string?` | The first match of `pattern` in `s`, or the part of it that group number `group` matched; null if there is no match |
| `regex_replace(s, pattern, to)` | `string` | `s` with every match of `pattern` replaced by `to` |

```biggo
print(regex_match("order-123", "[0-9]+"), regex_match("order-123", "^[0-9]+$"))
print(regex_extract("order-123", "[0-9]+"), regex_extract("order-123", "([a-z]+)-([0-9]+)", 1))
print(regex_replace("2026-01-05", "(\\d+)-(\\d+)-(\\d+)", "$3/$2/$1"))
```

```text output
true false
123 order
05/01/2026
```

- A pattern matches anywhere in the string. Put `^` and `$` around it to match the whole string.
- Patterns have the [syntax of the Rust `regex` library](https://docs.rs/regex/latest/regex/#syntax):
  character classes, repetition, groups, alternation, anchors and Unicode classes. There is no
  look-around and there are no backreferences, which is what lets every pattern run in time
  proportional to the length of the text.
- A backslash is written twice in a string: the pattern `\d+` is the string `"\\d+"`.
- Groups are numbered from 1 by their opening parenthesis, and group 0 is the whole match. A group
  that took no part in the match gives null.
- In `to`, `$1` stands for what group 1 matched (write `${1}` when a letter or digit follows),
  `$name` for a group written `(?P<name>...)`, and `$$` for a dollar sign.
- The pattern and the group cannot depend on a column: the pattern is compiled once for the whole
  query. They can be any other value of the program.
- A pattern that is written out as a string is checked before the program runs:

```biggo error
print(regex_extract("order-123", "([a-z]+-", 1))
```

```text output
error: `([a-z]+-` is not a regular expression: unclosed group
 --> example.bgo:1:34
  |
1 | print(regex_extract("order-123", "([a-z]+-", 1))
  |                                  ^^^^^^^^^^
```

### Dates and times

| Function | Takes | Returns | Meaning |
| --- | --- | --- | --- |
| `year(d)` `month(d)` `day(d)` | moment | `int` | Year, month (1–12), day of the month |
| `hour(t)` `minute(t)` `second(t)` | moment | `int` | Hour (0–23), minute, second; for a `date` these are 0 |
| `days(n)` `hours(n)` `minutes(n)` `seconds(n)` | `int` or `float` | `duration` | A span of time `n` units long |
| `total_seconds(d)`, `total_minutes(d)`, `total_hours(d)`, `total_days(d)` | `duration` | `float` | Length in that unit |
| `today()`, `now()` | | `date`, `datetime` | The day and the moment the program started |
| `weekday(d)` | moment | `int` | 1 for Monday to 7 for Sunday |
| `week(d)` | moment | `int` | The ISO week number: weeks start on Monday, and week 1 holds the year's first Thursday |
| `quarter(d)`, `day_of_year(d)` | moment | `int` | The quarter (1–4), and the day of the year (1–366) |
| `month_name(d)`, `day_name(d)` | moment | `string` | `"January"`, `"Monday"` |
| `is_weekend(d)` | moment | `bool` | Saturday or Sunday |
| `start_of_week(d)`, `start_of_month(d)`, `start_of_quarter(d)`, `start_of_year(d)` | moment | `date` | The first day of the period that `d` is in. A week starts on Monday |
| `end_of_month(d)` | moment | `date` | The last day of the month |
| `add_days(d, n)`, `add_months(d, n)`, `add_years(d, n)` | moment, `int` | type of `d` | `n` days, months or years later, or earlier when `n` is negative |
| `days_between(a, b)`, `months_between(a, b)`, `years_between(a, b)` | two moments | `int` | The whole days, months or years from `a` to `b` |
| `make_date(year, month, day)` | `int`s | `date` | A date from its parts |
| `make_datetime(year, month, day, hour, minute, second)` | `int`s | `datetime` | A datetime from its parts |
| `format_date(d, pattern)` | moment, `string` | `string` | A date or a datetime as text |
| `parse_date(s, pattern)`, `parse_datetime(s, pattern)` | `string`s | `date`, `datetime` | Text in a pattern as a date or a datetime |
| `try_parse_date(s, pattern)`, `try_parse_datetime(s, pattern)` | `string`s | `date?`, `datetime?` | The same, with null for text that does not fit |
| `time_bucket(t, size)` | moment, `duration` | `datetime` | The start of the interval of length `size` that `t` falls in |
| `to_unix(t)` | moment | `int` | Seconds since 1970-01-01T00:00:00 |
| `from_unix(n)` | `int` or `float` | `datetime` | The moment `n` seconds after 1970-01-01T00:00:00 |
| `fiscal_year(d, first_month)` | moment, `int` | `int` | The fiscal year that `d` belongs to |
| `buddhist_year(d)` | moment | `int` | The year of the Buddhist era, 543 more than `year(d)` |

A *moment* is a `date` or a `datetime`.

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

Parts of the calendar, and periods:

```biggo
let day = @2026-10-09
print(weekday(day), day_name(day), week(day), quarter(day), day_of_year(day), is_weekend(day))
print(start_of_week(day), start_of_month(day), start_of_quarter(day), start_of_year(day), end_of_month(day))
print(add_days(day, 30), add_months(@2026-01-31, 1), add_years(@2024-02-29, 1), add_months(@2026-01-31T09:30, 1))
print(days_between(@2026-01-01, day), months_between(@2026-01-15, day), years_between(@1990-05-20, day))
print(make_date(2026, 2, 28), time_bucket(@2026-10-09T14:44:59, minutes(15)), fiscal_year(day, 10))
```

```text output
5 Friday 41 4 282 false
2026-10-05 2026-10-01 2026-10-01 2026-01-01 2026-10-31
2026-11-08 2026-02-28 2025-02-28 2026-02-28T09:30:00
281 8 36
2026-02-28 2026-10-09T14:30:00 2027
```

- `add_days`, `add_months` and `add_years` give a `date` for a `date`, and for a `datetime` a
  `datetime` at the same time of day. (`d + days(1)` gives a `datetime` even for a `date`.)
- A day that the new month does not have becomes that month's last day: one month after 31
  January is 28 or 29 February, and one year after 29 February is 28 February.
- The `_between` functions count whole units, so they round toward zero:
  `months_between(@2026-01-15, @2026-02-14)` is 0. They are negative when `b` is before `a`.
  `years_between(born, today())` is an age.
- `time_bucket` counts intervals from 1970-01-01T00:00:00. Use it to group readings by the hour
  or by the quarter of an hour: `group(slot = time_bucket(at, minutes(15)))`.
- `fiscal_year(d, first_month)` names a fiscal year for the calendar year it ends in. With years
  that start in October, `fiscal_year(@2026-10-09, 10)` is 2027.
- `make_date` and `make_datetime` stop the program for parts that are no date, such as day 30 of
  month 2. No date is earlier than the year 0 or later than the year 9999.
- `today()` and `now()` read the clock of the machine, in its own time zone, the first time the
  program asks, and give that same moment for the rest of the run. Set the environment variable
  `BIGGO_NOW` to run a program as of another moment: `BIGGO_NOW=2026-01-31T18:30:00 biggo run report.bgo`.

Dates as text, in other forms than `2026-10-09`:

```biggo
let day = @2026-10-09
print(format_date(day, "%d/%m/%Y"), format_date(day, "%A %-d %B %Y"), format_date(@2026-10-09T14:05, "%H:%M on %d %b"))
print(parse_date("09/10/2026", "%d/%m/%Y"), parse_datetime("20261009 1405", "%Y%m%d %H%M"))
print(try_parse_date("31/02/2026", "%d/%m/%Y"), try_parse_date("9 Oct 2026", "%d %b %Y"))

// Thai dates count years in the Buddhist era.
print(format_date(day, "%d/%m/%Y", era = "buddhist"), parse_date("29/02/2567", "%d/%m/%Y", era = "buddhist"))
```

```text output
09/10/2026 Friday 9 October 2026 14:05 on 09 Oct
2026-10-09 2026-10-09T14:05:00
null 2026-10-09
09/10/2569 2024-02-29
```

A pattern is text in which each `%` code stands for a part of the date:

| Code | Part | Code | Part |
| --- | --- | --- | --- |
| `%Y` | year, 4 digits | `%H` | hour, 00–23 |
| `%y` | year, 2 digits | `%I` | hour, 01–12 |
| `%m` | month, 01–12 | `%p` | `AM` or `PM` |
| `%B`, `%b` | month name, full and short | `%M` | minute |
| `%d` | day, 01–31 | `%S` | second |
| `%e` | day, padded with a space | `%f` | fraction of a second, 9 digits |
| `%j` | day of the year | `%A`, `%a` | weekday name, full and short |
| `%F` | the same as `%Y-%m-%d` | `%T` | the same as `%H:%M:%S` |
| `%-d`, `%-m`, `%-H` | the part with no padding | `%%` | a percent sign |

- The codes are those of `strftime`. The full list is in the documentation of the
  [chrono library](https://docs.rs/chrono/latest/chrono/format/strftime/index.html), which biggo
  uses. Codes for a time zone are refused, because a `datetime` has none.
- Names of months and days are in English.
- Reading is lenient about padding: `%d/%m/%Y` reads both `09/10/2026` and `9/10/2026`.
- `parse_date` and `parse_datetime` stop the program for text that does not fit the pattern or
  that is no real date. The `try_` forms give null instead.
- `parse_datetime` with a pattern that has no time of day gives the start of the day.
- `era = "buddhist"` writes and reads years 543 ahead, as Thai dates have them: 2569 for 2026.
  Subtracting 543 after reading would not be enough, because 29 February 2567 is a real day but
  the year 2567 has no 29 February.
- The pattern and the era cannot depend on a column. A pattern that is written out as a string is
  checked before the program runs.

### Conversions

| Function | Takes | Returns |
| --- | --- | --- |
| `to_int(x)` | `int` `float` `decimal` `string` `bool` | `int` (the fraction is dropped) |
| `to_float(x)` | `int` `float` `decimal` `string` | `float` |
| `to_decimal(x)` | `int` `float` `decimal` `string` | `decimal` |
| `to_string(x)` | any | `string` |
| `to_date(x)` | `string` `date` `datetime` | `date` |
| `to_datetime(x)` | `string` `date` `datetime` | `datetime` |

| `to_bool(x)` | `string` `int` `bool` | `bool` |
| `try_to_int(x)`, `try_to_float(x)`, `try_to_decimal(x)`, `try_to_date(x)`, `try_to_datetime(x)`, `try_to_bool(x)` | as the function without `try_` | the same type, nullable |
| `parse_number(s)` | `string` | `float?` |
| `null_if(x, value)` | two values of one type | that type, nullable |

A value that cannot be converted stops the program with an error. The `try_` form of each
conversion gives null for such a value instead, which is what data that people typed needs:

```biggo
let answers = from_rows([{ age: "42", joined: "yes" }, { age: "n/a", joined: "-" }, { age: "17 ", joined: "N" }])
answers
  |> derive(years = try_to_int(age), member = try_to_bool(joined), known = null_if(joined, "-"))
  |> print()
print(parse_number("1,234.50"), parse_number("45%"), parse_number("(120)"), parse_number("twelve"))
```

```text output
+-----+--------+-------+--------+-------+
| age | joined | years | member | known |
+-----+--------+-------+--------+-------+
| 42  | yes    | 42    | true   | yes   |
| n/a | -      | null  | null   | null  |
| 17  | N      | 17    | false  | N     |
+-----+--------+-------+--------+-------+
1234.5 0.45 -120.0 null
```

- `to_bool` reads `true` / `false`, `t` / `f`, `yes` / `no`, `y` / `n` and `1` / `0`, in any letter
  case and with spaces around them, and the ints 1 and 0.
- `parse_number` reads numbers as people write them: with `,` between thousands, with `%` (the
  result is divided by 100), and in parentheses for a negative amount. Anything else gives null.
- `null_if(x, value)` gives null when `x` equals `value` and `x` otherwise. It turns the markers
  that files use for "no value" into null: `null_if(amount, -999)`, `null_if(name, "")`.
- `to_int` and `try_to_int` take digits with an optional sign, and allow spaces around them:
  `"17 "` is 17, but `"1,200"` and `"12 kg"` are not ints. Use `parse_number` when the data has
  separators between thousands.

Details and more examples are in [The type system](03-types.md#conversion-functions).

## Aggregate

Used inside `agg(...)` and `pivot(...)`. Those that take one column, or a condition, also work
inside `window(...)`, where they are computed over each partition. They reduce the values of a
group to a single value, and skip nulls.

| Function | Takes | Returns |
| --- | --- | --- |
| `count()` | — | `int` number of rows |
| `count(x)` | any | `int` number of non-null values |
| `count_distinct(x)` | any | `int` number of distinct values |
| `sum(x)` | number, `duration` | same type |
| `mean(x)` | number | `float` |
| `median(x)` | number | `float` |
| `stddev(x)`, `variance(x)` | number | `float?` standard deviation and variance of a sample; null if there are fewer than 2 values |
| `stddev_pop(x)`, `variance_pop(x)` | number | `float?` the same of a whole population, which divide by the number of values instead of one less |
| `quantile(x, share)` | number, `float` | `float` the value a share of the way from the smallest to the largest: `quantile(x, 0.5)` is the median |
| `product(x)` | number | `float` all the values multiplied |
| `count_if(condition)` | `bool` | `int` number of rows where the condition is true |
| `any(condition)`, `all(condition)` | `bool` | `bool` whether the condition is true in some row, and in every row |
| `count_null(x)` | any | `int` number of nulls |
| `string_agg(x, separator)` | `string`, `string` | `string` the values joined, in the order of the rows, with the separator between them |
| `arg_max(x, by)`, `arg_min(x, by)` | any, any except `bool` | type of `x`: the value of `x` in the row where `by` is largest or smallest |
| `weighted_mean(x, weight)` | number, number | `float` the mean in which each value counts as much as its weight |
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

```biggo
type Sale = { date: date, region: string, product: string, qty: int, price: float? }
read_csv<Sale>("data/sales.csv")
  |> group(region)
  |> agg(
    big = count_if(qty > 3),
    every = all(qty > 0),
    p90 = quantile(qty, 0.9),
    products = string_agg(product, ", "),
    best = arg_max(product, qty),
    paid = round(weighted_mean(price, qty), 2),
  )
  |> sort(region)
  |> print()
```

```text output
+--------+-----+-------+--------------------+-------------------------------+--------+------+
| region | big | every | p90                | products                      | best   | paid |
+--------+-----+-------+--------------------+-------------------------------+--------+------+
| east   | 1   | true  | 3.6                | gizmo, widget, gadget         | widget | 31.4 |
| north  | 2   | true  | 9.100000000000001  | widget, gadget, widget, gizmo | widget | 8.21 |
| south  | 2   | false | 10.600000000000001 | widget, gadget, widget        | widget | 2.5  |
+--------+-----+-------+--------------------+-------------------------------+--------+------+
```

- `quantile` places the share between the two nearest values, in proportion: of the values 1, 2,
  3 and 10, `quantile(x, 0.9)` is 7.9. The share is a value of the program from 0 to 1, not a
  column.
- `count_if`, `any` and `all` take any condition over the columns. A row where the condition is
  null is not counted, does not make `any` true, and does not make `all` false.
- `any` and `all` are null for a group with no row where the condition is known.
- `string_agg` skips nulls. Its separator is a value of the program, not a column.
- `arg_max` and `arg_min` take the first such row when several have the same largest or smallest
  value, and skip the rows where `by` is null: `arg_max(product, qty)` is the best-selling
  product, and `arg_min(name, date)` the name on the earliest date.
- `weighted_mean(x, weight)` is `sum(x * weight) / sum(weight)` over the rows where neither is
  null.

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
| `read_csv<T>(path)`, `read_csv<T>(path, delimiter = d, encoding = e)` | A table from a CSV file, which can have another delimiter than `,` and another encoding than UTF-8 |
| `read_parquet<T>(path)` | A table from a Parquet file |
| `read_json<T>(path)` | A table from a JSON Lines file |
| `read_sql<T>(path, query)` | The result of a query on a SQLite database |
| `write_csv(t, path)`, `write_csv(t, path, delimiter = d, encoding = e)` | Writes a table as CSV |
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
| `split(s, separator)` | `list<string>` | The pieces of the string `s` between its separators; an empty separator splits nothing |
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

## Program arguments

| Function | Returns | Meaning |
| --- | --- | --- |
| `args()` | `list<string>` | The arguments that follow the program on the command line: `biggo run report.bgo 2026-01 north` gives `["2026-01", "north"]`. The list is empty when there are none |

Details: [biggo run](07-tools.md#biggo-run)

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
