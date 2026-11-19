//! NaN, infinities and big numbers on the read surfaces and in JSON Schema
//! rules, and `keys` / `values` / `len` on a node of another type.
//!
//! `get`, `values`, `select` and `query` print a NaN or an infinity one way
//! per output format, whatever format it was read from: `.nan` / `.inf` /
//! `-.inf` on the console, in YAML and in TOON, `nan` / `inf` in TOML. A
//! JSON Schema rule reads them as numbers.

use assert_cmd::Command;
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

const NAN_YAML: &str = "x: .nan\ny: [1, .inf]\nz: -.inf\n";
const NAN_TOML: &str = "x = nan\ny = [1, inf]\nz = -inf\n";
const SHAPES_YAML: &str = "a: [1, 2]\ns: hi\nn: 3\nz: null\n";

/// A temp dir holding `files`.
fn dir_with(files: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    for (name, src) in files {
        std::fs::write(dir.path().join(name), src).expect("write fixture");
    }
    dir
}

/// `(exit code, stdout, stderr)` of `dq --no-color <args>` in `dir`.
fn run(dir: &TempDir, args: &[&str]) -> (i32, String, String) {
    let out = dq()
        .current_dir(dir.path())
        .arg("--no-color")
        .args(args)
        .output()
        .expect("dq runs");
    (
        out.status.code().expect("exit code"),
        String::from_utf8(out.stdout).expect("UTF-8 stdout"),
        String::from_utf8(out.stderr).expect("UTF-8 stderr"),
    )
}

/// Stdout of a `dq --no-color <args>` that must succeed.
fn stdout_of(dir: &TempDir, args: &[&str]) -> String {
    let (code, stdout, stderr) = run(dir, args);
    assert_eq!(code, 0, "{args:?}: {stderr}");
    stdout
}

#[test]
fn values_and_select_write_a_yaml_nan_as_a_yaml_float() {
    let dir = dir_with(&[("n.yaml", NAN_YAML)]);
    assert_eq!(
        stdout_of(&dir, &["values", "n.yaml", "", "-F", "yaml"]),
        "- .nan\n- - 1\n  - .inf\n- -.inf\n"
    );
    assert_eq!(
        stdout_of(&dir, &["select", "n.yaml", "$.x", "-F", "yaml"]),
        "- .nan\n"
    );
}

#[test]
fn console_and_toon_spell_a_toml_nan_as_yaml_does() {
    let dir = dir_with(&[("n.toml", NAN_TOML)]);
    assert_eq!(
        stdout_of(&dir, &["values", "n.toml", ""]),
        ".nan\n[1, .inf]\n-.inf\n"
    );
    assert_eq!(
        stdout_of(&dir, &["select", "n.toml", "$.*"]),
        ".nan\n[1, .inf]\n-.inf\n"
    );
    let toon = stdout_of(&dir, &["get", "n.toml", "", "-F", "toon"]);
    assert!(toon.starts_with("x: .nan\ny[2]: 1,.inf\n"), "{toon}");
}

#[test]
fn values_and_keys_of_a_sequence_list_its_items_and_indices() {
    let dir = dir_with(&[("s.yaml", SHAPES_YAML)]);
    assert_eq!(stdout_of(&dir, &["values", "s.yaml", "/a"]), "1\n2\n");
    assert_eq!(stdout_of(&dir, &["keys", "s.yaml", "/a"]), "0\n1\n");
}

#[test]
fn keys_values_and_len_of_a_scalar_are_a_wrong_type_error() {
    let dir = dir_with(&[("s.yaml", SHAPES_YAML)]);
    let cases = [
        (
            ["keys", "s.yaml", "/n"],
            "cannot list the keys of '/n': it is int, not an object or an array\n",
        ),
        (
            ["values", "s.yaml", "/s"],
            "cannot list the values of '/s': it is string, not an object or an array\n",
        ),
        (
            ["len", "s.yaml", "/z"],
            "cannot take the length of '/z': it is null, not an array, a string or an object\n",
        ),
    ];
    for (args, message) in cases {
        let (code, stdout, stderr) = run(&dir, &args);
        assert_eq!(
            (code, stdout.as_str(), stderr.as_str()),
            (1, "", message),
            "{args:?}"
        );
    }
}

/// `(exit code, stdout)` of `dq check` of `doc` against a rule whose
/// schema is `{properties: {x: <schema>}}`.
fn schema_check(schema: &str, doc: &str) -> (i32, String) {
    let rule = format!(
        "id: t.num\ndescription: x\nseverity: error\nmatch: {{ format: yaml }}\n\
         check:\n  schema: {{ properties: {{ x: {schema} }} }}\n"
    );
    let dir = dir_with(&[("rule.yml", &rule), ("doc.yaml", doc)]);
    let (code, stdout, _) = run(&dir, &["check", "--rule", "rule.yml", "doc.yaml"]);
    (code, stdout)
}

