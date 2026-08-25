#!/usr/bin/env python3
"""Run other projects' ISO 20022 samples through the reader each one routes to.

The corpus in `testdata/` is this project's own: every file in it was added
because a reader had been wrong about it, so it says nothing about the shapes
nobody here has thought of yet. The last real defect was found by hand, feeding
another project's fixtures to these readers -- a pain.001 that stopped inside an
open `<CstmrCdtTrfInitn>` came back as zero rows and no error. This is that
experiment as a gate.

Five sources, fetched as immutable archives - crate tarballs by version, GitHub
tarballs by commit sha - so a run cannot change meaning because someone edited a
branch:

* iso20022-payment-core 0.5.0 -- 3 valid fixtures and 9 the crate files as
  invalid
* rust_iso20022 0.1.1 -- 3 message samples
* mx-message 3.1.4 -- 172 datafake scenarios, which ship no XML at all.
  tools/mxgen turns them into documents with an `<Envelope>` root and no
  namespace declaration anywhere, a shape no local fixture has, where identity
  can only come from the container name.
* swift-mt-message 3.1.5 -- datafake scenarios again, and again no MT text at
  all; tools/mtgen turns them into full `{1:}`..`{5:}` messages.
* wolph/mt940 and prowide/prowide-core -- the MT corpora, and the only published
  input the MT readers have. They are what a bank actually sends: entry dates
  padded with spaces, a `:86:` narrative wrapped mid-word, ACK envelopes
  interleaved with the messages they acknowledge, and one value date that reads
  `345454`.

Every file is routed by `sniff_iso20022` and read by the reader it names, one
child process per file. That is what makes a panic visible: a Rust panic
crossing the C ABI takes the process down with it, and the parent sees a dead
child instead of losing the whole run.

`audit_addresses` runs on the same file beside the reader. It is not a reader and
routing says nothing about it -- it takes any ISO 20022 XML and any SWIFT MT,
whatever the family, and refuses only bytes that are neither -- so nothing here
would exercise it otherwise. Running the two together is the point: they share
the sniffer's identity vocabulary, the wire walk and the MT framing, so a file
one accepts and the other refuses is a defect in whichever is wrong, and R7 is
that comparison. This corpus is where the audit earns its keep: 149 `:50K:` and
`:59:` name-and-address fields, which is the shape the 14 November 2026 rule
refuses, and not one `:50F:`, which is the shape that survives it.

The four supplementary camt readers run beside the primary one for every
camt.052, camt.053 and camt.054, and for the same reason: routing names one
reader per family, so nothing would call `read_camt_transactions`,
`read_camt_balances`, `read_camt_amount_details` or `read_camt_remittance`
otherwise. They walk the bytes the primary reader already walked, so a raise
from one of them where the primary succeeded is two walks of one statement
disagreeing, which is R9. R10 is the pair of them in agreement about nothing: a
camt.052 or camt.053 with no entries and no balances is not a statement, and
before there was a balance grain there was no way to tell that apart from a
statement of balances read correctly.

The rules are R1 to R10, spelled out in `RULES`. Two outcomes are not findings:
a valid ISO message quackiso has no reader for, which is inventory, and a
counted family that reports zero transactions and returns zero rows, which is
what a statement of balances alone produces.

The XSD is not consulted here, and ADR 0003 says why, so most of the nine
invalid fixtures parse without complaint: a missing required field is data, not
a syntax error. Expectations are therefore recorded from a live run by --record
and compared on every run, the same bargain `EXPECTED_ERRORS` strikes in
scripts/check_column_coverage.py. A recorded error holds the reason alone, with
the file name and the DuckDB error class stripped, so the record does not pin
itself to one corpus path.

Generated files are recorded too, keyed by the generated filename, and only on
the fields a rerun of the generator cannot move: the type, the family, the
reader, and the five row counts. Not the text, which datafake-rs invents afresh
every run, and not `audit_findings`, which moves with the invention. Without
that tier, 18 CBPR+ messages sniffed as "unrecognised message" for a whole
release and no rule noticed, because every rule read their NULL reader as
inventory. `--record` still runs every rule that is a claim about the file --
R1, R2, R4, R5, R7, R8, R9, R10 -- and writes nothing if one of them fires, so
recording cannot bless an unidentified, empty or crashing message.

Usage:
    configure/venv/bin/python3 scripts/sweep_foreign_corpora.py --fetch
    configure/venv/bin/python3 scripts/sweep_foreign_corpora.py
    configure/venv/bin/python3 scripts/sweep_foreign_corpora.py --record

Exit status 0 means every file was read the way the record says. Exit status 1
prints one line per finding and copies the file that produced each one into
`<corpus>/findings/`, which is the only evidence a generated file leaves. Exit
status 2 means the sweep could not run here, which is not a finding about
quackiso.
"""

