# Streaming

Files are parsed as an event stream, one entry at a time. A 1.7 GB statement of
three million entries is read in under 2 MB of live heap and about 1 MB of added
resident memory; peak does not follow file size.

**That number is measured, not remembered.** `src/membound.rs` writes the
statement, runs the scan loop `read_iso20022` runs, and reads the peak back from
a tracking allocator and from the kernel:

```console
$ cargo test --release membound -- --ignored --nocapture
[membound] the documented statement: 3000000 rows, 3000000 entries, 1.73 GB on
disk -> peak live heap 1.73 MiB, peak RSS +1.15 MiB (process peak 6.87 MiB)
```

**It is a standalone parser figure.** The test binary loads no DuckDB, so this
is what one scan adds to its own process — `VmHWM`, reset immediately before the
parse — not a process total and not an increment over DuckDB. The heap half of
it is the same number on every machine tried; the resident half moves by a few
hundred KiB, and by more than that between runs, which is why the resident
figure is held to a ceiling and the heap to a band.

**Gzip costs a decoder, not a fraction of the file.** The same statement gzipped
parses in the same batch, plus one decoder's worth of fixed state: an input
buffer, an LZ77 window, huffman tables. That is 131,369 bytes, measured as the
difference and recorded as `GZIP_HEAP`, and nothing of it is per entry.

```console
$ cargo test --release --lib peak_does_not_follow_compression -- --nocapture
[membound] 32k entries: 32000 rows, 32000 entries, 18.4 MB on disk -> peak live heap 1.72 MiB, peak RSS +1.13 MiB (process peak 6.53 MiB)
[membound] 32k entries gzipped: 32000 rows, 32000 entries, 0.7 MB on disk -> peak live heap 1.85 MiB, peak RSS +0.13 MiB (process peak 6.66 MiB)
[membound] the decoder adds 131369 bytes
```

**A declared encoding costs two buffers.** A file that declares an encoding
other than UTF-8 is decoded as it is read, through 8 KiB of staging for the raw
bytes and 8 KiB for the decoded text, plus the decoded prefix the shape guard
holds. That is fixed by construction and does not follow the file; a UTF-8 file
pays none of it, and `src/membound.rs` measures UTF-8 only.

**What compression does change is which number bounds the subtree.** An entry
used to be capped by the file it arrived in: a 16 MiB `<Ntry>` needed 16 MiB on
disk. Gzipped it needs a hundredth of that, and the peak is still six times the
inflated entry — so the term to watch is the inflated size, which `ls` no longer
shows:

```console
$ cargo test --release --lib a_small_gzip_can_carry_a_large_subtree -- --nocapture
[membound] one 16.00 MiB entry gzipped: 200 rows, 200 entries, 0.0 MB on disk -> peak live heap 97.26 MiB, peak RSS +64.84 MiB (process peak 69.75 MiB)
```

Inside DuckDB the same query adds 9.0 MiB to a 57 MiB baseline on the machine
above — that one is host-dependent, which is why CI asserts a 16 MiB ceiling
rather than a number. What it does not track is the file: the same 9 MiB answers
for a 1.73 GB statement as for a small one, because the growth is DuckDB's own
per-chunk machinery settling and not the reader.
`scripts/measure_in_duckdb.py` is that second measurement, on the same generated
statement.

**Streaming means aggregating.** Both figures are for a query that consumes rows
and drops them — an aggregate, a filter, a `LIMIT`. Asking for the rows
themselves is a different budget and a measured one: returning all three million
rows of the documented statement costs 3,991 MiB, 443× the scan that produced
them. That is the result set, not the parser.

**Bounded is not independent of the input.** The peak is one output batch plus
the largest single subtree, and both terms move it:

| input | peak live heap |
| --- | --- |
| 8× the file, same entry shape | unchanged, 1.73 MiB |
| 4 KiB of remittance text per row | 9.68 MiB — 2048 rows are in flight at once |
| one 16 MiB `<Ntry>` | 97 MiB — a fat subtree is live as a copy, as a deserialized struct, and as a row |
| 24 files instead of 8, same 8 workers | 25 batches at most — 19–27 MiB by machine, never by glob |
| 20,000 entries copied verbatim out of the corpus | 1.64 MiB — real shapes, same bound |
| one entry of 32,000 transactions, read at transaction grain | 99 MiB, which is the entry's own subtree plus one batch |

Those rows are tests, not prose. They run on every `cargo test` and on every
push; the 1.7 GB reproduction and the in-DuckDB measurement run at full size
every Monday. The pathological single entry is the one input that can hurt —
memory follows the largest subtree at about six times its size, so a
hypothetical 300 MB `<Ntry>` is a 1.8 GB parse. Statements with millions of
ordinary entries, which do exist, are the case that is bounded.

**The five supplementary camt readers are bounded the same way**, and the last
row above is why they need their own measurement. Each walks the statement the
entry reader walks and then a cursor over the entry it was handed, and a cursor
is two integers: an entry of 32,000 transactions is 32,000 rows out, taken 2048
at a time. A `Vec` of every row an entry produces would pass every case that
has two transactions per entry and fail only here, so
`one_entry_of_many_transactions_costs_a_cursor_and_not_a_queue` measures the
difference against a plain entry scan of the same file and holds it to one
batch. `read_camt_balances` and `read_camt_statements` skip every `<Ntry>`
subtree and count it, which is why they are the cheapest of the six, the balance
reader at 0.90 MiB over a statement of any size.

**A glob is parsed in parallel, one worker per file.** The unit is the whole
file because XML has no safe split points — there is no way to start parsing a
statement in the middle, unlike a block-structured format such as OSM's PBF —
so a single document is always one sequential pass. Workers claim files from a
shared counter and hand vector-sized batches over a bounded channel, so memory
stays O(threads × batch) regardless of how many files the glob matched —
measured through DuckDB, four copies of the documented statement behind four
workers add 15.1 MiB to the baseline, against 9.0 MiB for one of them. Rows of
one file stay in order; files interleave, which is what `source_file` is for. A
malformed amount in any file still fails the whole query.

The default is one worker per file, capped at the machine's parallelism;
`threads := 1` forces the sequential scan, `threads := n` pins the pool, itself
capped at four times the machine's parallelism:

```sql
SELECT count(*), SUM(amount)
FROM read_iso20022('statements/*.xml', threads := 8);
```

Measured on 8 × 35 MB statements (320,000 entries, debug build): 28.5 s
sequential, 4.1 s with 8 workers — 6.9×, with identical totals.