#[test]
fn schema_rules_read_non_finite_and_big_numbers_as_numbers() {
    for (schema, doc) in [
        ("{type: number}", "x: .nan\n"),
        ("{type: number}", "x: -.inf\n"),
        ("{minimum: 0}", "x: .inf\n"),
        ("{type: integer}", "x: 18446744073709551616\n"),
        ("{type: integer}", "x: 1e400\n"),
    ] {
        let (code, stdout) = schema_check(schema, doc);
        assert_eq!(code, 0, "{schema} on {doc:?}: {stdout}");
    }
    for (schema, doc, message) in [
        (
            "{type: string}",
            "x: .nan\n",
            r#".nan is not of type "string""#,
        ),
        (
            "{type: integer}",
            "x: .inf\n",
            r#".inf is not of type "integer""#,
        ),
        (
            "{maximum: 10}",
            "x: .inf\n",
            ".inf is greater than the maximum of 10",
        ),
        (
            "{minimum: 0}",
            "x: -.inf\n",
            "-.inf is less than the minimum of 0",
        ),
        (
            "{maximum: 10}",
            "x: .nan\n",
            ".nan has no order, so it is not at most the maximum of 10",
        ),
        (
            "{maximum: 10}",
            "x: 1e400\n",
            "1e+400 is greater than the maximum of 10",
        ),
    ] {
        let (code, stdout) = schema_check(schema, doc);
        assert_eq!(code, 4, "{schema} on {doc:?}: {stdout}");
        assert!(stdout.contains(message), "{schema} on {doc:?}: {stdout}");
    }
}

#[test]
fn schema_rules_hold_exact_and_non_finite_numbers_of_their_own() {
    for (schema, doc) in [
        ("{const: 18446744073709551616}", "x: 18446744073709551616\n"),
        (
            "{maximum: 18446744073709551616}",
            "x: 18446744073709551616\n",
        ),
        ("{maximum: 1e400}", "x: 1e399\n"),
        ("{exclusiveMaximum: 0.10000000000000000001}", "x: 0.1\n"),
        ("{enum: [.inf, 2]}", "x: .inf\n"),
    ] {
        let (code, stdout) = schema_check(schema, doc);
        assert_eq!(code, 0, "{schema} on {doc:?}: {stdout}");
    }
    for (schema, doc, message) in [
        (
            "{const: 18446744073709551616}",
            "x: 18446744073709551617\n",
            "/properties/x/const: 18446744073709551616 was expected",
        ),
        (
            "{maximum: 18446744073709551616}",
            "x: 18446744073709551617\n",
            "/properties/x/maximum: 18446744073709551617 is greater than the maximum of \
             18446744073709551616",
        ),
        (
            "{maximum: 1e400}",
            "x: .inf\n",
            ".inf is greater than the maximum of 1e+400",
        ),
        (
            "{const: 0.10000000000000000001}",
            "x: 0.1\n",
            "0.10000000000000000001 was expected",
        ),
        (
            "{enum: [.inf, 2]}",
            "x: -.inf\n",
            "-.inf is not one of .inf or 2",
        ),
    ] {
        let (code, stdout) = schema_check(schema, doc);
        assert_eq!(code, 4, "{schema} on {doc:?}: {stdout}");
        assert!(stdout.contains(message), "{schema} on {doc:?}: {stdout}");
    }
}

#[test]
fn an_exact_bound_behind_a_ref_names_the_ref_path() {
    let rule = "id: t.ref\ndescription: x\nseverity: error\nmatch: { format: yaml }\n\
                check:\n  schema:\n    $defs: { n: { maximum: 18446744073709551616 } }\n    \
                properties: { x: { $ref: '#/$defs/n' } }\n";
    let dir = dir_with(&[
        ("rule.yml", rule),
        ("doc.yaml", "x: 18446744073709551617\n"),
    ]);
    let (code, stdout, stderr) = run(&dir, &["check", "--rule", "rule.yml", "doc.yaml"]);
    assert_eq!(code, 4, "{stdout}{stderr}");
    assert!(
        stdout.contains("/properties/x/$ref/maximum: 18446744073709551617 is greater"),
        "{stdout}"
    );
}

#[test]
fn a_schema_file_holds_exact_numbers() {
    let rule = "id: t.file\ndescription: x\nseverity: error\nmatch: { format: yaml }\n\
                check:\n  schema_file: s.json\n";
    let schema = r##"{"$defs": {"n": {"maximum": 18446744073709551616}},
                      "properties": {"x": {"$ref": "#/$defs/n"}}}"##;
    let dir = dir_with(&[
        ("rule.yml", rule),
        ("s.json", schema),
        ("ok.yaml", "x: 18446744073709551616\n"),
        ("bad.yaml", "x: 18446744073709551617\n"),
    ]);
    let (code, stdout, stderr) = run(&dir, &["check", "--rule", "rule.yml", "ok.yaml"]);
    assert_eq!(code, 0, "{stdout}{stderr}");
    let (code, stdout, stderr) = run(&dir, &["check", "--rule", "rule.yml", "bad.yaml"]);
    assert_eq!(code, 4, "{stdout}{stderr}");
    assert!(
        stdout.contains(
            "/properties/x/$ref/maximum: 18446744073709551617 is greater than the maximum of \
             18446744073709551616"
        ),
        "{stdout}"
    );
}

#[test]
fn select_filters_compare_non_finite_and_big_numbers_as_numbers() {
    let doc = "- {n: .nan, k: nan}\n- {n: .inf, k: inf}\n- {n: -.inf, k: ninf}\n\
               - {n: 1, k: one}\n- {n: 18446744073709551616, k: big}\n- {n: '.nan', k: str}\n";
    let dir = dir_with(&[("d.yaml", doc)]);
    for (expr, expected) in [
        ("$[?@.n > 1].k", "- inf\n- big\n"),
        ("$[?@.n < 1].k", "- ninf\n"),
        ("$[?@.n == '.nan'].k", "- str\n"),
        ("$[?@.n > 1].n", "- .inf\n- 18446744073709551616\n"),
    ] {
        assert_eq!(
            stdout_of(&dir, &["select", "d.yaml", expr, "-F", "yaml"]),
            expected,
            "{expr}"
        );
    }
}
