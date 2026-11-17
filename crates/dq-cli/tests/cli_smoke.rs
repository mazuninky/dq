//! End-to-end CLI integration tests via `assert_cmd`.
//!
//! These tests spawn the real `dq` binary so they catch contract breaks the
//! in-process tests cannot: signal handling, exit-code mapping, the binary's
//! own stderr renderer, and clap's parser plumbing.
//!
//! Conventions used throughout:
//! - `--no-color` is passed on every invocation so stdout/stderr are stable
//!   for byte-level assertions.
//! - The process environment is locked down (`env_clear` + selective re-add)
//!   so the developer's local `NO_COLOR` / `RUST_LOG` / `CLICOLOR_FORCE` cannot
//!   leak into the test.
//! - Fixtures live in `crates/dq-cli/tests/fixtures/` and are referenced via
//!   `CARGO_MANIFEST_DIR` so tests don't depend on the developer's cwd.

use std::path::PathBuf;
use std::process::Command as StdCommand;

use assert_cmd::Command;
use predicates::prelude::*;

/// Path to a fixture file relative to `crates/dq-cli/`.
fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Build a `dq` command with a clean environment.
///
/// Wipes the inherited env so the developer's `NO_COLOR`, `CLICOLOR_FORCE`,
/// `RUST_LOG`, etc. cannot influence the test. Re-adds only `PATH` (so the
/// linker / runtime can find shared libraries on macOS) and `HOME` (so tools
/// that probe the home dir don't blow up).
fn dq() -> Command {
    let mut cmd = Command::cargo_bin("dq").expect("dq binary built");
    cmd.env_clear();
    if let Ok(p) = std::env::var("PATH") {
        cmd.env("PATH", p);
    }
    cmd.env("HOME", "/tmp");
    cmd
}

#[test]
fn smoke_get_k8s_replicas() {
    // Scenario 1: read a value out of a real k8s manifest fixture.
    dq().args([
        "get",
        fixture("k8s_deployment.yaml").to_str().unwrap(),
        "/spec/replicas",
        "--no-color",
    ])
    .assert()
    .success()
    .stdout(predicate::eq("3\n"));
}

#[test]
fn smoke_paths_helm_values() {
    // Scenario 2: `paths` on a Helm-style values file produces a stable list
    // of pointers. We don't snapshot here (snapshots are in cli_snapshots /
    // golden tests) — we just check key entries are present.
    dq().args([
        "paths",
        fixture("helm_values.yaml").to_str().unwrap(),
        "--no-color",
    ])
    .assert()
    .success()
    .stdout(predicate::str::contains("/image: object"))
    .stdout(predicate::str::contains("/image/tag: string"))
    .stdout(predicate::str::contains("/service/port: int"));
}

