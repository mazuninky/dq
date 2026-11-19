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

use std::cmp::Ordering;
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
/// allocating unless the exponent is past `i128`.
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
    /// Past `i128`, in decimal: sign and digits without leading zeros.
    Big(bool, String),
}

impl Exponent {
    /// The exponent as a whole number of any size.
    fn whole(&self) -> BigWhole {
        match self {
            Self::Small(n) => BigWhole::of_i128(*n),
            Self::Big(negative, digits) => BigWhole {
                negative: *negative,
                digits: digits.clone(),
            },
        }
    }
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
        // One form per value: `Small` whenever the adjusted exponent fits an
        // `i128`, so `1e1…0` (31 digits) and `10e9…9` (30) compare equal.
        let signed = |magnitude: i128| if exp_negative { -magnitude } else { magnitude };
        let small = (exp_digits.len() <= 38)
            .then(|| exp_digits.parse::<i128>().unwrap_or(0))
            .and_then(|magnitude| signed(magnitude).checked_add(adjust));
        let exponent = match small {
            Some(exponent) => Exponent::Small(exponent),
            None => {
                let digits = add_small(exp_negative, exp_digits, adjust);
                match digits.parse::<i128>() {
                    Ok(magnitude) => Exponent::Small(signed(magnitude)),
                    Err(_) => Exponent::Big(exp_negative, digits),
                }
            }
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

    /// The power of ten of the first significant digit, at any size. Only
    /// for a nonzero number.
    fn leading_power(&self) -> BigWhole {
        let above_last = self.len.saturating_sub(1);
        let adjust = i128::try_from(above_last).unwrap_or(i128::MAX);
        match &self.exponent {
            Exponent::Small(last) => match last.checked_add(adjust) {
                Some(power) => BigWhole::of_i128(power),
                // Past `i128` only when `last` is near its edge: 39 digits.
                None => BigWhole {
                    negative: *last < 0,
                    digits: add_small(*last < 0, &last.unsigned_abs().to_string(), adjust),
                },
            },
            Exponent::Big(negative, digits) => BigWhole {
                negative: *negative,
                digits: add_small(*negative, digits, adjust),
            },
        }
    }

    /// The power of ten of the first significant digit, when it is small.
    fn leading_exponent(&self) -> Option<i128> {
        match self.exponent {
            Exponent::Small(last) => last.checked_add(i128::try_from(self.len).ok()? - 1),
            Exponent::Big(..) => None,
        }
    }
}

/// `digits` (39 or more of them, so larger than `adjust` by far) with
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

/// Whether a decimal literal spells a whole number, at any size: `1e400`,
/// `1.50e1` and `-0.0` do, `1.5` and `1e-400` do not. `false` when it is
/// not a decimal literal.
pub(crate) fn decimal_is_whole(literal: &str) -> bool {
    DecimalParts::parse(literal).is_some_and(|parts| {
        let normal = Normal::of(&parts);
        normal.len == 0
            || match normal.exponent {
                Exponent::Small(last) => last >= 0,
                Exponent::Big(negative, _) => !negative,
            }
    })
}

/// How two decimal literals order by their exact value, at any size:
/// `0.1` is below `0.10000000000000000001`, `1e400` above
/// `18446744073709551616`, `-0.0` equal to `0`. `None` when either is
/// not a decimal literal.
pub(crate) fn compare_decimal(a: &str, b: &str) -> Option<Ordering> {
    let (a, b) = (DecimalParts::parse(a)?, DecimalParts::parse(b)?);
    let (a, b) = (Normal::of(&a), Normal::of(&b));
    let sign = |n: &Normal<'_>| match (n.len, n.negative) {
        (0, _) => 0,
        (_, true) => -1,
        (_, false) => 1,
    };
    let order = sign(&a).cmp(&sign(&b));
    if order != Ordering::Equal || a.len == 0 {
        return Some(order);
    }
    // Same sign, neither zero: the larger magnitude has the higher first
    // digit's power of ten, or the same power and the larger digits —
    // which have no trailing zeros, so a longer run past a common prefix
    // is the larger.
    let magnitude = a
        .leading_power()
        .compare(&b.leading_power())
        .then_with(|| a.digits().cmp(b.digits()));
    Some(if a.negative {
        magnitude.reverse()
    } else {
        magnitude
    })
}

