#!/usr/bin/env python3
"""Measure what a `read_iso20022` scan adds to a real DuckDB process.

`src/membound.rs` measures the parser standalone: a test binary with no DuckDB
in it, a tracking allocator, and `VmHWM` reset around one scan. That is the
number `README.md` quotes, and it is the reader's own cost.

It is not the same number as "what the extension adds to DuckDB". DuckDB is
already resident before a row is read -- its own allocator, catalog, and thread
pool -- so the honest second figure is an *increment over a live DuckDB
baseline*, measured in the process that actually runs the query. That is what
this script reports:

    RSS before the query   the DuckDB baseline, extension loaded, engine warm
    peak RSS during        VmHWM, reset immediately before the query
    added by the scan      the difference, which is what streaming buys

The statement is not generated here. One generator, two measurements:

    make release
    QUACKISO_MEMBOUND_KEEP=/var/tmp cargo test --release membound -- --ignored --nocapture
    configure/venv/bin/python3 scripts/measure_in_duckdb.py \\
        --fixture /var/tmp/quackiso-membound-documented.xml

The query is an aggregate on purpose. An aggregate streams: DuckDB consumes each
output chunk and drops it, so the resident set follows the reader's bound. A
query that returns every row materialises a result set, and that memory is
DuckDB's and the client's, not the parser's -- a different budget with a
different answer. `--materialise` prices that difference instead of asserting
it, and fails if it is not at least `--contrast` times the scan, because that
contrast is what README.md tells people.

Linux only: `/proc/self/status` is the only place `VmRSS` and `VmHWM` are both
exposed, and `/proc/self/clear_refs` is the only way to reset the peak so that
one query can be measured instead of the whole process.

Exit status 0 means every measurement stayed inside its bound. Exit status 1
means a memory ceiling, glob-copy consistency check, or materialisation contrast
failed. Exit status 2 means this host is missing the Linux /proc, fixture, or
extension prerequisites.
"""

from __future__ import annotations

import argparse
import os
import shutil
import sys
import tempfile
from pathlib import Path

STATUS = Path("/proc/self/status")
CLEAR_REFS = Path("/proc/self/clear_refs")
MIB = 1024 * 1024


def status_field(name: str) -> int:
    """A /proc/self/status size field, in bytes."""
    for line in STATUS.read_text(encoding="utf-8").splitlines():
        if line.startswith(name):
            return int(line.split()[1]) * 1024
    raise RuntimeError(f"{STATUS} has no {name} field")


def reset_peak_rss() -> None:
    """Drop VmHWM back to the current VmRSS. 5 is CLEAR_REFS_MM_HIWATER_RSS."""
    CLEAR_REFS.write_text("5\n", encoding="utf-8")


