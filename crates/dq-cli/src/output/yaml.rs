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
}

#[cfg(test)]
mod tests {
    use super::*;

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
