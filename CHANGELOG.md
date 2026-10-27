# Changelog

## 1.9.0 (2026-10-27)

**DuckDB versions.** Unchanged from 1.8.0: one binary built against the stable
C API at minimum v1.5.6 loads into DuckDB 2.0.0 and into 1.5.6 and later 1.5
releases. On DuckDB 1.5.5, stay on 1.7.0.

**Statement headers.** `read_camt_statements` returns one row per camt.052,
camt.053 or camt.054 statement: message creation and pagination, the electronic
and legal sequence numbers, the period, the copy and pagination flags, the
account owner and servicer, and the transaction summary, beside the number of
entries and balances the statement actually holds. Continuity and duplicate
statements across files can now be checked in SQL, as `read_mt940` already
allowed for MT.

**A function catalogue.** `quackiso_functions()` lists every function with a
one-sentence description and a runnable example. DuckDB's C API cannot attach
either to a table function yet, so `duckdb_functions()` shows neither; the build
checks the catalogue against what `LOAD` registers and runs every example.

**A list of paths.** Every function takes a list of files and globs as well as
one. The resolved files are read in argument order, and a file named twice is
read once, so it is not summed twice. With `SET VARIABLE` and `getvariable`, the
inbox recipe (sniff, then read) runs in one SQL session. When a file fails a
reader that `sniff_iso20022` would not choose for it, the error ends with the
reader to use. A path that is not text, such as `42`, is now an error instead of
a file named `42`.

**Declared encodings.** An XML file that declares Latin-1, Windows-1251 or
another non-UTF-8 encoding, or that starts with a UTF-16 byte order mark, is
decoded on the way in, so its names read correctly. Bytes that are malformed in
the declared encoding fail the scan. Nothing is guessed: SWIFT MT and XML
without a declaration stay UTF-8, and their errors say how to convert the file.
A byte that is not UTF-8 inside a CDATA section now fails the scan as it does
anywhere else, instead of becoming a replacement character.

**camt detail.** `read_iso20022` gains `additional_entry_info`.
`read_camt_transactions` gains `additional_transaction_info`, the local
instrument, purpose and return reason as code and proprietary columns, and the
charges total, filled only when exactly one total is stated.
`read_camt_remittance` gains the `REFERRED_DOCUMENT` slot, the invoice number
from `RfrdDocInf/Nb` that cash application matches on.

**Stability.** A NULL path aborted the DuckDB process; it is now an error. A
panic in a worker thread dropped that file's rows from a multi-file scan without
an error; it now fails the query, and the short MT option-F line that caused it
no longer panics. Every scan error names its file, and a folder given as the
path says to read `folder/*`. `threads := NULL` now means the default instead of
a sequential scan.

**Fixes.**

- SWIFT MT: a byte order mark hid the first statement, and a file with CR-only
  line ends read as no message. `:71F:` took the first occurrence, not the last,
  which SWIFT defines as the sender's charges. A `:61:` reference with non-ASCII
  text failed the scan. `sniff_iso20022` named the first message type in a file
  rather than the first one with a reader, so an MT199 before an MT103 hid the
  MT103.
- camt: a self-closing `<Ntry/>` took an `entry_index` in the supplementary
  readers but not in `read_iso20022`, so the join paired an entry with the
  transactions of the entry before it.
- pacs.007 reads the pre-2009 reversal spellings `RvslRsn`, `AddtlRvslRsnInf`
  and `RvslOrgtr`. camt.029 and camt.056 transaction rows fall back to the
  message-level case id. pacs.004 takes the original agents from the return
  chain when `OrgnlTxRef` states none.

**Observable changes.** Three results change for files that read before:

- `read_mt940.account` is the whole `:25:` value, as in `read_mt942`:
  `PL61 1090 1014 0000 0712 1981 2874` was returned as `PL61`.
- `remittance_count` counts invoice numbers, so an entry with free text and an
  invoice number has `remittance_count = 2` and NULL `remittance_info`.
  `read_camt_remittance` returns both.
- An amount element that is present and blank, and a `:61:` amount written with
  `.`, fail the scan. They used to read as NULL and as a number cut at the point.

**Documentation.** `docs/recipes.md` holds SQL for the common jobs: the inbox,
batched entries, reconciling a statement, continuity across files, MT940 beside
camt.053, pain.001 rejects and invoice numbers. Every block runs against the
test fixtures on each build. The balance recipe in `docs/reference.md`, which
failed on any glob, is replaced.

## 1.8.0 (2026-10-27)

**DuckDB versions.** This release loads into DuckDB 2.0.0 and into DuckDB 1.5.6
and later 1.5 releases, from one binary. It is built against the stable C API
at minimum version v1.5.6 instead of for one exact DuckDB, because DuckDB 2.0
refuses the exact-version binaries 1.7.0 and earlier were. `INSTALL quackiso FROM
community` failed on 2.0.0 for that reason; it works again on 2.0.0 and on
1.5.6. It does **not** load into DuckDB 1.5.5 or earlier: on 1.5.5, stay on
1.7.0, which is the last build for it.

**CBPR+ address deadline.** CBPR+ was to stop accepting a fully unstructured
postal address on 14 November 2026. On 27 August 2026 Swift delayed the whole
November 2026 payments standards release, the address removal with it, and
promised a revised schedule by December 2026. The README, the registry
descriptor and `audit_addresses` now say so: a `finding` names what the rule
will refuse once it lands, not what is refused today. Nothing in the audit's
classification changed.

**Statement detail readers.** `read_iso20022` returned one row per booked entry
carrying the first transaction's details, so an entry holding three `<NtryDtls>`
reported one of the three payments. Four functions carry the grains it could not:
`read_camt_transactions`, `read_camt_balances`, `read_camt_amount_details` and
`read_camt_remittance`. They join to `read_iso20022` on `(source_file,
statement_index, entry_index)`. `read_iso20022` gained eleven columns for it:
`statement_kind`, `statement_index`, `entry_index`, `transaction_count`,
`remittance_count`, `reversal_indicator` and five bank-transaction-code columns.
One observable change: `remittance_info` is now populated only when a
transaction states exactly one remittance leaf, so a sole transaction with both
free text and a structured reference returns NULL there rather than the free
text alone. `read_camt_remittance` is the lossless replacement.

**Archives and envelopes are refused by name.** A ZIP, TAR, gzip-TAR,
PKCS#7, OpenPGP or EBICS file is rejected with what to do about it. A TAR of two
statements used to parse as one document and return the entries of both under
one `source_file`.

**Build.** `extension-ci-tools` moved to its `main` branch, the test DuckDB is
pinned to 2.0.0 in the `Makefile`, and `pytest.ini` keeps pytest out of the
submodule's helper scripts.

## 1.7.0

Added `audit_addresses`, which grades the postal address of every party over
SWIFT MT and ISO 20022 through one classifier, and the `read_mt101` and
`read_mt104` readers. Built for DuckDB 1.5.5.
