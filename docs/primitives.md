# Primitives

This document explains the mechanisms quackiso rests on: not how to use the extension, but what each primitive does and why the code is shaped around it. Every entry is anchored to source, tests, or ADRs that were opened and checked before writing.

The line-by-line annotations behind these entries live in [`primitives.code.json`](primitives.code.json): the cited ranges, the note lines, and a fingerprint of the code each one was written against. `python3 scripts/check_primitives_anchors.py` fails when an anchor in this file or in that one no longer points at the code it describes. The Primitives workflow runs it on every push, re-anchors a pure line shift by matching content instead of line numbers, and pushes that repair back, so only a substantive code change needs a person.

## Numbers and money

### Binary floating point

A binary floating-point number is a finite sum of halves: $1/2$, $1/4$, $1/8$, and so on. One tenth is not a finite sum of those pieces, so `0.1` in a `DOUBLE` is the nearest available binary fraction, not exactly one tenth.

**Where:** `src/decimal.rs:1-7` - amount comments say money must not round-trip through `f64`; `src/lib.rs:552-570` maps money columns to DuckDB `DECIMAL`, not `DOUBLE`.

**What breaks if it is wrong:** 1. The file carries `0.10`, `0.20`, `0.30`, and `1500.10`. 2. By hand, `0.10 + 0.20 + 0.30 = 0.60`, and `0.60 + 1500.10 = 1500.70`. 3. Stored as binary floats, those decimal values are approximations, and the old total was `1500.7000000000003`. 4. A reconciliation query can fail an equality check or show a strange cent-level tail even though the wire values look ordinary.

**Caught by:** `test/sql/quackiso.test:234-249` asserts the `SUM(amount)` is `1500.70000` and exactly equals `1500.70`; `decimal::tests::exact_where_float_is_not` in `src/decimal.rs:98-106` checks the scaled representation of `0.1` and `1500.10`.

### Scaled integer amounts

A scaled integer stores a decimal by removing the decimal point and remembering the scale. With scale 5, `1500.10` is stored as `150010000`: the last five digits are the fractional part, so addition is integer addition.

**Where:** `src/decimal.rs:13-30` defines scale 5 and parses wire text into an `i128`; `src/model.rs:208`, `src/pacs008.rs:73`, and `src/pain001.rs:84` store amounts as scaled `i128` values in rows.

**What breaks if it is wrong:** 1. A parser reads the text amount and converts through a float. 2. The amount loses exact decimal identity before DuckDB ever sees it. 3. `SUM`, equality, and grouping operate on the rounded value. 4. The SQL result is plausible enough to pass a glance and wrong enough to matter.

**Caught by:** `decimal::tests::shapes_seen_in_real_messages` in `src/decimal.rs:108-118` checks common wire shapes, trimming, signs, `.5`, and five fractional digits.

### `DECIMAL(38,5)` and 128-bit storage

`DECIMAL(width, scale)` means a fixed-point number with at most `width` total digits and `scale` digits after the decimal point. DuckDB stores `DECIMAL(38,5)` in a 128-bit integer; `DECIMAL(18,5)` is only a 64-bit decimal class, and it cannot hold a legal ISO amount with 18 integer digits once the five scale digits are appended. The storage is *wider* than the column it backs: `i128` reaches about `1.7 * 10^38` and `DECIMAL(38,5)` stops at `10^38 - 1`, so an amount can scale into a value the integer holds and the column cannot.

**Where:** `src/decimal.rs:9-17` states the ISO 18-significant-digit requirement and the `DECIMAL(38,5)` choice; `src/lib.rs:566-570` declares money columns with `decimal::WIDTH` and `decimal::SCALE`; `src/lib.rs:633-636` writes that column as `i128`.

**What breaks if it is wrong:** 1. A legal amount arrives as `123456789012345678`. 2. At scale 5, the stored integer must be `12345678901234567800000`. 3. A 64-bit decimal representation cannot hold that integer. 4. The scan either errors on a legal file or silently switches to a less exact representation. In the other direction: a 34-integer-digit amount passes every overflow check the arithmetic can make and is still unstorable, so it is refused at `src/decimal.rs:23` rather than written and read back as something else.

