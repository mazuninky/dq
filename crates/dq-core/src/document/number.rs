//! Decimal number literals: the exact value they spell, and whether an
//! `f64` holds it.
//!
//! One rule decides, for every reader and for jq: a literal with a fraction
//! or an exponent is a [`Value::Float`] when it parses to a finite `f64`,
//! has at most 17 significant digits, and is that `f64` correctly rounded
//! to its own number of digits — `0.1`, `1.10`, `1e3`, and the `%.17g`
//! spelling `0.10000000000000001` all qualify. Any other is a
//! [`Value::BigFloat`] carrying the literal: more digits than an `f64`
//! holds (`0.1000000000000000000000000001`), a 17-digit decimal that is not
//! the rounding of its `f64` (`0.10000000000000004`), or a value past the
//! `f64` range (`1e400`, `1e-400`).

use std::fmt;

use super::Value;

/// The parts of a decimal literal `[-+]digits[.digits][(e|E)[-+]digits]`,
/// with a digit somewhere in the mantissa (a `.` may lead or end it).
pub(crate) struct DecimalParts<'a> {
    negative: bool,
    int: &'a str,
    frac: Option<&'a str>,
    /// The exponent with its `e` / `E`: `e+400`.
    exponent: Option<&'a str>,
}

impl<'a> DecimalParts<'a> {
    /// `None` unless `literal` is a decimal literal.
    pub(crate) fn parse(literal: &'a str) -> Option<Self> {
        let (negative, rest) = match literal.as_bytes().first()? {
            b'-' => (true, &literal[1..]),
            b'+' => (false, &literal[1..]),
            _ => (false, literal),
        };
        let (mantissa, exponent) = match rest.find(['e', 'E']) {
            Some(at) => (&rest[..at], Some(&rest[at..])),
            None => (rest, None),
        };
        let (int, frac) = match mantissa.split_once('.') {
            Some((int, frac)) => (int, Some(frac)),
            None => (mantissa, None),
        };
        let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
        if !digits(int) || !frac.is_none_or(digits) || int.len() + frac.map_or(0, str::len) == 0 {
            return None;
        }
        if let Some(exponent) = exponent {
            let exponent = &exponent[1..];
            let unsigned = exponent.strip_prefix(['-', '+']).unwrap_or(exponent);
            if unsigned.is_empty() || !digits(unsigned) {
                return None;
            }
        }
        Some(Self {
            negative,
            int,
            frac,
            exponent,
        })
    }

    /// Whether the literal has neither a fraction nor an exponent.
    pub(crate) fn is_integer(&self) -> bool {
        self.frac.is_none() && self.exponent.is_none()
    }

    /// The literal as JSON writes it: no `+`, no leading zeros, a digit on
    /// both sides of a `.`; the exponent stays as written.
    pub(crate) fn json_spelling(&self) -> String {
        let int = self.int.trim_start_matches('0');
        let mut out = String::from(if self.negative { "-" } else { "" });
        out.push_str(if int.is_empty() { "0" } else { int });
        if let Some(frac) = self.frac {
            out.push('.');
            out.push_str(if frac.is_empty() { "0" } else { frac });
        }
        if let Some(exponent) = self.exponent {
            out.push_str(exponent);
        }
        out
    }
}

/// A decimal literal's value in normal form: sign, the significant digits
/// (leading and trailing zeros dropped), and the power of ten of the last
/// of them. `-0.0120e2` (-1.2) is `-`, `12`, `-1`. Built without
/// allocating unless the exponent has more than 30 digits.
struct Normal<'a> {
    negative: bool,
    int: &'a str,
    frac: &'a str,
    /// Leading zeros of `int` followed by `frac`.
    skip: usize,
    /// Significant digits; 0 for a zero.
    len: usize,
    exponent: Exponent,
}

/// The power of ten of a [`Normal`]'s last digit.
#[derive(Debug, PartialEq, Eq)]
enum Exponent {
    Small(i128),
    /// Past 30 digits, in decimal: sign and digits without leading zeros.
    Big(bool, String),
}

