//! Integration-level CLI tests for `dq set --jq` driven through `dq::run`.
//!
//! Each test parses a real CLI invocation via `Cli::try_parse_from(...)` and
//! calls `dq::run` with `Vec<u8>` writers, then asserts on the on-disk file
//! contents (for `-i` mode), stdout (for `--diff`), or domain error variants
//! (for the failure paths). We deliberately AVOID `assert_cmd` here — the
//! in-process tests are dramatically faster and give full debuggability.
//!
//! Coverage tracks the `data-query-write` delta spec for the `--jq` mode of
//! `dq set`:
//! - increment, add-key, delete-key happy paths (`-i`)
//! - `--diff` renders unified diff, file unchanged
//! - compile error → `PARSE_ERROR` (3)
//! - runtime error → `GENERIC` (1)
//! - `--check` against changes-pending → `CheckPending` → exit 1
//! - `--check` against no-op transform → exit 0
//! - re-emit drops YAML comments (documented behaviour, not a bug)
//!
//! Per-file fixtures use `tempfile::NamedTempFile` so multiple tests run in
//! parallel without sharing state and the checked-in fixtures are never
//! mutated.
//!
//! On-disk content is asserted via raw substring checks rather than re-parsing
//! into a JSON `Value`. This keeps the test crate's dev-dependencies minimal
//! (no new `serde_yaml` import needed) and matches the style of `unit_set.rs`.

use std::io::Write as _;

use clap::Parser;
use dq::Cli;
use dq::exit_code;
use tempfile::NamedTempFile;

/// Write `content` to a YAML temp file, kept alive for the test's lifetime.
fn write_yaml(content: &str) -> NamedTempFile {
    let mut tmp = NamedTempFile::with_suffix(".yaml").expect("tempfile");
    tmp.write_all(content.as_bytes()).expect("write tempfile");
    tmp
}

#[test]
fn set_jq_increments_counter_in_place() {
    // Spec scenario "`--jq` increments a counter": `.spec.replicas |= . + 1`
    // bumps the field on disk and exits 0. Stdout is empty under `-i` mode.
    let tmp = write_yaml("spec:\n  replicas: 3\n");
    let path = tmp.path().to_str().unwrap();
    let cli = Cli::try_parse_from([
        "dq",
        "-i",
        "set",
        path,
        "--jq",
        ".spec.replicas |= . + 1",
        "--no-color",
    ])
    .unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("--jq increment must succeed");
    assert!(
        out.is_empty(),
        "in-place mode must not write to stdout, got: {:?}",
        String::from_utf8_lossy(&out),
    );

    let on_disk = std::fs::read_to_string(tmp.path()).unwrap();
    assert!(
        on_disk.contains("replicas: 4"),
        "expected `replicas: 4` on disk, got:\n{on_disk}",
    );
    assert!(
        !on_disk.contains("replicas: 3"),
        "old value `replicas: 3` must be gone, got:\n{on_disk}",
    );
}

#[test]
fn set_jq_adds_new_key_to_object() {
    // Spec scenario "`--jq` adds a new key": `. + {"newKey": "newValue"}`
    // unions a new top-level entry. Existing keys must survive.
    let tmp = write_yaml("foo: 1\nbar: hello\n");
    let path = tmp.path().to_str().unwrap();
    let cli = Cli::try_parse_from([
        "dq",
        "-i",
        "set",
        path,
        "--jq",
        ". + {\"newKey\": \"newValue\"}",
        "--no-color",
    ])
    .unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("--jq add-key must succeed");

    let on_disk = std::fs::read_to_string(tmp.path()).unwrap();
    assert!(
        on_disk.contains("newKey:") && on_disk.contains("newValue"),
        "newKey: newValue must land on disk, got:\n{on_disk}",
    );
    assert!(
        on_disk.contains("foo:") && on_disk.contains("1"),
        "foo must survive, got:\n{on_disk}",
    );
    assert!(
        on_disk.contains("bar:") && on_disk.contains("hello"),
        "bar must survive, got:\n{on_disk}",
    );
}

