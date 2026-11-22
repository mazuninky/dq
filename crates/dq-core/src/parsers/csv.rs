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
use crate::error::{Error, invalid_utf8_span};
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
/// with no rows left — a header alone has nothing else to hold it — and,
/// for the tree the reader built and the point edits made to it, first,
/// every column of it whether a row still holds it or not.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct CsvLayout {
    header: Vec<String>,
    /// Whether the header's columns come first and stay: false once a
    /// filter rebuilt the tree ([`CsvLayout::rebuilt`]), whose rows then
    /// decide the columns and their order.
    kept: bool,
}

impl CsvLayout {
    /// This layout for a tree a filter rebuilt (`set --jq`, `fix.jq`):
    /// the header only for a tree with no rows left.
    pub(crate) fn rebuilt(&self) -> Self {
        Self {
            header: self.header.clone(),
            kept: false,
        }
    }
}

/// The columns to write `rows` with: the header the file was read with,
/// its columns in its order, when `layout` keeps it; then every other key
/// of every row, in the order first seen. The read-back check reads a row
/// back with every one of them, a missing one empty.
pub(crate) fn columns<'a>(layout: &'a Layout, rows: &'a [Value]) -> Vec<&'a str> {
    let mut columns: IndexMap<&str, ()> = IndexMap::new();
    if let Layout::Csv(CsvLayout { header, kept: true }) = layout {
        for column in header {
            columns.entry(column.as_str()).or_insert(());
        }
    }
    for row in rows {
        if let Value::Map(cells) = row {
            for key in cells.keys() {
                columns.entry(key.as_str()).or_insert(());
            }
        }
    }
    columns.into_keys().collect()
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
            return Err(parse_error(
                bytes,
                &e,
                &format!("failed to read {upper} header"),
            ));
        }
    };

    let mut rows: Vec<Value> = Vec::new();
    for (record_idx, result) in rdr.records().enumerate() {
        let record =
            result.map_err(|e| parse_error(bytes, &e, &format!("{upper} record {record_idx}")))?;
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
        Layout::Csv(CsvLayout {
            header: headers,
            kept: true,
        }),
    ))
}

/// `err`, which the `csv` crate raised on `bytes`, as an [`Error::Parse`]
/// at the start of the record it is about — at the first bad byte, when
/// that is one that is no UTF-8 — and worded `{what}: ` and the crate's
/// message without its record position.
fn parse_error(bytes: &[u8], err: &csv::Error, what: &str) -> Error {
    let record = err
        .position()
        .and_then(|p| usize::try_from(p.byte()).ok())
        .unwrap_or_default()
        .min(bytes.len());
    // Where a record begins the crate has not yet passed over the rest of
    // the line break before it (`\n` after `\r`) and blank lines.
    let start = record
        + bytes[record..]
            .iter()
            .take_while(|b| matches!(b, b'\r' | b'\n'))
            .count();
    let (span, cause) = match err.kind() {
        csv::ErrorKind::Utf8 { err, .. } => (
            std::str::from_utf8(&bytes[start..]).err().map(|e| {
                let bad = invalid_utf8_span(&bytes[start..], e);
                start + bad.start..start + bad.end
            }),
            format!("CSV parse error: invalid UTF-8 in field {}", err.field()),
        ),
        csv::ErrorKind::UnequalLengths {
            expected_len, len, ..
        } => (
            None,
            format!(
                "CSV error: found record with {len} fields, but the previous record has \
                 {expected_len} fields"
            ),
        ),
        _ => (None, err.to_string()),
    };
    Error::parse_at(
        bytes,
        span.unwrap_or(start..start),
        format!("{what}: {cause}"),
    )
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
        if let Layout::Csv(CsvLayout { header, .. }) = doc.layout()
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

    // The header is the file's, then every other key of every row, in the
    // order first seen ([`columns`]); a row without one of them has an
    // empty cell there.
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
        rows.push(map);
    }
    let headers = columns(doc.layout(), items);
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

    /// `(line, col, span, snippet, message)` of the error `bytes` fail with.
    fn parse_error(bytes: &[u8]) -> (u32, u32, std::ops::Range<usize>, String, String) {
        match Csv.parse(bytes).expect_err("must fail") {
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
    fn a_parse_error_is_at_its_record() {
        let uneven = "CSV record 1: CSV error: found record with 1 fields, but the previous record \
                      has 2 fields";
        for (text, line, span) in [
            ("a,b\n1,2\n3\n", 3, 8..8),
            // A line break and blank lines before it; a record of two lines.
            ("a,b\r\n1,2\r\n\r\n3\r\n", 4, 12..12),
            ("a,b\n\"x\ny\",2\n3\n", 4, 12..12),
            ("\u{feff}a,b\n1,2\n3", 3, 11..11),
            ("a,b\r1,2\r3\r", 3, 8..8),
        ] {
            assert_eq!(
                parse_error(text.as_bytes()),
                (line, 1, span, "3".to_owned(), uneven.to_owned()),
                "{text:?}"
            );
        }
    }

    #[test]
    fn bytes_that_are_no_utf8_are_placed_at_the_first_of_them() {
        assert_eq!(
            parse_error(b"a,b\n\xD1\x8F,x\xFF\n"),
            (
                2,
                5,
                8..9,
                "я,x\u{fffd}".to_owned(),
                "CSV record 0: CSV parse error: invalid UTF-8 in field 1".to_owned()
            )
        );
        assert_eq!(
            parse_error(b"a,\xFF\n1,2\n"),
            (
                1,
                3,
                2..3,
                "a,\u{fffd}".to_owned(),
                "failed to read CSV header: CSV parse error: invalid UTF-8 in field 1".to_owned()
            )
        );
    }

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
