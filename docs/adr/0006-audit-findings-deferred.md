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

Ten were not. They divide cleanly, and the division is what this document
records, because "we did not notice" and "we decided not to" look identical in a
diff six months later.

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

Each widens a published schema. Adding a column is cheap; removing one is a
breaking change, and every one of these should be argued on its own merits with
a fixture in hand rather than swept in behind a bug-fix release.

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
wrong-row fix and five schema widenings are indistinguishable, and reviewing it
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
