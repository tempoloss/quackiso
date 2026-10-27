# Functions

| Function | Messages | Grain |
| --- | --- | --- |
| `read_iso20022(path)` | camt.053 statements, camt.054 notifications, camt.052 reports | one row per booked entry |
| `read_camt_transactions(path)` | the same three, at transaction grain | one row per `NtryDtls/TxDtls` |
| `read_camt_balances(path)` | the same three, at balance grain | one row per `Bal` |
| `read_camt_amount_details(path)` | the same three, at amount-block grain | one row per `AmtDtls` block |
| `read_camt_remittance(path)` | the same three, at remittance grain | one row per remittance text leaf |
| `read_pacs008(path)` | pacs.008 FI-to-FI credit transfer (the ISO 20022 MT103) | one row per `CdtTrfTxInf` |
| `read_pacs009(path)` | pacs.009 financial institution transfer (MT202 / MT202COV) | one row per `CdtTrfTxInf` |
| `read_pacs003(path)` | pacs.003 FI-to-FI direct debit (the interbank leg of pain.008) | one row per `DrctDbtTxInf` |
| `read_pacs010(path)` | pacs.010 FI-to-FI direct debit (both sides are banks) | one row per `DrctDbtTxInf` |
| `read_pacs004(path)` | pacs.004 payment return (settled money coming back) | one row per `TxInf` |
| `read_pacs007(path)` | pacs.007 payment reversal (the sender takes it back) | one row per `TxInf` |
| `read_pacs002(path)` | pacs.002 FI-to-FI payment status report | one row per status statement |
| `read_pacs028(path)` | pacs.028 FI-to-FI payment status request (the "where is my money?") | one row per status request |
| `read_pain001(path)` | pain.001 credit transfer initiation | one row per transaction |
| `read_pain002(path)` | pain.002 customer payment status report | one row per status statement |
| `read_pain008(path)` | pain.008 direct debit initiation (the creditor pulls) | one row per collection |
| `read_pain009(path)` | pain.009 mandate initiation (the mandate a pain.008 pulls against) | one row per `Mndt` |
| `read_pain010(path)` | pain.010 mandate amendment | one row per amendment |
| `read_pain011(path)` | pain.011 mandate cancellation | one row per cancellation |
| `read_pain012(path)` | pain.012 mandate acceptance report (the answer to the three above) | one row per answer |
| `read_pain013(path)` | pain.013 creditor payment activation request (request to pay) | one row per `CdtTrfTx` |
| `read_pain014(path)` | pain.014 creditor payment activation request status report | one row per status statement |
| `read_camt056(path)` | camt.056 payment cancellation request | one row per cancellation statement |
| `read_camt055(path)` | camt.055 customer payment cancellation request | one row per cancellation statement |
| `read_camt029(path)` | camt.029 resolution of investigation (the answer to a camt.056) | one row per statement |
| `read_camt027(path)` | camt.027 claim of non-receipt (the money never arrived) | one row per claim |
| `read_camt028(path)` | camt.028 additional payment information | one row per answer |
| `read_camt030(path)` | camt.030 notification of case assignment | one row per notification |
| `read_camt031(path)` | camt.031 reject investigation | one row per rejection |
| `read_camt036(path)` | camt.036 debit authorisation response | one row per response |
| `read_camt037(path)` | camt.037 debit authorisation request (may I take this back?) | one row per request |
| `read_camt087(path)` | camt.087 request to modify a payment | one row per request |
| `read_camt057(path)` | camt.057 notification to receive (money on its way in) | one row per `Itm` |
| `read_mt101(path)` | SWIFT MT101 request for transfer (the FIN original of pain.001) | one row per **transaction** |
| `read_mt103(path)` | SWIFT MT103 single customer credit transfer | one row per message |
| `read_mt104(path)` | SWIFT MT104 direct debit and request for debit transfer (the FIN side of pain.008) | one row per **transaction** |
| `read_mt202(path)` | SWIFT MT202 and MT202COV financial institution transfer | one row per message |
| `read_mt940(path)` | SWIFT MT940 customer statement | one row per `:61:` line |
| `read_mt942(path)` | SWIFT MT942 interim transaction report | one row per `:61:` line |
| `sniff_iso20022(path)` | any ISO 20022 message above, any SWIFT MT message above, or anything claiming to be one | one row per **file** |
| `audit_addresses(path)` | any ISO 20022 message above **or any SWIFT MT**, for the postal address of every party in it | one row per **party** |

