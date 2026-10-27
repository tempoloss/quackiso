# 9. A statement's details get their own grains, not wider entry rows

Status: accepted

Decided on 2026-08-25, with the change that added `read_camt_transactions`,
`read_camt_balances`, `read_camt_amount_details` and `read_camt_remittance`.

## Context

`read_iso20022` is one row per `<Ntry>`, which is the grain a bank statement is
reconciled at. It is not the grain the statement is written at.

A batch posts as one entry. `testdata/camt053_batched_entry.xml` is the shape:
900 CHF out, three `<NtryDtls>` of one `<TxDtls>` each, three end-to-end ids,
three counterparties, three creditor accounts, six remittance leaves. The entry
row has one column for each of those, and it filled them from
`ntry_dtls.first().tx_dtls.first()`. Three payments to three parties were
reported as one payment to the first of them, with no column saying so.

Three more things on the wire had nowhere to go at all:

* `<Bal>`. `testdata/camt053_empty_statement.xml` states two balances and no
  entries. `read_iso20022` truthfully returned zero rows for it, and nothing else
  returned the balances, so an account's closing position was unreachable from
  SQL while the file stated it plainly.
* `<AmtDtls>`. A cross-currency entry states the instructed amount, the settled
  amount, the counter value, and the rate and contract between them. One `amount`
  column showed one of those.
* repeated `RmtInf` leaves. Two invoice numbers in two `<Ustrd>` were joined by a
  space, and a transaction carrying free text beside a structured creditor
  reference reported only the free text.

## Decision

Four functions, one per grain, sharing one walk.

* `read_camt_transactions` -- one row per `Ntry/NtryDtls/TxDtls`.
* `read_camt_balances` -- one row per `<Bal>` directly under a camt.052 `Rpt` or
  a camt.053 `Stmt`.
* `read_camt_amount_details` -- one row per amount block inside an entry-level or
  transaction-level `<AmtDtls>`.
* `read_camt_remittance` -- one row per non-empty supported remittance text leaf.

`src/camt.rs` owns everything above the record: which files are one of the three
messages, where the per-account containers are, which statement a record belongs
to, and what that statement says about itself. Five copies of that is how four of
them come to disagree about which `<Acct>` an entry was under.

### No synthetic rows

An entry with no `<TxDtls>` produces no transaction rows. A `<Btch>` stating
`NbOfTxs` of 5 with no transaction under it produces none either: five rows with
every transaction column NULL would be five claims nobody made.
`read_iso20022.transaction_count` is 0 for that entry, which is the fact.

A statement with no records of the kind a reader is after returns nothing and is
not an error. A file that is not a camt.052/.053/.054 at all raises, with the
same sentence `read_iso20022` raises, because the four share its walk.

### No hidden fallback

Nothing in a supplementary row falls back to anything else. `debtor_name` is
`RltdPties/Dbtr`, not a counterparty resolved across both sides. `amount` is
`TxDtls/Amt`, not the entry's. `bank_transaction_domain` is the transaction's
`BkTxCd`, not the entry's, and a proprietary code is never mapped onto a domain
code. The entry's own values are repeated under `entry_*`, so a query can compare
the two instead of being handed one where it asked for the other.

That is the whole difference between these functions and the convenience columns
on the entry row, and it is why both exist. The entry row answers "what is this
entry" for the common case of one payment; these answer "what does the message
say", exactly.

### The entry row's convenience columns became conditional

`end_to_end_id`, `counterparty_name` and `counterparty_iban` are populated only
when `transaction_count = 1`. `remittance_info` also needs `remittance_count = 1`.
`transaction_count` and `remittance_count` are new columns and are never NULL:
every entry has an exact number of each, zero included.

This is an observable change and not a cosmetic one. A sole transaction carrying
both free text and a structured reference now returns NULL in `remittance_info`
where it used to return the free text alone. That value was one of two answers
with nothing saying which, and `read_camt_remittance` has both.

## Alternatives rejected

**Widen the entry row.** Forty columns of first-transaction detail on a row whose
grain is the entry, and a batch of three still gets one set of them. The grain is
the problem; width does not touch it.

**One function with a `grain` argument.** A table function's schema is fixed at
bind time, so this is four schemas behind one name, and every query has to know
which columns exist for which argument. DuckDB has no way to say that, and a
caller reading a `NULL` cannot tell "absent from this grain" from "absent from
this message".