**Caught by:** `test/sql/quackiso.test:251-257` asserts that `123456789012345678.00000` survives; `decimal::tests::eighteen_integer_digits_fit` in `src/decimal.rs:120-128` checks the scaled integer directly, and `decimal::tests::a_value_i128_holds_but_the_column_does_not_is_refused` in `src/decimal.rs:137-143` pins both edges of the band above it.

### Five fractional digits

Scale 5 means the system keeps five digits after the decimal point. Scale 2 is enough for cents, but ISO 20022 amounts are not just card charges or ledgers in a two-decimal currency; real files can carry five fractional digits.

**Where:** `src/decimal.rs:13-19` fixes the scale at 5; `testdata/camt053_decimal_sample.xml:40-44` records the real `5013090.23491` shape; `test/sql/quackiso.test:259-265` asserts that value comes out unchanged.

**What breaks if it is wrong:** 1. A message carries `5013090.23491`. 2. A scale-2 parser has no exact place for `491`. 3. It must reject, round, or truncate. 4. Rejecting loses a readable bank file; rounding or truncating changes money.

**Caught by:** `test/sql/quackiso.test:259-265` checks the five-decimal amount; `decimal::tests::precision_loss_is_refused_but_padding_is_not` in `src/decimal.rs:130-135` rejects a sixth meaningful digit while accepting trailing zeros.

### Amount errors instead of NULL

SQL `NULL` means “missing”, not “bad but close enough”. Aggregates such as `SUM` ignore `NULL`, so turning a malformed amount into `NULL` can return a total that looks valid and is missing a row.

**Where:** `src/decimal.rs:25-29` documents `Err` instead of silent `None`; `src/wire.rs:154-160` returns that error for a malformed amount rather than a NULL; `src/model.rs:225-246` propagates it into the scan.

**What breaks if it is wrong:** 1. One row says `<Amt>12.34.56</Amt>`. 2. The parser stores `NULL` for that amount and continues. 3. `SUM(amount)` ignores the row. 4. The query exits 0 with a smaller total and no visible sign that money disappeared.

**Caught by:** `test/sql/quackiso.test:267-271` expects an error for `camt053_bad_amount.xml`; `decimal::tests::malformed_is_an_error_not_a_null` in `src/decimal.rs:145-153` rejects empty, alphabetic, comma, and double-dot amounts.

## Streaming

### Pull-based XML events

A pull parser gives the program the next XML event only when the program asks: start tag, text, end tag, end of file. That is different from building a document tree, where the whole XML file is loaded into nested objects before the first row can be returned.

**Where:** `src/stream.rs:47-102` loops on `read_event_into` and returns one row at a time — the pacs and pain readers do the same — while `src/wire.rs:82-120` copies only the current subtree, so no reader ever builds a document tree.

**What breaks if it is wrong:** 1. A 1.7 GB statement is parsed into a tree. 2. The process needs memory proportional to the whole file plus deserialized objects. 3. DuckDB has no row to consume until that tree exists. 4. Large statements fail or swap before SQL sees the first entry.

**Caught by:** `test/sql/quackiso.test:8-30` exercises the streamed camt rows; `membound::the_documented_statement` in `src/membound.rs:904-943` generates that 1.7 GB, three-million-entry statement, parses it through the production scan loop, and holds the peak to 1.23 MiB of live heap and 2.04 MiB of resident memory — the tree that is never built, measured rather than asserted in prose. Generated entries are uniform, so `membound::peak_is_bounded_on_real_entry_shapes` in `src/membound.rs:869-888` repeats the bound over 20,000 `<Ntry>` subtrees copied verbatim out of the corpus files.

### Row grain and carried context

The grain is the thing one SQL row represents. In camt files it is one booked `<Ntry>`; in pacs.008 and pain.001 it is one `<CdtTrfTxInf>`, with message, statement, or payment-group context carried beside that subtree.

**Where:** `src/lib.rs:3-37` names the thirteen readers, the sniffer, and their row grain; `src/stream.rs:117-119` keeps statement context outside entry subtrees; `src/pain001.rs:5-12` explains that debtor context lives on `PmtInf` and must be carried down.

