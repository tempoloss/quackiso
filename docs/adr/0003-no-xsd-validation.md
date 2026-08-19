# 3. No XSD validation

Status: accepted

## Context

An early roadmap listed "XSD validation" as a feature. It is worth saying plainly
why it was dropped rather than leaving it dangling.

Tooling exists. On crates.io today there is `libxml` 0.3.16 (a libxml2 binding,
~1.9M downloads), and pure-Rust efforts such as `uppsala` 0.9 and `xsd-schema`
0.1.3. So this is not "impossible in Rust". It is a choice.

## Decision

quackiso does not validate. It reads.

Three reasons, in order of weight.

**1. Every bug the real corpus found was a tolerance bug, not a validity bug.**
Across the 45 messages from nine sources audited then, the defects fixed were:
namespace prefixes (`<Doc:CdtTrfTxInf>`, `<urn2:...>`) that the reader mishandled
while copying subtrees; entries carrying only `<Cdtr>` where the code demanded
`<Dbtr>`; party names nested under `Pty/Nm` in the .08 schemas; accounts under
`Othr/Id` instead of `IBAN`; settlement dates on the group header rather than the
transaction; execution dates wrapped in `DtTm` rather than `Dt`. In every case the
data was present and readable and the reader was too strict. A validating reader
optimises for the opposite behaviour — refusing input — which is the wrong
direction for this tool.

**2. Validation needs the right schema per message, and there are many.** The
test corpus alone spans camt.027, camt.028, camt.029, camt.030, camt.031,
camt.036, camt.037, camt.052, camt.053, camt.054, camt.055, camt.056,
camt.087, pacs.002, pacs.003, pacs.004, pacs.007, pacs.008, pacs.009,
pacs.028, pain.001, pain.002 and pain.008 to pain.012, some of them inside a
head.001 envelope - forty-two ISO 20022 message namespaces across twenty-seven
families. Bundling ISO 20022 XSDs raises a distribution
question and adds megabytes; requiring users to supply paths adds surface for a
job they can already do. `libxml` would also add a C dependency to an extension
that must build across Linux, macOS, Windows and WASM.

**3. It is a different job, already well served.** `xmllint --schema`, Prowide,
and every bank gateway validate ISO 20022. Nothing is gained by doing it again
inside a SQL reader, and users who need it can validate before ingest.

## What is guaranteed instead

Malformed input is not silently tolerated where tolerance would corrupt a result:

* an amount that cannot be represented exactly is an **error**, not a NULL — a
  NULL amount disappears from a `SUM` and returns a plausible wrong total
  (`testdata/camt053_bad_amount.xml` locks this);
* an ill-formed document fails the scan with DuckDB reporting the parse error;
* a missing optional element becomes SQL NULL, which is what it means.

That is the useful half of validation — the half that protects an answer — without
rejecting statements banks actually send.
