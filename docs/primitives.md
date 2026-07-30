# Primitives

This document explains the mechanisms quackiso rests on: not how to use the extension, but what each primitive does and why the code is shaped around it. Every entry is anchored to source, tests, or ADRs that were opened and checked before writing.

The line-by-line annotations behind these entries live in [`primitives.code.json`](primitives.code.json): the cited ranges, the note lines, and a fingerprint of the code each one was written against. `python3 scripts/check_primitives_anchors.py` fails when an anchor in this file or in that one no longer points at the code it describes, and CI runs it on every push.

## Numbers and money

### Binary floating point

A binary floating-point number is a finite sum of halves: $1/2$, $1/4$, $1/8$, and so on. One tenth is not a finite sum of those pieces, so `0.1` in a `DOUBLE` is the nearest available binary fraction, not exactly one tenth.

**Where:** `src/decimal.rs:1-7` - amount comments say money must not round-trip through `f64`; `src/lib.rs:163-180` maps money columns to DuckDB `DECIMAL`, not `DOUBLE`.

**What breaks if it is wrong:** 1. The file carries `0.10`, `0.20`, `0.30`, and `1500.10`. 2. By hand, `0.10 + 0.20 + 0.30 = 0.60`, and `0.60 + 1500.10 = 1500.70`. 3. Stored as binary floats, those decimal values are approximations, and the old total was `1500.7000000000003`. 4. A reconciliation query can fail an equality check or show a strange cent-level tail even though the wire values look ordinary.

**Caught by:** `test/sql/quackiso.test:134-149` asserts the `SUM(amount)` is `1500.70000` and exactly equals `1500.70`; `decimal::tests::exact_where_float_is_not` in `src/decimal.rs:89-97` checks the scaled representation of `0.1` and `1500.10`.

### Scaled integer amounts

A scaled integer stores a decimal by removing the decimal point and remembering the scale. With scale 5, `1500.10` is stored as `150010000`: the last five digits are the fractional part, so addition is integer addition.

**Where:** `src/decimal.rs:13-26` defines scale 5 and parses wire text into an `i128`; `src/model.rs:233-234`, `src/pacs008.rs:176-177`, and `src/pain001.rs:209-210` store amounts as scaled `i128` values in rows.

**What breaks if it is wrong:** 1. A parser reads the text amount and converts through a float. 2. The amount loses exact decimal identity before DuckDB ever sees it. 3. `SUM`, equality, and grouping operate on the rounded value. 4. The SQL result is plausible enough to pass a glance and wrong enough to matter.

**Caught by:** `decimal::tests::shapes_seen_in_real_messages` in `src/decimal.rs:99-109` checks common wire shapes, trimming, signs, `.5`, and five fractional digits.

### `DECIMAL(38,5)` and 128-bit storage

`DECIMAL(width, scale)` means a fixed-point number with at most `width` total digits and `scale` digits after the decimal point. DuckDB stores `DECIMAL(38,5)` in a 128-bit integer; `DECIMAL(18,5)` is only a 64-bit decimal class, and it cannot hold a legal ISO amount with 18 integer digits once the five scale digits are appended.

**Where:** `src/decimal.rs:9-17` states the ISO 18-significant-digit requirement and the `DECIMAL(38,5)` choice; `src/lib.rs:176-180` declares money columns with `decimal::WIDTH` and `decimal::SCALE`; `src/lib.rs:234-237` writes that column as `i128`.

**What breaks if it is wrong:** 1. A legal amount arrives as `123456789012345678`. 2. At scale 5, the stored integer must be `12345678901234567800000`. 3. A 64-bit decimal representation cannot hold that integer. 4. The scan either errors on a legal file or silently switches to a less exact representation.

**Caught by:** `test/sql/quackiso.test:151-157` asserts that `123456789012345678.00000` survives; `decimal::tests::eighteen_integer_digits_fit` in `src/decimal.rs:111-118` checks the scaled integer directly.