**What breaks if it is wrong:** 1. A pain.001 file has two `PmtInf` groups with different debtors. 2. The reader treats debtor as a transaction-local field or forgets to reset group context. 3. Rows inherit the wrong payer or lose it. 4. SQL groups payments under the wrong account.

**Caught by:** `test/sql/quackiso.test:191-224` asserts three pain.001 transaction rows, debtor context by payment group, requested execution dates, and group-level `ChrgBr` inheritance.

### Batch-sized chunks and `O(batch)` memory

`O(batch)` here means the live output rows are bounded by one DuckDB vector batch: at most 2048 flattened rows, plus the XML event buffer and the one entry or transaction subtree currently being copied. It does not mean the parser has loaded the file: a 1.7 GB statement reads in 1.23 MiB of live heap and about 2 MB resident (`README.md:297-298`). It also does not mean the peak is independent of the input. Both terms are real and both are measured — 2048 rows carrying 4 KiB of remittance text cost 8 MiB more than narrow ones, and one 16 MiB `<Ntry>` costs about six times its own size, because a fat subtree is live as a copy, as a deserialized struct, and as a row at the same time.

**Where:** `src/lib.rs:102-103` sets `VECTOR_SIZE` to 2048; `src/lib.rs:342-366` fills a `Vec` until that size or end-of-file; `src/lib.rs:688-693` writes that batch and tells DuckDB the row count.

**What breaks if it is wrong:** 1. `pull_batch` keeps appending until a file ends. 2. A large statement creates a huge `Vec<Row>`. 3. DuckDB still receives rows only after the file drains. 4. Memory follows file size instead of the output chunk.

**Caught by:** `membound::peak_does_not_follow_file_size` in `src/membound.rs:609-645` — eight times the file, the same 1.23 MiB peak — and `membound::peak_follows_the_output_batch` in `src/membound.rs:685-715`, which widens the row and moves the peak by exactly one batch. `membound::peak_follows_the_largest_subtree` in `src/membound.rs:724-770` holds the other term: quadruple the subtree, quadruple the peak. Each case is held to the value it measured within ±25%, not to a loose ceiling: a ceiling four times over the measurement would hide a doubling.

### Compression is decided by the bytes, not the name

A gzipped statement is the same statement, so nothing about it is configured: the reader takes the first two bytes of a file, hands them back to the stream, and either inflates the rest or does not. `.xml`, `.xml.gz`, and a gzipped file that kept its `.xml` name all read alike, and one glob may mix them. Handing the bytes back rather than seeking over them is what keeps the source ordinary — a statement may arrive down a FIFO, and a FIFO cannot seek. Concatenated members are one document, which is what an appended daily dump is; bytes after the last member are an error rather than padding to ignore, so a half-written append fails instead of truncating, where `zcat` would hand back a short statement. What the decoder adds to memory is its own fixed state — an input buffer, an LZ77 window, huffman tables, 82,217 bytes measured — and nothing per entry. What it does change is the subtree term: an entry used to be capped by the file it came in, and a 35 KB gzip now carries a 16 MiB `<Ntry>` that peaks at 96.7 MiB, so the bound is in inflated bytes and `ls` no longer shows it.

**Where:** `src/lib.rs:162-198` reads the magic and reports how much of it a short file had; `src/lib.rs:142-179` puts those bytes back in front of the file, wraps the result in a `MultiGzDecoder` when they match, and gives the source its name so a mid-stream failure says which file.

**What breaks if it is wrong:** 1. A `.xml.gz` file is parsed as XML and fails as not well-formed. 2. `GzDecoder` in place of `MultiGzDecoder` stops at the first member and silently truncates an appended dump. 3. Detection by extension misses a gzipped file named `.xml` and mis-reads a plain file named `.gz`. 4. Consuming the magic without putting it back eats the first two bytes of the document — and seeking back instead demands a seekable source, which a pipe is not. 5. A truncated member fails with `unexpected end of file` and no file name, which over a year of statements names nothing at all. 6. "Compression is free" is read as covering the subtree term, and a small file is assumed to be a small parse.

