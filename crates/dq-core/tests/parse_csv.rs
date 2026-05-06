//! Component tests for the CSV / TSV parser/writer.
//!
//! Stage 2's inline tests cover three minimal cases. These tests pin the
//! rest of the contract: TSV delimiter, write-side validation (non-array top
//! level / non-uniform keys), and the "every cell is `Value::String`" rule.

use std::io::{self, Write};

use dq_core::{Document, Format, FormatTag, Value};
use indexmap::IndexMap;
use pretty_assertions::assert_eq;

fn csv() -> &'static dyn Format {
    dq_core::by_name("csv").expect("csv format must be registered")
}

fn tsv() -> &'static dyn Format {
    dq_core::by_name("tsv").expect("tsv format must be registered")
}

#[test]
fn parse_two_row_csv_produces_array_of_two_maps() {
    // `name,age\nalice,30\nbob,25\n` — the header `name,age` becomes the
    // shared map keys; each subsequent line becomes one Map entry in the
    // outer Array. Cells are strings even when they look numeric.
    let doc = csv()
        .parse(b"name,age\nalice,30\nbob,25\n")
        .expect("simple CSV must parse");
    let Value::Array(items) = doc.value() else {
        panic!("expected top-level Array, got: {:?}", doc.value());
    };
    assert_eq!(
        items.len(),
        2,
        "expected exactly 2 data rows, got {}",
        items.len()
    );
    let Value::Map(row0) = &items[0] else {
        panic!("row 0 must be Map, got: {:?}", items[0]);
    };
    assert_eq!(row0.get("name"), Some(&Value::String("alice".into())));
    assert_eq!(
        row0.get("age"),
        Some(&Value::String("30".into())),
        "numeric-looking cells stay as Value::String per design D3",
    );
}

#[test]
fn parse_tsv_uses_tab_delimiter() {
    // `Tsv` must dispatch its delimiter through the same shared helper
    // without falling back to comma. Pin the contract by feeding tab-
    // delimited bytes; a comma-delimited input would parse as a single
    // mega-column.
    let doc = tsv()
        .parse(b"name\tage\nalice\t30\n")
        .expect("simple TSV must parse");
    let Value::Array(items) = doc.value() else {
        panic!()
    };
    let Value::Map(row) = &items[0] else { panic!() };
    assert_eq!(row.get("age"), Some(&Value::String("30".into())));
    assert_eq!(row.get("name"), Some(&Value::String("alice".into())));
}

#[test]
fn write_array_of_maps_with_consistent_keys_emits_csv_with_header() {
    // Construct a minimal `Array<Map>` with shared keys; the writer must
    // emit a header row taken from the first map's key order, then one row
    // per item with cells in that header's order.
    let mut row0 = IndexMap::new();
    row0.insert("name".to_string(), Value::String("alice".into()));
    row0.insert("age".to_string(), Value::String("30".into()));
    let mut row1 = IndexMap::new();
    row1.insert("name".to_string(), Value::String("bob".into()));
    row1.insert("age".to_string(), Value::String("25".into()));
    let arr = Value::Array(vec![Value::Map(row0), Value::Map(row1)]);
    let doc = Document::value_only(arr, FormatTag::Csv);
    let mut buf: Vec<u8> = Vec::new();
    csv()
        .write(&doc, &mut buf)
        .expect("write of well-shaped Array<Map> must succeed");
    let s = String::from_utf8(buf).expect("csv writer produces utf-8");
    assert_eq!(
        s, "name,age\nalice,30\nbob,25\n",
        "expected canonical CSV with header taken from row 0's key order",
    );
}

#[test]
fn write_rejects_non_array_top_level_with_format_error() {
    // CSV can only express `Array<Map>` at the top level. A scalar / Map at
    // the root must surface as `Error::Format` with the format name set.
    let doc = Document::value_only(
        {
            let mut m = IndexMap::new();
            m.insert("a".into(), Value::Int(1));
            Value::Map(m)
        },
        FormatTag::Csv,
    );
    let mut buf: Vec<u8> = Vec::new();
    let err = csv()
        .write(&doc, &mut buf)
        .expect_err("non-array top level must error");
    match err {
        dq_core::Error::Format { format, .. } => {
            assert_eq!(format, "csv", "expected `csv` format error tag");
        }
        other => panic!("expected Error::Format, got: {other:?}"),
    }
}

#[test]
fn write_rejects_inconsistent_keys_with_message_naming_offending_row() {
    // Per implementation: the header is the key set of `items[0]`. A
    // subsequent row whose keys differ is rejected with a message naming the
    // offending row index — pin that contract so the diagnostic stays
    // actionable for users.
    let mut row0 = IndexMap::new();
    row0.insert("a".into(), Value::String("1".into()));
    let mut row1_extra = IndexMap::new();
    row1_extra.insert("a".into(), Value::String("2".into()));
    row1_extra.insert("b".into(), Value::String("rogue".into()));
    let arr = Value::Array(vec![Value::Map(row0), Value::Map(row1_extra)]);
    let doc = Document::value_only(arr, FormatTag::Csv);
    let mut buf: Vec<u8> = Vec::new();
    let err = csv()
        .write(&doc, &mut buf)
        .expect_err("non-uniform keys must surface as Error::Format");
    match err {
        dq_core::Error::Format { message, .. } => {
            // Row index 1 carries the rogue key.
            assert!(
                message.contains("row 1"),
                "error message must name the offending row index, got: {message:?}",
            );
        }
        other => panic!("expected Error::Format, got: {other:?}"),
    }
}