**A nested type -- a `LIST` of structs per entry.** It keeps one row per entry
and makes every question about a transaction an `UNNEST`, which is the join this
avoids written per query instead of once. It also puts every transaction of an
entry in memory at once, which is the bound `src/membound.rs` exists to hold.

**Emit a synthetic transaction row for a batch-only `NtryDtls`.** Tempting,
because `Btch/NbOfTxs` and `Btch/TtlAmt` are real facts with no row to sit on. It
was rejected because those facts describe the batch and not a transaction, and a
row that exists to carry them would be indistinguishable in SQL from a
transaction the bank actually sent. The batch columns are context on real
transaction rows; a batch with no real transaction is visible as
`transaction_count = 0`.

## Consequences

Five functions read camt.052/.053/.054, joined on `source_file`,
`statement_index`, `entry_index` and, below that, `entry_details_index` and
`transaction_index`. ADR 0004's amendment states which entries are joinable: the
three scope columns on the entry row are NULL exactly for the entries the strict
walk does not emit.

`model::Row` grew eleven columns, which moved the first term of the memory bound:
`STEADY_HEAP` went from 1,260 KiB to 1,826 KiB, measured. The second term did not
move. The four cursors are two integers each and `wire::skip_subtree` consumes the
record a scan is not interested in, so `read_camt_balances` walks past three
million entries in constant space rather than copying each one out to drop it.

The corpus sweep runs all four beside the primary reader on every camt.052,
camt.053 and camt.054, because routing cannot reach them. R9 makes a raise from
one of them where the primary succeeded a finding: they walk the same bytes, so a
disagreement is a defect in whichever is wrong. R10 makes a camt.052 or camt.053
with neither entries nor balances a finding, which is a thing that could not be
stated until there was a balance grain to state it with.

Other structured remittance objects -- `RfrdDocInf`, `RfrdDocAmt`, `TaxRmt`,
`GrnshmtRmt` -- are not covered, and are not described as covered. What
`read_camt_remittance` exposes is the textual leaves that were being lost or
collapsed: `Ustrd`, `CdtrRefInf/Ref` and `AddtlRmtInf`.

## Amendment, 2026-08-25: the container rule is the entry reader's

Review of the first implementation found the walk gated on the resolved message
family: a container opened only when its name was the one that family states,
and only after something had identified the message. `read_iso20022` has neither
gate -- any `Stmt`, `Ntfctn` or `Rpt` not already inside one opens a container
there. Three shapes came out of the disagreement, and the third is the reason
this is an amendment and not a note:

- `<Document><NtlStmtFile><Stmt>` with no namespace and no `AppHdr`. The entry
  reader scoped the entry; all four raised `no <Stmt>, <Ntfctn> or <Rpt> found`
  at a file whose `<Stmt>` is right there. The sentence was false.
- A camt.053 that states `<Rpt>`. The entry reader reported `statement_kind =
  'Rpt'`; the four walked past it. Nonconformant, and still a row on one side
  only.
- `<Stmt>A</Stmt><Rpt>B</Rpt><Stmt>C</Stmt>` under one message. The entry
  reader numbered A=1, B=2, C=3; the four numbered A=1, C=2. A join on
  `(source_file, statement_index, entry_index)` then hung statement C's
  transactions, amounts and remittance off statement B's entry rows, with no
  error anywhere. The join key is the contract these functions exist to serve,
  so this one is not a tolerance question.

The family is context, not permission. `camt::is_statement` is now the whole
container rule, matching `stream::is_container`, and the wrong-file refusal moved
to the only place it is still true: a file that holds no container at all. A
pain.001 still raises the sentence `read_iso20022` raises.

Two identity fixes came with it. `GrpHdr/MsgId` is read on every occurrence
rather than once per `<Document>`, because one Document can hold two complete
messages and the second header names the rows after it -- the shape
`testdata/pacs002_two_reports.xml` pins for the address audit. And the namespace
an envelope declares is no longer taken from the element that opens the message,
which was handing a `<Document>`'s own family to the next namespace-free
`<Document>` in the file and dropping its statements silently.

A self-closing `<Bal/>` is a balance row with every fact NULL, the same as
`<Bal></Bal>`. It used to produce nothing and consume no index, which reported
the third balance of a statement as its second -- an index that depended on how
the writer closed its tags. `read_camt_amount_details` already emitted a row for
`<TxAmt/>` on the same reasoning.

## Amendment, 2026-10-27: a self-closing `<Ntry/>` is no entry

