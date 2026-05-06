//! Pretty-printed JSON reporter.

use std::io::Write;

use super::Reporter;

/// Pretty-printed JSON reporter (two-space indent, trailing newline).
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonReporter;

impl Reporter for JsonReporter {
    fn report(&self, value: &serde_json::Value, w: &mut dyn Write) -> anyhow::Result<()> {
        serde_json::to_writer_pretty(&mut *w, value)?;
        w.write_all(b"\n")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