/// Whether the decimal literal `x` is a whole multiple of the decimal
/// literal `d`, by their exact values and at any size: `x / d` is an
/// integer. `0.3` is a multiple of `0.1`, `18446744073709551616` of `2`,
/// `4` not of `1e400` — of which only `0` is, among finite numbers.
/// `None` when either is not a decimal literal or `d` is zero.
pub(crate) fn decimal_is_multiple(x: &str, d: &str) -> Option<bool> {
    let (x, d) = (DecimalParts::parse(x)?, DecimalParts::parse(d)?);
    let (x, d) = (Normal::of(&x), Normal::of(&d));
    if d.len == 0 {
        return None;
    }
    if x.len == 0 {
        return Some(true);
    }
    // x = X·10^ex and d = D·10^ed with X, D whole: x / d = X·10^k / D for
    // k = ex - ed.
    let (big_x, big_d) = (whole_digits(&x), whole_digits(&d));
    let k = x.exponent.whole().minus(&d.exponent.whole());
    // Too long for a `usize` is past every literal's length.
    let steps = k.digits.parse::<usize>().unwrap_or(usize::MAX);
    if k.negative {
        // D·10^|k| must divide X, which is below 10^(its length).
        if steps >= big_x.len() {
            return Some(false);
        }
        let divisor = format!("{big_d}{}", "0".repeat(steps));
        return Some(remainder(&big_x, &divisor) == "0");
    }
    // D divides X·10^k iff D / gcd(D, 10^k) divides X; each power of ten
    // takes one 2 and one 5 out of D, while it has them.
    let mut rest = big_d;
    for _ in 0..steps {
        let last = rest.as_bytes().last().map_or(0, |b| b - b'0');
        rest = match (last % 2 == 0, last % 5 == 0) {
            (true, true) => divide_small(&rest, 10),
            (true, false) => divide_small(&rest, 2),
            (false, true) => divide_small(&rest, 5),
            (false, false) => break,
        };
    }
    Some(remainder(&big_x, &rest) == "0")
}

/// A nonzero [`Normal`]'s significant digits, as a whole number.
fn whole_digits(normal: &Normal<'_>) -> String {
    normal.digits().map(char::from).collect()
}

/// `a mod m` for whole numbers in decimal without leading zeros, `m`
/// nonzero.
fn remainder(a: &str, m: &str) -> String {
    let mut rest = String::from("0");
    for digit in a.chars() {
        if rest == "0" {
            rest.clear();
        }
        rest.push(digit);
        while compare_whole(&rest, m) != Ordering::Less {
            rest = subtract_whole(&rest, m);
        }
    }
    rest
}

/// `a / divisor` for a whole number in decimal that `divisor` divides.
fn divide_small(a: &str, divisor: u8) -> String {
    let mut out = String::with_capacity(a.len());
    let mut carry = 0u8;
    for digit in a.bytes() {
        let current = carry * 10 + (digit - b'0');
        let quotient = current / divisor;
        carry = current % divisor;
        if !(out.is_empty() && quotient == 0) {
            out.push(char::from(b'0' + quotient));
        }
    }
    if out.is_empty() {
        out.push('0');
    }
    out
}

