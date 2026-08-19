#!/usr/bin/env python3
"""Run the SQL this repository shows to people who have not installed it yet.

The `hello_world` block in the community descriptor is the extension's page in
the DuckDB community registry, and the README's blocks are the first thing a
reader sees. Nothing executed either of them. A `read_mt940` example asked for a
`currency` column, which MT does not put on a `:61:` line and the reader does not
declare, and it shipped that way: the SQL suite covers what the readers do, the
coverage gate covers what the columns do, and neither reads the documents.

The illustrative paths in those examples (`statements/*.xml`, `pacs008.xml`) name
nothing on disk, so each is replaced by a fixture that `sniff_iso20022` routes to
the same reader, which is how a file reaches a reader everywhere else in this
repository. A path that does exist is left alone.

Two failures are told apart, because they call for different repairs. A statement
DuckDB cannot bind names a column or a function that is not there, and it will
fail against every fixture, so it fails here at once. A statement that binds but
returns nothing on every routed fixture is an example the corpus does not
demonstrate, which is a hole in the corpus or in the example's WHERE clause.

Usage:
    configure/venv/bin/python3 scripts/check_hello_world.py [--extension PATH]

Exit status 0 means every statement bound and returned rows. Exit status 1 prints
one line per problem. Exit status 2 means the check could not run here, which is
not a documentation failure.
"""

from __future__ import annotations

import argparse
import re
import sys
from collections import defaultdict
from pathlib import Path

# The documents whose SQL is executed, and how the SQL is found in each: a YAML
# block scalar under `docs:`, and fenced ```sql blocks.
DESCRIPTOR = Path("community-extension/description.yml")
README = Path("README.md")

# What routes a fixture to a reader. `testdata/*` and not the two globs the
# coverage gate uses, because a gzipped fixture is routed too and its suffix says
# nothing about its content.
CORPUS = "testdata/*"

# Statements that set up a session rather than query one. They cannot run here:
# the registry is what INSTALL reads, and the extension under test is loaded from
# a path instead.
SETUP = ("install ", "load ")

# DuckDB says which layer refused. A binder or parser complaint is about the
# statement and repeats on every file; anything else is the reader's verdict on
# one fixture, so the next fixture gets a turn.
STATEMENT_ERRORS = ("Binder Error", "Parser Error", "Catalog Error")


def sql_literal(value: str) -> str:
    return value.replace("'", "''")


def descriptor_sql(text: str) -> str:
    """The `hello_world` block scalar, dedented by its four spaces."""
    match = re.search(r"^  hello_world: \|\n((?:    .*\n|\n)+)", text, re.M)
    if match is None:
        raise LookupError("no `hello_world: |` block in the descriptor")
    return "\n".join(line[4:] for line in match.group(1).split("\n"))


def readme_sql(text: str) -> str:
    return "\n\n".join(re.findall(r"^```sql\n(.*?)^```", text, re.M | re.S))


def statements(sql: str) -> list[str]:
    """The runnable statements in a block of SQL, comments and setup removed."""
    out = []
    for chunk in sql.split(";"):
        body = "\n".join(
            line for line in chunk.split("\n") if not line.strip().startswith("--")
        ).strip()
        if not body or body.lower().startswith(SETUP):
            continue
        out.append(body)
    return out


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument(
        "--extension",
        type=Path,
        default=Path("build/debug/quackiso.duckdb_extension"),
        help="the built extension to LOAD (default: %(default)s)",
    )
    args = parser.parse_args()

    if not args.extension.is_file():
        print(f"{args.extension} does not exist; `make debug` builds it", file=sys.stderr)
        return 2

    documents = []
    for path, extract in ((DESCRIPTOR, descriptor_sql), (README, readme_sql)):
        if not path.is_file():
            print(f"{path} does not exist", file=sys.stderr)
            return 2
        try:
            sql = extract(path.read_text(encoding="utf-8"))
        except LookupError as error:
            print(f"{path}: {error}", file=sys.stderr)
            return 2
        found = statements(sql)
        if not found:
            print(f"{path} holds no runnable SQL, which it did before", file=sys.stderr)
            return 2
        documents.append((path, found))

    import duckdb

    connection = duckdb.connect(config={"allow_unsigned_extensions": "true"})
    connection.execute(f"LOAD '{sql_literal(str(args.extension))}'")

    routed: dict[str, list[str]] = defaultdict(list)
    for source, reader in connection.execute(
        f"SELECT source_file, reader FROM sniff_iso20022('{sql_literal(CORPUS)}') "
        "WHERE reader IS NOT NULL ORDER BY source_file"
    ).fetchall():
        routed[reader].append(source.replace("\\", "/"))

    if not routed:
        print(f"{CORPUS} routed no files at all", file=sys.stderr)
        return 2

    problems: list[str] = []
    executed = 0
    exercised: set[str] = set()

    for path, found in documents:
        for statement in found:
            calls = re.findall(r"\b(read_\w+|sniff_iso20022)\('([^']*)'", statement)
            if not calls:
                problems.append(f"{path}: no table function in {statement.splitlines()[0]!r}")
                continue

            # One substitution per call, so a statement joining two readers gets a
            # fixture for each. `sniff_iso20022` takes the corpus itself.
            candidates: list[list[str]] = []
            for name, illustrative in calls:
                exercised.add(name)
                if Path(illustrative).exists():
                    candidates.append([illustrative])
                elif name == "sniff_iso20022":
                    candidates.append([CORPUS])
                elif routed.get(name):
                    candidates.append(routed[name])
                else:
                    problems.append(
                        f"{path}: {name} is called with '{illustrative}', which does not "
                        f"exist, and no fixture in {CORPUS} routes to {name}"
                    )
                    candidates.append([])
            if any(not choices for choices in candidates):
                continue

            # The calls are walked together: attempt N takes the Nth choice of
            # each, which is enough because a statement naming two readers is
            # naming two corpora, not a cross product worth exhausting.
            attempts = max(len(choices) for choices in candidates)
            verdict = ""
            for attempt in range(attempts):
                run = statement
                for (name, illustrative), choices in zip(calls, candidates):
                    real = choices[min(attempt, len(choices) - 1)]
                    run = run.replace(f"{name}('{illustrative}'", f"{name}('{real}'", 1)
                try:
                    rows = connection.execute(run).fetchall()
                except duckdb.Error as error:
                    first = str(error).splitlines()[0]
                    if any(layer in first for layer in STATEMENT_ERRORS):
                        verdict = f"does not bind: {first}"
                        break
                    verdict = f"last fixture said {first!r}"
                    continue
                if not rows:
                    verdict = "binds, but no routed fixture returns a row for it"
                    continue
                verdict = ""
                executed += 1
                break
            if verdict:
                problems.append(f"{path}: {statement.splitlines()[0]!r} {verdict}")

    if problems:
        print(f"{len(problems)} hello_world problem(s):", file=sys.stderr)
        for problem in problems:
            print(f"  {problem}", file=sys.stderr)
        return 1

    counts = ", ".join(f"{len(found)} in {path}" for path, found in documents)
    print(
        f"hello_world verified: {executed} statements executed ({counts}), "
        f"{len(exercised)} functions exercised, every one non-empty"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
