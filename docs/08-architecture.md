# Architecture

This page explains how `biggo` works inside, for people who will read or change the source code.
The compiler and engine are written entirely in Rust, split into 8 crates in `crates/`.

## Overview

A biggo program has two worlds that work very differently:

- **Ordinary code** (variables, functions, `if`, lists) is translated to bytecode and run on a
  virtual machine one instruction at a time.
- **Tables** are translated to a *plan* (logical plan), which is optimized and then run by an engine
  that works one column at a time on every core.

The type checker is what separates these two worlds, at compile time.

```text
source (.bgo)
   │  lexer + parser                          biggo-syntax
   ▼
  AST ───────────────► formatter              biggo-fmt
   │  type checker                            biggo-types
   ▼
  HIR  (names bound, types complete, table operations have a plan node attached)
   │  bytecode compiler                       biggo-eval
   ▼
bytecode ── VM ──► values (int, string, list, record, closure, ...)
              │
              │ table operations build plan nodes
              ▼
        logical plan                          biggo-plan
              │  optimizer
              ▼
        optimized plan
              │  execution engine             biggo-exec
              ▼
        Arrow record batches ──► print / write / to_rows
```

| Crate | Role | Approximate size |
| --- | --- | --- |
| `biggo-syntax` | spans, tokens, lexer, parser, AST, diagnostics, text forms of decimal/datetime | 2,800 lines |
| `biggo-types` | name binding, type checking, schema inference, converting the AST to HIR | 5,200 |
| `biggo-plan` | schema, column expressions, logical plan, optimizer | 2,100 |
| `biggo-exec` | engine: reading/writing files, filter, aggregate, join, sort, window | 4,100 |
| `biggo-eval` | bytecode compiler, VM, `Session` (including `import`) | 2,800 |
| `biggo-fmt` | formatter | 800 |
| `biggo-lsp` | language server | 300 |
| `biggo-cli` | the `biggo` command: run, repl, check, test, build, ... | 600 |

Dependencies go one way: `syntax` ← `plan` ← `types` ← `eval` → `exec`.
`biggo-types` knows nothing about the engine (only the shape of a plan), and `biggo-exec` knows
nothing about the language (it takes a plan in and returns Arrow batches). The main external
dependencies are `arrow` / `parquet` (data format and compute kernels), `rayon` (thread pool),
`rusqlite` (bundled SQLite), `memmap2`, `hashbrown`.

## Front end: from text to AST

The **lexer** (`lexer.rs`) is hand-written and reads one byte at a time. Each token records a span
(start and end positions as `u32`) and a `newline_before` flag, which the parser uses to decide
whether a statement has ended. Comments are stored separately for the formatter.

The **parser** (`parser.rs`) is recursive descent for statements and a Pratt parser for expressions
(each operator has a "binding power"). Key design points:

- **The AST lives in an arena**: every expression is in a single `Vec`, and expressions refer to
  each other by `ExprId` (a 32-bit number). Later passes can therefore attach data to an expression
  with a table indexed by id. The type checker uses this to store the type of every expression, so
  that the language server can answer hover requests.
- **Names are interned**: each name is a `Symbol` (a number), so names are compared by comparing
  numbers.
- **Error recovery**: on a syntax error, the parser skips to the start of the next statement and
  continues, so it reports several errors in one pass.
- **Depth limit** of 256 levels, so that strange input cannot overflow the stack.

A **diagnostic** has only a span and a message. `SourceFile::render` draws it with the source line
and `^^^`. Every pass, from the lexer to the VM, reports errors with this same structure.

## The type checker

`biggo-types` does three things in one pass: it binds names to the locations of their values, checks
and infers types, and converts the AST to **HIR** (`hir.rs`), a form that can be compiled directly:

- Every name becomes `Local(slot)`, `Capture(index)`, `Global(slot)` or `Function(id)`.
- Automatic conversions are written out as nodes (`ToFloat`, `Convert`).
- Named arguments are already matched to parameters.
- Each table operation becomes a `TableExpr`: a plan node (`TableOp`) + the input tables +
  the values from the program that the column expressions refer to (parameters).

