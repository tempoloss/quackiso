# quackiso

Query [ISO 20022](https://www.iso20022.org/) and SWIFT MT financial messages as
SQL in DuckDB - no Python preprocessing, no per-schema glue.

```sql
INSTALL quackiso FROM community;
LOAD quackiso;

-- bank statements: one row per booked entry
SELECT booking_date, amount, currency, credit_debit, counterparty_name
FROM read_iso20022('statements/*.xml')
ORDER BY booking_date;
```

Point it at a folder of bank XML or SWIFT MT text, get transactions as rows.

One question in here has a date attached. On 14 November 2026 CBPR+ stops
accepting a fully unstructured postal address, with no grace period, and
`audit_addresses` answers what comes before that migration: of the traffic
already on disk, which parties would be refused, and why. Over SWIFT MT as well
as ISO 20022, because an MT `:50K:` is a name and then free-text lines, which is
exactly the shape that stops being accepted.

```sql
SELECT family, role, address_format, address_text, finding
FROM audit_addresses('inbox/**/*')
WHERE finding IS NOT NULL
ORDER BY family, role;
```

## Functions

| Function | Messages | Grain |
| --- | --- | --- |
| `read_iso20022(path)` | camt.053 statements, camt.054 notifications, camt.052 reports | one row per booked entry |
| `read_camt_transactions(path)` | the same three, at transaction grain | one row per `NtryDtls/TxDtls` |
| `read_camt_balances(path)` | the same three, at balance grain | one row per `Bal` |
| `read_camt_amount_details(path)` | the same three, at amount-block grain | one row per `AmtDtls` block |
| `read_camt_remittance(path)` | the same three, at remittance grain | one row per remittance text leaf |
| `read_pacs008(path)` | pacs.008 FI-to-FI credit transfer (the ISO 20022 MT103) | one row per `CdtTrfTxInf` |
| `read_pacs009(path)` | pacs.009 financial institution transfer (MT202 / MT202COV) | one row per `CdtTrfTxInf` |
| `read_pacs003(path)` | pacs.003 FI-to-FI direct debit (the interbank leg of pain.008) | one row per `DrctDbtTxInf` |
| `read_pacs010(path)` | pacs.010 FI-to-FI direct debit (both sides are banks) | one row per `DrctDbtTxInf` |
| `read_pacs004(path)` | pacs.004 payment return (settled money coming back) | one row per `TxInf` |
| `read_pacs007(path)` | pacs.007 payment reversal (the sender takes it back) | one row per `TxInf` |
| `read_pacs002(path)` | pacs.002 FI-to-FI payment status report | one row per status statement |
| `read_pacs028(path)` | pacs.028 FI-to-FI payment status request (the "where is my money?") | one row per status request |
| `read_pain001(path)` | pain.001 credit transfer initiation | one row per transaction |
| `read_pain002(path)` | pain.002 customer payment status report | one row per status statement |
| `read_pain008(path)` | pain.008 direct debit initiation (the creditor pulls) | one row per collection |
| `read_pain009(path)` | pain.009 mandate initiation (the mandate a pain.008 pulls against) | one row per `Mndt` |
| `read_pain010(path)` | pain.010 mandate amendment | one row per amendment |
| `read_pain011(path)` | pain.011 mandate cancellation | one row per cancellation |
| `read_pain012(path)` | pain.012 mandate acceptance report (the answer to the three above) | one row per answer |
| `read_pain013(path)` | pain.013 creditor payment activation request (request to pay) | one row per `CdtTrfTx` |
| `read_pain014(path)` | pain.014 creditor payment activation request status report | one row per status statement |
| `read_camt056(path)` | camt.056 payment cancellation request | one row per cancellation statement |
| `read_camt055(path)` | camt.055 customer payment cancellation request | one row per cancellation statement |
| `read_camt029(path)` | camt.029 resolution of investigation (the answer to a camt.056) | one row per statement |
| `read_camt027(path)` | camt.027 claim of non-receipt (the money never arrived) | one row per claim |
| `read_camt028(path)` | camt.028 additional payment information | one row per answer |
| `read_camt030(path)` | camt.030 notification of case assignment | one row per notification |
| `read_camt031(path)` | camt.031 reject investigation | one row per rejection |
| `read_camt036(path)` | camt.036 debit authorisation response | one row per response |
| `read_camt037(path)` | camt.037 debit authorisation request (may I take this back?) | one row per request |
| `read_camt087(path)` | camt.087 request to modify a payment | one row per request |
| `read_camt057(path)` | camt.057 notification to receive (money on its way in) | one row per `Itm` |
| `read_mt101(path)` | SWIFT MT101 request for transfer (the FIN original of pain.001) | one row per **transaction** |
| `read_mt103(path)` | SWIFT MT103 single customer credit transfer | one row per message |
| `read_mt104(path)` | SWIFT MT104 direct debit and request for debit transfer (the FIN side of pain.008) | one row per **transaction** |
| `read_mt202(path)` | SWIFT MT202 and MT202COV financial institution transfer | one row per message |
| `read_mt940(path)` | SWIFT MT940 customer statement | one row per `:61:` line |
| `read_mt942(path)` | SWIFT MT942 interim transaction report | one row per `:61:` line |
| `sniff_iso20022(path)` | any ISO 20022 message above, any SWIFT MT message above, or anything claiming to be one | one row per **file** |
| `audit_addresses(path)` | any ISO 20022 message above **or any SWIFT MT**, for the postal address of every party in it | one row per **party** |

`path` is a file or a glob, gzipped or not: `.xml`, `.xml.gz`, and a gzipped file
that kept its `.xml` name all read alike, because the first two bytes decide and
not the name. Every row carries `source_file`, so a glob over a year of
statements stays attributable, and one glob may mix the two. Bytes after the last
gzip member are an error and not padding to ignore, so a half-written append
fails the query rather than quietly truncating the statement. Every function also
takes `threads := n`; see Streaming.

XML readers inspect at most the first 64 KiB of the decompressed stream before
starting quick-xml. A file with no markup in that prefix fails as
`not XML: no markup in the first 64 KiB`; a SWIFT MT marker before markup fails
as `not XML: SWIFT MT marker before markup`. Accepted bytes are replayed, so
valid plain files, gzip files, concatenated gzip members, and FIFO input keep
the same parser behavior. `sniff_iso20022` still returns one result row for
non-XML input instead of aborting the scan.

Five kinds of file are refused by name before any of that, because a container
is not a message and it holds bytes that look like one:

| bytes | refusal |
| --- | --- |
| ZIP, including empty and ZIP64 | `ZIP archive: extract a member before reading` |
| TAR, ustar/GNU/v7, gzipped or not | `TAR archive: extract a member before reading` |
| PKCS#7 or CMS, armored, S/MIME or DER | `PKCS#7 envelope: unwrap, decrypt, or verify it before reading` |
| OpenPGP, armored or binary | `PGP envelope: decrypt it before reading` |
| EBICS request or response | `EBICS transport envelope: process it with an EBICS client first` |

Every public path says the same sentence: `sniff_iso20022` puts it in `error`
and keeps its row, and the readers and the audit raise it with the path. Nothing
is decompressed, decrypted or read as a member. Detection is structural rather
than a magic string, so an XML document quoting `ustar` in a comment, an
ordinary DER certificate, and a runbook mentioning PGP all stay what they are.

An envelope is what the whole file is, so PEM armor has to open the file: a
camt.053 shipping its own detached signature in `<SplmtryData><Envlp><Sgntr>`,
and a payment whose free text reads `MIGRATION BEGIN CMS PHASE 2`, are messages
that carry an envelope rather than envelopes. A media type is read from a MIME
header block and not from a body that names one.

The sniffer recognises SWIFT MT as well, by the block structure rather than by a
namespace: an MT file reports an `mt.nnn` family, a NULL `namespace`, and a
`records` count that is the rows its reader would return. The MT readers take the
same globs and the same gzip as the XML ones. `audit_addresses` is the one
function that reads both wire formats, so it has a guard of its own: it refuses
only bytes that are neither, as `neither XML nor SWIFT MT in the first 64 KiB`.

### read_iso20022

`msg_id`, `account_iban`, `statement_id`, `entry_ref`, `amount`, `currency`,
`credit_debit`, `status`, `booking_date`, `value_date`, `bank_ref`,
`end_to_end_id`, `counterparty_name`, `counterparty_iban`, `remittance_info`,
`statement_kind`, `statement_index`, `entry_index`, `transaction_count`,
`remittance_count`, `reversal_indicator`, `bank_transaction_domain`,
`bank_transaction_family`, `bank_transaction_subfamily`,
`bank_transaction_proprietary`, `bank_transaction_proprietary_issuer`,
`source_file`

An entry is what a bank reconciles against, and it is not always one payment. A
batch posts as one `<Ntry>` of 900 CHF holding three `<NtryDtls>` of one
transaction each, with three end-to-end ids, three counterparties and three
remittance texts under it. `transaction_count` says how many are there, and
`end_to_end_id`, `counterparty_name` and `counterparty_iban` are filled only
when that count is 1. `remittance_info` needs `remittance_count` to be 1 as
well: a transaction carrying free text beside a structured creditor reference has
two answers and no column to hold both.

`statement_kind`, `statement_index` and `entry_index` are the join keys the four
functions below share. All three are NULL for an `<Ntry>` that is not a direct
child of its statement - one inside a transaction summary, or one outside the
statement altogether. Those are still rows, because dropping an entry
under-reports an account, and they are not joinable, because there is nothing
for them to join to.

`reversal_indicator` is the raw `RvslInd`. Amounts stay unsigned and directions
stay as the wire spelled them: a reversal is a fact about the entry, not a minus
sign this applies on your behalf.

### read_camt_transactions

One row per `Ntry/NtryDtls/TxDtls` of a camt.052, camt.053 or camt.054. An entry
with no transactions produces no rows here; its money is on the entry row.

```sql
-- the payments inside a batch, which the entry row cannot show
SELECT e.entry_ref, e.amount AS entry_total, t.end_to_end_id, t.amount, t.creditor_name
FROM read_iso20022('statements/*.xml') e
JOIN read_camt_transactions('statements/*.xml') t
  ON t.source_file = e.source_file
 AND t.statement_index = e.statement_index
 AND t.entry_index = e.entry_index
WHERE e.transaction_count > 1
ORDER BY e.entry_ref, t.entry_details_index, t.transaction_index;
```

`msg_id`, `statement_kind`, `statement_index`, `statement_id`, `account_iban`,
`account_currency`, `entry_index`, `entry_ref`, `entry_amount`,
`entry_currency`, `entry_credit_debit`, `entry_reversal_indicator`,
`entry_status`, `booking_date`, `value_date`, `bank_ref`,
`entry_bank_transaction_domain`, `entry_bank_transaction_family`,
`entry_bank_transaction_subfamily`, `entry_bank_transaction_proprietary`,
`entry_bank_transaction_proprietary_issuer`, `entry_details_index`,
`transaction_index`, `batch_message_id`, `batch_payment_info_id`,
`batch_number_of_transactions`, `batch_total_amount`, `batch_total_currency`,
`batch_credit_debit`, `instruction_id`, `end_to_end_id`, `transaction_id`,
`uetr`, `amount`, `currency`, `credit_debit`, `debtor_name`, `debtor_account`,
`ultimate_debtor_name`, `creditor_name`, `creditor_account`,
`ultimate_creditor_name`, `bank_transaction_domain`, `bank_transaction_family`,
`bank_transaction_subfamily`, `bank_transaction_proprietary`,
`bank_transaction_proprietary_issuer`, `remittance_count`, `source_file`

Nothing here falls back to anything else. `debtor_name` is `RltdPties/Dbtr` and
not a counterparty resolved across both sides; `amount` is `TxDtls/Amt` and not
the entry's; `bank_transaction_domain` is the transaction's `BkTxCd` and not the
entry's. The entry's own values are repeated under `entry_*` so a query can
compare the two rather than be handed one where it asked for the other. A batch
that states five transactions in `batch_number_of_transactions` and carries none
has no rows here; `read_iso20022.transaction_count` is 0 for it.

### read_camt_balances

One row per `<Bal>` directly under a camt.052 `Rpt` or a camt.053 `Stmt`. An
account with no movements is a valid statement, and this is the half of it the
entry grain cannot show.

```sql
-- opening plus the entries equals closing, exactly
SELECT (SELECT amount FROM read_camt_balances('stmt.xml') WHERE balance_type = 'OPBD')
     + (SELECT sum(CASE WHEN credit_debit = 'CRDT' THEN amount ELSE -amount END)
        FROM read_iso20022('stmt.xml'))
     = (SELECT amount FROM read_camt_balances('stmt.xml') WHERE balance_type = 'CLBD')
       AS reconciles;
```

`msg_id`, `statement_kind`, `statement_index`, `statement_id`, `account_iban`,
`account_currency`, `balance_index`, `balance_type`, `balance_type_scheme`,
`balance_subtype`, `balance_subtype_scheme`, `amount`, `currency`,
`credit_debit`, `balance_date`, `source_file`

There is no fixed four-balance projection: a proprietary balance type is a row
like any other, and `balance_type_scheme` says whether the value came from the
published code list (`CODE`) or from the bank's own vocabulary (`PROPRIETARY`).

### read_camt_amount_details

One row per amount block inside an entry-level or transaction-level `<AmtDtls>`:
`InstdAmt`, `TxAmt`, `CntrValAmt`, `AnncdPstngAmt` and every `PrtryAmt`. This is
where a cross-currency entry says what the bank actually applied.

```sql
SELECT entry_ref, amount_kind, amount, currency,
       exchange_source_currency, exchange_target_currency, exchange_rate
FROM read_camt_amount_details('statements/*.xml')
WHERE exchange_rate IS NOT NULL;
```

`msg_id`, `statement_kind`, `statement_index`, `statement_id`, `account_iban`,
`entry_index`, `entry_ref`, `entry_details_index`, `transaction_index`, `scope`,
`amount_kind`, `amount_index`, `proprietary_type`, `amount`, `currency`,
`exchange_source_currency`, `exchange_target_currency`,
`exchange_unit_currency`, `exchange_rate`, `exchange_contract_id`,
`exchange_quotation_time`, `source_file`

`scope` is `ENTRY` or `TRANSACTION`, and `entry_details_index` and
`transaction_index` are NULL on an entry-level row. `amount_index` counts only
the blocks that are on the wire, in schema order, within one `<AmtDtls>`.
`exchange_rate` is VARCHAR: a rate is not money, and the five fraction digits an
ISO 20022 amount allows would round a ten-digit rate or refuse the file over it.

### read_camt_remittance

One row per non-empty remittance text leaf under a transaction: every `Ustrd`,
every `Strd/CdtrRefInf/Ref`, every `Strd/AddtlRmtInf`. Two invoice numbers in
two `<Ustrd>` are two rows, because the string `"INV-1 INV-2"` cannot be taken
apart again.

```sql
SELECT entry_ref, transaction_index, remittance_index, slot, text
FROM read_camt_remittance('statements/*.xml')
ORDER BY entry_index, transaction_index, remittance_index;
```

`msg_id`, `statement_kind`, `statement_index`, `statement_id`, `account_iban`,
`entry_index`, `entry_ref`, `entry_details_index`, `transaction_index`,
`remittance_index`, `structured_index`, `slot`, `text`, `source_file`

`slot` is `UNSTRUCTURED`, `CREDITOR_REFERENCE` or `ADDITIONAL`.
`remittance_index` is 1-based inside its transaction, in schema slot order;
`structured_index` is the document ordinal of the owning `<Strd>`, NULL for a
`Ustrd`. Other structured remittance objects - `RfrdDocInf`, `TaxRmt` and the
rest - are not covered.

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
function that covers the family; `records` counts the record elements a reader
would turn into a row, so a self-closing `<Ntry/>` is on the wire and not in
the count (status and cancellation readers emit group-level rows on top of
that). A truncated download, a stray XSD, a non-ISO payload get a
row whose `error` says why — nothing a file *contains* aborts an inventory
scan. Identity comes from the `Document` namespace, then from
`AppHdr/MsgDefIdr`, then from the era-spelled container names the readers
accept, then from the envelope's binding (BizMsgEnvlp, SWIFTNet DataPDU,
Fedwire, issettled/montran RTGS traffic with no `<Document>` at all). The
namespace wins a disagreement with the header, because it is the schema the
bytes were written against; the header is what answers a CBPR+ message that
declares no namespace at all and states what it is nowhere else. `head.001` —
the AppHdr's own binding, beside the message — is never mistaken for the
message itself. A file whose first bytes are a `{1:` block header or a bare
`:20:` is SWIFT MT: the family is the MT number (`mt.940`), extended by the
block-3 validation flag when there is one (`mt.103.stp`), with `namespace` and
`created` NULL because MT carries neither, and `records` counting the rows the
named reader would return. A file with no markup in it at all is reported as
such rather than handed to the XML parser. The sniffer routes, the readers
judge: a file the sniffer attributes to `read_pacs008` can still fail loudly
there, and that division is the point.

