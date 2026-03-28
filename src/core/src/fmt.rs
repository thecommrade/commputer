//! Human-readable $COMME amount formatting and parsing.
//! COMME has 8 decimal places (like satoshis to BTC).

use crate::token::UNITS_PER_COMME;

const SYMBOL: &str = "COMME";

/// Format a raw amount (smallest unit) to human-readable "1.5 COMME".
/// Trailing zeros are trimmed for readability.
pub fn format_comme(raw: u64) -> String {
    let whole = raw / UNITS_PER_COMME;
    let frac = raw % UNITS_PER_COMME;
    if frac == 0 {
        format!("{} {}", whole, SYMBOL)
    } else {
        let frac_str = format!("{:08}", frac);
        let trimmed = frac_str.trim_end_matches('0');
        format!("{}.{} {}", whole, trimmed, SYMBOL)
    }
}

/// Format with exact 8 decimal places (no trimming).
pub fn format_comme_exact(raw: u64) -> String {
    let whole = raw / UNITS_PER_COMME;
    let frac = raw % UNITS_PER_COMME;
    format!("{}.{:08} {}", whole, frac, SYMBOL)
}

/// Parse a human-readable amount string to raw units.
/// Accepts: "1.5", "1.5 COMME", "0.00000001"
pub fn parse_comme(input: &str) -> Result<u64, ParseError> {
    let s = input.trim().trim_end_matches(SYMBOL).trim();

    if s.is_empty() {
        return Err(ParseError::Empty);
    }

    if let Some(dot_pos) = s.find('.') {
        let whole_str = &s[..dot_pos];
        let frac_str = &s[dot_pos + 1..];

        if frac_str.len() > 8 {
            return Err(ParseError::TooManyDecimals);
        }

        let whole: u64 = if whole_str.is_empty() {
            0
        } else {
            whole_str.parse().map_err(|_| ParseError::InvalidNumber)?
        };

        let padded = format!("{:0<8}", frac_str);
        let frac: u64 = padded.parse().map_err(|_| ParseError::InvalidNumber)?;

        whole
            .checked_mul(UNITS_PER_COMME)
            .and_then(|w| w.checked_add(frac))
            .ok_or(ParseError::Overflow)
    } else {
        let val: u64 = s.parse().map_err(|_| ParseError::InvalidNumber)?;
        if input.contains(SYMBOL) {
            val.checked_mul(UNITS_PER_COMME).ok_or(ParseError::Overflow)
        } else {
            Ok(val)
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum ParseError {
    Empty,
    InvalidNumber,
    TooManyDecimals,
    Overflow,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::Empty => write!(f, "empty input"),
            ParseError::InvalidNumber => write!(f, "invalid number"),
            ParseError::TooManyDecimals => write!(f, "too many decimal places (max 8)"),
            ParseError::Overflow => write!(f, "amount overflow"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_whole() {
        assert_eq!(format_comme(100_000_000), "1 COMME");
        assert_eq!(format_comme(0), "0 COMME");
        assert_eq!(format_comme(1_000_000_000), "10 COMME");
    }

    #[test]
    fn format_fractional() {
        assert_eq!(format_comme(150_000_000), "1.5 COMME");
        assert_eq!(format_comme(1), "0.00000001 COMME");
        assert_eq!(format_comme(100_000_001), "1.00000001 COMME");
    }

    #[test]
    fn parse_decimal() {
        assert_eq!(parse_comme("1.5"), Ok(150_000_000));
        assert_eq!(parse_comme("0.00000001"), Ok(1));
        assert_eq!(parse_comme("1.5 COMME"), Ok(150_000_000));
    }

    #[test]
    fn parse_whole_with_symbol() {
        assert_eq!(parse_comme("10 COMME"), Ok(1_000_000_000));
    }

    #[test]
    fn parse_too_many_decimals() {
        assert_eq!(parse_comme("1.123456789"), Err(ParseError::TooManyDecimals));
    }

    #[test]
    fn parse_empty() {
        assert_eq!(parse_comme(""), Err(ParseError::Empty));
    }

    #[test]
    fn roundtrip() {
        let original = 123_456_789u64;
        let formatted = format_comme(original);
        let parsed = parse_comme(&formatted).unwrap();
        assert_eq!(parsed, original);
    }
}
