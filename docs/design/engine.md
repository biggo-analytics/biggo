# Engine design: execution, large data, memory and data sources

This document proposes the next generation of the execution engine (`crates/biggo-exec`). It covers
four areas: in-memory and parallel execution, data larger than memory, memory management, and data
sources. Nothing in it is built yet.

How to read it:

- **Today** describes the code at commit `561a4e3` (9 October 2026). Every statement there comes
  from reading the code. Where it says "observed", the existing binary was run to confirm it.
- Timings come from [docs/09-performance.md](../09-performance.md) (5 million rows, Apple M1 Pro,
  8 October 2026). Every other number about speed is marked as an estimate.
- Each area has the same five parts: today, proposed, why this and not the alternatives, keeping
  results reproducible, how to test and measure it.
- Out of scope: the bytecode VM, reordering of joins, and running on several machines.

Terms used throughout:

| Term | Meaning |
| --- | --- |
| batch | An Arrow `RecordBatch` of a bounded number of rows, plus an optional mask of selected rows |
| morsel | The unit of scheduling: the rows of one split of a source (2 MiB of CSV text, one Parquet row group, ...). One worker runs a morsel from the source to the sink |
| pipeline | A source, a chain of streaming operators, and a sink |
| breaker | An operator that must see all its input before it produces output: aggregate, sort, join build, window. It is the sink of one pipeline and the source of the next |
| seq | The number of a morsel in its pipeline. Morsel order is row order |
| row ordinal | The position of a row: `(seq, row within the morsel)`, packed in a `u64` |

The decisions, in one table:

| Problem | Decision |
| --- | --- |
| Execution model | Push-based pipelines, driven by morsels, on the existing rayon pool |
| Row order | Every morsel has a seq. Order-sensitive steps combine morsels in seq order, or recover order from row ordinals |
| Float sums | Fixed order: per-morsel sums, folded in seq order. Morsel boundaries are part of the language's behavior; nothing else is |
| Filters | A mask on the batch, compacted late; never one copy per condition |
| Strings | `Utf8View` arrays that point into the source bytes; dictionaries kept only when Parquet provides them |
| Group keys | Three layouts chosen at plan time: one fixed-width key, one string key, generic packed rows |
| Aggregation | Per-morsel pre-aggregation, 64 radix partitions, ordered merge per partition |
| Join | Partitioned parallel build, Bloom filter, build side chosen from source statistics when row order allows it |
| Sort | Normalized keys (kept), no `concat`, sorted runs, heap-based top-n |
| Larger than RAM | External merge sort, partition spilling for aggregate, join and window, Arrow IPC spill files |
| Memory | One pool per process, default limit 50% of RAM, reservations per operator, spill then a precise error |
| Sources | One `Connector` trait: validate, plan splits, negotiate pushdown per condition, read, write with atomic replace |
| I/O | Synchronous, on a small dedicated I/O pool; no async runtime in the engine |

## 1. In-memory processing and parallel execution

### Today

- **Model.** `execute` (`lib.rs`) walks the plan and returns a `BatchIter`, a boxed iterator of
  `Result<RecordBatch>`: a pull model. `Filter`, `Project`, `Unpivot`, `Explode` and the probe side
  of a join wrap their input in `ops::par_map`. `ParMap::next` pulls
  `rayon::current_num_threads()` batches, maps them with `into_par_iter`, and yields the results in
  input order. Each operator is its own fork-join step: a batch waits at one barrier per operator
  and can change core between operators.
- **Breakers.** `ops::sort`, `aggregate::aggregate`, `join::join` (the build) and `window::window`
  do all their work inside `execute`, before the iterator is returned. Sort, window and the join
  build start with `ops::concat`, which copies every batch into one `RecordBatch`.
- **Scan.** `scan::scan` cuts a file into `Piece`s, closures that return `Vec<RecordBatch>`: 2 MiB
  of CSV or JSON text (`PIECE_BYTES`), one Parquet row group, or a whole SQLite result.
  `ScanIter::next` decodes `current_num_threads()` pieces at a time and queues their batches of
  32,768 rows (`BATCH_ROWS`).
- **Filters copy.** `ops::filter` and `Shape::finish` call `filter_record_batch`, which copies every
  column. `Shape::finish` does so once per pushed condition.
- **Expressions.** `expr::eval` walks the `Expr` tree for every batch. It finds columns by name
  (`column_by_name`), builds literal arrays again for each batch, and keeps single values as
  `Col::Scalar`. An operand that `can_fail` goes through `eval_where`: filter the whole batch,
  evaluate, scatter back with `take`. The engine does not share common subexpressions; the
  optimizer's `merge_projects` only avoids duplicating a computed column that is used twice.
  Strings are `Utf8` arrays (`convert::arrow_type`). There are no dictionaries. `expr::strings`
  and `text.rs` allocate one `String` per row.
- **Aggregate** (`aggregate.rs`). `aggregate` reads 64 batches at a time (`WINDOW`) and aggregates
  each run of 4 consecutive batches (`RUN`) on its own core into a `State`. Keys always go through
  `arrow::row::RowConverter`, then `Groups::intern` (a `hashbrown::HashTable<u32>` over the key
  bytes). All run states stay in memory until the input ends. Then they are merged in run order:
  on one thread below 50,000 groups (`PARTITION_MIN`), otherwise by `merge_partitioned`, which
  splits groups by hash into `current_num_threads()` parts and restores first-appearance order by
  sorting `(run, group id)`. With many groups every key is hashed and inserted about twice (once
  in its run, once in the merge), and pre-aggregation removes almost nothing.
- **Join** (`join.rs`). `join` concatenates the whole right side, and `Table::build` builds on one
  thread: row-format keys, a `HashTable<u32>` of chain heads and a `next` chain per row. `probe`
  runs under `par_map`, looks keys up row by row and gathers both sides with `take`. Nothing
  chooses the build side. `matched: Vec<AtomicBool>` is allocated for every join kind, though only
  a full join reads it.
- **Sort** (`ops.rs`). `sort` concatenates the input, evaluates the keys, and calls
  `sort_indices_from`. Without `fetch` and from 16,384 rows it encodes normalized keys in lots of
  65,536 rows in parallel, sorts `(u64, u64, u32)` entries (16 key bytes and the row number) with
  `par_sort_unstable_by`, and breaks ties by row number. With `fetch` it calls
  `lexsort_to_indices(.., fetch)`: a partial sort, but still over the concatenated table (q4 peaks
  at 387 MB). The output is one batch made by `take_record_batch`.
- **Window** (`window.rs`). `window` concatenates the input, sorts all rows by partition keys then
  order keys with `sort_indices`, finds boundaries with `changes`, computes each function on its
  own core (`compute`), and puts every result back with `take` over the inverse permutation.
- **distinct.** `distinct()` is an `Aggregate` with no aggregates (type checker). `count_distinct`
  keeps every value of every group (`Acc::DistinctFixed`: a `Vec<i64>` per group, sorted and
  deduplicated at the end) or a `HashSet<Box<[u8]>>` per group (`Acc::Distinct`).
