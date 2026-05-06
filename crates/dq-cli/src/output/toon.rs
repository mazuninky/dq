//! TOON reporter — emits Token-Oriented Object Notation via `toon-format`.

use std::io::Write;

use super::Reporter;

/// TOON reporter.
///
/// Delegates to [`toon_format::encode_default`] which accepts any
/// `serde::Serialize` value and returns the TOON string. We then write the
/// string and append a trailing newline so the output is friendly to shell
/// piping.
#[derive(Debug, Clone, Copy, Default)]
pub struct ToonReporter;

impl Reporter for ToonReporter {
    fn report(&self, value: &serde_json::Value, w: &mut dyn Write) -> anyhow::Result<()> {
        let encoded = toon_format::encode_default(value)
            .map_err(|e| anyhow::anyhow!("toon encode failed: {e}"))?;
        w.write_all(encoded.as_bytes())?;
        if !encoded.ends_with('\n') {
            w.write_all(b"\n")?;
        }
        Ok(())
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
}
