# The language

This page explains the syntax and semantics of the parts of the biggo language that do not involve
tables: values, variables, operators, `if`, `match`, functions, lambdas, lists, records, maps and
`import`. Tables are covered in [Working with tables](04-tables.md), and the details of types are in
[The type system](03-types.md).

Every example in the documentation is actually run during `cargo test`. The output you see is the
real output.

## Program structure

A program is a sequence of *statements*, of which there are 5 kinds:

| Statement | Example |
| --- | --- |
| `import` | `import "lib/shapes.bgo"` |
| `type` | `type Sale = { region: string, qty: int }` |
| `let` | `let total = 10` |
| `fn` | `fn double(n: int) -> int { n * 2 }` |
| expression | `print(total)` |

A program runs from top to bottom. There is no `main` function.

### Where a statement ends

A statement ends at the end of the line. There is no `;`. A statement continues onto the next line
when it is clearly not finished, that is, when:

- it is inside `(...)` or `[...]` brackets that are not closed yet
- the line ends with an operator, such as `1 +`
- the next line starts with an operator, such as `|>` `+` `and` `??` `.`

```biggo
let total = 1 +
  2
let longer = [1, 2, 3]
  |> map(fn(n) { n * total })
  |> fold(0, fn(sum, n) { sum + n })
print(total, longer)
```

```text output
3 18
```

The exceptions are `-`, `(` and `[`: at the start of a line they always begin a new statement,
because each of the three can start an expression by itself (`-x`, `(a + b)`, `[1, 2]`).

### comment

`//` to the end of the line is a comment. There are no multi-line comments.

## Basic values

```biggo
print(42, 1_000_000, 3.14, 2.5e3)
print(19.99d)
print("hello", "tab\there", "quote: \"x\"")
print(true, false, null)
print(@2026-01-31, @2026-01-31T18:30:00)
```

```text output
42 1000000 3.14 2500.0
19.99
hello tab	here quote: "x"
true false null
2026-01-31 2026-01-31T18:30:00
```

| Type | Literal | Notes |
| --- | --- | --- |
| `int` | `42`, `1_000_000` | 64-bit integer. `_` can separate digits |
| `float` | `3.14`, `2.5e3` | 64-bit floating point (IEEE 754) |
| `decimal` | `19.99d`, `5d` | Exact decimal with 6 decimal places. Ends with `d` |
| `string` | `"text"`, `r"\d+"` | UTF-8. Supported escapes: `\n` `\t` `\r` `\0` `\\` `\"`. In a raw string, written with `r` in front, a backslash is only a backslash, and the string ends at the first `"` |
| `bool` | `true`, `false` | |
| `date` | `@2026-01-31` | A calendar date. It must be a date that exists |
| `datetime` | `@2026-01-31T18:30:00` | Seconds are optional. Up to 6 digits of fractional seconds. No time zone |
| null | `null` | "No value". Allowed only with a type that ends in `?` |

A string cannot span lines. If you need a newline, use `\n`.

