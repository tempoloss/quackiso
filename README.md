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
| `read_pacs009(path)` | pacs.009 financial institution transfer (MT202 / MT202COV) | one row per `CdtTrfTxInf` |
| `read_pacs003(path)` | pacs.003 FI-to-FI direct debit (the interbank leg of pain.008) | one row per `DrctDbtTxInf` |
| `read_pacs004(path)` | pacs.004 payment return (settled money coming back) | one row per `TxInf` |
| `read_pacs007(path)` | pacs.007 payment reversal (the sender takes it back) | one row per `TxInf` |
| `read_pacs002(path)` | pacs.002 FI-to-FI payment status report | one row per status statement |
| `read_pain001(path)` | pain.001 credit transfer initiation | one row per transaction |
| `read_pain002(path)` | pain.002 customer payment status report | one row per status statement |
| `read_pain008(path)` | pain.008 direct debit initiation (the creditor pulls) | one row per collection |
| `read_camt056(path)` | camt.056 payment cancellation request | one row per cancellation statement |
| `read_camt055(path)` | camt.055 customer payment cancellation request | one row per cancellation statement |
| `read_camt029(path)` | camt.029 resolution of investigation (the answer to a camt.056) | one row per statement |
| `sniff_iso20022(path)` | any of the above, or anything claiming to be ISO 20022 | one row per **file** |

`path` is a file or a glob. Every row carries `source_file`, so a glob over a
year of statements stays attributable. Every function also takes
`threads := n`; see Streaming.

### read_iso20022

`msg_id`, `account_iban`, `statement_id`, `entry_ref`, `amount`, `currency`,
`credit_debit`, `status`, `booking_date`, `value_date`, `bank_ref`,
`end_to_end_id`, `counterparty_name`, `counterparty_iban`, `remittance_info`,
`source_file`

### sniff_iso20022

The inventory function: point it at a directory before choosing a reader.

```sql
SELECT family, reader, count(*), sum(records)
FROM sniff_iso20022('inbox/**/*.xml')
GROUP BY family, reader;
```

`message_type` (`pacs.008.001.08`), `family` (`pacs.008`), `namespace`,
`msg_id`, `created`, `records`, `reader`, `error`, `source_file`

One row per file, whatever the file turns out to be. `reader` names the
function that covers the family; `records` counts the transaction-level
elements on the wire (status and cancellation readers emit group-level rows
on top of that). A truncated download, a stray XSD, a non-ISO payload get a
row whose `error` says why — nothing a file *contains* aborts an inventory
scan. Identity comes from the `Document` namespace, the era-spelled container
names the readers accept, or the envelope's binding (BizMsgEnvlp, SWIFTNet
DataPDU, Fedwire, issettled/montran RTGS traffic with no `<Document>` at
all); `head.001` — the AppHdr beside the message — is never mistaken for the
message itself. The sniffer routes, the readers judge: a file the sniffer
attributes to `read_pacs008` can still fail loudly there, and that division
is the point.

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

### read_pacs009

`msg_id`, `instr_id`, `end_to_end_id`, `tx_id`, `uetr`, `amount`, `currency`,
`settlement_date`, `debtor_fi`, `debtor_account`, `debtor_agent_bic`,
`creditor_fi`, `creditor_account`, `creditor_agent_bic`,
`underlying_debtor_name`, `underlying_debtor_account`,
`underlying_creditor_name`, `underlying_creditor_account`,
`underlying_remittance_info`, `source_file`

Banks moving money between themselves; the parties are financial institutions,
hence `debtor_fi`/`creditor_fi`. In the COV form the transfer settles a
customer payment that travelled separately as a pacs.008, and the
`underlying_*` columns carry that customer debtor and creditor — MT202COV
exists because hiding them made cover payments a money-laundering corridor, so
dropping the block would reproduce exactly the opacity the format was created
to remove.

### read_pacs007

`msg_id`, `reversal_id`, `original_msg_id`, `original_msg_name_id`,
`original_instr_id`, `original_end_to_end_id`, `original_tx_id`,
`original_uetr`, `amount`, `currency`, `original_amount`, `original_currency`,
`settlement_date`, `charge_bearer`, `reversal_reason_code`,
`reversal_reason_info`, `reversal_originator`, `original_debtor_name`,
`original_debtor_account`, `original_debtor_agent_bic`,
`original_creditor_name`, `original_creditor_account`,
`original_creditor_agent_bic`, `remittance_info`, `source_file`

pacs.004's twin with the direction flipped at the source: a return is the
receiver sending money back, a reversal is the **sender** taking a settled
payment back — typically a direct debit collected in error, undone by the bank
that collected it. As in pacs.004, `amount < original_amount` is a reversal
with charges kept. There is no `RtrChain` equivalent: the parties appear only
in the carried copy of the original, whose sides are the original sides.

