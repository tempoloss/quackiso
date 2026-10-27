# Recipes

Questions people ask of bank files, as SQL. Every `sql` block on this page is run
by `scripts/check_hello_world.py` against the fixtures in `testdata/`, so a
recipe that stops binding or stops returning rows fails the build and not a
reader; the one whose right answer is no row only has to run. The paths
(`statements/*.xml`, `payments/*.xml`, `mt/*.txt`, `inbox/*`) are placeholders:
point them at your own files.

## Read an inbox in two steps

A folder usually holds more than messages, and a reader fails loudly on what it
does not own. The sniffer lists what is there without failing, and a variable
carries the files it names into the reader
([ADR 0011](adr/0011-a-path-can-be-a-list.md)):

```sql
SET VARIABLE files = (
    SELECT list(source_file) FROM sniff_iso20022('inbox/*')
    WHERE reader = 'read_iso20022' AND error IS NULL
);
SELECT * FROM read_iso20022(getvariable('files'));
```

Change `read_iso20022` in both places to read another family. A scalar subquery
is refused as a table-function argument, and a variable is not.

## Split a batched entry

A batch posts as one `<Ntry>` whose amount is the total and whose payments are
inside it. The two grains join on the keys they share:

```sql
SELECT e.entry_ref, e.amount AS entry_total, e.transaction_count,
       t.end_to_end_id, t.amount, t.creditor_name
FROM read_iso20022('statements/*.xml') e
JOIN read_camt_transactions('statements/*.xml') t
  ON t.source_file = e.source_file
 AND t.statement_index = e.statement_index
 AND t.entry_index = e.entry_index
WHERE e.transaction_count > 1
ORDER BY e.entry_ref, t.entry_details_index, t.transaction_index;
```

## Reconcile one statement

Opening plus the booked entries, each signed by its own direction, equals
closing. One row per statement, driven from `read_camt_statements` so that a
statement with no balance at all is a row too, with `difference` NULL when the
statement states no opening or no closing balance, so a missing balance cannot
pass as a zero:

```sql
WITH balances AS (
    SELECT source_file, statement_index,
           max(CASE credit_debit WHEN 'DBIT' THEN -amount ELSE amount END)
               FILTER (WHERE balance_type = 'OPBD') AS opening,
           max(CASE credit_debit WHEN 'DBIT' THEN -amount ELSE amount END)
               FILTER (WHERE balance_type = 'CLBD') AS closing
    FROM read_camt_balances('statements/*.xml')
    GROUP BY source_file, statement_index
),
entries AS (
    SELECT source_file, statement_index,
           sum(CASE credit_debit WHEN 'DBIT' THEN -amount ELSE amount END) AS movement
    FROM read_iso20022('statements/*.xml')
    WHERE status = 'BOOK'
    GROUP BY source_file, statement_index
)
SELECT s.source_file, s.statement_index, b.opening,
       coalesce(e.movement, 0) AS movement, b.closing,
       b.opening + coalesce(e.movement, 0) - b.closing AS difference
FROM read_camt_statements('statements/*.xml') s
LEFT JOIN balances b
  ON b.source_file = s.source_file AND b.statement_index = s.statement_index
LEFT JOIN entries e
  ON e.source_file = s.source_file AND e.statement_index = s.statement_index
ORDER BY s.source_file, s.statement_index;
```