`duration` (a span of time) has no literal. You create one with the functions `days(n)`, `hours(n)`,
`minutes(n)`, `seconds(n)`, or by subtracting two datetime values. See
[The type system](03-types.md#dates-and-times-date-datetime-duration).

## Variables

`let` binds a name to a value. The value of a variable cannot change (there is no reassignment), but
you can declare the same name again, which creates a new variable that shadows the old one.

```biggo
let price = 100
let price = price * 1.07      // a new variable, computed from the old one
let label: string = "total"   // you can state the type
let nothing: int? = null      // null always needs a type
print(label, price, nothing)
```

```text output
total 107.0 null
```

Usually you do not need to write the type, because the compiler infers it from the value. You must
write it when the value does not determine the type fully, such as `null` or the empty list `[]`.

## Operators

Listed from the tightest binding to the loosest:

| Level | Operator | Meaning | Associativity |
| --- | --- | --- | --- |
| 1 | `f(x)` `x.name` `x[i]` | function call, field, index | left to right |
| 2 | `-x` | negative value | |
| 3 | `*` `/` `%` | multiply, divide, remainder | left to right |
| 4 | `+` `-` | add, subtract | left to right |
| 5 | `??` | if the left side is null, use the right side | right to left |
| 6 | `==` `!=` `<` `<=` `>` `>=` `in` `not in` | comparison, membership in a list | cannot be chained |
| 7 | `not` | logical negation | |
| 8 | `and` | logical and | left to right |
| 9 | `or` | logical or | left to right |
| 10 | `\|>` | pass a value into a function | left to right |

```biggo
print(1 + 2 * 3, (1 + 2) * 3, -2 * 3, 10 - 4 - 3)
print(7 / 2, 6 / 3, 7 % 3, -7 % 3, 7.5 % 2)
print(not true or true, 1 < 2 and 2 < 3)
print("data" + "base", [1, 2] + [3])
print(3 in [1, 2, 3], "x" not in ["a", "b"])
```

```text output
7 9 -6 3
3.5 2.0 1 -1 1.5
true true
database [1, 2, 3]
true true
```

Rules worth knowing:

- **`/` always gives a `float`**, even when the division is exact (`6 / 3` is `2.0`). If you want an
  integer, use `to_int(a / b)`. The exception is when the dividend or the divisor is a `decimal`:
  the result is then a `decimal`.
- **An `int` that overflows stops the program**. It does not wrap around silently:
  `9223372036854775807 + 1` is the runtime error `integer overflow`, and `x % 0` is
  `division by zero`. Dividing a `float` by zero gives `inf`.
- **Comparisons cannot be chained**: `1 < x < 10` is a syntax error. Write `1 < x and x < 10`.
- `+` can concatenate strings and concatenate lists.
- You can mix `int` and `float`. The `int` side is converted to `float` automatically. The full
  rules are in [The type system](03-types.md#automatic-conversions).
- `and` / `or` evaluate the right side only when necessary.
- **`x in list`** is true when the list has a value equal to `x`, and `x not in list` is the
  opposite. The list can be written out or be any `list` value of the program. In a table
  operation `x` can be a column, but the list cannot depend on one:
  `where(region in ["north", "east"])`. Like `==`, a null `x` gives null.

## null

A value that "may be missing" has a type that ends in `?`, such as `int?`. The rule for null is the
same as in SQL: **a computation that involves null gives null**.

```biggo
let n: int? = null
print(n + 1, n > 1, upper("a") + (if is_null(n) { "!" } else { "?" }))
print(n ?? 0, is_null(n), not is_null(n))
// and / or can know the answer even when the other side is null
print(false and n > 1, true or n > 1, true and n > 1)
```

```text output
null null A!
0 true false
false true null
```

There are two tools for handling null:

- `a ?? b`: uses `a` if it is not null, otherwise uses `b` (the result is not null if `b` is not
  null)
- `is_null(a)`: always gives `true`/`false`

There are also two things that are not allowed, which the compiler checks for you:

```biggo error
let n: int? = null
if n > 1 { print("big") }
```

```text output
error: this condition can be null; say what null means, for example with `?? false`
 --> example.bgo:2:4
  |
2 | if n > 1 { print("big") }
  |    ^^^^^
```

The condition of an `if` must be a `bool` that is not null. The program has to say for itself what
null means, for example `if n > 1 ?? false { ... }`.

```biggo error
let n: int? = null
print(n == null)
```

```text output
error: comparing with `null` always gives null; use `is_null(...)` to test for null
 --> example.bgo:2:7
  |
2 | print(n == null)
  |       ^^^^^^^^^
```

By the rule above, `x == null` always gives null, which is almost never what you intend, so it is an
error at compile time.

## if

`if` is an expression: it has a value, and that value is the value of the branch that is chosen.

```biggo
let qty = 7
let size = if qty == 0 {
  "none"
} else if qty < 5 {
  "small"
} else {
  "large"
}
print(size, if qty > 5 { 1 } else { 2.5 })
```

```text output
large 1.0
```

Every branch must have a compatible type (`1` and `2.5` combine into `float`; a value and `null`
combine into a type with `?`). An `if` without an `else` has no value. Use it only for side effects
such as `print`.

## match

`match` compares a value against patterns, one arm at a time from top to bottom, and gives the value
of the first arm that matches.

```biggo
fn size(qty: int) -> string {
  match qty {
    0 => "none"
    1 | 2 | 3 => "small"
    -1 => "returned"
    _ => "large"
  }
}
print(map([0, 2, 50, -1], size))
```

```text output
["none", "small", "large", "returned"]
```

- A pattern is a literal (number, string, bool, date, `null`) or `_`, which matches every value.
- `a | b` means "matches `a` or `b`".
- Arms are separated by a newline or `,`.
- **It must cover every value**: there must be a final `_` arm, except for a `bool` that already has
  both `true` and `false`.
- The matched value is computed once, no matter how many arms there are.

A null value does not match any literal. It matches only the patterns `null` and `_`:

```biggo
fn label(score: int?) -> string {
  match score {
    null => "no score"
    100 => "perfect"
    _ => "scored"
  }
}
let missing: int? = null
print(label(missing), label(100), label(55))
print(match true { true => "yes", false => "no" })
```

```text output
no score perfect scored
yes
```

```biggo error
let qty = 3
print(match qty { 1 => "one", 2 => "two" })
```

```text output
error: this `match` does not cover every value; add a `_ => ...` arm
 --> example.bgo:2:13
  |
2 | print(match qty { 1 => "one", 2 => "two" })
  |             ^^^
```

You can also use `match` in column expressions. See
[Working with tables](04-tables.md#column-expressions).

## block

`{ ... }` is a block: a sequence of statements with its own variable scope. The value of a block is
the value of its last expression.

```biggo
let area = {
  let width = 3
  let height = 4
  width * height
}
print(area)
```

```text output
12
```

Variables in a block are not visible from outside. A function body and the branches of an `if` are
blocks too. The language has no `return`: a function gives the value of the last expression in its
body.

## Functions

```biggo
fn area(width: float, height: float) -> float {
  width * height
}

// the result type is optional: the compiler works it out from the function body
fn show(name: string, size: float) {
  print(name, "has area", size)
}

show("room", area(3, 4.5))
// you can pass arguments by name, in any order, but they must come after the positional arguments
print(area(height = 2, width = 10), area(10, height = 2))
```

```text output
room has area 13.5
20.0 20.0
```

- A parameter must always state its type.
- The result type (`-> T`) can be omitted, except that a function that calls itself (recursive) must
  state it.
- A function that has no value (one that ends with `print`, for example) needs no `->`.
- `area(3, 4.5)`: `3` is an `int` but the parameter is a `float`, so it is converted automatically.

### Name visibility

A top-level function can be called from anywhere in the file, even if it is declared further down.
But **a function sees only the variables declared before it in the file**.

```biggo
let rate = 0.07
print(with_tax(100))        // you can call it before it is declared

fn with_tax(amount: float) -> float { amount * (1 + rate) }
```

```text output
107.0
```

If a function is above a variable that it uses, the compiler rejects it:

```biggo error
fn with_tax(amount: float) -> float { amount * (1 + rate) }
let rate = 0.07
```

```text output
error: undefined name `rate`
 --> example.bgo:1:53
  |
1 | fn with_tax(amount: float) -> float { amount * (1 + rate) }
  |                                                     ^^^^
```

This rule prevents almost every mistake. One case is left that has to wait until run time:
*calling* a function before the `let` of a variable it uses has run:

```biggo error
print(with_tax(100))        // rate has no value yet at this point
let rate = 0.07
fn with_tax(amount: float) -> float { amount * (1 + rate) }
```

```text output
error: `rate` is used before it has a value
 --> example.bgo:3:53
  |
3 | fn with_tax(amount: float) -> float { amount * (1 + rate) }
  |                                                     ^^^^
```

A safe approach: declare constants at the top, then the functions, and then the code that calls
them.

### recursion

```biggo
fn factorial(n: int) -> int {
  if n <= 1 { 1 } else { n * factorial(n - 1) }
}
fn fib(n: int) -> int { if n < 2 { n } else { fib(n - 1) + fib(n - 2) } }
print(factorial(20), fib(20))
```

```text output
2432902008176640000 6765
```

Calls can nest 100,000 levels deep. Beyond that the program stops with
`stack overflow: the program recurses too deeply`. The language has no `for`/`while` loops:
repeated work uses `map`, `filter`, `fold`, `each` on a list (see below) or recursion.

### Functions inside functions

You can declare a `fn` inside a block. The inner function sees the variables of the outer function
(a closure).

```biggo
fn total_with_fee(amounts: list<float>, fee: float) -> float {
  fn add_fee(amount: float) -> float { amount + fee }
  fold(map(amounts, add_fee), 0.0, fn(sum, x) { sum + x })
}
print(total_with_fee([10, 20.5], 1))
```

```text output
32.5
```

## lambda and functions as values

A `fn(...) { ... }` without a name is a lambda: a function that is a value. You can store it in a
variable, pass it as an argument, or return it from another function.

```biggo
let double = fn(x: int) -> int { x * 2 }
print(double(21))

// a function type is written fn(parameter types) -> result type
fn twice(f: fn(int) -> int, x: int) -> int { f(f(x)) }

// a lambda passed as an argument needs no types: they come from the parameter that receives it
print(twice(double, 3), twice(fn(n) { n + 1 }, 3))

// a function that returns a function: the lambda remembers the variables around it
fn adder(amount: int) -> fn(int) -> int {
  fn(n) { n + amount }
}
let add5 = adder(5)
print(add5(1), map([1, 2, 3], add5))
```

```text output
42
12 5
6 [6, 7, 8]
```

You can omit the parameter types of a lambda when the context already determines them, that is,
when the lambda:

- is an argument of a function whose parameter has a function type (including `map`, `filter`,
  `fold`, `each`)
- is the value of a `let` that states a type
- is the last value of a function whose declared result type is a function

Anywhere else you must write them yourself:

```biggo error
let inc = fn(x) { x + 1 }
```

```text output
error: cannot tell the type of `x` here; write it, as in `x: int`
 --> example.bgo:1:14
  |
1 | let inc = fn(x) { x + 1 }
  |              ^
```

A function declared with a name can also be used as a value (`map(xs, double)`), but **a built-in
function cannot be used as a value**. Wrap it in a lambda: `map(names, fn(s) { upper(s) })`.

## list

A list is a sequence of values of the same type, written in `[...]`. Its type is `list<T>`.

```biggo
let primes = [2, 3, 5, 7, 11]
print(primes[0], primes[-1], len(primes))      // a negative index counts from the end
print(primes + [13], range(5), range(2, 6))

print(map(primes, fn(p) { p * p }))
print(filter(primes, fn(p) { p % 4 == 3 }))
print(fold(primes, 0, fn(sum, p) { sum + p }))
each(filter(primes, fn(p) { p > 6 }), fn(p) { print("big prime:", p) })

let empty: list<string> = []
print(empty, len(empty), [1, 2.5], [1, null])
```

```text output
2 11 5
[2, 3, 5, 7, 11, 13] [0, 1, 2, 3, 4] [2, 3, 4, 5]
[4, 9, 25, 49, 121]
[3, 7, 11]
28
big prime: 7
big prime: 11
[] 0 [1.0, 2.5] [1, null]
```

| Function | Result |
| --- | --- |
| `len(xs)` | the number of elements |
| `xs[i]` | element number `i`, counting from 0; `-1` is the last one; out of range is a runtime error |
| `xs + ys` | a new list, the two concatenated |
| `range(n)`, `range(a, b)` | `[0, ..., n-1]`, `[a, ..., b-1]` (at most 10 million elements) |
| `map(xs, f)` | the list of `f(x)` |
| `filter(xs, f)` | only the elements for which `f(x)` is `true` |
| `fold(xs, start, f)` | starts from `start`, then `acc = f(acc, x)` one element at a time |
| `each(xs, f)` | calls `f(x)` one element at a time for its side effects; has no value |

The elements of a list must be data: numbers, strings, dates, records, maps or lists, which can be
nested. A list cannot hold functions or tables. A value of any type cannot be changed after it is
created, so functions such as `+` and `map` always create a new list.

`fold` needs a `start` with a definite type, and the function must return the same type as `start`:
to accumulate a `float`, start with `0.0`, not `0`.

## record

A record is a value with named fields. Write it as `{ name: value, ... }` and read a field with `.`.

```biggo
let ann = { name: "Ann", age: 31, tags: ["admin", "dev"] }
print(ann, ann.name, ann.age + 1, ann.tags[0])

type Point = { x: float, y: float }
fn norm(p: Point) -> float { sqrt(p.x * p.x + p.y * p.y) }
print(norm({ x: 3, y: 4 }))

let points: list<Point> = [{ x: 1, y: 2 }, { x: 0.5, y: 0 }]
print(map(points, fn(p) { p.x + p.y }))
```

```text output
{name: "Ann", age: 31, tags: ["admin", "dev"]} Ann 32 admin
5.0
[3.0, 0.5]
```

The type of a record is the list of its fields with their types, in order: `{ x: float, y: float }`.
Two record types are the same type when their fields have the same names, in the same order, with
matching types. A record that is *written out directly* (`{ x: 3, y: 4 }`) is converted field by
field to fit the type that is required, for example `3` becomes `3.0` above.

A table row is also a record: `to_rows(table)` gives a `list` of records, and `from_rows(list)`
creates a table from a list of records. See
[Working with tables](04-tables.md#tables-and-lists-of-records).

## map

A map pairs keys with values. Write it as `{ key: value, ... }`, where each key is a literal. Its
type is `map<K, V>`.

```biggo
let stock = { "tea": 4, "cake": 0 }
print(stock["tea"], stock["milk"], stock["milk"] ?? 0)   // a missing key gives null

let more = put(put(stock, "milk", 9), "tea", 5)          // put returns a new map
print(more, stock)
print(keys(more), values(more), len(more), has_key(more, "milk"))

// an empty map needs a type
let none: map<string, int> = {}
let counts = fold(["a", "b", "a"], none, fn(seen, word) {
  put(seen, word, (seen[word] ?? 0) + 1)
})
print(counts)
```

```text output
4 null 0
{"tea": 5, "cake": 0, "milk": 9} {"tea": 4, "cake": 0}
["tea", "cake", "milk"] [5, 0, 9] 3 true
{"a": 2, "b": 1}
```

- A key is a `string`, `int`, `bool` or `date`; a value is data of any type (the same as the
  elements of a list).
- `m[key]` has the type `V?`: it is null when the key is not there. Use `??` to set a default value.
- A map remembers the order in which its keys were first inserted. `keys`, `values` and printing
  follow that order.
- `put` does not modify the original map. It returns a new map (it has to copy the whole map, so it
  suits small to medium maps).

A `{ ... }` that starts with `name:` is a record, one that starts with `literal:` is a map, and
anything else is a block. So the keys of a map that is written out directly must be literals. If a
key comes from a variable, use `put`.

## pipeline

`a |> f(b, c)` means exactly the same as `f(a, b, c)`: the value on the left is the first argument.
The right side of `|>` must be a function call.

```biggo
fn bounded(x: int, low: int, high: int) -> int {
  if x < low { low } else if x > high { high } else { x }
}
print(15 |> bounded(0, 10))
[3, 1, 2]
  |> map(fn(n) { n * 10 })
  |> filter(fn(n) { n > 10 })
  |> print()
```

```text output
10
[30, 20]
```

`|>` binds the loosest, so the whole expression on the left is the value that is passed:
`1 + 2 |> print()` prints `3`.

## import

`import "path"` brings in the top-level definitions (variables, functions, types) of another file.
The path is relative to the location of the file that contains the `import`.

The file [`lib/geometry.bgo`](lib/geometry.bgo):

```biggo check
type Rect = { width: float, height: float }

fn area(rect: Rect) -> float { rect.width * rect.height }

let unit_square: Rect = { width: 1, height: 1 }
```

A program that uses it:

```biggo
import "lib/geometry.bgo"

let door: Rect = { width: 0.9, height: 2.0 }
print(area(door), area(unit_square))
```

```text output
1.8 1.0
```

- `import` must be at the top of the file, before any other statement.
- An imported file is *run* once (its top-level statements really execute, including `print`),
  before the file that imports it. It runs only once, no matter how many places import it.
- All names live in one place (there are no namespaces): names from an imported file, including
  names from the files that it imports in turn, can be used directly. A name that is declared again
  later shadows the earlier one.
- The paths of data files in an imported file are relative to the location of that file itself.
- A circular import (A imports B, B imports A) is an error.
- `biggo build` also bundles the imported files into the executable.

## Names

The names of variables, functions, types and columns start with a letter or `_`, followed by
letters, digits or `_`. Letters are Unicode letters, so you can use Thai, as the example below does.

```biggo
let ยอดขาย = [120, 80]
let ภาษี = fn(ยอด: int) -> float { ยอด * 0.07 }
print(map(ยอดขาย, ภาษี))

// a name that contains spaces or clashes with a keyword goes inside backticks
let `unit price` = 2.5
print(`unit price` * 4)
```

```text output
[8.4, 5.6000000000000005]
10.0
```

Keywords: `let` `fn` `type` `import` `if` `else` `match` `and` `or` `not` `true` `false` `null`

The name of a built-in function (such as `print`, `sum`, `map`, `where`) cannot be used to name a
function, but it can be used as the name of a variable or a column, because a built-in function is
always called in the form `name(...)`, which can be told apart from a variable.

## Errors at run time

Most errors are caught at compile time. The rest, which can only be known at run time, stop the
program and report the location:

```biggo error
fn bucket(id: int, buckets: int) -> int {
  id % buckets
}
print("before")
print(bucket(7, 0))
```

```text output
before
error: division by zero
 --> example.bgo:2:3
  |
2 |   id % buckets
  |   ^^^^^^^^^^^^
```

The runtime errors of the language (not counting those that come from data, such as a missing file
or a value of the wrong type in a CSV):

| Error | When it happens |
| --- | --- |
| `integer overflow` | the result of `+` `-` `*` or `-x` on an `int` exceeds 64 bits |
| `division by zero` | `%` by zero, or `/` `%` of a `decimal` by zero |
| `decimal overflow` | the result of a `decimal` exceeds 38 digits |
| `index ... is out of range` | a list index is out of range |
| `stack overflow` | function calls nest more than 100,000 levels deep |
| `assertion failed` | an `assert` / `assert_eq` fails |
| `... is used before it has a value` | a top-level variable is read before its `let` has run |

A program that ends with an error exits with exit code 1. Everything it printed before that is still
there in full.
