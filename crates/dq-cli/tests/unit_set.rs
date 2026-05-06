//! Integration-level unit tests for `dq set` driven through `dq::run`.
//!
//! These tests skip the binary spawn — they call `dq::run` with `Vec<u8>`
//! writers, so a failed assertion is debuggable in-process. Each case follows
//! the dependency-injection pattern from `references/cli-testing.md`: build
//! `Cli` via `Cli::try_parse_from(...)` (so clap's `global = true` plumbing is
//! exercised), put real bytes in a tempfile, then assert on stdout / file
//! contents / domain error variants.
//!
//! Coverage is the M2 §9 contract for `set`:
//! - default mode (stdout)
//! - `-i` atomic write + `--backup`
//! - `--diff` unified-diff rendering
//! - `--no-create` rejection
//! - `--value-string` vs JSON-literal heuristic
//! - `@<path>` and `--value-from <path>` value sources
//! - flag-combination rejections (`-i --diff`, `-i -F`, `--backup` w/o `-i`)
//! - write-failure handling with the original file left intact (Unix only).

use std::io::Write as _;

use clap::Parser;
use dq::Cli;
use tempfile::NamedTempFile;

/// Write `content` to a temp YAML file and return the handle (so it stays
/// alive for the test's lifetime).
fn write_yaml(content: &str) -> NamedTempFile {
    let mut tmp = NamedTempFile::with_suffix(".yaml").expect("tempfile");
    tmp.write_all(content.as_bytes()).expect("write tempfile");
    tmp
}

/// Write `content` to a temp JSON file (used for `--value-from` fixtures).
fn write_json(content: &str) -> NamedTempFile {
    let mut tmp = NamedTempFile::with_suffix(".json").expect("tempfile");
    tmp.write_all(content.as_bytes()).expect("write tempfile");
    tmp
}

