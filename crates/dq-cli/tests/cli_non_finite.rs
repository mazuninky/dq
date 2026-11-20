//! NaN and infinity through the CLI: YAML spells them `.nan` / `.inf`,
//! TOML `nan` / `inf`, JSON has no token for them — a JSON output of one
//! is a generic failure (exit 1) with nothing on stdout. Inside jq they are
//! numbers.

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

/// A temp dir holding `files`; a name may hold a `/` (`.dq/rules/r.yml`).
fn dir_with(files: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    for (name, src) in files {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir fixture");
        std::fs::write(path, src).expect("write fixture");
    }
    dir
}

/// The content of `name` in `dir`.
fn read(dir: &TempDir, name: &str) -> String {
    std::fs::read_to_string(dir.path().join(name)).expect("read back")
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
        "x: .nan\n'y': -.inf\nl:\n- .inf\nm:\n  k: .nan\n"
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
    // The console spells them the YAML way whatever the source format.
    let toml = dir_with(&[("n.toml", "x = nan\ny = -inf\n")]);
    assert_eq!(stdout_of(&toml, &["get", "n.toml", "/x"]), ".nan\n");
    assert_eq!(
        stdout_of(&toml, &["get", "n.toml", ""]),
        "x: .nan\ny: -.inf\n"
    );
    // Inside jq a non-finite float is a number; YAML output writes it back
    // as a float.
    assert_eq!(
        stdout_of(
            &dir,
            &["query", "-F", "yaml", "[.x, .y, (.x | type)]", "n.yaml"]
        ),
        "- - .nan\n  - -.inf\n  - number\n"
    );
    // JSON has no token for it: JSON / JSONL output of a query result fails
    // as `convert -F json` does, with nothing on stdout.
    for format in ["json", "jsonl"] {
        dq().current_dir(dir.path())
            .args(["--no-color", "query", "-F", format, ".x", "n.yaml"])
            .assert()
            .code(1)
            .stdout("")
            .stderr(
                predicate::str::contains("non-finite float")
                    .and(predicate::str::contains("panicked").not()),
            );
    }
}

/// A YAML NaN and -infinity next to a finite key jq filters edit.
const NAN_YAML: &str = "x: .nan\ny: -.inf\nz: 1\n";

/// The same in TOML.
const NAN_TOML: &str = "x = nan\ny = -inf\nz = 1\n";

#[test]
fn query_sees_nan_and_infinity_as_jq_numbers() {
    // `s.yaml` holds the string `.nan`, which stays a string.
    let dir = dir_with(&[("x.yaml", NAN_YAML), ("s.yaml", "x: '.nan'\n")]);
    let query = |expression: &str, file: &str| stdout_of(&dir, &["query", expression, file]);
    assert_eq!(query(".x | type", "x.yaml"), "number\n");
    assert_eq!(query(".x | isnan", "x.yaml"), "true\n");
    assert_eq!(query(".y | isinfinite", "x.yaml"), "true\n");
    assert_eq!(query(".z | isnan", "x.yaml"), "false\n");
    assert_eq!(query(".x | type", "s.yaml"), "string\n");
    // Arithmetic and jq's own non-finite numbers run. The console output
    // spells a non-finite float the YAML way: `.nan`, `.inf`, `-.inf`;
    // in a collection a key YAML 1.1 reads as a bool (`y`) is quoted.
    assert_eq!(query(".x + 1", "x.yaml"), ".nan\n");
    assert_eq!(query("nan", "x.yaml"), ".nan\n");
    assert_eq!(query("[infinite, -infinite]", "x.yaml"), "[.inf, -.inf]\n");
    assert_eq!(query(".", "x.yaml"), "{x: .nan, 'y': -.inf, z: 1}\n");
    // YAML output writes them back as YAML floats.
    assert_eq!(
        stdout_of(&dir, &["query", "-F", "yaml", ".x", "x.yaml"]),
        "- .nan\n"
    );
    assert_eq!(
        stdout_of(&dir, &["query", "-F", "yaml", "[nan, -infinite]", "x.yaml"]),
        "- - .nan\n  - -.inf\n"
    );
}