### audit_addresses

On 14 November 2026 CBPR+ stops accepting a fully unstructured postal address,
with no grace period. This is the question that comes before the migration: of
the traffic already on disk, which parties would be refused, and why. MT counts
here as much as MX does, and the same query reads both — an MT `:50K:` is a name
and then free-text lines, which is exactly the shape that stops being accepted.

```sql
-- Every party that would be refused, and what is wrong with it.
SELECT family, role, town, country, address_format, finding
FROM audit_addresses('inbox/**/*')
WHERE finding IS NOT NULL
ORDER BY family, role;

-- The migration's scoreboard: how much of the traffic is in which shape.
SELECT address_format, count(*) AS parties, count(finding) AS would_be_refused
FROM audit_addresses('inbox/**/*')
GROUP BY address_format
ORDER BY parties DESC;

-- What the verdict alone does not say: which repair each refusal actually needs.
-- A free-text address whose town is already written needs labelling, a bare one
-- needs the data collected, and a structured address missing `Ctry` needs
-- neither. All three read `finding IS NOT NULL` and are different jobs.
SELECT CASE WHEN address_text IS NULL THEN 'structured, incomplete'
            ELSE 'free text, ' || address_lines || ' line(s) to label' END AS repair,
       count(*) AS parties
FROM audit_addresses('inbox/**/*')
WHERE finding IS NOT NULL
GROUP BY repair
ORDER BY parties DESC;
```