from __future__ import annotations

import argparse
import fnmatch
import io
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

CORPUS_DIR = Path("target/foreign-corpus")
EXPECTATIONS = Path("scripts/foreign_corpus_expectations.json")
EXTENSION = Path("build/debug/quackiso.duckdb_extension")
# 3 records what the generated tier did as well, keyed by generated filename,
# and adds the four supplementary camt readers beside the primary one.
SCHEMA = 3
CRATES = "https://static.crates.io/crates"

# A whole DuckDB start plus one file. Generous, and still short enough that a
# reader stuck in a loop is a reported finding instead of a hung gate.
CHILD_TIMEOUT = 120

RULES = """R1 crash             the child printed no verdict, or never finished
R2 unexpected error  a raise the record does not account for
R3 missing error     the record holds an error the reader no longer raises
R4 silent empty      records > 0, zero rows, no error
R5 silent empty      an uncounted family returned zero rows with no error
R6 changed outcome   reader, row count, record count or audited parties moved
R7 audit drift       the address audit refused a file a reader read rows out of
R8 unidentified      generated input the sniffer could not name
R9 detail error      a supplementary camt reader raised where the primary did not
R10 empty statement  a camt.052/.053 report with neither entries nor balances"""

# The camt families whose statements the supplementary readers describe, and the
# four readers themselves beside the verdict field each one fills. They are not
# routed: `sniff_iso20022` names one reader per family, so nothing here would
# run them otherwise -- the same reason `audit_addresses` is called by hand.
STATEMENT_FAMILIES = ("camt.052", "camt.053", "camt.054")
SUPPLEMENTARY = (
    ("read_camt_transactions", "transaction"),
    ("read_camt_balances", "balance"),
    ("read_camt_amount_details", "amount_detail"),
    ("read_camt_remittance", "remittance"),
)

# What a generated file's outcome is recorded as. Randomised text is not among
# them and neither is `audit_findings`: datafake-rs invents different parties
# every run, and how many of them would be refused moves with the invention.
# How many there are does not, and neither does anything else here.
GENERATED_FIELDS = (
    "message_type",
    "family",
    "reader",
    "records",
    "rows",
    "audit_parties",
    "transaction_rows",
    "balance_rows",
    "amount_detail_rows",
    "remittance_rows",
)

# The seven families `record_elem_of` in src/sniff.rs:152-165 returns None for.
# `records` is always NULL for them, so R4 can never fire; each emits one row
# per message container, which
# `tests::the_investigation_readers_yield_one_row_per_message` pins.
UNCOUNTED_READERS = frozenset(
    {
        "read_camt027",
        "read_camt028",
        "read_camt030",
        "read_camt031",
        "read_camt036",
        "read_camt037",
        "read_camt087",
    }
)

# What to keep out of each tarball. `*` in these patterns crosses `/`, which is
# how `fixtures/*.xml` reaches both valid/ and invalid/. Licenses come along
# because the files are read on this machine and the terms should be next to
# them; nothing fetched here is ever committed.
PACKAGES = (
    ("iso20022-payment-core", "0.5.0", ("fixtures/*.xml", "LICENSE-MIT", "LICENSE-APACHE")),
    ("rust_iso20022", "0.1.1", ("tests/data/*.xml", "LICENSE")),
    ("mx-message", "3.1.4", ("test_scenarios/*", "LICENSE")),
    ("swift-mt-message", "3.1.5", ("test_scenarios/*", "LICENSE")),
)