**Caught by:** `tests::gzip_reads_exactly_like_the_plain_file` in `src/lib.rs:1690-1709` — one member, two members, and a misnamed file all produce the rows the plain file produces; `tests::a_broken_gzip_fails_instead_of_panicking` in `src/lib.rs:1712-1753` holds seven shapes of broken input to an error rather than a panic, and to naming the file: truncated, bad deflate, one byte, empty, trailing bytes, zero padding, and a gzip inside a gzip; `tests::another_reader_gets_gzip_from_the_shared_source` in `src/lib.rs:1760-1769` reads a namespace-prefixed pacs.008 through the decoder, standing in for the thirteen readers that are not `read_iso20022`; `tests::a_statement_may_arrive_down_a_pipe` in `src/lib.rs:1790-1822` feeds both shapes through a FIFO, resolved as a path the way a query would; `membound::peak_does_not_follow_compression` in `src/membound.rs:651-679` holds the decoder's own cost to the recorded `GZIP_HEAP` within ±25%; `membound::a_small_gzip_can_carry_a_large_subtree` in `src/membound.rs:778-814` measures the term compression decouples; `test/sql/quackiso.test:32-71` runs it through DuckDB, including a glob that mixes the two and a sniff of the gzip.

## Parallelism

### The parallel unit is the file

A file of XML can only be parsed front to back: at any byte in the middle, a
parser cannot know what element it is inside. Block-structured formats are
divisible by design; XML is not. So when a glob matches many files, the unit of
parallel work is the whole file, and a single document is always one sequential
pass.

**Where:** `src/lib.rs:379-394` picks the worker count — an explicit
`threads := n` wins, itself capped at the file count and at four times the
machine's parallelism, the default is one worker per file capped at that
parallelism, and one file is always sequential; `src/lib.rs:467-494`
decides sequential-versus-parallel at the first batch, when both the file count
and the argument are in hand.

**What breaks if it is wrong:** 1. A reader splits one document at byte N. 2.
The parser lands mid-element with no path context. 3. Whatever "rows" it
recovers are stitched from tag soup. 4. Money columns filled by guesswork are
worse than a slower scan.

**Caught by:** `test/sql/quackiso.test:733-730` runs the same glob
with `threads := 4` and `threads := 1` and expects identical counts and
identical sums.

### Bounded channels and backpressure

A channel is a queue between threads. A *bounded* channel has a capacity, and a
sender that reaches it blocks — that blocking is backpressure: producers can
never run further ahead of the consumer than the capacity allows.

**Where:** `src/lib.rs:396-460` gives the workers a `sync_channel` of
`threads × 2` batches, so parse-ahead memory is O(threads × batch) no matter
how many files the glob matched; a dropped receiver — a `LIMIT`, an error —
fails every following `send` and the workers exit; the template sender is
dropped after spawning, so the channel disconnects when the last worker
finishes, which is how the scan knows it is done.

**What breaks if it is wrong:** 1. The channel is unbounded. 2. Workers parse a
10,000-file year of statements faster than DuckDB drains rows. 3. Every parsed
row waits in memory at once. 4. The streaming reader's O(batch) promise
silently becomes O(corpus).

**Caught by:** `test/sql/quackiso.test:747-752` asserts an error
in any worker fails the whole query; `membound::parallel_peak_follows_threads_not_corpus`
in `src/membound.rs:820-862` puts three times the corpus behind the same eight
workers and holds the peak to the structure — a batch per worker, twice that
queued, one in the consumer's hand, 25 in all — rather than to a number, because
this is the one figure here that moves with the machine: 9.3 MiB on a four-core
runner, 19.9 MiB on a 32-core box, never with the glob. End to end, through
DuckDB rather than through the scan loop, `scripts/measure_in_duckdb.py
--glob-copies 8` scans eight 173 MB statements for 23.7 MiB over the baseline,
against 7.6 MiB for one — and checks the rows and the sum, since a worker that
claimed a file twice would otherwise look thrifty.

### Atomic work claiming