#[test]
fn set_jq_deletes_nested_key_preserving_siblings() {
    // Spec scenario "`--jq` removes a key": `del(.metadata.annotations.old)`
    // drops the `old` key; sibling keys at the same level must remain.
    //
    // Use unambiguous values for both keys ("drop-me" / "keep-me") so the
    // post-delete substring assertion is unambiguous.
    let tmp = write_yaml(concat!(
        "metadata:\n",
        "  annotations:\n",
        "    old: drop-me\n",
        "    new: keep-me\n",
    ));
    let path = tmp.path().to_str().unwrap();
    let cli = Cli::try_parse_from([
        "dq",
        "-i",
        "set",
        path,
        "--jq",
        "del(.metadata.annotations.old)",
        "--no-color",
    ])
    .unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("--jq del must succeed");

    let on_disk = std::fs::read_to_string(tmp.path()).unwrap();
    assert!(
        !on_disk.contains("drop-me"),
        "deleted value `drop-me` must be gone, got:\n{on_disk}",
    );
    assert!(
        !on_disk.contains("old:"),
        "deleted key `old:` must be gone, got:\n{on_disk}",
    );
    assert!(
        on_disk.contains("keep-me"),
        "sibling value `keep-me` must survive, got:\n{on_disk}",
    );
    assert!(
        on_disk.contains("new:"),
        "sibling key `new:` must survive, got:\n{on_disk}",
    );
}

#[test]
fn set_jq_with_diff_renders_unified_diff_and_leaves_file_unchanged() {
    // Spec scenario "`--jq` with `--diff` renders unified diff": the diff
    // contains both the removed-original (`-replicas: 3`) and the added-new
    // (`+replicas: 4`) lines, and the file on disk is untouched.
    let tmp = write_yaml("spec:\n  replicas: 3\n");
    let path = tmp.path().to_str().unwrap();
    let original = std::fs::read_to_string(tmp.path()).unwrap();
    let cli = Cli::try_parse_from([
        "dq",
        "--diff",
        "set",
        path,
        "--jq",
        ".spec.replicas |= . + 1",
        "--no-color",
    ])
    .unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("--diff with --jq must succeed");

    let s = String::from_utf8(out).unwrap();
    // The diff path uses two-space-indented YAML for the `replicas:` value
    // line, mirroring the splice path's diff (see set_diff_mode tests in
    // unit_set.rs / cli_smoke.rs). Both `-` and `+` must appear.
    assert!(
        s.contains("-  replicas: 3"),
        "expected `-  replicas: 3` in diff, got:\n{s}",
    );
    assert!(
        s.contains("+  replicas: 4"),
        "expected `+  replicas: 4` in diff, got:\n{s}",
    );
    // File on disk untouched in --diff mode.
    let after = std::fs::read_to_string(tmp.path()).unwrap();
    assert_eq!(after, original, "--diff must NOT modify the file");
}

