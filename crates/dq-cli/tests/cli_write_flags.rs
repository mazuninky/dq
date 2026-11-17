//! Write-flag rejection tests.
//!
//! The write flags `-i/--in-place`, `--diff`, and `--backup` parse globally
//! (so the `set` / `del` subcommands can consume them) but every read
//! subcommand calls `Cli::ensure_no_write_flags` on its first line and
//! rejects the flag with a structured error naming the offending flag and
//! pointing the user at the read-only nature of the subcommand (with `set` /
//! `del` as the verbs that do accept the flag). Per spec, this is a
//! caller-side input error so each rejection MUST exit 6 (`INVALID_INPUT`)
//! with stderr stating that the flag is not accepted by the read-only
//! subcommand.

use std::path::PathBuf;

use assert_cmd::Command;
use predicates::prelude::*;

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

#[test]
fn in_place_short_flag_rejected_on_read_only_subcommand() {
    // `-i` short form must be rejected.
    dq().args([
        "-i",
        "get",
        fixture("server_config.yaml").to_str().unwrap(),
        "/x",
        "--no-color",
    ])
    .assert()
    .code(6)
    .stderr(predicate::str::contains("--in-place"))
    .stderr(predicate::str::contains("read-only"));
}

#[test]
fn in_place_long_flag_rejected_on_read_only_subcommand() {
    // `--in-place` long form is the same global flag — rejection mirrors `-i`.
    dq().args([
        "--in-place",
        "get",
        fixture("server_config.yaml").to_str().unwrap(),
        "/x",
        "--no-color",
    ])
    .assert()
    .code(6)
    .stderr(predicate::str::contains("--in-place"))
    .stderr(predicate::str::contains("read-only"));
}

#[test]
fn diff_flag_rejected_on_read_only_subcommand() {
    dq().args([
        "--diff",
        "get",
        fixture("server_config.yaml").to_str().unwrap(),
        "/x",
        "--no-color",
    ])
    .assert()
    .code(6)
    .stderr(predicate::str::contains("--diff"))
    .stderr(predicate::str::contains("read-only"));
}

#[test]
fn backup_flag_rejected_on_read_only_subcommand() {
    dq().args([
        "--backup",
        "get",
        fixture("server_config.yaml").to_str().unwrap(),
        "/x",
        "--no-color",
    ])
    .assert()
    .code(6)
    .stderr(predicate::str::contains("--backup"))
    .stderr(predicate::str::contains("read-only"));
}

// ---------------------------------------------------------------------------
// M4 §6.3: `--sort-keys` is read-tolerant — it must be accepted by every
// read command (silently ignored, since reads do not emit) and by every
// write command (no-op for textual-edit splice paths like `set`/`del`,
// honoured by `fmt`/`convert -i`). These tests pin both directions.
// ---------------------------------------------------------------------------

#[test]
fn read_command_accepts_sort_keys_as_no_op() {
    // M4 §3: `dq get config.yaml /a --sort-keys` exits 0 because reads do
    // not write — `--sort-keys` is a re-emit knob and a no-op here. This
    // test guards against accidentally adding it to the offenders list in
    // `Cli::ensure_no_write_flags`.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.yaml");
    std::fs::write(&path, b"a: 1\nb: 2\n").unwrap();
    dq().args([
        "--sort-keys",
        "get",
        path.to_str().unwrap(),
        "/a",
        "--no-color",
    ])
    .assert()
    .success()
    .stdout(predicate::str::contains("1"));
}

#[test]
fn set_command_accepts_sort_keys_as_no_op_for_splice() {
    // M4 design D5: `dq set f.yaml /x 1 --sort-keys -i` accepts the flag
    // but does NOT reorder existing sibling keys — the textual-edit splice
    // path preserves byte order; reordering would defeat the M2 round-trip
    // contract. We seed a file whose keys are intentionally out of order
    // (`z, m, a`) and verify `set` only updates `/m` while `z` still
    // appears before `a` byte-wise.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("f.yaml");
    std::fs::write(&path, b"z: 1\nm: 0\na: 2\n").unwrap();
    let path_str = path.to_str().unwrap();
    dq().args([
        "--sort-keys",
        "set",
        path_str,
        "/m",
        "5",
        "-i",
        "--no-color",
    ])
    .assert()
    .success();
    let contents = std::fs::read_to_string(&path).unwrap();
    // `m` was updated to 5 (the splice did happen).
    assert!(
        contents.contains("m: 5"),
        "splice must apply the new value, got:\n{contents}",
    );
    // Sibling order MUST NOT have been re-sorted alphabetically — `z`
    // still precedes `a`. If `--sort-keys` accidentally triggered a
    // re-emit through `Format::write_with_options`, `a` would now come
    // first.
    let pos_z = contents.find("z:").expect("`z:` must remain in the file");
    let pos_a = contents.find("a:").expect("`a:` must remain in the file");
    assert!(
        pos_z < pos_a,
        "splice path must preserve original key order; alphabetic re-sort is a regression. Got:\n{contents}",
    );
}

#[test]
fn multiple_write_flags_listed_together_in_rejection() {
    // When the user passes more than one write flag at once, the rejection
    // message should name all of them (joined by `,`) so they don't have to
    // run the command multiple times to discover which flags are reserved.
    let out = dq()
        .args([
            "-i",
            "--diff",
            "--backup",
            "get",
            fixture("server_config.yaml").to_str().unwrap(),
            "/x",
            "--no-color",
        ])
        .assert()
        .code(6);
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).to_string();
    assert!(stderr.contains("--in-place"));
    assert!(stderr.contains("--diff"));
    assert!(stderr.contains("--backup"));
    assert!(stderr.contains("read-only"));
}
