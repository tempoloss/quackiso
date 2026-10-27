# Column reference

Columns and grain of each reader. The function list is in [functions.md](functions.md); `audit_addresses` has [its own page](address-audit.md).

## read_iso20022

`msg_id`, `account_iban`, `statement_id`, `entry_ref`, `amount`, `currency`,
`credit_debit`, `status`, `booking_date`, `value_date`, `bank_ref`,
`end_to_end_id`, `counterparty_name`, `counterparty_iban`, `remittance_info`,
`statement_kind`, `statement_index`, `entry_index`, `transaction_count`,
`remittance_count`, `reversal_indicator`, `bank_transaction_domain`,
`bank_transaction_family`, `bank_transaction_subfamily`,
`bank_transaction_proprietary`, `bank_transaction_proprietary_issuer`,
`additional_entry_info`, `source_file`

An entry is what a bank reconciles against, and it is not always one payment. A
batch posts as one `<Ntry>` of 900 CHF holding three `<NtryDtls>` of one
transaction each, with three end-to-end ids, three counterparties and three
remittance texts under it. `transaction_count` says how many are there, and
`end_to_end_id`, `counterparty_name` and `counterparty_iban` are filled only
when that count is 1. `remittance_info` needs `remittance_count` to be 1 as
well: a transaction carrying free text beside a structured creditor reference or
an invoice number has two answers and no column to hold both.

`statement_kind`, `statement_index` and `entry_index` are the join keys the
supplementary camt functions below share (`read_camt_statements` has no
`entry_index`, its grain being the statement). All three are NULL for an
`<Ntry>` that is not a direct child of its statement - one inside a transaction
summary, or one outside the statement altogether. Those are still rows, because
dropping an entry under-reports an account, and they are not joinable, because
there is nothing for them to join to.

`additional_entry_info` is `Ntry/AddtlNtryInf`, the bank's free text about the
entry, as stated. It is a column and not a remittance slot because it is 0..1 on
the entry and an entry with no transaction has no remittance row to carry it.

`reversal_indicator` is the raw `RvslInd`. Amounts stay unsigned and directions
stay as the wire spelled them: a reversal is a fact about the entry, not a minus
sign this applies on your behalf.

## read_camt_transactions

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
`bank_transaction_proprietary_issuer`, `remittance_count`,
`additional_transaction_info`, `local_instrument_code`,
`local_instrument_proprietary`, `purpose_code`, `purpose_proprietary`,
`return_reason_code`, `return_reason_proprietary`, `charges_amount`,
`charges_currency`, `source_file`

Nothing here falls back to anything else. `debtor_name` is `RltdPties/Dbtr` and
not a counterparty resolved across both sides; `amount` is `TxDtls/Amt` and not
the entry's; `bank_transaction_domain` is the transaction's `BkTxCd` and not the
entry's. The entry's own values are repeated under `entry_*` so a query can
compare the two rather than be handed one where it asked for the other. A batch
that states five transactions in `batch_number_of_transactions` and carries none
has no rows here; `read_iso20022.transaction_count` is 0 for it.

The nine columns after `remittance_count` each read exactly one element of the
transaction: `additional_transaction_info` is `AddtlTxInf`;
`local_instrument_code` and `local_instrument_proprietary` are `LclInstrm/Cd` and
`LclInstrm/Prtry`; `purpose_code` and `purpose_proprietary` are `Purp/Cd` and
`Purp/Prtry`; `return_reason_code` and `return_reason_proprietary` are
`RtrInf/Rsn/Cd` and `RtrInf/Rsn/Prtry`. A code is not copied into the
proprietary column or the other way round. `charges_amount` and
`charges_currency` are `Chrgs/TtlChrgsAndTaxAmt`, filled only when the
transaction states exactly one such total: camt.053.001.02 repeats `Chrgs`, and
a sum or a pick of two would be a figure the bank never wrote. A malformed total
fails the scan like any other amount.

## read_camt_balances

One row per `<Bal>` directly under a camt.052 `Rpt` or a camt.053 `Stmt`. An
account with no movements is a valid statement, and this is the half of it the
entry grain cannot show.

An opening balance plus the booked entries, each signed by its own direction,
is a closing balance. This is the check with a row per statement, and a
`difference` that is NULL where the statement states no opening or no closing
balance:

