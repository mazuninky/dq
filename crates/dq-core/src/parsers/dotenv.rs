//! `.env` file parser and writer.
//!
//! The parser is hand-rolled rather than delegated to the `dotenvy` crate:
//! `dotenvy`'s public API is a runtime env-vars loader (`from_read`,
//! `from_path`) that mutates `std::env`, not a value-extracting parser.
//! Pulling its internal `Iter` API into our build for a 50-line scanner is
//! not worth the dependency surface; we keep `dotenvy` listed as a workspace
//! dep for parity with design D11 but do not import it here.
//!
//! Per spec: `KEY=VALUE` lines, `export KEY=VALUE` (export prefix stripped),
//! double-quoted with backslash escapes (`\\`, `\"`, `\n`, `\t`, `\r`),
//! single-quoted (literal — no escape processing), comment lines (`#`), and
//! blank lines. Variable interpolation (`${...}`) is NOT performed; the raw
//! string is stored verbatim. Comments and the original quote style are NOT
//! preserved through round-trip (per spec D4).

use std::io::Write;

use camino::Utf8PathBuf;
use indexmap::IndexMap;

use crate::Result;
use crate::document::{Document, FormatTag, Value};
use crate::error::Error;
use crate::format::Format;

/// `.env` format implementation.
#[derive(Debug, Clone, Copy)]
pub struct DotEnv;

impl Format for DotEnv {
    fn name(&self) -> &'static str {
        "dotenv"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["env"]
    }

    fn parse(&self, bytes: &[u8]) -> Result<Document> {
        let text = std::str::from_utf8(bytes).map_err(|e| Error::Parse {
            file: None,
            line: 0,
            col: 0,
            span: 0..0,
            snippet: String::new(),
            message: format!("invalid UTF-8 in .env input: {e}"),
        })?;
        let mut out: IndexMap<String, Value> = IndexMap::new();
        for (lineno_0, raw) in text.split('\n').enumerate() {
            let line_no = (lineno_0 + 1) as u32;
            // Trim trailing CR for CRLF-terminated lines; leading whitespace
            // is allowed and stripped.
            let line = raw.trim_end_matches('\r');
            let trimmed = line.trim_start();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            // Strip leading `export ` prefix if present.
            let body = if let Some(rest) = trimmed.strip_prefix("export ") {
                rest.trim_start()
            } else {
                trimmed
            };
            let Some(eq_idx) = body.find('=') else {
                return Err(Error::Parse {
                    file: None,
                    line: line_no,
                    col: 1,
                    span: 0..0,
                    snippet: line.to_owned(),
                    message: "expected `KEY=VALUE` in .env line".to_owned(),
                });
            };
            let key = body[..eq_idx].trim().to_owned();
            if key.is_empty() {
                return Err(Error::Parse {
                    file: None,
                    line: line_no,
                    col: 1,
                    span: 0..0,
                    snippet: line.to_owned(),
                    message: "empty key in .env line".to_owned(),
                });
            }
            let raw_val = &body[eq_idx + 1..];
            let value = parse_value(raw_val, line_no, line)?;
            out.insert(key, Value::String(value));
        }
        Ok(Document::value_only(Value::Map(out), FormatTag::DotEnv))
    }

    fn write(&self, doc: &Document, w: &mut dyn Write) -> Result<()> {
        let Value::Map(map) = doc.value() else {
            return Err(Error::Format {
                format: "dotenv",
                message: format!(
                    "expected top-level map<string, string>, got {}",
                    doc.value().type_name(),
                ),
            });
        };
        for (key, val) in map {
            let s = match val {
                Value::String(s) => s.clone(),
                Value::Bool(b) => b.to_string(),
                Value::Int(n) => n.to_string(),
                Value::Float(n) => n.to_string(),
                Value::BigInt(s) | Value::BigFloat(s) => s.clone(),
                Value::Null => String::new(),
                Value::Array(_) | Value::Map(_) => {
                    return Err(Error::Format {
                        format: "dotenv",
                        message: format!(
                            "key '{key}': nested {} cannot be serialized to .env",
                            val.type_name(),
                        ),
                    });
                }
            };
            let line = format!("{key}={}\n", quote_for_env(&s));
            w.write_all(line.as_bytes()).map_err(|source| Error::Io {
                path: Utf8PathBuf::from("<dotenv-writer>"),
                source,
            })?;
        }
        Ok(())
    }
}

