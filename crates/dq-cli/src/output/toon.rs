//! TOON reporter — emits Token-Oriented Object Notation via `toon-format`.

use std::io::Write;

use super::Reporter;

/// TOON reporter.
///
/// Delegates to [`toon_format::encode_default`] which accepts any
/// `serde::Serialize` value and returns the TOON string. We then write the
/// string and append a trailing newline so the output is friendly to shell
/// piping.
///
/// `toon_format` reads every number as an `i64`, a `u64` or an `f64`, so a
/// number no one of them holds — a 40-digit integer, a float literal with
/// more digits than an `f64`, `1e400` — would come out rounded or as
/// `null`. Such a value is refused instead ([`ensure_toon_numbers`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct ToonReporter;

impl Reporter for ToonReporter {
    fn report(&self, value: &serde_json::Value, w: &mut dyn Write) -> anyhow::Result<()> {
        ensure_toon_numbers(value)?;
        let mut respelled = Vec::new();
        let marked = mark_tie_floats(value, &mut respelled);
        let mut encoded = toon_format::encode_default(marked.as_ref())
            .map_err(|e| anyhow::anyhow!("toon encode failed: {e}"))?;
        for (at, spelling) in respelled.iter().enumerate() {
            encoded = encoded.replacen(&float_marker(at), spelling, 1);
        }
        w.write_all(encoded.as_bytes())?;
        if !encoded.ends_with('\n') {
            w.write_all(b"\n")?;
        }
        Ok(())
    }

    /// As [`ToonReporter::report`], refusing a number no `i64`, `u64` or
    /// `f64` holds by the literal dq holds — the one the TOML and HCL
    /// writers name too.
    fn report_value(&self, value: &dq_core::Value, w: &mut dyn Write) -> anyhow::Result<()> {
        ensure_toon_value(value)?;
        self.report(&value.to_serde_json(), w)
    }
}

/// The placeholder [`mark_tie_floats`] puts in place of the `at`-th float
/// it respells: private-use characters, which TOON writes bare.
fn float_marker(at: usize) -> String {
    format!("\u{E000}{at}\u{E001}")
}

/// `value` with each float that `toon_format` would spell otherwise than
/// [`dq_core::plain_float`] replaced by a [`float_marker`] string, its
/// spelling pushed to `respelled`. `toon_format` writes a non-whole float
/// with Rust's `Display`, which breaks a tie in the 17th digit to the odd
/// digit (`1658206780088562.3`), while every other dq writer picks the
/// even one (`…562.2`). Borrowed when there is none — or when some string
/// already holds a private-use marker character, which is left as it is.
fn mark_tie_floats<'v>(
    value: &'v serde_json::Value,
    respelled: &mut Vec<String>,
) -> std::borrow::Cow<'v, serde_json::Value> {
    fn respelling(n: &serde_json::Number) -> Option<String> {
        if n.as_i64().is_some() || n.as_u64().is_some() {
            return None;
        }
        let f = n.as_f64().filter(|f| f.is_finite())?;
        // `toon_format` writes a whole float in `i64` range, [-2^63, 2^63),
        // as that integer.
        const TWO_TO_63: f64 = 9_223_372_036_854_775_808.0;
        let in_i64 = f.fract() == 0.0 && (-TWO_TO_63..TWO_TO_63).contains(&f);
        let plain = dq_core::plain_float(f);
        (!in_i64 && plain != f.to_string()).then_some(plain)
    }
    fn has_tie(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::Number(n) => respelling(n).is_some(),
            serde_json::Value::Array(items) => items.iter().any(has_tie),
            serde_json::Value::Object(map) => map.values().any(has_tie),
            _ => false,
        }
    }
    fn holds_marker(value: &serde_json::Value) -> bool {
        let marks = |s: &str| s.contains(['\u{E000}', '\u{E001}']);
        match value {
            serde_json::Value::String(s) => marks(s),
            serde_json::Value::Array(items) => items.iter().any(holds_marker),
            serde_json::Value::Object(map) => map.iter().any(|(k, v)| marks(k) || holds_marker(v)),
            _ => false,
        }
    }
    fn mark(value: &mut serde_json::Value, respelled: &mut Vec<String>) {
        match value {
            serde_json::Value::Number(n) => {
                if let Some(plain) = respelling(n) {
                    *value = serde_json::Value::String(float_marker(respelled.len()));
                    respelled.push(plain);
                }
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(|v| mark(v, respelled)),
            serde_json::Value::Object(map) => map.values_mut().for_each(|v| mark(v, respelled)),
            _ => {}
        }
    }
    if !has_tie(value) || holds_marker(value) {
        return std::borrow::Cow::Borrowed(value);
    }
    let mut owned = value.clone();
    mark(&mut owned, respelled);
    std::borrow::Cow::Owned(owned)
}