### Five fractional digits

Scale 5 means the system keeps five digits after the decimal point. Scale 2 is enough for cents, but ISO 20022 amounts are not just card charges or ledgers in a two-decimal currency; real files can carry five fractional digits.

**Where:** `src/decimal.rs:13-19` fixes the scale at 5; `testdata/camt053_decimal_sample.xml:40-44` records the real `5013090.23491` shape; `test/sql/quackiso.test:159-165` asserts that value comes out unchanged.

**What breaks if it is wrong:** 1. A message carries `5013090.23491`. 2. A scale-2 parser has no exact place for `491`. 3. It must reject, round, or truncate. 4. Rejecting loses a readable bank file; rounding or truncating changes money.

**Caught by:** `test/sql/quackiso.test:159-165` checks the five-decimal amount; `decimal::tests::precision_loss_is_refused_but_padding_is_not` in `src/decimal.rs:121-125` rejects a sixth meaningful digit while accepting trailing zeros.

### Amount errors instead of NULL

SQL `NULL` means “missing”, not “bad but close enough”. Aggregates such as `SUM` ignore `NULL`, so turning a malformed amount into `NULL` can return a total that looks valid and is missing a row.

**Where:** `src/decimal.rs:21-25` documents `Err` instead of silent `None`; `src/model.rs:251-272`, `src/pacs008.rs:191-195`, and `src/pain001.rs:220-224` propagate amount parse errors into the scan.

**What breaks if it is wrong:** 1. One row says `<Amt>12.34.56</Amt>`. 2. The parser stores `NULL` for that amount and continues. 3. `SUM(amount)` ignores the row. 4. The query exits 0 with a smaller total and no visible sign that money disappeared.

**Caught by:** `test/sql/quackiso.test:167-171` expects an error for `camt053_bad_amount.xml`; `decimal::tests::malformed_is_an_error_not_a_null` in `src/decimal.rs:128-136` rejects empty, alphabetic, comma, and double-dot amounts.

## Streaming

### Pull-based XML events

A pull parser gives the program the next XML event only when the program asks: start tag, text, end tag, end of file. That is different from building a document tree, where the whole XML file is loaded into nested objects before the first row can be returned.

**Where:** `src/stream.rs:40-89`, `src/pacs008.rs:267-323`, and `src/pain001.rs:307-363` loop on `read_event_into` and return one row at a time; `src/model.rs:289-291` marks the eager `flatten` path as test-only, not the extension scan path.

**What breaks if it is wrong:** 1. A 1.7 GB statement is parsed into a tree. 2. The process needs memory proportional to the whole file plus deserialized objects. 3. DuckDB has no row to consume until that tree exists. 4. Large statements fail or swap before SQL sees the first entry.

**Caught by:** `test/sql/quackiso.test:8-30` exercises the streamed camt rows, but there is nothing yet that measures the memory boundary.

### Row grain and carried context

The grain is the thing one SQL row represents. In camt files it is one booked `<Ntry>`; in pacs.008 and pain.001 it is one `<CdtTrfTxInf>`, with message, statement, or payment-group context carried beside that subtree.

**Where:** `src/lib.rs:3-10` names the three table functions and their row grain; `src/stream.rs:151-153` keeps statement context outside entry subtrees; `src/pain001.rs:5-12` explains that debtor context lives on `PmtInf` and must be carried down.

**What breaks if it is wrong:** 1. A pain.001 file has two `PmtInf` groups with different debtors. 2. The reader treats debtor as a transaction-local field or forgets to reset group context. 3. Rows inherit the wrong payer or lose it. 4. SQL groups payments under the wrong account.

**Caught by:** `test/sql/quackiso.test:91-124` asserts three pain.001 transaction rows, debtor context by payment group, requested execution dates, and group-level `ChrgBr` inheritance.

### Batch-sized chunks and `O(batch)` memory