# GitHub sources, pinned by commit sha. codeload serves a .tar.gz, so the same
# extraction reads it; the sha is the pin because a tag can move and GitHub does
# not promise byte-stable tarballs.
#
# wolph/mt940 is BSD-3-Clause (Rick van Hattem) and vendors three sample sets
# under their own permissive terms: betterplace Apache-2.0, cmxl MIT (Michael
# Bumann), jejik MIT (Frank Oxener, Agile Dovadi BV). prowide-core is
# Apache-2.0 with no NOTICE file.
REPOS = (
    (
        "wolph/mt940",
        "c634dc83fbb76beec35118aedd146ea3ad9a6c5d",
        ("mt940_tests/*.sta", "mt940_tests/*.txt", "mt940_tests/*LICENSE", "LICENSE"),
    ),
    (
        "prowide/prowide-core",
        "1bb510dee22f9093034688773864caee8113a09e",
        ("src/test/resources/*.fin", "src/test/resources/*.rje",
         "src/test/resources/*.txt", "LICENSE.txt"),
    ),
)
GITHUB = "https://codeload.github.com"

# Every suffix a corpus file may carry. MT arrives as .sta, .fin, .rje and .txt
# depending on who wrote it, and none of those says anything about the content:
# the sniffer decides what each file is.
CORPUS_SUFFIXES = ("*.xml", "*.txt", "*.sta", "*.fin", "*.rje")

READER_NAME = re.compile(r"^read_[a-z0-9_]+$")


def sql_literal(value: str) -> str:
    return value.replace("'", "''")


def extract(payload: bytes, destination: Path, keep: tuple[str, ...]) -> int:
    """Write the members of one archive that `keep` matches. Returns files written."""
    destination = destination.resolve()
    taken = 0
    with tarfile.open(fileobj=io.BytesIO(payload), mode="r:gz") as archive:
        members = archive.getmembers()
        if not members:
            return 0
        # The archive names its own root. A crates.io `.crate` spells it
        # `<name>-<version>` and a codeload tarball `<repo>-<sha>`, and neither
        # has to be guessed from the pin.
        root = members[0].name.split("/")[0]
        for member in members:
            if not member.isfile() or not member.name.startswith(root + "/"):
                continue
            relative = member.name[len(root) + 1 :]
            # No schema ever enters the corpus. These packages exclude their
            # xsds/ directories already; this is the belt to that braces.
            if relative.endswith(".xsd"):
                continue
            if not any(fnmatch.fnmatch(relative, pattern) for pattern in keep):
                continue

            target = (destination / relative).resolve()
            try:
                target.relative_to(destination)
            except ValueError:
                print(f"  refusing {member.name}: escapes {destination}", file=sys.stderr)
                continue

            extracted = archive.extractfile(member)
            if extracted is None:
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(extracted.read())
            taken += 1
    return taken


def fetch(corpus: Path) -> int:
    """Extract every pinned source into `<corpus>/static/`. Returns files written."""
    static = corpus / "static"
    static.mkdir(parents=True, exist_ok=True)
    written = 0

    for name, version, keep in PACKAGES:
        url = f"{CRATES}/{name}/{name}-{version}.crate"
        print(f"fetching {url}")
        with urllib.request.urlopen(url, timeout=180) as response:
            payload = response.read()
        root = f"{name}-{version}"
        taken = extract(payload, static / root, keep)
        print(f"  {root}: {taken} files")
        written += taken

    for repo, sha, keep in REPOS:
        url = f"{GITHUB}/{repo}/tar.gz/{sha}"
        print(f"fetching {url}")
        with urllib.request.urlopen(url, timeout=180) as response:
            payload = response.read()
        root = f"{repo.split('/')[1]}-{sha[:12]}"
        taken = extract(payload, static / root, keep)
        print(f"  {root}: {taken} files")
        written += taken

    return written