/// Fail on the first number of `value` that `toon_format` would not write
/// as the number it is, naming it as `value` spells it.
///
/// # Errors
///
/// [`dq_core::Error::Format`] naming the number.
fn ensure_toon_value(value: &dq_core::Value) -> Result<(), dq_core::Error> {
    use dq_core::Value;
    match value {
        Value::BigInt(s) if s.parse::<u64>().is_ok() => Ok(()),
        Value::BigFloat(_) if value.non_finite_float().is_some() => Ok(()),
        Value::BigInt(s) | Value::BigFloat(s) => Err(toon_refuses(s)),
        Value::Array(items) => items.iter().try_for_each(ensure_toon_value),
        Value::Map(map) => map.values().try_for_each(ensure_toon_value),
        Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::String(_) => Ok(()),
    }
}

fn toon_refuses(literal: &str) -> dq_core::Error {
    dq_core::Error::Format {
        format: "toon",
        message: format!("TOON cannot represent arbitrary-precision number {literal}"),
    }
}

/// Fail on the first number of `value` that `toon_format` would not write
/// as the number it is.
///
/// # Errors
///
/// [`dq_core::Error::Format`] naming the number.
fn ensure_toon_numbers(value: &serde_json::Value) -> Result<(), dq_core::Error> {
    match value {
        serde_json::Value::Number(n) => {
            let literal = n.to_string();
            let exact = n.as_u64().is_some()
                || matches!(
                    dq_core::Value::from_number_literal(&literal),
                    Some(dq_core::Value::Int(_) | dq_core::Value::Float(_))
                );
            if exact {
                Ok(())
            } else {
                Err(toon_refuses(&literal))
            }
        }
        serde_json::Value::Array(items) => items.iter().try_for_each(ensure_toon_numbers),
        serde_json::Value::Object(map) => map.values().try_for_each(ensure_toon_numbers),
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::String(_) => {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_some_toon_for_an_object() {
        let mut buf: Vec<u8> = Vec::new();
        ToonReporter
            .report(&serde_json::json!({"name": "alice"}), &mut buf)
            .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("name"), "expected key in toon output: {out:?}");
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn refuses_a_number_toon_would_round() {
        for literal in [
            "1e400",
            "0.1000000000000000000000000001",
            "-18446744073709551617",
        ] {
            let value: serde_json::Value =
                serde_json::from_str(&format!("{{\"a\": [{literal}]}}")).unwrap();
            let err = ToonReporter
                .report(&value, &mut Vec::new())
                .expect_err(literal);
            // serde_json spells `1e400` as `1e+400`.
            let message = err.to_string();
            assert!(
                message.starts_with(
                    "toon format error: TOON cannot represent arbitrary-precision number "
                ),
                "{message}"
            );
        }
        let exact = serde_json::json!({"u": 18446744073709551615_u64, "f": 1.5, "i": -3});
        assert!(ToonReporter.report(&exact, &mut Vec::new()).is_ok());
    }

    #[test]
    fn report_value_names_the_number_as_dq_holds_it() {
        for literal in ["1e400", "1E+400", "0.1000000000000000000000000001"] {
            let value = dq_core::Value::Array(vec![dq_core::Value::BigFloat(literal.into())]);
            let err = ToonReporter
                .report_value(&value, &mut Vec::new())
                .expect_err(literal);
            assert_eq!(
                err.to_string(),
                format!(
                    "toon format error: TOON cannot represent arbitrary-precision number {literal}"
                )
            );
        }
        let exact = dq_core::Value::BigInt("18446744073709551615".into());
        assert!(ToonReporter.report_value(&exact, &mut Vec::new()).is_ok());
    }

    #[test]
    fn encodes_toon_spec_v4_empty_arrays_and_quoting() {
        let mut buf: Vec<u8> = Vec::new();
        ToonReporter
            .report(&serde_json::json!({"roles": [], "tag": "#bob"}), &mut buf)
            .unwrap();
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "roles: []\ntag: \"#bob\"\n"
        );
    }
}
