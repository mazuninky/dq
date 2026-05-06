//! Integration-level unit tests for `dq del` driven through `dq::run`.
//!
//! Same shape as `unit_set.rs`: build `Cli` via `Cli::try_parse_from(...)`,
//! drop bytes in a tempfile, call `dq::run` with `Vec<u8>` writers. Tests
//! pin the M2 §10 contract for `del`:
//! - leaf removal at object key
//! - array element removal (subsequent indices shift)
//! - missing pointer → Path error (exit 2)
//! - root pointer (`""`) → TypeMismatch (per spec, deleting the document is
//!   a hard error, not a silent empty file)
//! - `-i` atomic write
//! - `--diff` shows a `-` line for the removed key

use std::io::Write as _;

use clap::Parser;
use dq::Cli;
use tempfile::NamedTempFile;

/// Returns a `TempPath` so the underlying handle is closed before the binary
/// touches the path. Windows requires this for the binary to overwrite the
/// path during in-place edits ("Access is denied. (os error 5)"). Applied
/// uniformly across the test suite for consistency.
fn write_yaml(content: &str) -> tempfile::TempPath {
    let mut tmp = NamedTempFile::with_suffix(".yaml").expect("tempfile");
    tmp.write_all(content.as_bytes()).expect("write tempfile");
    tmp.into_temp_path()
}

#[test]
fn del_removes_leaf() {
    // Removing key `a` from `{a: 1, b: 2}` leaves only `b`. We assert on
    // the post-state and confirm `a:` is gone — string-contains plus a
    // negation pair so a regression that just no-ops the delete is caught.
    let tmp = write_yaml("a: 1\nb: 2\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::try_parse_from(["dq", "del", path, "/a"]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("del should succeed");
    let s = String::from_utf8(out).unwrap();
    assert!(s.contains("b: 2"), "expected `b: 2` to remain, got:\n{s}");
    assert!(
        !s.contains("a: 1"),
        "expected `a: 1` to be removed, got:\n{s}",
    );
}

#[test]
fn del_array_element_shifts_indices() {
    // Deleting index 2 from a 5-element list — the resulting list must be
    // 4 elements long, with the prior /3 element now at /2. We verify by
    // looking at the rendered YAML: items 1, 2, 4, 5 remain (item 3 is the
    // removed `c`), in source order.
    let tmp = write_yaml("items:\n  - a\n  - b\n  - c\n  - d\n  - e\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::try_parse_from(["dq", "del", path, "/items/2"]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("del should succeed");
    let s = String::from_utf8(out).unwrap();
    // The four remaining items must all appear; the deleted one must not.
    for keep in ["- a", "- b", "- d", "- e"] {
        assert!(s.contains(keep), "expected `{keep}` to remain, got:\n{s}");
    }
    assert!(!s.contains("- c"), "expected `- c` removed, got:\n{s}");
}

#[test]
fn del_missing_pointer_returns_path_error() {
    // Spec: missing pointer is NOT silent for `del`; users rely on it for
    // "this key existed and is now gone" semantics. Maps to NOT_FOUND (2).
    let tmp = write_yaml("a: 1\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::try_parse_from(["dq", "del", path, "/nonexistent"]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e = dq::run(&cli, false, &mut out, &mut err)
        .expect_err("missing pointer must surface a path error");
    let domain = e
        .downcast_ref::<dq_core::Error>()
        .expect("expected dq_core::Error");
    assert_eq!(
        domain.kind_name(),
        "path",
        "expected path-kind (exit 2), got: {domain:?}",
    );
}

#[test]
fn del_root_pointer_returns_type_mismatch() {
    // Empty pointer == document root. Deleting the root would empty the
    // file. Spec: `del_at("")` returns `Path { kind: TypeMismatch }` so
    // the user gets a clear "you cannot delete the document" message
    // instead of silently writing an empty file.
    let tmp = write_yaml("a: 1\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::try_parse_from(["dq", "del", path, ""]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e = dq::run(&cli, false, &mut out, &mut err).expect_err("del root must fail");
    let domain = e
        .downcast_ref::<dq_core::Error>()
        .expect("expected dq_core::Error");
    // Outer kind is `path` (so it maps to exit 2), inner detail is
    // TypeMismatch with `found = "root"`. We pin both so a regression that
    // turned root deletion into a silent success would surface here.
    assert_eq!(domain.kind_name(), "path");
    let dq_core::Error::Path { kind, .. } = domain else {
        panic!("expected Path variant, got: {domain:?}");
    };
    match kind {
        dq_core::PathErrorKind::TypeMismatch { found, .. } => {
            assert_eq!(*found, "root", "root rejection must report `found = root`");
        }
        other => panic!("expected TypeMismatch, got: {other:?}"),
    }
}

#[test]
fn del_in_place_writes_atomically() {
    // `-i` flushes the modification to disk via `dq_core::atomic_write::write`,
    // and stdout stays empty.
    let tmp = write_yaml("a: 1\nb: 2\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::try_parse_from(["dq", "-i", "del", path, "/a"]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("del should succeed");
    assert!(out.is_empty(), "in-place mode must not write to stdout");
    let on_disk = std::fs::read_to_string(&tmp).unwrap();
    assert_eq!(on_disk, "b: 2\n");
}

#[test]
fn del_diff_shows_removal() {
    // `--diff` produces a unified diff with at least a `-` line for the
    // removed key. We assert a `-a: 1` line so a regression that produced
    // an empty diff or a `+`-line would fail the assertion.
    let tmp = write_yaml("a: 1\nb: 2\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::try_parse_from(["dq", "--diff", "del", path, "/a"]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("del should succeed");
    let s = String::from_utf8(out).unwrap();
    assert!(
        s.contains("-a: 1"),
        "expected `-a: 1` in unified diff, got:\n{s}",
    );
    // File on disk untouched in diff mode.
    let on_disk = std::fs::read_to_string(&tmp).unwrap();
    assert_eq!(on_disk, "a: 1\nb: 2\n");
}
