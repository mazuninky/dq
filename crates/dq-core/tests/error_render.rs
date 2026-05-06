//! Snapshot tests for the structured shape of `dq_core::Error` payloads.
//!
//! `dq_core::Error` does not impl `serde::Serialize` directly (the variants
//! carry runtime types like `std::io::Error` and `std::ops::Range<usize>` that
//! aren't trivially serializable in a stable, redactable way). This test file
//! defines a small `ErrorView` newtype that renders the variant name, the
//! relevant fields, and stable shapes for spans / paths — then snapshots that
//! view via `insta::assert_yaml_snapshot!`.
//!
//! The snapshots live under `tests/snapshots/` and are generated on first
//! run. Reviewers run `cargo insta review` to accept or reject diffs.

use std::io;

use camino::Utf8PathBuf;
use dq_core::template_guard::TemplateMarker;
use dq_core::{Error, PathErrorKind, Pointer, Value};
use indexmap::IndexMap;
use serde::Serialize;

/// Serializable mirror of the relevant portions of `dq_core::Error`.
///
/// Kept narrow on purpose: only the fields the CLI's reporters surface to
/// users go in. If a future variant adds a transient runtime field, leave it
/// out — snapshot diffs should be human-meaningful.
#[derive(Serialize)]
#[serde(tag = "kind")]
enum ErrorView<'a> {
    #[serde(rename = "io")]
    Io { path: String, message: String },
    #[serde(rename = "write_io")]
    WriteIo { path: String, message: String },
    #[serde(rename = "parse")]
    Parse {
        file: Option<String>,
        line: u32,
        col: u32,
        #[serde(rename = "span_start")]
        span_start: usize,
        #[serde(rename = "span_end")]
        span_end: usize,
        snippet: &'a str,
        message: &'a str,
    },
    #[serde(rename = "path")]
    Path {
        pointer: &'a str,
        matched_prefix: &'a str,
        path_kind: PathKindView,
        did_you_mean: &'a [String],
    },
    #[serde(rename = "unsupported_format")]
    UnsupportedFormat { name: &'a str },
    #[serde(rename = "format")]
    Format { format: &'a str, message: &'a str },
    #[serde(rename = "write_unavailable")]
    WriteUnavailable { reason: &'a str },
    #[serde(rename = "templated_file")]
    TemplatedFile {
        line: u32,
        snippet: &'a str,
        hint: &'a str,
    },
    #[serde(rename = "patch_test_failed")]
    PatchTestFailed {
        pointer: &'a str,
        expected: String,
        actual: String,
    },
}

#[derive(Serialize)]
#[serde(tag = "kind")]
enum PathKindView {
    #[serde(rename = "MissingKey")]
    MissingKey,
    #[serde(rename = "OutOfBounds")]
    OutOfBounds,
    #[serde(rename = "TypeMismatch")]
    TypeMismatch {
        expected: &'static str,
        found: &'static str,
    },
}

impl From<&PathErrorKind> for PathKindView {
    fn from(value: &PathErrorKind) -> Self {
        match value {
            PathErrorKind::MissingKey => Self::MissingKey,
            PathErrorKind::OutOfBounds => Self::OutOfBounds,
            PathErrorKind::TypeMismatch { expected, found } => {
                Self::TypeMismatch { expected, found }
            }
        }
    }
}

fn view<'a>(e: &'a Error) -> ErrorView<'a> {
    match e {
        Error::Io { path, source } => ErrorView::Io {
            path: path.to_string(),
            message: source.to_string(),
        },
        Error::WriteIo { path, source } => ErrorView::WriteIo {
            path: path.to_string(),
            message: source.to_string(),
        },
        Error::Parse {
            file,
            line,
            col,
            span,
            snippet,
            message,
        } => ErrorView::Parse {
            file: file.as_ref().map(ToString::to_string),
            line: *line,
            col: *col,
            span_start: span.start,
            span_end: span.end,
            snippet,
            message,
        },
        Error::Path {
            pointer,
            matched_prefix,
            kind,
            did_you_mean,
        } => ErrorView::Path {
            pointer,
            matched_prefix,
            path_kind: kind.into(),
            did_you_mean,
        },
        Error::UnsupportedFormat { name } => ErrorView::UnsupportedFormat { name },
        Error::Format { format, message } => ErrorView::Format { format, message },
        Error::WriteUnavailable { reason } => ErrorView::WriteUnavailable { reason },
        Error::TemplatedFile {
            line,
            snippet,
            hint,
        } => ErrorView::TemplatedFile {
            line: *line,
            snippet,
            hint,
        },
        Error::PatchTestFailed {
            pointer,
            expected,
            actual,
        } => ErrorView::PatchTestFailed {
            pointer,
            expected: expected.to_string(),
            actual: actual.to_string(),
        },
    }
}

/// Builds `{"server": {"port": 8080, "host": "localhost"}}` for the simple
/// path-miss scenarios.
fn doc_with_server() -> Value {
    let mut server = IndexMap::new();
    server.insert("port".to_owned(), Value::Int(8080));
    server.insert("host".to_owned(), Value::String("localhost".to_owned()));
    let mut root = IndexMap::new();
    root.insert("server".to_owned(), Value::Map(server));
    Value::Map(root)
}

/// Builds `{"metadata": {"labels": {"app.kubernetes.io/name": "web"}}}`.
fn doc_with_kubernetes_labels() -> Value {
    let mut labels = IndexMap::new();
    labels.insert(
        "app.kubernetes.io/name".to_owned(),
        Value::String("web".to_owned()),
    );
    let mut metadata = IndexMap::new();
    metadata.insert("labels".to_owned(), Value::Map(labels));
    let mut root = IndexMap::new();
    root.insert("metadata".to_owned(), Value::Map(metadata));
    Value::Map(root)
}

