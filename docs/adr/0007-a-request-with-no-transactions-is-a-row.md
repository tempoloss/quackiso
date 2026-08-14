# 7. A pacs.028 that names no transaction is one GROUP row

Status: accepted

Decided with `read_pacs028`, the reader that added the pacs.028 family, and
written down before the reader was released so the grain is arguable from the
outside rather than only from the event loop.

## Context

pacs.028 is pacs.002 with the answer removed. A payment status request carries no
status and no reason: only the references that identify the original payment and,
when the sender includes one, a carried copy of it. It asks at two grains, and
the message schema makes both optional:

```text
FIToFIPmtStsReq
  GrpHdr                  - who is asking whom
  OrgnlGrpInf             - the whole original message   (0..1)
  TxInf                   - one original transaction     (0..n)
```

"Where is batch BATCH-DOM-01?" is a message-level `OrgnlGrpInf` and no `TxInf` at
all. A reader whose grain is the transaction parses that file to zero rows, and
zero rows with no error is indistinguishable from a bank that asked nothing. The
project already had this problem three times -- camt.056 batch cancellations,
camt.029 message-level resolutions, pacs.002 group-level acknowledgements -- and
solved it each time by emitting the statement the message actually made.

The question this ADR settles is the other half: what to do when the message
makes both statements, a message-level reference *and* transactions.

## Decision

One `TRANSACTION` row per `TxInf`. One `GROUP` row at container close, and only
when that container produced no transaction row.

The transaction rows already carry the message-level reference: `original_msg_id`
and `original_msg_name_id` fall back to it when the transaction states none. A
GROUP row beside them would repeat those two values and leave the other twelve
columns NULL -- a row that answers no question a transaction row does not already
answer, and one that every `count(*)` and every join then has to filter out.

The column that names the grain is `scope`, as in `read_camt056` and
`read_camt029`, not `status_level` as in `read_pacs002`. A request has no status;
naming a column after one invites `WHERE status_level = 'ACCP'` against a family
that can never answer it.

Two consequences of the "only when no transactions" rule are load-bearing in the
reader. The flag is per container, not per file, because one `Document` may hold
several complete requests: left latched by a request that detailed transactions,
the next request's batch question comes back as no row. And the EOF identity
guard is on `saw_request`, not on "a row was emitted", because a request that
emits its group row and a request that emits transaction rows are both valid and
neither can stand in for the file-level judgement. `testdata/pacs028_mixed_grains.xml`
is the file that fails if either is wrong, and
`testdata/pacs028_envelope_two_messages.xml` is the file that fails if the scope
is a latch: a camt.056 in the same envelope names its transaction `TxInf` too,
and an unscoped flag reads the cancellation as a second status request.

## Alternatives rejected

**Always emit a GROUP row when `OrgnlGrpInf` is present.** The pacs.002 shape,
where `OrgnlGrpInfAndSts` carries a status of its own and therefore says something
no transaction row repeats. Here it carries only ids, and those ids are already on
every transaction row. Rejected as a near-empty duplicate: `SELECT count(*)`
over a request for three transactions would answer four.

**Never emit a GROUP row; require a transaction.** Simplest event loop, and it
loses the message. It is the bug this project keeps naming, and pacs.028 is the
family where it is most likely: a request about a whole batch is exactly what a
bank sends when it has nothing more specific to ask.

**Emit the GROUP row at `</OrgnlGrpInf>` rather than at container close.** Cannot
work: whether the message details transactions is not known until the container
closes, and the group block comes first.

**A `has_transactions` boolean column instead of `scope`.** Rejected as the same
information in a shape that cannot grow: camt.055 already needs three values
(`GROUP`, `PAYMENT_INFO`, `TRANSACTION`), and a boolean would make pacs.028 the
one exception reader that spells its grain differently.

## Consequences

A `GROUP` row is a row about a message, so its transaction columns are NULL, and
`status_request_id` is NULL with them -- the id belongs to a `TxInf`. Aggregates
over a mixed file must say which grain they mean, exactly as they must for
camt.056 and pacs.002.

`original_amount` and `original_currency` are populated only from a carried copy.
A request that names its payment by reference alone -- legal, and the least a
request can carry -- has no amount anywhere in the message, so those columns are
NULL rather than zero.

Agents are read from the group header only. Standard pacs.028 has no
`InstgAgt`/`InstdAgt` inside `TxInf`, unlike pacs.002 where a transaction may
override the pair. If a file turns up that restates them per transaction, the fix
is the two `Option<Agent>` fields and the `or_else` fallback `pacs002::row_from_tx`
already has; it is not written now because no file demonstrates it.

The fixtures are hand-written. The corpus has no bank pacs.028 -- the same
position camt.052 is in -- so the shapes are taken from the neighbouring families
that do have real files: the member-id agent identification the SIX camt.056
files use in place of a BIC, the `Amt/InstdAmt` pricing of the pain side, the
enveloped no-`Document` framing of the issettled and montran RTGS traffic, and
the corpus's own `PACS8-PFX-0001` payment, so a request joins the payment it
asks about.