#[test]
fn set_replaces_scalar() {
    // Default mode (no `-i`, no `--diff`) writes the modified document to
    // stdout. The file on disk is untouched. Asserting the stdout substring
    // pins the behaviour without binding to the parser's exact reflow.
    let tmp = write_yaml("spec:\n  replicas: 3\n");
    let path = tmp.path().to_str().unwrap();
    let cli = Cli::try_parse_from(["dq", "set", path, "/spec/replicas", "5"]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("set should succeed");
    let s = String::from_utf8(out).unwrap();
    assert!(
        s.contains("replicas: 5"),
        "stdout must contain `replicas: 5`, got: {s:?}",
    );
    // File on disk untouched without `-i`.
    let on_disk = std::fs::read_to_string(tmp.path()).unwrap();
    assert_eq!(on_disk, "spec:\n  replicas: 3\n");
}

#[test]
fn set_in_place_writes_file_atomically() {
    // `-i` mode: file content updated, stdout empty. We use the atomic-write
    // path through `dq_core::atomic_write::write` — its byte-for-byte
    // semantics are exercised here.
    let tmp = write_yaml("spec:\n  replicas: 3\n");
    let path = tmp.path().to_str().unwrap();
    let cli = Cli::try_parse_from(["dq", "-i", "set", path, "/spec/replicas", "5"]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("set should succeed");
    assert!(out.is_empty(), "in-place mode must not write to stdout");
    let on_disk = std::fs::read_to_string(tmp.path()).unwrap();
    assert_eq!(on_disk, "spec:\n  replicas: 5\n");
}

#[test]
fn set_in_place_with_backup_creates_bak_file() {
    // `-i --backup` writes the new content to the file AND creates a `.bak`
    // alongside containing the original bytes. The backup path always appends
    // `.bak` (`foo.yaml` → `foo.yaml.bak`) per `atomic_write::backup_path_for`.
    let original = "spec:\n  replicas: 3\n";
    let tmp = write_yaml(original);
    let path = tmp.path().to_str().unwrap();
    let cli =
        Cli::try_parse_from(["dq", "-i", "--backup", "set", path, "/spec/replicas", "5"]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("set should succeed");

    let on_disk = std::fs::read_to_string(tmp.path()).unwrap();
    assert_eq!(on_disk, "spec:\n  replicas: 5\n");

    // Backup path: append `.bak` to the full path (`foo.yaml` → `foo.yaml.bak`).
    let bak_path =
        std::path::PathBuf::from(format!("{}.bak", tmp.path().to_str().expect("utf-8 path")));
    let bak_contents = std::fs::read_to_string(&bak_path)
        .unwrap_or_else(|e| panic!("backup file missing at {}: {e}", bak_path.display()));
    assert_eq!(
        bak_contents, original,
        "backup must contain the original bytes",
    );

    // Cleanup so a re-run doesn't see leftover `.bak`.
    let _ = std::fs::remove_file(&bak_path);
}

#[test]
fn set_diff_outputs_unified_diff() {
    // `--diff` produces a unified diff with `-` / `+` lines. We assert both
    // the removed-original and added-new line so a regression where the diff
    // is reversed (or empty) is caught immediately.
    let tmp = write_yaml("spec:\n  replicas: 3\n");
    let path = tmp.path().to_str().unwrap();
    let cli = Cli::try_parse_from(["dq", "--diff", "set", path, "/spec/replicas", "5"]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("set should succeed");
    let s = String::from_utf8(out).unwrap();
    assert!(
        s.contains("-  replicas: 3"),
        "expected `-  replicas: 3` in diff, got:\n{s}",
    );
    assert!(
        s.contains("+  replicas: 5"),
        "expected `+  replicas: 5` in diff, got:\n{s}",
    );
    // File on disk untouched in diff mode.
    let on_disk = std::fs::read_to_string(tmp.path()).unwrap();
    assert_eq!(on_disk, "spec:\n  replicas: 3\n");
}

#[test]
fn set_no_create_rejects_missing_pointer() {
    // Spec: missing intermediate pointer with `--no-create` → `Error::Path`,
    // which the exit-code mapper translates to NOT_FOUND (2). The M2 baseline
    // also rejects non-existent pointers without `--no-create` because the
    // mkdir-p path is not yet wired; this test pins the `--no-create` case
    // so the future M3 work cannot accidentally regress the strict mode.
    let tmp = write_yaml("a: 1\n");
    let path = tmp.path().to_str().unwrap();
    let cli =
        Cli::try_parse_from(["dq", "set", path, "/missing/path", "hello", "--no-create"]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let result = dq::run(&cli, false, &mut out, &mut err);
    let e = result.expect_err("missing pointer with --no-create must error");
    let domain = e
        .downcast_ref::<dq_core::Error>()
        .expect("missing pointer must surface as dq_core::Error");
    assert_eq!(
        domain.kind_name(),
        "path",
        "expected path-kind error (maps to exit 2), got {domain:?}",
    );
}

#[test]
fn set_value_string_forces_string() {
    // Without `--value-string` a literal `"8080"` would be parsed as a string
    // (heuristic); but this test pins the explicit `--value-string` flag so
    // the "force string" behaviour can't regress to JSON-literal parsing.
    // We pass the literal `8080` (which would normally parse as Int) plus
    // `--value-string` so the resulting node is a string.
    let tmp = write_yaml("port: 80\n");
    let path = tmp.path().to_str().unwrap();
    let cli = Cli::try_parse_from(["dq", "set", path, "/port", "8080", "--value-string"]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("set should succeed");
    let s = String::from_utf8(out).unwrap();
    // YAML emitter quotes strings; the exact quote style depends on the
    // backend — accept either single or double quoting.
    assert!(
        s.contains("'8080'") || s.contains("\"8080\""),
        "expected quoted string in output, got:\n{s}",
    );
}

#[test]
fn set_inline_json_literal_int_heuristic() {
    // Complementary to `set_value_string_forces_string`: bare `8080` without
    // the flag MUST be parsed as an integer. A regression that broke the
    // heuristic would emit `'8080'` instead.
    let tmp = write_yaml("port: 80\n");
    let path = tmp.path().to_str().unwrap();
    let cli = Cli::try_parse_from(["dq", "set", path, "/port", "8080"]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("set should succeed");
    let s = String::from_utf8(out).unwrap();
    assert!(s.contains("port: 8080"), "got:\n{s}");
    assert!(
        !s.contains("'8080'") && !s.contains("\"8080\""),
        "got quoted, expected unquoted int:\n{s}",
    );
}

#[test]
fn set_at_file_value_from_path() {
    // `@<path>` shorthand: read the value from a file. The handler parses
    // the value source as a structured document and splices its top-level
    // value into the target. We mutate an existing leaf (`/name`) so the
    // M2 baseline's "no mkdir-p yet" restriction doesn't apply.
    let target = write_yaml("name: old\n");
    let value_src = write_json(r#""loaded-from-file""#);
    let target_path = target.path().to_str().unwrap();
    let value_path = value_src.path().to_str().unwrap();
    let at_arg = format!("@{value_path}");
    let cli = Cli::try_parse_from(["dq", "set", target_path, "/name", &at_arg]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("set should succeed");
    let s = String::from_utf8(out).unwrap();
    assert!(
        s.contains("loaded-from-file"),
        "expected `loaded-from-file` from value source, got:\n{s}",
    );
}

#[test]
fn set_with_value_from_flag() {
    // `--value-from path` is the long form of `@path`; same semantics. We
    // use a different shape (top-level scalar) to exercise the path where
    // the value source is a single value rather than a map.
    let target = write_yaml("greeting: hello\n");
    let value_src = write_json(r#""hi there""#);
    let target_path = target.path().to_str().unwrap();
    let value_path = value_src.path().to_str().unwrap();
    let cli = Cli::try_parse_from([
        "dq",
        "set",
        target_path,
        "/greeting",
        "--value-from",
        value_path,
    ])
    .unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("set should succeed");
    let s = String::from_utf8(out).unwrap();
    assert!(
        s.contains("hi there"),
        "expected loaded scalar in output, got:\n{s}",
    );
}

#[test]
fn set_rejects_in_place_with_diff() {
    // `-i --diff` is a contradictory output-mode pair; `ensure_write_flags_consistent`
    // raises an `InvalidInput` so the exit-code mapper produces 6.
    let tmp = write_yaml("a: 1\n");
    let path = tmp.path().to_str().unwrap();
    let cli = Cli::try_parse_from(["dq", "-i", "--diff", "set", path, "/a", "2"]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e = dq::run(&cli, false, &mut out, &mut err)
        .expect_err("`-i --diff` must be rejected up-front");
    assert!(
        e.downcast_ref::<dq::error::InvalidInput>().is_some(),
        "rejection must carry InvalidInput marker (exit 6), got: {e:?}",
    );
    assert!(
        e.to_string().contains("mutually exclusive"),
        "error message must explain the conflict, got: {e}",
    );
}

#[test]
fn set_rejects_in_place_with_format_override() {
    // `-i -F json` against a YAML file would imply an in-place format change,
    // which is M3 territory. The handler must reject it before any I/O.
    let tmp = write_yaml("a: 1\n");
    let path = tmp.path().to_str().unwrap();
    let cli = Cli::try_parse_from(["dq", "-i", "-F", "json", "set", path, "/a", "2"]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e = dq::run(&cli, false, &mut out, &mut err).expect_err("`-i -F` must be rejected");
    assert!(
        e.downcast_ref::<dq::error::InvalidInput>().is_some(),
        "rejection must carry InvalidInput marker (exit 6), got: {e:?}",
    );
    assert!(
        e.to_string().contains("-F") || e.to_string().contains("format"),
        "error message must mention -F, got: {e}",
    );
}

#[test]
fn set_rejects_backup_without_in_place() {
    // `--backup` only makes sense with `-i`; without `-i` there's no
    // replacement to back up, so the flag has no effect and is rejected.
    let tmp = write_yaml("a: 1\n");
    let path = tmp.path().to_str().unwrap();
    let cli = Cli::try_parse_from(["dq", "--backup", "set", path, "/a", "2"]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e = dq::run(&cli, false, &mut out, &mut err)
        .expect_err("`--backup` without `-i` must be rejected");
    assert!(
        e.downcast_ref::<dq::error::InvalidInput>().is_some(),
        "rejection must carry InvalidInput marker (exit 6), got: {e:?}",
    );
    let msg = e.to_string();
    assert!(
        msg.contains("--backup"),
        "error must mention --backup, got: {msg}"
    );
    assert!(
        msg.contains("--in-place") || msg.contains("-i"),
        "error must mention -i/--in-place dependency, got: {msg}",
    );
}

#[test]
fn set_inline_json_literal_string_heuristic_falls_back() {
    // The heuristic in `parse_inline_value`: leading `{` or `[` (etc.)
    // triggers the JSON-literal parse attempt; if that fails, fall back to
    // the literal string instead of raising a parse error. Here `{not json}`
    // triggers the heuristic (leading `{`) but is not valid JSON, so it must
    // round-trip as the literal string `{not json}`.
    let tmp = write_yaml("name: old\n");
    let path = tmp.path().to_str().unwrap();
    let cli = Cli::try_parse_from(["dq", "set", path, "/name", "{not json}"]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("set should succeed");
    let s = String::from_utf8(out).unwrap();
    // The string `{not json}` must appear in the output. YAML emitters will
    // quote it (literal `{` would otherwise start a flow mapping) — accept
    // either single or double quoting.
    assert!(
        s.contains("'{not json}'") || s.contains("\"{not json}\""),
        "expected quoted literal `{{not json}}` in output, got:\n{s}",
    );
}

/// Failure-path: `-i` on a directory we make read-only via permissions. The
/// atomic-write tempfile creation must fail, the original file must be left
/// untouched, and the exit-code mapper must receive a `WriteIo` /
/// `WriteUnavailable` error (kind name `write_io` / `write_unavailable` —
/// both map to exit 7 WRITE_FAILED).
///
/// Skipped on Windows because the permissions model is different (and chmod
/// 0 is meaningless for the file owner there).
#[cfg(unix)]
#[test]
fn set_in_place_failure_leaves_original_intact() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("victim.yaml");
    let original = "a: 1\n";
    std::fs::write(&path, original).expect("seed victim file");

    // Make the parent directory non-writable. `atomic_write::write` creates
    // a tempfile in the same directory before persisting; that creation must
    // fail.
    let mut dir_perms = std::fs::metadata(dir.path()).unwrap().permissions();
    dir_perms.set_mode(0o500);
    std::fs::set_permissions(dir.path(), dir_perms).expect("chmod dir");

    let path_str = path.to_str().unwrap();
    let cli = Cli::try_parse_from(["dq", "-i", "set", path_str, "/a", "2"]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let result = dq::run(&cli, false, &mut out, &mut err);

    // Restore permissions before asserting so a failed assertion doesn't
    // prevent tempdir cleanup.
    let mut restore = std::fs::metadata(dir.path()).unwrap().permissions();
    restore.set_mode(0o700);
    std::fs::set_permissions(dir.path(), restore).expect("restore dir perms");

    let e = result.expect_err("write into a read-only dir must fail");
    let domain = e
        .downcast_ref::<dq_core::Error>()
        .expect("expected dq_core::Error");
    let kind = domain.kind_name();
    assert!(
        matches!(kind, "write_io" | "write_unavailable"),
        "expected write-class error (exit 7), got `{kind}`: {domain:?}",
    );
    // Original bytes must be intact — atomic write means failure leaves the
    // target untouched.
    let on_disk = std::fs::read_to_string(&path).expect("victim still readable");
    assert_eq!(
        on_disk, original,
        "original file must be unchanged on write failure",
    );
}
