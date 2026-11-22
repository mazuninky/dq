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
//! preserved through round-trip (per spec D4); which keys had `export` is
//! ([`DotEnvLayout`]), and the writer writes them with it.

use std::collections::HashSet;

use std::io::Write;

use camino::Utf8PathBuf;
use indexmap::IndexMap;

use crate::Result;
use crate::document::{Document, FormatTag, Layout, Value};
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
        let text = std::str::from_utf8(bytes).map_err(|e| Error::invalid_utf8(bytes, e, ".env"))?;
        let mut out: IndexMap<String, Value> = IndexMap::new();
        let mut layout = DotEnvLayout::default();
        let mut next_line = 0;
        for raw in text.split('\n') {
            let line_start = next_line;
            next_line += raw.len() + 1;
            // Trim trailing CR for CRLF-terminated lines; leading whitespace
            // is allowed and stripped.
            let line = raw.trim_end_matches('\r');
            let trimmed = line.trim_start();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            // Strip leading `export ` prefix if present.
            let exported = trimmed.strip_prefix("export ");
            let body = exported.map_or(trimmed, str::trim_start);
            // `body` and the value are tails of `line`.
            let offset_of = |tail: &str| line_start + line.len() - tail.len();
            let Some(eq_idx) = body.find('=') else {
                let start = offset_of(trimmed);
                return Err(Error::parse_at(
                    bytes,
                    start..start + trimmed.trim_end().len(),
                    "expected `KEY=VALUE` in .env line",
                ));
            };
            let key = body[..eq_idx].trim().to_owned();
            if key.is_empty() {
                let eq = offset_of(&body[eq_idx..]);
                return Err(Error::parse_at(bytes, eq..eq + 1, "empty key in .env line"));
            }
            let raw_val = &body[eq_idx + 1..];
            let value = parse_value(raw_val).map_err(|unterminated| {
                // At the quote nothing closes, to the end of the line.
                Error::parse_at(
                    bytes,
                    offset_of(raw_val.trim_start())..line_start + line.len(),
                    unterminated,
                )
            })?;
            // The last assignment of a key wins, with or without `export`.
            if exported.is_some() {
                layout.exported.insert(key.clone());
            } else {
                layout.exported.remove(&key);
            }
            out.insert(key, Value::String(value));
        }
        Ok(Document::value_only(Value::Map(out), FormatTag::DotEnv)
            .with_layout(Layout::DotEnv(layout))
            .with_source_bytes(bytes))
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
        let exported = match doc.layout() {
            Layout::DotEnv(layout) => Some(&layout.exported),
            _ => None,
        };
        for (key, val) in map {
            let s = match val {
                Value::String(s) => s.clone(),
                Value::Bool(b) => b.to_string(),
                Value::Int(n) => n.to_string(),
                Value::Float(n) => crate::document::shortest_float(*n),
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
            let export = if exported.is_some_and(|keys| keys.contains(key)) {
                "export "
            } else {
                ""
            };
            let line = format!("{export}{key}={}\n", quote_for_env(&s));
            w.write_all(line.as_bytes()).map_err(|source| Error::Io {
                path: Utf8PathBuf::from("<dotenv-writer>"),
                source,
            })?;
        }
        Ok(())
    }
}

/// What the dotenv reader saw of a file beyond its value tree
/// ([`Layout::DotEnv`]): the keys assigned with `export`, which the writer
/// writes with it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct DotEnvLayout {
    exported: HashSet<String>,
}

/// Parse the value side of a single `.env` line; `Err` says which quote it
/// opens and does not close.
fn parse_value(raw: &str) -> std::result::Result<String, &'static str> {
    // Skip leading whitespace; everything up to either a quote or end-of-line.
    let trimmed = raw.trim_start();
    if let Some(after) = trimmed.strip_prefix('"') {
        parse_double_quoted(after).ok_or("unterminated double-quoted value")
    } else if let Some(after) = trimmed.strip_prefix('\'') {
        // Literal — no escape processing.
        after
            .find('\'')
            .map(|end| after[..end].to_owned())
            .ok_or("unterminated single-quoted value")
    } else {
        Ok(parse_unquoted(trimmed))
    }
}

/// Parse a double-quoted value past its opening quote, applying backslash
/// escape sequences; `None` without a closing quote.
fn parse_double_quoted(input: &str) -> Option<String> {
    let mut chars = input.chars();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
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
    None
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

    /// `(line, col, span, snippet, message)` of the error `bytes` fail with.
    fn parse_error(bytes: &[u8]) -> (u32, u32, std::ops::Range<usize>, String, String) {
        match DotEnv.parse(bytes).expect_err("must fail") {
            Error::Parse {
                file: None,
                line,
                col,
                span,
                snippet,
                message,
            } => (line, col, span, snippet, message),
            other => panic!("expected a parse error, got {other:?}"),
        }
    }

    #[test]
    fn a_parse_error_is_at_the_text_at_fault() {
        for (text, line, col, span, snippet, message) in [
            (
                "A=1\r\n  я б \r\n",
                2,
                3,
                7..12,
                "  я б ",
                "expected `KEY=VALUE` in .env line",
            ),
            (
                "export  =x\n",
                1,
                9,
                8..9,
                "export  =x",
                "empty key in .env line",
            ),
            (
                "B= \"я\r\n",
                1,
                4,
                3..6,
                "B= \"я",
                "unterminated double-quoted value",
            ),
            (
                "B='x",
                1,
                3,
                2..4,
                "B='x",
                "unterminated single-quoted value",
            ),
            (
                "A=1\n B=\"x\\",
                2,
                4,
                7..10,
                " B=\"x\\",
                "unterminated double-quoted value",
            ),
        ] {
            assert_eq!(
                parse_error(text.as_bytes()),
                (line, col, span, snippet.to_owned(), message.to_owned()),
                "{text:?}"
            );
        }
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

    #[test]
    fn a_rewrite_keeps_export_on_the_keys_that_had_it() {
        let pointer = |p: &str| crate::pointer::Pointer::parse(p).unwrap();
        let mut doc = DotEnv
            .parse(b"export A=\"hello world\"\nB=2\n  export   C=x\n")
            .expect("parse");
        // A point edit, and a transform written with the file's layout.
        let transformed =
            Document::value_only(doc.value().clone(), FormatTag::DotEnv).with_layout_of(&doc);
        doc.set_at(&pointer("/B"), Value::Int(3)).expect("set");
        doc.set_at(&pointer("/D"), Value::Int(4)).expect("set");
        assert_eq!(
            String::from_utf8(doc.original_bytes().to_vec()).unwrap(),
            "export A=\"hello world\"\nB=3\nexport C=x\nD=4\n"
        );
        let mut buf = Vec::new();
        DotEnv.write(&transformed, &mut buf).expect("write");
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "export A=\"hello world\"\nB=2\nexport C=x\n"
        );
    }
}
