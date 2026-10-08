"""Runs the biggo benchmarks and prints their timings as Markdown tables.

    cargo build --release
    python3 bench/gen.py 5000000                      # data/sales.csv, data/products.csv
    target/release/biggo run bench/to_parquet.bgo     # data/sales.parquet
    target/release/biggo run bench/to_json.bgo        # data/sales.json
    target/release/biggo run bench/to_sqlite.bgo      # data/sales.db
    python3 bench/run.py

Every time is the best of several runs of a whole process, from its start to its exit, so it
includes loading the executable and compiling the program. Memory is the peak resident size
of that process. `bench/compare.py` times the same queries in DuckDB and Polars."""

import argparse
import os
import platform
import subprocess
import sys
import tempfile
import time
from pathlib import Path

BENCH = Path(__file__).parent
ROOT = BENCH.parent
BIGGO = ROOT / "target" / "release" / "biggo"

QUERIES = [
    ("q1_filter_group", "filter, compute, group by 2 columns (CSV)", 5_000_000),
    ("q2_many_groups", "group by 1M distinct keys (CSV)", 5_000_000),
    ("q3_join", "join with a small table, group (CSV)", 5_000_000),
    ("q4_top", "top 5 rows by a computed value (CSV)", 5_000_000),
    ("q5_parquet", "q1 on Parquet", 5_000_000),
    ("q6_pivot", "pivot: 4 conditional sums per month (CSV)", 5_000_000),
    ("q7_window", "window over 1M partitions (CSV)", 5_000_000),
    ("q8_json", "q1 on JSON Lines", 5_000_000),
    ("q9_distinct", "count distinct per group (CSV)", 5_000_000),
    ("q10_decimal", "exact decimal sums (CSV)", 5_000_000),
    ("q11_sqlite", "q1 on SQLite", 1_000_000),
]
VM = [
    ("vm_fib", "fib(32): 7 million calls"),
    ("vm_loops", "map, filter, fold over 3M numbers"),
    ("vm_records", "300k records, strings, a map"),
]
CONVERSIONS = [
    ("to_parquet", "CSV to Parquet, 5M rows"),
    ("to_json", "CSV to JSON Lines, 5M rows"),
    ("to_sqlite", "CSV to SQLite, 1M rows"),
]


