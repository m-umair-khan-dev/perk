//! Parsing and formatting of human-friendly durations ("90", "1h30m", "1.5 h", "1:30").

use std::fmt;
use std::time::Duration;

/// Longest custom session accepted: one week.
pub const MAX_MINUTES: u32 = 7 * 24 * 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    Empty,
    Invalid(String),
    OutOfRange,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Empty => write!(f, "enter a duration such as 45m or 1h30m"),
            ParseError::Invalid(s) => {
                write!(f, "'{s}' is not a valid duration (try 45m, 2h or 1h30m)")
            }
            ParseError::OutOfRange => write!(f, "duration must be between 1 minute and 7 days"),
        }
    }
}

impl std::error::Error for ParseError {}

/// Parse a duration typed by the user into whole minutes.
/// A bare number means minutes.
pub fn parse_minutes(input: &str) -> Result<u32, ParseError> {
    let s = input.trim().to_ascii_lowercase();
    if s.is_empty() {
        return Err(ParseError::Empty);
    }
    let invalid = || ParseError::Invalid(input.trim().to_string());

    let total_minutes: f64 = if let Some((h, m)) = s.split_once(':') {
        let h: u32 = h.trim().parse().map_err(|_| invalid())?;
        let m: u32 = m.trim().parse().map_err(|_| invalid())?;
        if m >= 60 {
            return Err(invalid());
        }
        f64::from(h) * 60.0 + f64::from(m)
    } else {
        let mut total = 0.0;
        let mut rest = s.as_str();
        let mut seen_unit_less = false;
        while !rest.trim_start().is_empty() {
            rest = rest.trim_start();
            let num_len = rest
                .find(|c: char| !(c.is_ascii_digit() || c == '.'))
                .unwrap_or(rest.len());
            if num_len == 0 {
                return Err(invalid());
            }
            let value: f64 = rest[..num_len].parse().map_err(|_| invalid())?;
            rest = rest[num_len..].trim_start();
            let unit_len = rest
                .find(|c: char| !c.is_ascii_alphabetic())
                .unwrap_or(rest.len());
            let factor = match &rest[..unit_len] {
                "" => {
                    if seen_unit_less {
                        return Err(invalid());
                    }
                    seen_unit_less = true;
                    1.0
                }
                "m" | "min" | "mins" | "minute" | "minutes" => 1.0,
                "h" | "hr" | "hrs" | "hour" | "hours" => 60.0,
                "d" | "day" | "days" => 1440.0,
                _ => return Err(invalid()),
            };
            total += value * factor;
            rest = &rest[unit_len..];
        }
        total
    };

    let minutes = total_minutes.round();
    if !(1.0..=f64::from(MAX_MINUTES)).contains(&minutes) {
        return Err(ParseError::OutOfRange);
    }
    Ok(minutes as u32)
}

/// "45 minutes", "1 hour", "1 h 30 min", "2 hours".
pub fn describe_minutes(minutes: u32) -> String {
    let (h, m) = (minutes / 60, minutes % 60);
    match (h, m) {
        (0, 1) => "1 minute".into(),
        (0, m) => format!("{m} minutes"),
        (1, 0) => "1 hour".into(),
        (h, 0) => format!("{h} hours"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// Compact remaining time for status text: "1 h 05 min", "12 min", "< 1 min".
pub fn format_remaining(d: Duration) -> String {
    // Round up so "59 s left" reads as "1 min", never "0 min".
    let minutes = d.as_secs().div_ceil(60);
    match minutes {
        0 => "< 1 min".into(),
        m if m < 60 => format!("{m} min"),
        m => format!("{} h {:02} min", m / 60, m % 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_forms() {
        let cases = [
            ("45", 45),
            ("45m", 45),
            ("45 min", 45),
            ("90 minutes", 90),
            ("2h", 120),
            ("2 hours", 120),
            ("1h30m", 90),
            ("1h 30m", 90),
            ("1.5h", 90),
            ("1:30", 90),
            ("0:05", 5),
            ("1d", 1440),
            ("  3H ", 180),
        ];
        for (input, want) in cases {
            assert_eq!(parse_minutes(input), Ok(want), "input {input:?}");
        }
    }

    #[test]
    fn rejects_garbage() {
        for input in ["abc", "5x", "h", "1:75", "1 2", "--5", "1..5h"] {
            assert!(
                matches!(parse_minutes(input), Err(ParseError::Invalid(_))),
                "input {input:?}"
            );
        }
        assert_eq!(parse_minutes("   "), Err(ParseError::Empty));
    }

    #[test]
    fn enforces_range() {
        assert_eq!(parse_minutes("0"), Err(ParseError::OutOfRange));
        assert_eq!(parse_minutes("8d"), Err(ParseError::OutOfRange));
        assert_eq!(parse_minutes("7d"), Ok(MAX_MINUTES));
    }

    #[test]
    fn describes_minutes() {
        assert_eq!(describe_minutes(1), "1 minute");
        assert_eq!(describe_minutes(30), "30 minutes");
        assert_eq!(describe_minutes(60), "1 hour");
        assert_eq!(describe_minutes(120), "2 hours");
        assert_eq!(describe_minutes(90), "1 h 30 min");
    }

    #[test]
    fn formats_remaining() {
        assert_eq!(format_remaining(Duration::ZERO), "< 1 min");
        assert_eq!(format_remaining(Duration::from_secs(59)), "1 min");
        assert_eq!(format_remaining(Duration::from_secs(45 * 60)), "45 min");
        assert_eq!(format_remaining(Duration::from_secs(65 * 60)), "1 h 05 min");
    }
}
