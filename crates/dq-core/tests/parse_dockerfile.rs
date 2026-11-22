//! Component tests for the Dockerfile parser (read-only).
//!
//! The Dockerfile format is read-only by design (D6). These tests pin
//! extension/filename detection, the source-line `line` field, the COPY
//! argument shape (shell form → string, JSON form → array), and the
//! read-only write contract.

use camino::Utf8Path;
use dq_core::{Document, Format, FormatTag, Value};
use pretty_assertions::assert_eq;

fn dockerfile() -> &'static dyn Format {
    dq_core::by_name("dockerfile").expect("dockerfile format must be registered")
}

#[test]
fn parse_two_instructions_emits_array_of_two_maps() {
    // `FROM alpine:latest` + `RUN apk add curl` → 2 entries in the result
    // array, each with `instruction` / `arguments` / `line` fields.
    let doc = dockerfile()
        .parse(b"FROM alpine:latest\nRUN apk add curl\n")
        .expect("simple Dockerfile must parse");
    let Value::Array(items) = doc.value() else {
        panic!("expected top-level Array, got: {:?}", doc.value());
    };
    assert_eq!(
        items.len(),
        2,
        "expected 2 instructions, got {}",
        items.len()
    );

    let Value::Map(from_row) = &items[0] else {
        panic!("row 0 must be Map, got: {:?}", items[0]);
    };
    assert_eq!(
        from_row.get("instruction"),
        Some(&Value::String("FROM".into()))
    );
    assert_eq!(
        from_row.get("arguments"),
        Some(&Value::String("alpine:latest".into())),
        "FROM with no alias must use the String shape, not Array",
    );
    // `line` is the 1-based source line where the instruction starts.
    assert_eq!(from_row.get("line"), Some(&Value::Int(1)));

    let Value::Map(run_row) = &items[1] else {
        panic!()
    };
    assert_eq!(
        run_row.get("instruction"),
        Some(&Value::String("RUN".into()))
    );
    assert_eq!(run_row.get("line"), Some(&Value::Int(2)));
}

#[test]
fn parse_copy_shell_form_is_a_string_and_json_form_an_array() {
    // Shell form keeps the argument text as written; the JSON form keeps the
    // exact array elements.
    let doc = dockerfile()
        .parse(b"COPY src dst\nCOPY [\"a b\", \"dst\"]\n")
        .expect("COPY must parse");
    let Value::Array(items) = doc.value() else {
        panic!()
    };
    let Value::Map(row) = &items[0] else { panic!() };
    assert_eq!(row.get("instruction"), Some(&Value::String("COPY".into())));
    assert_eq!(
        row.get("arguments"),
        Some(&Value::String("src dst".into())),
        "shell-form COPY must produce the argument text as a String",
    );
    let Value::Map(row) = &items[1] else { panic!() };
    assert_eq!(
        row.get("arguments"),
        Some(&Value::Array(vec![
            Value::String("a b".into()),
            Value::String("dst".into()),
        ])),
        "JSON-form COPY must produce Array<String>",
    );
    assert_eq!(row.get("line"), Some(&Value::Int(2)));
}

#[test]
fn registry_detects_literal_dockerfile_filename_without_extension() {
    // Per spec, the literal `Dockerfile` (no extension) must dispatch to the
    // dockerfile parser via the FILENAME_FALLBACK table. This is the
    // realistic path users hit — most Dockerfiles have no extension at all.
    let fmt = dq_core::detect(Utf8Path::new("Dockerfile"))
        .expect("literal `Dockerfile` filename must resolve to the dockerfile parser");
    assert_eq!(fmt.name(), "dockerfile");
    // Sanity: `Containerfile` (Podman convention) also resolves.
    let fmt = dq_core::detect(Utf8Path::new("Containerfile")).expect("Containerfile must resolve");
    assert_eq!(fmt.name(), "dockerfile");
}

#[test]
fn registry_detects_dockerfile_extension() {
    // Some teams name auxiliary Dockerfiles `<service>.dockerfile`; the
    // registry must accept that extension too.
    let fmt = dq_core::detect(Utf8Path::new("api.dockerfile"))
        .expect("`.dockerfile` extension must resolve to the dockerfile parser");
    assert_eq!(fmt.name(), "dockerfile");
}

#[test]
fn write_returns_format_error_with_read_only_message() {
    // Dockerfiles are read-only. `Format::write` must return
    // `Error::Format` with the format tagged `dockerfile` and a message
    // mentioning "read-only" so users see the rejection cause without having
    // to look up the docs.
    let doc = Document::value_only(Value::Array(vec![]), FormatTag::Dockerfile);
    let mut buf: Vec<u8> = Vec::new();
    let err = dockerfile()
        .write(&doc, &mut buf)
        .expect_err("Dockerfile write must always error");
    match err {
        dq_core::Error::Format { format, message } => {
            assert_eq!(format, "dockerfile", "format tag must be `dockerfile`");
            assert_eq!(message, "Dockerfiles are read-only");
        }
        other => panic!("expected Error::Format, got: {other:?}"),
    }
}