`O(batch)` here means the live output rows are bounded by one DuckDB vector batch: at most 2048 flattened rows, plus the XML event buffer and the one entry or transaction subtree currently being copied. It does not mean the parser has loaded the file; the repo records a 1.7 GB statement reading in about 2 MB resident memory (`README.md:84-85`).

**Where:** `src/lib.rs:41-42` sets `VECTOR_SIZE` to 2048; `src/lib.rs:103-127` fills a `Vec` until that size or end-of-file; `src/lib.rs:285-289` writes that batch and tells DuckDB the row count.

**What breaks if it is wrong:** 1. `pull_batch` keeps appending until a file ends. 2. A large statement creates a huge `Vec<Row>`. 3. DuckDB still receives rows only after the file drains. 4. Memory follows file size instead of the output chunk.

**Caught by:** nothing yet for exact chunk size or resident memory; the SQL tests verify row contents, not the memory bound.

## XML

### Elements, attributes, and namespaces

An XML element is a named container like `<Amt>18500.75</Amt>`. An attribute is a key-value attached to the start tag, such as `Ccy="EUR"`; a namespace qualifies names so different vocabularies can share words without collision.

**Where:** `src/model.rs:93-100` maps amount text and the `@Ccy` attribute separately; `src/model.rs:6-8` notes that quick-xml serde matches local tag names for default namespaces; `src/stream.rs:194-199` strips prefixes to local names.

**What breaks if it is wrong:** 1. The reader treats attributes as child elements and never reads `Ccy`. 2. It treats `{namespace}Amt` as a different field from `Amt`. 3. Amounts still appear but currencies or whole transactions are `NULL`. 4. A result set looks populated while losing the fields needed to interpret the money.

**Caught by:** `test/sql/quackiso.test:23-30` checks amounts in camt rows; `test/sql/quackiso.test:58-69` checks amount, currency, and BICFI values in a prefixed pacs.008 file.

### Namespace-prefixed subtrees

A namespace prefix is the short name before the colon: in `<Doc:CdtTrfTxInf>`, `Doc` is the prefix and `CdtTrfTxInf` is the local element name. This reader copies one transaction subtree into a synthetic unprefixed root before deserializing it, so every copied start and end tag must be normalised the same way.

**Where:** `src/pacs008.rs:325-329` describes the prefixed-subtree failure; `src/pacs008.rs:330-374` rewrites copied start, empty, and end tags to local names while preserving attributes; `src/stream.rs:91-98` does the same for camt entries.

**What breaks if it is wrong:** 1. The source has `<Doc:CdtTrfTxInf>`. 2. The copied buffer starts with synthetic `<CdtTrfTxInf>`. 3. The copied close tag remains `</Doc:CdtTrfTxInf>`. 4. The buffer is not well-formed XML because the root name does not match its close tag, and deserialization rejects it.

**Caught by:** `test/sql/quackiso.test:55-62` reads `pacs008_prefixed_sample.xml` and expects the transaction, UETR, amount, and currency.

### No XSD validation

Ill-formed XML is not XML: tags do not nest, a close tag does not match, or the file ends inside an element. Schema-invalid XML is still XML, but it does not satisfy a particular XSD; quackiso rejects ill-formed input and bad amounts, but deliberately does not run XSD validation before reading.

**Where:** `docs/adr/0003-no-xsd-validation.md:20-38` gives the blocker: real corpus defects were reader-tolerance bugs, the test corpus spans roughly fifteen schemas, and `libxml` would add a C dependency across native and WASM builds; `docs/adr/0003-no-xsd-validation.md:46-52` states what is still rejected.

**What breaks if it is wrong:** 1. The extension validates against the wrong one of many ISO 20022 schemas. 2. A bank file that has readable fields but a version or wrapper variation is refused before extraction. 3. Users get no SQL rows even though the data the reader needs is present. 4. The code optimises for rejecting inputs when the real bugs were mostly the reader being too strict.

