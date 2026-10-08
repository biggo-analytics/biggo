"""Runs the benchmark queries in DuckDB and Polars, for comparison with biggo.

    pip install duckdb polars
    python bench/compare.py

Prints the best of three timings of each query and its first row. The times cover the query
only, not starting Python or importing the libraries."""

import time
from datetime import date
from pathlib import Path

import duckdb
import polars as pl

data = Path(__file__).parent / "data"
sales_csv, products_csv, sales_parquet, sales_json = (
    str(data / "sales.csv"),
    str(data / "products.csv"),
    str(data / "sales.parquet"),
    str(data / "sales.json"),
)

SALES = (
    "read_csv('%s', header=true, columns={'date': 'DATE', 'region': 'VARCHAR', "
    "'product_id': 'BIGINT', 'customer_id': 'BIGINT', 'qty': 'BIGINT', 'price': 'DOUBLE'})"
    % sales_csv
)
PRODUCTS = (
    "read_csv('%s', header=true, columns={'product_id': 'BIGINT', 'category': 'VARCHAR', "
    "'cost': 'DOUBLE'})" % products_csv
)
Q1 = """
    SELECT region, month(date) AS month, sum(qty * coalesce(price, 0)) AS total,
           count(*) AS orders, avg(qty * coalesce(price, 0)) AS avg
    FROM {source} WHERE qty > 0 AND date >= DATE '2025-01-01'
    GROUP BY ALL ORDER BY total DESC LIMIT 5"""

duck = {
    "q1_filter_group": Q1.format(source=SALES),
    "q2_many_groups": f"""
        SELECT customer_id, count(*) AS orders, sum(qty) AS units, max(price) AS best
        FROM {SALES} GROUP BY ALL ORDER BY units DESC, customer_id LIMIT 5""",
    "q3_join": f"""
        SELECT category, sum(qty * (coalesce(price, cost) - cost)) AS margin, sum(qty) AS units
        FROM {SALES} JOIN {PRODUCTS} USING (product_id)
        GROUP BY ALL ORDER BY margin DESC LIMIT 5""",
    "q4_top": f"""
        SELECT date, region, customer_id, qty * coalesce(price, 0) AS revenue
        FROM {SALES} ORDER BY revenue DESC, date, customer_id LIMIT 5""",
    "q5_parquet": Q1.format(source=f"read_parquet('{sales_parquet}')"),
    "q6_pivot": f"""
        SELECT year(date) AS year, month(date) AS month,
               sum(qty) FILTER (WHERE region = 'north') AS north,
               sum(qty) FILTER (WHERE region = 'south') AS south,
               sum(qty) FILTER (WHERE region = 'east') AS east,
               sum(qty) FILTER (WHERE region = 'west') AS west
        FROM {SALES} GROUP BY ALL ORDER BY year, month LIMIT 5""",
    "q7_window": f"""
        SELECT count(*) AS customers, sum(running) AS units FROM (
            SELECT row_number() OVER w AS n,
                   sum(qty) OVER (w ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) AS running
            FROM {SALES}
            WINDOW w AS (PARTITION BY customer_id ORDER BY date, product_id, qty)
        ) WHERE n = 3""",
    "q8_json": Q1.format(
        source="read_json('%s', format='newline_delimited', columns={'date': 'DATE', "
        "'region': 'VARCHAR', 'product_id': 'BIGINT', 'customer_id': 'BIGINT', "
        "'qty': 'BIGINT', 'price': 'DOUBLE'})" % sales_json
    ),
    "q9_distinct": f"""
        SELECT region, count(DISTINCT customer_id) AS customers,
               count(DISTINCT product_id) AS products
        FROM {SALES} GROUP BY region ORDER BY region LIMIT 5""",
    "q10_decimal": f"""
        SELECT region, sum(qty * coalesce(price, 0)) AS total, max(price) AS dearest
        FROM {SALES.replace("'price': 'DOUBLE'", "'price': 'DECIMAL(18, 6)'")}
        GROUP BY region ORDER BY total DESC LIMIT 5""",
}