```sql
WITH balances AS (
    SELECT source_file, statement_index,
           max(CASE credit_debit WHEN 'DBIT' THEN -amount ELSE amount END)
               FILTER (WHERE balance_type = 'OPBD') AS opening,
           max(CASE credit_debit WHEN 'DBIT' THEN -amount ELSE amount END)
               FILTER (WHERE balance_type = 'CLBD') AS closing
    FROM read_camt_balances('statements/*.xml')
    GROUP BY source_file, statement_index
),
entries AS (
    SELECT source_file, statement_index,
           sum(CASE credit_debit WHEN 'DBIT' THEN -amount ELSE amount END) AS movement
    FROM read_iso20022('statements/*.xml')
    WHERE status = 'BOOK'
    GROUP BY source_file, statement_index
)
SELECT b.source_file, b.statement_index, b.opening,
       coalesce(e.movement, 0) AS movement, b.closing,
       b.opening + coalesce(e.movement, 0) - b.closing AS difference
FROM balances b
LEFT JOIN entries e USING (source_file, statement_index)
ORDER BY b.source_file, b.statement_index;
```

`msg_id`, `statement_kind`, `statement_index`, `statement_id`, `account_iban`,
`account_currency`, `balance_index`, `balance_type`, `balance_type_scheme`,
`balance_subtype`, `balance_subtype_scheme`, `amount`, `currency`,
`credit_debit`, `balance_date`, `source_file`

There is no fixed four-balance projection: a proprietary balance type is a row
like any other, and `balance_type_scheme` says whether the value came from the
published code list (`CODE`) or from the bank's own vocabulary (`PROPRIETARY`).

## read_camt_amount_details

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

## read_camt_remittance

One row per non-empty remittance text leaf under a transaction: every `Ustrd`,
every `Strd/RfrdDocInf/Nb`, every `Strd/CdtrRefInf/Ref`, every
`Strd/AddtlRmtInf`. Two invoice numbers in two `<Ustrd>` are two rows, because
the string `"INV-1 INV-2"` cannot be taken apart again.

```sql
SELECT entry_ref, transaction_index, remittance_index, slot, text
FROM read_camt_remittance('statements/*.xml')
ORDER BY entry_index, transaction_index, remittance_index;
```

`msg_id`, `statement_kind`, `statement_index`, `statement_id`, `account_iban`,
`entry_index`, `entry_ref`, `entry_details_index`, `transaction_index`,
`remittance_index`, `structured_index`, `slot`, `text`, `source_file`

`slot` is `UNSTRUCTURED`, `REFERRED_DOCUMENT`, `CREDITOR_REFERENCE` or
`ADDITIONAL`. `remittance_index` is 1-based inside its transaction, in schema
slot order: inside a `<Strd>`, the invoice numbers first, then the creditor
reference, then the additional text. `structured_index` is the document ordinal
of the owning `<Strd>`, NULL for a `Ustrd`. `REFERRED_DOCUMENT` is the invoice
number a cash application matches on; `RfrdDocAmt`, `TaxRmt` and the other
structured remittance objects are not covered.

## read_camt_statements

One row per `Stmt` of a camt.053, `Rpt` of a camt.052 or `Ntfctn` of a camt.054: the header the statement states about itself, beside the number of entries and balances it holds.

```sql
-- what the bank says a statement holds, beside what was found
SELECT account_iban, electronic_sequence, from_date, to_date,
       summary_entries, entry_count, summary_entries - entry_count AS short_by
FROM read_camt_statements('statements/*.xml')
ORDER BY account_iban, electronic_sequence;
```

`msg_id`, `message_created`, `message_page`, `message_last_page`,
`statement_kind`, `statement_index`, `statement_id`, `electronic_sequence`,
`legal_sequence`, `statement_created`, `from_date`, `to_date`,
`copy_duplicate`, `page_number`, `last_page`, `account_iban`,
`account_currency`, `account_owner_name`, `account_servicer_bic`,
`summary_entries`, `summary_sum`, `summary_net_amount`,
`summary_net_credit_debit`, `summary_credit_entries`, `summary_credit_sum`,
`summary_debit_entries`, `summary_debit_sum`, `entry_count`, `balance_count`,
`source_file`

The sequence number of a statement is on no other row, as `read_mt940` has
`statement_number` and `sequence_number`: `ElctrncSeqNb` and `LglSeqNb` are
columns here, so a query can ask whether Tuesday's statement arrived, arrived
twice, or follows the one before it
([recipes](recipes.md#continuity-across-files),
[ADR 0012](adr/0012-the-statement-header-is-its-own-grain.md)). The row joins to
the other camt functions on `source_file` and `statement_index`.

Each column reads exactly the element it is named for and falls back to nothing,
and a header the statement does not state is NULL, never the previous
statement's: the message fields (`msg_id` and the three `message_*`, from
`GrpHdr`) belong to the message and are kept across the statements inside it,
and every other field is reset where a statement opens. `account_servicer_bic` is
`Acct/Svcr/FinInstnId/BICFI`, or the pre-2019 `BIC`. The `summary_*` columns are
what the bank states in `TxsSummry` (`TtlNtries`, `TtlCdtNtries`,
`TtlDbtNtries`).

