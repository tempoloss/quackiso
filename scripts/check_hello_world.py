#!/usr/bin/env python3
"""Run the SQL this repository shows to people who have not installed it yet.

The `hello_world` block in the community descriptor is the extension's page in
the DuckDB community registry, and the README's blocks (and those of the docs
pages split out of it) are the first things a reader sees. Nothing executed
either of them. A `read_mt940` example asked for a
`currency` column, which MT does not put on a `:61:` line and the reader does not
declare, and it shipped that way: the SQL suite covers what the readers do, the
coverage gate covers what the columns do, and neither reads the documents.

The illustrative paths in those examples (`statements/*.xml`, `pacs008.xml`) name
nothing on disk, so each is replaced by a fixture that `sniff_iso20022` routes to
the same reader, which is how a file reaches a reader everywhere else in this
repository. A path that does exist is left alone. The two functions that are not
a reader take a corpus instead of a routed fixture: `sniff_iso20022` the whole of
it, `audit_addresses` the XML half, because it refuses a file that is not XML.

Two failures are told apart, because they call for different repairs. A statement
DuckDB cannot bind names a column or a function that is not there, and it will
fail against every fixture, so it fails here at once. A statement that binds but
returns nothing on every routed fixture is an example the corpus does not
demonstrate, which is a hole in the corpus or in the example's WHERE clause.

The catalogue `quackiso_functions()` is held to the loaded extension the same way:
the function names LOAD adds to `duckdb_functions()` must equal the names it
lists, and each of its examples runs through the routing above and must return
rows, so a function added without a row, or a row whose example rots, fails here
and not in a user's session.

Two kinds of statement are not a plain `SELECT * FROM reader('path')`. A
`SET VARIABLE` over `sniff_iso20022` is given the readable fixtures of `INBOX`
and checked by the variable it set, and the `getvariable(...)` read after it
runs as written, on that variable. A statement whose own comment says
`an empty result is the answer` (a duplicate check over a corpus that has none)
has to bind and run and is not made to return a row.

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
# The README and the docs pages it was split into, which hold the fenced SQL the
# README used to carry itself.
MARKDOWN = (
    Path("README.md"),
    Path("docs/reference.md"),
    Path("docs/address-audit.md"),
    Path("docs/types.md"),
    Path("docs/streaming.md"),
    Path("docs/recipes.md"),
)

# What routes a fixture to a reader. `testdata/*` and not the two globs the
# coverage gate uses, because a gzipped fixture is routed too and its suffix says
# nothing about its content.
CORPUS = "testdata/*"

# What `audit_addresses` gets. It reads MT as well as XML, so the narrowing is
# not about format: it refuses a truncated file, and the corpus keeps several on
# purpose. A glob that hits one fails as a statement about the corpus rather than
# about the example. The pacs.008 fixtures are intact, and
# `pacs008_address_formats.xml` carries all four address shapes, so an example
# that filters on `finding` still returns rows.
XML_CORPUS = "testdata/pacs008_*.xml"

# What `SET VARIABLE ... sniff_iso20022('inbox/*')` gets. The recipe reads the
# files the sniffer names in the next statement, so the corpus must be files the
# readers accept: `testdata/*` holds the truncated and bad-amount fixtures on
# purpose, and a list that reached them would fail the read and not the example.
INBOX = "testdata/camt053_s*.xml"

# The five supplementary camt readers, and the reader whose routed fixtures are
# theirs. Nothing routes to them - the sniffer names one reader per family and
# camt.052/.053/.054 all name the entry reader - so an example calling one of
# them has no fixture to be substituted with unless it is given the same ones.
ALIASED_TO = {
    "read_camt_transactions": "read_iso20022",
    "read_camt_balances": "read_iso20022",
    "read_camt_statements": "read_iso20022",
    "read_camt_amount_details": "read_iso20022",
    "read_camt_remittance": "read_iso20022",
}

# Statements that set up a session rather than query one. They cannot run here:
# the registry is what INSTALL reads, and the extension under test is loaded from
# a path instead.
SETUP = ("install ", "load ")

# DuckDB says which layer refused. A binder or parser complaint is about the
# statement and repeats on every file; anything else is the reader's verdict on
# one fixture, so the next fixture gets a turn.
STATEMENT_ERRORS = ("Binder Error", "Parser Error", "Catalog Error")

# A statement whose comment says this may return nothing, because nothing is the
# answer it looks for (a duplicate check over a corpus with no duplicate). It
# still has to bind and run; it is not made to return a row.
EMPTY_IS_THE_ANSWER = "an empty result is the answer"


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


def _body(chunk: str) -> str:
    return "\n".join(
        line for line in chunk.split("\n") if not line.strip().startswith("--")
    ).strip()


def statements(sql: str) -> list[str]:
    """The runnable statements in a block of SQL, comments and setup removed."""
    out = []
    for chunk in sql.split(";"):
        body = _body(chunk)
        if not body or body.lower().startswith(SETUP):
            continue
        out.append(body)
    return out


def empty_ok(sql: str) -> set[str]:
    """The statements whose own comment says an empty result is their answer."""
    return {_body(chunk) for chunk in sql.split(";") if EMPTY_IS_THE_ANSWER in chunk}


def function_names(connection) -> set[str]:
    """Every function name `duckdb_functions()` lists, built in or loaded."""
    return {row[0] for row in connection.execute("SELECT function_name FROM duckdb_functions()").fetchall()}


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
    quiet: set[str] = set()
    for path, extract in ((DESCRIPTOR, descriptor_sql), *((p, readme_sql) for p in MARKDOWN)):
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
        quiet |= empty_ok(sql)

    import duckdb

    connection = duckdb.connect(config={"allow_unsigned_extensions": "true"})
    before = function_names(connection)
    connection.execute(f"LOAD '{sql_literal(str(args.extension))}'")

    # The catalogue is held to what LOAD registered, in both directions, and each
    # of its examples is then run like any other statement below.
    problems: list[str] = []
    gained = function_names(connection) - before
    catalogue = connection.execute(
        "SELECT function_name, description, example FROM quackiso_functions()"
    ).fetchall()
    listed = [name for name, _, _ in catalogue]
    for name in sorted(gained - set(listed)):
        problems.append(f"{name} is registered and missing from quackiso_functions()")
    for name in sorted(set(listed) - gained):
        problems.append(f"quackiso_functions() lists {name}, which LOAD did not register")
    for name in sorted({name for name in listed if listed.count(name) > 1}):
        problems.append(f"quackiso_functions() lists {name} more than once")
    for name, description, example in catalogue:
        if not description or not example:
            problems.append(f"quackiso_functions(): {name} has no description or no example")
    documents.append(("quackiso_functions()", [example for _, _, example in catalogue]))

    routed: dict[str, list[str]] = defaultdict(list)
    for source, reader in connection.execute(
        f"SELECT source_file, reader FROM sniff_iso20022('{sql_literal(CORPUS)}') "
        "WHERE reader IS NOT NULL ORDER BY source_file"
    ).fetchall():
        routed[reader].append(source.replace("\\", "/"))

    if not routed:
        print(f"{CORPUS} routed no files at all", file=sys.stderr)
        return 2

    for alias, of in ALIASED_TO.items():
        routed[alias] = list(routed.get(of, []))

    executed = 0
    exercised: set[str] = set()

    for path, found in documents:
        for statement in found:
            setter = re.match(r"SET VARIABLE (\w+)", statement, re.I)
            calls = re.findall(
                r"\b(read_\w+|sniff_iso20022|audit_addresses)\('([^']*)'", statement
            )
            if not calls:
                # The one function with no path has nothing to route: run it as is.
                if re.search(r"\bFROM quackiso_functions\(\)", statement):
                    exercised.add("quackiso_functions")
                    if connection.execute(statement).fetchall():
                        executed += 1
                    else:
                        problems.append(f"{path}: {statement!r} returns no row")
                elif "getvariable(" in statement:
                    # A reader given the list a SET VARIABLE built just before it:
                    # the variable holds the routed files already.
                    exercised.update(re.findall(r"\b(read_\w+)\(getvariable\(", statement))
                    try:
                        rows = connection.execute(statement).fetchall()
                    except duckdb.Error as error:
                        problems.append(f"{path}: {statement.splitlines()[0]!r} said {str(error).splitlines()[0]!r}")
                        continue
                    if rows:
                        executed += 1
                    else:
                        problems.append(f"{path}: {statement.splitlines()[0]!r} returns no row")
                else:
                    problems.append(f"{path}: no table function in {statement.splitlines()[0]!r}")
                continue

            # One substitution per call, so a statement joining two readers gets a
            # fixture for each. The two non-readers take a corpus of their own.
            candidates: list[list[str]] = []
            for name, illustrative in calls:
                exercised.add(name)
                if Path(illustrative).exists():
                    candidates.append([illustrative])
                elif name == "sniff_iso20022":
                    candidates.append([INBOX if setter else CORPUS])
                elif name == "audit_addresses":
                    candidates.append([XML_CORPUS])
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
                if setter:
                    # SET VARIABLE returns no rows of its own: the variable it
                    # set is what has to be there.
                    rows = [
                        r
                        for r in connection.execute(
                            f"SELECT getvariable('{setter.group(1)}')"
                        ).fetchall()
                        if r[0]
                    ]
                if not rows and statement not in quiet:
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
        + (f" except {len(quiet)} whose comment says an empty result is the answer" if quiet else "")
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