An atomic integer is a counter the hardware updates in one indivisible step.
`fetch_add(1)` hands every caller a value nobody else received — which makes
one atomic counter the entire scheduler: no job queue, no lock, no coordinator
thread.

**Where:** `src/lib.rs:421-428` shares one `AtomicUsize` across the
workers; each claims the next unparsed file with `fetch_add(1, Relaxed)` and
exits when the index runs off the end. `Relaxed` suffices because the counter
guards nothing but itself — the row handoff happens in the channel, which
brings its own ordering.

**What breaks if it is wrong:** 1. The counter is a plain integer, read then
incremented. 2. Two workers read 7 at once and both parse file 7. 3. Every row
of that file appears twice. 4. `SUM(amount)` doubles for one file — plausible,
wrong, and timing-dependent.

**Caught by:** `test/sql/quackiso.test:733-730` — a duplicated
claim would double both the count and the sum; the test pins both.

## XML

### Elements, attributes, and namespaces

An XML element is a named container like `<Amt>18500.75</Amt>`. An attribute is a key-value attached to the start tag, such as `Ccy="EUR"`; a namespace qualifies names so different vocabularies can share words without collision.

**Where:** `src/model.rs:67-74` maps amount text and the `@Ccy` attribute separately; `src/model.rs:10-12` notes that quick-xml serde matches local tag names for default namespaces; `src/wire.rs:33-44` strips prefixes to local names.

**What breaks if it is wrong:** 1. The reader treats attributes as child elements and never reads `Ccy`. 2. It treats `{namespace}Amt` as a different field from `Amt`. 3. Amounts still appear but currencies or whole transactions are `NULL`. 4. A result set looks populated while losing the fields needed to interpret the money.

**Caught by:** `test/sql/quackiso.test:23-30` checks amounts in camt rows; `test/sql/quackiso.test:98-169` checks amount, currency, and BICFI values in a prefixed pacs.008 file.

### Namespace-prefixed subtrees

A namespace prefix is the short name before the colon: in `<Doc:CdtTrfTxInf>`, `Doc` is the prefix and `CdtTrfTxInf` is the local element name. This reader copies one transaction subtree into a synthetic unprefixed root before deserializing it, so every copied start and end tag must be normalised the same way.

**Where:** `src/wire.rs:72-81` describes the prefixed-subtree failure; `src/wire.rs:82-120` rewrites copied start, empty, and end tags to local names while preserving attributes; every reader hands its subtree to that one shared function.

**What breaks if it is wrong:** 1. The source has `<Doc:CdtTrfTxInf>`. 2. The copied buffer starts with synthetic `<CdtTrfTxInf>`. 3. The copied close tag remains `</Doc:CdtTrfTxInf>`. 4. The buffer is not well-formed XML because the root name does not match its close tag, and deserialization rejects it.

**Caught by:** `test/sql/quackiso.test:155-162` reads `pacs008_prefixed_sample.xml` and expects the transaction, UETR, amount, and currency.

### No XSD validation

Ill-formed XML is not XML: tags do not nest, a close tag does not match, or the file ends inside an element. Schema-invalid XML is still XML, but it does not satisfy a particular XSD; quackiso rejects ill-formed input and bad amounts, but deliberately does not run XSD validation before reading.

**Where:** `docs/adr/0003-no-xsd-validation.md:20-38` gives the blocker: real corpus defects were reader-tolerance bugs, the test corpus spans roughly fifteen schemas, and `libxml` would add a C dependency across native and WASM builds; `docs/adr/0003-no-xsd-validation.md:46-52` states what is still rejected.

**What breaks if it is wrong:** 1. The extension validates against the wrong one of many ISO 20022 schemas. 2. A bank file that has readable fields but a version or wrapper variation is refused before extraction. 3. Users get no SQL rows even though the data the reader needs is present. 4. The code optimises for rejecting inputs when the real bugs were mostly the reader being too strict.

**Caught by:** `test/sql/quackiso.test:73-80` catches later camt party/account shapes; `test/sql/quackiso.test:155-162` catches prefixed pacs.008; `test/sql/quackiso.test:207-224` catches pain.001 date wrapping and group-level fields.


### Message identity is the container

