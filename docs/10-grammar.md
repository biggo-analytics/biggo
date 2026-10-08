# Grammar

This is the formal grammar of biggo, written in EBNF: `[ x ]` means optional, `{ x }` means repeated any
number of times (including zero), `|` means a choice, and text in `"..."` is a literal token.
For a usage-oriented explanation, see [The language](02-language.md).

## Tokens

```text
NAME      = (letter | "_") { letter | digit | "_" }      not a keyword
          | "`" { any char other than "`" or a newline } "`"
INT       = digit { digit | "_" }
FLOAT     = INT "." INT [ exponent ] | INT exponent
exponent  = ("e" | "E") [ "+" | "-" ] digit { digit }
DECIMAL   = INT [ "." INT ] "d"
STRING    = '"' { char | escape } '"'                    on a single line
          | 'r"' { any char other than '"' or a newline } '"'
escape    = "\n" | "\t" | "\r" | "\0" | "\\" | '\"'
DATE      = "@" YYYY "-" MM "-" DD
DATETIME  = DATE "T" HH ":" MM [ ":" SS [ "." digit{1,6} ] ]
comment   = "//" to end of line
```

- `letter` is a Unicode character that can start a name (XID_Start). In `NAME`, the `letter | digit`
  that follows stands for any character that can continue one (XID_Continue), so names can be
  written in any script, such as Thai. Everywhere else, `digit` is one of `0` to `9`
- Keywords: `let` `fn` `type` `import` `if` `else` `match` `and` `or` `not` `in` `true` `false` `null`
- A single `_` is an ordinary name, except in a `match` pattern, where it means "any value"
- A number followed immediately by a letter (`5days`) is an error
- `DATE` and `DATETIME` must be a date and time that really exist (`@2026-02-30` is an error)
- `DECIMAL` can have at most 6 digits after the point and 32 digits before the point

## Statements

```text
program   = { statement }
statement = import | type_decl | let | fn_decl | expr

import    = "import" STRING
type_decl = "type" NAME "=" type
let       = "let" NAME [ ":" type ] "=" expr
fn_decl   = "fn" NAME "(" [ param { "," param } [ "," ] ] ")" [ "->" type ] block
param     = NAME ":" type [ "=" expr ]
```

- `import` and `type_decl` are allowed only at the top level of a file, and `import` must come before
  any other statement
- `fn` followed by `NAME` is a function declaration; `fn` followed by `(` is a lambda (an expression)
- Statements are separated by newlines. There is no `;`

### Newlines

A newline ends a statement, except when

1. it is inside a `( )` or `[ ]` that is not closed yet (including the arguments of a function call)
2. the last token of the line is a binary operator, or `=` `:` `->` `=>` `,`
3. the first token of the next line is a binary operator that cannot start an expression:
   `|>` `+` `*` `/` `%` `==` `!=` `<` `<=` `>` `>=` `??` `and` `or` `in` `.`

`-` `(` `[` at the start of a line always begin a new statement. Inside the `{ }` of a block and of a
`match`, newlines separate statements/arms again, even when that block is inside parentheses.

## Types

```text
type       = type_atom [ "?" ]
type_atom  = NAME [ "<" type { "," type } [ "," ] ">" ]
           | "{" [ field_type { "," field_type } [ "," ] ] "}"
           | "fn" "(" [ type { "," type } [ "," ] ] ")" [ "->" type ]
field_type = NAME ":" type
```

- The `NAME`s of the built-in types: `int` `float` `bool` `string` `date` `datetime` `duration` `decimal`
  (no argument), `list<T>` `table<T>` (one argument), `map<K, V>` (two). Any other name is an alias
  declared with `type`
- A `?` after a function type belongs to the result: `fn() -> int?` is a function that returns `int?`

## Expressions

Listed from the loosest binding to the tightest:

```text
expr       = pipe
pipe       = or { "|>" call }
or         = and { "or" and }
and        = not { "and" not }
not        = "not" not | comparison
comparison = coalesce [ ( "==" | "!=" | "<" | "<=" | ">" | ">=" | "in" | "not" "in" ) coalesce ]
coalesce   = additive [ "??" coalesce ]
additive   = term { ( "+" | "-" ) term }
term       = unary { ( "*" | "/" | "%" ) unary }
unary      = "-" unary | postfix
postfix    = primary { call_args | "." NAME | "[" expr "]" }

call       = postfix that ends with call_args
call_args  = [ "<" type { "," type } ">" ] "(" [ arg { "," arg } [ "," ] ] ")"
arg        = [ NAME "=" ] expr
```

- The right-hand side of `|>` must be a function call: `a |> f(b)` is `f(a, b)`
- Comparisons cannot be chained (`a < b < c` is an error), and `in` and `not in` count as
  comparisons
- `not in` is one operator when `not` is on the same line as the expression before it. A `not`
  that starts a line starts a statement
- `??` groups from the right: `a ?? b ?? c` is `a ?? (b ?? c)`
- The `(` and `[` of a `postfix` must be on the same line as the thing they follow
- In `arg`, a name followed by `=` (not `==`) is a named argument. An ordinary function call must put
  positional arguments first, whereas a table operation accepts `name = expr` mixed with column names
  in any order

```text
primary = INT | FLOAT | DECIMAL | STRING | DATE | DATETIME
        | "true" | "false" | "null"
        | NAME
        | "(" expr ")"
        | list | record | update | map | block | if | match | lambda

list    = "[" [ expr { "," expr } [ "," ] ] "]"
record  = "{" NAME ":" expr { "," NAME ":" expr } [ "," ] "}"
update  = "{" "..." expr { "," NAME ":" expr } [ "," ] "}"       a record made from another
map     = "{" key ":" expr { "," key ":" expr } [ "," ] "}"
key     = literal that is not null                           (the first key shows that it is a map)
block   = "{" { statement } "}"
if      = "if" expr block [ "else" ( if | block ) ]
match   = "match" expr "{" { arm } "}"
arm     = pattern { "|" pattern } "=>" expr                  arms are separated by a newline or ","
pattern = "_" | [ "-" ] literal
lambda  = "fn" "(" [ lparam { "," lparam } [ "," ] ] ")" [ "->" type ] block
lparam  = NAME [ ":" type ]
```

Disambiguating `{`:

| What follows `{` | Meaning |
| --- | --- |
| `NAME :` | record |
| `...` | a record made from another |
| literal `:` | map |
| Anything else (including `}`) | block |

`{}` is therefore an empty block, which is interpreted as an empty map when it appears where a map is
expected. The body of an `if` and of a function is always a block.

## Parser limitations

- Nesting (parentheses, blocks, types) can be up to 256 levels deep
- When the parser finds a syntax error, it skips ahead and restarts at the next statement, so it can
  report several errors in one pass

## An example of what the parser sees

`biggo parse file.bgo` prints the syntax tree as an S-expression:

```text
$ cat demo.bgo
let total = 1 + 2 * 3
sales |> where(qty > 0 and not is_null(price)) |> take(5)
let f = fn(x: int) -> int { -x ?? 0 }

$ biggo parse demo.bgo
(let total (+ 1 (* 2 3)))
(|> sales (call where (and (> qty 0) (not (call is_null price)))) (call take 5))
(let f (lambda (x: int) -> int (block (?? (- x) 0))))
```