/// Parse the value side of a single `.env` line.
fn parse_value(raw: &str, line_no: u32, full_line: &str) -> Result<String> {
    // Skip leading whitespace; everything up to either a quote or end-of-line.
    let trimmed = raw.trim_start();
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    let bytes = trimmed.as_bytes();
    match bytes[0] {
        b'"' => parse_double_quoted(trimmed, line_no, full_line),
        b'\'' => parse_single_quoted(trimmed, line_no, full_line),
        _ => Ok(parse_unquoted(trimmed)),
    }
}

/// Parse a double-quoted value, applying backslash escape sequences.
fn parse_double_quoted(input: &str, line_no: u32, full_line: &str) -> Result<String> {
    debug_assert!(input.starts_with('"'));
    let mut chars = input[1..].chars();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Ok(out),
            '\\' => match chars.next() {
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => break,
            },
            other => out.push(other),
        }
    }
    Err(Error::Parse {
        file: None,
        line: line_no,
        col: 1,
        span: 0..0,
        snippet: full_line.to_owned(),
        message: "unterminated double-quoted value".to_owned(),
    })
}

/// Parse a single-quoted value (literal — no escape processing).
fn parse_single_quoted(input: &str, line_no: u32, full_line: &str) -> Result<String> {
    debug_assert!(input.starts_with('\''));
    let after = &input[1..];
    if let Some(end) = after.find('\'') {
        return Ok(after[..end].to_owned());
    }
    Err(Error::Parse {
        file: None,
        line: line_no,
        col: 1,
        span: 0..0,
        snippet: full_line.to_owned(),
        message: "unterminated single-quoted value".to_owned(),
    })
}

/// Parse an unquoted value: literal until end-of-line or first inline `#`
/// comment marker preceded by whitespace, then trim trailing whitespace.
fn parse_unquoted(input: &str) -> String {
    // Walk for an inline comment: a `#` preceded by whitespace marks the
    // start of a comment. We must not strip a `#` that's part of the value
    // itself (no preceding whitespace).
    let mut end = input.len();
    let mut prev_was_ws = true; // start-of-token counts as preceded by ws
    for (i, c) in input.char_indices() {
        if c == '#' && prev_was_ws {
            end = i;
            break;
        }
        prev_was_ws = c.is_whitespace();
    }
    input[..end].trim_end().to_owned()
}

/// Decide whether `s` needs double-quoting and emit the appropriately
/// escaped form. Per spec: quote if value contains whitespace, `#`, `=`,
/// `$`, `\`, `"`, or any non-printable char.
fn quote_for_env(s: &str) -> String {
    let needs_quote = s.is_empty()
        || s.chars().any(|c| {
            c.is_whitespace() || matches!(c, '#' | '=' | '$' | '\\' | '"') || (c as u32) < 0x20
        });
    if !needs_quote {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_assignment() {
        let doc = DotEnv.parse(b"KEY=value\n").expect("parse");
        let Value::Map(m) = doc.value() else {
            panic!("expected map")
        };
        assert_eq!(m.get("KEY"), Some(&Value::String("value".into())));
    }

    #[test]
    fn parse_export_prefix_strips_keyword() {
        let doc = DotEnv.parse(b"export PATH=/usr/bin\n").expect("parse");
        let Value::Map(m) = doc.value() else { panic!() };
        assert_eq!(m.get("PATH"), Some(&Value::String("/usr/bin".into())));
    }

    #[test]
    fn parse_double_quoted_with_escapes() {
        let doc = DotEnv.parse(b"M=\"a\\nb\"\n").expect("parse");
        let Value::Map(m) = doc.value() else { panic!() };
        assert_eq!(m.get("M"), Some(&Value::String("a\nb".into())));
    }

    #[test]
    fn write_quotes_value_with_whitespace() {
        let mut map = IndexMap::new();
        map.insert("M".to_string(), Value::String("hello world".into()));
        let doc = Document::value_only(Value::Map(map), FormatTag::DotEnv);
        let mut buf: Vec<u8> = Vec::new();
        DotEnv.write(&doc, &mut buf).expect("write");
        assert_eq!(String::from_utf8(buf).unwrap(), "M=\"hello world\"\n");
    }
}