def run(args, threads=None, accept=(0,)):
    """Runs a command; returns its wall time in seconds, its peak memory in bytes, and what
    it printed. `accept` lists the exit codes that count as success."""
    env = dict(os.environ)
    if threads is not None:
        env["RAYON_NUM_THREADS"] = str(threads)
    start = time.perf_counter()
    process = subprocess.Popen(args, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    output = process.stdout.read()
    errors = process.stderr.read()
    _, status, usage = os.wait4(process.pid, 0)
    seconds = time.perf_counter() - start
    process.returncode = os.waitstatus_to_exitcode(status)
    if process.returncode not in accept:
        sys.exit(f"{' '.join(map(str, args))} failed:\n{errors.decode()}")
    # macOS reports the peak in bytes, Linux in kilobytes.
    peak = usage.ru_maxrss * (1 if platform.system() == "Darwin" else 1024)
    return seconds, peak, output.decode()


def best(args, runs, threads=None, accept=(0,)):
    """The best time and the largest peak memory of several runs, after one to warm up."""
    run(args, threads, accept)
    results = [run(args, threads, accept) for _ in range(runs)]
    return min(r[0] for r in results), max(r[1] for r in results), results[0][2]


def program(name):
    return [BIGGO, "run", BENCH / f"{name}.bgo"]


def megabytes(size):
    return f"{size / (1 << 20):.0f} MB"


def table(header, rows):
    print("| " + " | ".join(header) + " |")
    print("|" + "|".join(" --- " for _ in header) + "|")
    for row in rows:
        print("| " + " | ".join(str(cell) for cell in row) + " |")
    print()


def large_program(functions):
    """A program of many small functions, to time the parser and the type checker."""
    lines = ["type Row = { id: int, name: string, score: float? }", "let base = 10"]
    lines.append("fn f0(x: int, y: float) -> float { y + x }")
    for i in range(1, functions):
        lines += [
            f"fn f{i}(x: int, y: float) -> float {{",
            f"  let scaled = x * {i} + base",
            f"  let label = if scaled % 2 == 0 {{ \"even\" }} else {{ \"odd\" }}",
            f"  let bonus = match label {{ \"even\" => 1.5, _ => 0.5 }}",
            f"  let row: Row = {{ id: scaled, name: label + \"{i}\", score: y * bonus }}",
            f"  (row.score ?? 0.0) + f{i - 1}(x, y) / {i}",
            "}",
        ]
    lines.append(f"print(f{functions - 1}(1, 2.0) > 0.0)")
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--runs", type=int, default=5, help="timed runs of each benchmark")
    parser.add_argument("--only", choices=["queries", "scaling", "vm", "tools"], action="append")
    options = parser.parse_args()
    wanted = lambda part: not options.only or part in options.only
    if not BIGGO.exists():
        sys.exit("build biggo first: cargo build --release")
    cores = os.cpu_count()
    version = subprocess.run([BIGGO, "version"], capture_output=True, text=True).stdout.strip()
    print(f"{version}, {platform.system()} {platform.machine()}, {cores} cores, best of {options.runs}\n")

    if wanted("queries"):
        print("## queries\n")
        rows = []
        for name, what, count in QUERIES:
            if not (BENCH / "data").exists():
                sys.exit("generate the data first; see the top of bench/run.py")
            all_cores, memory, output = best(program(name), options.runs)
            one_core, _, single = best(program(name), max(options.runs // 2, 2), threads=1)
            # The answer must not depend on how many threads computed it.
            if output != single:
                sys.exit(f"{name} prints different results on 1 thread:\n{output}\n{single}")
            rate = count / all_cores / 1e6
            rows.append((name, what, f"{all_cores:.3f} s", f"{one_core:.3f} s",
                         f"{one_core / all_cores:.1f}x", f"{rate:.0f} M rows/s", megabytes(memory)))
        table(("query", "what it does", f"{cores} cores", "1 core", "speedup", "throughput",
               "peak memory"), rows)

    if wanted("scaling"):
        print("## scaling with threads\n")
        counts = [n for n in (1, 2, 4, 8, 16) if n <= cores]
        rows = []
        for name, _, _ in QUERIES[:5]:
            times = [best(program(name), max(options.runs // 2, 2), threads=n)[0] for n in counts]
            rows.append([name] + [f"{t:.3f} s" for t in times])
        table(["query"] + [f"{n} thread{'s' if n > 1 else ''}" for n in counts], rows)

    if wanted("vm"):
        print("## the virtual machine\n")
        rows = []
        for name, what in VM:
            seconds, memory, _ = best(program(name), options.runs)
            rows.append((name, what, f"{seconds:.3f} s", megabytes(memory)))
        table(("program", "what it does", "time", "peak memory"), rows)

    if wanted("tools"):
        print("## writing files\n")
        rows = []
        for name, what in CONVERSIONS:
            seconds, memory, _ = best(program(name), max(options.runs // 2, 2))
            rows.append((name, what, f"{seconds:.3f} s", megabytes(memory)))
        table(("program", "what it does", "time", "peak memory"), rows)

        print("## the compiler front end\n")
        with tempfile.TemporaryDirectory() as directory:
            hello = Path(directory) / "hello.bgo"
            hello.write_text('print("hello")\n')
            startup, memory, _ = best([BIGGO, "run", hello], options.runs * 2)
            large = Path(directory) / "large.bgo"
            source = large_program(4000)
            large.write_text(source)
            lines = source.count("\n")
            check, check_memory, _ = best([BIGGO, "check", large], options.runs)
            # `fmt --check` exits with 1 when the file is not yet in the standard form.
            formatted, _, _ = best([BIGGO, "fmt", "--check", large], options.runs, accept=(0, 1))
        table(("task", "time", "rate", "peak memory"), [
            ("run `print(\"hello\")`", f"{startup * 1000:.1f} ms", "", megabytes(memory)),
            (f"check a program of {lines:,} lines", f"{check * 1000:.0f} ms",
             f"{lines / check / 1000:.0f}k lines/s", megabytes(check_memory)),
            (f"format the same program", f"{formatted * 1000:.0f} ms",
             f"{lines / formatted / 1000:.0f}k lines/s", ""),
        ])


if __name__ == "__main__":
    main()
