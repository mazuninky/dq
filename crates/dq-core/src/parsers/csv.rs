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
use crate::document::{Document, FormatTag, Layout, Value};
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
        let (value, layout) = parse_with_delimiter(bytes, b',', "csv")?;
        Ok(Document::value_only(value, FormatTag::Csv)
            .with_layout(layout)
            .with_source_bytes(bytes))
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
        let (value, layout) = parse_with_delimiter(bytes, b'\t', "tsv")?;
        Ok(Document::value_only(value, FormatTag::Tsv)
            .with_layout(layout)
            .with_source_bytes(bytes))
    }

    fn write(&self, doc: &Document, w: &mut dyn Write) -> Result<()> {
        write_with_delimiter(doc, w, b'\t', "tsv")
    }
}

/// What the CSV / TSV reader saw of a file beyond its value tree
/// ([`Layout::Csv`]): its header, which the writer writes for a document
/// with no rows left — a header alone has nothing else to hold it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct CsvLayout {
    header: Vec<String>,
}

/// Parse `bytes` as delimited records into `Value::Array<Value::Map>`, and
/// the header they were read with.
fn parse_with_delimiter(
    bytes: &[u8],
    delimiter: u8,
    format_name: &'static str,
) -> Result<(Value, Layout)> {
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
    Ok((
        Value::Array(rows),
        Layout::Csv(CsvLayout { header: headers }),
    ))
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

    // No rows: the header the file was read with, if any, alone.
    if items.is_empty() {
        if let Layout::Csv(CsvLayout { header }) = doc.layout()
            && !header.is_empty()
        {
            wtr.write_record(header)
                .map_err(|e| map_delim_write_err(format_name, e))?;
        }
        return wtr.flush().map_err(|source| Error::Io {
            path: Utf8PathBuf::from(format!("<{format_name}-writer>")),
            source,
        });
    }

    // The header is every key of every row, in the order first seen; a
    // row without one of them has an empty cell there.
    let mut columns: IndexMap<&str, ()> = IndexMap::new();
    let mut rows = Vec::with_capacity(items.len());
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
        for key in map.keys() {
            columns.entry(key.as_str()).or_insert(());
        }
        rows.push(map);
    }
    let headers: Vec<&str> = columns.into_keys().collect();
    wtr.write_record(&headers)
        .map_err(|e| map_delim_write_err(format_name, e))?;

    for (idx, map) in rows.into_iter().enumerate() {
        let mut cells: Vec<String> = Vec::with_capacity(headers.len());
        for h in &headers {
            match map.get(*h) {
                Some(Value::String(s)) => cells.push(s.clone()),
                Some(Value::Bool(b)) => cells.push(b.to_string()),
                Some(Value::Int(n)) => cells.push(n.to_string()),
                Some(Value::Float(n)) => cells.push(crate::document::shortest_float(*n)),
                Some(Value::BigInt(s)) | Some(Value::BigFloat(s)) => cells.push(s.clone()),
                Some(Value::Null) => cells.push(String::new()),
                Some(Value::Array(_) | Value::Map(_)) => {
                    return Err(Error::Format {
                        format: format_name,
                        message: format!(
                            "row {idx} key '{h}': nested {} cannot be serialized to {format_name}",
                            map.get(*h).expect("just matched").type_name(),
                        ),
                    });
                }
                None => cells.push(String::new()),
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
    fn a_document_with_no_rows_writes_the_header_it_was_read_with() {
        let mut doc = Csv.parse(b"a,b\n1,2\n").expect("parse");
        doc.del_at(&crate::pointer::Pointer::parse("/0").unwrap())
            .expect("del");
        assert_eq!(doc.original_bytes(), b"a,b\n");
        // Read from no file: no header to write.
        let mut buf: Vec<u8> = Vec::new();
        Csv.write(&Document::single(Value::Array(Vec::new())), &mut buf)
            .expect("write");
        assert!(buf.is_empty());
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

    #[test]
    fn the_header_is_the_union_of_the_keys_in_first_seen_order() {
        let row = |cells: &[(&str, &str)]| {
            Value::Map(
                cells
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), Value::String((*v).to_owned())))
                    .collect(),
            )
        };
        let rows = Value::Array(vec![
            row(&[("a", "1"), ("b", "2")]),
            row(&[("a", "3"), ("c", "4")]),
            row(&[("c", "5")]),
        ]);
        for (format, delimited) in [
            (&Csv as &dyn Format, "a,b,c\n1,2,\n3,,4\n,,5\n"),
            (&Tsv, "a\tb\tc\n1\t2\t\n3\t\t4\n\t\t5\n"),
        ] {
            let mut buf: Vec<u8> = Vec::new();
            format
                .write(&Document::single(rows.clone()), &mut buf)
                .expect("write");
            assert_eq!(String::from_utf8(buf).unwrap(), delimited);
        }
        // And through a point edit that appends a row with a new column.
        let mut doc = Csv.parse(b"a,b\n1,2\n").expect("parse");
        doc.set_at(
            &crate::pointer::Pointer::parse("/-").unwrap(),
            row(&[("a", "3"), ("c", "4")]),
        )
        .expect("set");
        assert_eq!(doc.original_bytes(), b"a,b,c\n1,2,\n3,,4\n");
    }
}