### read_pacs003

`msg_id`, `instr_id`, `end_to_end_id`, `tx_id`, `uetr`, `amount`, `currency`,
`settlement_date`, `requested_collection_date`, `sequence_type`,
`charge_bearer`, `mandate_id`, `mandate_signed_on`, `creditor_name`,
`creditor_account`, `creditor_agent_bic`, `debtor_name`, `debtor_account`,
`debtor_agent_bic`, `remittance_info`, `source_file`

The interbank leg of a direct debit: what the creditor's bank sends the
debtor's bank to collect what a pain.008 asked for. The mandate travels with
the collection — the debtor's bank is entitled to check it before letting
money leave the account — and the settlement date and sequence type sit once
on the group header in real files and are carried down.

### read_pacs002

`msg_id`, `instructing_agent_bic`, `instructed_agent_bic`, `status_level`,
`status_id`, `status`, `reason_code`, `reason_info`, `reason_originator`,
`original_msg_id`, `original_msg_name_id`, `original_instr_id`,
`original_end_to_end_id`, `original_tx_id`, `original_uetr`,
`acceptance_date_time`, `original_amount`, `original_currency`,
`original_settlement_date`, `original_debtor_name`, `original_creditor_name`,
`source_file`

The interbank sibling of pain.002, minus the payment-info level: `status_level`
is `GROUP` or `TRANSACTION`. Unlike pain.002, the group block is optional —
CBPR+-era messages reference the original inside each transaction instead — and
one `Document` may hold several complete reports, each with its own header; all
carried context resets at each one.

### read_camt056

`assignment_id`, `assignment_created`, `assigner`, `assignee`, `scope`,
`cancellation_id`, `case_id`, `group_cancellation`, `original_number_of_txs`,
`original_msg_id`, `original_msg_name_id`, `original_instr_id`,
`original_end_to_end_id`, `original_tx_id`, `original_uetr`, `original_amount`,
`original_currency`, `original_settlement_date`, `cancellation_reason_code`,
`cancellation_reason_info`, `cancellation_originator`, `original_debtor_name`,
`original_debtor_account`, `original_creditor_name`,
`original_creditor_account`, `remittance_info`, `source_file`

A cancellation request moves no money, so there is no `amount` column at all:
every monetary column is `original_*`, describing the payment it asks to undo.
`scope` is `GROUP` or `TRANSACTION`, because a batch-wide cancellation
(`GrpCxl` true) may list no transactions and must still be a row — a reader
whose grain is the transaction parses "cancel the entire batch" to zero rows.

### read_camt055

`assignment_id`, `assignment_created`, `assigner`, `assignee`, `scope`,
`cancellation_id`, `group_cancellation`, `original_number_of_txs`,
`original_msg_id`, `original_msg_name_id`, `original_payment_info_id`,
`original_instr_id`, `original_end_to_end_id`, `original_uetr`,
`original_amount`, `original_currency`, `original_execution_date`,
`cancellation_reason_code`, `cancellation_reason_info`,
`cancellation_originator`, `original_debtor_name`, `original_creditor_name`,
`original_creditor_account`, `remittance_info`, `source_file`

The customer-side camt.056: the initiating party asking its own bank to cancel
payments it initiated with a pain.001 or pain.008, so the assigner is usually a
customer party, not a bank. Being pain-side it has the payment-info level
camt.056 lacks — `scope` is `GROUP`, `PAYMENT_INFO` or `TRANSACTION` — and
`original_execution_date` is the execution date on the pain.001 side and the
collection date on the pain.008 side.

### read_pain008

`msg_id`, `initiating_party`, `payment_info_id`, `payment_method`,
`sequence_type`, `requested_collection_date`, `creditor_name`,
`creditor_account`, `creditor_agent_bic`, `creditor_scheme_id`, `instr_id`,
`end_to_end_id`, `uetr`, `amount`, `currency`, `charge_bearer`, `mandate_id`,
`mandate_signed_on`, `debtor_name`, `debtor_account`, `debtor_agent_bic`,
`remittance_info`, `source_file`

pain.001 mirrored: a direct debit is the CREDITOR pulling, so the collector —
its account, agent, scheme id and the collection date — lives on the `<PmtInf>`
group and is carried down, while every transaction names a debtor to charge.
The mandate (`mandate_id`, `mandate_signed_on`) is the debtor's signed
authorisation, and `sequence_type` (FRST/RCUR/OOFF/FNAL) says where in the
mandate's life this collection sits; a transaction may restate it.

