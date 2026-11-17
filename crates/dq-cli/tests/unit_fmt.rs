//! Integration-level unit tests for `dq fmt` driven through `dq::run`.
//!
//! These cover the M4 `data-query-fmt` spec scenarios at the handler layer:
//! default stdout, `-i` atomic write, `--diff`, `--check` (both directions),
//! `--sort-keys`, source-format preservation, glob expansion, and `--indent`
//! for JSON.
//!
//! Conventions match `unit_set.rs` / `unit_convert.rs`: each test parses a
//! `Cli` via `Cli::try_parse_from(...)`, calls `dq::run` with `Vec<u8>`
//! writers, and asserts on stdout / file contents / domain error variants.
//! No subprocess spawn — failures are debuggable in-process.
//!
//! ## Why we materialize "canonical" fixtures via the writer
//!
//! `dq fmt --check` only exits 0 when source bytes equal the writer's
//! re-emitted bytes. Hand-written YAML almost never hits that contract on
//! the first try (the writer adds quoting around reserved words, normalises
//! trailing newlines, etc.). Tests that need a canonical fixture render it
//! once via `dq::run(..., fmt, ...)` then write the captured stdout back to
//! a fresh tempfile — that file is by construction canonical for `--check`.

use std::io::Write as _;

use clap::Parser;
use dq::Cli;
use tempfile::NamedTempFile;

/// Write `content` to a temp YAML file and return a `TempPath`. We return
/// `TempPath` (not `NamedTempFile`) so the underlying handle is closed before
/// the binary touches the path. On Windows, holding the `NamedTempFile` open
/// blocks any in-place rewrite of the same path with "Access is denied. (os
/// error 5)". The `TempPath` still removes the file on drop, so cleanup is
/// preserved.
fn write_yaml(content: &str) -> tempfile::TempPath {
    let mut tmp = NamedTempFile::with_suffix(".yaml").expect("tempfile");
    tmp.write_all(content.as_bytes()).expect("write tempfile");
    tmp.into_temp_path()
}

/// Write `content` to a temp JSON file and return a `TempPath` — see
/// `write_yaml` for the Windows rationale.
fn write_json(content: &str) -> tempfile::TempPath {
    let mut tmp = NamedTempFile::with_suffix(".json").expect("tempfile");
    tmp.write_all(content.as_bytes()).expect("write tempfile");
    tmp.into_temp_path()
}

/// Render `path` through `dq fmt` (default mode → stdout) and return the
/// captured stdout. Used to materialize canonical fixtures.
fn render_canonical(path: &str) -> Vec<u8> {
    let cli = Cli::try_parse_from(["dq", "fmt", path]).expect("clap parse");
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("fmt rendering");
    out
}

