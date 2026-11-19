//! Pretty-printed JSON reporter.

use std::io::Write;

use dq_core::Format;

use super::Reporter;

/// Pretty-printed JSON reporter (two-space indent, trailing newline).
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonReporter;

impl Reporter for JsonReporter {
    /// Writes through `dq-core`'s JSON writer too, in the layout
    /// `serde_json`'s pretty printer has, so a float is spelled one way on
    /// every JSON output: the shortest round-trip form, `1e300` (serde_json
    /// writes `1e+300`).
    fn report(&self, value: &serde_json::Value, w: &mut dyn Write) -> anyhow::Result<()> {
        let core = super::from_serde_value(value);
        dq_core::parsers::Json.write(&dq_core::Document::single(core), w)?;
        Ok(())
    }

    /// Writes through `dq-core`'s JSON writer, as `convert -F json` and
    /// the JSONL reporter do, so a number is spelled one way on every JSON
    /// output: a big number as dq holds its literal (`1e400`, not
    /// serde_json's `1e+400`), an `f64` as the writer spells it. JSON has
    /// no NaN or infinity: fail as `convert -F json` does rather than
    /// print one as a string.
    fn report_value(&self, value: &dq_core::Value, w: &mut dyn Write) -> anyhow::Result<()> {
        dq_core::parsers::json::ensure_finite(value)?;
        dq_core::parsers::Json.write(&dq_core::Document::single(value.clone()), w)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORTER: JsonReporter = JsonReporter;

    #[test]
    fn report_value_of_nan_or_infinity_fails_as_convert_to_json_does() {
        for bad in [f64::NAN, f64::INFINITY] {
            let value = dq_core::Value::Array(vec![dq_core::Value::Float(bad)]);
            let mut buf: Vec<u8> = Vec::new();
            let err = REPORTER.report_value(&value, &mut buf).unwrap_err();
            assert!(
                matches!(
                    err.downcast_ref::<dq_core::Error>(),
                    Some(dq_core::Error::Format { format: "json", message })
                        if message.contains("non-finite float")
                ),
                "{err:?}"
            );
            assert!(buf.is_empty(), "nothing is written: {buf:?}");
        }
    }

    #[test]
    fn report_value_of_finite_data_matches_report() {
        let value = dq_core::Value::from_serde_json(&serde_json::json!([{"a": 1.5}]));
        let (mut via_value, mut via_json) = (Vec::new(), Vec::new());
        REPORTER.report_value(&value, &mut via_value).unwrap();
        REPORTER
            .report(&serde_json::json!([{"a": 1.5}]), &mut via_json)
            .unwrap();
        assert_eq!(via_value, via_json);
    }

    #[test]
    fn writes_pretty_with_trailing_newline() {
        let mut buf: Vec<u8> = Vec::new();
        JsonReporter
            .report(&serde_json::json!({"a": 1}), &mut buf)
            .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.ends_with('\n'),
            "expected trailing newline, got: {out:?}"
        );
        assert!(out.contains("  \"a\": 1"));
    }

    #[test]
    fn renders_arrays_pretty() {
        let mut buf: Vec<u8> = Vec::new();
        JsonReporter
            .report(&serde_json::json!([1, 2]), &mut buf)
            .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("[\n"));
        assert!(out.ends_with("]\n"));
    }
}
