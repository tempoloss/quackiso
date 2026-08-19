# 4. A message's container is its scope, not a latch

Status: accepted

Decided in `be5485a` (2026-08-07), the change that scoped the container flag in
all twelve payment and status readers.

## Context

ISO 20022 reuses transaction element names across families. camt.056 calls its
transaction `TxInf` exactly as pacs.004 does; pacs.002 and pain.002 both say
`TxInfAndSts`; pacs.008, pacs.009 and pain.001 all say `CdtTrfTxInf`. What
identifies a message is therefore its own container -- `PmtRtr`, `FIToFIPmtCxlReq`,
`CstmrCdtTrfInitn` -- and every payment and status reader gates on having seen it.

Until now that gate was a `bool` latched at the first matching start element and
never cleared. Two things followed from the latch, and both were wrong.

An envelope carrying a pacs.004 and then a camt.056 made `read_pacs004` emit rows
for the cancellations. Both name their transaction `TxInf`, the latch was still
true from the pacs.004, and the camt.056 transactions deserialized happily into a
return row with every return-specific column NULL. That is precisely the failure
the container gate exists to prevent, and the README describes it as prevented.

A Document holding two messages of one family filed the second message's
transactions under the first message's `MsgId`, settlement date and group reason.
pacs.004 was worse than stale: `reason_code` is first-write-wins and `reason_info`
is a `Vec` that is only ever pushed to, so message 2 carried message 1's code
beside the concatenation of every group reason text in the file.

## Decision

Split the single flag in two.

`saw_*: bool` stays latched for the whole file and is read by exactly one place,
the EOF check: having seen the container anywhere is what distinguishes a pacs.004
from a file of another type, and that judgement is about the file.

`in_*: Option<usize>` holds `path.len()` at the container's start and is cleared
when the path returns to that depth. Every gate that decides whether an element
becomes a row tests this one. Every `path.pop()` routes through a `pop` method
that maintains it.

Opening a container also clears the carried group context, so a second message
starts from nothing rather than from its predecessor's header.

`src/stream.rs` is deliberately unchanged. camt.05x has no container gate to
scope -- `<Ntry>` becomes a row wherever it appears -- and it already resets the
statement context at each `<Stmt>`. Adding a scoped flag there would introduce a
gate that does not exist today.

## Alternatives rejected

**Keep the latch and compare the container name at each transaction.** The reader
would have to remember which container it was inside, which is the same state in
a worse shape, and it says nothing about a second message of the *same* family.

**A stack of depths (`Vec<usize>`).** Correct for a container nested inside
another of its own family, and rejected because that nesting is not a thing the
corpus contains, while the one place it plausibly occurs wants the opposite
behaviour. camt.029 files spell the container both as `RsltnOfInvstgtn` and as the
versioned `camt.029.001.xx`, and a file nesting one inside the other emitted two
identical RESOLUTION rows. With one slot the inner close ends the scope and
exactly one row is produced, which is the intended grain.

The cost is recorded rather than hidden: a container that genuinely repeats inside
itself -- a `<PmtRtr>` buried in `SplmtryData/Envlp`, say -- ends the outer scope
early and the outer message loses the transactions that follow. No file in the
corpus does this. If one turns up, the fix is the stack, and camt.029 then needs
its own guard.

## Consequences

Output changes for two shapes of input that previously produced wrong rows: a
multi-family envelope stops yielding foreign transactions, and a multi-message
Document files each message under its own header. Both are more rows being
correct, not fewer rows.

`test/sql/quackiso.test` already asserted the pacs.002 multi-message case; it now
holds for every family by construction rather than because pacs.002 was the one
reader written for it. The nesting limit is stated on `RtrStream::in_return` in
`src/pacs004.rs`, which is the worked example the other thirty-one follow.
