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
        let encoded = toon_format::encode_default(value)
            .map_err(|e| anyhow::anyhow!("toon encode failed: {e}"))?;
        w.write_all(encoded.as_bytes())?;
        if !encoded.ends_with('\n') {
            w.write_all(b"\n")?;
        }
        Ok(())
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
                Err(dq_core::Error::Format {
                    format: "toon",
                    message: format!("TOON cannot represent arbitrary-precision number {literal}"),
                })
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
