# 8. The address audit is its own function, with its own grain

Status: accepted

Decided with `audit_addresses`, and written before the function was released
because the grain and the fact/verdict split are the arguable parts, not the
element names.

## Context

On 14 November 2026 CBPR+ stops accepting a fully unstructured postal address.
Town and country must sit in a `<TwnNm>` and a `<Ctry>` of their own; the rest is
either dedicated elements throughout, or at most two `<AdrLine>` of at most 70
characters beside them. There is no grace period.

That is a data question and not a messaging one. The message a bank sends after
the deadline is the message it sends now with the addresses enriched, and the
work is finding out which of the addresses it holds are not enriched yet. A
practitioner on r/fintech had already written that measurement by hand: a method
over translated pacs messages "to generate statistics on how many still contain
unstructured addresses ... the goal is simply to measure the current situation
ahead of the November deadline and identify where action is still needed."

quackiso could not answer it. No reader parsed `PstlAdr` at all: a party reaches
a row as `debtor_name` and the address was not on the row in any form. On the MT
side it was worse -- `mt::party` kept the name line of a `:50K:` and dropped the
address lines under it, so the address was not merely off the row, it was thrown
away during parsing.

## Decision

A separate table function, `audit_addresses`, at one row per party occurrence.

The grain is the reason it is separate. pacs.008 alone carries five parties and
six agents that may hold an address; at four columns each that is forty columns
added to a transaction row to answer a question nobody asks per transaction, in
every one of the thirty-three readers. And the question is asked across
families -- "of everything in this folder, which parties break" -- which a
per-family reader cannot answer at all. `sniff_iso20022` already established
that a cross-cutting inspector with its own grain belongs beside the readers
rather than inside them.

`address_format` is off the wire and takes four values:

```text
NONE          no address elements at all
STRUCTURED    dedicated elements, no AdrLine
HYBRID        AdrLine beside a TwnNm and a Ctry of their own
UNSTRUCTURED  AdrLine without both of them
```

`finding` is the rule applied to those facts, and NULL means nothing in this
party would be refused. Keeping the two apart is what makes the function useful
after this deadline passes: the facts stay true, and a rule that moves is one
column.

Scope is decided by family, because the mandate excludes the cash-management and
administration messages (camt.052, camt.053, camt.054, camt.060, camt.025,
admi.024). Their parties are still rows, with their format reported, and never
with a finding.

## Alternatives rejected

**Address columns on every reader.** Forty columns per reader for a question
asked per folder, and thirty-three places to fix when a spelling is missed.
Rejected on grain before size: a transaction row cannot answer "how many
parties", because a party may appear once for a whole payment group.

**A boolean `cbpr_compliant` column.** Rejected because a boolean cannot say
why, and the why is the whole deliverable -- a team fixing 40,000 addresses
needs to know whether it is a missing `TwnNm`, a third address line or an
over-long one, since those are three different repairs. `finding IS NULL` gives
the boolean back to anyone who wants it.

**Reporting only the parties that carry an address.** Rejected: a BIC-only agent
is the compliant case and has to be visible as such, otherwise a count of
compliant parties silently excludes the ones that needed nothing.

**A verdict for a party with no address.** Whether an address was required
there is a usage-guideline question this cannot see. A `NONE` row with a NULL
finding says what is on the wire and nothing more; inventing a verdict would
bury the rows that genuinely break under rows that merely might.

**Auditing only ISO 20022.** Rejected while writing this: it makes the answer
useless to the bank that most needs it. MT is where the unstructured address
comes from. A `:50K:` is a name and then free-text lines, a `:50H:` and a
letterless `:59:` the same, and this repository's own foreign corpus holds 149 of
them and not one `:50F:` -- so a bank auditing its traffic on the eve of the
deadline would have had every message the mandate is aimed at excluded from the
report.

The argument for excluding it was that every MT party would come out
UNSTRUCTURED and the column would carry no information. That is wrong twice.
`:50F:` numbers its subfields and `3/BE/BRUSSELS` states a country and a town
where something other than a human can find them, so MT has both shapes and the
audit tells them apart. And the count is the information: which payments, from
which correspondents, in which of a folder's files.

So the function takes a guard of its own -- XML or MT, refusing only bytes that
are neither -- and one classifier grades both. The mapping is that a free-text
line and a `2/` subfield are both an `<AdrLine>`, because both are prose a
translator cannot place, and only `3/` is structured. `record_index` is NULL for
MT: MT numbers its transactions differently in every type, `party_path` carries
the field tag instead, and reporting an ordinal the message does not state would
be the kind of invention this file exists to prevent.

