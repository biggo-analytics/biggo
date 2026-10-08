# Performance

This page reports the measured speed of biggo, how it was measured, how it compares with DuckDB,
Polars and CPython, where it is fast, where it is still slow, and why. All numbers were actually
measured on 8 October 2026 and can be reproduced with the scripts in [`bench/`](../bench).

## Summary

5 million rows of data on an Apple M1 Pro with 8 cores:

- Typical analytical queries on a 187 MB CSV (filter + compute + group, join, top-n, pivot, count
  distinct) take **0.18–0.24 seconds**, which is about 21–28 million rows per second, including
  process start-up time and compilation.
- The same query on Parquet takes **0.06 seconds** (80 million rows per second).
- Compared with DuckDB 1.5.6: biggo is faster in 6 of 10 queries, equal in 1, slower in 3
  (grouping into a million groups, Parquet, JSON).
- Compared with Polars 2.0.0: Polars is faster in 8 of 10 queries (1.1–1.9 times, and 2.6–2.7 times
  in two queries), equal in 1, and biggo is faster in 1 (JSON).
- Using 8 cores is about 4 times faster than a single core, and **results are identical bit for
  bit** however many threads are used.
- Ordinary code on the VM is in the same speed class as CPython 3.14 (1.0–1.5 times slower).
- Starting the process and running `print("hello")` takes 5 milliseconds; the type checker handles
  about 670,000 lines per second.

## Machine and method

| | |
| --- | --- |
| Machine | Apple M1 Pro: 8 cores (6 performance + 2 efficiency), 32 GB RAM, internal SSD |
| System | macOS 27.0.1 |
| biggo | 0.1.0, `cargo build --release` (Rust 1.99.0, thin LTO, `codegen-units = 1`) |
| Main libraries | arrow 60.0, parquet 60.0, rayon 1.12, rusqlite 0.40 (bundled SQLite) |
| The other engines | DuckDB 1.5.6 and Polars 2.0.0 on CPython 3.14.7 |

**How biggo is timed**: `bench/run.py` runs `biggo run <query>` as a new process every time and
times it from the start of the process until it exits, so the time **includes** loading the
executable, parsing, type checking, compiling and printing the result. Each query gets 1 warm-up run
and then 5 measured runs, and the best value is reported. Memory is the peak resident set size of
the process (from `wait4`).

**How the other engines are timed**: `bench/compare.py` runs queries with the same meaning in a
Python process that has already imported the libraries, and times only the query itself (best of 3
runs), so the time **does not include** Python start-up and imports. This comparison therefore leans
slightly in favor of the other engines (about 5 milliseconds per query).

**Correctness**: the first row of the result from all three engines was compared for every query,
and they match (`float` sums can differ at the 15th–16th decimal place because the order of
addition differs). `bench/run.py` also confirms every time that biggo prints the same result when
run with 8 threads and with 1 thread.

**Caveats**: the machine was not idle during the measurements. Other programs were running (load
average 8–15). Taking the best value reduces the effect, but each number can still vary by about
±10% from one set of measurements to the next. Differences smaller than that should not be treated
as significant. The measurements were made on one machine, one system, and one data size.

## Data and queries

`bench/gen.py` generates random data that comes out the same every time (fixed seed):

| File | Contents | Size |
| --- | --- | --- |
| `sales.csv` | 5,000,000 rows: `date`, `region` (8 values), `product_id` (2,000 values), `customer_id` (1 million values), `qty`, `price` (2% are empty) | 187 MB |
| `products.csv` | 2,000 rows: `product_id`, `category`, `cost` | 32 KB |
| `sales.parquet` | `sales.csv` written with `write_parquet` (Snappy) | 65 MB |
| `sales.json` | `sales.csv` written with `write_json` | 500 MB |
| `sales.db` | the first 1,000,000 rows, written with `write_sql` | 42 MB |

