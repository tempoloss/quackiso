# 11. A path can be a list, and a wrong reader is named

Status: accepted

Decided with the 1.9.0 release. Two things the readers could not do and one
thing they did not say, all met at the same place: the argument that names the
files.

## Context

ADR 0008 left an inbox as two steps, "sniff to find out what is in the folder,
then audit the files that are messages", and gave no way to make the second step
from the first. The only argument a reader took was one string, so the files the
sniffer had judged readable had to be copied out of its result by hand into a
glob that happened to match them, or into a generated `UNION ALL` of one call per
file. A folder of a year's deliveries holds statements, pain.001 files, MT
messages and the acknowledgements beside them, and a glob over it fails on the
first one the reader cannot read (ADR 0008).

The obvious ways to feed one result into the next do not work on DuckDB 2.0.0.
A subquery as the argument is refused when the query is bound:

```text
Binder Error: Table function cannot contain subqueries
```

and so is a lateral call that passes the sniffer's `source_file` along:

```text
Binder Error: Table function "read_iso20022" does not support lateral join
column parameters - cannot use column "s.source_file" in this context.
The function only supports literals as parameters.
```

A session variable is a constant by the time the function is bound, and it is
accepted.

The second thing is the error a caller meets first. `read_pacs008` on a camt.053
statement fails with `no <FIToFICstmrCdtTrf> found - is this a pacs.008 credit
transfer?`, which is true and leaves the caller to find out what the file is.
`sniff_iso20022` knows what the file is, and the caller has to run it to find
out.

## Decision

**The path is a VARCHAR or a VARCHAR list.** Every table function that takes a
path declares the parameter as `ANY` and decides at bind time:

- a VARCHAR is one pattern, exactly as before;
- a list is one pattern per item, in the order written;
- a NULL, as the whole argument or as an item, is an error: `path is NULL`,
  `the path list holds a NULL`;
- an empty list is an error: `the path list is empty`. A list that selects nothing
  is a mistake and not a request for zero rows, as a glob that matches nothing
  already is;
- any other type is an error that names the type: `path must be VARCHAR or
  VARCHAR[], got INTEGER`. `read_iso20022(42)` used to read a file called `42`,
  because DuckDB cast the integer to the declared VARCHAR; with `ANY` there is no
  cast, and a number that is not a path is no longer treated as one.

Each pattern goes through the resolution a single path always had: a glob, a
literal that glob cannot compile, the refusals for a remote path (ADR 0002) and
for a folder. The results are concatenated in argument order and deduplicated on
the resolved string, keeping the first occurrence. A file named twice, whether
literally or by a glob and a literal that both reach it, is read once, because the
alternative is a `SUM` that counts it twice. The comparison is on the exact string
the resolution returns, with `\` already turned into `/`; two different spellings
of one file, such as a relative and an absolute path, are two files, because
deciding they are the same needs the file system and the same answer on every
platform is not available.

A list fails on the first pattern that matches nothing or the first file that
cannot be read, which is ADR 0008's rule for a glob applied to the list.

**The inbox recipe becomes one session.**

```sql
SET VARIABLE files = (
    SELECT list(source_file)
    FROM sniff_iso20022('inbox/*')
    WHERE reader = 'read_iso20022' AND error IS NULL
);
SELECT * FROM read_iso20022(getvariable('files'));
```

The sniffer is the filter: `reader` says which function a file belongs to and
`error` says that it cannot be read. `list()` over no rows is NULL, so an inbox
with nothing for that reader fails with `path is NULL` rather than reading
nothing.

**A scan error names the right function when the file belongs to another.** When
a scan of a file fails, both in the sequential path and in a worker, the message
gets ` - sniff_iso20022 reads this file as camt.053, use read_iso20022` appended
when the sniffer reads the file as something that has a reader and this is not
it. The error text in front is unchanged, so every substring that was matched
before still matches.

The hint is built by `sniff::reader_for`, which opens the file again, runs the
sniffer's own stream over it and takes the one row. It runs only after a scan has
failed, so a scan that works pays nothing, and a file that cannot be opened or
sniffed a second time gets no hint rather than a wrong one. Three cases are
exempt, because `use X` would be wrong there:

- the file is read by the function that failed, so a truncated statement given
  to `read_iso20022` is a damaged file and not a wrong choice;
- `read_camt_transactions`, `read_camt_balances`, `read_camt_amount_details` and
  `read_camt_remittance`, and any later `read_camt_*` reader, read what
  `read_iso20022` reads, so for them only a file that is not camt.052, camt.053
  or camt.054 gets the hint;
- `audit_addresses` reads every message and `sniff_iso20022` never aborts on a
  message it cannot read.

## Alternatives rejected

**`on_error := 'skip'`.** A named parameter that drops the files that fail would
make the inbox one call. It contradicts ADR 0001, where a malformed amount fails
the whole scan and there is no skip mode for errors, and ADR 0008, where the
readers judge loudly and the sniffer is the function that reports a file instead
of failing on it. The sniffer already is the skip mode, with its `error` column
saying why each file was left out; a reader that skipped silently would be the
`SUM` that misses a file with nothing to say that it did.

**A lateral call, `read_x(s.source_file)`.** The natural SQL, and DuckDB 2.0.0
refuses it for these table functions, with the error quoted above: the binder
says the function only supports literals as parameters.

**Two registrations, one VARCHAR and one VARCHAR list.** The fallback this was
written with, for the case that DuckDB refused `ANY` as a table-function
parameter or replaced one overload with the next. Neither happened: `ANY` was
accepted, and `duckdb_functions()` lists each reader once. One registration keeps
one place where the path rule lives, and one set of error messages for what is not
a path.

**Appending the hint to every error.** A malformed amount in a camt.053 read
through `read_camt_balances` is not helped by being told to use `read_iso20022`;
the hint would be advice to a caller who made no mistake.

## Consequences

The macro that generates the table functions is the only place the path is read:
`paths_of` decides what the argument is, `resolve_paths` turns patterns into
files, and `resolve_files` is unchanged. `threads` and the file-level parallelism
see a file list and nothing about how it was named.

A list of a thousand literal paths resolves a thousand times at bind, once each,
and the deduplication holds the resolved strings in a set. That is the size of
the list and not of any file.

The hint reads a file that already failed, which can be large; it is a second pass
at the cost of the first, on the error path only. `sniff::reader_of` is now
`pub(crate)` for it, so the sniffer's family-to-reader table stays the one place
that decides which function reads what.

A caller that passed a number or a BLOB as a path now gets an error. Nothing that
named a file did.