ISO 20022 reuses transaction element names across message families: camt.056
names its transaction `TxInf` exactly as pacs.004 does, pacs.002 uses
`TxInfAndSts` like pain.002, and pacs.008, pacs.009 and pain.001 all say
`CdtTrfTxInf`. What identifies a message is therefore not its transaction
element — it is the message's own container, whose name also changed between
eras.

**Where:** `src/pacs004.rs:292-306` records why `TxInf` alone is
not identity; every payment reader treats a transaction element as one only
while the cursor is inside its own container — the flag carries that
container's depth, so a second message in the same envelope cannot claim the
first one's transactions — and the EOF error names the container that was
missing.

**What breaks if it is wrong:** 1. A camt.056 is passed to `read_pacs004`. 2.
Its `TxInf` deserializes — the field names overlap. 3. Plausible rows appear
with every return-specific column NULL. 4. Nothing fails, and the "returns"
table quietly contains cancellation requests.

**Caught by:** `test/sql/quackiso.test:459-495` and the guard tests
beside each reader: every wrong-type pairing is asserted to fail loudly,
naming the expected container.

## Returns and status reports

### The return chain is read crossed

A payment return (pacs.004) points at money that already settled and is now coming back. Every reference in it describes the message being undone, and the parties are often stated only in `<RtrChain>` — the chain of the *return*, whose debtor is the party giving the money back, i.e. the original creditor.

**Where:** `src/pacs004.rs:162-194` resolves the original sides from `OrgnlTxRef` when present and otherwise reads `<RtrChain>` crossed, so `original_debtor_*` is the party that was originally paid from, never the party sending the reversal.

**What breaks if it is wrong:** 1. A return states parties only in `<RtrChain>`. 2. The reader copies `RtrChain/Dbtr` into `original_debtor_name`. 3. Every returned payment names the wrong payer with full confidence. 4. A reconciliation join against the original pacs.008 silently matches the wrong side, which is worse than a `NULL`.

**Caught by:** `test/sql/quackiso.test:301-308` joins the return to its original pacs.008 on the shared UETR and asserts both sides agree.

### Status at three levels

A payment status report (pain.002) states its status at three nested levels: the whole batch, one payment group, and one transaction. Only the group level is mandatory, so a bank can accept or reject an entire file without detailing a single transaction.

**Where:** `src/pain002.rs:72-74` names the three levels; `src/pain002.rs:347-364` emits one row per status statement as each element closes and clears the payment-group context so no row inherits a neighbour's id.

**What breaks if it is wrong:** 1. A bank rejects a whole batch at group level and lists no transactions. 2. A reader whose grain is the transaction returns zero rows. 3. The query for "was my batch accepted?" returns nothing while the message plainly said so. 4. A batch-level rejection is invisible in SQL.

**Caught by:** `test/sql/quackiso.test:391-397` asserts a three-level report produces one group row, one row per payment group, and one per transaction.

## Dates and times

### `DATE` versus `TIMESTAMP`

A `DATE` is a calendar day. A `TIMESTAMP` is an instant or local date-time with hours, minutes, seconds, and fractional seconds, so `2019-01-23` and `2023-10-01T13:37:14.000Z` cannot both be faithfully described as a date-only value.

**Where:** `src/temporal.rs:3-7` lists the mixed wire shapes and says date-times become UTC-normalised timestamps; `src/lib.rs:552-554` states that dates keep wire precision; `src/lib.rs:713-729` makes camt booking/value dates `Col::Stamp`, while `src/lib.rs:762-770` and `src/lib.rs:811-816` use `Col::Date` for settlement and requested execution dates.

**What breaks if it is wrong:** 1. A timestamp value is forced into a `DATE` column. 2. The time and offset are thrown away. 3. Two payments on the same day but different instants become indistinguishable. 4. SQL date arithmetic may still run, but it runs on truncated data.

**Caught by:** `test/sql/quackiso.test:171-189` asserts camt date columns are `TIMESTAMP` and support timestamp arithmetic; `test/sql/quackiso.test:207-214` asserts pain requested execution dates are `DATE`.

### UTC offsets and normalisation

