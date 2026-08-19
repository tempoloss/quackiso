#!/usr/bin/env python3
"""Verify that every column a reader declares is populated by some fixture.

A column no fixture exercises is a column no test can be wrong about: the
element name in the serde model can be misspelled, the flatten can drop it, and
every query still returns the NULL the corpus was going to return anyway. The
corpus is judged by routing, because routing is how a file reaches a reader:
`sniff_iso20022` names the reader for each file, and a column is covered when
some file routed to that reader carries a value for it.

The hole this closes is the error. A reader that raises on a routed file
contributes no rows, so every column that file would have covered drops out of
the count with nothing to show for it. Reading errors are therefore recorded
rather than skipped: a raising pair must appear in EXPECTED_ERRORS with the
message it raises, and a pair that stops raising fails too, so the list cannot
rot into a blanket excuse.

`sniff_iso20022` is not checked here. It never appears as a value of its own
`reader` column, so routing says nothing about which of its columns are live.

Usage:
    configure/venv/bin/python3 scripts/check_column_coverage.py [--extension PATH] [--corpus GLOB ...]

Exit status 0 means every routed reader has every column populated and every
reading error was expected. Exit status 1 prints one line per problem. Exit
status 2 means the check could not run here, which is not a coverage failure.
"""

from __future__ import annotations

import argparse
import sys
from collections import defaultdict
from pathlib import Path

# Both corpora, because `sniff_iso20022` routes both: XML by its namespace or
# container, MT by its block structure. A column NULL in every file routed to a
# reader is a column no query can be wrong about, whether the file is XML or FIN.
CORPUS = ("testdata/*.xml", "testdata/*.txt")

# Files sniff routes to a reader that refuses them, and the message each raises.
# A truncated document, an amount that is not a number, an envelope with no
# message inside: the corpus keeps these on purpose and the SQL suite asserts
# them. They are named here so that an error can never quietly take a column's
# only coverage with it.
#
# The value is matched as a substring, and the readers prefix theirs with the
# file name, which is spelled with the platform separator: record the part that
# says what was wrong, not the whole line the run printed.
EXPECTED_ERRORS: dict[tuple[str, str], str] = {
    ("read_iso20022", "testdata/camt053_bad_amount.xml"): 'amount "10.1234567" has 7 fraction digits',
    ("read_iso20022", "testdata/camt053_truncated.xml"): "syntax error: tag not closed",
    ("read_pacs008", "testdata/envelope_no_message.xml"): "no <FIToFICstmrCdtTrf> found",
    ("read_pacs028", "testdata/pacs028_bad_amount.xml"): 'amount "18500.1234567" has 7 fraction digits',
    ("read_pain001", "testdata/pain001_truncated.xml"): "end of input inside <CstmrCdtTrfInitn>",
    ("read_pain002", "testdata/pain002_v1_unsupported.xml"): "no <OrgnlGrpInfAndSts> found",
}


def sql_literal(value: str) -> str:
    return value.replace("'", "''")


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
    parser.add_argument(
        "--corpus",
        action="append",
        help=f"a glob every reader is measured over, repeatable (default: {' '.join(CORPUS)})",
    )
    args = parser.parse_args()

    if not args.extension.is_file():
        print(f"{args.extension} does not exist; `make debug` builds it", file=sys.stderr)
        return 2

    import duckdb

    connection = duckdb.connect(config={"allow_unsigned_extensions": "true"})
    connection.execute(f"LOAD '{sql_literal(str(args.extension))}'")

    corpora = args.corpus or list(CORPUS)
    routed: dict[str, list[str]] = defaultdict(list)
    for corpus in corpora:
        for source, reader in connection.execute(
            f"SELECT source_file, reader FROM sniff_iso20022('{sql_literal(corpus)}') "
            "WHERE reader IS NOT NULL ORDER BY source_file"
        ).fetchall():
            routed[reader].append(source.replace("\\", "/"))

    if not routed:
        print(f"{' '.join(corpora)} routed no files at all", file=sys.stderr)
        return 2

    problems: list[str] = []
    unrecorded: list[str] = []
    raised: set[tuple[str, str]] = set()
    columns = files_read = 0

    for reader in sorted(routed):
        names = [
            row[0]
            for row in connection.execute(
                f"DESCRIBE SELECT * FROM {reader}('{sql_literal(routed[reader][0])}')"
            ).fetchall()
        ]
        columns += len(names)
        live = dict.fromkeys(names, 0)
        counted = 0
        for path in routed[reader]:
            counts = ", ".join(f'count("{name}")' for name in names)
            try:
                got = connection.execute(
                    f"SELECT {counts} FROM {reader}('{sql_literal(path)}')"
                ).fetchone()
            except duckdb.Error as error:
                first = str(error).splitlines()[0]
                raised.add((reader, path))
                expected = EXPECTED_ERRORS.get((reader, path))
                if expected is None:
                    unrecorded.append(f'    ("{reader}", "{path}"): "{first}",')
                elif expected not in first:
                    problems.append(
                        f"{reader} on {path} raised {first!r}, "
                        f"EXPECTED_ERRORS was written against {expected!r}"
                    )
                continue
            counted += 1
            files_read += 1
            for name, value in zip(names, got):
                live[name] += value
        if not counted:
            problems.append(f"{reader}: every routed file raised, so nothing is covered")
        for name in names:
            if live[name] == 0:
                problems.append(f"{reader}.{name} is NULL in every file routed to {reader}")

    for reader, path in sorted(EXPECTED_ERRORS):
        if (reader, path) not in raised:
            problems.append(
                f"EXPECTED_ERRORS names {reader} on {path}, which no longer raises there"
            )

    if problems or unrecorded:
        print(f"{len(problems) + len(unrecorded)} column coverage problem(s):", file=sys.stderr)
        for problem in problems:
            print(f"  {problem}", file=sys.stderr)
        if unrecorded:
            print("  unrecorded reading errors, paste into EXPECTED_ERRORS:", file=sys.stderr)
            for line in sorted(unrecorded):
                print(f"  {line}", file=sys.stderr)
        return 1

    print(
        f"column coverage verified: {len(routed)} readers, {files_read} files read, "
        f"{columns} columns, {len(EXPECTED_ERRORS)} expected errors"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
