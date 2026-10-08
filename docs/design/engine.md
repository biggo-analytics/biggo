# Engine design: execution, large data, memory and data sources

This document proposes the next generation of the execution engine (`crates/biggo-exec`), in four
areas: in-memory and parallel execution, data larger than memory, memory management, and data
sources. Nothing in it is built yet.

- **Today** describes the code at commit `561a4e3` (9 October 2026), from reading it. "Observed"
  means that an existing binary was run to confirm the statement.
- Timings are quoted from [docs/09-performance.md](../09-performance.md) (5 million rows, Apple M1
  Pro, 8 October 2026). Every other figure about speed or size is an estimate, and says so.
- Each area has five parts: today, proposed, why this and not the alternatives, keeping results
  reproducible, how to test and measure it.
- Out of scope: the bytecode VM, reordering of joins, running on several machines.

| Term | Meaning |
| --- | --- |
| batch | An Arrow `RecordBatch` of a bounded number of rows, plus an optional mask of selected rows |
| morsel | The unit of scheduling: the rows of one split of a source (2 MiB of CSV text, one Parquet row group, ...). One worker takes a morsel from the source to the sink |
| pipeline | A source, a chain of streaming operators, and a sink |
| breaker | An operator that needs all its input before it produces output: aggregate, sort, join build, window. It is the sink of one pipeline and the source of the next |
| seq | The number of a morsel in its pipeline. Morsel order is row order |
| row ordinal | The position of a row, `(seq, row within the morsel)`, packed in a `u64` |

The decisions at a glance:

| Problem | Decision |
| --- | --- |
| Execution model | Push-based pipelines, driven by morsels, on the existing rayon pool |
| Row order | Every morsel has a seq. Order-sensitive steps combine morsels in seq order, or restore order from row ordinals |
| Float sums | Fixed order: per-morsel sums, folded in seq order. Morsel boundaries are part of the language's behavior; nothing else is |
| Filters | A mask on the batch, compacted late; never one copy per condition |
| Strings | `Utf8View` arrays that point into the source bytes; dictionaries only where Parquet provides them |
| Group and join keys | Three layouts chosen at plan time: one fixed-width key, one string key, packed rows |
| Aggregation | Per-morsel pre-aggregation, 64 radix partitions, ordered merge per partition |
| Join | Partitioned parallel build, Bloom filter, build side chosen from statistics where row order allows |
| Sort | Normalized keys (kept), no `concat`, sorted runs, heap-based top-n |
| Larger than RAM | External merge sort; partition spilling for aggregate, join and window; Arrow IPC spill files |
| Memory | One pool, default limit 50% of RAM, a reservation per operator, spill, then a precise error |
| Sources | One `Connector` trait: validate, plan splits, negotiate pushdown per condition, read, write with atomic replace |
| I/O | Synchronous, on a small I/O pool of its own; no async runtime in the engine |

## 1. In-memory processing and parallel execution

### Today