`family`, `message_id`, `record_index`, `party_path`, `role`, `party_kind`,
`name`, `bic`, `town`, `country`, `address_text`, `address_lines`,
`longest_address_line`, `structured_elements`, `address_format`, `finding`,
`source_file`

One row per party occurrence, which is the grain the question has: a party may
be stated once for a whole payment group (`record_index` NULL) or per
transaction, and pacs.008 alone carries five parties and six agents that may
hold an address. `address_format` is read off the wire and is one of
`STRUCTURED` (dedicated elements, no `AdrLine`), `HYBRID` (`AdrLine` beside a
`TwnNm` and a `Ctry` of their own), `UNSTRUCTURED` (`AdrLine` without both of
them) or `NONE`. `finding` is the rule applied to those facts and is NULL when
nothing in the party would be refused, so `count(finding)` is the size of the
job and `finding` itself says which repair it needs — a missing `TwnNm`, a third
address line, or a line past 70 characters. Address lines are measured in
characters, not bytes.

`address_text` is the address lines themselves, newline-joined and in wire order,
NULL when the party carries none. The counts say how far a party is from the
rule; the text says what there is to work with, and the two do not follow from
each other. Real traffic carries `FOOSTREET 65 / MADRID SPAIN 28010` beside a
bare `BEX 99`, and both are refused for having no `TwnNm` and no `Ctry`: the
first needs its town moved into an element, the second needs a town. Deciding
which is which is a reading of the data, so the audit reports the lines and
stops there rather than guessing a town out of free text.

