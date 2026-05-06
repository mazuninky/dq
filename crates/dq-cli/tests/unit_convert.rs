//! Unit tests for `dq convert` driven through `dq::run`.
//!
//! Spec contract: re-emit the document in the format selected by `-F`. The
//! input format is detected from the file extension; `-F` picks the *output*
//! format. Malformed input is a parse error.
//!
//! M3 §8.4 adds in-place coverage:
//! - extension swap + source removal,
//! - `--keep-source` retains both files,
//! - same-format conversion is rejected as `InvalidInput`,
//! - bulk glob converts every match,
//! - `--backup` preserves a pre-existing target file as `<target>.bak`.

use std::fs;
use std::io::Write as _;

use camino::Utf8PathBuf;
use clap::Parser;
use dq::Cli;
use dq::error::InvalidInput;
use tempfile::{NamedTempFile, TempDir};

fn write_yaml(content: &str) -> NamedTempFile {
    let mut tmp = NamedTempFile::with_suffix(".yaml").expect("tempfile");
    tmp.write_all(content.as_bytes()).expect("write tempfile");
    tmp
}

fn write_json(content: &str) -> NamedTempFile {
    let mut tmp = NamedTempFile::with_suffix(".json").expect("tempfile");
    tmp.write_all(content.as_bytes()).expect("write tempfile");
    tmp
}

