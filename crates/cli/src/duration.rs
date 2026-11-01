//! Durations on the command line (`--expires 1h`).

use std::time::Duration;

use bucketbridge_core::ops::{MAX_PRESIGN_EXPIRY, MIN_PRESIGN_EXPIRY};

/// Parses a duration: a plain number of seconds (`90`), or one or more
/// groups of a number and a unit, `s`, `m`, `h` or `d` (`30s`, `15m`, `1h`,
/// `7d`, `1h30m`; groups add up). Signs, fractions, spaces and other units
/// are rejected.
pub fn parse_duration(text: &str) -> Result<Duration, String> {
    let invalid = || {
        format!(
            "invalid duration '{text}': use seconds (90) or a number with a unit s, m, h or d (30s, 15m, 1h, 7d, 1h30m)"
        )
    };
    if text.is_empty() {
        return Err(invalid());
    }
    if text.bytes().all(|b| b.is_ascii_digit()) {
        return text
            .parse::<u64>()
            .map(Duration::from_secs)
            .map_err(|_| invalid());
    }
    let mut total: u64 = 0;
    let mut digits = String::new();
    for c in text.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        let unit: u64 = match c {
            's' => 1,
            'm' => 60,
            'h' => 60 * 60,
            'd' => 24 * 60 * 60,
            _ => return Err(invalid()),
        };
        if digits.is_empty() {
            return Err(invalid());
        }
        let value: u64 = digits.parse().map_err(|_| invalid())?;
        total = value
            .checked_mul(unit)
            .and_then(|seconds| total.checked_add(seconds))
            .ok_or_else(invalid)?;
        digits.clear();
    }
    if !digits.is_empty() {
        // A trailing number without a unit after other groups ("1h30").
        return Err(invalid());
    }
    Ok(Duration::from_secs(total))
}

/// Parses the lifetime of a presigned URL: a duration from 1 second to 7 days.
pub fn parse_expiry(text: &str) -> Result<Duration, String> {
    let duration = parse_duration(text)?;
    if !(MIN_PRESIGN_EXPIRY..=MAX_PRESIGN_EXPIRY).contains(&duration) {
        return Err(format!(
            "'{text}' is out of range: a presigned URL is valid for 1s to 7d ({} seconds)",
            MAX_PRESIGN_EXPIRY.as_secs()
        ));
    }
    Ok(duration)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(text: &str) -> u64 {
        parse_duration(text).unwrap().as_secs()
    }

    #[test]
    fn units_and_groups() {
        assert_eq!(secs("30s"), 30);
        assert_eq!(secs("15m"), 900);
        assert_eq!(secs("1h"), 3600);
        assert_eq!(secs("7d"), 604_800);
        assert_eq!(secs("1h30m"), 5400);
        assert_eq!(secs("1d2h3m4s"), 93_784);
        assert_eq!(secs("90"), 90);
        assert_eq!(secs("0"), 0);
        assert_eq!(secs("007m"), 420);
    }

    #[test]
    fn malformed_durations_are_rejected() {
        for text in [
            "",
            "h",
            "1.5h",
            "-1h",
            "+1h",
            "1 h",
            " 1h",
            "1h ",
            "1H",
            "1w",
            "1h30",
            "m30",
            "1hh",
            "abc",
            "99999999999999999999",
            "99999999999999999999s",
            "999999999999999999d",
        ] {
            let err = parse_duration(text).unwrap_err();
            assert!(err.contains("invalid duration"), "{text:?}: {err}");
        }
    }

    #[test]
    fn presign_expiry_range() {
        assert_eq!(parse_expiry("1s").unwrap().as_secs(), 1);
        assert_eq!(parse_expiry("1").unwrap().as_secs(), 1);
        assert_eq!(parse_expiry("7d").unwrap().as_secs(), 604_800);
        assert_eq!(parse_expiry("604800s").unwrap().as_secs(), 604_800);
        for text in ["0s", "0", "7d1s", "8d", "604801"] {
            let err = parse_expiry(text).unwrap_err();
            assert!(err.contains("out of range"), "{text}: {err}");
        }
        assert!(
            parse_expiry("soon")
                .unwrap_err()
                .contains("invalid duration")
        );
    }
}