An agent named by a BIC alone needs no address, and a party carrying none may or
may not have needed one: both are `NONE` with no finding, because whether it was
required there is a usage-guideline question this cannot see. The cash-management
and administration families are outside the mandate (camt.052, camt.053,
camt.054, camt.060, camt.025, admi.024) — their parties are reported with their
format and never with a finding, and `family` is on every row so the line is
visible.

On the MT side the shapes are the same distinction spelled in another alphabet. A
`:50K:`, `:50H:` or letterless `:59:` is a name and then free-text lines:
`UNSTRUCTURED`, and the reason the mandate exists. A `:50F:` numbers its
subfields — `1/` name, `2/` address line, `3/` country and town — so `3/BE/BRUSSELS`
fills a `TwnNm` and a `Ctry`, and the party comes out `HYBRID` with no finding.
An institution given as a BIC (`:57A:`) is `NONE`, as it is in XML.

Two MT details are worth knowing. MT numbers its transactions differently in
every message type, so `record_index` is NULL for MT rather than guessed, and
`party_path` locates the party by field tag instead: `50K`, `52A`, and `52A#2`
for the second occurrence — an MT202COV states `:52a:` in both of its sequences.
And the audit reads MT types no reader here covers, MT300 and MT320 among them,
because a party field is a party field whether or not the rest of the message is
understood. Fields 50 to 59 are the party fields of every payment type: 50 and 59
are the customers at each end, everything between them is an institution on the
route. Which end is which depends on the message and not on the tag, so `role` is
read against the message number: field 59 is the beneficiary of an MT103 and the
debtor of an MT104, because a direct debit collects where a credit transfer pays.