#[test]
fn query_json_output_of_a_computed_nan_or_infinity_exits_1() {
    let dir = dir_with(&[("x.yaml", NAN_YAML)]);
    // A first result that is one: nothing precedes the failure.
    let first = [".x", ".y", "nan", "(-infinite)"];
    // JSONL of these: `query_jsonl_output_of_a_later_nan_prints_nothing`.
    let later = [".z, nan", "[1, infinite]", "{a: 1, b: .x}"];
    let cases = first
        .iter()
        .chain(&later)
        .map(|e| ("json", *e))
        .chain(first.iter().map(|e| ("jsonl", *e)));
    for (format, expression) in cases {
        dq().current_dir(dir.path())
            .args(["--no-color", "query", "-F", format, expression, "x.yaml"])
            .assert()
            .code(1)
            .stdout("")
            .stderr(
                predicate::str::contains("non-finite float")
                    .and(predicate::str::contains("panicked").not()),
            );
    }
    // A finite result from the same file prints.
    assert_eq!(
        stdout_of(&dir, &["query", "-F", "json", ".z", "x.yaml"]),
        "[\n  1\n]\n"
    );
    assert_eq!(
        stdout_of(&dir, &["query", "-F", "jsonl", ".x | isnan", "x.yaml"]),
        "true\n"
    );
}

#[test]
fn query_jsonl_output_of_a_later_nan_prints_nothing() {
    let dir = dir_with(&[("x.yaml", NAN_YAML)]);
    for expression in [".z, nan", "[1, infinite]", "{a: 1, b: .x}"] {
        dq().current_dir(dir.path())
            .args(["--no-color", "query", "-F", "jsonl", expression, "x.yaml"])
            .assert()
            .code(1)
            .stdout("")
            .stderr(predicate::str::contains("non-finite float"));
    }
}

#[test]
fn set_jq_in_place_keeps_untouched_nan_and_infinity_byte_for_byte() {
    let dir = dir_with(&[("x.yaml", NAN_YAML), ("x.toml", NAN_TOML)]);
    for file in ["x.yaml", "x.toml"] {
        dq().current_dir(dir.path())
            .args(["--no-color", "set", "--jq", ".z = 2", "-i", file])
            .assert()
            .success()
            .stdout("");
    }
    // The jq path re-emits the YAML file: the key `y` is quoted, a bool to a
    // YAML 1.1 reader.
    assert_eq!(read(&dir, "x.yaml"), "x: .nan\n'y': -.inf\nz: 2\n");
    assert_eq!(read(&dir, "x.toml"), "x = nan\ny = -inf\nz = 2\n");
}

#[test]
fn set_jq_writing_nan_into_json_exits_1_and_leaves_the_file() {
    const SRC: &str = "{\"y\": 1}\n";
    let dir = dir_with(&[("s.json", SRC)]);
    for expression in [".x = nan", ".x = [-infinite]"] {
        dq().current_dir(dir.path())
            .args(["--no-color", "set", "--jq", expression, "-i", "s.json"])
            .assert()
            .code(1)
            .stdout("")
            .stderr(
                predicate::str::contains("non-finite float")
                    .and(predicate::str::contains("panicked").not()),
            );
        assert_eq!(read(&dir, "s.json"), SRC, "{expression}");
    }
    // A finite value through the same path writes.
    dq().current_dir(dir.path())
        .args(["--no-color", "set", "--jq", ".x = 1.5", "-i", "s.json"])
        .assert()
        .success();
    assert_eq!(read(&dir, "s.json"), "{\n  \"y\": 1,\n  \"x\": 1.5\n}\n");
}