| Part | Where | How it works today |
| --- | --- | --- |
| Model | `execute` (`lib.rs`), `ops::par_map` | Pull: `execute` returns a `BatchIter`, a boxed iterator of `Result<RecordBatch>`. Filter, project, unpivot, explode and the join probe go through `ParMap::next`, which pulls `rayon::current_num_threads()` batches, maps them with `into_par_iter`, and yields them in input order. Each operator is its own fork-join step: a batch waits at one barrier per operator and can change core between operators |
| Breakers | `ops::sort`, `aggregate::aggregate`, `join::join`, `window::window` | They run to completion inside `execute`. Sort, window and the join build start with `ops::concat`, which copies all batches into one |
| Scan | `scan::scan`, `ScanIter::next` | A file is cut into `Piece`s, closures that return `Vec<RecordBatch>`: 2 MiB of CSV or JSON text (`PIECE_BYTES`), a Parquet row group, or a whole SQLite result. `current_num_threads()` pieces are decoded at a time, in batches of 32,768 rows (`BATCH_ROWS`) |
| Filters | `ops::filter`, `Shape::finish` | `filter_record_batch` copies every column, once per pushed condition in a scan |
| Expressions | `expr::eval`, `eval_where` | The tree is walked for every batch: columns are found by name, literal arrays rebuilt, single values kept as `Col::Scalar`. An operand that `can_fail` is evaluated by filtering the batch and scattering back. Nothing shares common subexpressions; `merge_projects` only avoids duplicating a computed column that is used twice. Strings are `Utf8`, never dictionaries; `expr::strings` and `text.rs` allocate per row |
| Aggregate | `aggregate.rs` | 64 batches are read at a time (`WINDOW`), and each run of 4 (`RUN`) becomes a `State` on its own core. Keys always go through `arrow::row::RowConverter` and `Groups::intern`. All run states are kept until the input ends, then merged in run order: on one thread below 50,000 groups, otherwise by `merge_partitioned` (hash into `current_num_threads()` parts; first-appearance order restored by sorting `(run, group id)`). With many groups each key is inserted about twice and pre-aggregation removes little: in q2 a run of 131,072 rows meets about 123,000 of the 1 million customers (arithmetic, not measured) |
| Join | `join.rs`: `Table::build`, `probe` | The right side is concatenated and built on one thread: row-format keys, a `HashTable<u32>` of chain heads, a `next` chain per row. The probe runs under `par_map`, row by row, and gathers both sides with `take`. Nothing chooses the build side |
| Sort | `ops::sort`, `sort_indices_from` | After `concat`. Without `fetch`, from 16,384 rows: normalized keys are encoded in parallel, and `(u64, u64, u32)` entries (16 key bytes, row number) are sorted with `par_sort_unstable_by`, ties by row number. With `fetch`: `lexsort_to_indices(.., fetch)`, a partial sort, but over the concatenated table (q4 peaks at 387 MB) |
| Window | `window::window`, `compute` | After `concat`: all rows are sorted by partition keys then order keys, each function is computed on its own core, and results are put back with `take` |
| distinct | `Acc::DistinctFixed`, `Acc::Distinct` | `distinct()` is an aggregate with no aggregates (type checker). `count_distinct` keeps every value of every group, sorted and deduplicated at the end, or a hash set per group |
| Order | | `par_map` keeps input order, pieces and runs have fixed sizes, partial aggregates merge in run order, sorts end with the row number. So results do not depend on the thread count, as `bench/run.py` checks. Errors are the exception (see [the findings](#findings-in-todays-code)) |
| Limits | | Row numbers are `u32` in sort, join and window. `concat` builds `Utf8` arrays with 32-bit offsets: a string column over 2 GiB makes sort, window and the join build fail |

### Proposed

#### 1.1 Execution model: push-based pipelines driven by morsels

The optimized `Plan` is cut at its breakers into pipelines. Pipelines run one after another, in
dependency order (a join's build before its probe); each uses every core.

```rust
pub struct Batch {
    pub columns: RecordBatch,          // at most BATCH_ROWS rows
    pub keep: Option<BooleanBuffer>,   // rows still selected; None means all
}
pub enum Flow { More, Done }           // Done: this consumer needs no more rows

pub trait Source: Send + Sync {
    /// The next morsel, in row order. Called under the dispatcher's lock, so a source that
    /// reads a stream does its sequential I/O here. `None` ends the input.
    fn claim(&self) -> Result<Option<MorselInput>>;
    /// Decodes one morsel on a worker and pushes its batches in row order.
    fn decode(&self, input: MorselInput, out: &mut dyn FnMut(Batch) -> Result<Flow>) -> Result<()>;
}
pub trait Operator: Send + Sync {
    fn push(&self, batch: Batch, out: &mut dyn FnMut(Batch) -> Result<Flow>) -> Result<Flow>;
}
pub trait Sink: Send + Sync {
    fn start(&self, seq: Seq) -> Box<dyn MorselState>;
    fn consume(&self, state: &mut dyn MorselState, batch: Batch) -> Result<Flow>;
    fn finish_morsel(&self, seq: Seq, state: Box<dyn MorselState>) -> Result<()>;   // any order
    fn finish(self: Arc<Self>) -> Result<Arc<dyn Source>>;    // the source of the next pipeline
}
```

```text
run(pipeline):
    next = 0                 # next seq to hand out
    low  = 0                 # smallest seq the sink has not released yet
    on every thread of the pool, loop:
        under the dispatcher's lock:
            wait while next >= low + W                 # admission window, W = 2 * threads
            stop if the sink is Done, the source is empty, or a lower seq has failed
            input = source.claim(); seq = next; next += 1
        state = sink.start(seq)
        source.decode(input, |b| operators.push(b, |b| sink.consume(state, b)))
        sink.finish_morsel(seq, state)                 # the sink advances `low` on release
    sink.finish()                                      # may use the whole pool again
```

- A morsel stays on one core from decoding to the sink, while its batches are in that core's
  cache. There is no barrier and no queue between operators.
- A worker claims a morsel when it finishes one, so a slow core takes fewer morsels. Today a
  group of pieces waits for its slowest member, one reason why 4 to 8 threads gains only 1.15 to
  1.3 times on a machine with two efficiency cores (docs/09).
- The outside interface stays `execute(plan) -> BatchIter`: the last sink is an ordered collector
  with a bounded queue that the iterator pops. Dropping the iterator sets `Done`.

| Source | One morsel |
| --- | --- |
| CSV, JSON Lines (any transport, compressed or not) | 2 MiB of text, cut at the first row end at or after each multiple of 2 MiB (today's rule) |
| Parquet; Arrow IPC | One row group; one record batch |
| Row streams (databases, Excel) | 65,536 rows |
| In-memory table, output of a breaker | 131,072 rows, by position |

#### 1.2 How order is tracked

```rust
pub struct Seq(pub u32);       // number of a morsel in its pipeline; morsel order is row order
pub struct RowOrd(pub u64);    // (seq, row in the morsel's output): 24 + 40 bits, both checked
```

Inside a morsel everything is sequential, so rows keep their order. Across morsels there are four
mechanisms, and each operator uses one:

1. **Turnstile.** A sink whose combining step is order-sensitive parks the result of a finished
   morsel in a reorder buffer and releases results in seq order. The worker does not block: it
   leaves the result and claims the next morsel. The admission window bounds the buffer at `W`
   morsels. Used by the ordered collector, file writers and aggregation.
2. **Gate.** `Limit` needs the row count before a morsel. The worker holds the morsel's batches at
   the gate until every lower seq has declared its count, slices, and continues. Once the limit
   is reached the gate answers `Done`, and higher seqs are discarded.
3. **Ordinal recovery.** Sinks that scatter rows (sort, join build, window, raw aggregation
   chunks) attach a `RowOrd` and restore order when they finish.
4. **Ordered errors.** An error is an event at a `(seq, row)`. The query fails with the error of
   the lowest seq that the consumer would have reached: lower seqs run on (they may fail earlier),
   higher seqs are cancelled, and an error beyond the point where a `Limit` stopped is dropped.

#### 1.3 Selection masks instead of filtered copies

`Filter` evaluates its conditions into one mask, `Batch::keep`, and copies nothing.

- Conditions run in program order. One that cannot fail runs on all rows of the batch. One that
  `can_fail` runs only on compacted rows: rows that earlier conditions dropped must not raise its
  error. The expression compiler inserts that compaction.
- A batch is compacted when its mask keeps less than half of the rows, and always before a sink
  stores it: one prepared `FilterPredicate` for all columns that the rest of the pipeline reads.
  Sinks read through the mask without compacting.
- Index vectors (as in DuckDB) are not used: Arrow kernels take whole arrays, masks combine with
  one `and`, and the filter kernels take masks.

#### 1.4 Expression evaluation

All expressions of one stage (a filter's conditions, a projection's columns, an aggregate's keys
and arguments) are compiled once into one register program:

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

- **Constant folding** stays in `optimize::simplify`; the program stops rebuilding literals.
- **Common subexpressions.** Compilation hash-conses on `(ExprKind, type, guard)`. The guard is
  the mask of rows on which a branch of `if`, `and`, `or` or `??` may run. An expression that
  cannot fail has no guard and is shared everywhere; one that can fail is shared only under the
  same guard. The optimizer may then merge projections freely: q1's `revenue`, used by `sum` and
  `mean`, is computed once without a projection of its own.
- **Guards replace `eval_where`:** a guarded step compacts only the columns it reads. Registers
  are freed after their last use.
- **No JIT.** Kernels over thousands of rows amortize the interpretation; a compiler backend would
  cost start-up time (5 ms today) and a large dependency.

#### 1.5 Strings: views and dictionaries

- `string` columns become `Utf8View`. A view is 16 bytes: the length, and either the string (up
  to 12 bytes) or a 4-byte prefix with a buffer number and an offset. Arrow 60 supports views in
  the CSV, JSON and Parquet readers, the row format, comparisons, `concat` and `cast`.
- Views point into the source bytes (a mapped CSV file, a decompressed block, a Parquet page), so
  a string that needs no unescaping is not copied. `filter`, `take` and the gather of a sort move
  16 bytes per string. An array is no longer limited to 2 GiB. Equality and hashing of strings up
  to 12 bytes work on one `u128`.
- **Dictionaries** are kept only where they exist: a Parquet column chunk whose pages are all
  dictionary-encoded is read as `Dictionary(Int32, Utf8View)`. Conditions against a constant and
  unary functions run once per dictionary entry; group and join keys translate codes through a
  table built once per morsel; any other operator gets views (the compiler inserts the cast).
  CSV and JSON are not dictionary-encoded on read: that costs a hash per value, and short strings
  are already cheap as views.

#### 1.6 Hash aggregation

**Key layouts**, chosen at plan time from the static key types:

| Layout | When | Key in the table |
| --- | --- | --- |
| `Fixed` | One key of `int`, `date`, `datetime`, `duration`, `bool` or `float` | `u64`: the value's bits; a separate slot for the null group |
| `Str` | One `string` key | The 16-byte view; a long string is copied to the table's arena and compared by prefix, then bytes |
| `Rows` | Anything else | Packed rows: a null bitmap, fixed-width values, then length and bytes per string. Byte equality is key equality |

`Rows` replaces `arrow::row` here: that format preserves order, which grouping does not need, and
costs more per string. `Fixed` compares the bits that the row format encodes, so the groups are
those of today (`0.0` and `-0.0` stay different keys).

**Algorithm.** The radix of a group is the top 6 bits of its hash: 64 partitions, always.

```text
per morsel m, on one worker:
    probe: count the distinct keys among the first 4,096 rows
           more than 1,024 -> mode Raw for the whole morsel, else mode PreAggregate
    mode PreAggregate: id = table.intern(key), remembering the row of first sight
                       accumulators.update(id, arguments)        # table is local to the morsel
    mode Raw:          append (key, arguments, row number) to buffer[radix(hash)]
    hand 64 chunks (m, partition, Partial or Raw) to the sink

sink, per partition p, chunks strictly in seq order (turnstile, one lock per partition):
    Partial chunk: id = global[p].intern(key); accumulators.merge(id, state)
    Raw chunk:     an order-sensitive accumulator is present -> pre-aggregate the chunk on its
                       own, then merge it as a Partial chunk
                   otherwise -> id = global[p].intern(key); accumulators.update(id, arguments)

finish: each partition gives its groups with `first`, the RowOrd of the group's first row;
        the output is the groups sorted by `first` (first-appearance order, as today)
```

- With few groups (q1: 96) each morsel sends a handful of partial rows.
- With many groups (q2: 1 million) a morsel goes raw after its probe. A row is hashed once,
  scattered once, and inserted once into a partition table of about 16,000 groups (1 million /
  64), small enough for the L2 cache (estimate). Today it is inserted twice into larger tables.
- Both modes give a group the same per-morsel state: its rows of that morsel, in row order. The
  mode is a tuning choice and not visible in results.
- A worker that finishes a morsel merges waiting chunks before it claims again; partitions merge
  at the same time. A partition table that passes 131,072 groups is split by 6 more hash bits.
- When a `Sort` follows directly (as in every benchmark query), the final reordering is skipped:
  the aggregate exposes `first` as a hidden last sort key.

**Accumulators.** Only the last two rows are order-sensitive:

| Aggregate | State per group | Combine |
| --- | --- | --- |
| `count` | `i64` | Add |
| `sum` of `int`, `duration` | `i128` | Add; the range is checked once, at the end |
| `sum` of `decimal` | `i128` and an `i64` carry | Add; checked at the end |
| `min`, `max` | The value | Total order (for `float`: `f64::total_cmp`, as in `sort`) |
| `first`, `last` | The value and its `RowOrd` | Smallest or largest `RowOrd` |
| `median` | The values | Append; sorted at the end |
| `sum` of `float`, `mean` | `f64` (and a count) | Add partial sums in seq order |
| `stddev`, `corr`, `cov`, `slope`, `intercept` | Count, means, sums of products | Today's pairwise `merge`, in seq order |

#### 1.7 Hash join

- **Build** is a pipeline whose sink scatters rows (`RowOrd`, keys in the layouts of 1.6, the
  columns the output needs) into the same 64 partitions. Each partition is finished on its own
  core: order its chunks by seq, then build `first[bucket]` and `next[row]` by walking the rows
  backwards, as `Table::build` does, so every chain lists its rows in input order. Rows with a
  null key are kept (a full join emits them) but not inserted.
- **Probe** is a streaming operator. Per batch: hash the keys; test a blocked Bloom filter (one
  64-bit word per key, built with the partitions); look up the survivors; gather. When every
  probe row matches exactly once (the usual foreign-key join, q3), the probe columns pass through
  untouched and only build columns are gathered.
- **Output order** is unchanged: probe rows in order, each with its matches in build order.
- **Kinds.** Inner, left, semi and anti are probe-side loops. A full join sets one bit per matched
  build row (allocated for this kind only) and emits the unmatched build rows at the end, merged
  across partitions by `RowOrd`.
- **Runtime filter.** For inner and semi joins, the Bloom filter and the key range of the build
  go to the probe-side scan as an extra, inexact condition (Parquet skips with it, section 2.7).
- **Build side.** The planner estimates rows times row width of both inputs from source
  statistics (4.1). It swaps the sides when the written build side (the right) is estimated at 4
  times the left or more, and either the kind is semi or anti (the build side then holds the left
  rows, marks them, and emits them in build order, which is left order), or the consumer cannot
  observe row order (an aggregate with only commutative accumulators, or a sort; both then use
  the pair of ordinals). Otherwise the sides stay as written: the documented output order is the
  left table's, and restoring it would mean sorting the output. The decision reads plan and file
  metadata only, so it is the same on every run.

#### 1.8 Sort and top-n

Kept: normalized keys, a 16-byte prefix in each entry, ties broken by row position. Changed:

- **No `concat`.** The sink keeps the batches of each morsel. Keys are encoded per morsel by the
  worker that produced it. An entry is `(prefix: u128, RowOrd)`.
- **Fixed-width path.** When every key is fixed-width and declared non-null, keys are written
  straight into the prefix without validity bytes. If they fit in 16 bytes, no row format is built.
- **Output** is gathered in parallel with `interleave`, one morsel of 131,072 rows per task.
- **Top-n** (`Sort` with `fetch`, when `skip + fetch <= 131,072`):

```text
shared: bound = the n-th best key so far; it only tightens, and readers may see an old value
per morsel: a heap of at most n entries (key, RowOrd), and the rows they refer to
    per batch: compare the first key column with bound (one vectorized comparison) -> mask
               encode keys for the survivors only; push; drop the worst beyond n
               compact the kept rows when they exceed 2n
    at the end: merge the heap into the global heap; tighten bound
finish: the global heap, sorted
```

  A stale `bound` only keeps extra candidates, so the result is exactly the first n rows of the
  stable sort. Memory is `threads * n` rows, not the table. Above the threshold the full sort
  runs, and each run keeps its first n rows.

#### 1.9 Window functions

- **Partition, then sort.** The sink keeps the input batches and scatters only `(RowOrd, partition
  keys, order keys, arguments)` by the hash of the partition keys into the 64 partitions. Each is
  sorted on its own core by (partition keys, order keys, `RowOrd`); `changes`, `layout` and
  `compute` then run on it as today.
- **Results are written back by `RowOrd`** into columns aligned with the kept batches. Payload
  columns never move, and the output is in input order by construction.
- **Reuse of order.** (1) If the input is already sorted by the partition keys, then the order
  keys (a `Sort` directly below, tracked as a property of the physical plan), the window is a
  streaming operator over adjacent rows: no hash, no sort. (2) Adjacent `Window` nodes with the
  same `partition` and `order`, where the second does not read the first's results, are merged
  and share one sort. (3) A window with only whole-partition aggregates and no `order` uses the
  hash aggregation of 1.6 and gathers the result per row: no sort.
- `moving_avg` restarts its running sums at every partition and counts non-finite values apart.

#### 1.10 `distinct` and `count_distinct`

- `distinct()` stays an aggregate without accumulators. When the merge of morsel `m` adds a key,
  the key is final and in first-appearance order, so it is emitted at once: `distinct |> take(n)`
  stops early.
- `count_distinct(x)` starts as today: keep the values per group, sort and count at the end. That
  path made q9 four times faster (docs/09). Its weakness is memory proportional to rows (q9 keeps
  two columns of 5 million `i64`, 80 MB by arithmetic). When the kept values pass a fixed size,
  it switches to two levels: a distinct set over `(group keys, x)`, built with 1.6 in a second
  table, then a count per group joined to the main result by key. That form is proportional to
  distinct pairs and can spill. Both forms are exact, so the switch is not visible in results.

### Why this and not the alternatives

| Alternative | Why it loses here |
| --- | --- |
| Keep pull with `par_map` | A barrier per operator, batches migrate between cores, breakers run inside `execute`, errors depend on the group size |
| Exchange operators, one partition per thread | The thread count shapes the data: batches, partial sums and row order change with it, against principle 2 |
| Async streams on tokio | A second scheduler and a large dependency for CPU-bound work; partitions again follow threads |
| Compiling queries to machine code | Start-up cost and a compiler backend to ship; Arrow kernels already use SIMD |
| Thread-local hash tables that outlive a morsel | A group's partial sum would depend on which morsels a thread happened to take |
| Order-independent float sums (exact or binned reproducible summation) | 48 bytes or more per sum and several times the work per value; `stddev` and `corr` would need another, less stable formula. An open question for 1.0 |
| One global join table built with atomic inserts | Chain order depends on timing; partitions give the same cache behavior with a fixed order |
| A radix join that also partitions the probe side | Breaks probe order and adds a pass over the big side. It is the out-of-core path (2.5), not the default |

### Keeping results reproducible

These rules hold for the whole design; later sections refer to them by number.

- **R1. Morsel boundaries are data.** They follow from the source bytes or metadata and the table
  in 1.1, never from threads, memory or timing. `x.csv` and `x.csv.gz` give the same morsels.
- **R2. A morsel is processed alone.** What a worker does with it depends on that morsel and on
  constants. It may read shared hints that cannot change the result (the top-n bound, a Bloom
  filter).
- **R3. Order-sensitive combining happens in seq order.** The float sum of a group is: its
  per-morsel sums, each added in row order from `0.0`, folded in seq order. This is today's
  definition with "run of 4 batches" replaced by "morsel". It does not depend on batch size,
  aggregation mode, partition count, spilling or pushdown (a row never changes morsel).
- **R4. Every comparison-based order ends with `RowOrd`.**
- **R5. Tuning values never reach results:** thread count, batch size, admission window, radix
  bits, hash seed, memory limit, spill decisions. The hash seed becomes fixed anyway (today
  `DefaultHashBuilder` is seeded at random per run), so that timings and spills are repeatable.
- **R6. Errors are ordered** (mechanism 4 of 1.2). Integer and decimal sums fail only when the
  final sum does not fit, not when a prefix overflows.

Two behaviors change on purpose (see the open questions): float sums over more than one morsel
change in their last bits once, and `min` and `max` of floats follow the total order.

### How to test and measure it

- **Oracle.** Today's operators stay in the tree as `reference::*` until the new ones have parity.
  A generator of random tables and plans (the xorshift style of the tests in `ops.rs`) compares
  both for every operator, with nulls, `NaN`, `-0.0`, empty inputs and strings over 12 bytes.
- **Determinism matrix.** Each plan runs under threads {1, 2, 3, 8}, batch rows {1, 7, 1,024,
  8,192}, window {1, 4, 64}, two hash seeds, and random delays in workers. The test compares a
  hash of the Arrow IPC bytes of the whole result, not printed text. The knobs are test-only.
- **Fast path against plain path** (ROADMAP section 12): `Fixed` and `Str` against `Rows`; top-n
  against sort then take; fixed-width sort keys against the row format; masks against compaction;
  dictionaries against views.
- **Errors.** A file with a bad value late in the data gives the same outcome and message for
  every thread count, with and without a `take` above it.
- **Measure.** `bench/run.py` (q1 to q11, 8 threads against 1, peak memory) after every step. New
  queries for what the set does not stress: a join with a large build side, a full sort, a top-n
  with a large n, a group by a long string key. `biggo explain --timings` (ROADMAP section 11)
  reports rows, time and peak bytes per operator, and is built first.

## 2. Big data and very large files

### Today

| Part | Where | How it works today |
| --- | --- | --- |
| What streams | `ops.rs`, `lib.rs` | Scan, filter, project, join probe and `limit`; `write_csv`, `write_json` and `write_parquet` write batch by batch. An aggregate with few groups holds little |
| What materializes | `ops::concat`, `aggregate`, `materialize` | Sort, window and the join build hold the input and its concatenated copy together; sort adds key rows, 24-byte entries and the gathered output. An aggregate keeps a `State` per run until the end. `collect`, `to_rows` and `write_sqlite` (which collects first) hold the whole result. There is no limit and no spill: q7 holds 818 MB for 5 million rows (docs/09) |
| CSV | `csv_pieces`, `split_rows` | The whole file is mapped, and the quotes of every 2 MiB block are counted before the first row is produced, so `take(5)` still reads the whole file. A position is "inside quotes" when the count of `"` bytes before it is odd. That is exact when quotes appear only around fields and doubled inside them. After a quote inside an unquoted field (`2" nail`), which the Arrow reader accepts as data, later cuts use the inverted state (see the findings). The splitter needs random access: no pipes, no compressed files |
| Other encodings | `csv_pieces` | A non-UTF-8 file is decoded whole (`encoding.decode(&mapped)`), on one core, into anonymous memory (`Text::Decoded`) |
| JSON | `json_pieces`, `split_lines` | The file is mapped and cut at the first `\n` after every 2 MiB, which is exact for JSON Lines. A file that is one JSON array is not supported |
| Parquet | `parquet_pieces` | The footer is read once; a piece per row group decodes all projected columns into a `Vec<RecordBatch>`; then `Shape::finish` casts and filters. Statistics, page indexes, Bloom filters and row filters are not used |
| SQLite | `sqlite_pieces` | One piece reads the whole result into `Vec<Scalar>` columns before it returns its first batch; a pushed `limit` cannot stop it |
| Not there | | Compressed input, several files in one call, standard input. A table used twice is computed twice; `collect` is manual (docs/04) |

### Proposed

#### 2.1 Which operators stream and which hold data

| Operator | State | When the state does not fit |
| --- | --- | --- |
| Scan, filter, project, unpivot, explode, union, join probe | One morsel in flight per worker | Cannot happen: the admission window bounds it |
| `Limit`, top-n | `skip + fetch` rows per worker | Above 131,072 rows, top-n becomes a sort |
| Aggregate, `distinct` | One entry per group | Partitions spill (2.4) |
| Join build | The build side | Partitions spill (2.5) |
| Sort | All rows | Sorted runs (2.3) |
| Window | All rows | Partitions and kept input spill (2.6) |
| `collect` | All rows | Chunks spill and are read back on use |
| File writers | Morsels waiting in the turnstile | Bounded by the admission window |

#### 2.2 Spill files: Arrow IPC messages with an index in memory

A spill file belongs to one operator and one partition. It holds a schema message, then one
record-batch message per chunk, aligned to 64 bytes. The operator keeps 32 bytes per chunk in
memory: `(seq, kind, offset, length, rows)`. Chunks come back through positioned reads into
aligned buffers that the pool counts.

| Operator | A chunk holds |
| --- | --- |
| Sort | A slice of a sorted run: the normalized key as a binary column, then the payload |
| Aggregate | Raw rows (keys, arguments, row number) or partial states (keys, state columns, `first`) |
| Join, window | Rows of one radix partition, with `RowOrd`; for a window, later its results |
| `collect` | The batches as they are |

Dictionaries are decoded before spilling. Files are not compressed by default (on a local SSD a
plain write is faster than LZ4, an estimate); `BIGGO_SPILL_COMPRESSION=lz4` is for slow disks.
Where files live and how they are removed is section 3.8.

#### 2.3 External merge sort

```text
sink:   keep batches and encoded keys while the reservation grows
        when it cannot grow: sort what is held (as in 1.8), write it as run k, release the memory
finish: no run written -> the in-memory path of 1.8;  otherwise write the rest as the last run
merge:  take T-1 splitter keys from the runs' sparse indexes (the first key of every chunk)
        count the rows below each splitter (one binary search per run): each task then knows
        the position of its first row in the sorted output
        task i merges the key range [splitter i-1, splitter i) of all runs with a loser tree
        more than 64 runs: first merge groups of 64 into longer runs
```

Splitters are full keys including `RowOrd`, so ranges do not overlap. A merge task holds one
chunk per run. The output is cut into morsels of 131,072 rows by position in the sorted order,
not by task, so the next pipeline's morsels do not depend on where runs or splitters fell (R1).
With `fetch`, each run keeps only its first `skip + fetch` rows.

#### 2.4 Aggregation that spills

```text
when the sink's reservation cannot grow:
    take the partition that holds the most bytes
    write its table as one partial chunk (the "base"), free it, mark the partition spilled
    from now on, chunks for it are appended to its file instead of merged
finish, for spilled partitions, as many at a time as memory allows:
    fits:         load the base, replay the chunks in seq order exactly as in 1.6, emit
    does not fit: split base and chunks by the next 6 hash bits into 64 files; repeat (3 levels)
output: each partition emits its groups sorted by `first`; the final order is a merge by `first`
```

The base is the fold of a prefix of the chunks in seq order (the turnstile guarantees a prefix),
and the replay continues that fold. Every group sees the same sequence of merges as in memory.
`median` keeps raw values: one enormous group can still exceed memory, and the query then fails
with the error of 3.7.

#### 2.5 Hybrid hash join

```text
build:  as in 1.7. When the reservation cannot grow: write the chunks of the largest partition
        to its file, mark it spilled, append its later chunks there.
probe, nothing spilled: as in 1.7.
probe, some partitions spilled:
    a row whose partition is in memory is joined at once
    a row whose partition is spilled goes, with its RowOrd, to that partition's probe file
    then, per spilled partition: load its build side (split by 6 more bits if it does not fit),
    read its probe chunks in seq order, join
```

Each of these streams is in probe order; together they are not. If the consumer observes row
order, every stream is written as a run sorted by `RowOrd` and the runs are merged as in 2.3
(the merged rows regroup into their original morsels). If it does not (the test of 1.7), rows
are pushed as they come. This is the price of the documented join order: a join that spills
writes its output once more. A key so frequent that its partition never fits is joined by a
block nested loop: build rows are loaded a block at a time, and the probe file is read once per
block.

#### 2.6 Window and `collect`

A window holds three things, and each spills on its own: the kept input batches (written in seq
order), the partition rows (as in 2.5), and the results (runs sorted by `RowOrd`). The output is
the input read back in order, zipped with a merge of the result runs. A collected table is a list
of chunks, any of which the pool may move to disk; a scan of it reads them back in order.

#### 2.7 Parquet

The footer is read once (as today); the page index only when the scan has conditions. A condition
gets a pruning form when it is `column op literal`, `is_null(column)`, `column in [...]`, or
`and`, `or`, `not` of these, and the file's type maps to the declared type exactly and
monotonically (4.2). On `(min, max, null count)` that form gives `Never`, `Always` or `Maybe`.
Then, per row group:

1. **Statistics.** Skip the row group if a condition is `Never`. A condition that is `Always` is
   dropped for this row group: it is not evaluated at all.
2. **Bloom filters**, for `==` and `in` on a column chunk that has one
   (`get_row_group_column_bloom_filter`, `Sbbf::check`).
3. **Page index.** The same evaluation per page gives a `RowSelection`.
4. **Late materialization.** The remaining conditions become a `RowFilter` of `ArrowPredicateFn`s,
   fixed-width columns first. The reader decodes the condition columns, and the other columns
   only for rows that passed.
5. **Limit:** `with_limit`, when the reader evaluates every condition.
6. **Streaming.** The row group is pushed batch by batch, never held whole.
7. **Strings** are read as views, and fully dictionary-encoded chunks as dictionaries (1.5).

Rules that keep this correct:

- A condition that `can_fail` runs only on rows that passed the conditions written before it, and
  no condition written after it may skip data before it has run: skipping would hide an error
  that the program would have raised.
- Float statistics follow the Parquet rules for `NaN` and signed zero, truncated string
  statistics are used on their safe side only, and a missing statistic is `Maybe`.

On q5 (0.063 s against 0.024 s for DuckDB and Polars), statistics will not help: the data is in
random order, and by the distributions in `bench/gen.py` the filter keeps about 60% of the rows.
The gains expected there come from reading `region` as a dictionary, typed group keys and the
fused pipeline. A second Parquet benchmark, sorted by date, is needed to measure pruning.

#### 2.8 CSV and JSON

**One decoder per format, written for static schemas.** The Arrow readers parse every field into
general builders; biggo knows the columns and types before it opens the file.

- **CSV**, per morsel: (1) one scan with `memchr` for delimiter, quote and newline that records
  the start and length of the projected fields only; (2) one tight loop per column over those
  offsets (`int`, `float` through `lexical-core`, `date`, `decimal`, `bool`); (3) strings become
  views into the source unless they contain a doubled quote; (4) the columns that the scan's
  conditions read are parsed first, the others only for the rows that pass. The options of
  ROADMAP section 8 (`header`, `skip`, `nulls`, `date_format`, `decimal`, `on_error`) are
  parameters of these loops.
- **JSON**, per morsel: no tape, no tree. The decoder expects the declared keys in the order of
  the previous object and checks each with one comparison; a miss falls back to a lookup. Values
  of undeclared keys are skipped structurally. Strings without a backslash are views.
- Text conversion is the largest cost of q1, q3, q4, q6, q9 and q10 (docs/09: about 900 MB/s on 8
  cores). The CSV decoder is expected to matter more for those queries than any operator change
  (estimate: 1.3 to 1.8 times).

**Splitting.**

- Quote parity stays: it is exact and parallel. Cut points are computed one admission window
  ahead, inside `claim`, not for the whole file, so a `Limit` stops the reading. The dialect
  becomes strict: a quote inside an unquoted field is an error that names the line, with
  `quote = ""` for files that use `"` as data. Parity is then right for every accepted file.
- Limits that remain: one quote character, no backslash escapes; a single row larger than the
  in-flight allowance (3.1) is an error.
- A JSON array (`[{...}, {...}]`) has no safe cut points without string and nesting state.
  `claim` runs a sequential structural scan (quotes, backslashes, depth) and hands out morsels of
  whole objects; parsing stays parallel. The scan is the one sequential part.
- **Other encodings.** In every encoding of `encoding_rs` except UTF-16 and ISO-2022-JP, the
  bytes `0x0A` and `0x22` never occur inside a multi-byte character. The splitter runs on the raw
  bytes, and each worker transcodes its own morsel. UTF-16 and ISO-2022-JP take the stream path
  of 2.9. Nothing is decoded whole.

#### 2.9 Inputs that cannot be split: compressed files, pipes, HTTP bodies

One reader thread (not a pool thread: it blocks on I/O) decompresses or reads, cuts the text into
morsels at row ends by the rule of 2.8 (it sees the bytes in order, so it carries the quote
state), and fills a queue of `W` blocks. `claim` pops from the queue; parsing is parallel; the
queue is the backpressure. Throughput is bounded by one core of decompression, which is inherent
in gzip and single-frame zstd. Formats that need seeking (Parquet, Arrow IPC files) are first
copied from a pipe to a temporary file. `zstd` is already in the tree through `parquet`; gzip
needs `flate2` with its pure-Rust backend (new).

#### 2.10 Many files

- `read_csv<T>("logs/2026-*.csv")` expands when the scan opens. **Files are ordered by the bytes
  of their paths**, never by directory order, and seq runs through them in that order.
- Each file is validated against the declared row type on its own; an error names the file.
- A path segment `key=value` (a partitioned directory) gives a constant column when the row type
  declares `key`. Conditions on such columns, and on the file-name column, are evaluated on the
  path with the evaluator of 2.7 (`min = max = value`); files that cannot match are not opened.
- Files open lazily, a few ahead of the workers, and close after their last morsel. Parquet
  footers are fetched by that read-ahead, not one by one under the dispatcher's lock.

#### 2.11 A table that is read twice

- **Inside one query** (a self-join, a `union` of two filters of one table): the physical planner
  finds subplans that are the same `Arc<Plan>` before optimization. If the shared part contains a
  breaker or scans a text format, it runs once into a buffer that spills like `collect`, with the
  union of the columns and the `or` of the pushed conditions; each consumer applies its own
  again. A shared Parquet scan is simply read twice, each side with its own pruning.
- **Across statements** (`let t = read_csv(...)`, then two queries on `t`): a **scan cache** owned
  by the process keeps fully decoded columns per `(file identity, morsel, column)`; the identity
  is path, size, modification time and inode. A later scan takes cached columns instead of
  parsing. The cache is the first thing the pool evicts, and it is never spilled.
- Automatic `collect` at compile time is rejected: two selective queries over a large file would
  materialize all of it.

### Why this and not the alternatives

| Alternative | Why it loses here |
| --- | --- |
| Sort-based aggregation and sort-merge join for large data | A full sort costs more than hash partitioning, and first-appearance or probe order must be restored afterwards anyway |
| A custom row-oriented spill format | More code to write and test. IPC needs no parsing on read, handles every type, is already compiled (through `parquet`), and is also the Arrow IPC connector |
| Parquet as the spill format | Pays for encoding and compression on data that is read back once |
| Mapping spill files, or leaving it to the OS to swap | Memory the pool cannot count, eviction it cannot steer, random reads |
| Guessing the quote state per chunk and verifying later | Needs a second pass or a retry path; parity is exact once the dialect is strict |
| Morsel sizes that adapt to time or memory | Breaks R1 |
| Decode with the Arrow readers, filter afterwards (today) | Parses fields that a condition is about to drop, and cannot skip pages |

### Keeping results reproducible

- Spilling moves chunks. It never changes their content, their seq, or the order in which a group
  or a merge consumes them (2.3 to 2.6), so the memory limit is invisible in results (R5).
- Pruning, late materialization and pushed conditions remove rows inside their morsel; the seq
  numbers of skipped row groups and files stay unused (R1, R3).
- The reader thread of 2.9 cuts by the rule of the mapped splitter, so a file and its compressed
  copy give the same morsels. File order is the byte order of paths. The scan cache returns the
  arrays that decoding would produce.
- Written files are bit-identical for any thread count: writers format morsels in parallel and
  append them through the turnstile, and Parquet row groups are cut by row count.

### How to test and measure it

- **Limit matrix.** Every query of the determinism matrix also runs with the memory limit at 1/2,
  1/8 and 1/64 of its in-memory peak. The result hash must not change.
- **Splitter.** Extend `splits_at_row_boundaries_only`: random files with quoted line breaks,
  every target size, every supported encoding; decoding in pieces equals decoding whole.
- **Pruning.** For random files and conditions, the pruned scan equals the unpruned scan. Include
  files from other writers, with and without statistics, page indexes and Bloom filters, and
  float columns with `NaN` and `-0.0`.
- **Failure.** Kill the process during a spill, and check that no file remains; fill the
  temporary directory, and check the message.
- **Measure.** Add to `bench/`: a Parquet file sorted by date; data sets of 50 and 500 million
  rows run under a 2 GB limit; a `.csv.gz` copy; the TPC-H queries of ROADMAP section 12. Report
  time, peak resident memory (already taken from `wait4`) and bytes spilled.

## 3. Memory management

### Today

- `biggo-exec` has no accounting, no limit and no temporary files. Memory is bounded by structure
  only: `ScanIter` holds one group of decoded pieces, `ParMap` one group of batches, `aggregate`
  64 batches (`WINDOW`). Breakers hold everything (section 2).
- Batches are shared by reference count; `ops::limit` slices; `Shape::finish` selects columns
  without copying. The copies are `filter_record_batch`, `concat` and `take`.
- CSV and JSON files are mapped with `memmap2`: their pages are file-backed but count as
  resident, and most of the 270 MB peak of the streaming queries is the mapped file (docs/09).
  Parquet is read through `File`. A non-UTF-8 CSV is copied whole into anonymous memory.
- `convert::batch` builds a new Arrow schema for every batch. The SQLite reader holds a
  `Vec<Scalar>` per column (an enum value per cell, an allocation per string). Threads come from
  rayon's global pool (`RAYON_NUM_THREADS`).

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
    /// batches is counted once, and released when its last owner drops it.
    pub fn claim(&self, batch: &RecordBatch);
}

pub trait Spill: Send + Sync {
    /// Frees at least `want` bytes if it can; returns what it freed. Called by the pool, on
    /// the requesting thread, only for consumers that are idle.
    fn spill(&self, want: usize) -> Result<usize>;
}
```

- The pool belongs to the process: collected tables and the scan cache outlive queries. The VM
  runs one query at a time, so a query has what those leave.
- An operator that keeps data owns one `Reservation` and grows it in steps of 1 MiB or more (one
  atomic addition per step).
- **How an operator is told to spill.** `try_grow` answers `false` only after the pool has evicted
  the scan cache and asked idle consumers to spill (collected tables, the runs of a finished
  sort). The operator then spills itself (2.3 to 2.6) and asks again. If it cannot spill, the
  query fails (3.7). A running operator is never interrupted from outside: it spills at its own
  `try_grow`, where its state is consistent, and no operator calls into another under a lock.
- **Morsels in flight** are not counted batch by batch. A pipeline reserves `W` times 8 MiB when
  it starts (an estimate, to be calibrated in step 0). If the limit cannot cover that, `W`
  shrinks, down to 1.

#### 3.2 The default limit: 50% of physical memory

The limit is `--memory-limit` or `BIGGO_MEMORY_LIMIT` (ROADMAP section 11); otherwise half of the
physical memory, or half of the cgroup limit when one is set (a few lines of platform code:
`sysctl`, `/proc/meminfo` and `memory.max`, `GlobalMemoryStatusEx`). Half, not the 80% that DuckDB
uses, because the pool counts operator state only. The other half serves what it does not count:
the page cache that mapped inputs and spill files depend on, the VM's values, allocator slack,
and temporaries inside one batch.

#### 3.3 Accounting

Every large allocation has exactly one accounting owner:

| Memory | Counted by |
| --- | --- |
| Hash tables, key arenas, accumulator columns, sort entries, partition buffers | The operator, exactly, when it grows them (`try_grow`) |
| Batches that a sink keeps | `Reservation::claim`, on Arrow's `pool` feature: `RecordBatch::claim` attaches a reservation to each underlying buffer; a second claim replaces the first; the reservation ends when the buffer is freed |
| Source blocks (decoded text, decompressed data, HTTP ranges, Parquet pages) | The in-flight allowance while the morsel runs; afterwards the claim of a batch that still points into them |
| Mapped files | Not counted against the limit (the OS can drop the pages); reported separately |

- `get_array_memory_size` is not used: it counts a shared buffer once per array that refers to
  it, and a whole 2 MiB source block for one string view.
- Before a sink keeps a batch, it makes the batch worth keeping. A sliced fixed-width column that
  uses less than half of its buffer is copied. A view column whose strings fill less than half
  of the blocks they pin is compacted (`gc`). Views into a mapped file are left alone: the file
  is their backing store, at no cost to the limit.
- Not counted: lists built by `to_rows`, allocator overhead, kernel temporaries.

#### 3.4 Backpressure

A morsel runs through its pipeline on one worker, so no queue sits between operators. Three
queues exist, and the admission window `W` bounds each: the reorder buffer of a turnstile, the
result queue to the VM, and the block queue of a reader thread (2.9). A slow sink (spilling, or
writing to a slow disk) keeps its seq unreleased: `low` stops, workers stop claiming, the scan
stops. A consumer that stops pulling (a REPL that printed 50 rows) leaves at most `W` morsels
decoded.

#### 3.5 Batch size and cache behavior

- **8,192 rows per batch** (today 32,768). Five 8-byte columns are then 320 KB, not 1.3 MB, and
  fit with their intermediates in a 1 MB L2 cache. The M1 Pro of docs/09 has larger caches, so
  the present value was not wrong there. The batch size is not visible in results (R5): the final
  value comes from measuring 4,096 to 32,768 on that machine and on an x86 server.
- Each worker reuses scratch buffers (hashes, masks, key bytes) across batches. Arrow schemas are
  built once per pipeline. Hash tables stay near cache size through the partitions (1.6, 1.7).
- `mimalloc` as the global allocator gets one measurement, and is adopted only if q1 to q10 gain
  5% or more. Its cost is C code in the build, as SQLite and zstd already are.

#### 3.6 Avoiding copies

| Technique | What it saves |
| --- | --- |
| Mapped input, kept for local uncompressed CSV and JSON | Views point into the file, and the file is the "spill" of those strings. `advise_range` asks the OS to read ahead, and to drop the pages of a morsel that left no views behind. Everything else uses read blocks |
| String views | `filter`, `take` and every gather move 16 bytes per string |
| Masks; slices and shared columns (as today) | One copy per condition (1.3) |
| No `concat` | Breakers keep chunks and gather once, where sort copies twice today |
| Spill writes of 1 MiB or more with a drop-behind hint (`F_NOCACHE` on macOS, `POSIX_FADV_DONTNEED` on Linux) | Spilled data does not stay in memory as dirty pages. Direct I/O is not used |

#### 3.7 At the limit

In order: evict the scan cache; spill idle consumers; the requesting operator spills itself;
shrink `W` to 1; fail. The error names operators by their line in `explain`, with sizes:

```text
error: not enough memory: the limit is 16.0 GB (the default, half of 32 GB)
  Sort: revenue desc, date, customer_id
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

| Question | Answer |
| --- | --- |
| Where | `BIGGO_TEMP_DIR`, else the system's temporary directory. On Linux, a `/tmp` that `statfs` reports as `tmpfs` (memory) is skipped in favor of `/var/tmp`. A process creates `biggo-<pid>-<random>/` with mode 0700 on first use |
| Cleanup on error | A spill file is owned by its operator. Returning an error, or unwinding from a panic caught at the pipeline boundary, drops the operator, which closes the file and releases its reservation |
| Cleanup on a crash | On Unix a spill file is unlinked right after it is opened, so the kernel frees it when the process ends for any reason, including `kill -9`. On Windows it is opened with `FILE_FLAG_DELETE_ON_CLOSE`. The empty directory of a crashed run is removed by the next run that spills (its pid is no longer alive) |
| Dependencies | None: about 50 lines over `std::fs` |

### Why this and not the alternatives

| Alternative | Why it loses here |
| --- | --- |
| No accounting: let the OS swap or kill | The process dies without a message, or the machine thrashes |
| Summing `get_array_memory_size` over batches | Counts shared buffers and source blocks many times |
| An allocator that refuses allocations past the limit | Fails at places that cannot spill; most Rust code aborts when an allocation fails |
| A pool that calls into running operators to make them spill | Lock ordering between operators, for no gain: pipelines run one at a time, so one operator grows at a time |
| A fixed share of the limit per operator | Wastes the shares of operators that are not growing |
| A default of 80% of RAM | The pool does not count everything, and biggo depends on the page cache |
| Direct I/O for spill files | Alignment rules and platform differences; the hints give most of the benefit |

### Keeping results reproducible

- No per-morsel decision reads the state of the pool (R2). A refused `try_grow` leads to a spill,
  and a spill does not change results (section 2). So the default limit, which differs between
  machines, cannot change a result (R5).
- One thing does depend on the machine: whether a query runs out of memory at all. That is the
  failure of a resource, like a full disk, and not a result. A query that succeeds gives the same
  result everywhere.

### How to test and measure it

- **Fault injection.** A test pool refuses the N-th `try_grow`, for random N. Every spill path
  runs, and the result hash equals that of the unlimited run.
- **Leaks.** After every query of the test suite: `used == 0` for the query's consumers, and no
  file in the temporary directory.
- **Accuracy.** For q1 to q11 and the large data sets of section 2, compare the pool's high-water
  mark with peak resident memory minus mapped files (`bench/run.py` records the peak). Target,
  not measurement: resident memory stays under the limit plus 25%.
- **Messages.** Golden tests for the two errors of 3.7. **Batch size.** The sweep of 3.5.

## 4. Data sources

### Today

| Part | Where | How it works today |
| --- | --- | --- |
| Formats | `Format`, `Scan` (`plan.rs`); `scan::scan` | Four formats: `Csv`, `Parquet`, `Json`, `Sqlite`. `Scan` carries the format, the path, the query (database only), the CSV options, the declared and the output schema, the pushed filters and the limit. `scan::scan` matches on the format and calls `csv_pieces`, `parquet_pieces`, `json_pieces` or `sqlite_pieces` |
| Shared back half | `Shape::new`, `Shape::finish` | Works out the columns to read; casts each column to its declared type, checks nullability, applies the filters, projects. A missing column is reported with the columns the file has (`Shape::missing_column`) |
| Pushdown | The optimizer; `ScanIter` | Filters, projection and limit are put into `Scan`. Projection is used by the file readers (CSV does not convert the fields, Parquet does not read the columns). Filters run after decoding. The limit stops `ScanIter` from decoding more pieces |
| Errors in data | `csv_error`, `json_error`, `quoted_value` | Made by rewriting the text of Arrow's messages. CSV errors give line and column, except for decimals (the Arrow message has no line). JSON errors give the field, no line. SQLite errors give the column, no row |
| SQLite | `sqlite_pieces`, `sql_value` | `read_sql<T>(path, query)` runs the query as written, on one connection, and converts value by value |
| Writers | `write_csv`, `write_json`, `write_parquet`, `write_sqlite` (`lib.rs`) | The first three open the target with `File::create`, which truncates it, and write on one thread. `write_sqlite` collects the result, then drops, creates and fills the table in one transaction, row by row |
| Not there | | Other databases, Excel, Arrow IPC, URLs, standard input and output |

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
    pub statistics: Statistics,      // rows, bytes (exact or estimated); min, max, nulls per column
    pub physical: SchemaRef,         // the Arrow types it emits (views or dictionaries for strings)
}

pub trait Connector: Send + Sync {
    /// Reads metadata only: a header line, a footer, a catalog. Checks the declared row type
    /// against the source and fails here, before any row, if they cannot agree.
    fn open(&self, ctx: &Context, at: &Location, request: &ScanRequest)
        -> Result<(ScanPlan, Arc<dyn Source>)>;            // `Source` as in section 1.1
    /// Starts a write, or says that this source cannot be written.
    fn create(&self, ctx: &Context, at: &Location, schema: &Schema, mode: WriteMode)
        -> Result<Arc<dyn Writer>>;
}

pub trait Writer: Send + Sync {
    fn encode(&self, batch: &RecordBatch) -> Result<Encoded>;   // on workers, in parallel
    fn append(&self, part: Encoded) -> Result<()>;              // in seq order, by the turnstile
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
- **No plugins.** `biggo-plan` holds data only (a `SourceKind`, a `Location`, options);
  `biggo-exec` maps the kind to a connector with a `match`. One executable (principle 5).
- **Validation** is one shared routine that every `open` calls. The connector lists the source's
  columns and types; the routine matches declared columns by name and looks each pair up in the
  connector's type table (4.2). It reports a missing column with the columns that exist, and a
  type that cannot be read with both types. The row type stays the only schema a program sees
  (principle 1): nothing is inferred at run time, and hidden columns such as `first` never leave
  the engine.
- **Splits** are the morsels of the `Source` (1.1). **Statistics** feed the build-side choice
  (1.7), table sizing and `explain`.

#### 4.2 How each source fits

| Source | Parallel reading | Projection, filter, limit | Statistics | Writing |
| --- | --- | --- | --- | --- |
| CSV | 2 MiB morsels; the stream path when not seekable | Unread fields are not parsed; conditions run in the decoder (`Exact`); the limit stops `claim` | Rows estimated from size and a sampled row length | Morsels formatted in parallel, appended in order |
| JSON Lines, JSON array | As CSV; an array needs the sequential boundary scan | As CSV | As CSV | JSON Lines |
| Parquet | Row groups | Columns not read; conditions `Exact` (statistics, Bloom filters and pages are steps inside the reader); limit | Exact rows; min, max, nulls from the footer | Row groups of 131,072 rows; the columns of a group encoded in parallel |
| Arrow IPC | Record batches (file format); else the stream path | Columns not decoded; no conditions; limit | Exact rows (file format) | Yes |
| Excel | One stream | Columns skipped; `sheet`, `range`; limit | None | Later |
| SQLite | Table form: rowid ranges on several read-only connections. Query form: one stream | Table form: all three as SQL. Query form: wrapped as a subquery | `sqlite_stat1` when present | One transaction, values bound from arrays, `mode = "append"` |
| PostgreSQL | Table form: ranges of an integer key, one connection each, one shared snapshot. Query form: one `COPY` stream, decoded in parallel | As SQL | `pg_class.reltuples`, `pg_stats` | `COPY FROM` binary; replace inside a transaction |
| MySQL | One stream, decoded in parallel (connections cannot share a snapshot) | As SQL | `information_schema.tables` | Multi-row inserts; replace by an atomic `RENAME TABLE` swap |
| Standard input, output | The stream path | By the format | None | Streamed in order; no replace |

**Types.** A pair that is not in this table is refused at `open`, with the column, its type in
the source, and the declared type.

| biggo | Parquet, Arrow IPC | PostgreSQL | MySQL | SQLite (as today) |
| --- | --- | --- | --- | --- |
| `int` | Int8 to Int64, UInt8 to UInt32; UInt64 with a range check | `int2`, `int4`, `int8` | `TINYINT` to `BIGINT`; unsigned `BIGINT` with a range check | INTEGER; numeric text |
| `float` | Float32, Float64 | `float4`, `float8`, `numeric` | `FLOAT`, `DOUBLE` | REAL, INTEGER; numeric text |
| `decimal` | Decimal with scale up to 6 | `numeric`, each value checked for 6 decimals | `DECIMAL`, same check | INTEGER, REAL, text |
| `bool` | Boolean | `bool` | `TINYINT(1)`, `BIT(1)` | 0 and 1; `true`, `false` |
| `string` | Utf8, LargeUtf8, Utf8View, and dictionaries of them | `text`, `varchar`, `char`, `uuid`, `json`, `jsonb`, enums | `CHAR`, `VARCHAR`, `TEXT`, `ENUM`, `JSON` | TEXT; numbers as text |
| `date` | Date32, Date64 | `date` | `DATE` | ISO text |
| `datetime` | Timestamp of any unit; a zone is converted to UTC and dropped | `timestamp`, `timestamptz` (to UTC) | `DATETIME`, `TIMESTAMP` | ISO text |
| `duration` | Duration of any unit | `interval` without months | `TIME` | A number of seconds |

Parquet pruning (2.7) uses a condition only when the pair is exact and keeps order: integers,
floats to `float`, dates, timestamps (the literal is converted to the file's unit), decimals of
the same scale, strings.

**Databases.**

| Topic | Design |
| --- | --- |
| Crates | `rusqlite` (present). PostgreSQL: `postgres`, a blocking API over `tokio-postgres` that keeps its runtime private: no async code in the engine, but tokio is compiled in. MySQL: `mysql`, blocking throughout. TLS for both, and for HTTP, through `rustls`. Estimated cost: 2 to 3 MB of binary (the release build of 8 October is 22.7 MB). Versions and feature sets could not be checked while writing this, and must be |
| Two forms | The *table form* names a table: biggo writes the SQL, so it can project, filter, limit, order and split. The *query form* (today's `read_sql<T>(path, query)`) is one stream; projection, conditions and limit are pushed by wrapping it: `SELECT a, b FROM (<query>) AS q WHERE ... LIMIT n`. MySQL may discard an `ORDER BY` inside such a subquery, so there the wrapping is skipped when the query contains one |
| Bulk reads from PostgreSQL | `COPY (SELECT ...) TO STDOUT (FORMAT binary)`. The reader thread cuts the stream into blocks of whole rows (it walks the length fields only). Workers decode: big-endian integers and floats, text as views, dates as days and timestamps as microseconds since 2000-01-01 (shifted to 1970), `numeric` from its base-10,000 digits. The special value `infinity` is a data error |
| Parallel reads by key range | For the table form with an integer primary key: read `min`, `max` and the row estimate; cut ranges of about 100,000 rows; range `i` is morsel `i`, run as `WHERE k >= a AND k < b ORDER BY k` on one of a few connections (4 by default). On PostgreSQL the connections share one snapshot (`pg_export_snapshot`, `SET TRANSACTION SNAPSHOT`), so the read is consistent while the table changes. SQLite does the same with rowid ranges |
| Credentials | A password is not written in a program: it comes from `env(...)` (ROADMAP section 8) or the usual variables and files (`PGPASSWORD`, `~/.pgpass`, `MYSQL_PWD`). A `Location` keeps the secret apart from the text that is shown: `explain`, errors and plans print `postgres://user:***@host/db`. An executable made by `biggo build` holds source only |
| TLS | `rustls` with the system's root certificates and a bundled set as fallback. For a host that is not the local machine, certificate and host name are verified by default; turning that off has to be written in the URL |

SQL is written by one generator over a small dialect interface:

```rust
pub trait Dialect {
    fn ident(&self, name: &str) -> String;                    // "x" or `x`
    fn literal(&self, value: &Scalar) -> String;
    /// SQL for one condition, and whether the database's answer is exact.
    fn condition(&self, expr: &Expr) -> Option<(String, Pushed)>;
}
```

A pushed condition must select a superset of what biggo's own evaluation selects: `Exact` where
the meaning is identical, `Inexact` (checked again) where it is widened, not pushed where not even
a superset is certain.

| Condition on | Pushed as |
| --- | --- |
| Integers, booleans; dates and datetimes in native types | `Exact` |
| Strings | Under a byte-wise collation (`COLLATE "C"`, `utf8mb4_bin`), and still `Inexact`: MySQL's default collations ignore case, and biggo compares bytes |
| Floats | SQL treats `-0.0` and `0.0` as equal, biggo's total order does not: a strict comparison goes in its non-strict form (`x < c` as `x <= c`), `Inexact` |
| Dates stored as text (SQLite) | The bound widened by one day, `Inexact` |
| Functions, arithmetic | Not pushed in the first version |
| A limit | Only when every condition is `Exact` or absent |
| Literals | Bound as parameters where the protocol allows. `COPY` takes none: there the generator writes them, with `standard_conforming_strings` on and quotes doubled. Only values of the eight biggo types can appear |

**HTTP(S) and S3-style object stores.**

| Topic | Design |
| --- | --- |
| Client | `ureq` (blocking) on `rustls`. S3 requests are signed in-house (SigV4 on `sha2` and `hmac`). Credentials come from `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`, `AWS_REGION` and `~/.aws/credentials`; `AWS_ENDPOINT_URL` selects a compatible store. Instance metadata and single sign-on are left out of the first version |
| Ranges | `read_ranges` merges ranges less than 1 MiB apart, splits ranges over 8 MiB, and fetches them in parallel on an I/O pool of 16 threads, separate from the rayon pool. Parquet asks for the last 64 KiB first (the footer, usually with the page index), then per row group only the column chunks or pages that survive 2.7; a `ChunkReader` serves the decoder from those ranges. CSV and JSON ranges are handed in order to the reader thread of 2.9 |
| Retries | On a failed connection, a timeout, a short body, and status 408, 429, 500, 502, 503 or 504: up to 5 attempts, waiting 100 ms doubling to 10 s with random jitter, honoring `Retry-After`, resuming a body at the byte where it stopped. Every request after the first carries `If-Match` with the first ETag: if the object changed, the query fails. It never mixes two versions of a file |
| Synchronous, not async | The engine is CPU-bound and runs morsels on rayon. An object store needs tens of concurrent requests, which 16 blocked threads provide. An async runtime would add a second scheduler, split `Source` into two kinds of function, and enlarge the binary, to serve thousands of concurrent requests that one machine reading analytics data does not issue |
| Writing | To S3: a temporary local file, then one `PUT` or a multipart upload; both replace the object atomically. Plain HTTP is read-only |

**The other sources.**

| Source | Design |
| --- | --- |
| Standard input and output | The path `-` selects them (ROADMAP section 8). Input takes the stream path of 2.9 and can be read once: a second scan in the same query is served by the shared buffer of 2.11, and one in a later statement is an error that suggests `collect`. Output is streamed in order, without atomic replace. A closed pipe cancels the query quietly |
| Excel | The `calamine` crate reads `.xlsx` (also `.xls`, `.ods`) in pure Rust; `zip` and `quick-xml` come with it. A sheet is one compressed XML stream: it is read on one thread and handed over in morsels of 65,536 rows. Cells are typed one by one: a number in a column declared `int` must be whole, a date is a number with a date format, an empty cell is null, an error cell (`#N/A`) is a data error. A sheet has at most 1,048,576 rows, so this path need not scale |
| Arrow IPC | Read and written with `arrow-ipc`, which is already compiled. The file format's footer gives the position of every record batch: batches are morsels, and an uncompressed local file whose buffers are aligned is decoded in place from the mapping. The stream format takes the stream path. The spill files of 2.2 use the same code |

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

- One function renders it, in today's CSV wording, so the golden files do not change:
  ``sales.csv, line 13: cannot read 'north' as an int for column `region` ``. JSON gains the
  line, Parquet and databases gain the row, Excel names the cell.
- Decoders build a `DataError` themselves; nothing parses the text of Arrow's messages any more.
  A decoder knows the row within its morsel. The absolute line is computed only when an error
  occurs, by counting line breaks before the morsel, as the closure in `csv_pieces` does today.
- The error reported is the first in row order (R6).
- `on_error = "null"` (ROADMAP section 8) is a policy passed to the decoder. The number of values
  replaced is a commutative count, and the examples shown are the first in seq order, so the
  report is reproducible too.

#### 4.4 Writing files with atomic replace

A file writer creates `.<name>.biggo-<pid>-<n>.tmp` in the directory of the target (the same file
system, so `rename` is atomic), writes, calls `fsync`, copies the permission bits of an existing
target, and renames. A writer that is dropped removes its temporary file. So a query that fails
leaves the old file untouched, and a query may write the file it reads: the old file stays open
and mapped until the scan ends. `append = true` (ROADMAP section 8) is not atomic, and the
documentation will say so.

### Why this and not the alternatives

| Alternative | Why it loses here |
| --- | --- |
| The `object_store` crate with tokio and the async Parquet reader | Complete, but an async runtime and several MB (estimate) for tens of concurrent requests |
| ODBC, ADBC, or the databases' C client libraries | Shared libraries on the user's machine: no longer one self-contained executable |
| A PostgreSQL protocol client written here | About 2,000 lines (estimate) with SCRAM and TLS, and security-sensitive. Worth it only if tokio's weight is unacceptable (open question) |
| Pushdown as all or nothing | Loses every case where a source can only narrow: statistics, collations, widened float bounds |
| The engine always checks every condition again | Simple, but repeats work where the source is exact |
| Connectors loaded as plugins | Against principle 5 |
| Truncating the target in place (today) | Loses data when the query fails or reads the same file (see the findings) |

### Keeping results reproducible

- Every source defines its row order, and seq follows it: the file; the sorted list of files; the
  key order of a table read in ranges; the order in which a database returns a query's rows.
- The last is the weak point: a query without `ORDER BY` is only as reproducible as the database
  makes it. The table form always orders by the key. Parallel ranges read one snapshot on
  PostgreSQL; on MySQL they are off by default for that reason.
- Remote files are pinned to one ETag. Retries and their jitter change timing only.
- Pushed conditions never change which rows pass (the superset rule and the second check).

### How to test and measure it

- **One conformance suite, run against every connector:** a round trip of every type with nulls
  and extreme values; every refusal at `open` with its message; row order; parallel equals
  single-threaded.
- **Pushdown equivalence.** For random conditions and data, the result with pushdown equals the
  result without, per connector and dialect. PostgreSQL and MySQL run in containers in CI
  (ROADMAP section 11); SQLite runs in process.
- **A local HTTP server that misbehaves on purpose:** short bodies, 503 with `Retry-After`,
  stalls, an object that changes between requests.
- **Atomic replace.** Kill the process during a write; write a file that the query reads; make a
  query fail after half the output. The target is the old file or the new one, never a part.
- **Measure.** q11 (SQLite: 0.339 s, no gain from more cores, docs/09) before and after; the same
  query against PostgreSQL and MySQL with 1 million rows; Parquet over a local S3-compatible
  store; the three conversions (0.88 s, 1.39 s and 0.84 s in docs/09).

## Order of implementation

Each step can be built, tested against the oracle, and measured with `bench/run.py` on its own.
"Effect" refers to docs/09; a figure that is not quoted from there is an estimate or a target.
ROADMAP puts data in and out (packages 9 to 12) before the engine (package 15): steps 1, 2 and 18
to 21 serve those packages and can move ahead of the others.

| # | Step | Depends on | Expected effect | Risk |
| --- | --- | --- | --- | --- |
| 0 | Measure and fix: `explain --timings`, a hash of full results, the test knobs of section 1; fix the local findings below (float `min`/`max`, `moving_avg`, the `can_fail` list, the splitter) | Nothing | No timing change. Every later step becomes measurable per operator | Low |
| 1 | Atomic file writes (4.4) | Nothing | None on reads; `fsync` adds a little to the three conversions | Low |
| 2 | `Connector`, `ByteSource`, `DataError`; the four sources moved behind them unchanged | 1 | None. Unblocks Excel, databases, many files | Low: golden error texts guard it |
| 3 | Pipeline executor (1.1, 1.2): sources, streaming operators, gate, ordered collector. Today's sort, aggregate, join and window run unchanged inside sinks | 2 | Estimate: 5 to 15% on q1 to q10 (no barriers, slow cores take fewer morsels). Errors stop depending on threads | High: touches every operator. Oracle and determinism matrix |
| 4 | Expression programs and masks (1.3, 1.4) | 3 | Estimate: 5 to 10% on the filtered CSV queries (q1, q8), more on q5 where decoding is cheap | Medium: `can_fail` semantics |
| 5 | Hash aggregation (1.6): key layouts, per-morsel pre-aggregation, partitions, ordered merge, exact sums | 3 | q2: 0.384 s today; the ROADMAP target is within 1.5 times of Polars (0.143 s). Small gains on q1, q6, q9, q10. Float sums change in their last bits once | High: R3. Determinism matrix; fast paths against `Rows` |
| 6 | Top-n (1.8) | 3 | q4: peak memory falls from 387 MB toward that of the streaming queries (about 270 MB); a modest time gain | Low |
| 7 | String views (1.5) through the whole engine | 3 | Little alone; needed for 8, 9, 10 and cheap gathers | Medium: a wide mechanical change (every `as_string::<i32>()`) |
| 8 | CSV decoder, lazy and strict splitting, encodings in parallel (2.8) | 2, 7 | The largest single effect. Estimate: 1.3 to 1.8 times on q1, q3, q4, q6, q9, q10, where Polars leads by 1.1 to 1.9 times today | Medium to high: CSV corner cases. Differential test against the Arrow reader; fuzzing |
| 9 | Parquet scan (2.7): streaming, statistics, pages, Bloom filters, `RowFilter`, dictionaries | 4, 5, 7 | q5: 0.063 s today; the ROADMAP target is within 1.5 times of 0.024 s. The gain comes from dictionaries and typed keys, not pruning (random data); a sorted file shows pruning | Medium: statistics corner cases |
| 10 | JSON decoder and JSON arrays (2.8) | 2, 7 | q8: 0.428 s today; the ROADMAP target is within 1.5 times of DuckDB (0.194 s) | Medium |
| 11 | Join (1.7): partitioned parallel build, key layouts, Bloom filter, pass-through probe, build side | 3, 5 | Small on q3 (its build side has 2,000 rows); shown by the new large-build benchmark | Medium |
| 12 | Sort and window without `concat` (1.8, 1.9) | 3, 11 | q7: 0.465 s and 818 MB today. Estimate: a third less memory, a modest time gain | Medium |
| 13 | Streaming `distinct`, two-level `count_distinct` (1.10) | 5 | q9: memory (349 MB) falls when the switch triggers; time must not exceed today's 0.216 s | Low to medium |
| 14 | Memory pool, accounting, the limit and its error (3.1 to 3.4, 3.7), still without spilling | 3 | Target: under 1% of time. A query over the limit fails with a clear message instead of being killed | Medium: accuracy of the accounting |
| 15 | Spill files and external sort (2.2, 2.3, 3.8) | 12, 14 | A sort larger than RAM completes. In-memory benchmarks unchanged | Medium |
| 16 | Spilling aggregation (2.4) | 5, 15 | A `group` with more groups than fit completes | Medium to high: fold order under spill |
| 17 | Hybrid hash join; window and `collect` spill (2.5, 2.6) | 11, 12, 15 | The last "must fit in RAM" limits of docs/09 are gone | High: restoring order after a spill is the most intricate part |
| 18 | Stream path, compressed input, standard input and output, many files (2.9, 2.10) | 2, 3 | New capability; no change in docs/09 | Low to medium |
| 19 | Shared subplans and the scan cache (2.11) | 14 | None on single-query benchmarks. A program that queries one CSV several times parses it once | Medium: cache invalidation |
| 20 | Databases (4.2): SQLite ranges, pushdown and direct binds first; then PostgreSQL; then MySQL | 2, 3 | q11: 0.339 s and no gain from 8 cores today. Estimate: 2 times or more | Medium: dialects, new dependencies |
| 21 | HTTP and S3, Excel, Arrow IPC (4.2) | 2, 9, 18 | New capability | Medium: dependencies, TLS |
| 22 | Batch-size sweep, allocator trial, TPC-H, Linux, a regression run in CI (3.5, ROADMAP section 12) | The engine steps | Sets the final constants | Low |

## Open questions

Decisions that need the owner:

1. **Float sums across versions.** R3 ties the last bits of a float sum to the morsel table of
   1.1, and step 5 changes them once (the unit becomes the morsel, not 4 batches). Is it
   acceptable to freeze that table at 1.0 (ROADMAP section 13 promises the same results in every
   1.x), or should 1.0 pay for order-independent summation?
2. **Three changes of behavior.** (a) `sum` of `int` fails only when the final sum overflows, not
   when a prefix does. (b) `min` and `max` of `float` use the total order: `NaN` is the largest
   value, as in `sort`. (c) `count_distinct` treats `0.0` and `-0.0` as `group` and `==` do (two
   values). Agree to all three?
3. **Strict CSV quoting.** A quote inside an unquoted field becomes an error, with `quote = ""` as
   the way out. The alternative keeps such files readable and makes exact parallel splitting
   cost a second pass.
4. **Row order of joins.** The build side is swapped only where order cannot be observed, so
   `small |> join(big)` followed by a float sum still builds on `big`. Keep the documented order
   (docs/04), or allow a swap that reorders the output?
5. **The memory limit.** Is 50% of RAM the right default? Are `--memory-limit`,
   `BIGGO_MEMORY_LIMIT` and `BIGGO_TEMP_DIR` the right names?
6. **Language surface this design assumes** but does not define: a table form of `read_sql`
   (`table = "..."`) and a split key; the name of the file-name column for many files; columns
   from `key=value` directories; `quote = ""`.
7. **Dependencies and binary size.** `postgres` (brings tokio), `mysql`, `ureq`, `rustls`,
   `calamine`, `flate2`, possibly `mimalloc`. Is there a size budget for the executable? If tokio
   is unwelcome, PostgreSQL needs a protocol client written here.
8. **String views.** Step 7 changes the physical type of every string column. Agree to that wide
   change, or stay on `Utf8` and give up zero-copy strings and cheap gathers?
9. **Databases and reproducibility.** The table form adds `ORDER BY` on the key (a cost on the
   server), and MySQL reads on one connection unless the program asks for more. Acceptable?
10. **The scan cache.** On by default? What share of the limit may it use?
11. **Mapped files.** Mapping stays for local text files, with the known risk: if another process
    truncates the file during a query, biggo dies with a signal. Keep that trade, or always read?

## Findings in today's code

Found while reading for this document. "Observed" means reproduced on 9 October 2026 with an
existing binary (the release build of 8 October; the debug build of 9 October for item 5).
"Read" means seen in the code and not run.

| # | Finding | Where | Evidence |
| --- | --- | --- | --- |
| 1 | Float `min` and `max` lose values when a group contains `NaN`. A `NaN` that is the first value of a run becomes that run's result, and then loses every comparison in the merge | `aggregate.rs`: `Acc::ExtremeFloat` in `update` and `merge` | Observed: a column of `1.0`, 250,000 `NaN`, one `3.0`, then more `NaN` gives `max = 1.0`. `[NaN, 3.0]` gives `NaN`; `[3.0, NaN]` gives `3.0` |
| 2 | `moving_avg` leaks across partitions: the running sums are never reset, so an average is a difference of sums over all earlier partitions | `window.rs`: `compute`, `WindowFn::MovingAvg` | Observed: a `NaN` in partition `a` makes every `moving_avg` of partition `b` `NaN`. Large values in one partition also cost precision in the next (read) |
| 3 | Whether a query fails can depend on the thread count. A group of pieces or batches is processed together, and an error in any of them discards the good batches before it. Which of two errors is reported is not fixed either (`collect::<Result<_>>` on a parallel iterator may return any) | `scan.rs`: `ScanIter::next`; `ops.rs`: `ParMap::next` | Observed: a CSV with a bad value at line 600,002, read by `derive |> where |> take(5)`, prints 5 rows with 1 thread and fails with 8 |
| 4 | File writers truncate the target in place, before the query has run | `lib.rs`: `write_csv`, `write_json`, `write_parquet` (`write_sqlite` is safe) | Observed: `read_csv("x.csv") |> where(...) |> write_csv("x.csv")` ends with a bus error (exit code 138) and leaves `x.csv` empty, because the scan maps the file that `File::create` truncates. Observed: a query that fails leaves the target empty |
| 5 | Guarded expressions can still fail: `div`, `abs` and `to_bool` can raise an error but are not in the list, so both branches of an `if` are evaluated | `biggo-plan/src/expr.rs`: `Expr::can_fail` | Observed: `if n != 0 { div(x, n) } else { 0 }` fails with "division by zero"; `if x > 0 { abs(x) } else { 0 }` with "integer overflow" for the smallest `int`; a guarded `to_bool` with "cannot convert". The same guard around `%`, which is in the list, works |
| 6 | A stray quote makes the CSV splitter quadratic: after a quote in an unquoted field, every later cut starts "inside quotes" and scans to the next quote or the end of the file, and the file is decoded as one piece on one thread | `scan.rs`: `split_rows` | Observed in one informal run (not a benchmark): a 128 MB file took about 16 times longer with one such quote |
| 7 | `sum` of `int` overflows depending on run boundaries: `update` fails when a prefix inside a run overflows, `merge` checks only the run totals | `aggregate.rs`: `Acc::SumInt` | Read: `[MAX, 1, -1]` fails inside one run and succeeds across two |
| 8 | `count_distinct` merges `0.0` and `-0.0`, while `group` and `==` keep them apart | `aggregate.rs`: `distinct_keys` | Observed: 2 groups, `count_distinct` = 1 |
| 9 | Silent or hard limits: `u32` row numbers (`rows as u32` would wrap above 4 billion rows); 2 GiB of string data per column in `concat`; a whole SQLite result is read before its first row, so a pushed `limit` does not stop it | `ops.rs`: `sort_indices_from`, `concat`; `scan.rs`: `sqlite_pieces` | Read |