impl<'a> Normal<'a> {
    fn of(parts: &DecimalParts<'a>) -> Self {
        let frac = parts.frac.unwrap_or("");
        let all = || parts.int.bytes().chain(frac.bytes());
        let total = parts.int.len() + frac.len();
        let skip = all().take_while(|&b| b == b'0').count();
        if skip == total {
            return Self {
                negative: false,
                int: parts.int,
                frac,
                skip,
                len: 0,
                exponent: Exponent::Small(0),
            };
        }
        let trailing = frac
            .bytes()
            .rev()
            .chain(parts.int.bytes().rev())
            .take_while(|&b| b == b'0')
            .count();
        let len = total - skip - trailing;
        // Both fit: they count bytes of one literal.
        let adjust = i128::try_from(trailing).unwrap_or(i128::MAX)
            - i128::try_from(frac.len()).unwrap_or(i128::MAX);
        let (exp_negative, exp_digits) = match parts.exponent {
            Some(exponent) => {
                let exponent = &exponent[1..];
                match exponent.strip_prefix('-') {
                    Some(digits) => (true, digits),
                    None => (false, exponent.strip_prefix('+').unwrap_or(exponent)),
                }
            }
            None => (false, "0"),
        };
        let exp_digits = exp_digits.trim_start_matches('0');
        let exponent = if exp_digits.len() <= 30 {
            let magnitude = exp_digits.parse::<i128>().unwrap_or(0);
            Exponent::Small(if exp_negative { -magnitude } else { magnitude } + adjust)
        } else {
            Exponent::Big(exp_negative, add_small(exp_negative, exp_digits, adjust))
        };
        Self {
            negative: parts.negative,
            int: parts.int,
            frac,
            skip,
            len,
            exponent,
        }
    }

    fn digits(&self) -> impl Iterator<Item = u8> + '_ {
        self.int
            .bytes()
            .chain(self.frac.bytes())
            .skip(self.skip)
            .take(self.len)
    }

    fn same(&self, other: &Self) -> bool {
        if self.len == 0 || other.len == 0 {
            return self.len == other.len;
        }
        self.negative == other.negative
            && self.len == other.len
            && self.exponent == other.exponent
            && self.digits().eq(other.digits())
    }

    /// The power of ten of the first significant digit, when it is small.
    fn leading_exponent(&self) -> Option<i128> {
        match self.exponent {
            Exponent::Small(last) => Some(last + i128::try_from(self.len).ok()? - 1),
            Exponent::Big(..) => None,
        }
    }
}

/// `digits` (over 30 of them, so larger than `adjust` by far) with
/// `adjust` added to the signed number they spell: the digits of the sum,
/// whose sign is the same.
fn add_small(negative: bool, digits: &str, adjust: i128) -> String {
    let mut out: Vec<u8> = digits.bytes().rev().map(|b| b - b'0').collect();
    let mut rest = adjust.unsigned_abs();
    let grow = (adjust < 0) == negative;
    let mut carry = 0u8;
    for digit in &mut out {
        let step = u8::try_from(rest % 10).unwrap_or(0) + carry;
        rest /= 10;
        if grow {
            let sum = *digit + step;
            *digit = sum % 10;
            carry = sum / 10;
        } else if *digit >= step {
            *digit -= step;
            carry = 0;
        } else {
            *digit = *digit + 10 - step;
            carry = 1;
        }
    }
    if carry > 0 {
        out.push(carry);
    }
    while out.len() > 1 && out.last() == Some(&0) {
        out.pop();
    }
    out.iter().rev().map(|d| char::from(b'0' + d)).collect()
}

/// Whether two decimal literals spell the same number, at any size: `1.10`
/// is `1.1`, `1e3` is `1000`, `1e99999999999999999999` is `10e99999999999999999998`.
/// `false` when either is not a decimal literal.
pub(crate) fn same_decimal(a: &str, b: &str) -> bool {
    match (DecimalParts::parse(a), DecimalParts::parse(b)) {
        (Some(a), Some(b)) => Normal::of(&a).same(&Normal::of(&b)),
        _ => false,
    }
}

/// The value of a decimal literal: an `i64` integer is a [`Value::Int`],
/// a longer one a [`Value::BigInt`], a literal with a fraction or an
/// exponent as [`float_value`] decides. `None` for anything else.
pub(crate) fn number_value(literal: &str) -> Option<Value> {
    let parts = DecimalParts::parse(literal)?;
    if !parts.is_integer() {
        return Some(float_value(literal, &parts));
    }
    Some(match literal.parse::<i64>() {
        Ok(i) => Value::Int(i),
        Err(_) => Value::BigInt(parts.json_spelling()),
    })
}

/// The value of a decimal literal read as a float, integers too (a YAML
/// `!!float 18446744073709551617`). `None` for anything else.
pub(crate) fn float_literal_value(literal: &str) -> Option<Value> {
    DecimalParts::parse(literal).map(|parts| float_value(literal, &parts))
}

/// A [`Value::Float`] when an `f64` holds `literal` (the module rule), else
/// a [`Value::BigFloat`] carrying it.
fn float_value(literal: &str, parts: &DecimalParts<'_>) -> Value {
    if let Ok(f) = literal.parse::<f64>()
        && holds(&Normal::of(parts), f)
    {
        Value::Float(f)
    } else {
        Value::BigFloat(parts.json_spelling())
    }
}