### read_camt029

`assignment_id`, `assignment_created`, `assigner`, `assignee`, `scope`,
`resolution_status`, `case_id`, `cancellation_status_id`,
`cancellation_status`, `reason_code`, `reason_info`, `reason_originator`,
`original_msg_id`, `original_msg_name_id`, `original_instr_id`,
`original_end_to_end_id`, `original_tx_id`, `original_uetr`,
`original_amount`, `original_currency`, `original_settlement_date`,
`original_debtor_name`, `original_creditor_name`, `source_file`

The answer to a camt.056. Most real camt.029 files answer at **message level
only** — an assignment, a resolved case and one confirmation code, no
transaction detail — so `scope` is `RESOLUTION`, `GROUP` or `TRANSACTION`, and
the message-level answer is a row of its own. `CNCL` means the cancellation was
carried out; `RJCR` means it was refused, and the transaction rows carry the
refusal reason.

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

**A glob is parsed in parallel, one worker per file.** The unit is the whole
file because XML has no safe split points — there is no way to start parsing a
statement in the middle, unlike a block-structured format such as OSM's PBF —
so a single document is always one sequential pass. Workers claim files from a
shared counter and hand vector-sized batches over a bounded channel, so memory
stays O(threads × batch) regardless of how many files the glob matched. Rows of
one file stay in order; files interleave, which is what `source_file` is for. A
malformed amount in any file still fails the whole query.

The default is one worker per file, capped at the machine's parallelism;
`threads := 1` forces the sequential scan, `threads := n` pins the pool:

```sql
SELECT count(*), SUM(amount)
FROM read_iso20022('statements/*.xml', threads := 8);
```

Measured on 8 × 35 MB statements (320,000 entries, debug build): 28.5 s
sequential, 4.1 s with 8 workers — 6.9×, with identical totals.

## Tested against real messages

Around 260 real messages from a dozen-plus sources — Goldman Sachs (US, UK, EU,
wire), actualbudget, genkgo, Nivaes, Prowide, OpenBankProject, Mbanq, SIX
interbank, CBPR+, ProgressSoft, prog-nov, salesking, Dolibarr, Handelsbanken,
issettled and others — across camt.053 `.02/.03/.04/.08/.09/.11`, camt.052/054,
camt.056 `.01/.02/.03/.04/.08/.10`, camt.029 `.01/.03/.04/.08/.11`, pacs.008
`.01/.02/.07/.08/.09`, pacs.004 `.01/.02/.03/.09/.10/.11`, pacs.002
`.02/.03/.04/.06/.10/.11`, pacs.003 `.01/.02/.03/.04/.09`, pacs.009
`.01/.02/.03/.08/.09/.10`, pain.001 `.03/.09/.11`, pain.002
`.02/.03/.04/.05/.09/.10/.11/.12/.13/.14/.15` and pain.008
`.01/.02/.03/.04/.08/.11`, pacs.007 `.01/.02/.03/.10/.11` and camt.055
`.01/.02/.03` plus SEPA variants.

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
  batch at group level and detail nothing, a camt.056 can cancel a whole batch
  (`GrpCxl`) the same way, and most camt.029 files answer at message level
  only, so the grain is the statement;
- **transaction elements collide across families** — camt.056 calls its
  transaction `TxInf` like pacs.004 does, pacs.002 calls its `TxInfAndSts` like
  pain.002 does, and pacs.008/pain.001 share `CdtTrfTxInf`. Identity is
  therefore the message's own container, and rows are only produced inside it —
  otherwise a camt.056 read as pacs.004 yields plausible rows with every
  return-specific column NULL;
- **one Document, several messages** — pacs.002.001.03 files carry several
  complete `FIToFIPmtStsRpt` blocks, each with its own header; carried context
  resets at each one;
- **agents without a BIC** — SIX identifies the camt.056 assigner only by
  clearing-system member id;
- **containers renamed between eras** — pacs.009's container was
  `FinInstnCdtTrf` before it became `FICdtTrf`, and the first editions of every
  family name the container after the message version itself.

Some apparent bugs turned out to be correct behaviour and were left alone: a
camt statement with only balances yields zero rows because it has no `<Ntry>`,
while a file of the wrong message type is a loud error rather than an empty
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

- `pacs.028` payment status requests — the "where is my money?" message — the
  last payments-family grain not yet covered.
- Remote paths, once the blocker in ADR 0002 is resolved.
- Within-file parallelism is **not** on the roadmap: XML has no safe split
  points, so the parallel unit is the file, and that is already built.

## Building

```sh
git submodule update --init --recursive
make configure && make debug && make test
```

## License

MIT