#[test]
fn set_jq_keeps_a_json_float_no_f64_holds_a_number() {
    // HEAD 427397b wrote `"e": {"$serde_json::private::Number": "1e+400"}`.
    let dir = dir_with(&[("big.json", "{\"e\": 1e400, \"i\": 1}\n")]);
    dq().current_dir(dir.path())
        .args(["--no-color", "set", "--jq", ".i = 4", "-i", "big.json"])
        .assert()
        .success()
        .stdout("");
    assert_eq!(
        read(&dir, "big.json"),
        "{\n  \"e\": 1e+400,\n  \"i\": 4\n}\n"
    );
    assert_eq!(stdout_of(&dir, &["type", "big.json", "/e"]), "float\n");
}

#[test]
fn bulk_set_jq_keeps_nan_in_every_file() {
    let dir = dir_with(&[
        ("g/a.yaml", "x: .nan\nz: 1\n"),
        ("g/b.yaml", "l: [-.inf, .nan]\nz: 1\n"),
    ]);
    dq().current_dir(dir.path())
        .args(["--no-color", "set", "--jq", ".z = 2", "-i", "g/*.yaml"])
        .assert()
        .success()
        .stdout("Modified: 2, Skipped: 0, Failed: 0\n");
    assert_eq!(read(&dir, "g/a.yaml"), "x: .nan\nz: 2\n");
    assert_eq!(read(&dir, "g/b.yaml"), "l:\n- -.inf\n- .nan\nz: 2\n");
}

#[test]
fn lint_user_rule_sees_a_yaml_nan_as_a_number() {
    const RULE: &str = r#"id: x-is-nan
description: x must not be NaN
severity: error
match:
  format: yaml
  filter: '.x | type == "number"'
check:
  jq: 'select(.x | isnan) | .'
  message: 'x is NaN'
"#;
    let dir = dir_with(&[
        (".dq/rules/x-is-nan.yml", RULE),
        ("x.yaml", NAN_YAML),
        // The string `.nan` fails the filter; a finite number the check.
        ("s.yaml", "x: '.nan'\n"),
        ("f.yaml", "x: 1.5\n"),
    ]);
    dq().current_dir(dir.path())
        .args(["--no-color", "-F", "json", "lint", "x.yaml"])
        .assert()
        .code(4)
        .stdout(
            r#"{
  "diagnostics": [
    {
      "path": "x.yaml",
      "line": 1,
      "col": 1,
      "message": "x is NaN",
      "severity": "error",
      "rule_id": "x-is-nan",
      "references": []
    }
  ]
}
"#,
        );
    for clean in ["s.yaml", "f.yaml"] {
        dq().current_dir(dir.path())
            .args(["--no-color", "-F", "json", "lint", clean])
            .assert()
            .success()
            .stdout("{\n  \"diagnostics\": []\n}\n");
    }
}

#[test]
fn fix_jq_keeps_nan_and_a_second_run_changes_nothing() {
    // The check fires on a fixed file too, so the second run evaluates the
    // fix and has to see that it changes nothing.
    const RULE: &str = r#"id: z-is-two
description: z must be 2 in a file whose x is NaN
severity: warn
match:
  format: [yaml, toml]
check:
  jq: 'select(.x | isnan) | .'
  message: 'x is NaN'
fix:
  jq: '.z = 2'
"#;
    let dir = dir_with(&[
        (".dq/rules/z-is-two.yml", RULE),
        ("x.yaml", NAN_YAML),
        ("x.toml", NAN_TOML),
    ]);
    let fix = |summary: &str| {
        dq().current_dir(dir.path())
            .args(["--no-color", "-i", "fix", "x.yaml", "x.toml"])
            .assert()
            .success()
            .stdout(summary.to_owned());
        (read(&dir, "x.yaml"), read(&dir, "x.toml"))
    };
    let fixed = (
        "x: .nan\n'y': -.inf\nz: 2\n".to_owned(),
        "x = nan\ny = -inf\nz = 2\n".to_owned(),
    );
    assert_eq!(fix("Modified: 2, Skipped: 0, Failed: 0\n"), fixed);
    assert_eq!(fix("Modified: 0, Skipped: 2, Failed: 0\n"), fixed);
}