/// `Write` adapter that fails on every call. Used to force the `csv` crate
/// to surface its own `csv::Error` so we can pin the format-label threading
/// through `map_delim_write_err` (the M5 bug fix: the helper previously
/// hardcoded `"csv"` even when called from the TSV writer path).
struct FailingWriter;
impl Write for FailingWriter {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("forced write failure"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("forced flush failure"))
    }
}

#[test]
fn tsv_write_format_error_carries_tsv_label_not_csv() {
    // Regression for the M5 bug where the private write-error helper
    // hardcoded `"csv"`, mislabeling write failures raised from the TSV
    // path. The shape-mismatch path (extra key in row 1) reliably reaches
    // `Error::Format`; pin its `format` field to `"tsv"`.
    let mut row0 = IndexMap::new();
    row0.insert("a".to_string(), Value::String("1".into()));
    let mut row1 = IndexMap::new();
    row1.insert("a".to_string(), Value::String("2".into()));
    row1.insert("b".to_string(), Value::String("rogue".into()));
    let arr = Value::Array(vec![Value::Map(row0), Value::Map(row1)]);
    let doc = Document::value_only(arr, FormatTag::Tsv);
    let mut buf: Vec<u8> = Vec::new();
    let err = tsv()
        .write(&doc, &mut buf)
        .expect_err("non-uniform keys must surface as Error::Format");
    match err {
        dq_core::Error::Format { format, .. } => {
            assert_eq!(
                format, "tsv",
                "TSV write error must label format as `tsv`, not `csv`",
            );
        }
        other => panic!("expected Error::Format, got: {other:?}"),
    }
}

#[test]
fn tsv_write_io_error_uses_tsv_writer_sentinel_not_csv_writer() {
    // The other half of the M5 bug: the IO-failure path also hardcoded the
    // sentinel path as `<csv-writer>`. Force an IO failure by handing the
    // writer a sink that errors on flush — the `csv` crate buffers internally
    // so write failures surface at flush time. Assert the sentinel reflects
    // the active format.
    let mut row = IndexMap::new();
    row.insert("a".to_string(), Value::String("1".into()));
    let arr = Value::Array(vec![Value::Map(row)]);
    let doc = Document::value_only(arr, FormatTag::Tsv);

    let err = tsv()
        .write(&doc, &mut FailingWriter)
        .expect_err("failing writer must surface as Error::Io");
    match err {
        dq_core::Error::Io { path, .. } => {
            assert_eq!(
                path.as_str(),
                "<tsv-writer>",
                "TSV IO error sentinel must name `tsv`, not `csv`",
            );
        }
        other => panic!("expected Error::Io, got: {other:?}"),
    }
}

#[test]
fn csv_write_io_error_keeps_csv_writer_sentinel() {
    // Companion test for the comma path so the sentinel can't regress.
    let mut row = IndexMap::new();
    row.insert("a".to_string(), Value::String("1".into()));
    let arr = Value::Array(vec![Value::Map(row)]);
    let doc = Document::value_only(arr, FormatTag::Csv);

    let err = csv()
        .write(&doc, &mut FailingWriter)
        .expect_err("failing writer must surface as Error::Io");
    match err {
        dq_core::Error::Io { path, .. } => {
            assert_eq!(
                path.as_str(),
                "<csv-writer>",
                "CSV IO error sentinel must remain `<csv-writer>`",
            );
        }
        other => panic!("expected Error::Io, got: {other:?}"),
    }
}

#[test]
fn parse_keeps_every_cell_as_string_no_numeric_inference() {
    // Spec D3: the parser does NOT infer types — cells like `"30"` and `"true"`
    // and `""` all stay strings. Anti-test for the obvious "infer ints"
    // regression.
    let doc = csv()
        .parse(b"a,b,c\n30,true,\n")
        .expect("CSV with mixed-looking cells must parse");
    let Value::Array(items) = doc.value() else {
        panic!()
    };
    let Value::Map(row) = &items[0] else { panic!() };
    assert!(
        matches!(row.get("a"), Some(Value::String(_))),
        "cell a must be String, got: {:?}",
        row.get("a"),
    );
    assert!(
        matches!(row.get("b"), Some(Value::String(_))),
        "cell b must be String even though it looks like a bool, got: {:?}",
        row.get("b"),
    );
    assert!(
        matches!(row.get("c"), Some(Value::String(s)) if s.is_empty()),
        "empty cell must be Value::String(\"\"), got: {:?}",
        row.get("c"),
    );
}