`entry_count` and `balance_count` are not on the wire: they are what the reader
counted. `entry_count` is the direct `<Ntry>` children of the statement, which is
the number of rows `read_iso20022` returns for that `statement_index` - a
self-closing `<Ntry/>` is no entry in either. `balance_count` is the direct
`<Bal>` children, which is the number of rows `read_camt_balances` returns for
it; a self-closing `<Bal/>` is a balance. Beside `summary_entries` they make a
short statement visible in SQL, and the difference is a question to ask of the
file and not proof that it is damaged: a bank that omits `TxsSummry` leaves
`summary_entries` NULL.

The counts and sequence numbers are BIGINT, and a text that is not a whole
number fails the scan like a malformed amount does:
`file.xml: <ElctrncSeqNb> "forty" is not a whole number`. The `summary_*` sums
are `DECIMAL(38,5)`. `message_created`, `statement_created`, `from_date` and
`to_date` are `TIMESTAMP`, normalised to UTC ([types](types.md)), so
`2026-10-03T06:15:00+02:00` is `2026-10-03 04:15:00`. `message_last_page` and
`last_page` keep the wire's spelling (`true`, `false`, `1`, `0`) as
`reversal_indicator` does. The page of a statement is `StmtPgntn`, the element
camt.053 names; camt.052 and camt.054 name theirs `RptPgntn` and `NtfctnPgntn`,
which are not read. A file with no statement at all raises the same error as the
other camt functions and is not an empty table.

## sniff_iso20022

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
`:20:` is SWIFT MT: the family is the MT number (`mt.940`) of the first message
in the file that has a reader, so a free-format MT199 ahead of an MT103 does not
hide the payment, extended by the block-3 validation flag when there is one
(`mt.103.stp`), with `namespace` and `created` NULL because MT carries neither,
and `records` counting the rows the named reader would return from the messages
of that type. A file with no markup in it at all is reported as such rather
than handed to the XML parser. The sniffer routes, the readers
judge: a file the sniffer attributes to `read_pacs008` can still fail loudly
there, and that division is the point.

## quackiso_functions

One row per function this extension registers, with a one-sentence description and a runnable example.

```sql
SELECT function_name, description, example
FROM quackiso_functions()
WHERE function_name LIKE 'read_camt%'
ORDER BY function_name;
```

`function_name`, `description`, `example`

It takes no argument. The C API a loadable extension registers table functions
through has no description or example setter, so `duckdb_functions()` shows these
functions with both columns empty; this table is the answer, and
[ADR 0013](adr/0013-function-help-is-a-table-function.md) records why it is a
function and not a macro per reader. Each `example` is a `SELECT * FROM` call on a
placeholder path in the style of this page (`statements/*.xml`, `payments/*.xml`,
`mt/*.txt`). The table is checked against the loaded extension on every build:
the names must equal what `LOAD` adds to `duckdb_functions()`, and every example
must return rows against a fixture.

## read_pacs004

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

## read_pacs008

`msg_id`, `instr_id`, `end_to_end_id`, `tx_id`, `uetr`, `amount`, `currency`,
`settlement_date`, `charge_bearer`, `debtor_name`, `debtor_account`,
`debtor_agent_bic`, `creditor_name`, `creditor_account`, `creditor_agent_bic`,
`remittance_info`, `source_file`

## read_pain001

`msg_id`, `initiating_party`, `payment_info_id`, `payment_method`,
`requested_execution_date`, `debtor_name`, `debtor_account`, `debtor_agent_bic`,
`instr_id`, `end_to_end_id`, `uetr`, `amount`, `currency`, `charge_bearer`,
`creditor_name`, `creditor_account`, `creditor_agent_bic`, `remittance_info`,
`source_file`

In pain.001 the payer sits on the `<PmtInf>` group rather than the transaction,
so `debtor_*`, `payment_method` and `requested_execution_date` are carried down to
every transaction in the group.

## read_pain002

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

## read_pacs009

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

## read_pacs007

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

## read_pacs003

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

## read_pacs002

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

## read_camt056

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

## read_camt055

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

## read_pain008

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

## read_pain009

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

## read_pain010

`msg_id`, `created`, `initiating_party`, `instructing_agent_bic`,
`instructed_agent_bic`, `amendment_reason`, `amendment_originator`,
`original_mandate_id`, `mandate_id`, `sequence_type`, `frequency`,
`collection_amount`, `currency`, `creditor_name`, `creditor_account`,
`debtor_name`, `debtor_account`, `debtor_agent_bic`, `source_file`