The self-closing rule above is the balance grain's, and it does not carry over
to entries. `read_iso20022` counts an entry when the element opens, so
`<Ntry/>` is no row there and takes no `entry_index`. The four entry-grain
readers numbered it anyway, which shifted every entry after it by one: on
`testdata/camt053_self_closing_entry.xml` the join the ADR 0004 scope columns
exist for paired `NTRY-0002` on the entry row with `NTRY-0001`'s transaction,
with no error anywhere. The join key is the contract, so the entry reader is
the one that decides.

`camt::StatementRecord::EMPTY_IS_RECORD` states the rule per grain: `true` for
`Balance`, whose `<Bal/>` is a row with every fact NULL, and `false` for `Ntry`,
which the transactions, amount details and remittance readers share. A
self-closing `<Ntry/>` states no amount, no status and no transaction, so
dropping it loses nothing a row could have said. `<Ntry></Ntry>` is an element
that opens, and both sides still number it.

## Amendment, 2026-10-27: entry and transaction text are columns, and the invoice number is a slot

Status: accepted, and it supersedes the Consequences paragraph above that lists
`RfrdDocInf` among the objects not covered, for `RfrdDocInf/Nb` only.

Context: a camt.053 states more than the readers exposed. A bank's free text about
the entry (`AddtlNtryInf`) and about the transaction (`AddtlTxInf`), the local
instrument, the purpose, the return reason and the charges total were all on the
wire and none was reachable. The invoice number, `Strd/RfrdDocInf/Nb`, is what
cash application matches a payment on, and it was the one structured remittance
fact a corporate statement carries in place of free text.

Decision, with the rule that decides each case: where a fact sits in the schema
is where it is read, and a repeated one gets a row only if it already has a row
to sit on.

- `AddtlNtryInf` and `AddtlTxInf` are columns, `read_iso20022.additional_entry_info`
  and `read_camt_transactions.additional_transaction_info`. Each is 0..1 on the
  record that owns it. A remittance slot for the entry-level text would need a
  remittance row with no transaction under it, which is a synthetic row and
  breaks the rule above.
- `read_camt_transactions` gains `local_instrument_code` and
  `local_instrument_proprietary` (`LclInstrm/Cd`, `LclInstrm/Prtry`),
  `purpose_code` and `purpose_proprietary` (`Purp/Cd`, `Purp/Prtry`),
  `return_reason_code` and `return_reason_proprietary` (`RtrInf/Rsn/Cd`,
  `RtrInf/Rsn/Prtry`). Each reads exactly its element: a proprietary value is not
  copied into the code column, and a code is not copied into the proprietary one.
- `charges_amount` and `charges_currency` read `Chrgs/TtlChrgsAndTaxAmt`. camt.053.001.02
  repeats `Chrgs`, so the columns are filled only when exactly one total is
  stated across the transaction and are NULL when there are two or more. A sum
  would be a figure the bank never wrote, and picking the first would hide the
  second. Every stated total is read through `model::money`, so a malformed one
  fails the scan under ADR 0001 whether or not it was the only one.
- `read_camt_remittance` gains the slot `REFERRED_DOCUMENT`, the text of
  `Strd/RfrdDocInf/Nb`. `RfrdDocInf` is 0..n, so each number is a row. Inside a
  `<Strd>` the slots come in schema order: invoice numbers, then the creditor
  reference, then the additional texts. `structured_index` is the owning `<Strd>`
  ordinal as before. `RfrdDocAmt`, `TaxRmt` and the other structured remittance
  objects stay out: they are amounts and tax facts and not a text a payment is
  matched on, and what is not described as covered is not covered.

Alternatives rejected: remittance slots for `AddtlNtryInf` and `AddtlTxInf`,
rejected above for the entry and, for the transaction, because the text is 0..1
and a join key (`remittance_index`) for one value would make a query that wants
it write a `WHERE slot = ...` to read what a column answers directly.

Consequences: `remittance_count` counts the new leaf, because it is the count of
leaves this ADR defined and an invoice number is one. An entry stating free text
and an invoice number now has `remittance_count = 2` and a NULL `remittance_info`,
where it used to report 1 and the free text. That is the existing rule applied to
a fact it did not see before, and it is an observable change for a query that
read `remittance_info` on such an entry: `read_camt_remittance` has both.
`model::Row` and `TransactionRow` grew one and nine columns, and
`src/membound.rs` still holds at `STEADY_HEAP` of 1,826 KiB with no constant
changed. `testdata/camt053_v14_header.xml` populates the new columns with codes
and `testdata/camt053_legacy_charges.xml` with the proprietary spellings and the
two-total case, so the coverage gate of ADR 0006 judges all of them.