The main mechanisms (`check.rs`):

- `unify(a, b)` finds a type that both sides can convert to (used for the branches of `if`, the
  elements of a list, ...).
- `coerce(expr, type)` inserts a conversion node when the conversion loses no information.
- A lambda gets the types of its parameters from the context (`FnHint`) passed down from the place
  where it is written.

### Column expressions

Inside `where`, `derive`, `agg` and so on (`verbs.rs`), the checker opens a *column scope*: a name
that matches a column of the table becomes a `Column` node. Then `lower` converts the checked
expression into one of two things:

- If it contains no column at all → it stays ordinary code, which the VM evaluates **once** when the
  plan is built, and the value is passed into the plan as a parameter.
- If it contains a column → it becomes a `plan::Expr` that the engine evaluates over the whole column.

This rule is why `where(qty > threshold())` calls `threshold()` once, and why a function you write
yourself cannot take a column (there is no `plan::Expr` form for it).

### What disappears before the engine (desugaring)

Several things in the language do not exist in the engine, because the type checker converts them
into things that already exist:

| What you write | What the checker builds |
| --- | --- |
| `match x { a => p, _ => q }` | `if x == a { p } else { q }` (the value of `x` is kept in a temporary variable) |
| `distinct()` | `group` by every column, then an `agg` with no aggregates |
| `pivot(c, [v1, v2], sum(x))` | `agg(v1 = sum(if c == v1 { x } else { null }), v2 = ...)` |
| `linreg(t, y, x)` | `agg(slope(y, x), intercept(y, x), corr(y, x))` + `project` + taking the single row |
| a `"right"` join | a `"left"` join with the sides swapped |

The benefit is that the optimizer and the engine do not need to know about anything new: `pivot`
gets filter pushdown and column pruning for free, because it is an ordinary aggregate.

## Bytecode and the virtual machine

`biggo-eval/compile.rs` converts the HIR of each function into a `Proto`: a sequence of `Op` values
with a table of constants. The VM (`vm.rs`) is a **stack machine**: each instruction takes its
operands from the top of the stack and puts the result back. Local variables live on the same stack,
at fixed positions counted from the base of the frame.

- **`Value`** is 24 bytes wide. Values larger than a number sit behind a reference count
  (`Rc`/`Arc`), so copying a value is always cheap, and values of every type are immutable.
- **Specialized instructions**: when the type checker knows that both sides are `int`, the compiler
  emits instructions such as `AddInt`, `LtInt` that need no type check at run time.
- A **closure** is a function id plus the captured values (`captures`), which are copied when the
  closure is created.
- **Loops** (`map` `filter` `fold` `each`) are compiled to `LoopStart` / `LoopNext` /
  `LoopStep` / `LoopEnd` instructions in the function that calls them, not to a nested call inside
  the VM. A function called by a loop is therefore an ordinary call in the same dispatch loop:
  recursion through a loop can go as deep as normal recursion.
- A **table operation** is a `Table(site)` instruction: it takes the input tables and the parameter
  values from the stack, then calls `TableOp::build`, which gives a `Value::Table(Arc<Plan>)`.
  Nothing has run yet.
- **Built-in functions that run a query** (`print`, `write_*`, `count`, `to_rows`, ...) call into
  `biggo-exec`.
- The call stack can be 100,000 frames deep.

`Session` (`lib.rs`) ties everything together for one piece of source: parse → load the files
pulled in by `import` (each file once, with circular imports detected) → type check → compile → run.
One session can take several pieces of source in a row, and each piece sees the definitions of the
earlier ones: the REPL is a session fed one line at a time, and `biggo test` is a session that runs
the test file and is then fed each `test_x()` in turn.

## Plans and the optimizer

A `Plan` (`plan.rs`) is a tree of nodes: `Scan`, `Memory`, `Filter`, `Project`, `Sort`, `Limit`,
`Group`, `Aggregate`, `Join`, `Window`, `Union`, `Unpivot`, `Explode`. Every node knows the schema
of its own result.

