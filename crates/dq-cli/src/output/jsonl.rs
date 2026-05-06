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
}

#[cfg(test)]
mod tests {
    use super::*;

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
