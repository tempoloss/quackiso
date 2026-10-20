# Tested against real messages

Around 283 real messages from a dozen-plus sources — Goldman Sachs (US, UK, EU,
wire), actualbudget, genkgo, Nivaes, Prowide, OpenBankProject, Mbanq, SIX
interbank, CBPR+, ProgressSoft, prog-nov, salesking, Dolibarr, Handelsbanken,
issettled and others — across camt.053 `.02/.03/.04/.08/.09/.11`, camt.054,
camt.056 `.01/.02/.03/.04/.08/.10`, camt.029 `.01/.03/.04/.08/.11`, pacs.008
`.01/.02/.07/.08/.09`, pacs.004 `.01/.02/.03/.09/.10/.11`, pacs.002
`.02/.03/.04/.06/.10/.11`, pacs.003 `.01/.02/.03/.04/.09`, pacs.009
`.01/.02/.03/.08/.09/.10`, pain.001 `.03/.09/.11`, pain.002
`.02/.03/.04/.05/.09/.10/.11/.12/.13/.14/.15` and pain.008
`.01/.02/.03/.04/.08/.11`, pacs.007 `.01/.02/.03/.10/.11` and camt.055
`.01/.02/.03` plus SEPA variants. The mandate and investigation families are the
ISO published business examples carried by prog-nov and the Nivaes resources:
pain.009 to pain.012 `.04`, camt.027 `.04`, camt.028 `.04`, camt.030 `.04`,
camt.031 `.04`, camt.036 `.03`, camt.037 `.01/.04` and camt.087 `.01`. camt.052
has no bank file in the corpus; its fixture is hand-written against `.08`.
pacs.028 likewise has no bank file, so its fixtures are hand-written too, three
of the eight saying so in their header.

Every fix in this reader came from one of those files:

- **namespace prefixes** — `<Doc:CdtTrfTxInf>`, `<urn2:...>`: tag names are
  normalised while a subtree is copied, which previously produced an ill-formed
  document;
- **one-sided entries** — a `CRDT` entry often carries only `<Cdtr>`; the
  counterparty falls back to the other side when the correct one names nobody
  and states no account. Name and account always come from the *same* side, so
  an unnamed payer whose account is on the wire reads as a NULL name beside
  that account rather than borrowing the name from the other party. An
  `UltmtDbtr`/`UltmtCdtr` stands in for its own side's missing name;
- **`.08` nesting** — party names under `Pty/Nm`, accounts under `Othr/Id`;
- **group-level fields** — SEPA puts `IntrBkSttlmDt` on the group header, and
  pain.001 puts the debtor and `ChrgBr` on `<PmtInf>`;
- **`<ReqdExctnDt><DtTm>`** — later pain.001 versions wrap the date differently;
- **structured remittance** — `Strd/CdtrRefInf/Ref` when there is no `Ustrd`;
- **the return chain is not the payment chain** — pacs.004 states the parties in
  `<RtrChain>`, whose debtor is the party giving the money back, so the original
  sides are read crossed; the SIX interbank sample pair proves the direction;
- **renamed reason blocks** — pacs.004's `RtrRsn`/`AddtlRtrRsnInf`/`RtrOrgtr` and
  pain.002's `StsRsn`/`StsOrgtr` are the older spellings of the same elements;
- **status without a transaction** — a pain.002 can accept or reject a whole
  batch at group level and detail nothing, a camt.056 can cancel a whole batch
  (`GrpCxl`) the same way, and most camt.029 files answer at message level
  only, so the grain is the statement;
- **transaction elements collide across families** — camt.056 calls its
  transaction `TxInf` like pacs.004 does, pacs.002 calls its `TxInfAndSts` like
  pain.002 does, and pacs.008/pain.001 share `CdtTrfTxInf`. Identity is
  therefore the message's own container, and rows are only produced inside it —
  otherwise a camt.056 read as pacs.004 yields plausible rows with every
  return-specific column NULL;
- **one Document, several messages** — pacs.002.001.03 files carry several
  complete `FIToFIPmtStsRpt` blocks, each with its own header. Every payment
  and status reader scopes its container by depth and clears the carried
  context when a new one opens, so this holds for any family, not just
  pacs.002. See
  [`docs/adr/0004-container-scope-is-message-scope.md`](adr/0004-container-scope-is-message-scope.md);
- **agents without a BIC** — SIX identifies the camt.056 assigner only by
  clearing-system member id;
- **containers renamed between eras** — pacs.009's container was
  `FinInstnCdtTrf` before it became `FICdtTrf`, and the first editions of every
  family name the container after the message version itself.

Some apparent bugs turned out to be correct behaviour and were left alone: a
camt statement with only balances yields zero rows from `read_iso20022` because
it has no `<Ntry>` — `read_camt_balances` is where those balances are — while a
file of the wrong message type is a loud error rather than an empty table: a
template with `{placeholder}` amounts or a pacs.002 pointed at `read_pacs004`
fails instead of silently returning nothing.
