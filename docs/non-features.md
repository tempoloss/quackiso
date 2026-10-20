# Deliberate non-features

- **No `s3://` or `https://` paths.** Attempted and removed: opening a remote file
  needs the executing query's client context, which `duckdb-rs` does not expose
  from a safe table function. See
  [`docs/adr/0002-no-remote-paths.md`](adr/0002-no-remote-paths.md).
- **No XSD validation.** Every defect the real corpus exposed was the reader being
  too strict, not the file being invalid. See
  [`docs/adr/0003-no-xsd-validation.md`](adr/0003-no-xsd-validation.md).
- **`threads := n` is not obeyed literally.** It is capped at the file count and
  at four times the machine's parallelism, because the failure a hundred thousand
  threads produces is resource exhaustion mid-scan rather than a slow scan. See
  [`docs/adr/0005-explicit-thread-count-is-capped.md`](adr/0005-explicit-thread-count-is-capped.md).
- **Nine known columns are missing on purpose.** camt.055 has no `case_id`,
  pacs.007 no `original_settlement_date`, pacs.002 no group-level
  `OrgnlNbOfTxs`/`OrgnlCtrlSum`, pacs.009 no non-COV `RmtInf`, camt.029 no
  `PAYMENT_INFO` scope. The mandate and investigation readers added three more:
  no `case_creator` on camt.055, camt.056 and camt.029, no `amount_to_debit` or
  `value_date_to_debit` on camt.036 and no `value_date_to_debit` on camt.037,
  and no instructing or instructed agent columns on `read_pain009`. Each widens
  a published schema and is argued separately.
  See [`docs/adr/0006-audit-findings-deferred.md`](adr/0006-audit-findings-deferred.md).
- **Archives and envelopes are named, not opened.** A ZIP or TAR of the day's
  statements, a PKCS#7-signed camt.053, a PGP-encrypted pain.001, an EBICS
  request: each is refused by name with what to do about it. A TAR of two
  statements used to parse as one file and return the entries of both members
  under one `source_file`, which is two accounts added together with nothing on
  the row saying so. Detection is structural rather than a magic string - a
  checksummed TAR header, a CMS content-type OID, a walkable OpenPGP packet
  chain - so an ordinary XML document that quotes the word `ustar` stays XML.
  `sniff_iso20022` reports the reason in `error` and keeps its row; every reader
  and the audit raise it with the path.
- **A statement's details are separate functions, not wider entry rows.** A
  batched entry has three end-to-end ids and one column to put them in, so the
  fix is a grain and not a column: `read_camt_transactions`,
  `read_camt_balances`, `read_camt_amount_details` and
  `read_camt_remittance`. Nothing in them falls back to an entry value, no row is
  invented for a batch that carries no transaction, and no nested type is
  returned. See
  [`docs/adr/0009-camt-details-have-separate-grains.md`](adr/0009-camt-details-have-separate-grains.md).
