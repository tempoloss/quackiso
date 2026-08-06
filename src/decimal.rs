//! Exact decimal amounts.
//!
//! Money must not round-trip through `f64`. `18500.75` happens to be exact in
//! binary, `1500.10` is not, and a `SUM` over a statement drifts. ISO 20022
//! amounts are decimal strings on the wire, so they are converted straight to a
//! scaled integer — the same representation DuckDB's `DECIMAL` uses — and never
//! touch a float.
//!
//! `ActiveCurrencyAndAmount` allows 18 significant digits with up to 5 fraction
//! digits. A `DECIMAL(18,5)` would overflow on a legal 18-integer-digit value,
//! so the column is `DECIMAL(38,5)`, physically INT128, which cannot.

/// Fraction digits carried in the scaled integer. ISO 20022 permits 5.
pub const SCALE: u8 = 5;
/// Total digits of the DuckDB column. 38 is the maximum and leaves the full
/// 18-significant-digit ISO range representable after scaling.
pub const WIDTH: u8 = 38;

const POW10: [i128; SCALE as usize + 1] = [1, 10, 100, 1_000, 10_000, 100_000];

/// The largest value a DECIMAL(38,5) column holds. i128 reaches about 1.7x this,
/// so scaling can succeed on a value the column cannot store.
const MAX_SCALED: i128 = 10i128.pow(WIDTH as u32) - 1;

/// Parse an ISO 20022 amount into an integer scaled by `10^SCALE`.
///
/// Returns `Err` with the offending text rather than a silent `None`: a NULL
/// amount would vanish from a `SUM` and quietly produce a wrong total, which is
/// worse than a failed query.
pub fn scaled(text: &str) -> Result<i128, String> {
    let s = text.trim();
    if s.is_empty() {
        return Err("empty amount".into());
    }

    let (neg, digits) = match s.as_bytes()[0] {
        b'-' => (true, &s[1..]),
        b'+' => (false, &s[1..]),
        _ => (false, s),
    };

    let (int_part, frac_part) = match digits.split_once('.') {
        Some((i, f)) => (i, f),
        None => (digits, ""),
    };
    if int_part.is_empty() && frac_part.is_empty() {
        return Err(format!("not a number: {text:?}"));
    }

    // Trailing zeros beyond the supported scale carry no value, so drop them
    // before deciding whether the amount is too precise to represent.
    let frac = frac_part.trim_end_matches('0');
    if frac.len() > SCALE as usize {
        return Err(format!(
            "amount {text:?} has {} fraction digits; ISO 20022 allows at most {SCALE}",
            frac.len()
        ));
    }

    let mut value: i128 = 0;
    for part in [int_part, frac] {
        for &b in part.as_bytes() {
            if !b.is_ascii_digit() {
                return Err(format!("not a number: {text:?}"));
            }
            value = value
                .checked_mul(10)
                .and_then(|v| v.checked_add((b - b'0') as i128))
                .ok_or_else(|| format!("amount {text:?} is too large for DECIMAL({WIDTH},{SCALE})"))?;
        }
    }
    // Left-align the fraction to the fixed scale.
    value = value
        .checked_mul(POW10[SCALE as usize - frac.len()])
        .ok_or_else(|| format!("amount {text:?} is too large for DECIMAL({WIDTH},{SCALE})"))?;
    if value > MAX_SCALED {
        return Err(format!(
            "amount {text:?} is too large for DECIMAL({WIDTH},{SCALE})"
        ));
    }

    Ok(if neg { -value } else { value })
}

/// Parse an optional amount, keeping absence distinct from malformed input.
pub fn scaled_opt(text: Option<&String>) -> Result<Option<i128>, String> {
    match text {
        Some(t) if t.trim().is_empty() => Ok(None),
        Some(t) => scaled(t).map(Some),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_where_float_is_not() {
        // 1500.10 is not representable in binary floating point; scaled it is.
        assert_eq!(scaled("1500.10").unwrap(), 150_010_000);
        assert_eq!(scaled("0.1").unwrap(), 10_000);
        // three tenths summed exactly
        let sum: i128 = ["0.1", "0.1", "0.1"].iter().map(|s| scaled(s).unwrap()).sum();
        assert_eq!(sum, scaled("0.3").unwrap());
    }

    #[test]
    fn shapes_seen_in_real_messages() {
        assert_eq!(scaled("250000.00").unwrap(), 25_000_000_000);
        assert_eq!(scaled("100").unwrap(), 10_000_000);
        assert_eq!(scaled(" 75000.5 ").unwrap(), 7_500_050_000);
        assert_eq!(scaled("-600.00").unwrap(), -60_000_000);
        assert_eq!(scaled("+12.34").unwrap(), 1_234_000);
        assert_eq!(scaled(".5").unwrap(), 50_000);
        // five decimals is the ISO limit and must survive
        assert_eq!(scaled("1.23456").unwrap(), 123_456);
    }

    #[test]
    fn eighteen_integer_digits_fit() {
        // Legal per ISO (18 significant digits) and the reason the column is
        // DECIMAL(38,5) rather than DECIMAL(18,5), which is only 64 bits.
        assert_eq!(
            scaled("123456789012345678").unwrap(),
            12_345_678_901_234_567_800_000
        );
    }

    #[test]
    fn precision_loss_is_refused_but_padding_is_not() {
        assert!(scaled("1.234567").is_err());
        // trailing zeros are not precision
        assert_eq!(scaled("1.2300000").unwrap(), 123_000);
    }

    #[test]
    fn a_value_i128_holds_but_the_column_does_not_is_refused() {
        // 34 integer digits: 1.2e38 scaled, inside i128 and outside DECIMAL(38,5)
        assert!(scaled("1200000000000000000000000000000000.00000").is_err());
        // 33 integer digits and 5 fraction digits is exactly the column's ceiling
        assert!(scaled("999999999999999999999999999999999.99999").is_ok());
    }

    #[test]
    fn malformed_is_an_error_not_a_null() {
        assert!(scaled("").is_err());
        assert!(scaled("abc").is_err());
        assert!(scaled("1,234.00").is_err());
        assert!(scaled("1.2.3").is_err());
        assert_eq!(scaled_opt(None).unwrap(), None);
        assert_eq!(scaled_opt(Some(&"  ".to_string())).unwrap(), None);
    }
}
