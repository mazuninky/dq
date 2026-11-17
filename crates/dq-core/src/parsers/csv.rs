//! CSV (comma) and TSV (tab) parser/writer via the `csv` crate.
//!
//! Top-level shape: `Array<Map<column, String>>`. Every cell is `Value::String` —
//! no numeric / boolean / null type inference (per design D3).
//!
//! Both `Csv` and `Tsv` delegate to a private helper that takes the delimiter
//! byte; the only `Format` impl difference is `name()`/`extensions()` and the
//! delimiter passed down to the `csv::ReaderBuilder` / `csv::WriterBuilder`.

use std::io::Write;

use camino::Utf8PathBuf;
use indexmap::IndexMap;

use crate::Result;
use crate::document::{Document, FormatTag, Value};
use crate::error::Error;
use crate::format::Format;

/// CSV format implementation (`,` delimiter).
#[derive(Debug, Clone, Copy)]
pub struct Csv;

/// TSV format implementation (`\t` delimiter).
#[derive(Debug, Clone, Copy)]
pub struct Tsv;

impl Format for Csv {
    fn name(&self) -> &'static str {
        "csv"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["csv"]
    }

    fn parse(&self, bytes: &[u8]) -> Result<Document> {
        let value = parse_with_delimiter(bytes, b',', "csv")?;
        Ok(Document::value_only(value, FormatTag::Csv))
    }

    fn write(&self, doc: &Document, w: &mut dyn Write) -> Result<()> {
        write_with_delimiter(doc, w, b',', "csv")
    }
}

impl Format for Tsv {
    fn name(&self) -> &'static str {
        "tsv"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["tsv"]
    }

    fn parse(&self, bytes: &[u8]) -> Result<Document> {
        let value = parse_with_delimiter(bytes, b'\t', "tsv")?;
        Ok(Document::value_only(value, FormatTag::Tsv))
    }

    fn write(&self, doc: &Document, w: &mut dyn Write) -> Result<()> {
        write_with_delimiter(doc, w, b'\t', "tsv")
    }
}

/// Parse `bytes` as delimited records into `Value::Array<Value::Map>`.
fn parse_with_delimiter(bytes: &[u8], delimiter: u8, format_name: &'static str) -> Result<Value> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .delimiter(delimiter)
        .from_reader(bytes);

    // Materialise the header into an owned `Vec<String>` — the borrow is
    // released before the records iterator runs.
    let upper = format_name.to_uppercase();
    let headers: Vec<String> = match rdr.headers() {
        Ok(h) => h.iter().map(str::to_owned).collect(),
        Err(e) => {
            return Err(Error::Parse {
                file: None,
                line: 0,
                col: 0,
                span: 0..0,
                snippet: String::new(),
                message: format!("failed to read {upper} header: {e}"),
            });
        }
    };

    let mut rows: Vec<Value> = Vec::new();
    for (record_idx, result) in rdr.records().enumerate() {
        let record = result.map_err(|e| {
            // `csv::Error::position()` is only set for parse-time errors; the
            // 1-indexed line is what users care about.
            let pos = e.position().map(|p| p.line()).unwrap_or(0);
            Error::Parse {
                file: None,
                line: pos as u32,
                col: 0,
                span: 0..0,
                snippet: String::new(),
                message: format!("{upper} record {record_idx}: {e}"),
            }
        })?;
        let mut row: IndexMap<String, Value> = IndexMap::with_capacity(headers.len());
        for (i, cell) in record.iter().enumerate() {
            let key = headers
                .get(i)
                .cloned()
                .unwrap_or_else(|| format!("column_{i}"));
            row.insert(key, Value::String(cell.to_owned()));
        }
        rows.push(Value::Map(row));
    }
    Ok(Value::Array(rows))
}