A glob fails on the first unreadable file, the way every reader here does, so
auditing a real inbox is two steps: `sniff_iso20022` to find out what is in the
folder, then `audit_addresses` over the files that are messages. ADR 0008
records why this is a function of its own rather than columns on the readers.

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

### read_pain009

`msg_id`, `created`, `initiating_party`, `mandate_id`, `mandate_request_id`,
`sequence_type`, `frequency`, `first_collection_date`,
`final_collection_date`, `collection_amount`, `currency`, `creditor_name`,
`creditor_account`, `creditor_agent_bic`, `debtor_name`, `debtor_account`,
`debtor_agent_bic`, `ultimate_debtor_name`, `referred_document_number`,
`source_file`

The mandate itself, which pain.008 only ever names by id: who may collect, from
which account, how much and how often. A mandate that has not been registered
yet has no `mandate_id` at all — `mandate_request_id` is the only identifier
the request has. There is no `mandate_signed_on` here: the signature date lives
in pain.008's `MndtRltdInf` and nowhere in the mandate block.

### read_pain010

`msg_id`, `created`, `initiating_party`, `instructing_agent_bic`,
`instructed_agent_bic`, `amendment_reason`, `amendment_originator`,
`original_mandate_id`, `mandate_id`, `sequence_type`, `frequency`,
`collection_amount`, `currency`, `creditor_name`, `creditor_account`,
`debtor_name`, `debtor_account`, `debtor_agent_bic`, `source_file`

An amendment carries both states: the mandate it changes
(`original_mandate_id`) and what it becomes. Every column after that names the
**new** mandate, so an amendment that changes one account states only that
account and leaves the rest NULL — what is absent is what does not change.

### read_pain011

`msg_id`, `created`, `initiating_party`, `instructing_agent_bic`,
`instructed_agent_bic`, `cancellation_reason`, `cancellation_reason_info`,
`original_mandate_id`, `creditor_name`, `creditor_account`, `debtor_name`,
`debtor_account`, `debtor_agent_bic`, `ultimate_debtor_name`, `source_file`

A cancellation names an existing mandate, so there is no `mandate_request_id`.
The detail columns are filled only when the sender repeated the mandate at
`OrgnlMndt/OrgnlMndt` — the same-named element nested inside the choice
wrapper; naming the mandate by id alone is legal and complete. Reason `NARR`
means the reason is the text, which is `cancellation_reason_info`.

### read_pain012

`msg_id`, `created`, `initiating_party`, `instructing_agent_bic`,
`instructed_agent_bic`, `original_msg_id`, `original_msg_name_id`,
`original_created`, `accepted`, `rejection_reason`, `original_mandate_id`,
`sequence_type`, `frequency`, `first_collection_date`, `creditor_name`,
`creditor_account`, `creditor_agent_bic`, `debtor_name`, `debtor_account`,
`debtor_agent_bic`, `referred_document_number`, `source_file`

One report shape answers all three requests, so `original_msg_name_id` is the
only thing that says which. `accepted` is text, as the wire spelled it, the
same discipline as `group_cancellation` in read_camt056.

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

### read_camt027

`assignment_id`, `assignment_created`, `assigner`, `assignee`, `case_id`,
`case_creator`, `original_msg_id`, `original_msg_name_id`,
`original_instr_id`, `original_amount`, `original_currency`,
`original_execution_date`, `original_settlement_date`, `source_file`

Where an investigation usually starts: the money never arrived. The six
case columns — the assignment pair and the case with its creator — are the
same in all seven investigation readers, so a case is one join across them.
A claim moves no money, so every monetary column is the missing payment's.
`Assgnr`, `Assgne` and `Cretr` are each a choice of a party or an agent, and
one real message mixes the two.

### read_camt028

`assignment_id`, `assignment_created`, `assigner`, `assignee`, `case_id`,
`case_creator`, `original_instr_id`, `original_amount`, `original_currency`,
`original_execution_date`, `original_settlement_date`, `remittance_info`,
`source_file`

The answer that supplies what the investigation asked for, which in every
published sample is the remittance detail the other side was missing.

### read_camt030

`assignment_id`, `assignment_created`, `assigner`, `assignee`, `case_id`,
`case_creator`, `notification_id`, `notification_from`, `notification_to`,
`notification_created`, `justification`, `source_file`

**Two party pairs, and they need not agree.** `notification_from`/`_to` are
who is being told; `assigner`/`assignee` are who the case is now with. In the
corpus sample the notification goes to EEEEUS33 while the case is assigned to
FFFFUS33, so one pair of columns would report the wrong bank. `justification`
is a bare code here (`CANC`, `FTHI`, `MINE`), where camt.031 wraps its reason
in `RjctnRsn`.

### read_camt031

`assignment_id`, `assignment_created`, `assigner`, `assignee`, `case_id`,
`case_creator`, `rejection_reason`, `source_file`

