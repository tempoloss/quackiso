# 6. Audit findings that need a new column are deferred

Status: accepted

Written on 2026-08-08, after the audit fixes in `934bc49`..`003348a` landed and
before anything was said about what they left out.

## Context

An audit of the readers turned up a set of defects. Most were fixed: a reachable
panic in date parsing, a calendar that accepted 31 February, CDATA read as
nothing at group level, a latched container flag, a counterparty assembled from
two different parties, an amount that fit `i128` and not the column, a directory
passed to a file reader, two contract violations in the sniffer.

Thirteen deferrals were not: nine missing columns and four existing-column
fixes. This document records the division, because "we did not notice" and "we
decided not to" look identical in a diff six months later.

## Decision

### Six need a new output column, and a column is a schema decision

* **camt.055 has no `case_id`** although camt.056 and camt.029 both expose one,
  so a customer cancellation cannot be joined to its own resolution by case.
* **pacs.007 has no `original_settlement_date`** although its twin pacs.004 does.
* **pacs.002 drops the group block's `OrgnlNbOfTxs` and `OrgnlCtrlSum`**, so a
  batch-level acknowledgement cannot be reconciled against what was sent.
* **pacs.009 drops a non-COV transaction's `RmtInf`**; only the COV variant's
  underlying remittance is exposed.
* **camt.029 has no `PAYMENT_INFO` scope**, so a resolution answering a camt.055
  payment-group cancellation has nowhere to put that level.

Each missing column widens a published schema. Adding a column is cheap; removing
one is a breaking change, and every one of these should be argued on its own
merits with a fixture in hand rather than swept in behind a bug-fix release.

Three more were added by the mandate and investigation readers and are deferred
on the same terms:

* **camt.055, camt.056 and camt.029 have no `case_creator`** although the seven
  investigation readers expose one, because every published investigation sample
  carries `Case/Cretr` and no cancellation sample does. Retrofitting it would
  widen three published schemas on the strength of a fixture that does not exist.
* **camt.036 exposes no `amount_to_debit` or `value_date_to_debit`, and camt.037
  no `value_date_to_debit`**, though `DebitAuthorisation` allows all three. No
  published sample of either message carries them, and a column no fixture
  populates is a column the coverage gate cannot judge.
* **read_pain009 has no instructing or instructed agent columns** although
  pain.010 to pain.012 have them: none of the three published pain.009 business
  examples states `InstgAgt` or `InstdAgt`.

### Four fill existing columns and were left out to keep one change reviewable

* **pacs.007 does not read the pre-2009 `RvslRsn` / `AddtlRvslRsnInf` /
  `RvslOrgtr` spellings**, so `.01` files come back with all three reason columns
  NULL. `wire::ReasonInfo` already carries the equivalent pre-2009 spellings for
  pacs.004 and pacs.002, so this is the same shape of fix.
* **camt.029 and camt.056 never fall back to a message-level `<Case><Id>`** when
  the case id is not on the assignment.
* **pacs.004's `RtrChain` omits `DbtrAgt` and `CdtrAgt`**.
* **`wire::money` cannot tell a present-but-unparseable amount element from an
  absent one**, so an `<Amt>` holding whitespace is indistinguishable from no
  `<Amt>` at all.

These need no schema change and no argument. They were excluded so that the
audit-fix change stayed reviewable, not because they are contentious.

## Alternatives rejected

**Fix everything in one change.** The result is a diff where a panic fix, a
wrong-row fix and six schema widenings are indistinguishable, and reviewing it
honestly means reviewing all of it at once.

**Open issues and link them.** The repository has no issue tracker in use, and a
finding recorded nowhere but a commit message is a finding lost. That is the same
reasoning that produced ADR 0001.

## Consequences

The four value fixes are the obvious next change and need no design work. The six
columns are a release of their own, and each one should arrive with the corpus
file that motivated it: none of the six is currently covered by a fixture, which
is part of why none was fixed blind.

Until then the columns are absent rather than NULL, which is the honest state --
a NULL would say the bank did not send it.

## Amendment, 2026-08-25: five of the six columns landed as four functions

Three of the six deferred columns were about camt.05x, and none of them was a
column: `RmtInf` collapsed to one string, `AmtDtls` unread, `<Bal>` unreachable.
They arrived as `read_camt_transactions`, `read_camt_balances`,
`read_camt_amount_details` and `read_camt_remittance`, because the grain was the
problem rather than the width. A batched entry has three end-to-end ids and one
column to put them in, and no number of columns on the entry row fixes that.

The coverage contract stated above -- a column no fixture populates is a column
the gate cannot judge -- now has to reach functions routing cannot reach.
`sniff_iso20022` names one reader per family, and camt.052, camt.053 and
camt.054 all name `read_iso20022`, so nothing routes to the four new readers at
all. `scripts/check_column_coverage.py` therefore aliases each of them to that
reader's routed files, which is honest because they read those same files at
another grain. `audit_addresses` gets a wider corpus for the same reason: the
routed union plus every message the sniffer identified with no reader behind it,
which is how a camt.107 cheque is measured at all.

Two fixtures carry the new columns. `testdata/camt053_batched_entry.xml` states
every one of them -- both bank-code vocabularies, both balance schemes, all five
amount kinds, all three remittance slots -- and pins the counts 2 / 4 / 5 / 9 / 8
across the five readers. `testdata/envelope_apphdr_camt107.xml` carries the three
cheque party roles. The four expected supplementary errors in the coverage gate
are all `camt053_truncated.xml`, and `camt053_bad_amount.xml` is not among them:
that entry has no transaction, no balance and no amount block, so none of the
four grains reads the amount that makes it fail.
