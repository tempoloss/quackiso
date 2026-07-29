# quackiso

Query [ISO 20022](https://www.iso20022.org/) financial messages as SQL in DuckDB —
no Python preprocessing, no per-schema glue.

```sql
INSTALL quackiso FROM community;
LOAD quackiso;

-- bank statements: one row per booked entry
SELECT booking_date, amount, currency, credit_debit, counterparty_name
FROM read_iso20022('statements/*.xml')
ORDER BY booking_date;
```

Point it at a folder of bank XML, get transactions as rows.

## Functions

| Function | Messages | Grain |
| --- | --- | --- |
| `read_iso20022(path)` | camt.053 statements, camt.054 notifications, camt.052 reports | one row per booked entry |
| `read_pacs008(path)` | pacs.008 FI-to-FI credit transfer (the ISO 20022 MT103) | one row per `CdtTrfTxInf` |
| `read_pain001(path)` | pain.001 credit transfer initiation | one row per transaction |

`path` is a file or a glob. Every row carries `source_file`, so a glob over a
year of statements stays attributable.

### read_iso20022

`msg_id`, `account_iban`, `statement_id`, `entry_ref`, `amount`, `currency`,
`credit_debit`, `status`, `booking_date`, `value_date`, `bank_ref`,
`end_to_end_id`, `counterparty_name`, `counterparty_iban`, `remittance_info`,
`source_file`

### read_pacs008

`msg_id`, `instr_id`, `end_to_end_id`, `tx_id`, `uetr`, `amount`, `currency`,
`settlement_date`, `charge_bearer`, `debtor_name`, `debtor_account`,
`debtor_agent_bic`, `creditor_name`, `creditor_account`, `creditor_agent_bic`,
`remittance_info`, `source_file`

### read_pain001

`msg_id`, `initiating_party`, `payment_info_id`, `payment_method`,
`requested_execution_date`, `debtor_name`, `debtor_account`, `debtor_agent_bic`,
`instr_id`, `end_to_end_id`, `amount`, `currency`, `charge_bearer`,
`creditor_name`, `creditor_account`, `creditor_agent_bic`, `remittance_info`,
`source_file`

In pain.001 the payer sits on the `<PmtInf>` group rather than the transaction,
so `debtor_*`, `payment_method` and `requested_execution_date` are carried down to
every transaction in the group.

## Types

**Amounts are `DECIMAL(38,5)`, never `DOUBLE`.** Values go from the wire string
straight to a scaled integer and never touch a float, so totals are exact:

```sql
-- 0.10 + 0.20 + 0.30 + 1500.10
-- as DOUBLE: 1500.7000000000003
SELECT SUM(amount) = 1500.70 FROM read_iso20022('testdata/camt053_decimal_sample.xml')
WHERE credit_debit = 'DBIT';
-- true
```

The width is not arbitrary. ISO 20022 allows 18 significant digits with up to 5
fraction digits: `DECIMAL(18,5)` is only 64 bits and overflows on a legal
18-integer-digit amount, and scale 2 would reject real files — prog-nov's pacs.008
carries `5013090.23491`.

An amount that cannot be represented exactly is an **error, not a NULL**. A NULL
amount disappears from a `SUM` and returns a total that looks plausible and is
wrong.

**Dates are real dates.** `booking_date` and `value_date` are `TIMESTAMP` because
the corpus mixes `2019-01-23` with `2023-10-01T13:37:14.000Z`; offsets are
normalised to UTC. `settlement_date` and `requested_execution_date` are `DATE`.
Both `<Dt>` and `<DtTm>` wrappings are read.

## Streaming

Files are parsed as an event stream, one entry at a time. A 1.7 GB statement is
read in about 2 MB of resident memory; peak does not follow file size.

## Tested against real messages

45 real messages from nine sources — Goldman Sachs (US, UK, EU, wire),
actualbudget, genkgo, Nivaes, Prowide, OpenBankProject, AWS, Mbanq, centiglobe,
prog-nov, salesking, Dolibarr — across camt.053 `.02/.03/.04/.08`, camt.054
`.02/.04/.08`, pacs.008 `.01/.02/.07/.08/.09` and pain.001
`.03/.09/.11/.13` plus SEPA `pain.001.002.03` / `.003.03`.

Every fix in this reader came from one of those files:

- **namespace prefixes** — `<Doc:CdtTrfTxInf>`, `<urn2:...>`: tag names are
  normalised while a subtree is copied, which previously produced an ill-formed
  document;
- **one-sided entries** — a `CRDT` entry often carries only `<Cdtr>`; the
  counterparty falls back to the other side, then to the ultimate parties;
- **`.08` nesting** — party names under `Pty/Nm`, accounts under `Othr/Id`;
- **group-level fields** — SEPA puts `IntrBkSttlmDt` on the group header, and
  pain.001 puts the debtor and `ChrgBr` on `<PmtInf>`;
- **`<ReqdExctnDt><DtTm>`** — later pain.001 versions wrap the date differently;
- **structured remittance** — `Strd/CdtrRefInf/Ref` when there is no `Ustrd`.

Two apparent bugs turned out to be correct behaviour and were left alone: files
containing only balances yield zero rows, because they contain no `<Ntry>`.

## Deliberate non-features

- **No `s3://` or `https://` paths.** Attempted and removed: opening a remote file
  needs the executing query's client context, which `duckdb-rs` does not expose
  from a safe table function. See
  [`docs/adr/0002-no-remote-paths.md`](docs/adr/0002-no-remote-paths.md).
- **No XSD validation.** Every defect the real corpus exposed was the reader being
  too strict, not the file being invalid. See
  [`docs/adr/0003-no-xsd-validation.md`](docs/adr/0003-no-xsd-validation.md).

## Roadmap

- `pacs.004` payment returns and `pain.002` payment-status reports, which need
  their own grain rather than a column on an existing one.
- Remote paths, once the blocker in ADR 0002 is resolved.

## Building

```sh
git submodule update --init --recursive
make configure && make debug && make test
```

## License

MIT
