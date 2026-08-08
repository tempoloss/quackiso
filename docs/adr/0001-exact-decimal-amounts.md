# 1. Amounts are exact decimals, and a malformed amount is an error

Status: accepted

Decided in `1f703cd` (2026-07-29), the release that added the exact `DECIMAL`
amounts. Written down as an ADR on 2026-07-30, when the decisions that until then
lived only in commit messages and module documentation were filed here; it also
fills the gap left by an ADR sequence that started at 0002.

## Context

ISO 20022 carries amounts as decimal strings: `<Amt Ccy="EUR">1500.10</Amt>`. A
reader has to choose a representation before anything else works.

Binary floating point cannot hold `1500.10`. It can hold `18500.75`, which is why
the problem hides in casual testing: a few amounts round-trip exactly and the rest
drift by fractions of a cent that accumulate in a `SUM` over a statement. In a tool
whose entire purpose is `SELECT SUM(amount) FROM read_iso20022(...)`, a total that is
almost right is worse than a query that fails.

`ActiveCurrencyAndAmount` permits 18 significant digits with up to 5 fraction
digits.

## Decision

Parse the wire text straight into an `i128` scaled by `10^5`, the same
representation DuckDB's `DECIMAL` uses, and expose the column as `DECIMAL(38,5)`.
The value never touches a float on the way in.

`i128` is the storage, not the contract. It reaches about `1.7 * 10^38` and the
column stops at `10^38 - 1`, so a 34-integer-digit amount scales into a value the
integer holds and the column cannot. That band is refused with the same message as
an overflow rather than written and read back as something else.

A text that is not a legal amount returns `Err` with the offending text, which the
table function turns into a failed query.

## Alternatives rejected

**`DOUBLE`.** The drift above. It is also the choice that is impossible to walk back
later: every downstream sum computed against a `DOUBLE` column has to be recomputed.

**`DECIMAL(18,5)`.** Physically a 64-bit integer, and it overflows on a legal
18-integer-digit amount once scaled by `10^5`. The width is not a matter of taste:
`38` is what keeps the full ISO range representable, and it is the maximum DuckDB
offers.

**Scale 2, "because money has cents".** Rejected on real data: the prog-nov corpus
contains `5013090.23491`, five fraction digits, in a production-shaped message. A
scale-2 column would have to reject or round it, and rounding a payment amount is
not a choice a reader gets to make.

**`Option<i128>` -- a malformed amount becomes `NULL`.** This is the quiet failure
mode, and it is the reason the signature returns `Result`. A `NULL` disappears from
a `SUM`: the query succeeds, the total is wrong, and nothing anywhere says so. A
failed query is loud, and loud is correct here.

## Consequences

One bad amount fails the whole scan rather than yielding the other rows, which is a
real cost for someone scanning a large corpus to see what is in it. That trade is
deliberate for money and is the opposite of the choice made for schema strictness in
ADR 0003.

Covered by `decimal::tests::exact_where_float_is_not`, which asserts the scaled sum
equals the exact decimal where the float comparison fails;
`eighteen_integer_digits_fit` for the width;
`a_value_i128_holds_but_the_column_does_not_is_refused` for the band above it;
`shapes_seen_in_real_messages` for wire
shapes, trimming, signs and `.5`; `precision_loss_is_refused_but_padding_is_not`; and
`malformed_is_an_error_not_a_null`. End to end, `test/sql/quackiso.test` asserts
`SUM(amount)` is exactly `1500.70000` and that reading
`testdata/camt053_bad_amount.xml` fails instead of returning a row.
