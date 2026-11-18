//! NaN and infinity through the CLI: YAML spells them `.nan` / `.inf`,
//! TOML `nan` / `inf`, JSON has no token for them — a JSON output of one
//! is a generic failure (exit 1) with nothing on stdout.

use assert_cmd::Command;
use predicates::prelude::*;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

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

/// NaN and infinities at the root, in a flow sequence and in a mapping.
const NON_FINITE_YAML: &str = "x: .nan\ny: -.inf\nl: [.Inf]\nm: {k: .NaN}\n";

/// A temp dir holding `files`.
fn dir_with(files: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    for (name, src) in files {
        std::fs::write(dir.path().join(name), src).expect("write fixture");
    }
    dir
}

/// Stdout of a successful `dq --no-color <args>` in `dir`.
fn stdout_of(dir: &TempDir, args: &[&str]) -> String {
    let out = dq()
        .current_dir(dir.path())
        .arg("--no-color")
        .args(args)
        .output()
        .expect("dq runs");
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("UTF-8 stdout")
}

#[test]
fn convert_to_json_of_nan_or_infinity_exits_1_with_nothing_on_stdout() {
    let dir = dir_with(&[
        ("n.yaml", NON_FINITE_YAML),
        ("n.toml", "a = nan\nb = -inf\n"),
    ]);
    for file in ["n.yaml", "n.toml"] {
        dq().current_dir(dir.path())
            .args(["--no-color", "-F", "json", "convert", file])
            .assert()
            .code(1)
            .stdout("")
            .stderr(
                predicate::str::contains("non-finite float")
                    .and(predicate::str::contains("panicked").not()),
            );
    }
    // A finite float converts.
    let dir = dir_with(&[("f.yaml", "x: 1.5\n")]);
    assert_eq!(
        stdout_of(&dir, &["-F", "json", "convert", "f.yaml"]),
        "{\n  \"x\": 1.5\n}\n"
    );
}

#[test]
fn convert_to_yaml_or_toml_keeps_nan_and_infinity_as_floats() {
    let dir = dir_with(&[
        ("n.yaml", NON_FINITE_YAML),
        ("n.toml", "a = nan\nb = -inf\n"),
    ]);
    assert_eq!(
        stdout_of(&dir, &["-F", "yaml", "convert", "n.yaml"]),
        "x: .nan\ny: -.inf\nl:\n- .inf\nm:\n  k: .nan\n"
    );
    assert_eq!(
        stdout_of(&dir, &["-F", "toml", "convert", "n.yaml"]),
        "x = nan\ny = -inf\nl = [inf]\n\n[m]\nk = nan\n"
    );
    assert_eq!(
        stdout_of(&dir, &["-F", "yaml", "convert", "n.toml"]),
        "a: .nan\nb: -.inf\n"
    );
}

#[test]
fn toml_set_from_a_yaml_value_file_writes_toml_nan_and_inf() {
    let cases: &[(&str, &str, &str, &str, &str)] = &[
        // A mapping holding NaN and a sequence holding -inf.
        (
            "v: .nan\nw: [-.inf, 1]\n",
            "j = 1\nk = 2\n",
            "/j",
            "j = { v = nan, w = [-inf, 1] }\nk = 2\n",
            "j:\n  v: .nan\n  w:\n  - -.inf\n  - 1\nk: 2\n",
        ),
        // Under a `[table]` header.
        (
            "v: .nan\n",
            "[t]\nj = 1\n",
            "/t/j",
            "[t]\nj = { v = nan }\n",
            "t:\n  j:\n    v: .nan\n",
        ),
        // A bare scalar, and a sequence into an array item.
        (".nan\n", "j = 1\n", "/j", "j = nan\n", "j: .nan\n"),
        (
            "[-.inf, .nan]\n",
            "k = [1]\n",
            "/k/0",
            "k = [[-inf, nan]]\n",
            "k:\n- - -.inf\n  - .nan\n",
        ),
    ];
    for (value, src, pointer, want, as_yaml) in cases {
        let dir = dir_with(&[("v.yaml", value), ("d.toml", src)]);
        dq().current_dir(dir.path())
            .args(["-i", "--no-color", "set", "d.toml", pointer])
            .args(["--value-from", "v.yaml"])
            .assert()
            .success()
            .stdout("");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("d.toml")).expect("read back"),
            *want,
            "set {pointer} from {value:?}"
        );
        // dq reads the file back with the floats as floats.
        assert_eq!(
            stdout_of(&dir, &["-F", "yaml", "convert", "d.toml"]),
            *as_yaml,
            "set {pointer} from {value:?}"
        );
    }
}

#[test]
fn get_and_query_print_nan_and_infinity() {
    let dir = dir_with(&[("n.yaml", NON_FINITE_YAML)]);
    assert_eq!(stdout_of(&dir, &["get", "n.yaml", "/x"]), ".nan\n");
    assert_eq!(stdout_of(&dir, &["get", "n.yaml", "/y"]), "-.inf\n");
    // jq hands a non-finite float back as its YAML text: JSON output of a
    // query result is the string, and exits 0.
    assert_eq!(
        stdout_of(&dir, &["query", "-F", "json", ".x", "n.yaml"]),
        "[\n  \".nan\"\n]\n"
    );
    assert_eq!(
        stdout_of(&dir, &["query", "-F", "jsonl", ".y", "n.yaml"]),
        "\"-.inf\"\n"
    );
}
