//! Component tests for the `.gitignore` / `.dockerignore` ignore-list parser.
//!
//! Read-only by design (D7). Stage 2's inline tests cover one parse + the
//! read-only write rejection. These tests pin filename detection,
//! whitespace handling, and the read-only contract.

use camino::Utf8Path;
use dq_core::{Document, Format, FormatTag, Value};
use pretty_assertions::assert_eq;

fn ignore_list() -> &'static dyn Format {
    dq_core::by_name("ignore-list").expect("ignore-list format must be registered")
}

#[test]
fn parse_drops_comments_and_blank_lines_emitting_only_patterns() {
    // Per spec D7: comments (`#`) and blank lines are NOT preserved in the
    // value tree. The result is a flat `Array<String>` of the surviving
    // patterns in source order.
    let doc = ignore_list()
        .parse(b"node_modules/\n# build artifacts\n*.log\n\ntarget/\n")
        .expect("ignore-list parse must succeed");
    let Value::Array(items) = doc.value() else {
        panic!("expected Array<String>, got: {:?}", doc.value());
    };
    let strings: Vec<&str> = items
        .iter()
        .map(|v| match v {
            Value::String(s) => s.as_str(),
            other => panic!("non-string element in ignore list: {other:?}"),
        })
        .collect();
    assert_eq!(
        strings,
        vec!["node_modules/", "*.log", "target/"],
        "comments and blanks must be filtered while preserving source order",
    );
}

#[test]
fn parse_strips_trailing_whitespace_per_pattern() {
    // Each pattern is stored without trailing whitespace (the parser uses
    // `trim_end`). Leading whitespace, by contrast, is preserved (gitignore
    // doesn't ascribe meaning to it, but we don't add a coercion here).
    let doc = ignore_list()
        .parse(b"node_modules/   \n*.log\t\t\n")
        .expect("trailing whitespace must not break the parse");
    let Value::Array(items) = doc.value() else {
        panic!()
    };
    let strings: Vec<&str> = items
        .iter()
        .map(|v| match v {
            Value::String(s) => s.as_str(),
            _ => panic!(),
        })
        .collect();
    assert_eq!(
        strings,
        vec!["node_modules/", "*.log"],
        "trailing whitespace must be trimmed from every pattern",
    );
}

#[test]
fn registry_detects_gitignore_and_dockerignore_filenames() {
    // The `.gitignore` and `.dockerignore` filenames must resolve via the
    // FILENAME_FALLBACK table. Both are extensionless from `Utf8Path`'s
    // perspective (the leading dot makes it a hidden file, not an extension).
    for name in [".gitignore", ".dockerignore"] {
        let fmt = dq_core::detect(Utf8Path::new(name))
            .unwrap_or_else(|| panic!("{name} must resolve to the ignore-list parser"));
        assert_eq!(
            fmt.name(),
            "ignore-list",
            "{name} must resolve to `ignore-list`, got `{}`",
            fmt.name(),
        );
    }
}

#[test]
fn write_returns_format_error_with_read_only_message() {
    // The ignore-list writer always errors. Pin both the format tag and the
    // "read-only" wording so users get a clear, consistent rejection.
    let doc = Document::value_only(Value::Array(vec![]), FormatTag::IgnoreList);
    let mut buf: Vec<u8> = Vec::new();
    let err = ignore_list()
        .write(&doc, &mut buf)
        .expect_err("ignore-list write must always error");
    match err {
        dq_core::Error::Format { format, message } => {
            assert_eq!(format, "ignore-list");
            assert!(
                message.contains("read-only"),
                "expected `read-only` in message, got: {message:?}",
            );
        }
        other => panic!("expected Error::Format, got: {other:?}"),
    }
}
