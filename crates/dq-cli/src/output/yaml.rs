//! YAML reporter — round-trips the `serde_json::Value` through `dq-core`'s
//! YAML writer so output matches `dq convert -F yaml` byte-for-byte.

use std::io::Write;

use dq_core::Format;
use dq_core::parsers::Yaml;

use super::{Reporter, from_serde_value};

/// YAML reporter.
#[derive(Debug, Clone, Copy, Default)]
pub struct YamlReporter;

impl Reporter for YamlReporter {
    fn report(&self, value: &serde_json::Value, w: &mut dyn Write) -> anyhow::Result<()> {
        let core = from_serde_value(value);
        let doc = dq_core::Document::single(core);
        Yaml.write(&doc, w)?;
        Ok(())
    }

    /// Writes the value as is, so a NaN or an infinity is `.nan` / `.inf`.
    fn report_value(&self, value: &dq_core::Value, w: &mut dyn Write) -> anyhow::Result<()> {
        Yaml.write(&dq_core::Document::single(value.clone()), w)?;
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
    fn report_value_writes_nan_and_infinity_as_yaml_floats() {
        let mut buf: Vec<u8> = Vec::new();
        YamlReporter.report_value(&non_finite(), &mut buf).unwrap();
        // `'y'`: the key is a bool to a YAML 1.1 reader.
        assert_eq!(String::from_utf8(buf).unwrap(), "x: .nan\n'y':\n- -.inf\n");
    }

    #[test]
    fn writes_a_yaml_mapping() {
        let mut buf: Vec<u8> = Vec::new();
        YamlReporter
            .report(&serde_json::json!({"a": 1, "b": 2}), &mut buf)
            .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("a: 1"), "expected `a: 1`, got: {out:?}");
        assert!(out.contains("b: 2"), "expected `b: 2`, got: {out:?}");
    }
}
