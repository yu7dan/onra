//! Number formatting. Prices of one asset share a precision so venues line
//! up digit for digit and spreads are visible at a glance.

use rust_decimal::{Decimal, RoundingStrategy};

/// Decimal places for prices around `reference`:
/// ≥ 1000 → 2 (`95,123.45`), ≥ 1 → 4 (`71.3895`), below 1 → 4 significant
/// digits (`0.5234`, `0.00001234`).
pub fn price_dp(reference: Decimal) -> u32 {
    let abs = reference.abs();
    if abs >= Decimal::ONE_THOUSAND {
        2
    } else if abs >= Decimal::ONE || abs.is_zero() {
        4
    } else {
        // Count leading zeros after the decimal point, then keep 4 digits.
        let mut dp = 0;
        let mut x = abs;
        while x < Decimal::ONE && dp < 24 {
            x *= Decimal::TEN;
            dp += 1;
        }
        dp + 3
    }
}

pub fn price(value: Decimal, dp: u32) -> String {
    let rounded = value.round_dp_with_strategy(dp, RoundingStrategy::MidpointAwayFromZero);
    let s = format!("{rounded:.prec$}", prec = dp as usize);
    group_thousands(&s)
}

/// `+2.03%` / `−0.41%`, with a true minus sign (U+2212) so it matches the
/// plus in width and weight. The explicit sign keeps the column aligned.
pub fn percent(value: Decimal) -> String {
    let rounded = value.round_dp(2);
    let sign = if rounded.is_sign_negative() && !rounded.is_zero() { "−" } else { "+" };
    format!("{sign}{:.2}%", rounded.abs())
}

/// Split a formatted number into its significant part and the trailing
/// zeros padding it to a fixed precision: `71.2800` → (`71.28`, `00`).
/// Renderers dim the zeros so the digits that carry information stand out.
pub fn split_trailing_zeros(formatted: &str) -> (&str, &str) {
    if !formatted.contains('.') {
        return (formatted, "");
    }
    let trimmed = formatted.trim_end_matches('0');
    // Keep at least one decimal digit: `71.0000` → (`71.0`, `000`).
    let keep = if trimmed.ends_with('.') { trimmed.len() + 1 } else { trimmed.len() };
    formatted.split_at(keep.min(formatted.len()))
}

fn group_thousands(s: &str) -> String {
    let (sign, rest) = s.strip_prefix('-').map_or(("", s), |r| ("-", r));
    let (int, frac) = rest.split_once('.').map_or((rest, None), |(i, f)| (i, Some(f)));
    let mut grouped = String::with_capacity(int.len() + int.len() / 3);
    for (i, ch) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    match frac {
        Some(f) => format!("{sign}{grouped}.{f}"),
        None => format!("{sign}{grouped}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    #[test]
    fn precision_follows_magnitude() {
        assert_eq!(price_dp(d("95123.4")), 2);
        assert_eq!(price_dp(d("71.39")), 4);
        assert_eq!(price_dp(d("0.5234")), 4);
        assert_eq!(price_dp(d("0.00001234")), 8);
    }

    #[test]
    fn formats_prices() {
        assert_eq!(price(d("95123.456"), 2), "95,123.46");
        assert_eq!(price(d("71.39"), 4), "71.3900");
        assert_eq!(price(d("1234567.5"), 2), "1,234,567.50");
        assert_eq!(price(d("0.000012345"), 8), "0.00001235");
        assert_eq!(price(d("-1000"), 2), "-1,000.00");
    }

    #[test]
    fn formats_percent() {
        assert_eq!(percent(d("2.031")), "+2.03%");
        assert_eq!(percent(d("-2.031")), "−2.03%");
        assert_eq!(percent(d("-0.001")), "+0.00%");
    }

    #[test]
    fn splits_trailing_zeros() {
        assert_eq!(split_trailing_zeros("71.2800"), ("71.28", "00"));
        assert_eq!(split_trailing_zeros("71.0000"), ("71.0", "000"));
        assert_eq!(split_trailing_zeros("95,123.45"), ("95,123.45", ""));
        assert_eq!(split_trailing_zeros("100"), ("100", ""));
    }
}
