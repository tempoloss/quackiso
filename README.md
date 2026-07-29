# quackiso

Query [ISO 20022](https://www.iso20022.org/) financial messages as SQL in
DuckDB — no Python preprocessing, no per-schema glue.

```sql
INSTALL quackiso FROM community;
LOAD quackiso;

-- bank statements: one row per booked entry
SELECT booking_date, amount, currency, credit_debit, counterparty_name, remittance_info
FROM read_iso20022('statements/*.xml')
WHERE credit_debit = 'DBIT' AND amount > 100000;

-- interbank credit transfers: one row per transaction
SELECT uetr, amount, currency, debtor_name, creditor_name, creditor_agent_bic
FROM read_pacs008('payments/*.xml');
```

ISO 20022 is the XML standard banks and payment systems are migrating to,
replacing legacy SWIFT MT. The messages are strict but deeply nested; querying
them normally means parsing to Parquet with a Python script first. quackiso
reads them directly, streaming one entry at a time so a multi-GB file costs the
same memory as a small one.

## Functions

### `read_iso20022(path)` — cash management

Handles **camt.053** (statement), **camt.054** (debit/credit notification) and
**camt.052** (account report). All three wrap the same `<Ntry>` children; only
the container differs (`Stmt` / `Ntfctn` / `Rpt`), so one reader serves the
family. Grain: one row per booked entry.

| column | source |
|---|---|
| `msg_id` | `GrpHdr/MsgId` |
| `account_iban` | account IBAN, or `Othr/Id` for non-IBAN accounts |
| `statement_id` | `Stmt`/`Ntfctn`/`Rpt` → `Id` |
| `entry_ref` | `Ntry/NtryRef` |
| `amount` | `Ntry/Amt` (DOUBLE) |
| `currency` | `Ntry/Amt/@Ccy` |
| `credit_debit` | `Ntry/CdtDbtInd` (`CRDT`/`DBIT`) |
| `status` | `Ntry/Sts`, whether plain text or `<Cd>` |
| `booking_date`, `value_date` | `BookgDt`, `ValDt` (`Dt` or `DtTm`) |
| `bank_ref` | `Ntry/AcctSvcrRef` |
| `end_to_end_id` | first `TxDtls/Refs/EndToEndId` |
| `counterparty_name` | other side of the flow — see below |
| `counterparty_iban` | matching account, IBAN or `Othr/Id` |
| `remittance_info` | `RmtInf/Ustrd`, else structured `Strd` reference |
| `source_file` | file the row came from |

**Counterparty** is the party on the *other* side of the flow: a debit shows who
you paid, a credit shows who paid you. Statements routinely populate only one
side, so the reader falls back to whichever party is present, and then to the
`UltmtDbtr`/`UltmtCdtr` pair.

### `read_pacs008(path)` — credit transfers

**pacs.008** is the FI-to-FI customer credit transfer, the ISO 20022 replacement
for SWIFT MT103. Grain: one row per `CdtTrfTxInf`.

| column | source |
|---|---|
| `msg_id` | `GrpHdr/MsgId` |
| `instr_id`, `end_to_end_id`, `tx_id`, `uetr` | `PmtId/*` |
| `amount`, `currency` | `IntrBkSttlmAmt`, falling back to `InstdAmt` |
| `settlement_date` | `IntrBkSttlmDt` |
| `charge_bearer` | `ChrgBr` (`DEBT`/`CRED`/`SHAR`) |
| `debtor_name`, `creditor_name` | `Dbtr`/`Cdtr`, incl. nested `Pty/Nm` |
| `debtor_account`, `creditor_account` | IBAN or `Othr/Id` |
| `debtor_agent_bic`, `creditor_agent_bic` | `BICFI`, `BIC`, else `ClrSysMmbId/MmbId`, else name |
| `remittance_info` | `RmtInf/Ustrd` |
| `source_file` | file the row came from |

`path` is a single file or a glob for both functions.

## Validated against

Not just hand-written samples — the readers are checked against real messages
from public corpora, which is where the interesting bugs came from:

- **camt.053** — Goldman Sachs US / UK / EU / US-wire, `actualbudget`, and the
  `genkgo/camt` suite: versions **.02 / .03 / .04 / .08**, plus multi-statement,
  five-decimal amounts, ultimate-parties-only, and balance-only files.
- **camt.054** — `genkgo/camt` v2 / v4 / v8 variants.
- **pacs.008** — versions **.01 / .02 / .07 / .08 / .09** from Nivaes, Prowide,
  OpenBankProject, AWS samples, Mbanq and centiglobe.

What that shook out, now regression-tested:

- v8 nests party names one level deeper (`Dbtr/Pty/Nm`) than v2.
- US accounts carry no IBAN — the number lives under `Id/Othr/Id`.
- Entries often name only one side of the flow.
- Some statements name only the *ultimate* parties.
- Corporate messages may carry structured (`Strd`) remittance and no free text.
- Prefixed namespaces (`<Doc:...>`, `<urn2:...>`) are common in CBPR+ and vendor
  messages; tag names are normalised while copying a subtree.

Deliberately out of scope for now: XSD validation, native DATE/DECIMAL typing,
and reads over DuckDB's own filesystems (http/s3).

Two design calls worth knowing: `amount` is `DOUBLE` (exact below 2^53; a
DECIMAL mode is planned) and dates are `VARCHAR` ISO strings — real files mix
`2019-01-23` with `2023-10-01T13:37:14.000Z`, so `CAST` them yourself if you
want a real `DATE`.

## Build

Needs the Rust toolchain and the DuckDB C-API build tooling (a submodule). The
extension pins one DuckDB version — see `TARGET_DUCKDB_VERSION` in the
`Makefile` — because duckdb-rs uses the unstable C API.

```sh
git submodule update --init --recursive
make configure
make debug        # or: make release
make test
```

```sh
duckdb -unsigned
```
```sql
LOAD './build/debug/quackiso.duckdb_extension';
SELECT * FROM read_iso20022('testdata/camt053_sample.xml');
SELECT * FROM read_pacs008('testdata/pacs008_prefixed_sample.xml');
```

## Publish

Submit `community-extension/description.yml` to
[`duckdb/community-extensions`](https://github.com/duckdb/community-extensions)
at `extensions/quackiso/description.yml`, with `ref` set to a tagged commit.

## Roadmap

- `pain.001` customer credit transfer initiation
- native `DATE` and exact `DECIMAL` typing
- reads over DuckDB filesystems (http/s3)
- balance table function for camt.053 opening/closing balances

## License

MIT
