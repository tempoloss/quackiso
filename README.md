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
| `read_pacs004(path)` | pacs.004 payment return (settled money coming back) | one row per `TxInf` |
| `read_pain001(path)` | pain.001 credit transfer initiation | one row per transaction |
| `read_pain002(path)` | pain.002 payment status report | one row per status statement |

`path` is a file or a glob. Every row carries `source_file`, so a glob over a
year of statements stays attributable.

### read_iso20022

`msg_id`, `account_iban`, `statement_id`, `entry_ref`, `amount`, `currency`,
`credit_debit`, `status`, `booking_date`, `value_date`, `bank_ref`,
`end_to_end_id`, `counterparty_name`, `counterparty_iban`, `remittance_info`,
`source_file`

### read_pacs004

`msg_id`, `return_id`, `original_msg_id`, `original_msg_name_id`,
`original_instr_id`, `original_end_to_end_id`, `original_tx_id`, `original_uetr`,
`amount`, `currency`, `original_amount`, `original_currency`, `settlement_date`,
`original_settlement_date`, `charge_bearer`, `return_reason_code`,
`return_reason_info`, `return_originator`, `original_debtor_name`,
`original_debtor_account`, `original_debtor_agent_bic`, `original_creditor_name`,
`original_creditor_account`, `original_creditor_agent_bic`, `remittance_info`,
`source_file`

A return is not a payment. `amount` is what came back; `original_amount` is what
the payment had settled for, so a return with charges deducted is
`amount < original_amount`. The `original_*` party columns are the sides of the
original transfer even when the message only states them in `<RtrChain>`, whose
debtor is the party giving the money back — the original creditor.

### read_pacs008

`msg_id`, `instr_id`, `end_to_end_id`, `tx_id`, `uetr`, `amount`, `currency`,
`settlement_date`, `charge_bearer`, `debtor_name`, `debtor_account`,
`debtor_agent_bic`, `creditor_name`, `creditor_account`, `creditor_agent_bic`,
`remittance_info`, `source_file`

### read_pain001

`msg_id`, `initiating_party`, `payment_info_id`, `payment_method`,
`requested_execution_date`, `debtor_name`, `debtor_account`, `debtor_agent_bic`,
`instr_id`, `end_to_end_id`, `uetr`, `amount`, `currency`, `charge_bearer`,
`creditor_name`, `creditor_account`, `creditor_agent_bic`, `remittance_info`,
`source_file`

In pain.001 the payer sits on the `<PmtInf>` group rather than the transaction,
so `debtor_*`, `payment_method` and `requested_execution_date` are carried down to
every transaction in the group.

### read_pain002

`msg_id`, `initiating_party`, `original_msg_id`, `original_msg_name_id`,
`status_level`, `original_payment_info_id`, `status_id`, `status`, `reason_code`,
`reason_info`, `reason_originator`, `original_number_of_txs`,
`original_control_sum`, `original_instr_id`, `original_end_to_end_id`,
`original_uetr`, `amount`, `currency`, `requested_execution_date`, `debtor_name`,
`debtor_account`, `creditor_name`, `creditor_account`, `remittance_info`,
`acceptance_date_time`, `source_file`

A status report states its status at three levels: the whole batch
(`OrgnlGrpInfAndSts`), one payment group (`OrgnlPmtInfAndSts`), and one
transaction (`TxInfAndSts`). Only the group level is mandatory, so a bank that
rejects a file outright details no transactions at all. The grain is therefore
one row per status statement, and `status_level` is `GROUP`, `PAYMENT_INFO` or
`TRANSACTION`. Only transaction rows carry an `amount`, so `SUM(amount)` is
unaffected by the coarser rows; filter with `WHERE status_level = 'TRANSACTION'`
for the transaction grain. pain.002.001.01 predates this structure and is
rejected by name.

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

Around 140 real messages from a dozen-plus sources — Goldman Sachs (US, UK, EU,
wire), actualbudget, genkgo, Nivaes, Prowide, OpenBankProject, Mbanq, SIX
interbank, CBPR+, prog-nov, salesking, Dolibarr, Handelsbanken, issettled and
others — across camt.053 `.02/.03/.04/.08/.09/.11`, camt.052/054, pacs.008
`.01/.02/.07/.08/.09`, pacs.004 `.01/.02/.03/.09/.10/.11`, pain.001
`.03/.09/.11` and pain.002 `.02/.03/.04/.05/.09/.10/.11/.12/.13/.14/.15` plus
SEPA variants.

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
- **structured remittance** — `Strd/CdtrRefInf/Ref` when there is no `Ustrd`;
- **the return chain is not the payment chain** — pacs.004 states the parties in
  `<RtrChain>`, whose debtor is the party giving the money back, so the original
  sides are read crossed; the SIX interbank sample pair proves the direction;
- **renamed reason blocks** — pacs.004's `RtrRsn`/`AddtlRtrRsnInf`/`RtrOrgtr` and
  pain.002's `StsRsn`/`StsOrgtr` are the older spellings of the same elements;
- **status without a transaction** — a pain.002 can accept or reject a whole
  batch at group level and detail nothing, so the grain is the status statement.

Some apparent bugs turned out to be correct behaviour and were left alone: a
camt statement with only balances yields zero rows because it has no `<Ntry>`,
while a file of the wrong message type is now a loud error rather than an empty
table — a template with `{placeholder}` amounts or a pacs.002 pointed at
`read_pacs004` fails instead of silently returning nothing.

## Deliberate non-features

- **No `s3://` or `https://` paths.** Attempted and removed: opening a remote file
  needs the executing query's client context, which `duckdb-rs` does not expose
  from a safe table function. See
  [`docs/adr/0002-no-remote-paths.md`](docs/adr/0002-no-remote-paths.md).
- **No XSD validation.** Every defect the real corpus exposed was the reader being
  too strict, not the file being invalid. See
  [`docs/adr/0003-no-xsd-validation.md`](docs/adr/0003-no-xsd-validation.md).

## Roadmap

- `pacs.002` FI-to-FI payment status reports and `camt.056` cancellation
  requests, each of which needs its own grain rather than a column on an
  existing reader.
- Remote paths, once the blocker in ADR 0002 is resolved.

## Building

```sh
git submodule update --init --recursive
make configure && make debug && make test
```

## License

MIT