Before a plan runs, `optimize` (`optimize.rs`) rewrites it in this order:

1. **simplify**: evaluates expressions that contain no column down to constants (constant folding),
   and removes filters that are always `true`.
2. **push filters**: pushes each `Filter` down as close to the data as possible. It goes through
   `Project` (rewriting the predicate with the source expressions), through `Aggregate` when it uses
   only group keys, through `Window` when it uses only partition columns, down both sides of a
   `Join` as far as the join type allows, and finally **into `Scan`**, which filters rows while the
   file is being decoded. A predicate that can fail at run time (such as `%`) is not moved past a
   point where it would meet rows that the original program never let it see.
3. **push limits**: a `Sort` followed by a `Limit` becomes a top-n (`fetch`), and a `Limit` above a
   `Scan` stops reading the file once enough rows have been read.
4. **prune columns**: walks from the top down to find which columns each node really needs, and cuts
   the rest, all the way to `Scan`, which does not decode columns that nobody uses.
5. **merge projects**: nested `Project` nodes are merged into a single layer.

Every rule keeps the result identical bit for bit, including the order of rows. `biggo explain`
shows the plan before and after.

## Execution engine

`biggo-exec` runs plans on **Apache Arrow**: a table is a sequence of `RecordBatch` values, each of
which stores every column as a contiguous array in memory (32,768 rows per batch). Operations work
on whole arrays with Arrow kernels, which the Rust compiler can turn into SIMD instructions.

**Execution model**: `execute(plan)` returns an iterator of batches (a pull model). When a consumer
stops pulling early (such as `take(5)`), the remaining work never happens. Operations that work one
batch at a time (`Filter`, `Project`, `Unpivot`, `Explode`, the probe side of a join) use `par_map`:
it pulls as many batches as there are cores, processes them at the same time, and emits them
**in the original order**.

**Scan** (`scan.rs`): a file is split into "chunks" that can be decoded independently. CSV and JSON
are cut every 2 MiB at a line boundary (CSV counts `"` so that it does not cut in the middle of a
value that contains a line break), and Parquet is split by row group. Chunks are decoded as many at
a time as there are cores, and the scan's filter runs inside the chunk. The chunk size is fixed and
does not depend on the number of cores, so that the batches that flow out are the same on every
machine.

**Aggregate** (`aggregate.rs`) is a two-stage hash aggregation:

