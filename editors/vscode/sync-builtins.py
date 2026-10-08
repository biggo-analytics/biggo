"""Rewrites the list of built-in function names in the grammar from the compiler's sources.

Run it from anywhere after adding a built-in function: `python3 editors/vscode/sync-builtins.py`.
A test in `crates/biggo-types/tests/in_sync.rs` fails when the two have drifted apart."""

import pathlib
import re

root = pathlib.Path(__file__).resolve().parents[2]
functions = (root / "crates/biggo-plan/src/expr.rs").read_text()
verbs = (root / "crates/biggo-types/src/verbs.rs").read_text()

# Scalar and aggregate functions are declared as `Variant => "name"`, and window functions as
# `(WindowFn::Variant, "name")`.
names = set(re.findall(r'=> "([a-z_0-9]+)"', functions))
names |= set(re.findall(r'\(WindowFn::\w+, "([a-z_0-9]+)"\)', functions))
# The other built-in functions are the strings of the `VERBS` list.
listed = re.search(r"const VERBS: \[&str; \d+\] = \[(.*?)\];", verbs, re.S).group(1)
names |= set(re.findall(r'"([a-z_0-9]+)"', listed))
# `desc` and `asc` are markers on sort keys, which the grammar colors on their own.
names -= {"desc", "asc"}

path = root / "editors/vscode/syntaxes/biggo.tmLanguage.json"
grammar = path.read_text()
pattern = re.compile(r'("name": "support\.function\.biggo",\s*"match": "\\\\b\()([a-z_0-9|]+)(\))')
found = pattern.search(grammar)
before = set(found.group(2).split("|"))
path.write_text(grammar[: found.start(2)] + "|".join(sorted(names)) + grammar[found.end(2) :])
print(f"{len(names)} names; added {sorted(names - before)}; removed {sorted(before - names)}")