def corpus_files(corpus: Path, sources: list[str]) -> list[tuple[str, Path]]:
    """Every corpus file to sweep, as (tier, path), ordered for a stable report."""
    found: list[tuple[str, Path]] = []
    for tier in sources:
        root = corpus / tier
        if not root.is_dir():
            continue
        matched = {path for suffix in CORPUS_SUFFIXES for path in root.rglob(suffix)}
        # A licence came along for attribution, not to be read as a message.
        found.extend(
            (tier, path) for path in sorted(matched) if not path.name.startswith("LICENSE")
        )
    return found


def relative_key(corpus: Path, tier: str, path: Path) -> str:
    return path.relative_to(corpus / tier).as_posix()


def error_reason(first_line: str, path: Path) -> str:
    """The part of an error that says what was wrong.

    The readers prefix the file name and DuckDB prefixes its own error class.
    Neither travels: the path depends on --corpus-dir and on which separator the
    platform spells it with, so recording the whole line would pin the record to
    one machine. Comparison stays a substring match against the full line, so
    the reason alone is enough to match.
    """
    for spelling in (str(path), str(path).replace("\\", "/"), str(path).replace("/", "\\")):
        marker = spelling + ": "
        cut = first_line.find(marker)
        if cut >= 0:
            return first_line[cut + len(marker) :]
    return first_line


def read_one(path: Path, extension: Path) -> int:
    """The child. Print one verdict object for `path`, then leave."""
    import duckdb

    connection = duckdb.connect(config={"allow_unsigned_extensions": "true"})
    connection.execute(f"LOAD '{sql_literal(str(extension))}'")

    literal = sql_literal(str(path))
    verdict: dict[str, object] = {
        "file": str(path),
        "message_type": None,
        "family": None,
        "reader": None,
        "records": None,
        "sniff_error": None,
        "sniff_raised": None,
        "rows": None,
        "reader_error": None,
        "audit_parties": None,
        "audit_findings": None,
        "audit_error": None,
    }
    for _, field in SUPPLEMENTARY:
        verdict[f"{field}_rows"] = None
        verdict[f"{field}_error"] = None

    try:
        row = connection.execute(
            "SELECT message_type, family, reader, records, error "
            f"FROM sniff_iso20022('{literal}')"
        ).fetchone()
    except duckdb.Error as error:
        verdict["sniff_raised"] = error_reason(str(error).splitlines()[0], path)
        print(json.dumps(verdict))
        return 0

    if row is not None:
        verdict["message_type"] = row[0]
        verdict["family"] = row[1]
        verdict["reader"] = row[2]
        verdict["records"] = None if row[3] is None else int(row[3])
        verdict["sniff_error"] = row[4]

    reader = verdict["reader"]
    if isinstance(reader, str):
        if not READER_NAME.match(reader):
            verdict["sniff_raised"] = f"sniff named a reader this script will not run: {reader!r}"
        else:
            try:
                counted = connection.execute(
                    f"SELECT count(*) FROM {reader}('{literal}')"
                ).fetchone()
                verdict["rows"] = None if counted is None else int(counted[0])
            except duckdb.Error as error:
                verdict["reader_error"] = error_reason(str(error).splitlines()[0], path)

    # The supplementary camt readers, which routing cannot reach: the sniffer
    # names one reader per family and camt.052/.053/.054 all name the primary
    # one. They are called only when that primary one succeeded, so a message
    # of the wrong family or a statement that holds no <Stmt> is one finding
    # here rather than five copies of it. A direct call still raises the same
    # refusal; nothing about it is softened, only counted once.
    if verdict["family"] in STATEMENT_FAMILIES and not verdict["reader_error"]:
        for name, field in SUPPLEMENTARY:
            try:
                counted = connection.execute(
                    f"SELECT count(*) FROM {name}('{literal}')"
                ).fetchone()
                verdict[f"{field}_rows"] = None if counted is None else int(counted[0])
            except duckdb.Error as error:
                verdict[f"{field}_error"] = error_reason(str(error).splitlines()[0], path)

    # Two counts and not the rows: which parties a generated file names is
    # different every run, but how many of them there are is not, and the count
    # of findings is the number the mandate is about.
    try:
        audited = connection.execute(
            "SELECT count(*), count(finding) " f"FROM audit_addresses('{literal}')"
        ).fetchone()
        if audited is not None:
            verdict["audit_parties"] = int(audited[0])
            verdict["audit_findings"] = int(audited[1])
    except duckdb.Error as error:
        verdict["audit_error"] = error_reason(str(error).splitlines()[0], path)

    print(json.dumps(verdict))
    return 0