A UTC offset says how far the written time is from UTC: `+01:00` means local time is one hour ahead of UTC. Normalising to UTC means subtracting that offset so different textual representations of the same instant store the same timestamp.

**Where:** `src/temporal.rs:67-69` defines timestamps as microseconds since the Unix epoch normalised to UTC; `src/temporal.rs:111-129` handles `Z`, `+hh:mm`, and `-hh:mm` offsets.

**What breaks if it is wrong:** 1. A file says `1970-01-01T01:00:00+01:00`. 2. The parser stores one hour after the epoch instead of subtracting the offset. 3. The same instant written as `1970-01-01T00:00:00Z` no longer compares equal. 4. Ordering across banks or time zones is wrong.

**Caught by:** `temporal::tests::timestamps_normalise_to_utc` in `src/temporal.rs:149-164` checks `Z`, `+01:00`, `-01:00`, fractional seconds, and real corpus shapes.

### DuckDB date and timestamp integers

DuckDB `DATE` values are stored as days since `1970-01-01`. DuckDB `TIMESTAMP` values are stored as microseconds since `1970-01-01T00:00:00`, so the writer must emit an `i32` for dates and an `i64` for timestamps.

**Where:** `src/temporal.rs:57-69` documents and returns those physical values; `src/lib.rs:633-634` instantiates `write_date` as `i32` and `write_timestamp` as `i64`; `src/lib.rs:737-742` and `src/lib.rs:787-790` feed parsed temporal integers into output vectors.

**What breaks if it is wrong:** 1. The parser returns a formatted string or the wrong integer unit. 2. The vector writer places bytes DuckDB interprets as a date or timestamp. 3. SQL shows nonsense dates, or arithmetic returns nonsense intervals. 4. The error appears in query results, not at the XML boundary.

**Caught by:** `temporal::tests::epoch_and_dates` in `src/temporal.rs:139-147` checks day counts and invalid dates; `temporal::tests::timestamps_normalise_to_utc` in `src/temporal.rs:149-164` checks microsecond counts.

### A date the calendar does not have, and text that is not a date

Parsing a fixed-width date means slicing text at known positions. Two things can go wrong that a range check does not catch: the positions may not be character boundaries, and the numbers may be in range and still name no day.

**Where:** `src/temporal.rs:29-31` refuses non-ASCII input at the single entry point, which is what makes the six byte-range slices below it safe; `src/temporal.rs:48-55` holds real month lengths and the Gregorian leap rule, so `valid` takes the year and not just the month and the day.

**What breaks if it is wrong:** 1. A spreadsheet export puts a non-breaking space or a `€` inside a date. 2. Rust text is UTF-8, so a byte range can cut a character in half. 3. `&s[8..10]` panics with `end byte index 10 is not a char boundary`. 4. The user asked for a NULL and got a Rust slicing message. Separately: 1. A file states `2019-02-31`. 2. A month-and-day range check accepts it. 3. `days_from_civil` is total and answers anyway. 4. The column reads `2019-03-03`, a date the file never mentioned, and nothing says so.

**Caught by:** `temporal::tests::hostile_text_is_null_not_a_panic` in `src/temporal.rs:166-176` puts a multi-byte character at each of the three slice sites and a non-breaking space inside the date rather than trailing, where `trim` cannot remove it; `temporal::tests::a_day_that_does_not_exist_is_null` in `src/temporal.rs:178-187` pins 31 February, 31 April, and the 1900/2000 century pair. End to end, `testdata/camt052_report.xml` books an entry on `2026-02-31` and `test/sql/quackiso.test:131-138` asserts the column is NULL.

## DuckDB extension mechanics

### Table functions: `bind`, `init`, and `func`

A DuckDB table function is a function that appears in `FROM` and returns rows. `bind` decides the schema and permanent scan inputs, `init` creates per-scan state, and `func` is called repeatedly to fill the next output chunk.

**Where:** `src/lib.rs:647-708` generates the three table functions; `src/lib.rs:668-672` declares columns and resolves files in `bind`; `src/lib.rs:676-678` creates scan state in `init`; `src/lib.rs:682-695` pulls and writes the next batch in `func`.