An account with no movements is a valid statement, so a statement with no entry
row has a movement of 0. An entry that is not a direct child of its statement
has no `statement_index` and joins to nothing, as
[the reference](reference.md#read_iso20022) says.

## Continuity across files

Does the opening balance of each statement equal the closing balance of the
previous one for the same account? `read_camt_statements` carries the sequence
number ([ADR 0012](adr/0012-the-statement-header-is-its-own-grain.md)), so the
previous statement is the one before it in `electronic_sequence`, and a gap in
the sequence is a statement that never arrived:

```sql
WITH balances AS (
    SELECT source_file, statement_index,
           max(CASE credit_debit WHEN 'DBIT' THEN -amount ELSE amount END)
               FILTER (WHERE balance_type = 'OPBD') AS opening,
           max(CASE credit_debit WHEN 'DBIT' THEN -amount ELSE amount END)
               FILTER (WHERE balance_type = 'CLBD') AS closing
    FROM read_camt_balances('statements/*.xml')
    GROUP BY source_file, statement_index
)
SELECT s.account_iban, s.electronic_sequence, s.source_file,
       b.opening, lag(b.closing) OVER w AS previous_closing,
       b.opening - lag(b.closing) OVER w AS difference,
       s.electronic_sequence - lag(s.electronic_sequence) OVER w AS sequence_step
FROM read_camt_statements('statements/*.xml') s
JOIN balances b USING (source_file, statement_index)
WINDOW w AS (PARTITION BY s.account_iban ORDER BY s.electronic_sequence)
ORDER BY s.account_iban, s.electronic_sequence;
```

`difference` is 0 and `sequence_step` is 1 for a statement that follows its
predecessor, and both are NULL for the first one of an account. The same
statement delivered in two files is the other half of the question:

```sql
-- an empty result is the answer: no statement arrived twice
SELECT account_iban, statement_id, electronic_sequence,
       count(DISTINCT source_file) AS files, list(DISTINCT source_file) AS source_files
FROM read_camt_statements('statements/*.xml')
GROUP BY account_iban, statement_id, electronic_sequence
HAVING count(DISTINCT source_file) > 1;
```

## MT940 beside camt.053

A bank that sends both leaves the same account in two formats. MT940 marks
direction as `C`, `D`, `RC` and `RD`, where a reversal of a credit is a debit,
and camt writes `CRDT` or `DBIT`. A full join on account, direction, value date
and amount shows what only one side has; `n` numbers equal entries so that two
identical payments on a day pair up one to one:

```sql
WITH mt AS (
    SELECT account AS iban, CAST(value_date AS DATE) AS value_date, amount,
           CASE credit_debit WHEN 'C' THEN 'CRDT' WHEN 'RC' THEN 'DBIT'
                             WHEN 'D' THEN 'DBIT' WHEN 'RD' THEN 'CRDT' END AS direction
    FROM read_mt940('mt/*.txt')
    WHERE entry_index IS NOT NULL
),
camt AS (
    SELECT account_iban AS iban, CAST(value_date AS DATE) AS value_date, amount,
           credit_debit AS direction
    FROM read_iso20022('statements/*.xml')
    WHERE status = 'BOOK'
),
numbered_mt AS (
    SELECT *, row_number() OVER (PARTITION BY iban, direction, value_date, amount) AS n FROM mt
),
numbered_camt AS (
    SELECT *, row_number() OVER (PARTITION BY iban, direction, value_date, amount) AS n FROM camt
)
SELECT coalesce(m.iban, c.iban) AS iban, coalesce(m.value_date, c.value_date) AS value_date,
       coalesce(m.direction, c.direction) AS direction,
       m.amount AS mt940_amount, c.amount AS camt_amount,
       CASE WHEN c.iban IS NULL THEN 'MT940 only'
            WHEN m.iban IS NULL THEN 'camt only' ELSE 'both' END AS found_in
FROM numbered_mt m
FULL JOIN numbered_camt c
  ON m.iban = c.iban AND m.direction = c.direction
 AND m.value_date = c.value_date AND m.amount = c.amount AND m.n = c.n
ORDER BY iban, value_date, direction;
```

Add `WHERE account = '...'` to the first CTE to compare one account.

## pain.001 payments the bank rejected

A pain.002 answers a pain.001 by the end-to-end id of each payment:

```sql
SELECT p.payment_info_id, p.end_to_end_id, p.debtor_name, p.creditor_name,
       p.amount, p.currency, s.status, s.reason_code, s.reason_info
FROM read_pain001('payments/*.xml') p
JOIN read_pain002('payments/*.xml') s
  ON s.original_end_to_end_id = p.end_to_end_id
WHERE s.status = 'RJCT'
ORDER BY p.payment_info_id, p.end_to_end_id;
```

A payment with no answer yet is not in this list. Replace `JOIN` with
`LEFT JOIN` and the `WHERE` with `s.status IS NULL` to list those instead.

## Invoice numbers

The number a cash application matches on is `Strd/RfrdDocInf/Nb`, one row per
invoice number:

```sql
SELECT entry_ref, transaction_index, structured_index, text AS invoice_number
FROM read_camt_remittance('statements/*.xml')
WHERE slot = 'REFERRED_DOCUMENT'
ORDER BY entry_index, transaction_index, remittance_index;
```

## Export to CSV or Excel

DuckDB writes the result of any of these to a file. These two are not run by the
check, because they write one:

```sh
duckdb -c "COPY (SELECT * FROM read_iso20022('statements/*.xml')) TO 'out.csv' (HEADER)"
duckdb -c "INSTALL excel; LOAD excel; COPY (SELECT * FROM read_iso20022('statements/*.xml')) TO 'out.xlsx' (FORMAT xlsx, HEADER true)"
```

The files are read and written on your machine and go nowhere else.