schema = {
    "date": pl.Date,
    "region": pl.String,
    "product_id": pl.Int64,
    "customer_id": pl.Int64,
    "qty": pl.Int64,
    "price": pl.Float64,
}
revenue = pl.col("qty") * pl.col("price").fill_null(0.0)


def sales():
    return pl.scan_csv(sales_csv, schema=schema)


def q1(source):
    return (
        source.filter((pl.col("qty") > 0) & (pl.col("date") >= date(2025, 1, 1)))
        .with_columns(revenue=revenue)
        .group_by("region", pl.col("date").dt.month().alias("month"))
        .agg(total=pl.col("revenue").sum(), orders=pl.len(), avg=pl.col("revenue").mean())
        .sort("total", descending=True)
        .head(5)
    )


polars = {
    "q1_filter_group": lambda: q1(sales()),
    "q2_many_groups": lambda: sales()
    .group_by("customer_id")
    .agg(orders=pl.len(), units=pl.col("qty").sum(), best=pl.col("price").max())
    .sort(["units", "customer_id"], descending=[True, False])
    .head(5),
    "q3_join": lambda: sales()
    .join(
        pl.scan_csv(
            products_csv,
            schema={"product_id": pl.Int64, "category": pl.String, "cost": pl.Float64},
        ),
        on="product_id",
    )
    .with_columns(margin=pl.col("qty") * (pl.col("price").fill_null(pl.col("cost")) - pl.col("cost")))
    .group_by("category")
    .agg(margin=pl.col("margin").sum(), units=pl.col("qty").sum())
    .sort("margin", descending=True)
    .head(5),
    "q4_top": lambda: sales()
    .with_columns(revenue=revenue)
    .sort(["revenue", "date", "customer_id"], descending=[True, False, False])
    .select("date", "region", "customer_id", "revenue")
    .head(5),
    "q5_parquet": lambda: q1(pl.scan_parquet(sales_parquet)),
    "q6_pivot": lambda: sales()
    .group_by(pl.col("date").dt.year().alias("year"), pl.col("date").dt.month().alias("month"))
    .agg(
        *[
            pl.col("qty").filter(pl.col("region") == region).sum().alias(region)
            for region in ("north", "south", "east", "west")
        ]
    )
    .sort(["year", "month"])
    .head(5),
    "q7_window": lambda: sales()
    .sort(["customer_id", "date", "product_id", "qty"])
    .with_columns(
        n=pl.int_range(1, pl.len() + 1).over("customer_id"),
        running=pl.col("qty").cum_sum().over("customer_id"),
    )
    .filter(pl.col("n") == 3)
    .select(customers=pl.len(), units=pl.col("running").sum()),
    "q8_json": lambda: q1(pl.scan_ndjson(sales_json, schema=schema)),
    "q9_distinct": lambda: sales()
    .group_by("region")
    .agg(
        customers=pl.col("customer_id").n_unique(),
        products=pl.col("product_id").n_unique(),
    )
    .sort("region")
    .head(5),
    "q10_decimal": lambda: pl.scan_csv(sales_csv, schema={**schema, "price": pl.Decimal(18, 6)})
    .group_by("region")
    .agg(
        total=(pl.col("qty").cast(pl.Decimal(18, 6)) * pl.col("price").fill_null(0)).sum(),
        dearest=pl.col("price").max(),
    )
    .sort("total", descending=True)
    .head(5),
}


def best_of(run, times=3):
    best, result = float("inf"), None
    for _ in range(times):
        start = time.perf_counter()
        result = run()
        best = min(best, time.perf_counter() - start)
    return best, result


print(f"duckdb {duckdb.__version__}, polars {pl.__version__}")
for name, sql in duck.items():
    try:
        seconds, rows = best_of(lambda: duckdb.sql(sql).fetchall())
        print(f"duckdb  {name:16} {seconds:6.3f}s  {rows[0]}")
    except Exception as error:
        print(f"duckdb  {name:16} failed: {error}")
for name, query in polars.items():
    try:
        seconds, frame = best_of(lambda: query().collect())
        print(f"polars  {name:16} {seconds:6.3f}s  {frame.row(0)}")
    except Exception as error:
        print(f"polars  {name:16} failed: {error}")