def ask_child(path: Path, extension: Path) -> tuple[dict | None, str]:
    """Run one file in its own process. Returns the verdict, or why there is none."""
    try:
        completed = subprocess.run(
            [
                sys.executable,
                str(Path(__file__).resolve()),
                "--one",
                str(path),
                "--extension",
                str(extension),
            ],
            capture_output=True,
            text=True,
            timeout=CHILD_TIMEOUT,
        )
    except subprocess.TimeoutExpired:
        # A reader that never returns would otherwise hang the sweep instead of
        # reporting the file that did it, which is the one thing this has to do.
        return None, f"child did not finish inside {CHILD_TIMEOUT}s"
    for line in reversed(completed.stdout.splitlines()):
        if line.startswith("{"):
            try:
                return json.loads(line), ""
            except json.JSONDecodeError:
                break

    detail = f"child exited {completed.returncode} with no verdict"
    noise = (completed.stderr or completed.stdout).strip().splitlines()
    if noise:
        detail += f": {noise[-1]}"
    return None, detail


def load_expectations() -> tuple[dict[str, dict], dict[str, dict]]:
    """The recorded outcomes, static tier and generated tier."""
    if not EXPECTATIONS.is_file():
        return {}, {}
    document = json.loads(EXPECTATIONS.read_text(encoding="utf-8"))
    if document.get("schema") != SCHEMA:
        raise SystemExit(f"{EXPECTATIONS}: schema {document.get('schema')!r} is not {SCHEMA}")
    return document.get("files", {}), document.get("generated", {})


def write_expectations(static: dict[str, dict], generated: dict[str, dict]) -> None:
    document = {
        "schema": SCHEMA,
        "note": (
            "Outcomes of the fetched foreign corpus. Written by --record, checked on "
            "every run. A change here is a behaviour change: read it before accepting it."
        ),
        "files": {key: static[key] for key in sorted(static)},
        "generated": {key: generated[key] for key in sorted(generated)},
    }
    EXPECTATIONS.write_text(
        json.dumps(document, indent=1, ensure_ascii=False) + "\n", encoding="utf-8", newline=""
    )


def generated_outcome(verdict: dict) -> dict:
    """What a generated file's outcome is compared on, run against run."""
    return {field: verdict.get(field) for field in GENERATED_FIELDS}


