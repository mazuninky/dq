//! Newline-delimited JSON (JSONL / NDJSON).
//!
//! Read collapses the line stream into an `Array` so it is addressable by
//! `/<idx>`. Write emits one compact JSON line per element of the top-level
//! `Array`. Other shapes (single object, scalar) are written as a single
//! line. Multi-document streams are not representable.

use std::io::Write;

use crate::Result;
use crate::WriteOptions;
use crate::document::{Document, Value};
use crate::error::Error;
use crate::format::Format;
use crate::parsers::json::{Json, write_value_compact, write_value_pretty_with_step};
use crate::write_options::canonicalize_keys;

/// JSONL format implementation.
#[derive(Debug, Clone, Copy)]
pub struct Jsonl;

impl Format for Jsonl {
    fn name(&self) -> &'static str {
        "jsonl"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["jsonl", "ndjson"]
    }

    fn parse(&self, bytes: &[u8]) -> Result<Document> {
        // Splitting on `\n` keeps the final partial line if the file isn't
        // newline-terminated. `is_empty` skip handles the trailing-newline
        // case so we don't emit a stray `Null` at the end.
        let text =
            std::str::from_utf8(bytes).map_err(|e| Error::invalid_utf8(bytes, e, "JSONL"))?;
        let mut items: Vec<Value> = Vec::new();
        let mut line_start = 0;
        for (lineno, raw) in text.split('\n').enumerate() {
            let start = line_start;
            line_start += raw.len() + 1;
            let line = raw.trim_end_matches('\r');
            if line.trim().is_empty() {
                continue;
            }
            let doc = Json
                .parse(line.as_bytes())
                .map_err(|e| e.relocated(line.as_bytes(), bytes, |at| start + at))?;
            if doc.is_multi() {
                // Each line must be a single JSON value; embedded multi
                // documents would mean unexpected structure.
                return Err(Error::Format {
                    format: "jsonl",
                    message: format!("line {} produced a multi-document value", lineno + 1),
                });
            }
            // `Document::value()` always returns the parsed top-level
            // value for a single-doc document; clone so we can move it
            // into our accumulating array.
            items.push(doc.value().clone());
        }
        // Point edits rewrite the file whole, records addressed by index
        // (`Document::edits_by_rewrite`); its bytes stay for an edit that
        // changes nothing.
        Ok(
            Document::value_only(Value::Array(items), crate::document::FormatTag::Jsonl)
                .with_source_bytes(bytes),
        )
    }

    fn write(&self, doc: &Document, w: &mut dyn Write) -> Result<()> {
        if doc.is_multi() {
            return Err(Error::Format {
                format: "jsonl",
                message: "multi-doc not supported in jsonl".to_owned(),
            });
        }
        match doc.value() {
            Value::Array(items) => {
                for item in items {
                    write_value_compact(item, w)?;
                    write_io(w, b"\n")?;
                }
                Ok(())
            }
            other => {
                // Non-array shapes are written as a single line; this matches
                // user expectations for `dq convert input.json -F jsonl`
                // when the input isn't an array.
                write_value_compact(other, w)?;
                write_io(w, b"\n")
            }
        }
    }

    fn write_with_options(
        &self,
        doc: &Document,
        w: &mut dyn Write,
        opts: &WriteOptions,
    ) -> Result<()> {
        // Default shape: delegate to `write` so the bytes match the M2
        // baseline exactly. Any non-default option forces a per-line re-emit
        // path that honours `sort_keys` and `indent`.
        if !opts.sort_keys && opts.indent.is_none() {
            return self.write(doc, w);
        }
        if doc.is_multi() {
            return Err(Error::Format {
                format: "jsonl",
                message: "multi-doc not supported in jsonl".to_owned(),
            });
        }
        // Helper closure: emit a single record honouring `opts.indent`. The
        // `--indent` flag on a JSONL writer is unusual (one record per line
        // is the whole point of the format) but documented as supported for
        // symmetry with JSON: `Some(0)` keeps the per-line compact shape,
        // `Some(n)` indents the inner structure but each record still ends
        // with a newline that separates it from the next record.
        let emit_one = |item: &Value, w: &mut dyn Write| -> Result<()> {
            let item = if opts.sort_keys {
                canonicalize_keys(item)
            } else {
                item.clone()
            };
            match opts.indent {
                Some(0) | None => write_value_compact(&item, w),
                Some(n) => write_value_pretty_with_step(&item, w, 0, n as usize),
            }
        };
        match doc.value() {
            Value::Array(items) => {
                for item in items {
                    emit_one(item, w)?;
                    write_io(w, b"\n")?;
                }
                Ok(())
            }
            other => {
                emit_one(other, w)?;
                write_io(w, b"\n")
            }
        }
    }
}

fn write_io(w: &mut dyn Write, bytes: &[u8]) -> Result<()> {
    w.write_all(bytes).map_err(|source| Error::Io {
        path: camino::Utf8PathBuf::from("<jsonl-writer>"),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Document {
        Jsonl.parse(s.as_bytes()).unwrap()
    }

    #[test]
    fn parse_one_line_per_record() {
        let doc = parse("{\"a\":1}\n{\"a\":2}\n{\"a\":3}\n");
        let Value::Array(items) = doc.value() else {
            panic!()
        };
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn parse_skips_blank_lines() {
        let doc = parse("\n{\"a\":1}\n\n{\"a\":2}\n");
        let Value::Array(items) = doc.value() else {
            panic!()
        };
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn a_parse_error_is_placed_in_the_file() {
        for (bytes, line, col, span, snippet) in [
            ("{\"a\":1}\r\n\r\n{\"я\":,}\r\n", 3, 7, 17..18, "{\"я\":,}"),
            // The end of a line cut short is the end of that line.
            ("[1\n[2]\n", 1, 3, 2..2, "[1"),
            ("\u{feff}[,]\n", 1, 2, 4..5, "[,]"),
        ] {
            match Jsonl.parse(bytes.as_bytes()).expect_err("must fail") {
                Error::Parse {
                    line: at_line,
                    col: at_col,
                    span: at_span,
                    snippet: at_snippet,
                    ..
                } => assert_eq!(
                    (at_line, at_col, at_span, at_snippet.as_str()),
                    (line, col, span, snippet),
                    "{bytes:?}"
                ),
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn write_each_array_item_on_own_line() {
        let doc = parse("{\"a\":1}\n{\"a\":2}\n");
        let mut buf: Vec<u8> = Vec::new();
        Jsonl.write(&doc, &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert_eq!(out.lines().count(), 2);
    }

    #[test]
    fn write_rejects_multi_doc() {
        let doc = Document::multi(vec![Value::Int(1)]);
        let mut buf: Vec<u8> = Vec::new();
        let err = Jsonl.write(&doc, &mut buf).unwrap_err();
        assert_eq!(err.kind_name(), "format");
    }
}