The case will not be worked. There is no underlying payment block at all: the
assignment, the case and the reason are the whole message.

### read_camt036

`assignment_id`, `assignment_created`, `assigner`, `assignee`, `case_id`,
`case_creator`, `debit_authorised`, `source_file`

The customer's answer to a camt.037. `debit_authorised` is text, as the wire
spelled it. The schema lets the response restate the amount and value date it
agrees to, but no published sample carries either, so there is no column for
them to be NULL in.

### read_camt037

`assignment_id`, `assignment_created`, `assigner`, `assignee`, `case_id`,
`case_creator`, `original_instr_id`, `original_amount`, `original_currency`,
`original_execution_date`, `original_settlement_date`, `cancellation_reason`,
`amount_to_debit`, `debit_currency`, `source_file`

May the bank take this back off the account? `amount_to_debit` is what is being
asked for and is **not** the original: a bank that kept its charges asks for
less than it paid out, so both amounts are columns with their own currencies.

### read_camt087

`assignment_id`, `assignment_created`, `assigner`, `assignee`, `case_id`,
`case_creator`, `original_msg_id`, `original_msg_name_id`,
`original_instr_id`, `original_end_to_end_id`, `original_amount`,
`original_currency`, `original_execution_date`, `original_settlement_date`,
`modified_amount`, `modified_currency`, `modified_remittance_info`,
`source_file`

Not "cancel it" but "send it differently". The original and the modification
sit in one row, so the difference is a subtraction rather than a second query.
`Mod` states its amount as `IntrBkSttlmAmt` on the interbank side and as
`Amt/InstdAmt` on the pain side; both feed `modified_amount`.

### read_pacs028

`msg_id`, `instructing_agent_bic`, `instructed_agent_bic`, `scope`,
`status_request_id`, `original_msg_id`, `original_msg_name_id`,
`original_instr_id`, `original_end_to_end_id`, `original_tx_id`,
`original_uetr`, `original_amount`, `original_currency`,
`original_settlement_date`, `original_debtor_name`, `original_creditor_name`,
`source_file`

pacs.002 with the answer removed: one bank asking another for the status of a
payment it already sent. A request carries no status and no reason of its own,
so there is no `amount` column at all - every monetary column is `original_*`,
read from the carried copy of the original (`OrgnlTxRef`) when the request
includes one. `scope` is `GROUP` or `TRANSACTION`: a request that names a whole
original message and details no transactions is one `GROUP` row, because a
reader whose grain is the transaction parses "where is batch X?" to zero rows.
The requesting pair is stated once on the group header and carried down to
every row. A group row is produced only when the request detailed no
transactions, since transaction rows already carry the message-level reference;
the grain and the alternatives are recorded in
[`docs/adr/0007-a-request-with-no-transactions-is-a-row.md`](docs/adr/0007-a-request-with-no-transactions-is-a-row.md).

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

Files are parsed as an event stream, one entry at a time. A 1.7 GB statement of
three million entries is read in under 2 MB of live heap and about 1 MB of added
resident memory; peak does not follow file size.

**That number is measured, not remembered.** `src/membound.rs` writes the
statement, runs the scan loop `read_iso20022` runs, and reads the peak back from
a tracking allocator and from the kernel:

```console
$ cargo test --release membound -- --ignored --nocapture
[membound] the documented statement: 3000000 rows, 3000000 entries, 1.73 GB on
disk -> peak live heap 1.73 MiB, peak RSS +1.15 MiB (process peak 6.87 MiB)
```

**It is a standalone parser figure.** The test binary loads no DuckDB, so this
is what one scan adds to its own process — `VmHWM`, reset immediately before the
parse — not a process total and not an increment over DuckDB. The heap half of
it is the same number on every machine tried; the resident half moves by a few
hundred KiB, and by more than that between runs, which is why the resident
figure is held to a ceiling and the heap to a band.

**Gzip costs a decoder, not a fraction of the file.** The same statement gzipped
parses in the same batch, plus one decoder's worth of fixed state: an input
buffer, an LZ77 window, huffman tables. That is 131,369 bytes, measured as the
difference and recorded as `GZIP_HEAP`, and nothing of it is per entry.

```console
$ cargo test --release --lib peak_does_not_follow_compression -- --nocapture
[membound] 32k entries: 32000 rows, 32000 entries, 18.4 MB on disk -> peak live heap 1.72 MiB, peak RSS +1.13 MiB (process peak 6.53 MiB)
[membound] 32k entries gzipped: 32000 rows, 32000 entries, 0.7 MB on disk -> peak live heap 1.85 MiB, peak RSS +0.13 MiB (process peak 6.66 MiB)
[membound] the decoder adds 131369 bytes
```

**What compression does change is which number bounds the subtree.** An entry
used to be capped by the file it arrived in: a 16 MiB `<Ntry>` needed 16 MiB on
disk. Gzipped it needs a hundredth of that, and the peak is still six times the
inflated entry — so the term to watch is the inflated size, which `ls` no longer
shows:

```console
$ cargo test --release --lib a_small_gzip_can_carry_a_large_subtree -- --nocapture
[membound] one 16.00 MiB entry gzipped: 200 rows, 200 entries, 0.0 MB on disk -> peak live heap 97.26 MiB, peak RSS +64.84 MiB (process peak 69.75 MiB)
```

