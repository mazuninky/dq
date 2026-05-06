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
}

#[cfg(test)]
mod tests {
    use super::*;

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