| Query | What it does |
| --- | --- |
| q1 | filter on 2 conditions, compute `revenue`, group by `region` and month (96 groups), 3 aggregates, top 5 |
| q2 | group by `customer_id` (1 million groups), 3 aggregates, sort, top 5 |
| q3 | join with `products` (2,000 rows), compute margin, group by `category` |
| q4 | compute `revenue`, then take the 5 largest rows (sorted by 3 keys) |
| q5 | q1 reading from Parquet |
| q6 | `pivot`: totals per month, split into 4 columns by `region` |
| q7 | `window` partitioned by `customer_id` (1 million partitions), ordered by 3 keys: `row_number` and `cumsum` |
| q8 | q1 reading from JSON Lines |
| q9 | `count_distinct` of `customer_id` and `product_id` per `region` |
| q10 | read `price` as `decimal`, then sum `qty * price` exactly |
| q11 | q1 reading from SQLite (1 million rows) |

The program for each query is a `bench/q*.bgo` file.

## biggo results

| Query | 8 cores | 1 core | Speedup | Rows per second (8 cores) | Peak memory |
| --- | --- | --- | --- | --- | --- |
| q1 filter + group (CSV) | 0.209 s | 0.918 s | 4.4x | 24 million | 266 MB |
| q2 group into 1 million groups (CSV) | 0.384 s | 1.388 s | 3.6x | 13 million | 491 MB |
| q3 join + group (CSV) | 0.237 s | 0.943 s | 4.0x | 21 million | 287 MB |
| q4 top 5 (CSV) | 0.243 s | 0.928 s | 3.8x | 21 million | 387 MB |
| q5 q1 on Parquet | 0.063 s | 0.246 s | 3.9x | 80 million | 69 MB |
| q6 pivot (CSV) | 0.241 s | 1.041 s | 4.3x | 21 million | 276 MB |
| q7 window, 1 million partitions (CSV) | 0.465 s | 1.372 s | 2.9x | 11 million | 818 MB |
| q8 q1 on JSON Lines | 0.428 s | 1.800 s | 4.2x | 12 million | 575 MB |
| q9 count distinct (CSV) | 0.216 s | 0.874 s | 4.1x | 23 million | 349 MB |
| q10 decimal sum (CSV) | 0.177 s | 0.770 s | 4.3x | 28 million | 293 MB |
| q11 q1 on SQLite (1 million rows) | 0.339 s | 0.351 s | 1.0x | 3 million | 51 MB |

Observations:

- **CSV is limited by text conversion.** q1, q3, q4, q6 and q9 take similar times (0.21–0.24 s)
  even though they do very different work, because most of the time goes to converting the 187 MB
  CSV into values (about 900 MB/s on 8 cores). q5, which does the same work as q1 on Parquet, is
  3.3 times faster.
- **q10 is faster than q1** even though it uses `decimal`, because it uses one column fewer (it does
  not have to convert `date`): the cost of 128-bit `decimal` arithmetic is very small compared with
  converting the CSV.
- **Memory** for streaming queries (q1, q3, q6) is about 270 MB, most of which is the mapped CSV
  file (pages that have been read count as resident). Queries that must hold the whole table (sort,
  window, many groups) use more, and q7 holds all 5 million rows together with their encoded keys.
- **SQLite** is read on a single thread and converted one value at a time, so it reaches 3 million
  rows per second and does not get faster with more cores.

## Compared with DuckDB and Polars

Times are in seconds, and lower is better. Bold marks the fastest in each row.

| Query | biggo | DuckDB 1.5.6 | Polars 2.0.0 | biggo vs DuckDB | biggo vs Polars |
| --- | --- | --- | --- | --- | --- |
| q1 filter + group | 0.209 | 0.273 | **0.147** | 1.3x faster | 1.4x slower |
| q2 group into 1 million groups | 0.384 | 0.294 | **0.143** | 1.3x slower | 2.7x slower |
| q3 join + group | 0.237 | 0.267 | **0.126** | 1.1x faster | 1.9x slower |
| q4 top 5 | 0.243 | 0.272 | **0.136** | 1.1x faster | 1.8x slower |
| q5 Parquet | 0.063 | **0.024** | **0.024** | 2.6x slower | 2.6x slower |
| q6 pivot | 0.241 | 0.268 | **0.150** | 1.1x faster | 1.6x slower |
| q7 window | 0.465 | 0.472 | **0.454** | equal | equal |
| q8 JSON Lines | 0.428 | **0.194** | 0.560 | 2.2x slower | 1.3x faster |
| q9 count distinct | 0.216 | 0.296 | **0.190** | 1.4x faster | 1.1x slower |
| q10 decimal sum | 0.177 | 0.251 | **0.143** | 1.4x faster | 1.2x slower |

