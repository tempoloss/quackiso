# quackiso

Query [ISO 20022](https://www.iso20022.org/) financial messages as SQL in
DuckDB — no Python preprocessing, no per-schema glue.

```sql
INSTALL quackiso FROM community;
LOAD quackiso;

-- point at a folder of bank statements, get one row per booked entry
SELECT booking_date, amount, currency, credit_debit, counterparty_name, remittance_info
FROM read_iso20022('statements/*.xml')
WHERE credit_debit = 'DBIT' AND amount > 100000;
```

ISO 20022 is the XML standard banks and payment systems are migrating to,
replacing legacy SWIFT MT. The messages are strict but deeply nested; querying
them normally means parsing to Parquet with a Python script first. quackiso
reads them directly.

## v1 scope

One table function, `read_iso20022(path)`, for **camt.053** bank statements.
`path` is a single file or a glob. Grain: one row per booked entry (`Ntry`).

| column | source |
|---|---|
| `msg_id` | `GrpHdr/MsgId` |
| `account_iban` | `Stmt/Acct/Id/IBAN` |
| `statement_id` | `Stmt/Id` |
| `entry_ref` | `Ntry/NtryRef` |
| `amount` | `Ntry/Amt` (DOUBLE) |
| `currency` | `Ntry/Amt/@Ccy` |
| `credit_debit` | `Ntry/CdtDbtInd` (`CRDT`/`DBIT`) |
| `status` | `Ntry/Sts` (text or `<Cd>`) |
| `booking_date`, `value_date` | `Ntry/BookgDt`, `Ntry/ValDt` |
| `bank_ref` | `Ntry/AcctSvcrRef` |
| `end_to_end_id` | first `TxDtls/Refs/EndToEndId` |
| `counterparty_name` | other side of the flow (`Cdtr` on debit, `Dbtr` on credit) |
| `counterparty_iban` | matching account IBAN |
| `remittance_info` | `TxDtls/RmtInf/Ustrd` joined |
| `source_file` | file the row came from |

Deliberately **out of v1** (keeping the first release shippable): pacs.008,
camt.054, XSD validation, native DATE/DECIMAL typing, and reading over DuckDB's
own filesystems (http/s3). See the roadmap below.

Two design calls worth knowing: `amount` is `DOUBLE` (exact below 2^53; a
DECIMAL mode is planned) and dates are `VARCHAR` ISO strings (`CAST` them if you
want a real `DATE`) so a `<Dt>` and a `<DtTm>` both land without a fragile parse.

## Build

Needs the Rust toolchain and the DuckDB C-API build tooling (vendored as a
submodule). The extension pins one DuckDB version — see `TARGET_DUCKDB_VERSION`
in the `Makefile` — because duckdb-rs uses the unstable C API.

```sh
git submodule add https://github.com/duckdb/extension-ci-tools
git submodule update --init --recursive

make configure
make debug        # or: make release
make test         # runs test/sql/quackiso.test
```

Load the built extension in DuckDB:

```sh
duckdb -unsigned
```
```sql
LOAD './build/debug/quackiso.duckdb_extension';
SELECT * FROM read_iso20022('testdata/camt053_sample.xml');
```

## Publish

Once a release is tagged, submit `community-extension/description.yml` to
[`duckdb/community-extensions`](https://github.com/duckdb/community-extensions)
at `extensions/quackiso/description.yml` with `ref` set to the tagged commit.
After it merges, anyone can `INSTALL quackiso FROM community`.

## Roadmap

- `pacs.008` credit transfers (one row per `CdtTrfTxInf`)
- `camt.054` debit/credit notifications
- native `DATE` and exact `DECIMAL` typing
- read over DuckDB filesystems (http/s3), matching the glob behaviour of `read_csv`
- optional balance table function for `camt.053` opening/closing balances

## License

MIT