## Consequences

A glob fails on the first unreadable file, like every reader here and unlike
`sniff_iso20022`. Auditing a real inbox is therefore two steps, which is the
order this repository already recommends: sniff to find out what is in the
folder, then audit the files that are messages.

`Assgnr`, `Assgne` and `Cretr` are not audited. An assigner is the bank handling
an investigation rather than a party to the payment. The payment parties a
camt.056 copies into `OrgnlTxRef` are spelled `Dbtr` and `Cdtr` and are audited
like any others, which means an audited row may describe the original payment
rather than this message -- `party_path` says which.

`AdrTp` is not counted as a structured element. It states which kind of address
this is and nothing about where the party is, so counting it would report a
structured element that carries no address.

The coverage gate does not reach this function: `scripts/check_column_coverage.py`
judges columns by what `sniff_iso20022` routes, and this is not a reader that
routing names. Its columns are covered by the SQL suite directly, the same
arrangement `sniff_iso20022` has.

Address lines are measured in characters and not bytes, because the 70-character
limit is a character limit; `testdata/pacs008_address_formats.xml` holds a
73-character line, and the unit tests hold a 71-character line of `ä` that is 142
bytes.

Address lines are counted as elements and the structured components as values,
which reads like an inconsistency and is not. The limits of two lines and 70
characters are limits on what is on the wire, so a blank `<AdrLine>` and a
self-closing one are both a line -- counting values instead let a three-line
address with one blank line pass a limit of two. `structured_elements` measures
how much of the address is in dedicated elements, so an empty `<TwnNm/>`
contributes nothing and leaves `town` NULL, which is what raises the finding.

The message, not the file, owns the identity and the numbering. One file may hold
several complete messages -- an envelope with two `<Document>`s, and
`testdata/pacs002_two_reports.xml`, which puts two status reports in one -- so the
message boundary is the family container rather than the wrapper around it, and
`message_id` and `record_index` reset there. The first version of this reader
scoped both to the file and attributed the second report's agents to the first
report, which is the failure ADR 0007 names on the pacs.028 side. `message_id`
is read from `GrpHdr/MsgId` or `Assgnmt/Id` through `sniff::is_message_id`, one
predicate shared with the sniffer: an `OrgnlMsgInf/MsgId` names the message being
answered, and reading that as the message's own id misattributes every row of the
answer.

## Amendment, 2026-08-25: the cheque roles, and why they are graded

`PARTY_ROLES` gained `Pyer` and `Pyee`, and `AGENT_ROLES` gained `DrwrAgt`. The
cheque messages -- camt.107 presentment, camt.108 stop request, camt.109 stop
report -- name nobody `Dbtr` or `Cdtr`, so before these three the audit read a
cheque presentment as zero parties. Zero parties is what a clean file looks
like, which makes it the worst possible answer: a bank with ten thousand cheque
notifications on disk saw nothing to migrate.

Ten generated CBPR+ cheque files in the foreign corpus state `Pyer`, `Pyee` and
`DrwrAgt` and nothing else. Roles are added when the pinned corpus states them,
which is why `Drwr`, `Drwee`, `Endrsee` and `ChqDpstr` are not here: the schemas
allow them, no sample sends them, and a role no fixture populates is a role the
coverage gate cannot judge -- ADR 0006 again.

**The three cheque families are inside the mandate and their parties are
graded.** `OUT_OF_SCOPE` lists the cash-management reports and the
administration messages -- admi.024, camt.025, camt.052, camt.053, camt.054,
camt.060 -- because the CBPR+ address rule is about payment instructions and
those messages report rather than instruct. A cheque presentment instructs: it
moves money and it names the two ends of the movement, so an unstructured `Pyer`
is exactly the shape the rule refuses. Leaving camt.107 out of `OUT_OF_SCOPE`
would have been that decision made by omission, so it is stated here instead.

The identity these rows carry comes from a tier the sniffer gained at the same
time. A CBPR+ cheque notification declares no namespace at all and states what
it is only in `AppHdr/MsgDefIdr`, so `AddressStream` keeps that family as a
pending fallback, ranked below a message namespace and above the container name.
A header alone does not make a file a message: it does not set `identified`, so a
header with no payload under it is still refused. `Rcpt` is now a mapped
container as well, and it is a generic enough name that a mapped container opens
a message only while no message scope is active -- otherwise a `<Rcpt>` nested
inside a pacs.008 would restart the numbering and take the identity with it.