def mib(byte_count: float) -> str:
    return f"{byte_count / MIB:.2f} MiB"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument(
        "--fixture",
        required=True,
        type=Path,
        help="statement to scan, as written by QUACKISO_MEMBOUND_KEEP",
    )
    parser.add_argument(
        "--extension",
        type=Path,
        default=Path("build/release/quackiso.duckdb_extension"),
        help="the built extension to LOAD (default: %(default)s)",
    )
    parser.add_argument(
        "--ceiling-mib",
        type=float,
        default=16.0,
        help="fail if the scan adds more than this (default: %(default)s)",
    )
    parser.add_argument(
        "--threads",
        type=int,
        default=1,
        help="DuckDB threads; one file is a sequential scan either way (default: %(default)s)",
    )
    parser.add_argument(
        "--glob-copies",
        type=int,
        default=0,
        help="also scan this many hardlinked copies with threads := N, to price the parallel path",
    )
    parser.add_argument(
        "--glob-ceiling-mib",
        type=float,
        default=64.0,
        help="fail if the parallel scan adds more than this (default: %(default)s)",
    )
    parser.add_argument(
        "--materialise",
        action="store_true",
        help="also return every row instead of aggregating, to price the result set",
    )
    parser.add_argument(
        "--contrast",
        type=float,
        default=4.0,
        help="materialising must cost at least this many times the scan (default: %(default)s)",
    )
    args = parser.parse_args()

    if not STATUS.is_file():
        print("this measurement needs /proc/self/status; Linux only", file=sys.stderr)
        return 2
    for path in (args.fixture, args.extension):
        if not path.is_file():
            print(f"{path} does not exist", file=sys.stderr)
            return 2

    import duckdb

    connection = duckdb.connect(config={"allow_unsigned_extensions": "true", "threads": args.threads})
    extension = str(args.extension).replace("'", "''")
    connection.execute(f"LOAD '{extension}'")
    # Warm the engine so the baseline is a running DuckDB, not a cold one: the
    # first query allocates the buffer manager, the vector pool, and the
    # scheduler, and none of that belongs to the scan.
    connection.execute("SELECT 1").fetchall()

    baseline = status_field("VmRSS:")
    reset_peak_rss()

    fixture = str(args.fixture).replace("'", "''")
    rows, total = connection.execute(
        f"SELECT count(*), sum(amount) FROM read_iso20022('{fixture}')"
    ).fetchone()

    peak = status_field("VmHWM:")
    added = peak - baseline
    size = args.fixture.stat().st_size

    print(f"duckdb {duckdb.__version__}, extension {args.extension}")
    print(f"fixture {args.fixture}: {size / 1e9:.2f} GB")
    print(f"rows {rows}, sum(amount) {total}")
    print(f"RSS before the query  {mib(baseline):>12}")
    print(f"peak RSS during       {mib(peak):>12}")
    print(f"added by the scan     {mib(added):>12}  (ceiling {args.ceiling_mib:.2f} MiB)")

    if added > args.ceiling_mib * MIB:
        print(
            f"the scan added {mib(added)} over a {mib(baseline)} baseline, "
            f"more than the {args.ceiling_mib:.2f} MiB ceiling",
            file=sys.stderr,
        )
        return 1

    if args.glob_copies:
        # The extension parses a glob one worker per file, so the parallel path
        # has its own bound -- O(threads x batch), not O(corpus). Rows and money
        # are checked as well as memory, because a worker that claimed a file
        # twice or dropped one would otherwise look thrifty.
        #
        # The copies are hardlinks: same bytes, same inode, N filenames, no
        # second gigabyte on disk. The directory is made beside the fixture so
        # that the link cannot land on another filesystem -- eight real copies
        # of the documented statement would be fourteen gigabytes.
        workers = args.glob_copies
        directory = Path(tempfile.mkdtemp(prefix=".quackiso-glob-", dir=args.fixture.parent))
        try:
            for n in range(workers):
                copy = directory / f"copy{n:02}.xml"
                try:
                    os.link(args.fixture, copy)
                except OSError as refused:
                    print(
                        f"hardlink refused ({refused}); copying {size / 1e9:.2f} GB "
                        f"x {workers} instead",
                        file=sys.stderr,
                    )
                    shutil.copy2(args.fixture, copy)

            before = status_field("VmRSS:")
            reset_peak_rss()
            glob = str(directory / "*.xml").replace("'", "''")
            glob_rows, glob_total = connection.execute(
                f"SELECT count(*), sum(amount) FROM read_iso20022('{glob}', threads := {workers})"
            ).fetchone()
            parallel = status_field("VmHWM:") - before
        finally:
            shutil.rmtree(directory, ignore_errors=True)

        print(
            f"{workers} files, {workers} workers {mib(parallel):>11}"
            f"  (ceiling {args.glob_ceiling_mib:.2f} MiB)"
        )
        if glob_rows != rows * workers or glob_total != total * workers:
            print(
                f"the parallel scan returned {glob_rows} rows summing to {glob_total}, "
                f"not {rows * workers} summing to {total * workers}: a file was "
                f"claimed twice or dropped",
                file=sys.stderr,
            )
            return 1
        if parallel > args.glob_ceiling_mib * MIB:
            print(
                f"{workers} workers added {mib(parallel)}, more than the "
                f"{args.glob_ceiling_mib:.2f} MiB ceiling",
                file=sys.stderr,
            )
            return 1

    if not args.materialise:
        return 0

    # The README says an aggregate streams and a full result set does not. That
    # is a claim about memory, so it gets measured too: same file, same scan,
    # every row handed back instead of folded into two numbers. What this adds
    # is the result set and the client's copy of it -- DuckDB's budget and
    # Python's, never the parser's, whose bound was just measured above.
    before = status_field("VmRSS:")
    reset_peak_rss()
    returned = connection.execute(f"SELECT * FROM read_iso20022('{fixture}')").fetchall()
    materialised = status_field("VmHWM:") - before
    print(
        f"returning {len(returned)} rows {mib(materialised):>11}"
        f"  ({materialised / max(added, 1):.0f}x the scan)"
    )
    del returned

    if materialised < added * args.contrast:
        print(
            f"materialising cost {mib(materialised)} against {mib(added)} for the scan, "
            f"under the {args.contrast:.0f}x the documentation claims: recheck README.md",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