def judge(
    tier: str,
    key: str,
    verdict: dict,
    expected: dict | None,
    recording: bool = False,
) -> tuple[list[tuple[str, str]], list[str]]:
    """The rules. Returns (findings, unrecorded paste lines) for one file.

    `recording` skips the two rules that are a comparison with the record --
    R3 and R6 -- and nothing else. Every rule that is a claim about the file
    itself still runs under `--record`, so a run that rewrites the record
    cannot bless an unidentified, empty, contradictory or crashing message on
    the way past.
    """
    findings: list[tuple[str, str]] = []
    unrecorded: list[str] = []

    reader = verdict.get("reader")
    error = verdict.get("reader_error")
    records = verdict.get("records")
    rows = verdict.get("rows")
    audit_error = verdict.get("audit_error")
    family = verdict.get("family")

    # The sniffer's contract is to report a bad document in its `error` column
    # and return a row anyway, so a raise from it is never an expected outcome
    # and there is no field to record one in. `sniff_error` is the column, which
    # a truncated file populates as a matter of course; `sniff_raised` is the
    # exception, and only that one is a finding.
    if verdict.get("sniff_raised"):
        findings.append(("R2", f"sniff_iso20022 raised: {verdict['sniff_raised']}"))

    if tier == "static" and expected is None and not recording:
        unrecorded.append(key)
    elif tier == "static" and not recording:
        recorded_error = expected.get("error")
        if error and not recorded_error:
            findings.append(("R2", f"the reader raised where the record says it does not: {error}"))
        elif error and recorded_error not in error:
            findings.append(
                ("R6", f"raised {error!r}, the record was written against {recorded_error!r}")
            )
        elif recorded_error and not error:
            findings.append(("R3", f"the record holds {recorded_error!r}, nothing raised"))

        if reader != expected.get("reader"):
            findings.append(
                ("R6", f"routes to {reader!r}, the record says {expected.get('reader')!r}")
            )
        if not error and not recorded_error:
            if rows != expected.get("rows"):
                findings.append(("R6", f"returned {rows} rows, the record says {expected.get('rows')}"))
            if records != expected.get("records"):
                findings.append(
                    ("R6", f"sniffed {records} records, the record says {expected.get('records')}")
                )

        recorded_audit = expected.get("audit_error")
        if audit_error and not recorded_audit:
            findings.append(
                ("R2", f"audit_addresses raised where the record says it does not: {audit_error}")
            )
        elif audit_error and recorded_audit not in audit_error:
            findings.append(
                (
                    "R6",
                    f"audit_addresses raised {audit_error!r}, the record was written "
                    f"against {recorded_audit!r}",
                )
            )
        elif recorded_audit and not audit_error:
            findings.append(
                ("R3", f"the record holds audit error {recorded_audit!r}, nothing raised")
            )
        elif not audit_error:
            for column, name in (("audit_parties", "parties"), ("audit_findings", "findings")):
                if verdict.get(column) != expected.get(column):
                    findings.append(
                        (
                            "R6",
                            f"audited {verdict.get(column)} {name}, the record says "
                            f"{expected.get(column)}",
                        )
                    )
    elif tier == "generated":
        if error:
            # Generated input has already been through mxgen, which rounds the
            # amounts datafake-rs invents down to the five fraction digits ISO
            # 20022 allows. MXMessage itself does not: it serialises an f64 at
            # full float precision, and that alone accounted for 101 of the 170
            # files refusing to read. Past that, a raise here has no record to
            # consult.
            findings.append(("R2", f"generated input raised: {error}"))
        if audit_error:
            findings.append(("R2", f"generated input raised in audit_addresses: {audit_error}"))

        # R8. A generated file is a message this project's own generator wrote
        # out of a published scenario, so the sniffer having nothing to say
        # about it is a defect in the sniffer and not news about the file. It
        # fires whether or not a reader was named: 18 CBPR+ messages sniffed as
        # "unrecognised message" with a NULL family and a NULL reader, and every
        # rule here read the NULL reader as "inventory" and passed them.
        if verdict.get("sniff_error"):
            findings.append(
                ("R8", f"the sniffer named nothing: {verdict['sniff_error']}")
            )

    # R4 and R5 hold whatever the record says. A silent empty result is the class
    # the truncation bug belonged to, and a gate that can record one away has
    # nothing left to catch.
    if reader and not error:
        if records is not None and records > 0 and rows == 0:
            findings.append(("R4", f"sniffed {records} records, {reader} returned no rows"))
        elif reader in UNCOUNTED_READERS and rows == 0:
            findings.append(("R5", f"{reader} returned no rows for an uncounted family"))

    # R9. The four supplementary readers walk the same statement the primary one
    # walked. It succeeded, so a raise from one of them is a disagreement
    # between two walks of the same bytes rather than a verdict on the file.
    for name, field in SUPPLEMENTARY:
        detail = verdict.get(f"{field}_error")
        if detail:
            findings.append(("R9", f"{name} raised where {reader} did not: {detail}"))

    # R10. A camt.052 or camt.053 states an account's position: entries, or the
    # balances alone when nothing moved. Neither of them is not a statement, and
    # before `read_camt_balances` existed there was no way to tell that apart
    # from a statement of balances read correctly. camt.054 is excluded: its
    # entries are 0..n and its schema has no <Bal> at all.
    if (
        family in ("camt.052", "camt.053")
        and reader
        and not error
        and not any(verdict.get(f"{field}_error") for _, field in SUPPLEMENTARY)
        and rows == 0
        and verdict.get("balance_rows") == 0
    ):
        findings.append(("R10", "a report or statement has neither entries nor balances"))

    # The audit reads both wire formats, so its refusals are claims about the file
    # and never about which format it is: no message found, or bytes that are
    # neither. A reader that returned rows out of the same file has disproved
    # both, whatever it was. The reverse is not a finding: a reader raising on an
    # amount while the audit reads the addresses beside it is two functions
    # reading different parts, and a valid family quackiso has no reader for is
    # inventory.
    if reader and rows is not None and not error and audit_error:
        findings.append(
            ("R7", f"{reader} returned {rows} rows, audit_addresses refused it: {audit_error}")
        )

    return findings, unrecorded


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument(
        "--extension",
        type=Path,
        default=EXTENSION,
        help="the built extension to LOAD (default: %(default)s)",
    )
    parser.add_argument(
        "--corpus-dir",
        type=Path,
        default=CORPUS_DIR,
        help="where the fetched and generated corpus lives (default: %(default)s)",
    )
    parser.add_argument(
        "--fetch", action="store_true", help="download and extract every pinned source, then stop"
    )
    parser.add_argument(
        "--record",
        action="store_true",
        help=f"write {EXPECTATIONS} from this run instead of checking against it",
    )
    parser.add_argument("--one", type=Path, help="internal: sweep one file and print its verdict")
    parser.add_argument(
        "--sources",
        default="static,generated",
        help="which tiers to sweep (default: %(default)s)",
    )
    args = parser.parse_args()

    if args.one is not None:
        if not args.extension.is_file():
            print(f"{args.extension} does not exist", file=sys.stderr)
            return 2
        return read_one(args.one, args.extension)

    if args.fetch:
        written = fetch(args.corpus_dir)
        print(f"fetched {written} files into {args.corpus_dir / 'static'}")
        return 0 if written else 2

    if not args.extension.is_file():
        print(f"{args.extension} does not exist; `make debug` builds it", file=sys.stderr)
        return 2

    sources = [tier for tier in args.sources.split(",") if tier]
    unknown = [tier for tier in sources if tier not in ("static", "generated")]
    if unknown:
        print(f"--sources: {', '.join(unknown)} is not a tier", file=sys.stderr)
        return 2

    files = corpus_files(args.corpus_dir, sources)
    if not files:
        print(
            f"{args.corpus_dir} holds nothing to sweep for {args.sources}; --fetch downloads "
            "the static tier and tools/mxgen and tools/mtgen write the generated one",
            file=sys.stderr,
        )
        return 2

    # `--record` rewrites both tiers, so it cannot be given half a corpus: a
    # record written from `--sources static` alone would drop every generated
    # file it does not know about, and the next normal run would call that the
    # record being up to date.
    if args.record and sorted(sources) != ["generated", "static"]:
        print(
            "--record writes both tiers and needs both: drop --sources, or pass "
            "static,generated",
            file=sys.stderr,
        )
        return 2

    # It writes the record and never consults it, and it has to work when the
    # record on disk is the previous schema: that is the run that replaces it.
    recorded_static, recorded_generated = ({}, {}) if args.record else load_expectations()

    # Every finding is copied out, so last run's evidence cannot be read as this
    # run's. The directory is what CI uploads.
    findings_dir = args.corpus_dir / "findings"
    if findings_dir.exists():
        shutil.rmtree(findings_dir)

    workers = min(8, (os.cpu_count() or 1) + 1)
    with ThreadPoolExecutor(max_workers=workers) as pool:
        answers = list(pool.map(lambda item: ask_child(item[1], args.extension), files))

    findings: list[str] = []
    unrecorded: list[str] = []
    observed: dict[str, dict] = {}
    observed_generated: dict[str, dict] = {}
    exercised: set[str] = set()
    audited = 0
    counted = {"static": 0, "generated": 0}
    guilty: list[tuple[str, Path]] = []

    for (tier, path), (verdict, why) in zip(files, answers):
        key = relative_key(args.corpus_dir, tier, path)
        counted[tier] += 1

        if verdict is None:
            findings.append(f"R1: {tier}/{key} - {why}")
            guilty.append((tier, path))
            continue

        if verdict.get("reader"):
            exercised.add(str(verdict["reader"]))
        if verdict.get("audit_error") is None:
            audited += 1

        if tier == "static":
            observed[key] = {
                "reader": verdict.get("reader"),
                "error": verdict.get("reader_error"),
                "rows": verdict.get("rows"),
                "records": verdict.get("records"),
                "audit_error": verdict.get("audit_error"),
                "audit_parties": verdict.get("audit_parties"),
                "audit_findings": verdict.get("audit_findings"),
            }
        else:
            observed_generated[key] = generated_outcome(verdict)

        rules, missing = judge(
            tier,
            key,
            verdict,
            recorded_static.get(key),
            recording=args.record,
        )
        for rule, detail in rules:
            findings.append(f"{rule}: {tier}/{key} - {detail}")
        if rules:
            guilty.append((tier, path))
        unrecorded.extend(missing)

    # The generated tier is compared on the fields a rerun of the generator
    # cannot move: the scenario names are stable, so the filename is the key and
    # a scenario that stops being written is as much a change as one that starts
    # reading differently.
    if not args.record and "generated" in sources:
        for key in sorted(set(observed_generated) | set(recorded_generated)):
            expected = recorded_generated.get(key)
            got = observed_generated.get(key)
            if expected is None:
                findings.append(f"R6: generated/{key} - the record does not name this file")
            elif got is None:
                findings.append(
                    f"R6: generated/{key} - the record names a file the generator no longer writes"
                )
            else:
                for field in GENERATED_FIELDS:
                    if got.get(field) != expected.get(field):
                        findings.append(
                            f"R6: generated/{key} - {field} is {got.get(field)!r}, "
                            f"the record says {expected.get(field)!r}"
                        )

    if args.record and not findings:
        write_expectations(observed, observed_generated)
        print(
            f"recorded {len(observed)} static and {len(observed_generated)} generated "
            f"outcomes in {EXPECTATIONS}"
        )
        return 0

    if not args.record:
        for key in sorted(recorded_static):
            if key not in observed and "static" in sources:
                findings.append(
                    f"R6: static/{key} - the record names a file the corpus no longer has"
                )

    if findings or unrecorded:
        findings_dir.mkdir(parents=True, exist_ok=True)
        for tier, path in guilty:
            flattened = f"{tier}__{relative_key(args.corpus_dir, tier, path).replace('/', '__')}"
            shutil.copyfile(path, findings_dir / flattened)

        print(f"{len(findings) + len(unrecorded)} foreign corpus finding(s):", file=sys.stderr)
        for finding in findings:
            print(f"  {finding}", file=sys.stderr)
        if unrecorded:
            print(
                f"  unrecorded static outcomes; `--record` writes them to {EXPECTATIONS}:",
                file=sys.stderr,
            )
            for key in sorted(unrecorded):
                print(f"    {key}", file=sys.stderr)
        if guilty:
            print(f"  the files that produced them are in {findings_dir}", file=sys.stderr)
        print(f"\n{RULES}", file=sys.stderr)
        return 1

    print(
        f"foreign corpus sweep verified: {counted['static']} static files, "
        f"{counted['generated']} generated files, {len(exercised)} readers exercised, "
        f"{audited} files audited for addresses, 0 findings"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
