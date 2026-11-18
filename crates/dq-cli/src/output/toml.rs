//! TOML reporter — round-trips through `dq-core`'s TOML writer.

use std::io::Write;

use dq_core::Format;
use dq_core::parsers::Toml;

use super::{Reporter, from_serde_value};

/// TOML reporter.
#[derive(Debug, Clone, Copy, Default)]
pub struct TomlReporter;

impl Reporter for TomlReporter {
    fn report(&self, value: &serde_json::Value, w: &mut dyn Write) -> anyhow::Result<()> {
        let core = from_serde_value(value);
        let doc = dq_core::Document::single(core);
        Toml.write(&doc, w)?;
        Ok(())
    }

    /// Writes the value as is, so a NaN or an infinity is `nan` / `inf`.
    fn report_value(&self, value: &dq_core::Value, w: &mut dyn Write) -> anyhow::Result<()> {
        Toml.write(&dq_core::Document::single(value.clone()), w)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `{x: NaN, y: [-inf]}` as jq hands it back.
    fn non_finite() -> dq_core::Value {
        let mut map = indexmap::IndexMap::new();
        map.insert("x".to_owned(), dq_core::Value::Float(f64::NAN));
        map.insert(
            "y".to_owned(),
            dq_core::Value::Array(vec![dq_core::Value::Float(f64::NEG_INFINITY)]),
        );
        dq_core::Value::Map(map)
    }

    #[test]
    fn report_value_writes_nan_and_infinity_as_toml_floats() {
        let mut buf: Vec<u8> = Vec::new();
        TomlReporter.report_value(&non_finite(), &mut buf).unwrap();
        assert_eq!(String::from_utf8(buf).unwrap(), "x = nan\ny = [-inf]\n");
    }

    #[test]
    fn writes_a_toml_table() {
        let mut buf: Vec<u8> = Vec::new();
        TomlReporter
            .report(&serde_json::json!({"a": 1}), &mut buf)
            .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("a = 1"), "expected `a = 1`, got: {out:?}");
    }
}
