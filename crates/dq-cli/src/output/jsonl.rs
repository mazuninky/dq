//! JSONL reporter — round-trips through `dq-core`'s JSONL writer.

use std::io::Write;

use dq_core::Format;
use dq_core::parsers::Jsonl;

use super::{Reporter, from_serde_value};

/// Newline-delimited JSON reporter.
///
/// When the value is a JSON array, each element renders on its own line.
/// Other shapes are written as a single line, matching the convert path.
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonlReporter;

impl Reporter for JsonlReporter {
    fn report(&self, value: &serde_json::Value, w: &mut dyn Write) -> anyhow::Result<()> {
        let core = from_serde_value(value);
        let doc = dq_core::Document::single(core);
        Jsonl.write(&doc, w)?;
        Ok(())
    }

    /// Writes through the JSONL writer. A NaN or an infinity anywhere in
    /// `value` fails as `convert -F json` does — checked before the first
    /// record, since the writer streams and would leave a truncated one.
    fn report_value(&self, value: &dq_core::Value, w: &mut dyn Write) -> anyhow::Result<()> {
        dq_core::parsers::json::ensure_finite(value)?;
        Jsonl.write(&dq_core::Document::single(value.clone()), w)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORTER: JsonlReporter = JsonlReporter;

    #[test]
    fn report_value_of_nan_or_infinity_fails_as_convert_to_json_does() {
        for bad in [f64::NAN, f64::INFINITY] {
            // After a finite record: the writer streams, so a late NaN
            // must still leave nothing on `w`.
            let value = dq_core::Value::Array(vec![
                dq_core::Value::Int(1),
                dq_core::Value::Array(vec![dq_core::Value::Int(1), dq_core::Value::Float(bad)]),
            ]);
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
    fn writes_each_array_element_on_own_line() {
        let mut buf: Vec<u8> = Vec::new();
        JsonlReporter
            .report(&serde_json::json!([{"a": 1}, {"a": 2}]), &mut buf)
            .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert_eq!(out.lines().count(), 2, "expected two lines, got: {out:?}");
    }
}
