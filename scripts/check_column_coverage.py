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

`sniff_iso20022` and `audit_addresses` are not values of a `reader` column, and
neither are the four supplementary camt readers, so routing alone reaches none
of them. The sniffer's own columns are covered by the SQL suite directly. The
other five are given a corpus here: the four camt grains take the files routed
to the entry reader, because they read those same files, and the audit takes the
routed union plus every message the sniffer identified with no reader behind it,
because it reads any family and a camt.107 cheque would otherwise go unmeasured.

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

# The four supplementary camt readers, and the reader whose routed files are
# theirs. Routing names one reader per family and camt.052/.053/.054 all name
# `read_iso20022`, so nothing routes to these four and their columns went
# unmeasured. They read the same files at four other grains, so the corpus that
# reaches them is that reader's.
ALIASED_TO = {
    "read_camt_transactions": "read_iso20022",
    "read_camt_balances": "read_iso20022",
    "read_camt_amount_details": "read_iso20022",
    "read_camt_remittance": "read_iso20022",
}

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
    # The audit walks the same two truncated documents the readers do, and breaks
    # in the same place with the same message: it is a parse of the same bytes.
    ("audit_addresses", "testdata/camt053_truncated.xml"): "syntax error: tag not closed",
    ("audit_addresses", "testdata/pain001_truncated.xml"): "end of input inside <CstmrCdtTrfInitn>",
    # The four supplementary readers walk the same bytes as the entry reader, so
    # they break in the same place. `camt053_bad_amount.xml` is not here for any
    # of them: none of their four grains projects an entry-level amount, and
    # that entry has no transaction, no balance and no amount block, so nothing
    # there reads it.
    ("read_camt_amount_details", "testdata/camt053_truncated.xml"): "syntax error: tag not closed",
    ("read_camt_balances", "testdata/camt053_truncated.xml"): "syntax error: tag not closed",
    ("read_camt_remittance", "testdata/camt053_truncated.xml"): "syntax error: tag not closed",
    ("read_camt_transactions", "testdata/camt053_truncated.xml"): "syntax error: tag not closed",
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
    # Files the sniffer identified without naming a reader for them. A valid
    # unsupported family is inventory to a reader and an ordinary message to the
    # audit, so this is where the audit's corpus is wider than routing.
    identified: list[str] = []
    for corpus in corpora:
        for source, reader, family, error in connection.execute(
            f"SELECT source_file, reader, family, error FROM sniff_iso20022('{sql_literal(corpus)}') "
            "ORDER BY source_file"
        ).fetchall():
            path = source.replace("\\", "/")
            if reader is not None:
                routed[reader].append(path)
            elif family is not None and error is None:
                identified.append(path)

    if not routed:
        print(f"{' '.join(corpora)} routed no files at all", file=sys.stderr)
        return 2

    # The four supplementary camt readers, which routing cannot reach: the
    # sniffer names one reader per family and camt.052/.053/.054 all name the
    # entry reader. They read the same files at four other grains, so they get
    # that reader's corpus.
    for alias, of in ALIASED_TO.items():
        routed[alias] = list(routed[of])

    # `audit_addresses` is not a reader and the sniffer never names one for it, so
    # routing alone cannot reach it and its columns went unmeasured. They are
    # columns like any other: one that is NULL for every file in the corpus is a
    # dead column, and the whole point of this gate is that such a column cannot
    # pass unseen. It reads both wire formats and every family, so its corpus is
    # the routed union plus the messages that resolved to a family with no reader
    # behind it - which is how a camt.107 cheque gets measured at all.
    routed["audit_addresses"] = sorted(
        {p for reader, paths in routed.items() if reader not in ALIASED_TO for p in paths}
        | set(identified)
    )

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