/// Write a `Value::Array<Value::Map>` document as delimited records.
fn write_with_delimiter(
    doc: &Document,
    w: &mut dyn Write,
    delimiter: u8,
    format_name: &'static str,
) -> Result<()> {
    let Value::Array(items) = doc.value() else {
        return Err(Error::Format {
            format: format_name,
            message: format!(
                "expected array of objects at top level, got {}",
                doc.value().type_name(),
            ),
        });
    };
    let mut wtr = csv::WriterBuilder::new()
        .delimiter(delimiter)
        .from_writer(w);

    if items.is_empty() {
        return wtr.flush().map_err(|source| Error::Io {
            path: Utf8PathBuf::from(format!("<{format_name}-writer>")),
            source,
        });
    }

    // Header is taken from the first map's keys in order. Subsequent maps
    // must share the header set; missing keys emit empty cells, extra keys
    // surface as Error::Format so callers see a clean diagnostic.
    let Value::Map(first) = &items[0] else {
        return Err(Error::Format {
            format: format_name,
            message: format!(
                "expected array of objects at top level; row 0 is {}",
                items[0].type_name(),
            ),
        });
    };
    let headers: Vec<String> = first.keys().cloned().collect();
    wtr.write_record(&headers)
        .map_err(|e| map_delim_write_err(format_name, e))?;

    for (idx, row) in items.iter().enumerate() {
        let Value::Map(map) = row else {
            return Err(Error::Format {
                format: format_name,
                message: format!(
                    "expected array of objects; row {idx} is {}",
                    row.type_name(),
                ),
            });
        };
        let mut cells: Vec<String> = Vec::with_capacity(headers.len());
        for h in &headers {
            match map.get(h) {
                Some(Value::String(s)) => cells.push(s.clone()),
                Some(Value::Bool(b)) => cells.push(b.to_string()),
                Some(Value::Int(n)) => cells.push(n.to_string()),
                Some(Value::Float(n)) => cells.push(n.to_string()),
                Some(Value::BigInt(s)) | Some(Value::BigFloat(s)) => cells.push(s.clone()),
                Some(Value::Null) => cells.push(String::new()),
                Some(Value::Array(_) | Value::Map(_)) => {
                    return Err(Error::Format {
                        format: format_name,
                        message: format!(
                            "row {idx} key '{h}': nested {} cannot be serialized to {format_name}",
                            map.get(h).expect("just matched").type_name(),
                        ),
                    });
                }
                None => cells.push(String::new()),
            }
        }
        // Surface unexpected extra keys (header is the set of keys from row 0).
        for k in map.keys() {
            if !headers.iter().any(|h| h == k) {
                return Err(Error::Format {
                    format: format_name,
                    message: format!("row {idx} has key '{k}' not present in header {headers:?}",),
                });
            }
        }
        wtr.write_record(&cells)
            .map_err(|e| map_delim_write_err(format_name, e))?;
    }
    wtr.flush().map_err(|source| Error::Io {
        path: Utf8PathBuf::from(format!("<{format_name}-writer>")),
        source,
    })
}

fn map_delim_write_err(format_label: &'static str, e: csv::Error) -> Error {
    Error::Format {
        format: format_label,
        message: format!("{} write failed: {e}", format_label.to_uppercase()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_csv_array_of_maps() {
        let doc = Csv.parse(b"name\nalice\n").expect("parse");
        let Value::Array(items) = doc.value() else {
            panic!("expected array")
        };
        assert_eq!(items.len(), 1);
        let Value::Map(row) = &items[0] else {
            panic!("expected row map")
        };
        assert_eq!(row.get("name"), Some(&Value::String("alice".into())));
    }

    #[test]
    fn parse_tsv_uses_tab_delimiter() {
        let doc = Tsv.parse(b"name\tage\nalice\t30\n").expect("parse tsv");
        let Value::Array(items) = doc.value() else {
            panic!()
        };
        assert_eq!(items.len(), 1);
        let Value::Map(row) = &items[0] else { panic!() };
        assert_eq!(row.get("name"), Some(&Value::String("alice".into())));
        assert_eq!(row.get("age"), Some(&Value::String("30".into())));
    }

    #[test]
    fn write_rejects_non_array_top_level() {
        let mut m = IndexMap::new();
        m.insert("a".into(), Value::Int(1));
        let doc = Document::value_only(Value::Map(m), FormatTag::Csv);
        let mut buf: Vec<u8> = Vec::new();
        let err = Csv.write(&doc, &mut buf).expect_err("must error");
        assert_eq!(err.kind_name(), "format");
    }
}
