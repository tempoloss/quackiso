# Changelog

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