/// How two whole numbers in decimal without leading zeros order.
fn compare_whole(a: &str, b: &str) -> Ordering {
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

/// `a + b` for whole numbers in decimal without leading zeros.
fn add_whole(a: &str, b: &str) -> String {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut out = Vec::with_capacity(a.len().max(b.len()) + 1);
    let mut carry = 0u8;
    for i in 0..a.len().max(b.len()) {
        let digit = |n: &[u8]| n.len().checked_sub(i + 1).map_or(0, |at| n[at] - b'0');
        let sum = digit(a) + digit(b) + carry;
        out.push(sum % 10);
        carry = sum / 10;
    }
    if carry > 0 {
        out.push(carry);
    }
    out.iter().rev().map(|d| char::from(b'0' + d)).collect()
}

/// `a - b` for whole numbers in decimal without leading zeros, `a >= b`.
fn subtract_whole(a: &str, b: &str) -> String {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut out = Vec::with_capacity(a.len());
    let mut borrow = 0u8;
    for i in 0..a.len() {
        let top = a[a.len() - 1 - i] - b'0';
        let bottom = b.len().checked_sub(i + 1).map_or(0, |at| b[at] - b'0') + borrow;
        if top >= bottom {
            out.push(top - bottom);
            borrow = 0;
        } else {
            out.push(top + 10 - bottom);
            borrow = 1;
        }
    }
    while out.len() > 1 && out.last() == Some(&0) {
        out.pop();
    }
    out.iter().rev().map(|d| char::from(b'0' + d)).collect()
}

/// A signed whole number of any size: sign and digits without leading
/// zeros (`0` is `false`, `"0"`).
#[derive(Debug, PartialEq, Eq)]
struct BigWhole {
    negative: bool,
    digits: String,
}

impl BigWhole {
    fn of_i128(n: i128) -> Self {
        Self {
            negative: n < 0,
            digits: n.unsigned_abs().to_string(),
        }
    }

    /// `self - other`.
    fn minus(&self, other: &Self) -> Self {
        // self + (-other): one sign adds magnitudes, two subtract the
        // smaller from the larger, which keeps its sign.
        let negated = !other.negative && other.digits != "0";
        if self.negative == negated || self.digits == "0" || other.digits == "0" {
            let digits = add_whole(&self.digits, &other.digits);
            let negative = if self.digits == "0" {
                negated
            } else {
                self.negative
            };
            return Self {
                negative: negative && digits != "0",
                digits,
            };
        }
        match compare_whole(&self.digits, &other.digits) {
            Ordering::Equal => Self::of_i128(0),
            Ordering::Greater => Self {
                negative: self.negative,
                digits: subtract_whole(&self.digits, &other.digits),
            },
            Ordering::Less => Self {
                negative: negated,
                digits: subtract_whole(&other.digits, &self.digits),
            },
        }
    }

    fn compare(&self, other: &Self) -> Ordering {
        let magnitude = |n: &Self| (n.digits.len(), n.digits.clone());
        match (self.negative, other.negative) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => magnitude(self).cmp(&magnitude(other)),
            (true, true) => magnitude(other).cmp(&magnitude(self)),
        }
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
        // Either side of where an exponent stops fitting a digit count.
        assert!(same_decimal(
            "1e1000000000000000000000000000000",
            "10e999999999999999999999999999999"
        ));
        let past_i128 = "1000000000000000000000000000000000000000";
        assert!(same_decimal(
            &format!("1e{past_i128}"),
            &format!("10e{}", "9".repeat(past_i128.len() - 1))
        ));
        assert!(same_decimal(
            "1e170141183460469231731687303715884105728",
            "10e170141183460469231731687303715884105727"
        ));
        assert!(same_decimal("-0.0120e2", "-1.2"));
        assert!(same_decimal("0.0", "-0e7"));
        assert!(!same_decimal("1", "-1"));
        for bad in ["", "-", ".", "1e", "NaN", "1_0", "1.2.3"] {
            assert!(!same_decimal(bad, bad), "{bad:?}");
        }
    }

    #[test]
    fn compare_decimal_orders_exact_values_of_any_size() {
        use Ordering::{Equal, Greater, Less};
        let cases = [
            ("0.1", "0.10000000000000000001", Less),
            ("1e400", "18446744073709551616", Greater),
            ("-1e400", "-18446744073709551616", Less),
            ("-0.0", "0", Equal),
            ("0", "1e-400", Less),
            ("-1e-400", "0", Less),
            ("1.10", "1.1", Equal),
            ("1e3", "999.9999999999999999999", Greater),
            ("12", "1.2e1", Equal),
            (
                "1e99999999999999999999999999999999999999999",
                "1e400",
                Greater,
            ),
            (
                "1e-99999999999999999999999999999999999999999",
                "1e-400",
                Less,
            ),
            (
                "1e170141183460469231731687303715884105727",
                "1e170141183460469231731687303715884105728",
                Less,
            ),
            (
                "12e170141183460469231731687303715884105727",
                "1e170141183460469231731687303715884105728",
                Greater,
            ),
            ("-2", "-10", Greater),
        ];
        for (a, b, order) in cases {
            assert_eq!(compare_decimal(a, b), Some(order), "{a} vs {b}");
            assert_eq!(compare_decimal(b, a), Some(order.reverse()), "{b} vs {a}");
        }
        assert_eq!(compare_decimal("x", "1"), None);
    }

    #[test]
    fn decimal_is_multiple_divides_exact_values_of_any_size() {
        for (x, d) in [
            ("0", "1e400"),
            ("0.3", "0.1"),
            ("18446744073709551616", "2"),
            ("18446744073709551616", "18446744073709551616"),
            ("-36893488147419103232", "18446744073709551616"),
            ("3e400", "3e399"),
            ("1e400", "1e-400"),
            ("1e99999999999999999999999999999999999999999", "5e3"),
            ("2e-400", "1e-400"),
            ("12.5", "2.5"),
            ("1000", "8"),
            ("1", "1e-99999999999999999999999999999999999999999"),
        ] {
            assert_eq!(decimal_is_multiple(x, d), Some(true), "{x} by {d}");
        }
        for (x, d) in [
            ("4", "1e400"),
            ("18446744073709551616", "1e400"),
            ("18446744073709551617", "2"),
            ("0.1", "0.3"),
            ("1e400", "3"),
            ("1e-400", "2e-400"),
            ("1", "3e-99999999999999999999999999999999999999999"),
            ("1e-99999999999999999999999999999999999999999", "1"),
            ("100", "16"),
        ] {
            assert_eq!(decimal_is_multiple(x, d), Some(false), "{x} by {d}");
        }
        assert_eq!(decimal_is_multiple("1", "0.0"), None);
        assert_eq!(decimal_is_multiple("x", "1"), None);
    }

    #[test]
    fn whole_arithmetic_carries_and_borrows() {
        assert_eq!(add_whole("999", "1"), "1000");
        assert_eq!(subtract_whole("1000", "1"), "999");
        assert_eq!(remainder("1000", "7"), "6");
        assert_eq!(divide_small("1000", 8), "125");
        let big = |negative: bool, digits: &str| BigWhole {
            negative,
            digits: digits.to_owned(),
        };
        assert_eq!(big(false, "5").minus(&big(false, "7")), big(true, "2"));
        assert_eq!(big(true, "5").minus(&big(true, "7")), big(false, "2"));
        assert_eq!(big(true, "5").minus(&big(false, "7")), big(true, "12"));
        assert_eq!(big(false, "0").minus(&big(false, "7")), big(true, "7"));
        assert_eq!(big(false, "7").minus(&big(false, "7")), big(false, "0"));
    }

    #[test]
    fn decimal_is_whole_reads_the_exact_value() {
        for whole in [
            "0",
            "-0.0",
            "12",
            "1e400",
            "1.50e1",
            "150e-1",
            "1e99999999999999999999999999999999999999999",
        ] {
            assert!(decimal_is_whole(whole), "{whole}");
        }
        for fraction in [
            "1.5",
            "1e-400",
            "1e-99999999999999999999999999999999999999999",
            "0.1",
        ] {
            assert!(!decimal_is_whole(fraction), "{fraction}");
        }
        assert!(!decimal_is_whole(".nan"));
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
