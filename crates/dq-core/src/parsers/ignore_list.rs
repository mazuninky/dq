//! `.gitignore` / `.dockerignore` style ignore-list parser (read-only).
//!
//! Top-level shape: flat `Array<String>`. Comments (`#` prefix after leading
//! whitespace) and blank lines are dropped — they are not preserved in the
//! value tree (per design D7). Trailing whitespace is trimmed from each
//! pattern.
//!
//! `Format::write` always returns `Error::Format { format: "ignore-list",
//! message: contains "read-only" }`.

use std::io::Write;

use crate::Result;
use crate::document::{Document, FormatTag, Value};
use crate::error::Error;
use crate::format::Format;

/// Ignore-list (`.gitignore` / `.dockerignore`) format implementation.
#[derive(Debug, Clone, Copy)]
pub struct IgnoreList;

impl Format for IgnoreList {
    fn name(&self) -> &'static str {
        "ignore-list"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["gitignore", "dockerignore"]
    }

    fn parse(&self, bytes: &[u8]) -> Result<Document> {
        let text = std::str::from_utf8(bytes).map_err(|e| Error::Parse {
            file: None,
            line: 0,
            col: 0,
            span: 0..0,
            snippet: String::new(),
            message: format!("invalid UTF-8 in ignore-list input: {e}"),
        })?;
        let mut out: Vec<Value> = Vec::new();
        for raw in text.split('\n') {
            let line = raw.trim_end_matches('\r').trim_end();
            let leading_trimmed = line.trim_start();
            if leading_trimmed.is_empty() || leading_trimmed.starts_with('#') {
                continue;
            }
            // Patterns are stored as their trimmed-trailing form; the leading
            // whitespace is preserved (it has no semantic meaning in
            // gitignore but rejection of leading-space patterns is left to
            // the consumer).
            out.push(Value::String(line.to_owned()));
        }
        Ok(Document::value_only(
            Value::Array(out),
            FormatTag::IgnoreList,
        ))
    }

    fn write(&self, _doc: &Document, _w: &mut dyn Write) -> Result<()> {
        Err(Error::Format {
            format: "ignore-list",
            message: "ignore-list is read-only in M5".to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_one_pattern_per_line() {
        let doc = IgnoreList
            .parse(b"node_modules/\n# build\n*.log\n\ntarget/\n")
            .expect("parse");
        let Value::Array(items) = doc.value() else {
            panic!("expected array")
        };
        let strings: Vec<&str> = items
            .iter()
            .map(|v| match v {
                Value::String(s) => s.as_str(),
                _ => panic!("non-string in ignore list"),
            })
            .collect();
        assert_eq!(strings, vec!["node_modules/", "*.log", "target/"]);
    }

    #[test]
    fn write_returns_format_error() {
        let doc = Document::value_only(Value::Array(vec![]), FormatTag::IgnoreList);
        let mut buf: Vec<u8> = Vec::new();
        let err = IgnoreList.write(&doc, &mut buf).expect_err("read-only");
        match err {
            Error::Format { format, message } => {
                assert_eq!(format, "ignore-list");
                assert!(
                    message.contains("read-only"),
                    "expected read-only message; got: {message}",
                );
            }
            other => panic!("expected Format error, got {other:?}"),
        }
    }
}
