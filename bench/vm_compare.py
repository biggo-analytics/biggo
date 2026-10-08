"""The programs of `bench/vm_*.bgo` in Python, to compare biggo's virtual machine with
CPython. Prints the best of five timings of each, without the start of the interpreter.

    python3 bench/vm_compare.py"""

import platform
import time


def fib(n):
    return n if n < 2 else fib(n - 1) + fib(n - 2)


def vm_fib():
    return fib(32)


def vm_loops():
    numbers = range(3_000_000)
    squares = list(map(lambda n: n * n % 1000, numbers))
    odd = list(filter(lambda n: n % 2 == 1, squares))
    total = 0
    for n in odd:
        total += n
    return len(odd), total


def tier(score):
    match score % 4:
        case 0:
            return "gold"
        case 1:
            return "silver"
        case _:
            return "bronze"


def vm_records():
    people = [{"id": i, "name": "user" + str(i), "score": i * 7 % 100} for i in range(300_000)]
    tiers = {}
    for person in people:
        name = tier(person["score"])
        tiers[name] = tiers.get(name, 0) + person["score"]
    return tiers, len(people[299_999]["name"])


print(f"CPython {platform.python_version()}")
for program in (vm_fib, vm_loops, vm_records):
    best, result = float("inf"), None
    for _ in range(5):
        start = time.perf_counter()
        result = program()
        best = min(best, time.perf_counter() - start)
    print(f"{program.__name__:12} {best:6.3f}s  {result}")