Inside DuckDB the same query adds 9.0 MiB to a 57 MiB baseline on the machine
above — that one is host-dependent, which is why CI asserts a 16 MiB ceiling
rather than a number. What it does not track is the file: the same 9 MiB answers
for a 1.73 GB statement as for a small one, because the growth is DuckDB's own
per-chunk machinery settling and not the reader.
`scripts/measure_in_duckdb.py` is that second measurement, on the same generated
statement.

**Streaming means aggregating.** Both figures are for a query that consumes rows
and drops them — an aggregate, a filter, a `LIMIT`. Asking for the rows
themselves is a different budget and a measured one: returning all three million
rows of the documented statement costs 3,991 MiB, 443× the scan that produced
them. That is the result set, not the parser.

**Bounded is not independent of the input.** The peak is one output batch plus
the largest single subtree, and both terms move it:

| input | peak live heap |
| --- | --- |
| 8× the file, same entry shape | unchanged, 1.73 MiB |
| 4 KiB of remittance text per row | 9.68 MiB — 2048 rows are in flight at once |
| one 16 MiB `<Ntry>` | 97 MiB — a fat subtree is live as a copy, as a deserialized struct, and as a row |
| 24 files instead of 8, same 8 workers | 25 batches at most — 19–27 MiB by machine, never by glob |
| 20,000 entries copied verbatim out of the corpus | 1.64 MiB — real shapes, same bound |
| one entry of 32,000 transactions, read at transaction grain | 99 MiB, which is the entry's own subtree plus one batch |

Those rows are tests, not prose. They run on every `cargo test` and on every
push; the 1.7 GB reproduction and the in-DuckDB measurement run at full size
every Monday. The pathological single entry is the one input that can hurt —
memory follows the largest subtree at about six times its size, so a
hypothetical 300 MB `<Ntry>` is a 1.8 GB parse. Statements with millions of
ordinary entries, which do exist, are the case that is bounded.

**The four supplementary camt readers are bounded the same way**, and the last
row above is why they need their own measurement. Each walks the statement the
entry reader walks and then a cursor over the entry it was handed, and a cursor
is two integers: an entry of 32,000 transactions is 32,000 rows out, taken 2048
at a time. A `Vec` of every row an entry produces would pass every case that
has two transactions per entry and fail only here, so
`one_entry_of_many_transactions_costs_a_cursor_and_not_a_queue` measures the
difference against a plain entry scan of the same file and holds it to one
batch. `read_camt_balances` skips every `<Ntry>` subtree to reach the balances,
which is why it is the cheapest of the five at 0.90 MiB over a statement of any
size.

**A glob is parsed in parallel, one worker per file.** The unit is the whole
file because XML has no safe split points — there is no way to start parsing a
statement in the middle, unlike a block-structured format such as OSM's PBF —
so a single document is always one sequential pass. Workers claim files from a
shared counter and hand vector-sized batches over a bounded channel, so memory
stays O(threads × batch) regardless of how many files the glob matched —
measured through DuckDB, four copies of the documented statement behind four
workers add 15.1 MiB to the baseline, against 9.0 MiB for one of them. Rows of
one file stay in order; files interleave, which is what `source_file` is for. A
malformed amount in any file still fails the whole query.

The default is one worker per file, capped at the machine's parallelism;
`threads := 1` forces the sequential scan, `threads := n` pins the pool, itself
capped at four times the machine's parallelism:

```sql
SELECT count(*), SUM(amount)
FROM read_iso20022('statements/*.xml', threads := 8);
```

Measured on 8 × 35 MB statements (320,000 entries, debug build): 28.5 s
sequential, 4.1 s with 8 workers — 6.9×, with identical totals.

## Tested against real messages

Around 283 real messages from a dozen-plus sources — Goldman Sachs (US, UK, EU,
wire), actualbudget, genkgo, Nivaes, Prowide, OpenBankProject, Mbanq, SIX
interbank, CBPR+, ProgressSoft, prog-nov, salesking, Dolibarr, Handelsbanken,
issettled and others — across camt.053 `.02/.03/.04/.08/.09/.11`, camt.054,
camt.056 `.01/.02/.03/.04/.08/.10`, camt.029 `.01/.03/.04/.08/.11`, pacs.008
`.01/.02/.07/.08/.09`, pacs.004 `.01/.02/.03/.09/.10/.11`, pacs.002
`.02/.03/.04/.06/.10/.11`, pacs.003 `.01/.02/.03/.04/.09`, pacs.009
`.01/.02/.03/.08/.09/.10`, pain.001 `.03/.09/.11`, pain.002
`.02/.03/.04/.05/.09/.10/.11/.12/.13/.14/.15` and pain.008
`.01/.02/.03/.04/.08/.11`, pacs.007 `.01/.02/.03/.10/.11` and camt.055
`.01/.02/.03` plus SEPA variants. The mandate and investigation families are the
ISO published business examples carried by prog-nov and the Nivaes resources:
pain.009 to pain.012 `.04`, camt.027 `.04`, camt.028 `.04`, camt.030 `.04`,
camt.031 `.04`, camt.036 `.03`, camt.037 `.01/.04` and camt.087 `.01`. camt.052
has no bank file in the corpus; its fixture is hand-written against `.08`.
pacs.028 likewise has no bank file, so its fixtures are hand-written too, three
of the eight saying so in their header.

Every fix in this reader came from one of those files:

