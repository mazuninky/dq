//! Component tests for the Dockerfile parser (read-only).
//!
//! The Dockerfile format is read-only by design (D6). Stage 2's inline
//! tests cover one happy path + the read-only write rejection. These tests
//! pin extension/filename detection, the COPY argument shape, and the
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
    // `line` is the 1-based instruction index in the M5 baseline (no real
    // line numbers from the upstream crate). Pinning it as Int(1) anchors the
    // contract so the field cannot quietly become `Null`.
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
fn parse_copy_with_two_args_emits_array_arguments() {
    // `COPY src dst` parses as two strings (sources + destination). The
    // implementation pushes destination onto the sources vec and runs the
    // result through `string_or_array`, which yields an Array when len > 1.
    let doc = dockerfile()
        .parse(b"COPY src dst\n")
        .expect("COPY must parse");
    let Value::Array(items) = doc.value() else {
        panic!()
    };
    let Value::Map(row) = &items[0] else { panic!() };
    assert_eq!(row.get("instruction"), Some(&Value::String("COPY".into())));
    let Some(Value::Array(args)) = row.get("arguments") else {
        panic!(
            "COPY with two args must produce Array<String>, got: {:?}",
            row.get("arguments"),
        );
    };
    assert_eq!(args.len(), 2);
    assert_eq!(args[0], Value::String("src".into()));
    assert_eq!(args[1], Value::String("dst".into()));
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
    // Dockerfile is read-only in M5. `Format::write` must return
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
            assert!(
                message.contains("read-only"),
                "message must mention `read-only`, got: {message:?}",
            );
        }
        other => panic!("expected Error::Format, got: {other:?}"),
    }
}