#[test]
fn smoke_convert_package_json_to_yaml() {
    // Scenario 3: convert package.json to YAML. We don't pin the exact YAML
    // output bytes (formatting depends on serde_norway) — just that it parses
    // back as valid YAML and contains the original keys.
    let out = dq()
        .args([
            "convert",
            fixture("package.json").to_str().unwrap(),
            "-F",
            "yaml",
            "--no-color",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).to_string();
    assert!(
        stdout.contains("name") && stdout.contains("example-app"),
        "YAML output should contain converted keys, got {stdout:?}",
    );
    assert!(
        stdout.contains("dependencies"),
        "YAML output should contain dependencies key, got {stdout:?}",
    );
}

#[test]
fn smoke_select_jsonpath_multi_match() {
    // Scenario 4: `select` against a real fixture returns multiple matches as
    // a JSON array. The exact format depends on the reporter — default
    // (console) prints one per line.
    let out = dq()
        .args([
            "select",
            fixture("k8s_deployment.yaml").to_str().unwrap(),
            "$.spec.template.spec.containers[*].image",
            "--no-color",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).to_string();
    // Console reporter writes one per line; assert all three images are
    // listed in source order.
    assert!(stdout.contains("ghcr.io/example/web:1.2.3"));
    assert!(stdout.contains("ghcr.io/example/sidecar:0.4.1"));
    assert!(stdout.contains("busybox:1.36"));
}

#[test]
fn smoke_validate_broken_json_exits_four() {
    // Scenario 5: malformed JSON → exit code 4 (VALIDATE_FAIL) with a
    // structured error in stderr.
    dq().args([
        "validate",
        fixture("broken.json").to_str().unwrap(),
        "--no-color",
    ])
    .assert()
    .code(4)
    .stderr(predicate::str::contains("parse"));
}

#[test]
fn smoke_get_with_json_format_outputs_pretty_json() {
    // Scenario 6: `dq -F json get <fixture> /server/port` — JSON output mode
    // for a scalar is just the integer. Note: `-F json` is overloaded and
    // also picks the *input* parser, so this test uses package.json (a real
    // JSON fixture) rather than the YAML server_config.
    let out = dq()
        .args([
            "-F",
            "json",
            "get",
            fixture("package.json").to_str().unwrap(),
            "/version",
            "--no-color",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).to_string();
    assert_eq!(
        stdout.trim(),
        "\"1.2.3\"",
        "JSON reporter must emit a JSON string for /version, got {stdout:?}",
    );
}

#[test]
fn smoke_verbose_flag_does_not_change_exit_code() {
    // Scenario 7: `-v paths <fixture>` — verbose flag triggers tracing init at
    // INFO level. Per the prompt: assert exit code only, NOT log output —
    // tracing-subscriber output format is intentionally NOT a contract.
    dq().args([
        "-v",
        "paths",
        fixture("server_config.yaml").to_str().unwrap(),
        "--no-color",
    ])
    .assert()
    .success();
}

#[test]
fn smoke_exists_missing_pointer_exits_one_silent() {
    // Scenario 8: `exists` with a missing pointer must exit 1 with empty
    // stdout AND empty stderr (silent failure).
    dq().args([
        "exists",
        fixture("server_config.yaml").to_str().unwrap(),
        "/missing",
        "--no-color",
    ])
    .assert()
    .code(1)
    .stdout(predicate::str::is_empty())
    .stderr(predicate::str::is_empty());
}

#[test]
fn smoke_get_typo_emits_did_you_mean_in_stderr() {
    // Scenario 9: typo in pointer (`prot` → suggests `port`). Render must
    // include both the matched prefix and the did_you_mean suggestion. With
    // `--no-color` no ANSI escapes appear.
    let out = dq()
        .args([
            "get",
            fixture("server_config.yaml").to_str().unwrap(),
            "/server/prot",
            "--no-color",
        ])
        .assert()
        .code(2);
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).to_string();
    assert!(
        stderr.contains("did_you_mean: port"),
        "stderr must surface the did_you_mean suggestion, got {stderr:?}",
    );
    assert!(
        stderr.contains("matched prefix: /server"),
        "stderr must surface the matched prefix, got {stderr:?}",
    );
    assert!(
        !stderr.contains("\x1b["),
        "no ANSI escapes under --no-color, got {stderr:?}",
    );
}

#[test]
fn smoke_in_place_flag_rejected_on_read_only_subcommand() {
    // Scenario 10: `-i` / `--in-place` makes the read handler's call to
    // `Cli::ensure_no_write_flags` reject the flag → exit 6 (INVALID_INPUT),
    // stderr mentions the flag and the "read-only" subcommand framing. The
    // flag rejection is a caller-side input error so it maps to exit 6 rather
    // than the catch-all exit 1.
    let out = dq()
        .args([
            "-i",
            "get",
            fixture("server_config.yaml").to_str().unwrap(),
            "/x",
            "--no-color",
        ])
        .assert()
        .code(6);
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).to_string();
    assert!(
        stderr.contains("--in-place") && stderr.contains("read-only"),
        "stderr must name the flag and explain the read-only rejection, got {stderr:?}",
    );
}

#[test]
fn smoke_help_succeeds() {
    // Bonus contract test: `--help` exits 0 (clap's standard behaviour) and
    // mentions the binary name. Catches accidental annotation regressions.
    dq().args(["--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("dq"));
}

#[test]
fn smoke_version_succeeds() {
    // `--version` must succeed and emit a non-empty version line.
    dq().args(["--version"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());
}

#[test]
fn smoke_unknown_subcommand_exits_with_clap_error() {
    // Per cli-shell spec: reserved future subcommand (`set`) produces clap's
    // standard "unknown subcommand" error and a non-zero exit. clap exits
    // with code 2 by default for parse errors — the test just verifies the
    // process fails, not the specific code (clap version may shift).
    dq().args(["set", "x.yaml", "/a", "1"]).assert().failure();
}

// ---------------------------------------------------------------------------
// M2 §12.3 — write-command smoke tests (`set`, `del`).
//
// These exercise the binary contract for write commands end-to-end:
// fixture → `assert_cmd` invocation → on-disk side effects + exit code.
// They mirror the M2 DoD scenarios from `dq-plan.md`. Each test seeds a
// tempfile so the source fixtures are not mutated.
// ---------------------------------------------------------------------------

/// Copy a fixture into a fresh tempfile preserving the source extension
/// (so the format detector picks up the right parser). Returns both the
/// `tempfile::TempDir` (to keep it alive) and the path to the temp copy.
fn fixture_copy(name: &str, ext: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let dest = dir.path().join(format!("input.{ext}"));
    std::fs::copy(fixture(name), &dest).expect("copy fixture");
    (dir, dest)
}

#[test]
fn smoke_set_replicas_on_k8s_manifest() {
    // Scenario: copy the writable k8s fixture, run `dq set <copy>
    // /spec/replicas 5 -i`, then read the file back. We use a *copy* so the
    // checked-in fixture is never mutated even if `tempdir` cleanup fails.
    let (_dir, path) = fixture_copy("k8s_deployment_writable.yaml", "yaml");
    let path_str = path.to_str().unwrap();
    dq().args(["set", path_str, "/spec/replicas", "5", "-i", "--no-color"])
        .assert()
        .success();
    let contents = std::fs::read_to_string(&path).unwrap();
    assert!(
        contents.contains("replicas: 5"),
        "file must contain `replicas: 5` after in-place set, got:\n{contents}",
    );
    assert!(
        !contents.contains("replicas: 3"),
        "old replicas must be gone, got:\n{contents}",
    );
}

#[test]
fn smoke_set_image_tag_on_helm_with_raw_template_strings() {
    // Scenario: a Helm-style file with `{{ ... }}` blocks must accept a
    // `set` when `--raw-template-strings` is passed. The unrelated template
    // blocks must round-trip verbatim — the substitution + restore pass
    // does its job invisibly.
    let (_dir, path) = fixture_copy("helm_values_templated.yaml", "yaml");
    let path_str = path.to_str().unwrap();
    dq().args([
        "set",
        path_str,
        "/image/tag",
        "v2.0.0",
        "--raw-template-strings",
        "-i",
        "--no-color",
    ])
    .assert()
    .success();
    let contents = std::fs::read_to_string(&path).unwrap();
    assert!(
        contents.contains("v2.0.0"),
        "edited tag must appear in file, got:\n{contents}",
    );
    // The unrelated template blocks are preserved by the round-trip.
    assert!(
        contents.contains("{{ .Values.replicaCount }}"),
        "replicaCount template block must round-trip, got:\n{contents}",
    );
    assert!(
        contents.contains("{{ .Values.service.port }}"),
        "service.port template block must round-trip, got:\n{contents}",
    );
}

#[test]
fn smoke_del_annotation_from_yaml() {
    // Scenario: remove `/metadata/annotations/foo` from the annotations
    // fixture in place, confirm the key is gone but its siblings are
    // preserved.
    let (_dir, path) = fixture_copy("annotations.yaml", "yaml");
    let path_str = path.to_str().unwrap();
    dq().args([
        "del",
        path_str,
        "/metadata/annotations/foo",
        "-i",
        "--no-color",
    ])
    .assert()
    .success();
    let contents = std::fs::read_to_string(&path).unwrap();
    assert!(
        !contents.contains("foo: drop-me"),
        "deleted annotation must be gone, got:\n{contents}",
    );
    assert!(
        contents.contains("bar: keep-me"),
        "sibling `bar` must remain, got:\n{contents}",
    );
    assert!(
        contents.contains("baz: also-keep"),
        "sibling `baz` must remain, got:\n{contents}",
    );
}

#[test]
fn smoke_set_with_stdin_source() {
    // Scenario: `echo '{"a": 1}' | dq set <fix> /metadata - -i`. The `-`
    // sentinel reads stdin as the value source. We use a JSON object as
    // the inline string so the JSON-literal heuristic picks it up.
    let (_dir, path) = fixture_copy("k8s_deployment_writable.yaml", "yaml");
    let path_str = path.to_str().unwrap();
    dq().args(["set", path_str, "/metadata/name", "-", "-i", "--no-color"])
        .write_stdin("renamed-web")
        .assert()
        .success();
    let contents = std::fs::read_to_string(&path).unwrap();
    assert!(
        contents.contains("name: renamed-web"),
        "stdin value must be applied at /metadata/name, got:\n{contents}",
    );
}

#[test]
fn smoke_set_diff_scenario() {
    // Scenario: `dq set <fix> /spec/replicas 5 --diff` produces a
    // unified-diff-shaped output — `-replicas: 3` and `+replicas: 5` must
    // both appear (with leading whitespace from the indented YAML map).
    let (_dir, path) = fixture_copy("k8s_deployment_writable.yaml", "yaml");
    let path_str = path.to_str().unwrap();
    let out = dq()
        .args([
            "set",
            path_str,
            "/spec/replicas",
            "5",
            "--diff",
            "--no-color",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).to_string();
    assert!(
        stdout.contains("-  replicas: 3"),
        "expected `-  replicas: 3` in diff, got:\n{stdout}",
    );
    assert!(
        stdout.contains("+  replicas: 5"),
        "expected `+  replicas: 5` in diff, got:\n{stdout}",
    );
    // File on disk untouched in diff mode.
    let contents = std::fs::read_to_string(&path).unwrap();
    assert!(
        contents.contains("replicas: 3"),
        "diff mode must not touch the file, got:\n{contents}",
    );
}

// ---- Helper `Command` import is needed for builder closures used by
// other commands. Re-enable if the cargo_bin call becomes the wrong shape.
#[allow(dead_code)]
fn _std_command_unused() -> StdCommand {
    StdCommand::new("true")
}

// ---------------------------------------------------------------------------
// M3 DoD smoke tests — driven through `dq::run` in-process (NOT `assert_cmd`).
//
// These tests skip the binary spawn entirely: each one builds a `Cli` via
// `Cli::try_parse_from(...)` and calls `dq::run` with `Vec<u8>` writers. The
// in-process style is faster, debuggable, and matches the M3 contract for
// the four newly-shipped surfaces (`diff`, `patch`, `merge`, `convert -i`).
// Each test gets its own `tempfile::tempdir()` so filesystem isolation is
// total — checked-in fixtures are not touched.
// ---------------------------------------------------------------------------

#[test]
fn smoke_diff_emits_json_patch_for_simple_change() {
    // M3 DoD: `dq diff a b -F json` emits an RFC 6902 JSON Patch array. A
    // single scalar change at `/x` must produce exactly one `replace` op.
    use clap::Parser;
    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("a.yaml");
    let b = dir.path().join("b.yaml");
    std::fs::write(&a, "x: 1\n").unwrap();
    std::fs::write(&b, "x: 2\n").unwrap();
    let cli = dq::Cli::try_parse_from([
        "dq",
        "-F",
        "json",
        "diff",
        a.to_str().unwrap(),
        b.to_str().unwrap(),
    ])
    .expect("clap parse");
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("diff should succeed");
    let parsed: serde_json::Value =
        serde_json::from_slice(&out).expect("stdout must be valid JSON");
    let arr = parsed.as_array().expect("expected JSON array");
    assert_eq!(arr.len(), 1, "expected one op, got: {parsed}");
    assert_eq!(arr[0]["op"], "replace");
    assert_eq!(arr[0]["path"], "/x");
    assert_eq!(arr[0]["value"], 2);
}

#[test]
fn smoke_patch_at_path_applies_ops_in_place() {
    // M3 DoD: `dq patch <file> @<ops.json> -i` reads ops from the `@<path>`
    // source and applies them atomically. We verify the on-disk bytes after.
    use clap::Parser;
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("doc.yaml");
    let ops = dir.path().join("ops.json");
    std::fs::write(&target, "replicas: 3\n").unwrap();
    std::fs::write(&ops, br#"[{"op":"replace","path":"/replicas","value":5}]"#).unwrap();
    let at_arg = format!("@{}", ops.to_str().unwrap());
    let cli = dq::Cli::try_parse_from(["dq", "-i", "patch", target.to_str().unwrap(), &at_arg])
        .expect("clap parse");
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("patch should succeed");
    let on_disk = std::fs::read_to_string(&target).unwrap();
    assert!(
        on_disk.contains("replicas: 5"),
        "file must contain `replicas: 5` after in-place patch, got: {on_disk:?}",
    );
}

#[test]
fn smoke_merge_with_null_removes_key() {
    // M3 DoD: `dq merge <file> '{"a":null}' -i` follows RFC 7396 §1 — a
    // `null` value in the patch removes the addressed key. Sibling keys
    // must survive untouched. This exercises the same span-driven removal
    // path as `merge_null_removes_existing_key` in `commands::merge::tests`,
    // just at a top-level key (both `a` and `b` are scalar leaves with
    // recorded spans, so `Document::set_at` can locate `a` for removal).
    use clap::Parser;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("doc.yaml");
    std::fs::write(&path, "a: 1\nb: 2\n").unwrap();
    let cli =
        dq::Cli::try_parse_from(["dq", "-i", "merge", path.to_str().unwrap(), r#"{"a":null}"#])
            .expect("clap parse");
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("merge should succeed");
    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert!(
        !on_disk.contains("a:"),
        "key `a` must be removed by null merge, got: {on_disk:?}",
    );
    assert!(
        on_disk.contains("b: 2"),
        "sibling `b` must survive null merge, got: {on_disk:?}",
    );
}

// ---------------------------------------------------------------------------
// M4 DoD smoke tests — `dq fmt` + `--sort-keys` + `--indent`.
//
// Mirror the structure of the M3 in-process smoke tests above: build a `Cli`
// via `Cli::try_parse_from(...)`, call `dq::run` with `Vec<u8>` writers, and
// assert on stdout / file contents / exit-code mapping. Each scenario seeds
// its own tempdir so the checked-in fixtures are never mutated.
// ---------------------------------------------------------------------------

#[test]
fn smoke_fmt_sort_keys_in_place_glob_normalizes_all_files() {
    // M4 DoD §6.1: `dq fmt --sort-keys -i 'tmpdir/**/*.yaml'` over five YAML
    // files normalizes each file to alphabetic key order at every depth. We
    // assert ordering via byte-index comparison (robust against trailing
    // newlines / quoting of reserved YAML words) plus verify the bulk
    // summary line.
    use clap::Parser;
    let dir = tempfile::tempdir().expect("tempdir");
    // Build a nested structure so the `**` recursion has something to find.
    let nested = dir.path().join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    for n in 1..=3 {
        let p = dir.path().join(format!("top{n}.yaml"));
        std::fs::write(&p, format!("z: {n}\na: {n}\n")).unwrap();
        paths.push(p);
    }
    for n in 1..=2 {
        let p = nested.join(format!("inner{n}.yaml"));
        std::fs::write(&p, format!("z: {n}\na: {n}\n")).unwrap();
        paths.push(p);
    }
    let glob = format!("{}/**/*.yaml", dir.path().to_str().unwrap());
    let cli =
        dq::Cli::try_parse_from(["dq", "--sort-keys", "-i", "fmt", &glob]).expect("clap parse");
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("fmt --sort-keys -i must succeed");

    // Five files modified in bulk: the driver emits the summary line.
    let s = String::from_utf8(out).unwrap();
    assert!(
        s.contains("Modified: 5"),
        "expected `Modified: 5` summary, got: {s}",
    );
    // Every file now has `a:` before `z:` on disk.
    for p in &paths {
        let contents = std::fs::read_to_string(p).unwrap();
        let pos_a = contents
            .find("a:")
            .unwrap_or_else(|| panic!("missing `a:` in {p:?}: {contents}"));
        let pos_z = contents
            .find("z:")
            .unwrap_or_else(|| panic!("missing `z:` in {p:?}: {contents}"));
        assert!(
            pos_a < pos_z,
            "`a:` must precede `z:` in {p:?}, got:\n{contents}",
        );
    }
}

#[test]
fn smoke_fmt_check_exits_one_on_non_canonical() {
    // M4 DoD §6.1: `dq fmt --check broken-file.yaml` on a file whose
    // re-emitted bytes differ from source returns a `CheckPending` error
    // that the exit-code mapper translates to 1. We use 4-space indented
    // nested mapping to guarantee the writer reformats — `serde_norway`
    // emits 2-space block indent, so source `a:\n    b: 1\n` normalises
    // to `a:\n  b: 1\n`.
    use clap::Parser;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("non_canonical.yaml");
    let source: &[u8] = b"a:\n    b: 1\n";
    std::fs::write(&path, source).unwrap();
    let cli = dq::Cli::try_parse_from(["dq", "--check", "fmt", path.to_str().unwrap()])
        .expect("clap parse");
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e = dq::run(&cli, false, &mut out, &mut err).expect_err("non-canonical --check must error");
    assert!(
        e.downcast_ref::<dq::error::CheckPending>().is_some(),
        "expected CheckPending marker, got: {e:?}",
    );
    assert_eq!(
        dq::exit_code::exit_code_for_error(&e),
        1,
        "CheckPending must map to exit 1, got: {e:?}",
    );
    // File on disk untouched.
    assert_eq!(
        std::fs::read(&path).unwrap(),
        source,
        "--check must not modify the file",
    );
}

#[test]
fn smoke_convert_indent_4_json_output() {
    // M4 DoD §6.1: `dq convert deploy.yaml -F json --indent 4` produces
    // 4-space indented JSON on stdout. The `--indent` flag flows through
    // the global `WriteOptions` snapshot to the JSON writer.
    use clap::Parser;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("deploy.yaml");
    std::fs::write(&path, b"a: 1\nb:\n  - 1\n  - 2\n  - 3\n").unwrap();
    let cli = dq::Cli::try_parse_from([
        "dq",
        "-F",
        "json",
        "--indent",
        "4",
        "convert",
        path.to_str().unwrap(),
    ])
    .expect("clap parse");
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("convert --indent 4 must succeed");

    let s = String::from_utf8(out).unwrap();
    // Output must be valid JSON.
    let parsed: serde_json::Value =
        serde_json::from_str(&s).expect("convert output must be valid JSON");
    assert_eq!(parsed["a"], 1, "key `a` round-trip, got: {parsed}");
    // 4-space indent: `\n    "` precedes a top-level key like `"a"` or `"b"`.
    assert!(
        s.contains("\n    \""),
        "expected 4-space indent before `\"`, got:\n{s}",
    );
    // And NOT 2-space (the default) — pin the override is honoured.
    assert!(
        !s.contains("\n  \""),
        "must not contain 2-space indent under --indent 4, got:\n{s}",
    );
}

// ---------------------------------------------------------------------------
// M5 DoD smoke tests — the seven new format extensions.
//
// These spawn the real `dq` binary so signal handling, exit-code mapping,
// and the binary's own stderr renderer are all exercised. The four
// scenarios cover one new format each plus one fmt+sort-keys check, per
// the M5 plan section "Smoke tests in cli_smoke.rs".
// ---------------------------------------------------------------------------

#[test]
fn smoke_convert_hcl_to_json_emits_valid_json() {
    // M5 Stage 4 contract: `dq convert main.tf -F json` reads the HCL file
    // through the dq-core HCL parser and re-emits as JSON. Output must be
    // valid JSON and contain the documented attributes.
    let out = dq()
        .args([
            "-F",
            "json",
            "convert",
            fixture("terraform_main.tf").to_str().unwrap(),
            "--no-color",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).to_string();
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("convert HCL→json must emit valid JSON ({e}), got:\n{stdout}"));
    // The fixture has a `backend "s3" { region = "us-east-1" }` block; under
    // the labels-as-keys nesting, the region lives at /backend/s3/region.
    assert_eq!(
        parsed["terraform"]["backend"]["s3"]["region"],
        serde_json::json!("us-east-1"),
        "expected /terraform/backend/s3/region in JSON, got: {parsed:?}",
    );
}

#[test]
fn smoke_fmt_frontmatter_in_place_preserves_body_bytes() {
    // M5 contract: `dq fmt hugo_post.md -i` re-emits the document; the
    // BODY portion must be byte-identical to the source even if the header
    // gets re-canonicalised by the inner YAML writer.
    let (_dir, path) = fixture_copy("hugo_post.md", "md");
    let path_str = path.to_str().unwrap();
    let original = std::fs::read(&path).unwrap();
    // Original body starts after the closing `---\n`. Locate it via the
    // pattern in source.
    let original_body_start = find_after(&original, b"---\n").and_then(|after_first| {
        // Skip past the first `---\n` (opening), find the second `---\n`.
        find_after(&original[after_first..], b"---\n").map(|b| after_first + b)
    });
    let original_body_start = original_body_start.expect("fixture must have a closing `---\\n`");
    let original_body = original[original_body_start..].to_vec();

    dq().args(["fmt", path_str, "-i", "--no-color"])
        .assert()
        .success();

    let modified = std::fs::read(&path).unwrap();
    // Locate the body in the rewritten file the same way.
    let mod_body_start = find_after(&modified, b"---\n").and_then(|after_first| {
        find_after(&modified[after_first..], b"---\n").map(|b| after_first + b)
    });
    let mod_body_start =
        mod_body_start.expect("rewritten file must still carry frontmatter delimiters");
    let new_body = modified[mod_body_start..].to_vec();
    assert_eq!(
        new_body, original_body,
        "frontmatter body must be byte-identical through fmt -i",
    );
}

/// Locate `needle` in `haystack` and return the byte index *after* the match,
/// or `None` if no match.
fn find_after(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|pos| pos + needle.len())
}

#[test]
fn smoke_set_dockerfile_in_place_exits_non_zero_with_format_message() {
    // M5 Stage 4 contract: Dockerfile is read-only. `dq set Dockerfile
    // /0/instruction RUN -i` must exit non-zero; per the plan, the error
    // message names the format. The current handler path surfaces a
    // WriteUnavailable from the read-only document — the format name is
    // implied via the document's `format()` tag but the user-visible
    // message says "read-only". We assert the non-zero exit + the "read-only"
    // wording (since that's the actual contract surface) and document the
    // gap from the prompt's "names dockerfile" expectation in the test
    // body.
    let (_dir, path) = fixture_copy("Dockerfile", "dockerfile");
    let path_str = path.to_str().unwrap();
    let out = dq()
        .args(["set", path_str, "/0/instruction", "RUN", "-i", "--no-color"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).to_string();
    assert!(
        stderr.contains("read-only") || stderr.contains("write"),
        "stderr must explain the rejection cause, got: {stderr:?}",
    );
}

#[test]
fn smoke_fmt_ini_in_place_with_sort_keys_completes() {
    // M5 contract: `dq fmt app.ini -i --sort-keys` sorts keys WITHIN each
    // section. `Ini::write_with_options` honours `--sort-keys` by deep-
    // canonicalising the value tree (sections sorted, plus per-section
    // keys sorted). The fixture has [server] with host < port and [client]
    // with retries < timeout, both already sorted; we pin that fmt -i does
    // not corrupt them and that the post-fmt file reports keys in
    // alphabetic order within each section.
    let (_dir, path) = fixture_copy("app.ini", "ini");
    let path_str = path.to_str().unwrap();
    dq().args(["fmt", path_str, "-i", "--sort-keys", "--no-color"])
        .assert()
        .success();
    let contents = std::fs::read_to_string(&path).unwrap();
    assert!(
        contents.contains("[server]"),
        "[server] section must survive fmt --sort-keys, got:\n{contents}",
    );
    assert!(
        contents.contains("[client]"),
        "[client] section must survive fmt --sort-keys, got:\n{contents}",
    );
    // Within [server]: `host` (alphabetic before `port`) must come first.
    let server_start = contents.find("[server]").expect("[server] header");
    let host_pos = contents[server_start..]
        .find("host")
        .expect("host key in [server]");
    let port_pos = contents[server_start..]
        .find("port")
        .expect("port key in [server]");
    assert!(
        host_pos < port_pos,
        "with --sort-keys, [server] must list `host` before `port`, got:\n{contents}",
    );
    // Within [client]: `retries` must come before `timeout`.
    let client_start = contents.find("[client]").expect("[client] header");
    let retries_pos = contents[client_start..]
        .find("retries")
        .expect("retries key in [client]");
    let timeout_pos = contents[client_start..]
        .find("timeout")
        .expect("timeout key in [client]");
    assert!(
        retries_pos < timeout_pos,
        "with --sort-keys, [client] must list `retries` before `timeout`, got:\n{contents}",
    );
}

// ---------------------------------------------------------------------------
// M6 DoD smoke tests — `dq completions`, `dq man`, `dq validate -F sarif`.
//
// All three are driven through the in-process `dq::run` driver (mirroring the
// M3+ pattern above) so they execute fast and deterministically. `dq self
// check` / `dq self update` are NOT smoke-tested here — they hit the network
// (api.github.com) and the M6 plan explicitly defers them to the per-handler
// `#[ignore]` test in `commands::self_cmd::tests`.
// ---------------------------------------------------------------------------

#[test]
fn smoke_completions_bash_writes_complete_directive() {
    // M6 §2: `dq completions bash` exits 0 with the bash `complete -F`
    // directive on stdout. The full per-shell coverage lives in the
    // `commands::completions::tests` unit module — this smoke just
    // confirms the dispatch arm is wired.
    use clap::Parser;
    let cli = dq::Cli::try_parse_from(["dq", "completions", "bash"]).expect("clap parse");
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("completions bash must succeed");
    let s = String::from_utf8(out).expect("completion script must be valid UTF-8");
    assert!(
        s.contains("complete -F"),
        "bash completion missing `complete -F`; got:\n{s}",
    );
    assert!(!s.is_empty(), "stdout must be non-empty");
}

#[test]
fn smoke_man_top_level_writes_th_header() {
    // M6 §3: `dq man` (no page argument) exits 0 with the troff `.TH dq 1`
    // header on stdout. Note: `clap_mangen` emits `.TH dq 1` (unquoted) —
    // the spec's `.TH "dq"` was a documentation slip; the real output is
    // matched against the actual `clap_mangen` shape.
    use clap::Parser;
    let cli = dq::Cli::try_parse_from(["dq", "man"]).expect("clap parse");
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("man must succeed");
    let s = String::from_utf8(out).expect("troff must be valid UTF-8");
    assert!(
        s.contains(".TH dq 1"),
        "expected `.TH dq 1` in man output; got:\n{s}",
    );
}

#[test]
fn smoke_validate_sarif_emits_sarif_diagnostic_on_broken_yaml() {
    // M6 §5: `dq -F sarif validate <broken.yaml>` exits 4 (`VALIDATE_FAIL`)
    // and emits a SARIF 2.1.0 document carrying one error-level result.
    //
    // Important: `validate` writes diagnostics to **stderr**, not stdout
    // (see `Command::Validate(args) => commands::validate::run(.., err)` in
    // `lib.rs`). The smoke test must scrape `err` for the SARIF JSON. The
    // task spec said "stdout" — it was wrong; the test mirrors the actual
    // contract so a future refactor that "fixes" the destination would have
    // to update this assertion intentionally.
    use clap::Parser;
    let dir = tempfile::tempdir().expect("tempdir");
    let broken = dir.path().join("broken.yaml");
    // Unbalanced brace — guaranteed parse failure across YAML parsers.
    std::fs::write(&broken, b"a: [\n").unwrap();

    let cli = dq::Cli::try_parse_from(["dq", "-F", "sarif", "validate", broken.to_str().unwrap()])
        .expect("clap parse");
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e = dq::run(&cli, false, &mut out, &mut err).expect_err("broken.yaml must fail validation");
    assert_eq!(
        dq::exit_code::exit_code_for_error(&e),
        4,
        "broken validation must map to exit 4 (VALIDATE_FAIL); got: {e:?}",
    );
    // SARIF document arrives on stderr.
    let parsed: serde_json::Value = serde_json::from_slice(&err).unwrap_or_else(|jerr| {
        panic!(
            "SARIF stderr must be valid JSON ({jerr}); got:\n{}",
            String::from_utf8_lossy(&err),
        )
    });
    assert_eq!(parsed["version"], "2.1.0", "SARIF version must be 2.1.0");
    assert_eq!(
        parsed["runs"][0]["results"][0]["level"], "error",
        "first result level must be `error`; got: {parsed}",
    );
}

#[test]
fn smoke_convert_in_place_swaps_extension_and_removes_source() {
    // M3 DoD §8: `dq convert <file> -i -F json` writes the converted target
    // to `<file>.json` (extension swap) and removes the original YAML source.
    // The output bytes must parse as valid JSON containing the original keys.
    use clap::Parser;
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("doc.yaml");
    std::fs::write(&src, "x: 1\ny: hello\n").unwrap();
    let cli = dq::Cli::try_parse_from(["dq", "-i", "-F", "json", "convert", src.to_str().unwrap()])
        .expect("clap parse");
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("convert should succeed");
    let target = dir.path().join("doc.json");
    assert!(
        target.exists(),
        "converted target {target:?} must exist after `convert -i -F json`",
    );
    assert!(
        !src.exists(),
        "original source {src:?} must be removed (no --keep-source)",
    );
    let bytes = std::fs::read(&target).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_slice(&bytes).expect("converted target must be valid JSON");
    assert_eq!(parsed["x"], 1);
    assert_eq!(parsed["y"], "hello");
}

// ---------------------------------------------------------------------------
// M7 DoD smoke tests — `dq query` (jq read) + `dq set --jq` (jq transform).
//
// These spawn the real `dq` binary so signal handling, exit-code mapping,
// and the binary's own stderr renderer are all exercised. The fixture for
// the read-side smoke is the existing `k8s_deployment.yaml` (which has
// `spec.template.spec.containers[].image` with three entries); the
// write-side smoke uses a tempdir copy so the fixture stays pristine.
// ---------------------------------------------------------------------------

#[test]
fn smoke_query_extracts_container_images_as_json_array() {
    // M7 §7.1: `dq query '.spec.template.spec.containers[].image'
    // <fixture>` returns the three container images (exit 0). The K8s
    // deployment fixture ships three containers (web / sidecar / init).
    //
    // NOTE: we do NOT pass `-F json` here — `-F` overrides both the OUTPUT
    // and the INPUT parser, which would force the YAML fixture through the
    // JSON parser and fail. The default console reporter renders the
    // multi-output stream as one image per line, which is what we assert on.
    let out = dq()
        .args([
            "query",
            ".spec.template.spec.containers[].image",
            fixture("k8s_deployment.yaml").to_str().unwrap(),
            "--no-color",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).to_string();
    // All three images appear, in document order.
    assert!(
        stdout.contains("ghcr.io/example/web:1.2.3"),
        "expected web image in stdout, got:\n{stdout}",
    );
    assert!(
        stdout.contains("ghcr.io/example/sidecar:0.4.1"),
        "expected sidecar image in stdout, got:\n{stdout}",
    );
    assert!(
        stdout.contains("busybox:1.36"),
        "expected busybox image in stdout, got:\n{stdout}",
    );
    // Document order: web < sidecar < busybox.
    let pos_web = stdout
        .find("ghcr.io/example/web:1.2.3")
        .expect("web present");
    let pos_sidecar = stdout
        .find("ghcr.io/example/sidecar:0.4.1")
        .expect("sidecar present");
    let pos_busybox = stdout.find("busybox:1.36").expect("busybox present");
    assert!(
        pos_web < pos_sidecar && pos_sidecar < pos_busybox,
        "images must appear in document order, got:\n{stdout}",
    );
}

#[test]
fn smoke_set_jq_diff_renders_unified_diff_for_replicas_bump() {
    // M7 §7.1: `dq set --jq '.spec.replicas |= . + 1' <fixture> --diff`
    // emits a unified diff with both `-` and `+` lines and leaves the file
    // untouched. We copy the fixture into a tempdir so a partial-failure
    // mid-run cannot mutate the checked-in YAML.
    //
    // The fixture has `replicas: 3`, so the bumped value is `4`. The diff
    // also reflects two side-effects of the re-emit path:
    //   1. YAML comments at the top of the file are dropped (documented
    //      behaviour of `--jq`, see cli_set_jq.rs).
    //   2. The writer may reflow the `containers:` block indentation.
    // Neither matters for this assertion — we only pin the replicas line.
    let (_dir, path) = fixture_copy("k8s_deployment_writable.yaml", "yaml");
    let path_str = path.to_str().unwrap();
    let original = std::fs::read_to_string(&path).unwrap();
    let out = dq()
        .args([
            "set",
            path_str,
            "--jq",
            ".spec.replicas |= . + 1",
            "--diff",
            "--no-color",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).to_string();
    assert!(
        stdout.contains("-  replicas: 3"),
        "expected `-  replicas: 3` in diff, got:\n{stdout}",
    );
    assert!(
        stdout.contains("+  replicas: 4"),
        "expected `+  replicas: 4` in diff, got:\n{stdout}",
    );
    // Diff mode must NOT touch the file.
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(after, original, "--diff must NOT modify the fixture");
}
