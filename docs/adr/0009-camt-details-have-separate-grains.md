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