A frank reading of the results:

- biggo is in **the same class** as the two leading engines: no query is more than 3 times apart,
  and 13 of the 20 pairwise comparisons are within 1.5 times.
- **Polars is faster in almost every query**, especially the CSV ones: the Polars CSV reader is
  faster than the Arrow one that biggo uses, and its hash aggregation is better when there are many
  groups (q2).
- **Compared with DuckDB, biggo is faster on CSV**, except when there are very many groups (q2), and
  it is clearly slower on Parquet and JSON.
- biggo's numbers include process start-up time (5 ms), which affects q5 the most: after subtracting
  it, biggo is still about 2.4 times slower.

## Scaling with thread count

| Query | 1 thread | 2 threads | 4 threads | 8 threads |
| --- | --- | --- | --- | --- |
| q1 | 0.912 s | 0.485 s | 0.260 s | 0.225 s |
| q2 | 1.384 s | 0.793 s | 0.472 s | 0.394 s |
| q3 | 0.937 s | 0.510 s | 0.279 s | 0.234 s |
| q4 | 0.937 s | 0.501 s | 0.285 s | 0.241 s |
| q5 | 0.245 s | 0.139 s | 0.082 s | 0.064 s |

- 1 → 2 threads is 1.75–1.9 times faster, and 1 → 4 is 2.9–3.5 times faster: the parts that can run
  in parallel scale well.
- 4 → 8 gains only a further 1.15–1.3 times, for two reasons: 2 of the 8 cores of the M1 Pro are
  efficiency cores, which are much slower, and other programs were competing for cores during the
  measurements. On an idle machine where every core is a performance core, this number should be
  better, but that was not measured.
- The parts that are still single-threaded are the final merge of an aggregate with few groups,
  building the hash table of a join, and concatenating batches before a sort.

Set the number of threads with `RAYON_NUM_THREADS=n`.

## Virtual machine

Code that does not work on tables runs on a bytecode VM (no JIT). Here it is compared with CPython
3.14.7 running equivalent programs (`bench/vm_compare.py`; the CPython times do not include
interpreter start-up, the biggo times do):

| Program | What it does | biggo | CPython 3.14 | biggo memory |
| --- | --- | --- | --- | --- |
| `vm_fib` | `fib(32)`: 7 million function calls | 0.314 s | 0.239 s | 7 MB |
| `vm_loops` | `map` / `filter` / `fold` over 3 million elements, with closures | 0.381 s | 0.367 s | 284 MB |
| `vm_records` | 300,000 records, strings, `match`, maps | 0.176 s | 0.116 s | 80 MB |

The VM is in the same speed class as CPython: equal in loops, 1.3 times slower in function calls,
and 1.5 times slower in record/map work (`put` copies the whole map every time). This speed is
intentional: the heavy work of a data-analysis program should be in table operations, which are
hundreds of times faster than the VM per row. The VM is there to tie queries together.

`vm_loops` uses 284 MB because a list of 3 million values takes 24 bytes per value, and there are
3 lists alive at the same time.

## Tools

| Task | Time | Rate |
| --- | --- | --- |
| process start-up + running `print("hello")` | 4.9 ms | — |
| `biggo check` on a 27,997-line program (4,000 functions) | 42 ms | 668,000 lines/second |
| `biggo fmt` on the same program | 51 ms | 552,000 lines/second |
| CSV → Parquet, 5 million rows | 0.88 s | 5.7 million rows/second |
| CSV → JSON Lines, 5 million rows | 1.39 s | 3.6 million rows/second |
| CSV → SQLite, 1 million rows | 0.84 s | 1.2 million rows/second |

The type checker is fast enough for the language server to re-analyze the whole file on every
keystroke.

## What makes it fast

1. **Computing one column at a time, not one row at a time**: a column expression becomes calls to
   Arrow kernels on contiguous arrays of 32,768 rows, where the CPU can process several values per
   instruction (SIMD), and no bytecode is interpreted per row.
2. **Types are known at compile time**: the engine does not need to check the type of a value at run
   time, and it can choose type-specific accumulators in advance.
