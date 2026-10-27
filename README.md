# quackiso

[![Tests](https://github.com/tempoloss/quackiso/actions/workflows/test.yml/badge.svg?branch=main)](https://github.com/tempoloss/quackiso/actions/workflows/test.yml)
[![Memory](https://github.com/tempoloss/quackiso/actions/workflows/memory.yml/badge.svg?branch=main)](https://github.com/tempoloss/quackiso/actions/workflows/memory.yml)

Query [ISO 20022](https://www.iso20022.org/) and SWIFT MT financial messages as
SQL in DuckDB - no Python preprocessing, no per-schema glue.

## Install

From the DuckDB community registry:

```sql
INSTALL quackiso FROM community;
LOAD quackiso;
```

## Examples

Point it at a folder of bank XML or SWIFT MT text, get transactions as rows:

```sql
-- bank statements: one row per booked entry
SELECT booking_date, amount, currency, credit_debit, counterparty_name
FROM read_iso20022('statements/*.xml')
ORDER BY booking_date;
```

CBPR+ was to stop accepting a fully unstructured postal address on 14 November
2026; Swift deferred that release on 27 August 2026 and promised a revised
schedule by December 2026. `audit_addresses` finds the parties in traffic you
already have that the rule will refuse once it lands, over ISO 20022 and SWIFT MT
alike:

```sql
SELECT family, role, address_format, address_text, finding
FROM audit_addresses('inbox/**/*')
WHERE finding IS NOT NULL
ORDER BY family, role;
```

`path` is a file, a glob, or a list of them (`['a.xml', 'inbox/*.xml']`), gzipped
or not; a file named twice is read once. Every row carries `source_file`.

## Design decisions

- **Amounts are `DECIMAL(38,5)`, never `DOUBLE`.** Wire text goes straight to a
  scaled integer, so totals are exact. An amount that cannot be represented is
  an error and not a NULL, because a NULL silently drops out of a `SUM`.
  ([types](docs/types.md), [ADR 0001](docs/adr/0001-exact-decimal-amounts.md))
- **Files are parsed as an event stream.** Memory follows one output batch plus
  the largest single subtree, not the file size. The claim is measured in CI,
  not remembered. ([streaming](docs/streaming.md))
- **One function per message family, and separate grains for details.** A
  batched entry has three end-to-end ids and one column to put them in, so
  `read_camt_transactions` and friends are their own functions instead of wider
  rows. ([ADR 0009](docs/adr/0009-camt-details-have-separate-grains.md))
- **Readers fail loudly.** A file of the wrong message type, a half-written
  gzip append, or a bad amount fails the query instead of returning an empty or
  quietly short table. ([functions](docs/functions.md))
- **Archives and envelopes are refused by name.** ZIP, TAR, PKCS#7, PGP and
  EBICS files are not read as if they were messages. ([non-features](docs/non-features.md))
- **A declared encoding is honoured, an undeclared one is not guessed.** A
  Latin-1 or UTF-16 statement reads as the text it declares; XML with no
  declaration is UTF-8 and MT is never transcoded.
  ([ADR 0010](docs/adr/0010-a-declared-encoding-is-honoured.md))
- **No XSD validation.** Every defect the real corpus exposed was the reader
  being too strict, not the file being invalid.
  ([ADR 0003](docs/adr/0003-no-xsd-validation.md))
- **No remote paths.** Removed because a safe table function cannot reach the
  query's client context. ([ADR 0002](docs/adr/0002-no-remote-paths.md))
- **`threads := n` is capped.** A hundred thousand threads fails by resource
  exhaustion mid-scan, so the pool is capped at the file count and at four times
  the machine's parallelism. ([ADR 0005](docs/adr/0005-explicit-thread-count-is-capped.md))

## Docs

- [Functions](docs/functions.md) - every reader, its message and its grain
- [Column reference](docs/reference.md) - columns and notes per reader
- [Recipes](docs/recipes.md) - inbox, reconciliation, continuity, rejects and invoice numbers as SQL, run on every build
- [audit_addresses](docs/address-audit.md)
- [Types](docs/types.md), [Streaming](docs/streaming.md)
- [Tested against real messages](docs/real-messages.md)
- [Deliberate non-features](docs/non-features.md), [Roadmap](docs/roadmap.md)
- [Primitives](docs/primitives.md), [ADRs](docs/adr/)

## DuckDB versions

Release 1.8.0 loads into DuckDB 2.0.0 and into DuckDB 1.5.6 and later, from one
binary. It is built against the stable C API at minimum version v1.5.6 rather
than for one exact DuckDB, because DuckDB 2.0 refuses the exact-version binaries
earlier releases were. It does not load into DuckDB 1.5.5 or earlier: stay on
1.7.0 there, the last build for it. The history is in
[`CHANGELOG.md`](CHANGELOG.md).

## Building

```sh
git submodule update --init --recursive
make configure && make debug && make test
```

## License

MIT