**Caught by:** `test/sql/quackiso.test:32-39` catches later camt party/account shapes; `test/sql/quackiso.test:55-62` catches prefixed pacs.008; `test/sql/quackiso.test:107-124` catches pain.001 date wrapping and group-level fields.

## Dates and times

### `DATE` versus `TIMESTAMP`

A `DATE` is a calendar day. A `TIMESTAMP` is an instant or local date-time with hours, minutes, seconds, and fractional seconds, so `2019-01-23` and `2023-10-01T13:37:14.000Z` cannot both be faithfully described as a date-only value.

**Where:** `src/temporal.rs:3-7` lists the mixed wire shapes and says date-times become UTC-normalised timestamps; `src/lib.rs:163-165` states that dates keep wire precision; `src/lib.rs:302-318` makes camt booking/value dates `Col::Stamp`, while `src/lib.rs:351-359` and `src/lib.rs:400-405` use `Col::Date` for settlement and requested execution dates.

**What breaks if it is wrong:** 1. A timestamp value is forced into a `DATE` column. 2. The time and offset are thrown away. 3. Two payments on the same day but different instants become indistinguishable. 4. SQL date arithmetic may still run, but it runs on truncated data.

**Caught by:** `test/sql/quackiso.test:71-89` asserts camt date columns are `TIMESTAMP` and support timestamp arithmetic; `test/sql/quackiso.test:107-114` asserts pain requested execution dates are `DATE`.

### UTC offsets and normalisation

A UTC offset says how far the written time is from UTC: `+01:00` means local time is one hour ahead of UTC. Normalising to UTC means subtracting that offset so different textual representations of the same instant store the same timestamp.

**Where:** `src/temporal.rs:59-61` defines timestamps as microseconds since the Unix epoch normalised to UTC; `src/temporal.rs:103-121` handles `Z`, `+hh:mm`, and `-hh:mm` offsets.

**What breaks if it is wrong:** 1. A file says `1970-01-01T01:00:00+01:00`. 2. The parser stores one hour after the epoch instead of subtracting the offset. 3. The same instant written as `1970-01-01T00:00:00Z` no longer compares equal. 4. Ordering across banks or time zones is wrong.

**Caught by:** `temporal::tests::timestamps_normalise_to_utc` in `src/temporal.rs:141-155` checks `Z`, `+01:00`, `-01:00`, fractional seconds, and real corpus shapes.

### DuckDB date and timestamp integers

DuckDB `DATE` values are stored as days since `1970-01-01`. DuckDB `TIMESTAMP` values are stored as microseconds since `1970-01-01T00:00:00`, so the writer must emit an `i32` for dates and an `i64` for timestamps.

**Where:** `src/temporal.rs:49-61` documents and returns those physical values; `src/lib.rs:234-235` instantiates `write_date` as `i32` and `write_timestamp` as `i64`; `src/lib.rs:326-331` and `src/lib.rs:376-379` feed parsed temporal integers into output vectors.

**What breaks if it is wrong:** 1. The parser returns a formatted string or the wrong integer unit. 2. The vector writer places bytes DuckDB interprets as a date or timestamp. 3. SQL shows nonsense dates, or arithmetic returns nonsense intervals. 4. The error appears in query results, not at the XML boundary.

**Caught by:** `temporal::tests::epoch_and_dates` in `src/temporal.rs:131-139` checks day counts and invalid dates; `temporal::tests::timestamps_normalise_to_utc` in `src/temporal.rs:141-155` checks microsecond counts.

## DuckDB extension mechanics

### Table functions: `bind`, `init`, and `func`

A DuckDB table function is a function that appears in `FROM` and returns rows. `bind` decides the schema and permanent scan inputs, `init` creates per-scan state, and `func` is called repeatedly to fill the next output chunk.

**Where:** `src/lib.rs:245-297` generates the three table functions; `src/lib.rs:266-270` declares columns and resolves files in `bind`; `src/lib.rs:273-276` creates scan state in `init`; `src/lib.rs:279-290` pulls and writes the next batch in `func`.