`path` is a file, a glob, or a list of them, gzipped or not: `.xml`, `.xml.gz`, and a gzipped file
that kept its `.xml` name all read alike, because the first two bytes decide and
not the name. Every row carries `source_file`, so a glob over a year of
statements stays attributable, and one glob may mix the two. Bytes after the last
gzip member are an error and not padding to ignore, so a half-written append
fails the query rather than quietly truncating the statement. Every function also
takes `threads := n`; see [Streaming](streaming.md).

A list (`read_iso20022(['a.xml', 'inbox/*.xml'])`) is read pattern by pattern in
the order written, and a file that two patterns reach is read once, because
reading it twice would double a `SUM`. An empty list, a NULL and a value that is
not VARCHAR or VARCHAR[] are errors. A scan that fails on a file another reader
owns says which: `... - sniff_iso20022 reads this file as camt.053, use
read_iso20022`. A whole inbox in one session, with the sniffer choosing the
files ([ADR 0011](adr/0011-a-path-can-be-a-list.md)):

```sql
SET VARIABLE files = (
    SELECT list(source_file) FROM sniff_iso20022('inbox/*')
    WHERE reader = 'read_iso20022' AND error IS NULL
);
SELECT * FROM read_iso20022(getvariable('files'));
```

XML readers inspect at most the first 64 KiB of the decompressed stream before
starting quick-xml. A file with no markup in that prefix fails as
`not XML: no markup in the first 64 KiB`; a SWIFT MT marker before markup fails
as `not XML: SWIFT MT marker before markup`. Accepted bytes are replayed, so
valid plain files, gzip files, concatenated gzip members, and FIFO input keep
the same parser behavior. `sniff_iso20022` still returns one result row for
non-XML input instead of aborting the scan.

Five kinds of file are refused by name before any of that, because a container
is not a message and it holds bytes that look like one:

| bytes | refusal |
| --- | --- |
| ZIP, including empty and ZIP64 | `ZIP archive: extract a member before reading` |
| TAR, ustar/GNU/v7, gzipped or not | `TAR archive: extract a member before reading` |
| PKCS#7 or CMS, armored, S/MIME or DER | `PKCS#7 envelope: unwrap, decrypt, or verify it before reading` |
| OpenPGP, armored or binary | `PGP envelope: decrypt it before reading` |
| EBICS request or response | `EBICS transport envelope: process it with an EBICS client first` |

Every public path says the same sentence: `sniff_iso20022` puts it in `error`
and keeps its row, and the readers and the audit raise it with the path. Nothing
is decompressed, decrypted or read as a member. Detection is structural rather
than a magic string, so an XML document quoting `ustar` in a comment, an
ordinary DER certificate, and a runbook mentioning PGP all stay what they are.

An envelope is what the whole file is, so PEM armor has to open the file: a
camt.053 shipping its own detached signature in `<SplmtryData><Envlp><Sgntr>`,
and a payment whose free text reads `MIGRATION BEGIN CMS PHASE 2`, are messages
that carry an envelope rather than envelopes. A media type is read from a MIME
header block and not from a body that names one.

The sniffer recognises SWIFT MT as well, by the block structure rather than by a
namespace: an MT file reports an `mt.nnn` family, a NULL `namespace`, and a
`records` count that is the rows its reader would return. The MT readers take the
same globs and the same gzip as the XML ones. `audit_addresses` is the one
function that reads both wire formats, so it has a guard of its own: it refuses
only bytes that are neither, as `neither XML nor SWIFT MT in the first 64 KiB`.