An amendment carries both states: the mandate it changes
(`original_mandate_id`) and what it becomes. Every column after that names the
**new** mandate, so an amendment that changes one account states only that
account and leaves the rest NULL — what is absent is what does not change.

## read_pain011

`msg_id`, `created`, `initiating_party`, `instructing_agent_bic`,
`instructed_agent_bic`, `cancellation_reason`, `cancellation_reason_info`,
`original_mandate_id`, `creditor_name`, `creditor_account`, `debtor_name`,
`debtor_account`, `debtor_agent_bic`, `ultimate_debtor_name`, `source_file`

A cancellation names an existing mandate, so there is no `mandate_request_id`.
The detail columns are filled only when the sender repeated the mandate at
`OrgnlMndt/OrgnlMndt` — the same-named element nested inside the choice
wrapper; naming the mandate by id alone is legal and complete. Reason `NARR`
means the reason is the text, which is `cancellation_reason_info`.

## read_pain012

`msg_id`, `created`, `initiating_party`, `instructing_agent_bic`,
`instructed_agent_bic`, `original_msg_id`, `original_msg_name_id`,
`original_created`, `accepted`, `rejection_reason`, `original_mandate_id`,
`sequence_type`, `frequency`, `first_collection_date`, `creditor_name`,
`creditor_account`, `creditor_agent_bic`, `debtor_name`, `debtor_account`,
`debtor_agent_bic`, `referred_document_number`, `source_file`

One report shape answers all three requests, so `original_msg_name_id` is the
only thing that says which. `accepted` is text, as the wire spelled it, the
same discipline as `group_cancellation` in read_camt056.

## read_camt029

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

## read_camt027

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

## read_camt028

`assignment_id`, `assignment_created`, `assigner`, `assignee`, `case_id`,
`case_creator`, `original_instr_id`, `original_amount`, `original_currency`,
`original_execution_date`, `original_settlement_date`, `remittance_info`,
`source_file`

The answer that supplies what the investigation asked for, which in every
published sample is the remittance detail the other side was missing.

## read_camt030

`assignment_id`, `assignment_created`, `assigner`, `assignee`, `case_id`,
`case_creator`, `notification_id`, `notification_from`, `notification_to`,
`notification_created`, `justification`, `source_file`

**Two party pairs, and they need not agree.** `notification_from`/`_to` are
who is being told; `assigner`/`assignee` are who the case is now with. In the
corpus sample the notification goes to EEEEUS33 while the case is assigned to
FFFFUS33, so one pair of columns would report the wrong bank. `justification`
is a bare code here (`CANC`, `FTHI`, `MINE`), where camt.031 wraps its reason
in `RjctnRsn`.

## read_camt031

`assignment_id`, `assignment_created`, `assigner`, `assignee`, `case_id`,
`case_creator`, `rejection_reason`, `source_file`

The case will not be worked. There is no underlying payment block at all: the
assignment, the case and the reason are the whole message.

## read_camt036

`assignment_id`, `assignment_created`, `assigner`, `assignee`, `case_id`,
`case_creator`, `debit_authorised`, `source_file`

The customer's answer to a camt.037. `debit_authorised` is text, as the wire
spelled it. The schema lets the response restate the amount and value date it
agrees to, but no published sample carries either, so there is no column for
them to be NULL in.

## read_camt037

`assignment_id`, `assignment_created`, `assigner`, `assignee`, `case_id`,
`case_creator`, `original_instr_id`, `original_amount`, `original_currency`,
`original_execution_date`, `original_settlement_date`, `cancellation_reason`,
`amount_to_debit`, `debit_currency`, `source_file`

May the bank take this back off the account? `amount_to_debit` is what is being
asked for and is **not** the original: a bank that kept its charges asks for
less than it paid out, so both amounts are columns with their own currencies.

## read_camt087

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

## read_pacs028

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
[`docs/adr/0007-a-request-with-no-transactions-is-a-row.md`](adr/0007-a-request-with-no-transactions-is-a-row.md).

## Not changed in 1.9.0

`read_iso20022.counterparty_name` keeps its current resolution: the party on the
other side of the sole transaction's money, with the other party answering when
the correct side states none. It is NULL when the entry holds more than one
transaction, as before. That fallback is deliberate and stays, because
[ADR 0003](adr/0003-no-xsd-validation.md) records in its context the entries
that carry only `<Cdtr>` where a reader demanded `<Dbtr>`; narrowing it would
blank a name in files that read correctly. `read_camt_transactions` is where the
two sides are separate columns, with nothing resolved for you.
