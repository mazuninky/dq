//! NaN and infinity — YAML `.nan` / `.inf`, TOML `nan` / `inf` — through
//! the format writers and through textual edits. JSON has no token for
//! them, YAML and TOML each spell them their own way.

use dq_core::{Document, Error, Pointer, Value, by_name, parse_yaml_with_spans};
use pretty_assertions::assert_eq;

const NON_FINITE_YAML: &str = "x: .nan\ny: -.inf\nl: [.Inf]\n";

fn yaml(src: &str) -> Document {
    parse_yaml_with_spans(src.as_bytes()).expect("YAML source parses")
}

fn parse(format: &str, src: &str) -> Document {
    by_name(format)
        .expect("format registered")
        .parse(src.as_bytes())
        .unwrap_or_else(|err| panic!("{format} source parses: {err}"))
}

/// `doc` as `format`'s writer emits it; `doc` parsed by a format's own
/// `parse`, so with no source bytes for the writer to copy.
fn write(format: &str, doc: &Document) -> Result<String, Error> {
    let mut buf = Vec::new();
    by_name(format)
        .expect("format registered")
        .write(doc, &mut buf)?;
    Ok(String::from_utf8(buf).expect("UTF-8"))
}

fn ptr(pointer: &str) -> Pointer {
    Pointer::parse(pointer).expect("pointer parses")
}

#[test]
fn the_json_writer_refuses_a_yaml_nan_or_infinity() {
    let err = write("json", &parse("yaml", NON_FINITE_YAML)).expect_err("JSON has no NaN");
    assert!(
        matches!(err, Error::Format { format: "json", .. }),
        "{err:?}"
    );
    // A literal past `f64` is still a JSON number.
    let out = write("json", &parse("yaml", "z: 1e400\n")).expect("writes");
    assert!(out.contains("1e400"), "{out}");
}

#[test]
fn the_yaml_writer_keeps_nan_and_infinity_floats() {
    assert_eq!(
        write("yaml", &parse("yaml", NON_FINITE_YAML)).expect("writes"),
        // `'y'`: the key is a bool to a YAML 1.1 reader.
        "x: .nan\n'y': -.inf\nl:\n- .inf\n"
    );
}

#[test]
fn the_toml_writer_spells_a_yaml_nan_or_infinity_its_own_way() {
    assert_eq!(
        write("toml", &parse("yaml", NON_FINITE_YAML)).expect("writes"),
        "x = nan\ny = -inf\nl = [inf]\n"
    );
}

#[test]
fn a_toml_set_of_a_yaml_nan_writes_toml_nan() {
    let value = yaml("v: .nan\nw: -.inf\n").value().clone();
    let mut doc = parse("toml", "j = 1\nk = 2\n");
    doc.set_at(&ptr("/j"), value).expect("set");
    assert_eq!(
        String::from_utf8_lossy(doc.original_bytes()),
        "j = { v = nan, w = -inf }\nk = 2\n"
    );
    let mut doc = parse("toml", "j = 1\n");
    doc.set_at(&ptr("/z"), Value::BigFloat(".inf".to_owned()))
        .expect("set");
    assert_eq!(
        String::from_utf8_lossy(doc.original_bytes()),
        "j = 1\nz = inf\n"
    );
}

#[test]
fn an_edit_next_to_a_toml_nan_reads_back() {
    let mut doc = parse("toml", "a = nan\nb = 1\n");
    doc.set_at(&ptr("/b"), Value::Int(2)).expect("set");
    assert_eq!(
        String::from_utf8_lossy(doc.original_bytes()),
        "a = nan\nb = 2\n"
    );
}

#[test]
fn a_json_set_of_nan_or_infinity_is_refused_without_a_way_around() {
    for value in [
        Value::BigFloat(".nan".to_owned()),
        Value::Float(f64::NEG_INFINITY),
        yaml("v: [1, .inf]\n").value().clone(),
    ] {
        let src = r#"{"j": 1}"#;
        let mut doc = parse("json", src);
        match doc.set_at(&ptr("/j"), value.clone()) {
            Err(Error::EditRejected { reason, .. }) => {
                assert_eq!(reason, "JSON has no NaN or Infinity");
            }
            other => panic!("set {value:?}: expected EditRejected, got {other:?}"),
        }
        assert_eq!(doc.original_bytes(), src.as_bytes(), "bytes untouched");
    }
}