#[test]
fn convert_yaml_to_json_emits_valid_json() {
    let tmp = write_yaml("server:\n  port: 8080\n  host: x\n");
    let path = tmp.path().to_str().unwrap();
    let cli = Cli::parse_from(["dq", "-F", "json", "convert", path, "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("convert must succeed");
    let parsed: serde_json::Value =
        serde_json::from_slice(&out).expect("convert -F json must emit valid JSON");
    assert_eq!(
        parsed,
        serde_json::json!({"server": {"port": 8080, "host": "x"}})
    );
}

#[test]
fn convert_json_to_toon_emits_toon_text_containing_keys() {
    // TOON is delegated to the `toon-format` crate; we don't assert on the
    // exact text shape (it's owned by the crate), only that the output
    // mentions the source keys (i.e. encoder ran).
    let tmp = write_json(r#"{"name": "alice", "age": 30}"#);
    let path = tmp.path().to_str().unwrap();
    let cli = Cli::parse_from(["dq", "-F", "toon", "convert", path, "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("convert must succeed");
    let s = String::from_utf8(out).unwrap();
    assert!(
        s.contains("name") && s.contains("alice"),
        "TOON output must include keys/values from the source doc, got {s:?}",
    );
}

#[test]
fn convert_malformed_json_returns_parse_error() {
    // Trailing comma — invalid JSON.
    let tmp = write_json("{ \"x\": 1, }");
    let path = tmp.path().to_str().unwrap();
    let cli = Cli::parse_from(["dq", "-F", "yaml", "convert", path, "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e = dq::run(&cli, false, &mut out, &mut err).expect_err("malformed JSON must error");
    let domain = e.downcast_ref::<dq_core::Error>().unwrap();
    assert_eq!(domain.kind_name(), "parse");
}

#[test]
fn convert_preserves_big_int_through_json_round_trip() {
    // The motivating spec example: a 22-digit integer must survive a
    // JSON → JSON round-trip byte-for-byte.
    let big = "4722366482869645213696";
    let mut tmp = NamedTempFile::with_suffix(".json").expect("tempfile");
    tmp.write_all(format!("{{\"id\":{big}}}").as_bytes())
        .unwrap();
    let path = tmp.path().to_str().unwrap();
    let cli = Cli::parse_from(["dq", "-F", "json", "convert", path, "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("convert must succeed");
    let s = String::from_utf8(out).unwrap();
    assert!(
        s.contains(big),
        "big-int literal must survive the JSON → JSON round-trip, got: {s:?}",
    );
}

// ---------------------------------------------------------------------------
// M3 §8.4: in-place conversion (`-i`)
// ---------------------------------------------------------------------------
//
// All in-place tests use `tempdir() + create file` rather than
// `NamedTempFile`. Reason: `NamedTempFile` removes its backing file on drop
// and would race with the source-removal performed by `convert -i`.
// Owning a `TempDir` and creating `<dir>/<name>.<ext>` inside it gives us
// deterministic semantics: the test files only get cleaned up when the
// `TempDir` is dropped at end of scope.

/// Seed `<dir>/<name>` with `content` and return its UTF-8 path. Caller
/// owns the `TempDir` (kept alive for the duration of the test) and
/// receives a path it can hand to clap.
fn seed_file(dir: &TempDir, name: &str, content: &str) -> Utf8PathBuf {
    let path = Utf8PathBuf::from_path_buf(dir.path().join(name)).expect("utf-8 tempdir");
    fs::write(path.as_std_path(), content).expect("seed file");
    path
}

// ---------------------------------------------------------------------------
// Test 1 — `convert -i -F json` swaps extension and removes the source.
// ---------------------------------------------------------------------------

#[test]
fn convert_in_place_yaml_to_json_removes_source_and_writes_target() {
    // Spec scenario: `dq convert deploy.yaml -i -F json` → `deploy.json`
    // exists with converted content, `deploy.yaml` is removed, exit 0.
    let dir = tempfile::tempdir().expect("tempdir");
    let source = seed_file(&dir, "deploy.yaml", "name: app\nport: 8080\n");
    let target = source.with_extension("json");

    let cli = Cli::try_parse_from([
        "dq",
        "-i",
        "-F",
        "json",
        "convert",
        source.as_str(),
        "--no-color",
    ])
    .expect("clap must accept convert -i -F json");

    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("convert -i must succeed");

    // Target exists with the expected JSON shape.
    assert!(
        target.as_std_path().exists(),
        "target {target} must exist after convert -i",
    );
    let bytes = fs::read(target.as_std_path()).expect("read target");
    let parsed: serde_json::Value =
        serde_json::from_slice(&bytes).expect("converted JSON must parse");
    assert_eq!(parsed, serde_json::json!({"name": "app", "port": 8080}));

    // Source removed.
    assert!(
        !source.as_std_path().exists(),
        "source {source} must be removed by convert -i (no --keep-source)",
    );

    // Single-file convert -i is silent on stdout.
    assert!(
        out.is_empty(),
        "single-file convert -i must not print summary, got {:?}",
        String::from_utf8_lossy(&out),
    );
}

// ---------------------------------------------------------------------------
// Test 2 — `--keep-source` preserves both files.
// ---------------------------------------------------------------------------

#[test]
fn convert_in_place_with_keep_source_preserves_both_files() {
    // Spec scenario: `dq convert deploy.yaml -i -F json --keep-source` →
    // both files exist, exit 0.
    let dir = tempfile::tempdir().expect("tempdir");
    let original = "name: app\nport: 8080\n";
    let source = seed_file(&dir, "deploy.yaml", original);
    let target = source.with_extension("json");

    let cli = Cli::try_parse_from([
        "dq",
        "-i",
        "-F",
        "json",
        "convert",
        source.as_str(),
        "--keep-source",
        "--no-color",
    ])
    .expect("clap must accept --keep-source");

    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("convert with --keep-source must succeed");

    // Both files exist.
    assert!(
        source.as_std_path().exists(),
        "source must be preserved with --keep-source",
    );
    assert!(
        target.as_std_path().exists(),
        "target must still be created with --keep-source",
    );

    // Source bytes are byte-identical to the original (we only WROTE the
    // target; the source must be untouched).
    let source_after = fs::read_to_string(source.as_std_path()).expect("read source");
    assert_eq!(
        source_after, original,
        "source content must be unchanged by --keep-source",
    );

    // Target parses as the expected JSON.
    let target_bytes = fs::read(target.as_std_path()).expect("read target");
    let parsed: serde_json::Value =
        serde_json::from_slice(&target_bytes).expect("target must be valid JSON");
    assert_eq!(parsed, serde_json::json!({"name": "app", "port": 8080}));
}

// ---------------------------------------------------------------------------
// Test 3 — `convert -i -F yaml` on a `.yaml` source is rejected.
// ---------------------------------------------------------------------------

#[test]
fn convert_in_place_same_format_returns_invalid_input() {
    // Spec scenario: `dq convert deploy.yaml -i -F yaml` → InvalidInput
    // (exit 6) "convert is a no-op". The handler computes the target path
    // by extension swap; for the same target format the swap produces the
    // source path, which the handler rejects.
    let dir = tempfile::tempdir().expect("tempdir");
    let source = seed_file(&dir, "deploy.yaml", "name: app\nport: 8080\n");
    let original = fs::read_to_string(source.as_std_path()).expect("read seed");

    let cli = Cli::try_parse_from([
        "dq",
        "-i",
        "-F",
        "yaml",
        "convert",
        source.as_str(),
        "--no-color",
    ])
    .expect("clap parse should still succeed; rejection happens in the handler");

    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let result = dq::run(&cli, false, &mut out, &mut err);
    let e = result.expect_err("same-format convert -i must be rejected");

    // Carries the InvalidInput marker so the exit-code mapper picks 6.
    let invalid = e
        .downcast_ref::<InvalidInput>()
        .unwrap_or_else(|| panic!("rejection must carry InvalidInput marker, got: {e:?}"));
    let msg = invalid.to_string();
    assert!(
        msg.contains(source.as_str()),
        "error message should reference the source path, got: {msg:?}",
    );
    assert_eq!(
        dq::exit_code::exit_code_for_error(&e),
        dq::exit_code::INVALID_INPUT,
        "InvalidInput marker must map to exit 6",
    );

    // Source on disk is byte-identical to the original (rejection happens
    // before any I/O).
    let source_after = fs::read_to_string(source.as_std_path()).expect("read source");
    assert_eq!(
        source_after, original,
        "rejected convert -i must NOT touch the source on disk",
    );
}

// ---------------------------------------------------------------------------
// Test 4 — bulk: `convert 'tmpdir/*.yaml' -i -F json` converts every match.
// ---------------------------------------------------------------------------

#[test]
fn convert_in_place_bulk_glob_converts_every_yaml_file() {
    // Spec scenario: glob expansion lifts in-place convert across multiple
    // files. Each source.yaml should produce source.json, and the originals
    // should be removed.
    let dir = tempfile::tempdir().expect("tempdir");
    let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("utf-8 tempdir");
    for i in 0..3 {
        seed_file(
            &dir,
            &format!("{i}.yaml"),
            &format!("name: file{i}\nport: 808{i}\n"),
        );
    }

    let glob = format!("{root}/*.yaml");
    let cli = Cli::try_parse_from(["dq", "-i", "-F", "json", "convert", &glob, "--no-color"])
        .expect("clap must accept the glob");

    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("bulk convert -i must succeed");

    // Every source removed; every target exists with valid JSON content.
    for i in 0..3 {
        let source = root.join(format!("{i}.yaml"));
        let target = root.join(format!("{i}.json"));
        assert!(
            !source.as_std_path().exists(),
            "source {source} should be removed by bulk convert -i",
        );
        assert!(
            target.as_std_path().exists(),
            "target {target} should exist after bulk convert -i",
        );
        let bytes = fs::read(target.as_std_path()).unwrap_or_else(|e| panic!("read {target}: {e}"));
        let parsed: serde_json::Value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|e| panic!("parse {target} as JSON: {e}"));
        assert_eq!(
            parsed,
            serde_json::json!({"name": format!("file{i}"), "port": 8080 + i}),
            "target {target} content mismatch",
        );
    }

    // Bulk runs print the summary line.
    let s = String::from_utf8(out).expect("stdout utf-8");
    assert!(
        s.contains("Modified: 3, Skipped: 0, Failed: 0"),
        "expected bulk summary line, got:\n{s}",
    );
}

// ---------------------------------------------------------------------------
// Test 5 — `--backup` preserves a pre-existing target file as `<target>.bak`.
// ---------------------------------------------------------------------------
//
// Production-code decision (from `crates/dq-cli/src/commands/convert.rs`,
// lines 220-224 + 67-75 of `crates/dq-core/src/atomic_write.rs`):
//
// ```text
// // Atomic write to target. `--backup` is honoured by atomic_write
// // when the target already exists.
// dq_core::atomic_write::write(&target_path, &output_bytes, cli.backup)
//     .map_err(anyhow::Error::new)?;
// ```
//
// And `atomic_write::write` only creates `<target>.bak` when the TARGET
// path already exists at write time:
//
// ```text
// if backup && path.exists() {
//     let backup_path = backup_path_for(path);
//     std::fs::copy(path.as_std_path(), backup_path.as_std_path())...
// }
// ```
//
// So convert's `--backup` semantics are: "back up the TARGET if it
// already exists; the source is never backed up". This test verifies
// that contract by pre-creating `deploy.json` with old content, running
// `convert deploy.yaml -i -F json --backup`, and asserting that
// `deploy.json.bak` carries the OLD `.json` content while `deploy.json`
// is overwritten with the freshly converted content. The source
// `deploy.yaml` is removed exactly as in the no-backup case.

#[test]
fn convert_in_place_with_backup_preserves_existing_target_as_bak() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = seed_file(&dir, "deploy.yaml", "name: app\nport: 8080\n");
    let target = source.with_extension("json");
    let backup = Utf8PathBuf::from(format!("{target}.bak"));

    // Pre-create the target with OLD content. atomic_write should copy it
    // to <target>.bak before overwriting.
    let old_target_content = "{\"name\":\"old\",\"port\":1}";
    fs::write(target.as_std_path(), old_target_content).expect("seed pre-existing target");

    let cli = Cli::try_parse_from([
        "dq",
        "-i",
        "--backup",
        "-F",
        "json",
        "convert",
        source.as_str(),
        "--no-color",
    ])
    .expect("clap must accept --backup with -i");

    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("convert -i --backup must succeed");

    // `<target>.bak` carries the OLD target content (proves backup ran
    // before the overwrite).
    assert!(
        backup.as_std_path().exists(),
        "backup file {backup} must exist when target pre-existed",
    );
    let backup_bytes = fs::read_to_string(backup.as_std_path()).expect("read backup");
    assert_eq!(
        backup_bytes, old_target_content,
        "{backup} must carry the OLD target content",
    );

    // Target is now the freshly converted JSON.
    assert!(target.as_std_path().exists(), "target must still exist");
    let target_bytes = fs::read(target.as_std_path()).expect("read target");
    let parsed: serde_json::Value =
        serde_json::from_slice(&target_bytes).expect("target must parse as JSON");
    assert_eq!(
        parsed,
        serde_json::json!({"name": "app", "port": 8080}),
        "target must be replaced with newly converted content",
    );

    // Source is removed (no `--keep-source`).
    assert!(
        !source.as_std_path().exists(),
        "source must still be removed when --backup is used (backup applies to target, not source)",
    );

    // No `<source>.bak` is created — backup is a target concern, not a
    // source concern.
    let source_bak = Utf8PathBuf::from(format!("{source}.bak"));
    assert!(
        !source_bak.as_std_path().exists(),
        "source backup {source_bak} must NOT be created — backup applies to target",
    );
}