#[test]
fn set_jq_compile_error_maps_to_parse_error_exit_three() {
    // Spec scenario "`--jq` compile error maps to PARSE_ERROR": malformed
    // jq syntax at compile time surfaces as `dq_core::Error::Parse` so the
    // exit-code mapper picks 3 (same family as file-parse failures).
    let tmp = write_yaml("a: 1\n");
    let path = tmp.path().to_str().unwrap();
    let cli =
        Cli::try_parse_from(["dq", "-i", "set", path, "--jq", ".foo |=", "--no-color"]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e =
        dq::run(&cli, false, &mut out, &mut err).expect_err("malformed jq expression must error");
    let domain = e
        .downcast_ref::<dq_core::Error>()
        .expect("compile failures map to dq_core::Error::Parse");
    assert_eq!(
        domain.kind_name(),
        "parse",
        "expected `parse` kind, got {domain:?}",
    );
    assert_eq!(
        exit_code::exit_code_for_error(&e),
        exit_code::PARSE_ERROR,
        "compile failures must map to exit 3 (PARSE_ERROR), got: {e:?}",
    );
}

#[test]
fn set_jq_runtime_error_maps_to_generic_exit_one() {
    // Spec scenario "`--jq` runtime error maps to GENERIC": file parsed
    // fine, expression compiled fine, but the evaluation against this
    // specific data (string + number) failed → exit 1.
    let tmp = write_yaml("\"hello\"\n");
    let path = tmp.path().to_str().unwrap();
    let cli =
        Cli::try_parse_from(["dq", "-i", "set", path, "--jq", ". + 1", "--no-color"]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e = dq::run(&cli, false, &mut out, &mut err)
        .expect_err("runtime type error in jq filter must surface");
    assert!(
        e.downcast_ref::<dq_core::Error>().is_none(),
        "runtime errors must NOT downcast to dq_core::Error (would mis-map to exit 3), got: {e:?}",
    );
    assert_eq!(
        exit_code::exit_code_for_error(&e),
        exit_code::GENERIC,
        "runtime errors must map to exit 1 (GENERIC), got: {e:?}",
    );
}

#[test]
fn set_jq_check_against_pending_change_exits_one_via_check_pending() {
    // Spec scenario "`--jq` with `--check` reports pending change": when the
    // transform would change the file, the bulk driver raises
    // `CheckPending` which the exit-code mapper translates to GENERIC (1).
    // The file on disk MUST stay unchanged.
    let tmp = write_yaml("spec:\n  replicas: 3\n");
    let path = tmp.path().to_str().unwrap();
    let original = std::fs::read_to_string(tmp.path()).unwrap();
    let cli = Cli::try_parse_from([
        "dq",
        "--check",
        "set",
        path,
        "--jq",
        ".spec.replicas |= . + 1",
        "--no-color",
    ])
    .unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e = dq::run(&cli, false, &mut out, &mut err)
        .expect_err("--check against pending change must error");
    assert!(
        e.downcast_ref::<dq::error::CheckPending>().is_some(),
        "rejection must carry CheckPending marker, got: {e:?}",
    );
    assert_eq!(
        exit_code::exit_code_for_error(&e),
        exit_code::GENERIC,
        "CheckPending must map to exit 1 (GENERIC), got: {e:?}",
    );
    let after = std::fs::read_to_string(tmp.path()).unwrap();
    assert_eq!(after, original, "--check must NOT modify the file");
}

#[test]
fn set_jq_check_against_no_op_transform_exits_zero() {
    // Spec scenario "`--jq` is idempotent through `--check`": a no-op
    // transform (`. + 0`) leaves the document structurally identical, so
    // `--check` reports no pending changes and exits 0.
    //
    // CAVEAT: the re-emit path produces canonicalised YAML even for a
    // no-op transform, so the source must already be in the canonical form
    // the writer emits. Single top-level scalar key is the simplest shape
    // that round-trips byte-identically through the YAML emitter.
    let tmp = write_yaml("replicas: 3\n");
    let path = tmp.path().to_str().unwrap();
    let original = std::fs::read_to_string(tmp.path()).unwrap();
    let cli = Cli::try_parse_from([
        "dq",
        "--check",
        "set",
        path,
        "--jq",
        ".replicas |= . + 0",
        "--no-color",
    ])
    .unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("--check on no-op transform must exit 0");
    let after = std::fs::read_to_string(tmp.path()).unwrap();
    assert_eq!(after, original, "no-op --check must NOT modify the file");
}

#[test]
fn set_jq_drops_yaml_comments_on_re_emit_documented_behaviour() {
    // Spec scenario "`--jq` re-emits via the native writer (comment loss)":
    // `dq set --jq` deliberately bypasses the M2 textual-edit splice and
    // routes through `Format::write_with_options`, which does NOT preserve
    // YAML comments. This test PINS that contract — if a future change
    // accidentally preserves comments on this path, this test must be
    // updated intentionally (e.g. swap to assert their presence).
    //
    // Source has a leading line-comment plus a trailing one; both must be
    // gone after re-emit, and the new value must land on disk.
    let tmp = write_yaml("# leading comment\nx: 1   # trailing comment\n");
    let path = tmp.path().to_str().unwrap();
    let cli =
        Cli::try_parse_from(["dq", "-i", "set", path, "--jq", ".x |= 2", "--no-color"]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("--jq with comments must succeed");

    let on_disk = std::fs::read_to_string(tmp.path()).unwrap();
    assert!(
        !on_disk.contains("# leading comment"),
        "re-emit drops comments by design, but `# leading comment` survived: {on_disk}",
    );
    assert!(
        !on_disk.contains("# trailing comment"),
        "re-emit drops comments by design, but `# trailing comment` survived: {on_disk}",
    );
    assert!(
        on_disk.contains("x: 2"),
        "the new value must land on disk, got:\n{on_disk}",
    );
}