/// Whether `f`, the `f64` nearest the literal `normal` is, holds it.
fn holds(normal: &Normal<'_>, f: f64) -> bool {
    if !f.is_finite() {
        return false;
    }
    if normal.len == 0 {
        return true;
    }
    if normal.len > 17 {
        return false;
    }
    // An `f64` keeps 15 significant digits through any normal value.
    if normal.len <= 15 && f.is_normal() {
        return true;
    }
    let mut spelled = StackText::default();
    if fmt::Write::write_fmt(&mut spelled, format_args!("{:.*e}", normal.len - 1, f)).is_err() {
        return false;
    }
    DecimalParts::parse(spelled.as_str()).is_some_and(|parts| Normal::of(&parts).same(normal))
}

/// Whether a decimal literal may be a number another reader, which parses
/// floats with Rust's `f64` parser and integers up to `u128` / `i128`,
/// gets wrong: an integer of 39 digits or more, or a float literal no
/// `f64` holds by the module rule. Cheap for the literals it clears: no
/// float parse for 15 digits or fewer inside the normal range. `false` for
/// anything that is not a decimal literal.
pub(crate) fn may_exceed_f64(literal: &str) -> bool {
    let Some(parts) = DecimalParts::parse(literal) else {
        return false;
    };
    if parts.is_integer() {
        return parts.int.trim_start_matches('0').len() >= 39;
    }
    let normal = Normal::of(&parts);
    if normal.len == 0 {
        return false;
    }
    if normal.len <= 15
        && normal
            .leading_exponent()
            .is_some_and(|e| (-300..=300).contains(&e))
    {
        return false;
    }
    !literal.parse::<f64>().is_ok_and(|f| holds(&normal, f))
}

/// A short text written without allocating: room for any `f64` in
/// exponent form at 17 digits.
#[derive(Default)]
struct StackText {
    bytes: [u8; 32],
    len: usize,
}

impl StackText {
    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }
}

impl fmt::Write for StackText {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let end = self.len + s.len();
        self.bytes
            .get_mut(self.len..end)
            .ok_or(fmt::Error)?
            .copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_float_holds_its_correctly_rounded_spellings_only() {
        for float in [
            "0.1",
            "1.10",
            "1e3",
            "0.10000000000000001",
            // `%.17g` of 37.7749295; `37.774929499999998` is no rounding of it.
            "37.774929499999999",
            "4.9e-324",
            "1.7976931348623157e308",
            "-0.0",
            "0e99999999999999999999999999999999",
            "1.0000000000000000000",
        ] {
            assert!(
                matches!(number_value(float), Some(Value::Float(_))),
                "{float}"
            );
            assert!(!may_exceed_f64(float), "{float}");
        }
        for big in [
            "0.10000000000000004",
            "37.774929499999998",
            "0.1000000000000000000000000001",
            "1e400",
            "1e-400",
            "2e-324",
        ] {
            let value = number_value(big);
            assert!(
                matches!(value, Some(Value::BigFloat(_))),
                "{big}: {value:?}"
            );
            assert!(may_exceed_f64(big), "{big}");
        }
    }

    #[test]
    fn integers_past_u128_may_exceed_and_shorter_ones_do_not() {
        assert!(!may_exceed_f64(&"9".repeat(38)));
        assert!(may_exceed_f64(&"9".repeat(39)));
        assert!(!may_exceed_f64(&format!("000{}", "9".repeat(30))));
        assert_eq!(
            float_literal_value("18446744073709551617"),
            Some(Value::BigFloat("18446744073709551617".into()))
        );
        assert_eq!(float_literal_value("12"), Some(Value::Float(12.0)));
    }

    #[test]
    fn same_decimal_compares_exponents_of_any_size() {
        assert!(same_decimal(
            "1e99999999999999999999",
            "1.0e+99999999999999999999"
        ));
        assert!(same_decimal(
            "1e99999999999999999999",
            "10e99999999999999999998"
        ));
        assert!(same_decimal(
            "0.001e-1000000000000000000000000000000000",
            "1e-1000000000000000000000000000000003"
        ));
        assert!(!same_decimal(
            "1e99999999999999999999",
            "1e99999999999999999998"
        ));
        assert!(same_decimal("-0.0120e2", "-1.2"));
        assert!(same_decimal("0.0", "-0e7"));
        assert!(!same_decimal("1", "-1"));
        for bad in ["", "-", ".", "1e", "NaN", "1_0", "1.2.3"] {
            assert!(!same_decimal(bad, bad), "{bad:?}");
        }
    }

    #[test]
    fn add_small_carries_and_borrows() {
        let big = "1".to_owned() + &"0".repeat(31);
        assert_eq!(add_small(false, &big, -1), "9".repeat(31));
        assert_eq!(
            add_small(true, &big, -1),
            "1".to_owned() + &"0".repeat(30) + "1"
        );
        assert_eq!(add_small(false, &"9".repeat(31), 1), big);
    }
}