#[test]
fn fmt_default_writes_to_stdout() {
    // Spec: `dq fmt deploy.yaml` (no `-i`, no `--diff`) writes the rendered
    // YAML to stdout and leaves the file on disk untouched.
    let original = "a: 1\nb: 2\n";
    let tmp = write_yaml(original);
    let path = tmp.to_str().unwrap();
    let cli = Cli::try_parse_from(["dq", "fmt", path]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("fmt should succeed");
    let s = String::from_utf8(out).unwrap();
    // The writer is byte-stable for this input — `a: 1\nb: 2\n` round-trips
    // without quoting changes — but the contract is that *something* lands
    // on stdout. We pin the load-bearing structure: both keys present.
    assert!(
        s.contains("a:") && s.contains("b:"),
        "stdout must contain both YAML keys, got: {s:?}",
    );
    // File on disk untouched without `-i` or `--check`.
    let on_disk = std::fs::read_to_string(&tmp).unwrap();
    assert_eq!(on_disk, original, "default mode must not modify the file");
}

#[test]
fn fmt_in_place_writes_back_atomically() {
    // Spec: `dq fmt deploy.yaml -i` rewrites the file with the re-emitted
    // bytes; stdout is empty; exit 0. We pre-render a canonical version,
    // write a *non-canonical* file (the fmt writer adds a trailing newline
    // we strip), and verify `-i` produces the canonical bytes.
    let original = "a: 1\nb: 2";
    let tmp = write_yaml(original);
    let path = tmp.to_str().unwrap();
    let canonical = render_canonical(path);

    let cli = Cli::try_parse_from(["dq", "-i", "fmt", path]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("fmt -i should succeed");
    assert!(out.is_empty(), "in-place mode must not write to stdout");
    let on_disk = std::fs::read(&tmp).unwrap();
    assert_eq!(
        on_disk, canonical,
        "in-place fmt must write the canonical bytes",
    );
}

#[test]
fn fmt_diff_shows_unified_diff_without_writing() {
    // Spec: `dq fmt deploy.yaml --diff` writes a unified diff to stdout and
    // leaves the file unchanged. We seed a non-canonical file (trailing
    // newline missing) so the diff is non-empty.
    let original = "a: 1\nb: 2"; // no trailing newline → writer will add one
    let tmp = write_yaml(original);
    let path = tmp.to_str().unwrap();
    let cli = Cli::try_parse_from(["dq", "--diff", "fmt", path]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("fmt --diff should succeed");
    let s = String::from_utf8(out).unwrap();
    // Unified-diff format markers.
    assert!(
        s.contains("---") && s.contains("+++"),
        "expected unified-diff `---` / `+++` markers, got:\n{s}",
    );
    // File on disk untouched in diff mode.
    let on_disk = std::fs::read_to_string(&tmp).unwrap();
    assert_eq!(on_disk, original, "--diff must not write to disk");
}

#[test]
fn fmt_check_exits_zero_on_canonical_file() {
    // Spec: when source bytes equal the writer's output, `--check` returns
    // `Ok(())` — exit 0. We materialize a canonical file by rendering once
    // and writing back; that file is by construction byte-equal to its
    // re-emission.
    let seed = write_yaml("a: 1\nb: 2\n");
    let canonical_bytes = render_canonical(seed.to_str().unwrap());
    let mut canonical = NamedTempFile::with_suffix(".yaml").unwrap();
    canonical.write_all(&canonical_bytes).unwrap();
    let canonical = canonical.into_temp_path();
    let canonical_path = canonical.to_str().unwrap();

    let cli = Cli::try_parse_from(["dq", "--check", "fmt", canonical_path]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("fmt --check on canonical file must return Ok");
}

#[test]
fn fmt_check_exits_one_on_non_canonical_file() {
    // Spec: when re-emitted bytes differ from source, `--check` errors with
    // a `CheckPending` marker that the exit-code mapper translates to 1.
    // We use 4-space indented nested mapping — `serde_norway` emits 2-space
    // block indent, so source `a:\n    b: 1\n` normalises to
    // `a:\n  b: 1\n`, which cannot match the source.
    let original = "a:\n    b: 1\n";
    let tmp = write_yaml(original);
    let path = tmp.to_str().unwrap();
    let cli = Cli::try_parse_from(["dq", "--check", "fmt", path]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e = dq::run(&cli, false, &mut out, &mut err)
        .expect_err("non-canonical file under --check must error");

    let pending = e
        .downcast_ref::<dq::error::CheckPending>()
        .expect("error must carry CheckPending marker so exit-code maps to 1");
    assert!(
        pending.count >= 1,
        "expected at least one would-modify entry, got count={}",
        pending.count,
    );
    // exit-code mapper translates CheckPending → GENERIC (1).
    assert_eq!(
        dq::exit_code::exit_code_for_error(&e),
        1,
        "CheckPending must map to exit code 1, got: {e:?}",
    );
    // File on disk untouched.
    let on_disk = std::fs::read_to_string(&tmp).unwrap();
    assert_eq!(on_disk, original, "--check must not modify the file");
}

#[test]
fn fmt_sort_keys_in_place_reorders_yaml_keys() {
    // Spec: `dq fmt --sort-keys -i` on a file whose keys are out of order
    // produces a file with keys in alphabetic order at every depth.
    // We use `z, a` at the top and `y, b` nested under `a` to verify both
    // top-level and nested ordering.
    let tmp = write_yaml("z: 1\na:\n  y: 2\n  b: 3\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::try_parse_from(["dq", "--sort-keys", "-i", "fmt", path]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("fmt --sort-keys -i should succeed");

    let on_disk = std::fs::read_to_string(&tmp).unwrap();
    // Assert ordering by byte index — `a` must precede `z` at the top level,
    // and `b` must precede `y` under `a`. Robust against minor formatting
    // differences (trailing newlines, quoting of reserved words like `y`).
    let pos_a = on_disk
        .find("a:")
        .expect("missing `a:` key in sorted output");
    let pos_z = on_disk
        .find("z:")
        .expect("missing `z:` key in sorted output");
    assert!(
        pos_a < pos_z,
        "`a:` must precede `z:` after --sort-keys, got:\n{on_disk}",
    );
    let pos_b = on_disk
        .find("b:")
        .expect("missing nested `b:` in sorted output");
    // The writer quotes reserved words (`y`) — match either `y:` or `'y':`.
    let pos_y = on_disk
        .find("y:")
        .or_else(|| on_disk.find("'y':"))
        .expect("missing nested `y:` (or `'y':`) in sorted output");
    assert!(
        pos_b < pos_y,
        "`b:` must precede `y:` after --sort-keys, got:\n{on_disk}",
    );
}

#[test]
fn fmt_preserves_source_format_yaml_to_yaml() {
    // Spec: `dq fmt config.yaml` produces YAML, not JSON. We pin this by
    // rejecting JSON-only artefacts (curly brace at column 0). The output
    // must contain YAML mapping syntax (`key: value` lines).
    let tmp = write_yaml("a: 1\nb: hello\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::try_parse_from(["dq", "fmt", path]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("fmt should succeed");
    let s = String::from_utf8(out).unwrap();
    assert!(
        s.contains("a:") && s.contains("b:"),
        "YAML output must keep `key: value` syntax, got: {s:?}",
    );
    assert!(
        !s.trim_start().starts_with('{'),
        "output must not be JSON (no leading `{{`), got: {s:?}",
    );
}

#[test]
fn fmt_preserves_source_format_json_to_json() {
    // Mirror of the YAML→YAML test for the JSON parser path. We use a
    // non-canonical source (no trailing newline + tight spacing) so the
    // writer produces a non-empty `Modified` result instead of skipping
    // with `Unchanged`. After `-i`, the file on disk is JSON (the writer
    // never silently switches to YAML/TOML).
    let tmp = write_json(r#"{"a":1,"b":"hello"}"#); // no trailing newline
    let path = tmp.to_str().unwrap();
    let cli = Cli::try_parse_from(["dq", "-i", "fmt", path]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("fmt should succeed");
    let on_disk = std::fs::read(&tmp).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_slice(&on_disk).expect("on-disk bytes must be valid JSON");
    assert_eq!(parsed["a"], 1, "key `a` must round-trip, got: {parsed}");
    assert_eq!(
        parsed["b"], "hello",
        "key `b` must round-trip, got: {parsed}",
    );
    // Bytes must look like JSON, not YAML or TOML.
    let s = String::from_utf8(on_disk).unwrap();
    assert!(
        s.trim_start().starts_with('{'),
        "JSON output must start with `{{`, got: {s:?}",
    );
}

#[test]
fn fmt_preserves_source_format_toml_to_toml() {
    // Mirror for the TOML parser path. We use a non-canonical input so
    // the writer produces fresh bytes (Modified rather than Unchanged) and
    // run with `-i` so the on-disk shape is what we assert on. The file
    // must remain TOML, not silently turn into YAML or JSON.
    let mut tmp = NamedTempFile::with_suffix(".toml").unwrap();
    // Tight whitespace so the writer's canonical form differs.
    tmp.write_all(b"a=1\nb=\"hello\"").unwrap();
    let tmp = tmp.into_temp_path();
    let path = tmp.to_str().unwrap();
    let cli = Cli::try_parse_from(["dq", "-i", "fmt", path]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("fmt should succeed");
    let on_disk = std::fs::read_to_string(&tmp).unwrap();
    // TOML uses `=` for key-value; YAML uses `:`. The output must look
    // like TOML, not YAML or JSON.
    assert!(
        on_disk.contains("a = 1") || on_disk.contains("a=1"),
        "TOML output must use `=` syntax, got: {on_disk:?}",
    );
    // Reject YAML/JSON shapes.
    assert!(
        !on_disk.contains("a: 1"),
        "TOML output must not use YAML `:`, got: {on_disk:?}",
    );
    assert!(
        !on_disk.trim_start().starts_with('{'),
        "TOML output must not be JSON, got: {on_disk:?}",
    );
}

#[test]
fn fmt_glob_processes_multiple_files() {
    // Spec: `dq fmt 'tmpdir/*.yaml' -i` rewrites each matching file. Five
    // files seeded with non-canonical content (no trailing newline) → the
    // writer adds it, and all five end up canonical after `-i`.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    for n in 1..=5 {
        let p = dir.path().join(format!("f{n}.yaml"));
        std::fs::write(&p, format!("a: {n}\nb: {n}")).unwrap();
        paths.push(p);
    }
    let glob = format!("{}/*.yaml", dir.path().to_str().unwrap());
    let cli = Cli::try_parse_from(["dq", "-i", "fmt", &glob]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("bulk fmt should succeed");

    let s = String::from_utf8(out).unwrap();
    // Bulk mode appends a `Modified: N, Skipped: M, Failed: K` summary line.
    // We expect 5 modifications because each file is missing the trailing
    // newline (the writer always adds one).
    assert!(
        s.contains("Modified: 5"),
        "expected `Modified: 5` summary, got: {s}",
    );
    assert!(s.contains("Failed: 0"), "no failures expected, got: {s}");

    // Re-running with --check on the same files now exits 0 because each
    // file is canonical post-rewrite.
    let cli2 = Cli::try_parse_from(["dq", "--check", "fmt", &glob]).unwrap();
    let mut out2: Vec<u8> = Vec::new();
    let mut err2: Vec<u8> = Vec::new();
    dq::run(&cli2, false, &mut out2, &mut err2).expect("post-fmt --check must pass");

    // Each file must have at least one trailing newline now.
    for p in &paths {
        let contents = std::fs::read_to_string(p).unwrap();
        assert!(
            contents.ends_with('\n'),
            "post-fmt file {p:?} must end with newline, got: {contents:?}",
        );
    }
}

#[test]
fn fmt_indent_4_for_json() {
    // Spec: `dq fmt config.json --indent 4 -i` produces 4-space indented
    // JSON. The default JSON writer uses 2-space indent; `--indent 4` must
    // override it.
    let tmp = write_json(r#"{"a": 1, "b": [1, 2, 3]}"#);
    let path = tmp.to_str().unwrap();
    let cli = Cli::try_parse_from(["dq", "--indent", "4", "-i", "fmt", path]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("fmt --indent 4 -i should succeed");

    let on_disk = std::fs::read_to_string(&tmp).unwrap();
    // 4-space indent: the line `"a": 1` (or any first-level entry) must be
    // preceded by exactly 4 spaces. Use `\n    "` as a probe — robust
    // against `serde_json` ordering of `a` vs `b`.
    assert!(
        on_disk.contains("\n    \""),
        "expected 4-space indent before `\"`, got:\n{on_disk}",
    );
    // And NOT the 2-space default.
    assert!(
        !on_disk.contains("\n  \""),
        "must not contain 2-space indent under --indent 4, got:\n{on_disk}",
    );
}

#[test]
fn fmt_diff_with_glob_produces_per_file_markers() {
    // Bulk + --diff: each per-file diff is preceded by a `=== <path> ===`
    // marker so consumers can split the stream. Two non-canonical files →
    // two markers in the captured stdout.
    let dir = tempfile::tempdir().expect("tempdir");
    let p1 = dir.path().join("a.yaml");
    let p2 = dir.path().join("b.yaml");
    // Non-canonical: missing trailing newline.
    std::fs::write(&p1, "a: 1\nb: 2").unwrap();
    std::fs::write(&p2, "x: 1\ny: 2").unwrap();
    let glob = format!("{}/*.yaml", dir.path().to_str().unwrap());
    let cli = Cli::try_parse_from(["dq", "--diff", "fmt", &glob]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("bulk fmt --diff should succeed");

    let s = String::from_utf8(out).unwrap();
    let p1_str = p1.to_str().unwrap();
    let p2_str = p2.to_str().unwrap();
    assert!(
        s.contains(&format!("=== {p1_str} ===")),
        "missing `=== <path> ===` marker for {p1_str} in:\n{s}",
    );
    assert!(
        s.contains(&format!("=== {p2_str} ===")),
        "missing `=== <path> ===` marker for {p2_str} in:\n{s}",
    );
}