3. **The optimizer**: filters are applied while the file is being decoded, unused columns are not
   converted, `sort` + `take` becomes a top-n, and `take` stops the file read.
4. **Every stage runs in parallel**: files are cut into chunks that are decoded on separate cores,
   filter/project handle several batches at once, aggregates are split into runs and then merged,
   and sorting uses every core.
5. **No unnecessary copies**: files are mapped into memory, batches are passed along by reference
   count, and selecting columns does not copy data.
6. **A single executable that starts fast**: there is no runtime to start, so it takes 5 ms from
   command to result.

### What was optimized in this round

This set of measurements revealed three slow spots, which were fixed and then measured again (same
machine, same method, same results):

| Spot | Before | After | What changed |
| --- | --- | --- | --- |
| q9 `count_distinct` | 0.93 s | 0.22 s | stopped maintaining a hash set per group along the way; the values are now kept, then sorted and counted at the end on every core |
| q7 `window` (and full-table `sort`) | 1.52 s | 0.47 s | sorts by keys encoded as bytes on every core, carrying the first 16 bytes along with the row number; finds partition boundaries with a vectorized comparison; computes each function on a separate core |
| `vm_records` | 0.36 s | 0.18 s | frequently used scalar functions (`to_string`, `length`, `is_null`, ...) are computed in the VM itself, instead of building a one-row column and sending it to the engine |

All three have tests that confirm the results equal those of the old path (the parallel sort against
the plain one, and the functions in the VM against the engine's, case by case).

## Slow spots and limitations

- **Parquet is 2.6 times slower than DuckDB/Polars** (q5): biggo does not yet use the min/max
  statistics of row groups to skip row groups that the filter rules out entirely, and it always
  decodes columns before filtering.
- **Many groups** (q2) is 2.7 times slower than Polars: group keys are always encoded as bytes,
  even for a single `int`, where a type-specific fast path would be faster.
- **JSON is 2.2 times slower than DuckDB** (q8): biggo uses Arrow's JSON reader as it is.
- **SQLite is read on a single thread, one value at a time** (q11), and biggo's filters are not
  passed into the SQL.
- **Everything is in memory**: a full-table `sort`, `window`, the right side of a `join`, and a
  `group` with many groups must hold all their data in RAM. There is no spill to disk yet. Data
  larger than RAM works only for streaming queries (filter, compute, grouping with few groups).
- **join** builds its hash table on a single thread and does not choose the sides for you: the small
  table should be on the right.
- **The optimizer is rule-based**: it has no statistics about the data and does not reorder joins.
- **The VM has no JIT**: code that loops over a list one row at a time is hundreds of times slower
  than table operations.

## Writing fast programs

- **Read from Parquet** if you need to read the same file more than once: convert it once with
  `write_parquet(read_csv<T>(...), "x.parquet")`, and every read after that is 3 or more times
  faster.
- **Keep the work in tables.** Do not call `to_rows` and then loop with `map` over large data:
  `derive` + `agg` do the same work hundreds of times faster.
- **`collect` a table that you reuse.** A table is a plan: use it twice and the file is read twice.
- **The small table goes on the right of a `join`**
- **Filter in SQL** when reading from SQLite: biggo's `where` runs after the rows have already been
  read out.
- **Use `float` when you do not need precision to the cent**, and `decimal` when you do:
  `decimal` is slower at multiplication and division, but on CSV the difference is barely visible.
- **You do not need to `select` yourself for speed.** The optimizer already removes unused columns,
  and for the same reason you do not need to move `where` up to the start of the pipeline.
- See what the engine does with `biggo explain file.bgo`.

## Reproducing the results

```sh
cargo build --release

python3 bench/gen.py 5000000                    # sales.csv, products.csv
target/release/biggo run bench/to_parquet.bgo
target/release/biggo run bench/to_json.bgo
target/release/biggo run bench/to_sqlite.bgo

python3 bench/run.py                            # all of biggo's tables (a few minutes)
python3 bench/run.py --only vm --runs 10        # select a subset: queries, scaling, vm, tools

pip install duckdb polars
python3 bench/compare.py                        # the same queries in DuckDB and Polars
python3 bench/vm_compare.py                     # the VM programs in CPython
```

`bench/data/` takes about 800 MB and is not kept in version control.
