//! The read surfaces over NaN, infinities and big numbers, and `keys` /
//! `values` / `len` on a node of every other type.
//!
//! `cli_number_surfaces.rs` holds one sanity case per behaviour; this file
//! is the matrix around them: every scalar type for the wrong-type error,
//! every output format for `get` / `values` / `select` / `query`, and the
//! corners of a `select` filter over non-finite numbers.
//!
//! `-F` sets the input format of `get`, `keys`, `values` and `select`
//! too (YAML, TOML, JSON, JSONL; not the console or TOON), so each output
//! format reads a file of its own format. `query` reads by extension.

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

/// The `(exit code, stdout, stderr)` a run is expected to end with.
type Outcome<'a> = (i32, &'a str, &'a str);

/// Run every `(args, outcome)` case in `dir` and fail once, listing every
/// case whose outcome differs.
fn assert_cases(dir: &TempDir, cases: &[(Vec<&str>, Outcome<'_>)]) {
    let mut wrong = Vec::new();
    for (args, expected) in cases {
        let (code, stdout, stderr) = run(dir, args);
        let actual = (code, stdout.as_str(), stderr.as_str());
        if actual != *expected {
            wrong.push(format!(
                "{args:?}\n  expected {expected:?}\n  actual   {actual:?}"
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} case(s) differ:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

/// `(0, stdout, "")`: a successful run printing `stdout`.
fn ok(stdout: &str) -> Outcome<'_> {
    (0, stdout, "")
}

const SCALARS_YAML: &str = "s: hi\ni: 3\nf: 1.5\nt: true\nu: null\n\
                            big: 18446744073709551616\nhuge: 1e400\nnan: .nan\nninf: -.inf\n\
                            em: {}\nea: []\nl: [1, .nan]\n";

#[test]
fn keys_values_and_len_name_every_scalar_type_they_refuse() {
    let dir = dir_with(&[("s.yaml", SCALARS_YAML)]);
    let mut cases = Vec::new();
    for (pointer, found) in [
        ("/s", "string"),
        ("/i", "int"),
        ("/f", "float"),
        ("/t", "bool"),
        ("/u", "null"),
        ("/big", "int"),
        ("/huge", "float"),
        ("/nan", "float"),
        ("/ninf", "float"),
    ] {
        for (command, action) in [
            ("keys", "list the keys of"),
            ("values", "list the values of"),
        ] {
            cases.push((
                vec![command, "s.yaml", pointer],
                format!("cannot {action} '{pointer}': it is {found}, not an object or an array\n"),
            ));
        }
        if found != "string" {
            cases.push((
                vec!["len", "s.yaml", pointer],
                format!(
                    "cannot take the length of '{pointer}': it is {found}, not an array, a \
                     string or an object\n"
                ),
            ));
        }
    }
    let cases: Vec<_> = cases
        .iter()
        .map(|(args, stderr)| (args.clone(), (1, "", stderr.as_str())))
        .collect();
    assert_cases(&dir, &cases);
    // What they read: `len` of a string, the keys / values of a sequence.
    assert_cases(
        &dir,
        &[
            (vec!["len", "s.yaml", "/s"], ok("2\n")),
            (vec!["keys", "s.yaml", "/l"], ok("0\n1\n")),
            (vec!["values", "s.yaml", "/l"], ok("1\n.nan\n")),
        ],
    );
}

#[test]
fn a_scalar_document_root_is_named_the_document_root() {
    let dir = dir_with(&[
        ("r.yaml", "42\n"),
        ("r.json", "\"hi\"\n"),
        ("t.yaml", "true\n"),
    ]);
    assert_cases(
        &dir,
        &[
            (
                vec!["keys", "r.yaml", ""],
                (
                    1,
                    "",
                    "cannot list the keys of the document root: it is int, not an object or an \
                     array\n",
                ),
            ),
            (
                vec!["values", "r.json", ""],
                (
                    1,
                    "",
                    "cannot list the values of the document root: it is string, not an object \
                     or an array\n",
                ),
            ),
            (
                vec!["len", "t.yaml", ""],
                (
                    1,
                    "",
                    "cannot take the length of the document root: it is bool, not an array, a \
                     string or an object\n",
                ),
            ),
            // A string root has a length.
            (vec!["len", "r.json", ""], ok("2\n")),
        ],
    );
}

#[test]
fn keys_and_values_of_an_empty_container_list_nothing() {
    let dir = dir_with(&[
        ("s.yaml", SCALARS_YAML),
        ("ea.json", "[]\n"),
        ("em.json", "{}\n"),
        ("e.json", "{\"a\": [], \"o\": {}}\n"),
    ]);
    let mut cases = Vec::new();
    for command in ["keys", "values"] {
        for pointer in ["/em", "/ea"] {
            cases.push((vec![command, "s.yaml", pointer], ok("")));
            cases.push((vec![command, "s.yaml", pointer, "-F", "yaml"], ok("[]\n")));
        }
        for file in ["ea.json", "em.json"] {
            cases.push((vec![command, file, ""], ok("")));
            cases.push((vec![command, file, "", "-F", "json"], ok("[]\n")));
        }
        for pointer in ["/a", "/o"] {
            cases.push((vec![command, "e.json", pointer, "-F", "json"], ok("[]\n")));
        }
    }
    assert_cases(&dir, &cases);
    assert_cases(
        &dir,
        &[
            (vec!["len", "s.yaml", "/ea"], ok("0\n")),
            (vec!["len", "ea.json", ""], ok("0\n")),
        ],
    );
}

const NAN_YAML: &str = "x: .nan\nz: -.inf\nb: 18446744073709551616\ne: 1e400\n";
const NAN_TOML: &str = "x = nan\ny = [1, inf]\nz = -inf\n";
const BIG_JSON: &str =
    "{\"b\": 18446744073709551616, \"e\": 1e400, \"l\": [1e400, -18446744073709551617]}\n";
const BIG_JSONL: &str = "{\"b\": 18446744073709551616, \"e\": 1e400}\n{\"b\": 1}\n";

#[test]
fn every_output_format_spells_non_finite_and_big_numbers_one_way() {
    let dir = dir_with(&[
        ("n.yaml", NAN_YAML),
        ("n.toml", NAN_TOML),
        ("b.json", BIG_JSON),
        ("b.jsonl", BIG_JSONL),
    ]);
    let yaml_items = "- .nan\n- -.inf\n- 18446744073709551616\n- 1.0e+400\n";
    let toml_console = ".nan\n[1, .inf]\n-.inf\n";
    let toml_toon = "[3]:\n  - .nan\n  - [2]: 1,.inf\n  - \"-.inf\"\n";
    let toml_yaml = "- .nan\n- - 1\n  - .inf\n- -.inf\n";
    let json_items = "[\n  18446744073709551616,\n  1e+400,\n  [\n    1e+400,\n    \
                      -18446744073709551617\n  ]\n]\n";
    let json_doc = "{\n  \"b\": 18446744073709551616,\n  \"e\": 1e+400,\n  \"l\": [\n    \
                    1e+400,\n    -18446744073709551617\n  ]\n}\n";
    let jsonl_lines = "{\"b\":18446744073709551616,\"e\":1e+400}\n{\"b\":1}\n";
    let non_finite_json = (
        1,
        "",
        "json format error: non-finite float (NaN or Infinity) cannot be serialized as JSON\n",
    );
    let toml_root = (
        1,
        "",
        "toml format error: a TOML document is a table, so its root is one, not an array\n",
    );
    assert_cases(
        &dir,
        &[
            // Console: a YAML and a TOML NaN alike.
            (
                vec!["get", "n.yaml", ""],
                ok("x: .nan\nz: -.inf\nb: 18446744073709551616\ne: 1e+400\n"),
            ),
            (
                vec!["values", "n.yaml", ""],
                ok(".nan\n-.inf\n18446744073709551616\n1e+400\n"),
            ),
            (
                vec!["select", "n.yaml", "$.*"],
                ok(".nan\n-.inf\n18446744073709551616\n1e+400\n"),
            ),
            (
                vec!["query", ".[]", "n.yaml"],
                ok(".nan\n-.inf\n18446744073709551616\n1e+400\n"),
            ),
            (
                vec!["get", "n.toml", ""],
                ok("x: .nan\ny: [1, .inf]\nz: -.inf\n"),
            ),
            (vec!["values", "n.toml", ""], ok(toml_console)),
            (vec!["values", "n.toml", "/y"], ok("1\n.inf\n")),
            (vec!["select", "n.toml", "$.*"], ok(toml_console)),
            (vec!["query", ".[]", "n.toml"], ok(toml_console)),
            // TOON (the console's input detection): a TOML NaN.
            (
                vec!["get", "n.toml", "", "-F", "toon"],
                ok("x: .nan\ny[2]: 1,.inf\nz: \"-.inf\"\n"),
            ),
            (vec!["values", "n.toml", "", "-F", "toon"], ok(toml_toon)),
            (
                vec!["values", "n.toml", "/y", "-F", "toon"],
                ok("[2]: 1,.inf\n"),
            ),
            (vec!["select", "n.toml", "$.*", "-F", "toon"], ok(toml_toon)),
            (vec!["query", ".[]", "n.toml", "-F", "toon"], ok(toml_toon)),
            // YAML: a float, never a quoted string.
            (
                vec!["get", "n.yaml", "", "-F", "yaml"],
                ok("x: .nan\nz: -.inf\nb: 18446744073709551616\ne: 1.0e+400\n"),
            ),
            (vec!["values", "n.yaml", "", "-F", "yaml"], ok(yaml_items)),
            (
                vec!["select", "n.yaml", "$.*", "-F", "yaml"],
                ok(yaml_items),
            ),
            (vec!["query", ".[]", "n.yaml", "-F", "yaml"], ok(yaml_items)),
            (vec!["query", ".[]", "n.toml", "-F", "yaml"], ok(toml_yaml)),
            // TOML: `nan` / `inf`; a list of values has no TOML root.
            (
                vec!["get", "n.toml", "", "-F", "toml"],
                ok("x = nan\ny = [\n    1,\n    inf,\n]\nz = -inf\n"),
            ),
            (vec!["values", "n.toml", "", "-F", "toml"], toml_root),
            (vec!["select", "n.toml", "$.*", "-F", "toml"], toml_root),
            (vec!["query", ".[]", "n.toml", "-F", "toml"], toml_root),
            // JSON / JSONL: a big number keeps its literal; a NaN is exit 1.
            (vec!["get", "b.json", "", "-F", "json"], ok(json_doc)),
            (vec!["values", "b.json", "", "-F", "json"], ok(json_items)),
            (
                vec!["select", "b.json", "$.*", "-F", "json"],
                ok(json_items),
            ),
            (
                vec!["values", "b.jsonl", "", "-F", "jsonl"],
                ok(jsonl_lines),
            ),
            (
                vec!["select", "b.jsonl", "$.*", "-F", "jsonl"],
                ok(jsonl_lines),
            ),
            (vec!["get", "b.jsonl", "", "-F", "jsonl"], ok(jsonl_lines)),
            (
                vec!["query", ".[]", "n.yaml", "-F", "json"],
                non_finite_json,
            ),
            (
                vec!["query", ".[]", "n.yaml", "-F", "jsonl"],
                non_finite_json,
            ),
            (
                vec!["query", ".[]", "n.toml", "-F", "json"],
                non_finite_json,
            ),
            (
                vec!["query", ".[]", "n.toml", "-F", "jsonl"],
                non_finite_json,
            ),
        ],
    );
}

#[test]
fn diff_and_convert_spell_a_toml_nan_as_yaml_does() {
    let dir = dir_with(&[
        ("a.toml", "x = nan\ny = 1\n"),
        ("b.toml", "x = inf\ny = 2\n"),
    ]);
    assert_cases(
        &dir,
        &[
            (vec!["convert", "a.toml"], ok("x: .nan\ny: 1\n")),
            (
                vec!["diff", "--unified", "a.toml", "b.toml"],
                ok(
                    "--- a/a.toml -> b.toml\n+++ b/a.toml -> b.toml\n@@ -1,4 +1,4 @@\n {\n\
                    -  \"x\": \".nan\",\n-  \"y\": 1\n+  \"x\": \".inf\",\n+  \"y\": 2\n }\n\
                    \\ No newline at end of file\n",
                ),
            ),
        ],
    );
}

const FILTER_YAML: &str = "- {n: .nan, k: nan}\n- {n: .inf, k: inf}\n- {n: -.inf, k: ninf}\n\
                           - {n: 1, k: one}\n- {n: 18446744073709551616, k: big}\n\
                           - {n: 1e400, k: huge}\n";

#[test]
fn select_filters_order_infinities_and_match_no_nan() {
    let dir = dir_with(&[
        ("s.yaml", FILTER_YAML),
        ("n.toml", "a = [nan, 1.5, inf, -inf]\n"),
        ("m.yaml", "n: .nan\n---\nn: .inf\n---\nn: 2\n"),
    ]);
    let select = |file, expr| vec!["select", file, expr, "-F", "yaml"];
    assert_cases(
        &dir,
        &[
            // `-.inf` is below every finite number, `.inf` and `1e400` above.
            (select("s.yaml", "$[?@.n < -1e308].k"), ok("- ninf\n")),
            (
                select("s.yaml", "$[?@.n > -1e308].k"),
                ok("- inf\n- one\n- big\n- huge\n"),
            ),
            (
                select("s.yaml", "$[?@.n >= 1].k"),
                ok("- inf\n- one\n- big\n- huge\n"),
            ),
            (select("s.yaml", "$[?@.n <= 1].k"), ok("- ninf\n- one\n")),
            (select("s.yaml", "$[?@.n > 1e300].k"), ok("- inf\n- huge\n")),
            // A NaN equals nothing, so `!=` holds for it.
            (select("s.yaml", "$[?@.n == 1].k"), ok("- one\n")),
            (
                select("s.yaml", "$[?@.n != 1].k"),
                ok("- nan\n- inf\n- ninf\n- big\n- huge\n"),
            ),
            // The documented f64 limits: no infinity is `==` itself, and
            // `1e400` compares as the f64 nearest it, an infinity.
            (select("s.yaml", "$[?@.n == @.n].k"), ok("- one\n- big\n")),
            (
                select("s.yaml", "$[?@.n != @.n].k"),
                ok("- nan\n- inf\n- ninf\n- huge\n"),
            ),
            // Recursive descent reaches the NaN and orders the rest.
            (
                select("s.yaml", "$..[?@ > 1]"),
                ok("- .inf\n- 18446744073709551616\n- 1.0e+400\n"),
            ),
            (select("s.yaml", "$..[?@ < 1]"), ok("- -.inf\n")),
            (
                select("s.yaml", "$..n"),
                ok("- .nan\n- .inf\n- -.inf\n- 1\n- 18446744073709551616\n- 1.0e+400\n"),
            ),
            // A TOML NaN and infinity are numbers to the filter too.
            (vec!["select", "n.toml", "$.a[?@ > 1]"], ok("1.5\n.inf\n")),
            (vec!["select", "n.toml", "$.a[?@ < 1]"], ok("-.inf\n")),
            (
                vec!["select", "n.toml", "$.a[?@ != 1.5]"],
                ok(".nan\n.inf\n-.inf\n"),
            ),
            // Over a stream: `--doc all` and one document.
            (
                vec![
                    "--doc",
                    "all",
                    "select",
                    "m.yaml",
                    "$[?@.n > 1].n",
                    "-F",
                    "yaml",
                ],
                ok("- .inf\n- 2\n"),
            ),
            (
                vec!["--doc", "1", "select", "m.yaml", "$[?@ > 1]"],
                ok(".inf\n"),
            ),
            (vec!["--doc", "0", "select", "m.yaml", "$[?@ > 1]"], ok("")),
        ],
    );
}

#[test]
fn a_select_filter_literal_past_i64_is_exit_1() {
    let dir = dir_with(&[("s.yaml", FILTER_YAML)]);
    for literal in ["9223372036854775808", "18446744073709551617"] {
        let expr = format!("$[?@.n == {literal}].k");
        let (code, stdout, stderr) = run(&dir, &["select", "s.yaml", &expr]);
        assert_eq!((code, stdout.as_str()), (1, ""), "{expr}: {stderr}");
        assert!(
            stderr.starts_with("invalid JSONPath expression: "),
            "{expr}: {stderr}"
        );
    }
}