/// Builds `{"users": [{"id": 1}, {"id": 2}]}`.
fn doc_with_users(n: usize) -> Value {
    let users: Vec<Value> = (1..=n)
        .map(|i| {
            let mut m = IndexMap::new();
            m.insert("id".to_owned(), Value::Int(i as i64));
            Value::Map(m)
        })
        .collect();
    let mut root = IndexMap::new();
    root.insert("users".to_owned(), Value::Array(users));
    Value::Map(root)
}

#[test]
fn snapshot_path_typo_suggests_close_key() {
    // /server/prot — typo for /server/port. did_you_mean must include "port".
    let value = doc_with_server();
    let p = Pointer::parse("/server/prot").unwrap();
    let err = p.resolve(&value).unwrap_err();
    insta::assert_yaml_snapshot!("path_typo_suggests_close_key", view(&err));
}

#[test]
fn snapshot_path_kubernetes_label_typo() {
    // /metadata/lables/app.kubernetes.io~1name — typo `lables` for `labels`.
    // The error must report `matched_prefix: "/metadata"` and suggest `labels`.
    let value = doc_with_kubernetes_labels();
    let p = Pointer::parse("/metadata/lables/app.kubernetes.io~1name").unwrap();
    let err = p.resolve(&value).unwrap_err();
    insta::assert_yaml_snapshot!("path_kubernetes_label_typo", view(&err));
}

#[test]
fn snapshot_path_array_out_of_bounds() {
    // /users/5 against a 2-element array.
    let value = doc_with_users(2);
    let p = Pointer::parse("/users/5").unwrap();
    let err = p.resolve(&value).unwrap_err();
    insta::assert_yaml_snapshot!("path_array_out_of_bounds", view(&err));
}

#[test]
fn snapshot_path_missing_key_inside_array_element() {
    // /users/0/profile/bio — key chain missing the `profile` step.
    // matched_prefix must be "/users/0".
    let value = doc_with_users(1);
    let p = Pointer::parse("/users/0/profile/bio").unwrap();
    let err = p.resolve(&value).unwrap_err();
    insta::assert_yaml_snapshot!("path_missing_key_inside_array_element", view(&err));
}

#[test]
fn snapshot_path_descend_into_scalar() {
    // /server/port/version — port is a scalar Int, can't descend further.
    // expected = "object", found = "int".
    let value = doc_with_server();
    let p = Pointer::parse("/server/port/version").unwrap();
    let err = p.resolve(&value).unwrap_err();
    insta::assert_yaml_snapshot!("path_descend_into_scalar", view(&err));
}

#[test]
fn snapshot_json_parse_error_captures_position() {
    // `{ "x": 1, }` — JSON forbids the trailing comma. The parser must
    // surface this as an `Error::Parse` with line and column populated and
    // a `message` describing the failure.
    let fmt = dq_core::by_name("json").expect("json registered");
    let err = fmt
        .parse(b"{ \"x\": 1, }")
        .expect_err("trailing comma must fail");
    insta::assert_yaml_snapshot!("json_parse_trailing_comma", view(&err), {
        // The exact wording of the underlying serde_json message can shift
        // across versions. Redact it to keep the snapshot diff focused on
        // shape (kind, line, col).
        ".message" => "[serde_json message]"
    });
}

#[test]
fn snapshot_console_templated_file_renders_marker_and_hint() {
    // The Display impl of `Error::TemplatedFile` must include the line, the
    // snippet, and the canonical hint mentioning both escape-hatch flags so
    // the user knows how to proceed without reading the docs.
    let err = Error::templated_file(TemplateMarker {
        line: 7,
        snippet: "tag: {{ .Values.image.tag }}".to_owned(),
    });
    let rendered = format!("{err}");
    insta::assert_snapshot!("console_templated_file", rendered);
}

#[test]
fn snapshot_json_templated_file_carries_kind_line_snippet_hint() {
    // The serializable view must surface kind="templated_file" plus all
    // three structured fields (line, snippet, hint) so JSON consumers can
    // render the same diagnostic without parsing the Display string.
    let err = Error::templated_file(TemplateMarker {
        line: 7,
        snippet: "tag: {{ .Values.image.tag }}".to_owned(),
    });
    insta::assert_yaml_snapshot!("json_templated_file", view(&err));
}

#[test]
fn snapshot_console_write_io_renders_path_and_source() {
    // The Display impl of `Error::WriteIo` must include both the target
    // path and the underlying I/O error message — without leaking platform
    // specifics like errno numbers, since the message text is supplied by
    // the caller in this test fixture.
    let err = Error::WriteIo {
        path: Utf8PathBuf::from("/tmp/x.yaml"),
        source: io::Error::new(io::ErrorKind::PermissionDenied, "perm denied"),
    };
    let rendered = format!("{err}");
    insta::assert_snapshot!("console_write_io", rendered);
}

#[test]
fn snapshot_json_write_io_carries_kind_path_source() {
    // Mirror of the console snapshot above, but for the structured JSON
    // view consumed by the CLI's `--format json` reporter.
    let err = Error::WriteIo {
        path: Utf8PathBuf::from("/tmp/x.yaml"),
        source: io::Error::new(io::ErrorKind::PermissionDenied, "perm denied"),
    };
    insta::assert_yaml_snapshot!("json_write_io", view(&err));
}
