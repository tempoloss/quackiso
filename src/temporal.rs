//! Date/time parsing for ISO 20022 values, without pulling in a date crate.
//!
//! Real messages mix `2019-01-23` (date only), `2023-10-01T13:37:14.000Z`
//! (UTC), `2015-03-10T18:43:50+00:00` (offset) and `20190123` (basic format) —
//! sometimes inside one corpus. Exposing those as VARCHAR pushes the problem to
//! the caller, so dates become DATE and date-times become TIMESTAMP, normalised
//! to UTC. Anything unparseable becomes NULL rather than a wrong number.

/// Days from 1970-01-01, Howard Hinnant's `days_from_civil`.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn num(s: &str) -> Option<i64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Leading calendar date: `YYYY-MM-DD` or `YYYYMMDD`. Returns (y, m, d, rest).
fn split_date(s: &str) -> Option<(i64, i64, i64, &str)> {
    let b = s.as_bytes();
    if b.len() >= 10 && b[4] == b'-' && b[7] == b'-' {
        let y = num(&s[0..4])?;
        let m = num(&s[5..7])?;
        let d = num(&s[8..10])?;
        Some((y, m, d, &s[10..]))
    } else if b.len() >= 8 && b[..8].iter().all(|c| c.is_ascii_digit()) {
        let y = num(&s[0..4])?;
        let m = num(&s[4..6])?;
        let d = num(&s[6..8])?;
        Some((y, m, d, &s[8..]))
    } else {
        None
    }
}

fn valid(m: i64, d: i64) -> bool {
    (1..=12).contains(&m) && (1..=31).contains(&d)
}

/// DuckDB DATE: days since 1970-01-01.
pub fn date_days(s: &str) -> Option<i32> {
    let s = s.trim();
    let (y, m, d, _) = split_date(s)?;
    if !valid(m, d) {
        return None;
    }
    i32::try_from(days_from_civil(y, m, d)).ok()
}

/// DuckDB TIMESTAMP: microseconds since 1970-01-01T00:00:00, normalised to UTC.
/// A date with no time component lands at midnight.
pub fn ts_micros(s: &str) -> Option<i64> {
    let s = s.trim();
    let (y, m, d, rest) = split_date(s)?;
    if !valid(m, d) {
        return None;
    }
    let mut micros = days_from_civil(y, m, d) * 86_400_000_000;

    // optional time, after 'T' or a space
    let rest = rest.strip_prefix('T').or_else(|| rest.strip_prefix(' ')).unwrap_or(rest);
    if rest.len() >= 5 && rest.as_bytes()[2] == b':' {
        let h = num(&rest[0..2])?;
        let mi = num(&rest[3..5])?;
        if h > 23 || mi > 59 {
            return None;
        }
        micros += (h * 3600 + mi * 60) * 1_000_000;
        let mut tail = &rest[5..];

        // seconds and fractional seconds
        if tail.len() >= 3 && tail.as_bytes()[0] == b':' {
            let sec = num(&tail[1..3])?;
            if sec > 60 {
                return None;
            }
            micros += sec * 1_000_000;
            tail = &tail[3..];
            if let Some(frac) = tail.strip_prefix('.') {
                let digits: String = frac.chars().take_while(|c| c.is_ascii_digit()).collect();
                if !digits.is_empty() {
                    // pad or truncate to microsecond precision
                    let mut us = digits.clone();
                    us.truncate(6);
                    while us.len() < 6 {
                        us.push('0');
                    }
                    micros += num(&us)?;
                    tail = &tail[1 + digits.len()..];
                }
            }
        }

        // zone: Z means UTC; an offset is subtracted to reach UTC
        let tail = tail.trim();
        if !tail.is_empty() && tail != "Z" && tail != "z" {
            let sign = match tail.as_bytes()[0] {
                b'+' => 1,
                b'-' => -1,
                _ => 0,
            };
            if sign != 0 && tail.len() >= 3 {
                let oh = num(&tail[1..3])?;
                let om = if tail.len() >= 6 && tail.as_bytes()[3] == b':' {
                    num(&tail[4..6])?
                } else if tail.len() >= 5 {
                    num(&tail[3..5]).unwrap_or(0)
                } else {
                    0
                };
                micros -= sign * (oh * 3600 + om * 60) * 1_000_000;
            }
        }
    }
    Some(micros)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_and_dates() {
        assert_eq!(date_days("1970-01-01"), Some(0));
        assert_eq!(date_days("2026-07-29"), Some(20663));
        // basic format, seen in older files
        assert_eq!(date_days("20260729"), Some(20663));
        assert_eq!(date_days("not-a-date"), None);
        assert_eq!(date_days("2026-13-01"), None);
    }

    #[test]
    fn timestamps_normalise_to_utc() {
        assert_eq!(ts_micros("1970-01-01"), Some(0));
        assert_eq!(ts_micros("1970-01-01T00:00:01"), Some(1_000_000));
        // fractional seconds are truncated to microseconds
        assert_eq!(ts_micros("1970-01-01T00:00:00.000001"), Some(1));
        assert_eq!(ts_micros("1970-01-01T00:00:00.000000001"), Some(0));
        // Z is UTC; a positive offset is behind UTC by that much
        assert_eq!(ts_micros("1970-01-01T01:00:00Z"), Some(3_600_000_000));
        assert_eq!(ts_micros("1970-01-01T01:00:00+01:00"), Some(0));
        assert_eq!(ts_micros("1970-01-01T00:00:00-01:00"), Some(3_600_000_000));
        // real shapes from the corpus
        assert!(ts_micros("2023-10-01T13:37:14.000Z").is_some());
        assert!(ts_micros("2015-03-10T18:43:50+00:00").is_some());
        assert!(ts_micros("2025-07-24T06:10:29.000000000").is_some());
    }
}