- **namespace prefixes** — `<Doc:CdtTrfTxInf>`, `<urn2:...>`: tag names are
  normalised while a subtree is copied, which previously produced an ill-formed
  document;
- **one-sided entries** — a `CRDT` entry often carries only `<Cdtr>`; the
  counterparty falls back to the other side when the correct one names nobody
  and states no account. Name and account always come from the *same* side, so
  an unnamed payer whose account is on the wire reads as a NULL name beside
  that account rather than borrowing the name from the other party. An
  `UltmtDbtr`/`UltmtCdtr` stands in for its own side's missing name;
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
  complete `FIToFIPmtStsRpt` blocks, each with its own header. Every payment
  and status reader scopes its container by depth and clears the carried
  context when a new one opens, so this holds for any family, not just
  pacs.002. See
  [`docs/adr/0004-container-scope-is-message-scope.md`](docs/adr/0004-container-scope-is-message-scope.md);
- **agents without a BIC** — SIX identifies the camt.056 assigner only by
  clearing-system member id;
- **containers renamed between eras** — pacs.009's container was
  `FinInstnCdtTrf` before it became `FICdtTrf`, and the first editions of every
  family name the container after the message version itself.

Some apparent bugs turned out to be correct behaviour and were left alone: a
camt statement with only balances yields zero rows from `read_iso20022` because
it has no `<Ntry>` — `read_camt_balances` is where those balances are — while a
file of the wrong message type is a loud error rather than an empty table: a
template with `{placeholder}` amounts or a pacs.002 pointed at `read_pacs004`
fails instead of silently returning nothing.

## Deliberate non-features

- **No `s3://` or `https://` paths.** Attempted and removed: opening a remote file
  needs the executing query's client context, which `duckdb-rs` does not expose
  from a safe table function. See
  [`docs/adr/0002-no-remote-paths.md`](docs/adr/0002-no-remote-paths.md).
- **No XSD validation.** Every defect the real corpus exposed was the reader being
  too strict, not the file being invalid. See
  [`docs/adr/0003-no-xsd-validation.md`](docs/adr/0003-no-xsd-validation.md).
- **`threads := n` is not obeyed literally.** It is capped at the file count and
  at four times the machine's parallelism, because the failure a hundred thousand
  threads produces is resource exhaustion mid-scan rather than a slow scan. See
  [`docs/adr/0005-explicit-thread-count-is-capped.md`](docs/adr/0005-explicit-thread-count-is-capped.md).
- **Nine known columns are missing on purpose.** camt.055 has no `case_id`,
  pacs.007 no `original_settlement_date`, pacs.002 no group-level
  `OrgnlNbOfTxs`/`OrgnlCtrlSum`, pacs.009 no non-COV `RmtInf`, camt.029 no
  `PAYMENT_INFO` scope. The mandate and investigation readers added three more:
  no `case_creator` on camt.055, camt.056 and camt.029, no `amount_to_debit` or
  `value_date_to_debit` on camt.036 and no `value_date_to_debit` on camt.037,
  and no instructing or instructed agent columns on `read_pain009`. Each widens
  a published schema and is argued separately.
  See [`docs/adr/0006-audit-findings-deferred.md`](docs/adr/0006-audit-findings-deferred.md).
- **Archives and envelopes are named, not opened.** A ZIP or TAR of the day's
  statements, a PKCS#7-signed camt.053, a PGP-encrypted pain.001, an EBICS
  request: each is refused by name with what to do about it. A TAR of two
  statements used to parse as one file and return the entries of both members
  under one `source_file`, which is two accounts added together with nothing on
  the row saying so. Detection is structural rather than a magic string - a
  checksummed TAR header, a CMS content-type OID, a walkable OpenPGP packet
  chain - so an ordinary XML document that quotes the word `ustar` stays XML.
  `sniff_iso20022` reports the reason in `error` and keeps its row; every reader
  and the audit raise it with the path.
- **A statement's details are separate functions, not wider entry rows.** A
  batched entry has three end-to-end ids and one column to put them in, so the
  fix is a grain and not a column: `read_camt_transactions`,
  `read_camt_balances`, `read_camt_amount_details` and
  `read_camt_remittance`. Nothing in them falls back to an entry value, no row is
  invented for a batch that carries no transaction, and no nested type is
  returned. See
  [`docs/adr/0009-camt-details-have-separate-grains.md`](docs/adr/0009-camt-details-have-separate-grains.md).

## Roadmap

- Remote paths, once the blocker in ADR 0002 is resolved.
- Archive members, now that an archive is named instead of misparsed. A member
  reader needs a grain of its own: `source_file` would have to say which member
  of which archive, and a glob over archives would have to say both. That is a
  design, not an extension of an existing function.
- The four value fixes listed in ADR 0006 -- pre-2009 pacs.007 reason spellings,
  a message-level `<Case><Id>` fallback, `RtrChain` agents, and telling an
  unparseable amount from an absent one. No design work needed.
- MT address lines as reader columns. `audit_addresses` reads them, so the parse
  exists; what is missing is `read_mt103` exposing the lines it drops beside the
  name it keeps. The audit answers the migration question without them, which is
  why this is a roadmap item and not a defect.
- Within-file parallelism is **not** on the roadmap: XML has no safe split
  points, so the parallel unit is the file, and that is already built.

## Building

```sh
git submodule update --init --recursive
make configure && make debug && make test
```

## License

MIT