- **Order.** `par_map` keeps input order; pieces and runs have fixed sizes; partial aggregates are
  merged in run order; sorts end with the row number. So results do not depend on the thread
  count, as `bench/run.py` checks. Two exceptions are listed in
  [the findings](#findings-in-todays-code): which error is reported, and whether one is reported
  at all, can depend on the thread count.
- **Limits by construction.** Row numbers are `u32` in sort, join and window. `concat` builds
  `Utf8` arrays with 32-bit offsets, so a string column that totals more than 2 GiB makes sort,
  window and the join build fail with Arrow's offset-overflow error.

### Proposed

#### 1.1 Execution model: push-based pipelines driven by morsels

The optimized `Plan` is cut at its breakers into pipelines. Pipelines run one after the other, in
dependency order (a join's build before its probe), and each one uses every core.

```rust
pub struct Batch {
    pub columns: RecordBatch,          // at most BATCH_ROWS rows
    pub keep: Option<BooleanBuffer>,   // rows still selected; None means all
}

pub trait Source: Send + Sync {
    /// Hands out the next morsel, in row order. Called under the dispatcher's lock, so a source
    /// that reads a stream does its sequential I/O here. `None` ends the input.
    fn claim(&self) -> Result<Option<MorselInput>>;
    /// Decodes one morsel on a worker and pushes its batches in row order.
    fn decode(&self, input: MorselInput, out: &mut dyn FnMut(Batch) -> Result<Flow>) -> Result<()>;
}

pub trait Operator: Send + Sync {
    /// Pushes zero or more batches for one input batch, in row order.
    fn push(&self, batch: Batch, out: &mut dyn FnMut(Batch) -> Result<Flow>) -> Result<Flow>;
}

pub trait Sink: Send + Sync {
    fn start(&self, seq: Seq) -> Box<dyn MorselState>;
    fn consume(&self, state: &mut dyn MorselState, batch: Batch) -> Result<Flow>;
    /// The morsel is complete. Morsels finish in any order.
    fn finish_morsel(&self, seq: Seq, state: Box<dyn MorselState>) -> Result<()>;
    /// All morsels are in. Returns the source of the next pipeline.
    fn finish(self: Arc<Self>) -> Result<Arc<dyn Source>>;
}

pub enum Flow { More, Done }           // Done: this consumer needs no more rows
```

```text
run(pipeline):
    next = 0                 # next seq to hand out
    low  = 0                 # smallest seq the sink has not released yet
    on every thread of the pool:
        loop:
            under the dispatcher's lock:
                wait while next >= low + W                 # admission window, W = 2 * threads
                stop if the sink is Done, the source is empty, or a lower seq has failed
                input = source.claim(); seq = next; next += 1
            state = sink.start(seq)
            source.decode(input, |b| operators.push(b, |b| sink.consume(state, b)))
            sink.finish_morsel(seq, state)                 # the sink advances `low` on release
    sink.finish()                                          # may use the whole pool again
```

- A morsel stays on one core from decoding to the sink, while its batches are in that core's cache.
  There is no barrier between operators and no queue between them.
- A worker claims a new morsel when it finishes one, so a slow core simply takes fewer morsels.
  Today a group of pieces waits for its slowest member, which is one reason 4 to 8 threads gains
  only 1.15 to 1.3 times on a machine with two efficiency cores (docs/09).
- The outside interface stays `execute(plan) -> BatchIter`. The last pipeline's sink is an ordered
  collector with a bounded queue; the iterator pops from it. Dropping the iterator sets `Done`.
- Sources of each kind of morsel:

| Source | One morsel |
| --- | --- |
| CSV, JSON Lines (any transport, compressed or not) | 2 MiB of decoded text, cut at the first row end at or after each multiple of 2 MiB (today's rule) |
| Parquet, Arrow IPC | One row group, one record batch |
| Row streams (databases, Excel) | 65,536 rows |
| In-memory table, output of a breaker | 131,072 rows by position |

#### 1.2 How order is tracked

```rust
/// Number of a morsel in its pipeline. Morsel order is row order.
pub struct Seq(pub u32);
/// Position of a row. Compares as (morsel, row within the morsel's output at the sink).
pub struct RowOrd(pub u64);            // 24 bits of seq, 40 bits of row; both are checked
```

Inside a morsel everything is sequential, so rows keep their order for free. Across morsels there
are four mechanisms, and every operator uses exactly one:

1. **Turnstile.** A sink whose combine step is order-sensitive parks the result of a finished
   morsel in a reorder buffer and releases results strictly in seq order. The worker does not
   block: it leaves the result and claims the next morsel. The admission window bounds the buffer
   at `W` morsels. Used by: the ordered collector, file writers, aggregation.
2. **Gate.** `Limit` needs the number of rows before a morsel. The worker buffers the morsel's
   batches at the gate, waits until every lower seq has declared its row count, slices, and
   continues downstream. Lower seqs only declare a count, so the wait is short. When the limit is
   reached the gate returns `Done` and higher seqs are discarded.
3. **Ordinal recovery.** Sinks that scatter rows (sort, join build, window, raw aggregation
   chunks) attach a `RowOrd` to each row or chunk and restore order when they finish.
4. **Ordered errors.** An error is an event at a `(seq, row)`. The query fails with the error of
   the lowest seq that the consumer would have reached. Workers on lower seqs run on (they may fail
   earlier in row order); higher seqs are cancelled. An error past the point where a `Limit`
   stopped is dropped.

#### 1.3 Selection masks instead of filtered copies

`Filter` evaluates its conditions to one mask and stores it in `Batch::keep`. It does not copy.

- Conditions are evaluated in program order. A condition that cannot fail is evaluated on all rows
  of the batch (a kernel over 8,192 values costs less than compacting first).
- A condition or expression that `can_fail` is only ever evaluated on compacted rows: rows that
  earlier conditions dropped must not raise its error. The compiler inserts the compaction.
- A batch is compacted when the mask keeps less than half of its rows, and always before a sink
  stores it. Compaction uses one prepared `FilterPredicate` for all columns and touches only the
  columns the rest of the pipeline reads. With string views it copies 16 bytes per string.
- Sinks read through the mask (set-bit iteration) without compacting.

Index vectors, as in DuckDB, are not used: Arrow kernels take whole arrays, masks combine with one
`and`, and the filter kernels take masks.

#### 1.4 Expression evaluation

All expressions of one pipeline stage (the conditions of a filter, the columns of a projection, the
keys and arguments of an aggregate) are compiled once into one register program:

```rust
pub struct ExprProgram {
    steps: Vec<Step>,                  // one per distinct subexpression, in dependency order
    outputs: Vec<Reg>,                 // one per expression asked for
}
enum Step {
    Column(usize),                     // index into the batch, bound at compile time
    Const(ArrayRef),                   // one-element array, built once
    Kernel { f: KernelFn, args: Vec<Reg>, guard: Option<Reg> },
}
```

- **Constant folding** stays where it is, in `optimize::simplify`. The program only stops
  rebuilding literal arrays per batch.
- **Common subexpressions.** Compilation hash-conses on `(ExprKind, type, guard)`. Two equal
  subexpressions under the same guard share a register. The guard is the mask of rows on which a
  branch of `if`, `and`, `or` or `??` may be evaluated. An expression that cannot fail is hoisted
  to "no guard" and shared everywhere; one that can fail is shared only under the same guard.
  The optimizer can then merge projections freely, and q1's `revenue`, used by `sum` and `mean`,
  is computed once without a projection of its own.
- **Guards replace `eval_where`.** A guarded step compacts only the columns it reads, evaluates,
  and scatters back.
- **Registers are freed after their last use**, so intermediate arrays do not outlive the step.
- **No JIT.** Kernels over thousands of rows amortize the interpretation of the tree; a JIT would
  add start-up time (5 ms today) and a large dependency.

#### 1.5 Strings: views and dictionaries

- `string` columns become `Utf8View` inside the engine. A view is 16 bytes: the length and either
  the string itself (up to 12 bytes) or a 4-byte prefix with a buffer number and offset. Arrow 60
  supports it in the CSV and JSON readers, the row format, comparisons, `concat`, `cast`, and the
  Parquet reader.
- Views point into the bytes of the source (a mapped CSV file, a decompressed block, a Parquet
  page), so reading a string that needs no unescaping copies nothing. `filter`, `take` and the
  final gather of a sort move 16 bytes per string. An array is no longer limited to 2 GiB.
- Equality and hashing of strings up to 12 bytes are operations on one `u128`.
- **Dictionaries** are kept only where they already exist: a Parquet column chunk whose pages are
  all dictionary-encoded is read as `Dictionary(Int32, Utf8View)`. Conditions against a constant,
  and unary functions, are evaluated once per dictionary entry and gathered by code. Group and
  join keys translate codes through a table built once per morsel. Any other operator decodes to
  views first (the compiler inserts the cast). CSV and JSON are not dictionary-encoded on read:
  building a dictionary costs a hash per value, and short strings are already cheap as views.

#### 1.6 Hash aggregation

**Key layouts**, chosen at plan time from the static key types:

| Layout | When | Key in the table |
| --- | --- | --- |
| `Fixed` | One key of `int`, `date`, `datetime`, `duration`, `bool` or `float` | `u64` (the value's bits; a separate slot for the null group) |
| `Str` | One `string` key | The 16-byte view; long strings are copied to the table's arena and compared by prefix, then bytes |
| `Rows` | Anything else | Packed rows: a null bitmap, fixed-width values, then `u32` length and bytes per string. Byte equality is key equality |

`Rows` replaces the row format of `arrow::row` here: that format is built to preserve order, which
grouping does not need, and it costs more per string. `Fixed` compares the same bits that the row
format encodes, so the groups are identical to today's (`0.0` and `-0.0` stay different keys).

**Algorithm.** The radix of a group is the top 6 bits of its hash: 64 partitions, fixed.

```text
per morsel m, on one worker:
    table = empty, local to the morsel
    for each batch:
        hash the keys
        mode PreAggregate: id = table.intern(key), remembering the row of first sight
                           accumulators.update(id, arguments)
        mode Raw:          append (key, arguments, row number) to buffer[radix(hash)]
        after the first 4,096 rows: if table has more than 1,024 groups, flush the table as
                                    partial chunks and continue in mode Raw
    hand over 64 chunks (m, partition, Partial or Raw) to the sink

sink, per partition p, chunks strictly in seq order (turnstile, one lock per partition):
    Partial chunk: id = global[p].intern(key); accumulators.merge(id, state)
    Raw chunk:     if any accumulator is order-sensitive: pre-aggregate the chunk on its own,
                       then merge it as a Partial chunk
                   else: id = global[p].intern(key); accumulators.update(id, arguments)

finish: each partition yields its groups with `first`, the RowOrd of the group's first row;
        the output is the groups sorted by `first` (first-appearance order, as today)
```

- With few groups (q1: 96) each morsel sends a handful of partial rows, and the merge is trivial.
- With many groups (q2: 1 million) a morsel stops pre-aggregating after 4,096 rows. Each row is
  then hashed once, scattered once, and inserted once into a partition table of about 16,000
  groups that stays in the L2 cache. Today each row is inserted twice into much larger tables.
- A worker that finishes a morsel helps merging when chunks are waiting, before it claims again.
  Merges of different partitions run at the same time.
- A partition whose table passes 131,072 groups is split by the next 6 hash bits.
- When a `Sort` follows directly (every benchmark query does this), the final reordering by
  `first` is skipped: the aggregate exposes `first` as a hidden last sort key instead.

**Accumulators** fall into two classes:

| Aggregate | State per group | Combine |
| --- | --- | --- |
| `count` | `i64` | Add |
| `sum` of `int`, `duration` | `i128` | Add; the range is checked once, at the end |
| `sum` of `decimal` | `i128` and an `i64` carry | Add; checked at the end |
| `min`, `max` | The value | Total order (for `float`: `f64::total_cmp`, as sort uses) |
| `first`, `last` | The value and its `RowOrd` | Smallest or largest `RowOrd` |
| `median` | The values | Append; sorted at the end |
| `sum` of `float`, `mean` | `f64` (and a count) | **Order-sensitive**: add partial sums in seq order |
| `stddev`, `corr`, `cov`, `slope`, `intercept` | Count, means, sums of products | **Order-sensitive**: today's pairwise `merge`, in seq order |

The first class is exact and commutative, so it does not care how chunks are formed. Only the
second class needs the fold order defined in [Keeping results reproducible](#keeping-results-reproducible).

#### 1.7 Hash join

- **Build** is a pipeline whose sink scatters rows (`RowOrd`, keys in the layouts of 1.6, the
  columns the output needs) into the same 64 radix partitions. At the end, each partition is
  finished on its own core: order its chunks by seq, so its rows are in input order; build
  `first[bucket]` and `next[row]` arrays by walking the rows backwards, as `Table::build` does
  today, so every chain lists its rows in input order. Rows with a null key are kept (a full join
  needs them) but not inserted.
- **Probe** is a streaming operator. Per batch: evaluate and hash the keys; test a blocked Bloom
  filter (one 64-bit word per key, built with the partitions); look up the survivors; gather. When
  every probe row matches exactly once (the usual foreign-key join, q3), the probe columns pass
  through untouched and only build columns are gathered.
- **Output order** is unchanged: probe rows in order, and for each its matches in build order.
- **Join kinds.** Inner, left, semi and anti are pure probe-side loops. A full join sets a bit per
  matched build row (`AtomicU64` words, allocated only for this kind) and emits the unmatched
  build rows at the end, merged across partitions by `RowOrd`.
- **Runtime filter.** For inner and semi joins, the build's Bloom filter and key range are handed
  to the probe-side scan as an extra, inexact condition. Parquet uses the range to skip row
  groups and pages (section 2.7).
- **Build side.** The planner estimates rows times row width for both inputs from source
  statistics (section 4.1). It swaps the sides when the written build side (the right) is
  estimated at 4 times the left or more, and one of these holds:
  - the kind is semi or anti: the build side then holds the left rows, marks them, and emits them
    in build order, which is left order;
  - the consumer does not observe row order: an aggregate with only commutative accumulators
    (first-appearance order then uses the pair of ordinals), or a sort.

  Otherwise the sides stay as written, because the documented output order is the left table's
  order and restoring it would mean sorting the whole output. The decision uses plan and file
  metadata only, so it is the same on every run.

#### 1.8 Sort and top-n

Kept from today: normalized keys, a 16-byte prefix carried with each entry, ties broken by row
position. Changed:

- **No `concat`.** The sort sink keeps the batches of each morsel. Keys are encoded per morsel by
  the worker that produced them (this replaces the 65,536-row lots). An entry is
  `(prefix: u128, RowOrd)`; the sort is `par_sort_unstable_by` on prefix, full key, `RowOrd`.
- **Fixed-width key path.** When every key is fixed-width and declared non-null, keys are written
  straight into the prefix, without validity bytes. If they fit in 16 bytes, the prefix is the
  whole key and no row format is built.
- **Output** is gathered in parallel, one output morsel of 131,072 rows per task, with
  `interleave` over the kept batches. The sorted table is never one giant batch.
- **Top-n** (`Sort` with `fetch`, when `skip + fetch <= 131,072`):

```text
shared: bound = the n-th best key seen so far (only ever tightens), behind a lock that readers skip
per morsel: heap of at most n entries (key, RowOrd), and the candidate rows they refer to
    for each batch:
        compare the first key column with bound's first key (one vectorized comparison) -> mask
        encode keys for the survivors only; push into the heap; drop the worst beyond n
        compact the kept candidate rows when they exceed 2n
at the end of the morsel: merge the heap into the global heap; tighten bound
finish: the global heap, sorted
```

  A stale `bound` only keeps extra candidates, so the result is exactly the first n rows of the
  stable sort. Memory is `threads * n` rows instead of the whole table. Above the threshold, the
  full sort runs and each run keeps only its first n rows.

#### 1.9 Window functions

- **Partition, then sort.** The sink keeps the input batches as they are and scatters only
  `(RowOrd, partition keys, order keys, arguments)` by the hash of the partition keys into the 64
  radix partitions. Each radix partition is sorted on its own core by (partition keys, order
  keys, `RowOrd`), and `changes`, `layout` and `compute` run on it as today.
- **Results are written back by `RowOrd`** into output columns aligned with the kept batches. The
  payload columns never move, and the output is in input order by construction.
- **Reuse of order.**
  - If the input is already sorted by the partition keys followed by the order keys (a `Sort`
    directly below, tracked as a property of the physical plan), the window runs as a streaming
    operator over adjacent rows: no hash, no sort.
  - Adjacent `Window` nodes with the same `partition` and `order`, where the second does not read
    the first's results, are merged by the optimizer and share one sort.
  - A window with only whole-partition aggregates and no `order` uses the hash aggregation of 1.6
    and gathers the result per row: no sort at all.
- `moving_avg` restarts its running sums at every partition and counts non-finite values
  separately (see the findings).

#### 1.10 `distinct` and `count_distinct`

- `distinct()` stays an aggregate with no accumulators. It gets one extra property: when the
  merge of morsel `m` adds a new key, that key is final and already in first-appearance order, so
  it is emitted immediately. `distinct |> take(n)` then stops early.
- `count_distinct(x)` per group becomes two levels: a distinct set over `(group keys, x)`, built
  with the machinery of 1.6 in a second table fed by the same morsels, then a count per group
  joined to the main result by key. Memory becomes proportional to distinct pairs, not rows
  (q9 keeps 80 MB of raw values today), and the set can spill like any aggregation.

### Why this and not the alternatives

| Alternative | Why it loses here |
| --- | --- |
| Keep pull with `par_map` | One barrier per operator, batches migrate between cores, breakers run inside `execute`, and errors depend on the group size |
| Exchange operators with one partition per thread | The data is shaped by the thread count: batches, partial sums and row order all change with it, against principle 2 |
| Async streams on tokio | A second scheduler and a large dependency for CPU-bound work; partition count again follows thread count |
| Compiling queries to machine code | Start-up cost, a compiler backend to ship, and Arrow kernels already use SIMD |
| Thread-local hash tables that live across morsels | The partial sum of a group would depend on which morsels a thread happened to take |
| Order-independent float sums (exact or binned reproducible summation) | 48 bytes or more per sum and several times the work per value; `stddev` and `corr` would need a different, less stable formula. Kept as an open question for 1.0 |
| A global hash table for the join, built with atomic inserts | Chain order depends on timing; partitions give the same cache behavior with a fixed order |
| Radix join that also partitions the probe side | Breaks probe order and pays a full pass over the big side; it is the out-of-core path (2.5), not the default |
| Dictionary-encoding every string column on read | A hash per value at decode time; views give the same benefit for short strings |

### Keeping results reproducible

These rules hold for the whole design. Later sections refer to them by number.

- **R1. Morsel boundaries are data.** They are a function of the source bytes or metadata and of
  the table in 1.1, never of threads, memory or timing. `x.csv` and `x.csv.gz` give the same
  morsels.
- **R2. A morsel is processed alone.** What a worker does with a morsel depends only on that
  morsel and on constants. It may read shared hints that cannot change the result (the top-n
  bound, the Bloom filter).
- **R3. Order-sensitive combining happens in seq order.** A float sum of a group is defined as:
  the per-morsel sums of that group, each added in row order from `0.0`, folded in seq order.
  This is today's definition with "run of 4 batches" replaced by "morsel". It does not depend on
  the batch size, on pre-aggregation versus raw mode, on partition counts, on spilling, or on
  pushdown (a row never changes morsel).
- **R4. Every comparison-based order ends with `RowOrd`.**
- **R5. Tuning values never reach results.** Thread count, batch size, admission window, radix
  bits, hash seed, memory limit and spill decisions are all free to change. The hash seed is
  fixed anyway (today `DefaultHashBuilder` is seeded randomly per run), so that runs are
  comparable in time and in spill behavior.
- **R6. Errors are ordered** (mechanism 4 of 1.2), and integer and decimal sums fail only when the
  final sum does not fit, not when a prefix overflows.

Two behaviors change on purpose and need the owner's agreement (see open questions): float sums
of inputs larger than one morsel change in their last bits once (the unit changes), and `min` and
`max` of floats follow the total order.

### How to test and measure it

- **Oracle.** The current operators stay in the tree as `reference::*` until the new ones have
  parity. A generator of random tables and plans (the xorshift style of the tests in `ops.rs`)
  compares new against reference for every operator, including nulls, `NaN`, `-0.0`, empty
  inputs, and strings longer than 12 bytes.
- **Determinism matrix.** One test runs each plan under threads {1, 2, 3, 8}, batch rows
  {1, 7, 1,024, 8,192}, window {1, 4, 64}, two hash seeds, and worker delays injected at random.
  It compares a hash of the Arrow IPC bytes of the full result, not printed text. The knobs are
  test-only environment variables.
- **Fast path against plain path**, as ROADMAP section 12 requires: `Fixed` and `Str` keys against
  `Rows`; top-n against sort then take; the fixed-width sort path against the row format; mask
  against compacted evaluation; dictionary against decoded strings.
- **Error tests.** A file with a bad value late in the data must give the same outcome and message
  for every thread count, with and without a `take` above it.
- **Measure.** `bench/run.py` (q1 to q11, 8 threads against 1, peak memory) after every step.
  Add queries for what the current set does not stress: a join with a large build side, a full
  sort, a top-n with a large n, a group by a long string key. `biggo explain --timings` (ROADMAP
  section 11) reports rows, time and peak bytes per operator, and is built first.

## 2. Big data and very large files

### Today

- **What streams.** Scan, filter, project, join probe and `limit` pass batches along, and
  `write_csv`, `write_json` and `write_parquet` write batch by batch. An aggregate with few groups
  holds little.
- **What materializes.** `ops::concat` in sort, window and the join build holds the input and its
  copy at the same time; sort then adds key rows, 24-byte entries and the gathered output. An
  aggregate keeps one `State` per run until the end, so with many groups it holds the keys of
  every run. `materialize` (`collect`), `to_rows` and `write_sqlite` (which calls `collect` first)
  hold the whole result. There is no memory limit and no spill: q7 holds 818 MB for 5 million rows
  (docs/09).
- **CSV.** `csv_pieces` maps the whole file. `split_rows` counts the quotes of every 2 MiB block of
  the file before the first row is produced, so `take(5)` on a large file still reads all of it.
  A position is "inside quotes" when the number of `"` bytes before it is odd. That is exact when
  quotes appear only around fields and doubled inside them. It is wrong after a quote inside an
  unquoted field (`2" nail`), which the Arrow reader accepts as data: later cuts use the inverted
  state (see the findings). The splitter needs random access, so it cannot read a pipe or a
  compressed file.
- **Other encodings.** `csv_pieces` decodes a non-UTF-8 file whole (`encoding.decode(&mapped)`),
  on one core, into anonymous memory (`Text::Decoded`).
- **JSON.** `json_pieces` maps the file and `split_lines` cuts at the first `\n` after every 2 MiB.
  This is exact for JSON Lines. A file that is one JSON array is not supported.
- **Parquet.** `parquet_pieces` reads the footer once, makes one piece per row group, and each
  piece decodes all projected columns of its row group into a `Vec<RecordBatch>` before
  `Shape::finish` casts and filters them. Statistics, page indexes, Bloom filters and row filters
  are not used.
- **SQLite.** `sqlite_pieces` makes one piece that reads the whole result into `Vec<Scalar>`
  columns and builds all its batches before returning the first. A pushed `limit` cannot stop it.
- **Not there.** Compressed input, several files in one call, standard input. A table used twice
  is computed twice; `collect` is manual (docs/04).

### Proposed

#### 2.1 Which operators stream and which hold data

| Operator | State | When the state does not fit |
| --- | --- | --- |
| Scan, filter, project, unpivot, explode, union, join probe | One morsel in flight per worker | Cannot happen: the admission window bounds it |
| `Limit`, top-n | `skip + fetch` rows per worker | Above 131,072 rows top-n becomes a sort |
| Aggregate, `distinct` | One entry per group | Partitions spill (2.4) |
| Join build | The build side | Partitions spill (2.5) |
| Sort | All rows | Sorted runs (2.3) |
| Window | All rows | Partitions and kept input spill (2.6) |
| `collect` | All rows | Chunks spill and are read back on use |
| File writers | One morsel per worker, in the turnstile | Bounded by the admission window |

#### 2.2 Spill files: Arrow IPC messages with an index in memory

A spill file belongs to one operator and one partition. It holds a schema message, then one
record-batch message per chunk, aligned to 64 bytes. The operator keeps a 32-byte index entry per
chunk in memory: `(seq, kind, offset, length, rows)`. Chunks are read back with positioned reads
into aligned buffers that the memory pool counts.

| Operator | A chunk holds |
| --- | --- |
| Sort | A slice of a sorted run: the normalized key as a binary column, then the payload |
| Aggregate | Raw rows (keys, arguments, row number) or partial states (keys, state columns, `first`) |
| Join | Build or probe rows of one radix partition, with `RowOrd` |
| Window | Partition rows; later, results keyed by `RowOrd` |
| `collect` | The batches as they are |

Dictionaries are decoded before spilling. Files are not compressed by default: on a local SSD the
copy is faster than LZ4 (estimate), and `BIGGO_SPILL_COMPRESSION=lz4` exists for slow disks.
Where the files live and how they are removed is section 3.8.

#### 2.3 External merge sort

```text
sink:   keep batches and encoded keys while the reservation grows
        when it cannot grow: sort what is held (as in 1.8), write it as run k, release the memory
finish: no run written  -> the in-memory path of 1.8
        otherwise       -> write the rest as the last run, then merge
merge:  take T-1 splitter keys from the runs' sparse indexes (the first key of every chunk)
        task i merges the key range [splitter i-1, splitter i) of all runs with a loser tree
        and is the source of the next pipeline for seq i: seq order is sort order
        more than 64 runs: first merge groups of 64 into longer runs
```

Splitters are full keys including `RowOrd`, so ranges do not overlap and ties cannot cross them.
A merge task holds one chunk per run. With `fetch`, every run keeps only its first `skip + fetch`
rows.

#### 2.4 Aggregation that spills

```text
when the sink's reservation cannot grow:
    take the partition that holds the most bytes
    write its table as one partial chunk (the "base"), free it, mark the partition spilled
    from now on, chunks for it are appended to its file instead of merged
finish, for spilled partitions, as many at a time as memory allows:
    fits:         load the base, replay the chunks in seq order exactly as in 1.6, emit
    does not fit: split base and chunks by the next 6 hash bits into 64 files; repeat (3 levels)
output: every partition emits its groups sorted by `first`; the final order is a merge by `first`
```

The base is the fold of a prefix of chunks in seq order (the turnstile guarantees a prefix), and
the replay continues that fold in seq order. So every group sees the same sequence of merges as
it does in memory. `median` keeps raw values: one enormous group can still exceed memory, and the
query then fails with the error of 3.7.

#### 2.5 Hybrid hash join

```text
build:  as in 1.7. When the reservation cannot grow: write the chunks of the largest partition
        to its file, mark it spilled, append its later chunks there.
probe, no partition spilled: as in 1.7.
probe, some partitions spilled:
    a row whose partition is in memory is joined at once
    a row whose partition is spilled is appended, with its RowOrd, to that partition's probe file
    then, per spilled partition: load its build side (split by 6 more bits if it does not fit),
    read its probe chunks in seq order, join
```

Each of these streams is in probe order, but together they are not. If the consumer observes row
order, every stream is written as a run sorted by `RowOrd`, and the runs are merged with the
merge of 2.3. If it does not (the test of 1.7), rows are pushed as they come. This is the price
of the documented join order: when a join spills, its output is written once more. A key so
frequent that its partition never fits is joined by a block nested loop: build rows are loaded
one block at a time and the probe file is read once per block.

#### 2.6 Window and `collect`

A window holds three things, and each spills on its own: the kept input batches (written in seq
order), the partition rows (per radix partition, as in 2.5), and the results (runs sorted by
`RowOrd`). The output is the input read back in order, zipped with a merge of the result runs.
A collected table is a list of chunks, any of which the pool may move to disk; a scan of it reads
them back in order.

#### 2.7 Parquet

Per scan: read the footer once (as today), and the page index only when the scan has conditions.
Per condition, derive a pruning form when it is `column op literal`, `is_null(column)`,
`column in [...]`, or `and`, `or`, `not` of these, and the file's type maps to the declared type
exactly and monotonically (section 4.2). Evaluating that form on `(min, max, null count)` gives
`Never`, `Always` or `Maybe`. Then, per row group:

1. **Statistics.** Skip the row group if a condition is `Never`. A condition that is `Always` is
   dropped for this row group: it is not evaluated at all.
2. **Bloom filters**, for `==` and `in` on a column chunk that has one
   (`get_row_group_column_bloom_filter`, `Sbbf::check`).
3. **Page index.** The same evaluation per page gives a `RowSelection`.
4. **Late materialization.** The remaining conditions become a `RowFilter` of `ArrowPredicateFn`s,
   fixed-width columns first. The reader decodes the condition columns, and the other columns
   only for the rows that passed.
5. **Limit.** `with_limit` when every condition is evaluated by the reader.
6. **Streaming.** The row group is pushed batch by batch; it is never held whole.
7. **Strings** are read as views, and fully dictionary-encoded chunks as dictionaries (1.5).

Rules that keep this correct:

- A condition that `can_fail` is evaluated only on rows that passed the conditions written before
  it, and no condition written after it may skip data before it has run. Skipping would hide an
  error that the program would have raised.
- Float statistics follow the Parquet rules for `NaN` and signed zero; when in doubt the answer is
  `Maybe`. Truncated string statistics are used only on their safe side.
- A missing statistic is `Maybe`. Pruning never changes which rows pass, only what is decoded.

On q5 (0.063 s against 0.024 s for DuckDB and Polars) statistics will not help: the benchmark data
is in random order and the filter keeps about 60% of the rows. The expected gains there are the
dictionary read of `region`, typed group keys and the fused pipeline. A second Parquet benchmark,
sorted by date, is needed to measure pruning.

#### 2.8 CSV and JSON

**One decoder per format, written for static schemas.** The Arrow readers parse every field into
general builders. biggo knows the columns and their types before it opens the file.

- CSV, per morsel: (1) one scan with `memchr` for delimiter, quote and newline that records the
  start and length of the projected fields only; (2) one tight loop per column over those offsets
  (`int`, `float` through `lexical-core`, `date`, `decimal`, `bool`); (3) strings become views into
  the source bytes unless they contain a doubled quote; (4) the columns that the scan's conditions
  read are parsed first, and the others only for the rows that pass. The options of ROADMAP
  section 8 (`header`, `skip`, `nulls`, `date_format`, `decimal`, `on_error`) are parameters of
  these loops.
- JSON, per morsel: no tape and no tree. The decoder expects the declared keys in the order of the
  previous object and checks each with one comparison; a miss falls back to a lookup. Values of
  undeclared keys are skipped structurally. Strings without a backslash are views into the source.
- CSV text is the largest cost of q1, q3, q4, q6, q9 and q10 (docs/09: "CSV is limited by text
  conversion", about 900 MB/s on 8 cores). The decoder is expected to matter more for those
  queries than any operator change (estimate: 1.3 to 1.8 times on them).

**Splitting.**

- Quote parity stays: it is exact and parallel. Two changes. Cut points are computed one admission
  window ahead, inside `claim`, not for the whole file up front, so a `Limit` stops the reading.
  And the dialect becomes strict: a quote inside an unquoted field is an error that names the
  line, with `quote = ""` to turn quoting off for files that use `"` as data. Then parity is
  right for every file the decoder accepts.
- The limits that remain: one quote character, no backslash escapes, and a single row longer than
  the admission window's memory fails with a clear error.
- A JSON array (`[{...}, {...}]`) has no safe cut points without knowing string and nesting state.
  `claim` runs a sequential structural scan (quotes, backslashes, depth) that finds the top-level
  element boundaries and hands out morsels of whole objects; parsing stays parallel. The scan is
  the one sequential part (estimate: over 1 GB/s on one core).
- **Other encodings.** In every encoding of `encoding_rs` except UTF-16 and ISO-2022-JP, the bytes
  `0x0A` and `0x22` never occur inside a multi-byte character. So the splitter runs on the raw
  bytes, and each worker transcodes its own morsel. UTF-16 and ISO-2022-JP take the stream path
  of 2.9. Nothing is decoded whole any more.

#### 2.9 Inputs that cannot be split: compressed files, pipes, HTTP bodies

One reader thread (not a pool thread, because it blocks on I/O) decompresses or reads, cuts the
text into morsels at row ends with the same rule as 2.8 (it sees the bytes in order, so the quote
state is simply carried), and puts them in a queue of `W` blocks. `claim` pops from the queue;
parsing is parallel. The queue is the backpressure. Throughput is bounded by one core of
decompression, which is inherent in gzip and single-frame zstd. Formats that need seeking
(Parquet, Arrow IPC files) are first copied from a pipe to a temporary file.

Dependencies: `zstd` is already in the tree (through `parquet`); gzip needs `flate2` with its
pure-Rust backend (new).

#### 2.10 Many files

- `read_csv<T>("logs/2026-*.csv")` expands when the scan opens. **Files are ordered by the bytes
  of their paths**, never by directory order, and seq runs through them in that order. So row
  order is file order, then row order within the file.
- Every file is validated against the declared row type on its own; an error names the file.
- A path segment `key=value` (a partitioned directory) gives a constant column when the row type
  declares `key`. Conditions on such columns, and on the file-name column, are evaluated on the
  path with the evaluator of 2.7 (`min = max = value`), and files that cannot match are never
  opened.
- Files open lazily, a few ahead of the workers, and close after their last morsel. Parquet
  footers are fetched by that read-ahead, so 10,000 small files do not cost 10,000 sequential
  round trips.

#### 2.11 A table that is read twice

Two different cases, two mechanisms:

- **Inside one query** (a self-join, a `union` of two filters of one table): the physical planner
  finds subplans that are the same `Arc<Plan>` before optimization. If the shared part contains a
  breaker, or scans a text format, it runs once into a buffer that spills like `collect`, with
  the union of the columns and the `or` of the pushed conditions; each consumer re-applies its
  own. A shared Parquet scan is simply read twice: each side keeps its own pruning.
- **Across statements** (`let t = read_csv(...)`, then two queries on `t`): a **scan cache** owned
  by the process keeps fully decoded columns per `(file identity, morsel, column)`, where the
  identity is path, size, modification time and inode. A later scan takes cached columns instead
  of parsing. The cache is the first thing the memory pool evicts and is never spilled.

Automatic `collect` at compile time is rejected: two selective queries over a large file would
materialize all of it.

### Why this and not the alternatives

| Alternative | Why it loses here |
| --- | --- |
| Sort-based aggregation and sort-merge join when data is large | A full sort costs more than hash partitioning, and first-appearance or probe order must be restored afterwards anyway |
| A custom row-oriented spill format | More code to write and test. IPC needs no parsing on read, handles every type, is already compiled (through `parquet`), and doubles as the Arrow IPC connector |
| Parquet as the spill format | Pays encoding and compression for data that is read back once |
| Mapping spill files, or leaving it to the OS to swap | Memory that the pool cannot count, eviction it cannot steer, random reads |
| Guessing the quote state per chunk and verifying later | Needs a second pass or a retry path; parity is exact once the dialect is strict |
| Morsel sizes that adapt to time or memory | Breaks R1 |
| Decoding with the Arrow readers and filtering afterwards (today) | Parses fields that a condition is about to drop, and cannot skip pages |

### Keeping results reproducible

- Spilling moves chunks; it never changes their content, their seq, or the order in which a group
  or a merge consumes them (2.3 to 2.6). The memory limit is therefore invisible in results (R5).
- Pruning, late materialization and pushed conditions remove rows inside their morsel. Seq numbers
  of skipped row groups and files are simply unused (R1, R3).
- The reader thread of 2.9 cuts by the same rule as the mapped splitter, so a file and its
  compressed copy give the same morsels.
- File order is the byte order of paths. The scan cache returns the same arrays that decoding
  would produce.
- Written files are bit-identical for any thread count: writers format morsels in parallel and
  append them through the turnstile, and Parquet row groups are cut by row count.

### How to test and measure it

- **Limit matrix.** Every query of the determinism matrix also runs with the memory limit at 1/2,
  1/8 and 1/64 of its measured in-memory peak. The result hash must not change.
- **Splitter.** Extend `splits_at_row_boundaries_only`: random files with quoted line breaks, every
  target size, and decode-in-pieces equals decode-whole. The same for every supported encoding.
- **Pruning.** For random files and random conditions, the pruned scan equals the unpruned scan.
  Include files from other writers, with and without statistics, page indexes and Bloom filters,
  and float columns with `NaN` and `-0.0`.
- **Failure.** Kill the process during a spill and check that no file remains; fill the temporary
  directory and check the message.
- **Measure.** Add to `bench/`: a Parquet file sorted by date; 50 and 500 million row data sets
  run with a 2 GB limit; a `.csv.gz` copy; the TPC-H queries that ROADMAP section 12 lists.
  Report time, peak resident memory (already from `wait4`) and bytes spilled.

## 3. Memory management

### Today

- `biggo-exec` has no accounting, no limit and no temporary files.
- Memory is bounded only by structure: `ScanIter` holds one group of decoded pieces, `ParMap` one
  group of batches, `aggregate` 64 batches (`WINDOW`). Breakers hold everything (section 2).
- Sharing: batches are passed by reference count; `ops::limit` slices; `Shape::finish` selects
  columns without copying. Copies: `filter_record_batch`, `concat`, `take`.
- CSV and JSON files are mapped with `memmap2`. Their pages are file-backed but count as
  resident: docs/09 notes that most of the 270 MB peak of streaming queries is the mapped file.
  Parquet is read through `File`. A non-UTF-8 CSV is copied whole into anonymous memory.
- `convert::batch` builds a new Arrow schema for every batch it makes. The SQLite reader holds a
  `Vec<Scalar>` per column (24 bytes per value, and an allocation per string).
- Threads come from rayon's global pool (`RAYON_NUM_THREADS`).

### Proposed

#### 3.1 One pool, one reservation per operator

```rust
pub struct MemoryPool { /* limit, used: AtomicUsize, the registered consumers */ }

impl MemoryPool {
    /// An account for one operator, one collected table, or the scan cache.
    pub fn consumer(self: &Arc<Self>, name: String, spill: Option<Arc<dyn Spill>>) -> Reservation;
}

pub struct Reservation { /* pool, consumer, bytes */ }      // dropping it releases everything

impl Reservation {
    /// Asks for `bytes` more. Never blocks. On `false` the caller spills or fails.
    pub fn try_grow(&mut self, bytes: usize) -> bool;
    pub fn shrink(&mut self, bytes: usize);
    /// Counts the buffers of a batch that this consumer keeps. A buffer shared by several
    /// batches is counted once, and is released when its last owner drops it.
    pub fn claim(&self, batch: &RecordBatch);
}

pub trait Spill: Send + Sync {
    /// Frees at least `want` bytes if it can, and returns what it freed. The pool calls this on
    /// the requesting thread, and only for consumers that are idle.
    fn spill(&self, want: usize) -> Result<usize>;
}
```

- The pool belongs to the process, not to one query: collected tables and the scan cache outlive
  queries. The VM runs one query at a time, so "per query" is what is left after those.
- An operator that keeps data owns one `Reservation` and grows it in steps of at least 1 MiB (one
  atomic addition per step).
- **How an operator is told to spill.** `try_grow` fails only after the pool has (1) evicted the
  scan cache and (2) asked idle consumers to spill (collected tables, the runs of a finished
  sort). On `false` the operator spills itself (2.3 to 2.6) and asks again. If it cannot spill,
  the query fails (3.7).
- A running operator is never interrupted from outside. It spills at its own `try_grow`, at a
  point where its state is consistent. So no operator calls into another while both hold locks.
- **Morsels in flight** are not counted batch by batch. A pipeline reserves `W` times 8 MiB when it
  starts (an estimate, to be calibrated with the measurements of step 0). If the limit cannot
  cover that, `W` shrinks, down to 1.

#### 3.2 The default limit: 50% of physical memory

The limit is `BIGGO_MEMORY_LIMIT` or `--memory-limit` (ROADMAP section 11); otherwise half of the
physical memory, or half of the cgroup limit when one is set. Physical memory is read with a few
lines of platform code (`sysctl`, `/proc/meminfo` and `memory.max`, `GlobalMemoryStatusEx`).

Half, not the 80% that DuckDB uses, because the limit covers operator state only. The other half
is for what the pool does not count: the page cache that mapped inputs and spill files depend
on, the VM's own values, allocator slack, and temporaries inside one batch.

#### 3.3 Accounting

Every large allocation has exactly one accounting owner:

| Memory | Counted by |
| --- | --- |
| Hash tables, key arenas, accumulator columns, sort entries, partition buffers | The operator, exactly, when it grows them (`try_grow`) |
| Batches that a sink keeps | `Reservation::claim`, built on Arrow's `pool` feature: `RecordBatch::claim` attaches one reservation to each underlying buffer, a second claim replaces the first, and the reservation ends when the buffer is freed |
| Source blocks (decoded text, decompressed data, HTTP ranges, Parquet pages) | The in-flight allowance while a morsel runs; the claim of the batch that still points into them afterwards |
| Mapped files | Not counted against the limit (the OS can drop these pages); reported separately |

- Arrow's `get_array_memory_size` is not used for this: it counts a shared buffer once per array
  that refers to it, and a whole 2 MiB source block for one string view.
- Before a sink keeps a batch, it makes the batch worth keeping: a sliced fixed-width column that
  uses less than half of its buffer is copied, and a view column whose strings fill less than
  half of the blocks they pin is compacted (`gc`). Views into a mapped file are left alone: the
  file is their backing store, at no cost to the limit.
- Not counted: lists built by `to_rows`, allocator overhead, kernel temporaries.

#### 3.4 Backpressure

A morsel runs through its whole pipeline on one worker, so there are no queues between operators.
Three queues exist, and the admission window `W` bounds each: the reorder buffer of a turnstile,
the result queue to the VM, and the block queue of a reader thread (2.9). A slow sink (spilling,
or writing to a slow disk) keeps its seq unreleased; `low` stops; workers stop claiming; the scan
stops. A consumer that stops pulling (a REPL that printed 50 rows) leaves at most `W` morsels
decoded.

#### 3.5 Batch size and cache behavior

- **8,192 rows per batch** (today 32,768). A batch of five 8-byte columns is 320 KB instead of
  1.3 MB, so it and its intermediates fit in a 1 MB L2 cache. The M1 Pro of docs/09 has much larger
  caches, so the present value was never wrong there. The batch size is not visible in results
  (R5): the final value comes from measuring 4,096 to 32,768 on that machine and on an x86 server.
- Each worker keeps scratch buffers (hashes, masks, key bytes) across batches. Arrow schemas are
  built once per pipeline, not per batch.
- Hash tables are kept near cache size by the radix partitions (1.6, 1.7).
- `mimalloc` as the global allocator is worth one measurement. It is adopted only if q1 to q10
  gain 5% or more. Cost: C code in the build (as SQLite and zstd already are).

#### 3.6 Avoiding copies

- **Mapped input** stays for local, uncompressed CSV and JSON: string views point into the file,
  and the file itself is the "spill" of those strings. `advise_range` tells the OS to read ahead
  and, after a morsel that left no views behind, that the range is no longer needed. Everything
  else (pipes, compressed data, remote files) uses read blocks.
- **Views** make `filter`, `take`, the join's gather and the sort's gather copy 16 bytes per
  string.
- **Masks** replace one copy per condition (1.3). **Slices** and shared columns stay as today.
- **No `concat`.** Breakers keep chunks and gather once into output batches: one copy where sort
  makes two today.
- **Spill files** are written with ordinary buffered writes in chunks of 1 MiB or more, followed by
  a drop-behind hint (`F_NOCACHE` on macOS, `POSIX_FADV_DONTNEED` on Linux), so spilled data does
  not sit in memory a second time as dirty pages. Direct I/O is not used.

#### 3.7 At the limit

Order of events: evict the scan cache; spill idle consumers; the requesting operator spills
itself; shrink `W` to 1; fail. The error names the operator by its line in `explain`, with sizes:

```text
error: not enough memory: the limit is 16.0 GB (set by default: half of 32 GB)
  Sort: desc(revenue), date, customer_id
      holds 1.2 GB in memory and 41.3 GB on disk, and needs 64 MB more
  Join inner: product_id == product_id
      holds 14.6 GB in memory (a hash table in use cannot be spilled)
  temporary files: /var/tmp/biggo-51234 (41.3 GB used, 212 GB free)
  to raise the limit: biggo run --memory-limit 24GB, or BIGGO_MEMORY_LIMIT
```

A full disk gives the same shape of message with the directory, the bytes written and the bytes
free. What cannot spill: a hash table that a probe is using, the values of one enormous `median`
group, and rows already handed to the VM by `to_rows`.

#### 3.8 Temporary files

- **Where.** `BIGGO_TEMP_DIR`, else the system's temporary directory. On Linux, `/tmp` is skipped
  when `statfs` says it is `tmpfs` (memory), in favor of `/var/tmp`. Each process creates
  `biggo-<pid>-<random>/` with mode 0700, on first use.
- **Cleanup on error.** A spill file is owned by its operator. Returning an error, or unwinding
  from a panic caught at the pipeline boundary, drops the operator, which closes the file and
  releases its reservation.
- **Cleanup on a crash.** On Unix a spill file is unlinked right after it is opened, so the kernel
  frees its blocks when the process ends for any reason, including `kill -9`. On Windows the file
  is opened with `FILE_FLAG_DELETE_ON_CLOSE`, with the same effect. The empty directory of a
  crashed run is removed by the next run that spills (its pid is no longer alive).
- No new dependency: this is about 50 lines over `std::fs`.

### Why this and not the alternatives

| Alternative | Why it loses here |
| --- | --- |
| No accounting: let the OS swap or kill | The process dies without a message, or the machine thrashes |
| Summing `get_array_memory_size` over batches | Counts shared buffers and source blocks many times |
| An allocator that refuses allocations past the limit | Fails at places that cannot spill; most Rust code aborts when allocation fails |
| A pool that calls into running operators to make them spill | Lock ordering between operators, for no gain: pipelines run one at a time, so one operator grows at a time |
| A fixed share of the limit per operator | Wastes the share of operators that are not growing |
| A default of 80% of RAM | The pool does not count everything, and biggo depends on the page cache |
| Mapping every input | Only local uncompressed text can be mapped; a file truncated by another process raises a signal |
| Direct I/O for spill files | Alignment rules and platform differences; the hints give most of the benefit |

### Keeping results reproducible

- No per-morsel decision reads the state of the pool (R2). A failed `try_grow` leads to a spill,
  and a spill does not change results (section 2). So the default limit, which differs between
  machines, cannot change a result (R5).
- One thing does depend on the machine: whether a query runs out of memory at all. That is a
  failure of a resource, like a full disk, not a result. A query that succeeds gives the same
  result everywhere.

### How to test and measure it

- **Fault injection.** A test pool makes the N-th `try_grow` fail, for random N. Every spill path
  runs, and the result hash must equal the unlimited run.
- **Leaks.** After every query in the test suite: `used == 0` for query consumers, and no file in
  the temporary directory.
- **Accuracy.** For q1 to q11 and the large data sets of section 2, compare the pool's high-water
  mark with peak resident memory minus mapped files (`bench/run.py` already records the peak).
  Target: resident memory stays under the limit plus 25% (a target, not a measurement).
- **Messages.** Golden tests for the two errors of 3.7.
- **Batch size.** The sweep of 3.5, on two machines.

## 4. Data sources

### Today

- `Format` (`plan.rs`) is `Csv`, `Parquet`, `Json`, `Sqlite`. `Scan` carries the format, the path,
  the query (database only), the CSV options, the declared schema, the output schema, the pushed
  filters and the limit. `scan::scan` matches on the format and calls `csv_pieces`,
  `parquet_pieces`, `json_pieces` or `sqlite_pieces`.
- `Shape` is the shared back half. `Shape::new` works out the columns to read. `Shape::finish`
  casts each column to its declared type, checks nullability, applies the filters and projects.
  A missing column is reported with the columns the file has (`Shape::missing_column`).
- **Pushdown.** The optimizer puts filters, projection and limit into `Scan`. Projection is used by
  every file reader (CSV does not convert the fields, Parquet does not read the columns). Filters
  run after decoding, in `Shape::finish`. The limit stops `ScanIter` from decoding more pieces.
- **Errors in data** are made by rewriting the text of Arrow's messages (`csv_error`,
  `json_error`, `quoted_value`). CSV errors give the line and the column, except decimals (the
  Arrow message has no line). JSON errors give the field but no line. SQLite errors give the
  column but no row.
- **SQLite.** `read_sql<T>(path, query)` runs the query as written on one connection and converts
  value by value (`sql_value`).
- **Writers** are four functions in `lib.rs`. `write_csv`, `write_json` and `write_parquet` open
  the target with `File::create`, which truncates it, and write on one thread. `write_sqlite`
  collects the result, then drops, creates and fills the table inside one transaction, row by
  row through `convert::scalar_at`.
- **Not there.** Other databases, Excel, Arrow IPC, URLs, standard input and output, compression,
  several files in one call.

### Proposed

#### 4.1 One connector abstraction

```rust
/// What the plan wants from a source.
pub struct ScanRequest<'a> {
    pub declared: &'a Schema,        // the row type written in read_xxx<T>
    pub columns: &'a [usize],        // the declared columns to produce
    pub filters: &'a [Expr],         // conditions over declared columns, in program order
    pub limit: Option<usize>,        // rows wanted after the filters
}

/// What a source does with one condition.
pub enum Pushed {
    Exact,      // every row it returns passes: the engine drops the condition
    Inexact,    // it uses the condition to read less, but rows may fail: the engine checks again
    No,
}

pub struct ScanPlan {
    pub filters: Vec<Pushed>,        // one answer per requested condition
    pub limit: bool,                 // the source stops after `limit` rows by itself
    pub statistics: Statistics,      // rows and bytes (exact or estimated); min, max, nulls per column
    pub physical: SchemaRef,         // the Arrow types it emits (views or dictionaries for strings)
}

pub trait Connector: Send + Sync {
    /// Reads metadata only: a header line, a footer, a catalog. Checks the declared row type
    /// against the source, and fails here, before any row, if they cannot agree.
    fn open(&self, ctx: &Context, at: &Location, request: &ScanRequest)
        -> Result<(ScanPlan, Arc<dyn Source>)>;            // `Source` as in section 1.1
    /// Starts a write, or says that this source cannot be written.
    fn create(&self, ctx: &Context, at: &Location, schema: &Schema, mode: WriteMode)
        -> Result<Arc<dyn Writer>>;
}

pub trait Writer: Send + Sync {
    fn encode(&self, batch: &RecordBatch) -> Result<Encoded>;   // on workers, in parallel
    fn append(&self, part: Encoded) -> Result<()>;              // in seq order, through the turnstile
    /// Makes the result visible, atomically. A writer dropped without `commit` leaves the
    /// target as it was.
    fn commit(&self) -> Result<()>;
}

/// Where bytes come from, apart from what they mean.
pub trait ByteSource: Send + Sync {
    fn identity(&self) -> Identity;                              // size, and mtime or ETag
    fn mapped(&self) -> Option<&[u8]>;                           // a local file: zero-copy
    fn read_ranges(&self, ranges: &[Range<u64>]) -> Result<Vec<Bytes>>;   // files, HTTP, S3
    fn stream(&self) -> Result<Box<dyn Read + Send>>;            // pipes, compressed bodies
}
```

- **Formats over transports.** CSV, JSON, Parquet, Arrow IPC and Excel are connectors over a
  `ByteSource` (local file, standard input, HTTP, S3). Databases are connectors of their own.
- **No plugins.** `biggo-plan` holds data only (`SourceKind`, a `Location`, options). `biggo-exec`
  maps the kind to a connector with a `match`. Everything stays in one executable (principle 5).
- **Validation** is one shared routine, called by every `open`. The connector lists the source's
  columns and types; the routine matches declared columns by name and looks each pair up in the
  connector's type table (4.2). It reports a missing column with the columns that exist, and a
  type that cannot be read with both types. The row type stays the only schema a program sees
  (principle 1): nothing is inferred at run time, and hidden columns such as `first` never leave
  the engine.
- **Splits** are the morsels of the `Source` (the table in 1.1).
- **Statistics** feed the build-side choice (1.7), table sizing, and `explain`.

#### 4.2 How each source fits

| Source | Parallel reading | Projection, filter, limit | Statistics | Writing |
| --- | --- | --- | --- | --- |
| CSV | 2 MiB morsels; stream path when not seekable | Unread fields are not parsed. Conditions are evaluated in the decoder: `Exact`. Limit stops `claim` | Rows estimated from size and a sampled row length | Morsels formatted in parallel, appended in order |
| JSON Lines, JSON array | As CSV; an array needs the sequential boundary scan | As CSV | As CSV | JSON Lines |
| Parquet | Row groups | Columns not read; conditions `Exact` (statistics, Bloom filters and pages are steps inside the reader); limit | Exact rows; min, max, nulls from the footer | Row groups of 131,072 rows; the columns of a group encoded in parallel |
| Arrow IPC | Record batches (file format); stream path otherwise | Columns not decoded; no conditions; limit | Exact rows (file format) | Yes |
| Excel | One stream | Columns skipped; `sheet`, `range`; limit | None | Later |
| SQLite | Table form: rowid ranges on several read-only connections. Query form: one stream | Table form: all three as SQL. Query form: wrapped as a subquery | `sqlite_stat1` when present | One transaction, values bound from arrays, `mode = "append"` |
| PostgreSQL | Table form: ranges of an integer key, one connection each, one shared snapshot. Query form: one `COPY` stream, decoded in parallel | As SQL | `pg_class.reltuples`, `pg_stats` | `COPY FROM` binary; replace inside a transaction |
| MySQL | One stream, decoded in parallel (connections cannot share a snapshot) | As SQL | `information_schema.tables` | Multi-row inserts; replace by an atomic `RENAME TABLE` swap |
| Standard input and output | Stream path | By the format | None | Streamed in order; no replace |

**Types.** A pair that is not in this table is refused at `open`, with the column, its type in the
source, and the declared type.

| biggo | Parquet, Arrow IPC | PostgreSQL | MySQL | SQLite (as today) |
| --- | --- | --- | --- | --- |
| `int` | Int8 to Int64, UInt8 to UInt32; UInt64 with a range check | `int2`, `int4`, `int8` | `TINYINT` to `BIGINT`; unsigned `BIGINT` with a range check | INTEGER; numeric text |
| `float` | Float32, Float64 | `float4`, `float8`, `numeric` | `FLOAT`, `DOUBLE` | REAL, INTEGER; numeric text |
| `decimal` | Decimal with scale up to 6 | `numeric`, each value checked for 6 decimals | `DECIMAL`, same check | INTEGER, REAL, text |
| `bool` | Boolean | `bool` | `TINYINT(1)`, `BIT(1)` | 0 and 1; `true`, `false` |
| `string` | Utf8, LargeUtf8, Utf8View, and dictionaries of them | `text`, `varchar`, `char`, `uuid`, `json`, `jsonb`, enums | `CHAR`, `VARCHAR`, `TEXT`, `ENUM`, `JSON` | TEXT; numbers as text |
| `date` | Date32, Date64 | `date` | `DATE` | ISO text |
| `datetime` | Timestamp of any unit; a zone is converted to UTC and dropped (as today) | `timestamp`, `timestamptz` (to UTC) | `DATETIME`, `TIMESTAMP` | ISO text |
| `duration` | Duration of any unit | `interval` without months | `TIME` | A number of seconds |

Parquet pruning (2.7) uses a condition only when the pair is exact and keeps order: integers,
floats to `float`, dates, timestamps (the literal is converted to the file's unit), decimals of
the same scale, strings.

**Databases.**

- **Crates.** `rusqlite` (present). PostgreSQL: the `postgres` crate, a blocking API over
  `tokio-postgres` that keeps its runtime private, so no async code appears in the engine, but
  tokio is compiled in. MySQL: the `mysql` crate, which is blocking throughout. TLS for both, and
  for HTTP, through `rustls`. Estimated cost: 2 to 3 MB of binary (22.7 MB today). Versions and
  feature sets could not be checked while writing this and must be verified.
- **Two forms.** The *table form* names a table: biggo writes the SQL, so it can project, filter,
  limit, order and split. The *query form* (today's `read_sql<T>(path, query)`) is run as one
  stream. For the query form the projection, the conditions and the limit are pushed by wrapping:
  `SELECT a, b FROM (<query>) AS q WHERE ... LIMIT n`. MySQL may discard an `ORDER BY` inside
  such a subquery, so there the wrapping is skipped when the query contains one.
- **SQL generation** is one generator with a small dialect interface:

```rust
pub trait Dialect {
    fn ident(&self, name: &str) -> String;                    // "x" or `x`
    fn literal(&self, value: &Scalar) -> String;
    /// SQL for one condition, and whether the database's answer is exact.
    fn condition(&self, expr: &Expr) -> Option<(String, Pushed)>;
}
```

  A pushed condition must select a superset of the rows that biggo's own evaluation selects.
  Where the meaning is identical it is `Exact`; where it is widened it is `Inexact` and checked
  again; where not even a superset is certain it is not pushed.
  - Integers, booleans, and dates and datetimes in native types: `Exact`.
  - Strings: compared under a byte-wise collation (`COLLATE "C"`, `utf8mb4_bin`), and still marked
    `Inexact`. MySQL's default collations ignore case, and biggo compares bytes.
  - Floats: SQL treats `-0.0` and `0.0` as equal, biggo's total order does not. A strict
    comparison is pushed as its non-strict form (`x < c` as `x <= c`): `Inexact`.
  - SQLite dates stored as text: the bound is widened by one day: `Inexact`.
  - Functions and arithmetic are not pushed in the first version.
  - A limit is pushed only when every condition is `Exact` or absent.
  - Literals are bound as parameters where the protocol allows it. `COPY` takes none, so there the
    generator writes them, with `standard_conforming_strings` on and quotes doubled. Only values
    of the eight biggo types can appear.
- **Bulk reads from PostgreSQL** use `COPY (SELECT ...) TO STDOUT (FORMAT binary)`. The reader
  thread cuts the stream into blocks of whole rows (it only walks the length fields); workers
  decode them: big-endian integers and floats, text as views, dates as days and timestamps as
  microseconds since 2000-01-01 (shifted to 1970), `numeric` from its base-10,000 digits. The
  special values `infinity` are data errors.
- **Parallel reads by key range.** For the table form with an integer primary key: read `min`,
  `max` and the row estimate; cut ranges of about 100,000 rows; range `i` is morsel `i`, run as
  `WHERE k >= a AND k < b ORDER BY k` on one of a few connections (4 by default). On PostgreSQL
  the connections share one snapshot (`pg_export_snapshot`, `SET TRANSACTION SNAPSHOT`), so the
  read is consistent while the table changes. SQLite does the same with rowid ranges.
- **Credentials.** A password is never written in a program: it comes from `env(...)` (ROADMAP
  section 8) or from the usual variables and files (`PGPASSWORD`, `~/.pgpass`, `MYSQL_PWD`). A
  `Location` keeps the secret apart from the text that is shown; `explain`, errors and plans
  print `postgres://user:***@host/db`. An executable made by `biggo build` holds source only.
- **TLS.** `rustls` with the system's root certificates and a bundled set as fallback. For any
  host that is not the local machine, the certificate and the host name are verified by default;
  turning that off has to be written in the URL.

**HTTP(S) and S3-style object stores.**

- **Client.** `ureq` (blocking) on `rustls`. S3 requests are signed in-house (SigV4 on `sha2` and
  `hmac`). Credentials come from `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`,
  `AWS_REGION` and `~/.aws/credentials`; `AWS_ENDPOINT_URL` selects a compatible store. Instance
  metadata and single sign-on are left out of the first version.
- **Ranges.** `read_ranges` merges ranges less than 1 MiB apart, splits ranges over 8 MiB, and
  fetches them in parallel on an I/O pool of 16 threads, separate from the rayon pool.
- **Parquet** asks for the last 64 KiB first (the footer, and usually the page index), then per
  row group only the column chunks or pages that survive 2.7. A `ChunkReader` serves the decoder
  from those ranges.
- **CSV and JSON** fetch ranges in parallel, hand them in order to the reader thread of 2.9, and
  parse in parallel.
- **Retries.** A read is retried on a failed connection, a timeout, a short body, and status 408,
  429, 500, 502, 503 and 504: up to 5 attempts, waiting 100 ms doubling to 10 s with random
  jitter, honoring `Retry-After`, and resuming a body from the byte it stopped at. Every request
  after the first carries `If-Match` with the first ETag. If the object changed, the query fails:
  it never mixes two versions of a file.
- **Synchronous, not async.** The engine is CPU-bound and runs morsels on rayon. An object store
  needs tens of concurrent requests, which 16 blocked threads provide. An async runtime would add
  a second scheduler, split `Source` into two kinds of function, and enlarge the binary, to
  serve thousands of concurrent requests that one machine reading analytics data never issues.
- **Writing.** To S3: write a temporary local file, then one `PUT` or a multipart upload; both
  replace the object atomically. Plain HTTP is read-only.

**Standard input and output.** The path `-` selects them (ROADMAP section 8). Input is the stream
path of 2.9. It can be read once: a second scan of it in one query is served by the shared buffer
of 2.11, and in a later statement is an error that suggests `collect`. Output is streamed in
order and has no atomic replace. A closed pipe cancels the query quietly.

**Excel.** The `calamine` crate reads `.xlsx` (and `.xls`, `.ods`) in pure Rust; `zip` and
`quick-xml` come with it. A sheet is one compressed XML stream, so it is read on one thread and
handed over in morsels of 65,536 rows. Cells are typed one by one: a number in a column declared
`int` must be whole, a date is a number with a date format, an empty cell is null, an error cell
(`#N/A`) is a data error. Sheets are at most 1,048,576 rows, so this path does not need to scale.

**Arrow IPC.** Read and written with `arrow-ipc`, which is already compiled. The file format has
a footer with the position of every record batch: batches are morsels, and an uncompressed local
file is decoded in place from the mapping, without copying. The stream format takes the stream
path. The spill files of 2.2 use the same code.

#### 4.3 Errors in data, reported one way

```rust
pub struct DataError {
    pub source: Arc<str>,            // the path or the redacted URL, as the program wrote it
    pub place: Place,
    pub column: Option<Arc<str>>,
    pub value: Option<String>,       // the text that did not fit, cut at 60 characters
    pub expected: Option<ColType>,
    pub kind: DataErrorKind,         // BadValue, Missing, FieldCount, Encoding, Changed, ...
}

pub enum Place {
    Line(u64),                                   // CSV, JSON Lines
    Element(u64),                                // JSON array
    Row { group: Option<usize>, row: u64 },      // Parquet, Arrow IPC, databases
    Cell { sheet: Arc<str>, cell: String },      // Excel: "C17"
}
```

- One function renders it, in today's CSV wording, so existing golden files do not change:
  ``sales.csv, line 13: cannot read 'north' as an int for column `region` ``. JSON gains the line,
  Parquet and databases gain the row, Excel names the cell.
- Decoders build a `DataError` themselves. Nothing parses the text of Arrow's messages any more.
- A decoder knows the row within its morsel. The absolute line is computed only when an error
  occurs, by counting line breaks before the morsel, as the closure in `csv_pieces` does today.
- The error reported is the first in row order (R6).
- `on_error = "null"` (ROADMAP section 8) is a policy passed to the decoder. The number of values
  replaced is a commutative count, and the examples shown are the first in seq order, so the
  report is reproducible too.

#### 4.4 Writing files with atomic replace

A file writer creates `.<name>.biggo-<pid>-<n>.tmp` in the directory of the target (the same file
system, so `rename` is atomic), writes, calls `fsync`, copies the permission bits of an existing
target, and renames. A writer that is dropped removes its temporary file. Two consequences:

- A query that fails leaves the old file untouched.
- A query may write the file it reads: the old file stays open and mapped until the scan ends.

`append = true` (ROADMAP section 8) is not atomic and is documented as such.

### Why this and not the alternatives

| Alternative | Why it loses here |
| --- | --- |
| The `object_store` crate with tokio and the async Parquet reader | Complete, but brings an async runtime and several MB (estimate) for tens of concurrent requests |
| ODBC, ADBC, or the databases' C client libraries | Shared libraries on the user's machine: no longer one self-contained executable |
| A PostgreSQL protocol client written here | About 2,000 lines (estimate) with SCRAM and TLS, and security-sensitive. Worth it only if tokio's weight is unacceptable (open question) |
| Pushdown as all or nothing | Loses every case where a source can only narrow: statistics, collations, widened float bounds |
| The engine checks every condition again, always | Simple, but repeats work where the source is exact |
| Loading connectors as plugins | Against principle 5 |
| Truncating the target in place (today) | Loses data when the query fails or reads the same file (see the findings) |

### Keeping results reproducible

- Every source defines its row order, and seq follows it: the file; the sorted list of files; the
  key order of a table read in ranges; the order in which a database returns a query's rows.
- The last one is the weak point. A query without `ORDER BY` is only as reproducible as the
  database makes it. The table form always orders by the key. Parallel ranges read one snapshot
  on PostgreSQL; on MySQL they are off by default for that reason.
- Remote files are pinned to one ETag. Retries and their jitter change timing only.
- Pushed conditions never change which rows pass (the superset rule and the second check).
- Written files are bit-identical for any thread count (section 2).

### How to test and measure it

- **One conformance suite, run against every connector:** a round trip of every type with nulls
  and extreme values; every refusal at `open`, with its message; row order; parallel equals
  single-threaded.
- **Pushdown equivalence.** For random conditions and data, the result with pushdown equals the
  result with pushdown disabled, per connector and per dialect. PostgreSQL and MySQL run in
  containers in CI (ROADMAP section 11); SQLite runs in process.
- **A local HTTP server that misbehaves on purpose:** short bodies, 503 with `Retry-After`,
  stalls, an object that changes between requests.
- **Atomic replace.** Kill the process during a write; write a file that the query reads; make a
  query fail after half the output. The target is the old file or the new one, never a part.
- **Messages.** Golden tests for `DataError` per source.
- **Measure.** q11 (SQLite: 0.339 s, no gain from more cores, docs/09) before and after; the same
  query against PostgreSQL and MySQL with 1 million rows; Parquet over a local S3-compatible
  store; `write_parquet`, `write_json` and `write_sql` (0.88 s, 1.39 s and 0.84 s in docs/09).

<!-- next-section -->