**What breaks if it is wrong:** 1. Parsing happens in `bind`. 2. A bad or remote path fails before the scan, but a huge local file is also read before DuckDB asks for rows. 3. The query cannot stream, cancel cleanly between chunks, or keep memory bounded. 4. Bind-time errors and scan-time errors become confused.

**Caught by:** `test/sql/quackiso.test:173-185` checks bind-time path errors; `test/sql/quackiso.test:187-193` checks glob/local file resolution and `source_file` output.

### Vectors, chunks, and validity masks

A DuckDB output chunk is a small block of rows. Each column in that chunk is a vector, and each vector has a validity mask saying which row positions are `NULL`; setting a value and setting nullness are separate operations.

**Where:** `src/lib.rs:191-203` writes text vectors with `insert` or `set_null`; `src/lib.rs:206-237` writes numeric vectors through raw slices and calls `set_null` for missing values; `src/lib.rs:289` sets the chunk length after writing.

**What breaks if it is wrong:** 1. A missing optional XML field is left as whatever bytes were in the vector. 2. The validity mask is not marked null. 3. DuckDB treats the slot as a real empty string, zero, old value, or invalid decimal. 4. SQL filters and aggregates operate on invented data.

**Caught by:** `test/sql/quackiso.test:78-82` checks the exposed DuckDB types, and `test/sql/quackiso.test:23-30`, `58-69`, and `107-124` exercise text, decimal, date, and inherited fields through vectors; there is nothing yet that directly asserts a particular missing field is `NULL`.

### No remote paths

A local path is opened by this process. A DuckDB remote URI such as `s3://...` or `https://...` should be opened by DuckDB's own filesystem because that is where secrets and session settings live. At the extension boundary, that filesystem is reached through the DuckDB C extension API: a table of function pointers plus opaque handles, not normal Rust methods.

**Where:** `src/lib.rs:130-159` rejects URI schemes while allowing Windows drive letters; `docs/adr/0002-no-remote-paths.md:12-18` lists the raw filesystem calls; `docs/adr/0002-no-remote-paths.md:34-54` explains the blocker: remote opens need an active transaction, `duckdb-rs` hides the raw `duckdb_function_info`, and wrapping DuckDB's output chunk is not public.

**What breaks if it is wrong:** 1. The extension caches a filesystem from a private connection. 2. A remote open tries to resolve secrets without the executing query's active transaction and fails with `TransactionContext::ActiveTransaction called without active transaction`. 3. The fix needs the scan callback's raw `duckdb_function_info`, so the safe `VTab` wrapper is no longer enough. 4. Hand-written C callbacks would then reimplement chunk sizing, string assignment, and validity masks in `unsafe` for a feature not needed to parse local files.

**Caught by:** `test/sql/quackiso.test:173-185` asserts `s3://` is refused with a clear message and `Z:/...` is treated as a Windows path, not a URI.

## Rust and FFI

### `unsafe` slices and borrows

`unsafe` marks code where Rust cannot prove the memory rules; it does not mean the rules stop applying. A borrow is a temporary loan of access: while a mutable slice borrowed from a vector is alive, the code cannot also call methods that touch the same vector.

**Where:** `src/lib.rs:206-228` puts the raw numeric slice in an inner scope before calling `set_null`; `src/stream.rs:43-46` converts XML events into owned actions so the borrow of `self.buf` ends before calling another `&mut self` method.

**What breaks if it is wrong:** 1. The code keeps `let slice = v.as_mut_slice()` alive. 2. It calls `v.set_null(i)` while the mutable slice still borrows the same vector. 3. Safe Rust rejects the compile because two mutable accesses overlap. 4. Forcing it with raw pointers would make it possible to write through a stale slice after DuckDB changed vector metadata.

**Caught by:** nothing yet at runtime; this is mainly caught by the Rust compiler. The SQL coverage in `test/sql/quackiso.test:78-82` and decimal/date unit tests exercise the writer after it compiles.
