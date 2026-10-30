//! Formatting of numbers for display, by language: decimal comma and
//! non-breaking space as thousands separator in French, point and comma in
//! English.

use crate::i18n::{lang, Lang};

/// Decimal separator of the interface language.
pub fn decimal_sep() -> &'static str {
    match lang() {
        Lang::Fr => ",",
        Lang::En => ".",
    }
}

/// Integer with thousands separators: 1 234 567 (1,234,567 in English).
pub fn count(n: u64) -> String {
    let sep = match lang() {
        Lang::Fr => '\u{00A0}',
        Lang::En => ',',
    };
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(sep);
        }
        out.push(ch);
    }
    out
}

/// Readable real number: scientific notation for extreme magnitudes.
pub fn real(x: f64) -> String {
    let a = x.abs();
    if x == 0.0 {
        "0".to_owned()
    } else if !(1e-3..1e6).contains(&a) {
        format!("{x:.3e}")
    } else if a >= 100.0 {
        format!("{x:.1}")
    } else {
        format!("{x:.4}")
    }
    .replace('.', crate::format::decimal_sep())
}

/// Signed percentage: +0,012 % (+0.012 % in English).
pub fn percent(x: f64) -> String {
    format!("{:+.4} %", 100.0 * x).replace('.', crate::format::decimal_sep())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thousands() {
        assert_eq!(count(1_234_567), "1\u{00A0}234\u{00A0}567");
        assert_eq!(count(999), "999");
    }
}
