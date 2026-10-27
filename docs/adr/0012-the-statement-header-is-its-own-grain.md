# 12. The statement header is its own grain

Status: accepted

Decided with the 1.9.0 release, with the change that added
`read_camt_statements`.

## Context

`read_mt940` returns `statement_number` and `sequence_number`, so a query can ask
whether the statement for Tuesday arrived, whether two files carry the same one,
and whether the opening balance of today's statement is the closing balance of
yesterday's. camt had no equivalent. The five camt functions are at the grain of
something inside a statement - an entry, a transaction, a balance, an amount
block, a remittance leaf - and what a statement says about itself had no row to
live on: the electronic and legal sequence numbers, the period it covers, whether
it is a copy, which page of how many, and the totals the bank states for it
(`TxsSummry`).

Those fields are not decoration. CBPR+ and the Deutsche Kreditwirtschaft
statements carry `ElctrncSeqNb` and `StmtPgntn` so that a receiver can find a gap
or a duplicate, and the comparable `camt053` crate keys its de-duplication on
`(MsgId, StmtId, ElctrncSeqNb)`. Without them a reconciliation can show that a
statement reconciles and cannot show that it is the statement that was expected.

The count the bank states is the second half of it. A statement that says it holds
twelve entries and delivers ten reconciles perfectly against its own balances
only if the two missing entries net to nothing. The two numbers are only a
comparison in SQL if both are columns of one row.

## Decision

**`read_camt_statements(path)` returns one row per `Rpt`, `Stmt` or `Ntfctn`.**
The container rule is the shared walk's (ADR 0009): any of the three, wherever it
sits, numbered across the file by `statement_index`, the same index every other
camt function and `read_iso20022` carry. A file with no statement at all raises the
shared sentence, `no <Stmt>, <Ntfctn> or <Rpt> found`, and is not an empty table.

The row is emitted when the container closes. Everything it needs is known by
then, and two of its columns can be known no earlier: `entry_count` and
`balance_count` are what the walk counted on the way past, in the sense every
other grain uses.

- `entry_count` is the direct `<Ntry>` children that open, which is the number of
  rows `read_iso20022` returns with that `statement_index`. A self-closing
  `<Ntry/>` is no entry there (ADR 0009 amendment, 2026-10-27) and is none here.
- `balance_count` is the direct `<Bal>` children, self-closing or not, which is
  the number of rows `read_camt_balances` returns for it. A `<Bal/>` is a
  balance (ADR 0009).

The entries and balances themselves are consumed whole by `wire::skip_subtree`
and are never copied out, so this reader walks past three million entries in the
same constant space as `read_camt_balances`.

The columns, in order:

- the message: `msg_id`, `message_created`, `message_page`,
  `message_last_page`, from `GrpHdr/MsgId`, `GrpHdr/CreDtTm` and
  `GrpHdr/MsgPgntn/PgNb|LastPgInd`;
- the statement: `statement_kind`, `statement_index`, `statement_id`,
  `electronic_sequence`, `legal_sequence`, `statement_created`, `from_date`,
  `to_date`, `copy_duplicate`, `page_number`, `last_page`, from the elements of
  the same names directly under the container and `FrToDt/FrDtTm|ToDtTm`,
  `StmtPgntn/PgNb|LastPgInd`;
- the account: `account_iban`, `account_currency`, `account_owner_name`,
  `account_servicer_bic`, from `Acct`, and from `Acct/Svcr/FinInstnId/BICFI` or
  the pre-2019 `BIC`;
- what the bank states: `summary_entries`, `summary_sum`,
  `summary_net_amount`, `summary_net_credit_debit`, `summary_credit_entries`,
  `summary_credit_sum`, `summary_debit_entries`, `summary_debit_sum`, from
  `TxsSummry/TtlNtries`, `TtlCdtNtries` and `TtlDbtNtries`;
- what was counted: `entry_count`, `balance_count`, and `source_file`.

Each column reads exactly the element it is named for and falls back to nothing
(ADR 0009). A header the statement does not state is NULL. Every statement-level
field is reset when a statement opens, so a statement that omits
`ElctrncSeqNb` is NULL and not the previous statement's number; the message
fields belong to the message and are kept across the statements inside it.

**Types.** The counts and sequence numbers are BIGINT, and a text that is not a
whole number fails the scan: `{file}: <ElctrncSeqNb> "forty" is not a whole
number`. That is ADR 0001's rule applied to a count, because a NULL for a number
the file stated is a gap that cannot be told from one the bank never stated. The
sums are DECIMAL(38,5) through the parser every amount goes through, so a
malformed one fails the scan the same way. The date-times are TIMESTAMP, parsed
by the parser `booking_date` uses and normalised to UTC (`docs/types.md`), so
`2026-10-03T06:15:00+02:00` is `2026-10-03 04:15:00`. `message_last_page` and
`last_page` stay VARCHAR as the wire spells them, like `reversal_indicator`: a
bank sends `true`, `false`, `1` or `0`, and reading one of them as a boolean
would invent the other three.

The header text is held as text until a row is built, and parsed there. The shared
walk captures it for every grain, but only this function returns it, so a
malformed `<ElctrncSeqNb>` fails `read_camt_statements` and no other reader: a
balance scan has no use for a number it does not return and must not fail on it.

**The reader has no `threads` rule of its own.** It takes the path argument and
`threads` like every other reader (ADR 0005, ADR 0011).

## Alternatives rejected

**Columns on `read_camt_balances`.** The balance is the nearest existing row that
carries the statement context, and putting the sequence number on it would make the
header one function. It is the wrong grain: a statement has n balances, so the
header would repeat on each, and a statement with no balance would have nowhere
to put it. `entry_count` would be a count of something that is not the row.

**The statement columns repeated on every entry row.** `read_iso20022` is at the
entry grain and the header is not a fact about an entry. Thirty columns repeated on
every one of three million rows is width without grain, which ADR 0009 rejected
for the transaction details, and the entry reader would stop being the cheap
function it is.

**Parsing the header in the shared walk.** It would fail every grain on a header
it does not return.

## Consequences

`read_camt_statements` joins to the other five on `(source_file, statement_index)`,
the same scope columns (ADR 0004), and `docs/recipes.md` uses that for the checks
the grain exists for: whether the opening balance of a statement is the closing
balance of the previous one for the same `account_iban` ordered by
`electronic_sequence`, and whether the same `(account_iban, statement_id,
electronic_sequence)` arrived in two files.

`entry_count` beside `summary_entries` makes a short statement visible in SQL. It
is a comparison and not a verdict: a bank that omits `TxsSummry` leaves
`summary_entries` NULL, and a difference between the two is a question to ask of
the file and not proof that it is damaged.

`StatementContext` grew by twenty-three fields and the walk by one record type
(`StatementClose`), selected by a constant on the record trait, so the entry,
transaction, balance, amount-detail and remittance grains run the code they always
did. The corpus sweep runs the new reader beside the primary one for every
camt.052, camt.053 and camt.054 (R9), and the column-coverage check measures
every column of it over the files routed to `read_iso20022`.

Only `StmtPgntn` is read for a statement-level page, as the schema names it for
camt.053. camt.052 and camt.054 name theirs `RptPgntn` and `NtfctnPgntn`, which
are different elements and, by the rule above, different columns if they are ever
wanted.