#[test]
fn set_jq_leaves_finite_numbers_as_head_wrote_them() {
    // `b` is what HEAD 427397b wrote too: the jq path re-emits through the
    // YAML writer, which spells `1e3` as `1000.0`. `c`, an integer past
    // `i64`, stays a plain integer (`yaml_writer_keeps_a_big_integer_a_number`
    // below).
    let dir = dir_with(&[(
        "f.yaml",
        "a: 1.0\nb: 1e3\nc: 18446744073709551615\nd: 0.1\n",
    )]);
    dq().current_dir(dir.path())
        .args(["--no-color", "set", "--jq", ".e = 1", "-i", "f.yaml"])
        .assert()
        .success()
        .stdout("");
    assert_eq!(
        read(&dir, "f.yaml"),
        "a: 1.0\nb: 1000.0\nc: 18446744073709551615\nd: 0.1\ne: 1\n"
    );
}

#[test]
fn yaml_writer_keeps_a_big_integer_a_number() {
    let dir = dir_with(&[("f.yaml", "c: 18446744073709551615\n")]);
    assert_eq!(stdout_of(&dir, &["type", "f.yaml", "/c"]), "int\n");
    dq().current_dir(dir.path())
        .args(["--no-color", "set", "--jq", ".e = 1", "-i", "f.yaml"])
        .assert()
        .success();
    assert_eq!(stdout_of(&dir, &["type", "f.yaml", "/c"]), "int\n");
}

/// HCL has no literal for NaN or an infinity, so every path that writes
/// HCL refuses one with exit 1, naming it, and writes nothing. `set --jq`
/// used to write `a = null` and `convert -F hcl` the string `a = ".nan"`.
#[test]
fn hcl_output_of_nan_or_infinity_exits_1_and_writes_nothing() {
    const RULE: &str = r#"id: a-is-nan
description: a must be NaN
severity: warn
match:
  format: [hcl]
check:
  jq: 'select(.a | isnan | not) | .'
  message: 'a is not NaN'
fix:
  jq: '.a = nan'
"#;
    const TF: &str = "a = 1\n";
    let dir = dir_with(&[
        (".dq/rules/a-is-nan.yml", RULE),
        ("n.yaml", NON_FINITE_YAML),
        ("s.tf", TF),
    ]);
    let refused = |args: &[&str], spelling: &str| {
        dq().current_dir(dir.path())
            .arg("--no-color")
            .args(args)
            .assert()
            .code(1)
            .stdout("")
            .stderr(format!(
                "hcl format error: HCL cannot represent non-finite number {spelling}\n"
            ));
    };
    refused(&["-q", "-F", "hcl", "convert", "n.yaml"], ".nan");
    refused(&["-q", "-F", "hcl", "convert", "-i", "n.yaml"], ".nan");
    assert_eq!(read(&dir, "n.yaml"), NON_FINITE_YAML);
    assert!(!dir.path().join("n.hcl").exists(), "convert -i wrote n.hcl");
    refused(
        &["set", "--jq", ".a = nan | .b = infinite", "-i", "s.tf"],
        ".nan",
    );
    refused(&["set", "--jq", ".a = [1, -infinite]", "s.tf"], "-.inf");
    refused(&["set", "--jq", ".a = {k: infinite}", "-i", "s.tf"], ".inf");
    refused(&["-i", "fix", "s.tf"], ".nan");
    assert_eq!(read(&dir, "s.tf"), TF);
    // A finite float through the same path writes.
    dq().current_dir(dir.path())
        .args(["--no-color", "set", "--jq", ".a = 1.5", "-i", "s.tf"])
        .assert()
        .success();
    assert_eq!(read(&dir, "s.tf"), "a = 1.5\n");
}
