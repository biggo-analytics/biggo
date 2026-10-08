"""Writes the benchmark data: `data/sales.csv` with the given number of rows, and a small
`data/products.csv` to join it with. The data is the same on every run."""

import random
import sys
from datetime import date, timedelta
from pathlib import Path

rows = int(sys.argv[1]) if len(sys.argv) > 1 else 5_000_000
out = Path(__file__).parent / "data"
out.mkdir(exist_ok=True)

random.seed(7)
regions = ["north", "south", "east", "west", "central", "bangkok", "isan", "coast"]
start = date(2024, 1, 1)
days = [(start + timedelta(days=d)).isoformat() for d in range(1000)]
products = 2_000
customers = max(rows // 5, 1)

with open(out / "products.csv", "w") as f:
    f.write("product_id,category,cost\n")
    for product in range(products):
        f.write(f"{product},cat{product % 25},{round(random.uniform(1, 80), 2)}\n")

with open(out / "sales.csv", "w") as f:
    f.write("date,region,product_id,customer_id,qty,price\n")
    chunk = []
    for _ in range(rows):
        price = "" if random.random() < 0.02 else f"{random.uniform(1, 200):.2f}"
        chunk.append(
            f"{random.choice(days)},{random.choice(regions)},{random.randrange(products)},"
            f"{random.randrange(customers)},{random.randrange(0, 20)},{price}\n"
        )
        if len(chunk) == 100_000:
            f.write("".join(chunk))
            chunk.clear()
    f.write("".join(chunk))
