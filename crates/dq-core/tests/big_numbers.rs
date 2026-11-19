//! The YAML reader's exact numbers at the edges of the scan that decides
//! whether a file needs its numbers re-read: 16 digits in one run (an
//! `f64` holds every 15-digit decimal) or a 3-digit exponent. Each literal
//! is read by both YAML readers — `Format::parse` and the span-aware
//! `parse_yaml_with_spans` — and must come back as the variant that holds
//! it exactly.

use dq_core::{Value, by_name, parse_yaml_with_spans};
use pretty_assertions::assert_eq;

/// `a: <literal>` as each YAML reader reads `/a`.
fn read_a(literal: &str) -> [Value; 2] {
    let src = format!("a: {literal}\n");
    let plain = by_name("yaml")
        .expect("yaml registered")
        .parse(src.as_bytes())
        .unwrap_or_else(|err| panic!("{literal}: {err}"));
    let spans =
        parse_yaml_with_spans(src.as_bytes()).unwrap_or_else(|err| panic!("{literal}: {err}"));
    [plain, spans].map(|doc| {
        let Value::Map(m) = doc.value() else {
            panic!("{literal}: not a map")
        };
        m["a"].clone()
    })
}

fn big(s: &str) -> Value {
    Value::BigFloat(s.to_owned())
}

#[test]
fn a_float_past_15_digits_or_a_2_digit_exponent_keeps_its_exact_value() {
    for (literal, want) in [
        // Below the scan's threshold: every such literal fits an `f64`.
        ("0.123456789012345", Value::Float(0.123456789012345)),
        ("1.5e99", Value::Float(1.5e99)),
        // At it, a literal that fits stays an `f64`…
        ("1.234567890123457", Value::Float(1.234567890123457)),
        ("1e308", Value::Float(1e308)),
        ("1234567890123456", Value::Int(1_234_567_890_123_456)),
        // …and one that does not keeps its literal: 16 digits split by a
        // `.` (the nearest `f64` is …998), past the range, below the
        // smallest subnormal's spelling, under it.
        ("9999999999999.999", big("9999999999999.999")),
        ("1.8e308", big("1.8e308")),
        ("4.9e-324", big("4.9e-324")),
        ("1e-400", big("1e-400")),
        ("-1E+400", big("-1E+400")),
    ] {
        for got in read_a(literal) {
            assert_eq!(got, want, "{literal}");
        }
    }
}

#[test]
fn a_long_digit_run_that_is_no_number_stays_a_string() {
    for literal in [
        "abc12345678901234567890",
        "1.2.3.4.5.6.7.8.9.0.1.2.3.4.5.6",
        "1_000_000_000_000_000_000",
        "'9999999999999.999'",
    ] {
        let want = Value::String(literal.trim_matches('\'').to_owned());
        for got in read_a(literal) {
            assert_eq!(got, want, "{literal}");
        }
    }
    // A long digit run in a comment only: the numbers stay as read.
    let src = "# 12345678901234567890\na: 0.1\n";
    let doc = parse_yaml_with_spans(src.as_bytes()).expect("parses");
    let Value::Map(m) = doc.value() else { panic!() };
    assert_eq!(m["a"], Value::Float(0.1));
}
