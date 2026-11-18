//! `dq set` / `dq del` / `dq lint` through the real binary, for YAML
//! textual-edit bugs that predate the saphyr-parser 0.3 bump.

use std::io::Write as _;

use assert_cmd::Command;
use tempfile::{NamedTempFile, TempPath};

/// Build a `dq` command with a clean environment (same discipline as
/// `cli_smoke.rs::dq`).
fn dq() -> Command {
    let mut cmd = Command::cargo_bin("dq").expect("dq binary built");
    cmd.env_clear();
    if let Ok(p) = std::env::var("PATH") {
        cmd.env("PATH", p);
    }
    cmd.env("HOME", "/tmp");
    cmd
}

/// Write `src` to a temp file named `*<suffix>`; the handle is closed
/// before the binary touches the path.
fn write_with_suffix(src: &str, suffix: &str) -> TempPath {
    let mut tmp = NamedTempFile::with_suffix(suffix).expect("tempfile");
    tmp.write_all(src.as_bytes()).expect("write tempfile");
    tmp.into_temp_path()
}

fn write_yaml(src: &str) -> TempPath {
    write_with_suffix(src, ".yaml")
}

#[test]
fn null_key_with_an_anchor_does_not_panic() {
    let file = write_yaml("f: {? &a}\n");
    dq().arg("lint")
        .arg(file.to_str().expect("UTF-8 path"))
        .arg("--no-color")
        .assert()
        .code(0);
    // No sibling span to anchor a new root key next to: a clean "not found".
    dq().arg("set")
        .arg(file.to_str().expect("UTF-8 path"))
        .args(["/g", "1", "--no-color"])
        .assert()
        .code(2);
}

#[test]
fn set_next_to_an_empty_key_does_not_panic_in_json_or_toml() {
    for (src, suffix, want) in [
        ("{\"\": 1, \"b\": 2}", ".json", "{\"\": 1, \"b\": 3}"),
        ("\"\" = 1\nb = 2\n", ".toml", "\"\" = 1\nb = 3\n"),
    ] {
        let file = write_with_suffix(src, suffix);
        let out = dq()
            .arg("set")
            .arg(file.to_str().expect("UTF-8 path"))
            .args(["/b", "3", "--no-color"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        assert_eq!(
            String::from_utf8(out).expect("UTF-8 stdout").trim_end(),
            want.trim_end()
        );
    }
}
