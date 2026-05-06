//! Snapshot tests for the structured rendering of Path / Parse errors.
//!
//! These tests spawn the real binary and snapshot stderr (where structured
//! errors land). We snapshot 8 cases: 4 with `-F json` (validate path produces
//! a JSON object; other commands produce console-text with the same fields)
//! and 4 with the default console formatter under `--no-color`.
//!
//! Snapshot redactions normalize the absolute fixture path so snapshots are
//! reproducible across machines / tempdirs.

use std::path::PathBuf;

use assert_cmd::Command;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn dq() -> Command {
    let mut cmd = Command::cargo_bin("dq").expect("dq binary built");
    cmd.env_clear();
    if let Ok(p) = std::env::var("PATH") {
        cmd.env("PATH", p);
    }
    cmd.env("HOME", "/tmp");
    cmd
}

/// Replace the absolute fixture path with `<FIXTURE>` so snapshots stay
/// portable across machines.
fn normalize_path(s: &str, original: &str) -> String {
    s.replace(original, "<FIXTURE>")
}

#[test]
fn snapshot_json_render_validate_malformed_json() {
    // Case 1 (JSON-render): validate emits a structured `{"kind": "parse",
    // "file": ..., "line": ..., "col": ..., "message": ..., "snippet": ...}`
    // object to stderr under `-F json`.
    let f = fixture("broken.json");
    let f_str = f.to_str().unwrap();
    let out = dq()
        .args(["-F", "json", "validate", f_str, "--no-color"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4), "validate must exit 4");
    let stderr = normalize_path(&String::from_utf8_lossy(&out.stderr), f_str);
    insta::with_settings!({
        filters => vec![
            // serde_json's parse-error wording shifts across versions.
            (r#""message": ".*?""#, r#""message": "[serde_json message]""#),
            // The exact column may shift if the parser implementation changes.
            (r#""col": \d+"#, r#""col": "[NUM]""#),
            (r#""line": \d+"#, r#""line": "[NUM]""#),
        ],
    }, {
        insta::assert_snapshot!("json_validate_malformed", stderr);
    });
}

#[test]
fn snapshot_console_render_validate_malformed_json() {
    // Case 2 (console-render): validate without `-F json` writes the same
    // structured error through the *console* reporter — still legible, with
    // `: line:col` style location.
    let f = fixture("broken.json");
    let f_str = f.to_str().unwrap();
    let out = dq()
        .args(["validate", f_str, "--no-color"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4));
    let stderr = normalize_path(&String::from_utf8_lossy(&out.stderr), f_str);
    insta::with_settings!({
        filters => vec![
            // Underlying serde_json message wording / position varies across
            // versions; redact the human-text portion so the snapshot is
            // resilient. The `kind: parse`, `file:`, `snippet:` fields are
            // the load-bearing structure.
            (r"line: \d+", "line: [NUM]"),
            (r"col: \d+", "col: [NUM]"),
            (r"message: .*", "message: [serde_json message]"),
        ],
    }, {
        insta::assert_snapshot!("console_validate_malformed", stderr);
    });
}

#[test]
fn snapshot_console_render_missing_pointer_with_did_you_mean() {
    // Case 3 (console-render): missing pointer produces `did_you_mean` and
    // matched_prefix on separate lines via `dq::render_error`.
    let f = fixture("server_config.yaml");
    let f_str = f.to_str().unwrap();
    let out = dq()
        .args(["get", f_str, "/server/prot", "--no-color"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    insta::assert_snapshot!("console_missing_pointer_did_you_mean", stderr);
}

#[test]
fn snapshot_console_render_array_out_of_bounds() {
    // Case 4 (console-render): out-of-bounds index against a 3-element array
    // (`/users/5`). Renderer surfaces matched_prefix `/users` and reports the
    // pointer.
    let f = fixture("server_config.yaml");
    let f_str = f.to_str().unwrap();
    let out = dq()
        .args(["get", f_str, "/users/5", "--no-color"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    insta::assert_snapshot!("console_array_out_of_bounds", stderr);
}

#[test]
fn snapshot_console_render_descend_into_scalar() {
    // Case 5 (console-render): TypeMismatch — descending into a scalar via
    // `/server/port/version`. The error reports the type mismatch.
    let f = fixture("server_config.yaml");
    let f_str = f.to_str().unwrap();
    let out = dq()
        .args(["get", f_str, "/server/port/version", "--no-color"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    insta::assert_snapshot!("console_descend_into_scalar", stderr);
}

#[test]
fn snapshot_console_render_in_place_flag_rejection() {
    // Case 6 (console-render): write-flag rejection (`-i get`). Surface the
    // structured read-only rejection message. `Cli::ensure_no_write_flags`
    // raises an `InvalidInput` domain error so the exit code is INVALID_INPUT
    // (6), not GENERIC (1).
    let f = fixture("server_config.yaml");
    let f_str = f.to_str().unwrap();
    let out = dq()
        .args(["-i", "get", f_str, "/x", "--no-color"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(6),
        "in-place rejection must map to INVALID_INPUT (6), got {:?} stderr={:?}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    insta::assert_snapshot!("console_in_place_rejection", stderr);
}

#[test]
fn snapshot_json_render_path_error_via_get() {
    // Case 7 (JSON-render): path error under `-F json` against a JSON
    // fixture (so the input parser is also JSON, not YAML). Today the binary
    // routes path errors through `render_error` which uses console-text
    // formatting regardless of `-F json` — the snapshot captures *current*
    // behaviour so any change is a deliberate spec evolution. The important
    // contract this snapshot pins is that the user still sees the structured
    // fields (matched_prefix, did_you_mean, pointer) even under `-F json`.
    let f = fixture("server_config.json");
    let f_str = f.to_str().unwrap();
    let out = dq()
        .args(["-F", "json", "get", f_str, "/server/prot", "--no-color"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "expected NOT_FOUND (2), got {:?} stderr={:?}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = normalize_path(&String::from_utf8_lossy(&out.stderr), f_str);
    insta::assert_snapshot!("json_path_typo_get", stderr);
}

#[test]
fn snapshot_templated_file_console_render() {
    // Case 9 (M2 §12.4): templated file without escape-hatch flags. A
    // helm-style fixture with `{{ ... }}` blocks must produce a structured
    // `TemplatedFile` error with stable text under `--no-color`.
    // The marker line/column is parser-positional; the snapshot pins the
    // shape of the message ("templated file" / "templates" / file path)
    // while filtering the line:col so a future template-detector tweak
    // doesn't churn the snapshot.
    let f = fixture("helm_values_templated.yaml");
    let f_str = f.to_str().unwrap();
    // Default mode (no `-i`, no `--diff`) — the template guard runs
    // unconditionally before any output-mode dispatch, so this exercises
    // the same TemplatedFile path without needing a tempfile.
    let out = dq()
        .args(["set", f_str, "/image/tag", "v2", "--no-color"])
        .output()
        .unwrap();
    // Exit code is 3 (PARSE_ERROR) per the exit-code mapping for
    // `Error::TemplatedFile`.
    assert_eq!(
        out.status.code(),
        Some(3),
        "templated file rejection must exit 3 (PARSE_ERROR), got {:?} stderr={:?}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = normalize_path(&String::from_utf8_lossy(&out.stderr), f_str);
    insta::with_settings!({
        filters => vec![
            // Position of the first detected template block depends on
            // fixture layout — scrub line/col so cosmetic edits don't
            // churn the snapshot.
            (r"line \d+", "line [NUM]"),
            (r"col \d+", "col [NUM]"),
            (r":\d+:\d+", ":[NUM]:[NUM]"),
        ],
    }, {
        insta::assert_snapshot!("console_templated_file", stderr);
    });
}

#[test]
fn snapshot_templated_file_json_render() {
    // Case 10 (M2 §12.4): same as above but under `-F json`. Today the
    // binary routes domain errors through `render_error` which emits
    // console-text regardless of `-F json` — the snapshot pins that
    // *current* behaviour. The contract this test pins is "the user still
    // sees the structured fields when they ask for JSON output, even if
    // the renderer hasn't been switched to a structured stderr writer
    // yet". A future M3 change that emits a JSON object on stderr would
    // be a deliberate spec evolution, requiring an `insta review`.
    let f = fixture("helm_values_templated.yaml");
    let f_str = f.to_str().unwrap();
    let out = dq()
        .args(["-F", "json", "set", f_str, "/image/tag", "v2", "--no-color"])
        .output()
        .unwrap();
    let code = out.status.code();
    let stderr = normalize_path(&String::from_utf8_lossy(&out.stderr), f_str);
    let stdout = normalize_path(&String::from_utf8_lossy(&out.stdout), f_str);
    insta::with_settings!({
        filters => vec![
            (r"line \d+", "line [NUM]"),
            (r"col \d+", "col [NUM]"),
            (r":\d+:\d+", ":[NUM]:[NUM]"),
        ],
    }, {
        insta::assert_snapshot!(
            "json_templated_file",
            format!(
                "exit_code: {}\nstdout:\n{}stderr:\n{}",
                code.unwrap_or(-1),
                stdout,
                stderr,
            ),
        );
    });
}

// ---------------------------------------------------------------------------
// M4 §6.2: snapshot the renderer outputs for `dq fmt --sort-keys` and the
// `dq fmt --diff` per-file marker pattern.
//
// These tests drive the CLI in-process via `dq::run` (not `assert_cmd`)
// because we want to capture the *exact* writer output without subprocess
// stderr noise. The fixtures are seeded in tempdirs so the snapshots stay
// deterministic regardless of the developer's machine — but we apply path
// filters so absolute tempdir paths don't leak into the snapshot.
// ---------------------------------------------------------------------------

#[test]
fn snapshot_fmt_sort_keys_yaml_output() {
    // M4 §6.2: snapshot the rendered YAML for `dq fmt --sort-keys`. The
    // input has unsorted keys at two depths; the output must show them
    // alphabetically reordered. Capture stdout (not stderr — fmt writes the
    // re-emitted bytes to stdout in default mode).
    use clap::Parser;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("input.yaml");
    std::fs::write(&path, b"z: 1\na:\n  y: 2\n  b: 3\nm: 4\n").unwrap();
    let cli = dq::Cli::try_parse_from(["dq", "--sort-keys", "fmt", path.to_str().unwrap()])
        .expect("clap parse");
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("fmt --sort-keys must succeed");
    let stdout = String::from_utf8(out).expect("stdout must be UTF-8");
    insta::assert_snapshot!("fmt_sort_keys_yaml_output", stdout);
}

#[test]
fn snapshot_fmt_diff_marker_format() {
    // M4 §6.2: snapshot the `=== <path> ===` per-file marker pattern that
    // bulk `--diff` mode emits. Two non-canonical YAML files in a tempdir
    // → the diff body for each is preceded by the marker. We filter the
    // tempdir path components so the snapshot stays reproducible.
    use clap::Parser;
    let dir = tempfile::tempdir().expect("tempdir");
    let p1 = dir.path().join("alpha.yaml");
    let p2 = dir.path().join("bravo.yaml");
    // Non-canonical: missing trailing newline → writer adds one.
    std::fs::write(&p1, b"a: 1\nb: 2").unwrap();
    std::fs::write(&p2, b"x: 1\ny: 2").unwrap();
    let glob = format!("{}/*.yaml", dir.path().to_str().unwrap());
    let cli = dq::Cli::try_parse_from(["dq", "--diff", "fmt", &glob]).expect("clap parse");
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("fmt --diff bulk must succeed");
    let stdout = String::from_utf8(out).expect("stdout must be UTF-8");

    // Replace the random tempdir prefix so the snapshot is stable across
    // runs. We anchor on `<TMP>/alpha.yaml` and `<TMP>/bravo.yaml`.
    let tmp_str = dir.path().to_str().unwrap();
    let normalized = stdout.replace(tmp_str, "<TMP>");
    insta::assert_snapshot!("fmt_diff_marker_format", normalized);
}

#[test]
fn snapshot_json_render_unsupported_format_via_get() {
    // Case 8 (JSON-render-ish): file extension that does not match any parser
    // produces `UnsupportedFormat`, exit code 6 (INVALID_INPUT). The
    // `--no-color` flag means stderr is plain text we can snapshot.
    let mut tmp = tempfile::NamedTempFile::with_suffix(".unknown").unwrap();
    use std::io::Write as _;
    tmp.write_all(b"hello\n").unwrap();
    // Close the underlying handle so the spawned binary can open the path
    // on Windows ("Access is denied. (os error 5)"). The `TempPath` keeps
    // the file alive for the test's lifetime and removes it on drop.
    let tmp = tmp.into_temp_path();
    let f_str = tmp.to_str().unwrap();
    let out = dq()
        .args(["-F", "json", "get", f_str, "/x", "--no-color"])
        .output()
        .unwrap();
    // Exit 3 (PARSE_ERROR) because the user provided `-F json` — the override
    // is honoured, so the parser tries to read the `.unknown` file as JSON,
    // which fails inside the parse layer and maps to PARSE_ERROR (3) rather
    // than the GENERIC (1) exit reserved for catch-all failures.
    let stderr = normalize_path(&String::from_utf8_lossy(&out.stderr), f_str);
    let code = out.status.code();
    insta::with_settings!({
        filters => vec![
            // Underlying parser message can change across versions.
            (r"expected value at line \d+ column \d+", "[serde_json message]"),
            (r"EOF while parsing.*", "[serde_json message]"),
            // Newer dq-core JSON parser produces a structured "expected JSON
            // value (object, array, scalar)" diagnostic — scrub it the same way
            // so the snapshot stays parser-version-agnostic.
            (
                r"expected JSON value \(object, array, scalar\)",
                "[serde_json message]",
            ),
        ],
    }, {
        insta::assert_snapshot!(
            "json_unsupported_or_parse_error",
            format!("exit_code: {}\nstderr:\n{}", code.unwrap_or(-1), stderr),
        );
    });
}