**What breaks if it is wrong:** 1. Parsing happens in `bind`. 2. A bad or remote path fails before the scan, but a huge local file is also read before DuckDB asks for rows. 3. The query cannot stream, cancel cleanly between chunks, or keep memory bounded. 4. Bind-time errors and scan-time errors become confused.

**Caught by:** `test/sql/quackiso.test:273-285` checks bind-time path errors; `test/sql/quackiso.test:287-293` checks glob/local file resolution and `source_file` output.

### Vectors, chunks, and validity masks

A DuckDB output chunk is a small block of rows. Each column in that chunk is a vector, and each vector has a validity mask saying which row positions are `NULL`; setting a value and setting nullness are separate operations.

**Where:** `src/lib.rs:582-595` writes text vectors with `insert` or `set_null`; `src/lib.rs:597-636` writes numeric vectors through raw slices, recording the missing positions in a stack bitmap so each getter runs once per row, and calls `set_null` for them afterwards; `src/lib.rs:693` sets the chunk length after writing.

**What breaks if it is wrong:** 1. A missing optional XML field is left as whatever bytes were in the vector. 2. The validity mask is not marked null. 3. DuckDB treats the slot as a real empty string, zero, old value, or invalid decimal. 4. SQL filters and aggregates operate on invented data.

**Caught by:** `test/sql/quackiso.test:120-182` checks the exposed DuckDB types, and `test/sql/quackiso.test:23-30`, `test/sql/quackiso.test:98-169`, and `test/sql/quackiso.test:207-224` exercise text, decimal, date, and inherited fields through vectors; there is nothing yet that directly asserts a particular missing field is `NULL`.

### No remote paths

A local path is opened by this process. A DuckDB remote URI such as `s3://...` or `https://...` should be opened by DuckDB's own filesystem because that is where secrets and session settings live. At the extension boundary, that filesystem is reached through the DuckDB C extension API: a table of function pointers plus opaque handles, not normal Rust methods.

**Where:** `src/lib.rs:499-528` rejects URI schemes while allowing Windows drive letters, keeps only the files a glob matched, and still resolves a literal name glob refuses to compile; `docs/adr/0002-no-remote-paths.md:12-18` lists the raw filesystem calls; `docs/adr/0002-no-remote-paths.md:34-54` explains the blocker: remote opens need an active transaction, `duckdb-rs` hides the raw `duckdb_function_info`, and wrapping DuckDB's output chunk is not public.

**What breaks if it is wrong:** 1. The extension caches a filesystem from a private connection. 2. A remote open tries to resolve secrets without the executing query's active transaction and fails with `TransactionContext::ActiveTransaction called without active transaction`. 3. The fix needs the scan callback's raw `duckdb_function_info`, so the safe `VTab` wrapper is no longer enough. 4. Hand-written C callbacks would then reimplement chunk sizing, string assignment, and validity masks in `unsafe` for a feature not needed to parse local files.

**Caught by:** `test/sql/quackiso.test:273-285` asserts `s3://` is refused with a clear message and `Z:/...` is treated as a Windows path, not a URI.

## Rust and FFI

### `unsafe` slices and borrows

`unsafe` marks code where Rust cannot prove the memory rules; it does not mean the rules stop applying. A borrow is a temporary loan of access: while a mutable slice borrowed from a vector is alive, the code cannot also call methods that touch the same vector.

**Where:** `src/lib.rs:597-621` puts the raw numeric slice in an inner scope before calling `set_null`; `src/stream.rs:49-52` converts XML events into owned actions so the borrow of `self.buf` ends before calling another `&mut self` method.

**What breaks if it is wrong:** 1. The code keeps `let slice = v.as_mut_slice()` alive. 2. It calls `v.set_null(i)` while the mutable slice still borrows the same vector. 3. Safe Rust rejects the compile because two mutable accesses overlap. 4. Forcing it with raw pointers would make it possible to write through a stale slice after DuckDB changed vector metadata.

**Caught by:** nothing yet at runtime; this is mainly caught by the Rust compiler. The SQL coverage in `test/sql/quackiso.test:120-182` and decimal/date unit tests exercise the writer after it compiles.