1. Batches are gathered into "runs" of 4 batches. Each run is aggregated on its own core into a
   partial result: the group keys are encoded as bytes (Arrow's row format), a hash table then finds
   the group number, and the accumulator of each aggregate updates over the whole batch in a tight
   loop.
2. The partial results are merged: if there are few groups, they are merged one run at a time; if
   there are more than 50,000 groups, the groups are split into partitions by the hash of the key
   and each partition is merged on its own core. Finally the groups are sorted back into the order
   in which they were first seen.

For fixed-width types, `count_distinct` simply keeps the values of each group, then sorts and counts
them at the end on every core, which is faster than maintaining a hash set per group along the way.

**Join** (`join.rs`) is a hash join: it builds a hash table from the whole right table, then probes
it with the batches of the left table in parallel.

**Sort** (`ops.rs`): top-n uses Arrow's partial selection. A full-table sort encodes the key of each
row as bytes that can be compared directly (in parallel, range by range), then sorts the row numbers
on every core, carrying the first 16 bytes of the key along with each row number so that most
comparisons finish without reading the full key. Rows with equal keys are decided by row number, so
it is a stable sort that gives the same result however the work is divided.

**Window** (`window.rs`): sorts by partition and order, finds the partition boundaries with a
vectorized comparison of adjacent rows, computes each function in that order (one core per
function), then puts the results back in the original row order.

### Reproducible results

A requirement of the engine is that **results must be identical bit for bit, however many threads
there are**. This shapes the design in several places:

- `par_map` always returns batches in input order.
- The size of file chunks and of aggregate runs is a constant that does not depend on the number of
  cores. A `float` sum (which depends on the order of addition) is therefore always added up in the
  same groupings.
- The groups of `group` come out in the order in which they were first seen, even when they are
  merged by partition.
- Sorting breaks ties by row number.

`bench/run.py` checks this every time: it runs each query with every core and with a single core,
then compares the printed output.

### Expressions that can fail

`plan::Expr::can_fail` says whether an expression can stop the program (for example `%` by zero, or
`int` overflow). A branch of `if`, or the right side of `and`/`or`/`??`, that can fail is evaluated
only on the rows that need it (`eval_where`), so that `if n != 0 { x % n } else { 0 }` means what
it says even though the engine evaluates whole columns. For an expression that cannot fail, both
branches are evaluated and then one is selected, which is faster.

## Tools

- The **formatter** (`biggo-fmt`) converts the AST into a Wadler-style "document" (`Doc`: text,
  groups, places where a line may break), then lets the layout step choose which groups fit on a
  line. Comments are put back using their position in the source.
- The **language server** (`biggo-lsp`) keeps the text of the open files, re-analyzes the whole file
  on every change (the checker is fast enough: several hundred thousand lines per second), and
  answers hover requests from the table of types per expression.
- **`biggo build`** copies its own executable, then writes the program's source into a 256 KiB block
  reserved in the binary (found by a marker). An executable that finds a program in this block runs
  it instead of reading the command line.

## Testing

| Kind | Where | What it checks |
| --- | --- | --- |
| unit tests | `src/` of each crate | individual parts: lexer, parser, VM, file readers, sorting |
| golden: syntax | `testdata/**/*.syntax` | the syntax tree or syntax errors of every example file |
| golden: run | `testdata/run/*.out` | the output and runtime errors of the example programs |
| golden: check | `testdata/check/*.out` | every type error message |
| golden: plan | `testdata/sales.plan` | the plan before and after optimization |
| robustness | `survives_mangled_programs` | every prefix of every program, and every program with one character deleted, must not panic |
| formatter | `biggo-fmt/tests` | formatting does not change the meaning, and formatting again gives the same result, for every example file |
| docs | `biggo-cli/tests/docs.rs` | every program in the docs runs, and the output shown matches the real output |
| consistency | `biggo-types/tests/in_sync.rs` | every built-in function is in the editor grammar and in the reference |
| CLI | `biggo-cli/tests/cli.rs` | real commands through real processes: run, repl, fmt, test, build, lsp |

```sh
cargo test                      # everything
BIGGO_BLESS=1 cargo test        # rewrite the golden files and the output in the docs to match current behavior
cargo clippy --all-targets      # lint
cargo fmt                       # format the Rust code
```

After `BIGGO_BLESS=1`, always read the diff of the files that changed: blessing is a statement that
the new behavior is correct.

## Adding a feature

**A scalar function** (such as `replace(s, a, b)`):

1. Add a variant to `ScalarFn`, with its name (`biggo-plan/src/expr.rs`).
2. Give the types of the arguments and the result in `scalar_call` (`biggo-types/src/verbs.rs`).
3. Write the computation on Arrow arrays in `call` (`biggo-exec/src/expr.rs`).
4. Add the name to the editor grammar and to `docs/06-builtins.md` (the `in_sync` test warns you if
   you forget).

You do not need to touch the VM: scalar values are computed through the same path as columns
(`call_scalar`), so you get the same behavior in both places automatically.

**An aggregate**: add it to `AggFn`, give its type in `agg_type`, and add an accumulator to `Acc`
(`new`, `update`, `merge`, `finish`). `merge` must give the same result as updating continuously.

**A table operation**: if it can be written in terms of existing operations, desugar it in the type
checker (like `pivot`). If not, add a node to `Plan` and `TableOp`, teach the optimizer (`map_plan`,
`push_filters`, `prune`) which columns the new node needs, then write its executor in `biggo-exec`.

**A built-in function for ordinary code** (like `len`): add it to `Builtin` (`hir.rs`), type check
it in `builtins.rs`, and run it in `Vm::builtin`.
