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
            // Console: a YAML and a TOML NaN alike; a big float as `-F yaml`
            // spells it.
            (
                vec!["get", "n.yaml", ""],
                ok("x: .nan\nz: -.inf\nb: 18446744073709551616\ne: 1.0e+400\n"),
            ),
            (
                vec!["values", "n.yaml", ""],
                ok(".nan\n-.inf\n18446744073709551616\n1.0e+400\n"),
            ),
            (
                vec!["select", "n.yaml", "$.*"],
                ok(".nan\n-.inf\n18446744073709551616\n1.0e+400\n"),
            ),
            (
                vec!["query", ".[]", "n.yaml"],
                ok(".nan\n-.inf\n18446744073709551616\n1.0e+400\n"),
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
                ok("--- a.toml\n+++ b.toml\n@@ -1,4 +1,4 @@\n {\n\
                    -  \"x\": \".nan\",\n-  \"y\": 1\n+  \"x\": \".inf\",\n+  \"y\": 2\n }\n"),
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

#[test]
fn json_and_jsonl_spell_a_big_number_as_dq_holds_it() {
    let dir = dir_with(&[(
        "n.yaml",
        "e: 1e400\nu: 1.0E+400\nb: 18446744073709551617\nf: 1.5\n",
    )]);
    assert_cases(
        &dir,
        &[
            (
                vec!["query", ".e, .u, .b, .f", "n.yaml", "-F", "json"],
                ok("[\n  1e400,\n  1.0E+400,\n  18446744073709551617,\n  1.5\n]\n"),
            ),
            (
                vec!["query", ".e, .u, .b, .f", "n.yaml", "-F", "jsonl"],
                ok("1e400\n1.0E+400\n18446744073709551617\n1.5\n"),
            ),
            (
                vec!["convert", "n.yaml", "-F", "jsonl"],
                ok("{\"e\":1e400,\"u\":1.0E+400,\"b\":18446744073709551617,\"f\":1.5}\n"),
            ),
        ],
    );
}

#[test]
fn hcl_keeps_integers_past_64_bits_exact() {
    let dir = dir_with(&[(
        "m.tf",
        "x = 18446744073709551617\nn = -9223372036854775809\nu = 18446744073709551615\n",
    )]);
    assert_cases(
        &dir,
        &[
            (vec!["get", "m.tf", "/x"], ok("18446744073709551617\n")),
            (vec!["get", "m.tf", "/n"], ok("-9223372036854775809\n")),
            (
                vec!["query", ".x == 18446744073709551616", "m.tf"],
                ok("false\n"),
            ),
            (
                vec!["convert", "m.tf", "-F", "json"],
                ok(
                    "{\n  \"x\": 18446744073709551617,\n  \"n\": -9223372036854775809,\n  \
                    \"u\": 18446744073709551615\n}\n",
                ),
            ),
            (
                vec!["convert", "m.tf", "-F", "hcl"],
                (
                    1,
                    "",
                    "hcl format error: HCL cannot represent arbitrary-precision number \
                     18446744073709551617\n",
                ),
            ),
        ],
    );
}

const FLOATS_YAML: &str = "a: 1e300\nb: -1e-300\nc: 1.5e20\nd: 123.25\ne: 3.0\n\
    f: 5e-324\ng: 1.7976931348623157e308\n";

/// `convert -F json` printed `1e300` as 301 digits and `1e-300` as `0.`
/// and 300 zeros; `query -F json` did too once it wrote through the same
/// writer. Every JSON output spells a float shortest, as ryu lays it out.
#[test]
fn json_output_spells_a_float_in_its_shortest_round_trip_form() {
    let dir = dir_with(&[("f.yaml", FLOATS_YAML)]);
    let query = ".a, .b, .c, .d, .e, .f, .g";
    assert_cases(
        &dir,
        &[
            (
                vec!["query", query, "f.yaml", "-F", "json"],
                ok(
                    "[\n  1e300,\n  -1e-300,\n  1.5e20,\n  123.25,\n  3.0,\n  5e-324,\n  \
                    1.7976931348623157e308\n]\n",
                ),
            ),
            (
                vec!["query", query, "f.yaml", "-F", "jsonl"],
                ok("1e300\n-1e-300\n1.5e20\n123.25\n3.0\n5e-324\n1.7976931348623157e308\n"),
            ),
            (
                vec!["-q", "convert", "f.yaml", "-F", "json"],
                ok(
                    "{\n  \"a\": 1e300,\n  \"b\": -1e-300,\n  \"c\": 1.5e20,\n  \
                    \"d\": 123.25,\n  \"e\": 3.0,\n  \"f\": 5e-324,\n  \
                    \"g\": 1.7976931348623157e308\n}\n",
                ),
            ),
            (
                vec!["convert", "f.yaml", "-F", "jsonl"],
                ok(
                    "{\"a\":1e300,\"b\":-1e-300,\"c\":1.5e20,\"d\":123.25,\"e\":3.0,\
                    \"f\":5e-324,\"g\":1.7976931348623157e308}\n",
                ),
            ),
        ],
    );
}

/// The TOML writer and the YAML / TOML / JSON textual edit printed the
/// same digit runs (`a = 1000…000.0`).
#[test]
fn toml_and_textual_edits_spell_a_float_without_a_digit_run() {
    let dir = dir_with(&[
        ("f.yaml", FLOATS_YAML),
        ("s.yaml", "a: 1 # kept\n"),
        ("s.toml", "a = 1 # kept\n"),
        ("s.json", "{\"a\": 1}\n"),
    ]);
    assert_cases(
        &dir,
        &[
            (
                vec!["-q", "convert", "f.yaml", "-F", "toml"],
                ok("a = 1e300\nb = -1e-300\nc = 1.5e20\nd = 123.25\ne = 3.0\n\
                    f = 5e-324\ng = 1.7976931348623157e308\n"),
            ),
            (
                vec!["set", "s.yaml", "/a", "1e300"],
                ok("a: 1.0e+300 # kept\n"),
            ),
            (
                vec!["set", "s.toml", "/a", "1.5e20"],
                ok("a = 1.5e20 # kept\n"),
            ),
            (vec!["set", "s.json", "/a", "1e-7"], ok("{\"a\": 1e-7}\n")),
        ],
    );
}

const SOME_FLOATS_YAML: &str = "a: 1e300\nb: -1e-300\nc: 1.5e20\nd: 123.25\ne: 3.0\nf: 1e-7\n";

/// A float on a 17th-digit tie is written by TOON, in its plain decimals,
/// as every other writer spells it, `…562.2`, not `toon_format`'s odd
/// `…562.3` — next to a string or a key holding the private-use characters
/// the respelling marks floats with too. Before, any such character turned
/// the respelling off for the whole document.
#[test]
fn toon_spells_a_tie_float_the_same_whatever_the_strings_hold() {
    let dir = dir_with(&[
        ("plain.json", "{\"v\": 1658206780088562.2}"),
        (
            "marked.json",
            "{\"s\": \"\u{E000}\", \"v\": 1658206780088562.2}",
        ),
        (
            "key.json",
            "{\"\u{E000}0\u{E001}\": 1, \"v\": [1658206780088562.2]}",
        ),
    ]);
    assert_cases(
        &dir,
        &[
            (
                vec!["get", "plain.json", "", "-F", "toon"],
                ok("v: 1658206780088562.2\n"),
            ),
            (
                vec!["get", "marked.json", "", "-F", "toon"],
                ok("s: \u{E000}\nv: 1658206780088562.2\n"),
            ),
            (
                vec!["get", "key.json", "", "-F", "toon"],
                ok("\"\u{E000}0\u{E001}\": 1\nv[1]: 1658206780088562.2\n"),
            ),
        ],
    );
}

/// The console printed serde_json's `1e+300`, `-F yaml` `1.0e+300`; the
/// line-oriented writers and XML printed 301 digits (and `3` for `3.0`).
/// Now the console spells a float as the YAML writer does, every other
/// writer as `-F json` does.
#[test]
fn the_console_and_every_writer_spell_a_float_without_a_digit_run() {
    let dir = dir_with(&[
        ("v.yaml", SOME_FLOATS_YAML),
        ("i.yaml", "s:\n  a: 1e300\n  e: 3.0\n"),
        ("rows.yaml", "- {a: 1e300, b: 1.5e20, e: 3.0}\n"),
        ("x.yaml", "r:\n- \"@attrs\": {a: 1e300}\n  y: [3.0, 1e-7]\n"),
    ]);
    let yaml_spelled = "a: 1.0e+300\nb: -1.0e-300\nc: 1.5e+20\nd: 123.25\ne: 3.0\nf: 1.0e-7\n";
    assert_cases(
        &dir,
        &[
            (vec!["get", "v.yaml", "/a"], ok("1.0e+300\n")),
            (vec!["get", "v.yaml", ""], ok(yaml_spelled)),
            (
                vec!["-q", "convert", "v.yaml", "-F", "yaml"],
                ok(yaml_spelled),
            ),
            (
                vec!["values", "v.yaml", ""],
                ok("1.0e+300\n-1.0e-300\n1.5e+20\n123.25\n3.0\n1.0e-7\n"),
            ),
            (vec!["select", "v.yaml", "$.c"], ok("1.5e+20\n")),
            (vec!["query", ".f, .e", "v.yaml"], ok("1.0e-7\n3.0\n")),
            (
                vec!["-q", "convert", "v.yaml", "-F", "dot-env"],
                ok("a=1e300\nb=-1e-300\nc=1.5e20\nd=123.25\ne=3.0\nf=1e-7\n"),
            ),
            (
                vec!["-q", "convert", "i.yaml", "-F", "ini"],
                ok("[s]\na=1e300\ne=3.0\n"),
            ),
            (
                vec!["-q", "convert", "rows.yaml", "-F", "csv"],
                ok("a,b,e\n1e300,1.5e20,3.0\n"),
            ),
            (
                vec!["-q", "convert", "rows.yaml", "-F", "tsv"],
                ok("a\tb\te\n1e300\t1.5e20\t3.0\n"),
            ),
            (
                vec!["-q", "convert", "x.yaml", "-F", "xml"],
                ok("<r a=\"1e300\">\n  <y>3.0</y>\n  <y>1e-7</y>\n</r>\n"),
            ),
        ],
    );
}

/// `convert -F hcl` wrote the float `3.0` as `e = 3`, and dq read
/// `e = 3.0` back as the integer 3: a whole float lost its float-ness both
/// ways. `1e16` came out as 17 digits and read back as an integer too.
#[test]
fn hcl_keeps_a_whole_float_a_float_both_ways() {
    let dir = dir_with(&[
        (
            "f.yaml",
            "e: 3.0\nn: -3.0\nh: 1e16\nl: [1.0, {m: 2.0}]\ni: 4\n",
        ),
        ("w.tf", "e = 3.0\nh = 1e16\nz = -0.0\ni = 4\nn = -3.0\n"),
    ]);
    assert_cases(
        &dir,
        &[
            (
                vec!["-q", "convert", "f.yaml", "-F", "hcl"],
                ok(
                    "e = 3.0\nn = -3.0\nh = 1e16\nl = [\n  1.0,\n  {\n    \"m\" = 2.0\n  }\n]\ni = 4\n",
                ),
            ),
            (
                vec!["convert", "w.tf", "-F", "json"],
                ok(
                    "{\n  \"e\": 3.0,\n  \"h\": 1e16,\n  \"z\": -0.0,\n  \"i\": 4,\n  \"n\": -3.0\n}\n",
                ),
            ),
            (vec!["type", "w.tf", "/e"], ok("float\n")),
            (vec!["type", "w.tf", "/i"], ok("int\n")),
        ],
    );
}

/// The XML writer wrote a `#text` that was no string — a number, a
/// boolean, a big integer — as an empty element, and dropped an array or a
/// map there, or an `@attrs` that was no map, without a word.
#[test]
fn xml_writes_a_scalar_text_and_refuses_a_container() {
    let dir = dir_with(&[
        (
            "t.yaml",
            "r:\n- \"@attrs\": {a: 1e300, b: true, c: null}\n  \
             x: [{\"#text\": 1.5e20}]\n  t: [{\"#text\": true}]\n  \
             i: [{\"#text\": 42}]\n  z: [{\"#text\": null}]\n  \
             big: [{\"#text\": 18446744073709551617}]\n",
        ),
        ("list.yaml", "r:\n- x: [{\"#text\": [1, 2]}]\n"),
        ("map.yaml", "r:\n- x: [{\"#text\": {a: 1}}]\n"),
        ("attrs.yaml", "r:\n- \"@attrs\": 5\n"),
    ]);
    assert_cases(
        &dir,
        &[
            (
                vec!["-q", "convert", "t.yaml", "-F", "xml"],
                ok(
                    "<r a=\"1e300\" b=\"true\" c=\"\">\n  <x>1.5e20</x>\n  <t>true</t>\n  \
                    <i>42</i>\n  <z/>\n  <big>18446744073709551617</big>\n</r>\n",
                ),
            ),
            (
                vec!["-q", "convert", "list.yaml", "-F", "xml"],
                (
                    1,
                    "",
                    "xml format error: XML write: '#text' of <x> must be a scalar (got an array)\n",
                ),
            ),
            (
                vec!["-q", "convert", "map.yaml", "-F", "xml"],
                (
                    1,
                    "",
                    "xml format error: XML write: '#text' of <x> must be a scalar (got a map)\n",
                ),
            ),
            (
                vec!["-q", "convert", "attrs.yaml", "-F", "xml"],
                (
                    1,
                    "",
                    "xml format error: XML write: '@attrs' of <r> must be a map of attributes \
                     (got a number 5)\n",
                ),
            ),
        ],
    );
}

/// A tree that did not come from XML may hold any scalar in `#comments`,
/// `#pi` and `#cdata`, or a lone scalar instead of the list: each is written
/// as its text, a null item skipped. A container there is refused, never
/// dropped.
#[test]
fn xml_writes_scalar_comments_pis_and_cdata_and_refuses_containers() {
    let dir = dir_with(&[
        (
            "items.yaml",
            "r:\n- \"#comments\": [42, c]\n  \"#pi\": [true]\n  \"#cdata\": [1.5e20, null]\n",
        ),
        ("lone.yaml", "r:\n- \"#comments\": only\n  \"#cdata\": 7\n"),
        ("nulls.yaml", "r:\n- \"#cdata\": [null]\n  \"#pi\": null\n"),
        ("item.yaml", "r:\n- \"#cdata\": [{a: 1}]\n"),
        ("entry.yaml", "r:\n- \"#pi\": {a: 1}\n"),
        ("top.yaml", "\"#comments\": [[1]]\nr:\n- {}\n"),
    ]);
    assert_cases(
        &dir,
        &[
            (
                vec!["-q", "convert", "items.yaml", "-F", "xml"],
                ok("<r><!--42--><!--c--><?true?><![CDATA[1.5e20]]></r>\n"),
            ),
            (
                vec!["-q", "convert", "lone.yaml", "-F", "xml"],
                ok("<r><!--only--><![CDATA[7]]></r>\n"),
            ),
            (
                vec!["-q", "convert", "nulls.yaml", "-F", "xml"],
                ok("<r/>\n"),
            ),
            (
                vec!["-q", "convert", "item.yaml", "-F", "xml"],
                (
                    1,
                    "",
                    "xml format error: XML write: '#cdata' of <r> must hold scalars (got a map)\n",
                ),
            ),
            (
                vec!["-q", "convert", "entry.yaml", "-F", "xml"],
                (
                    1,
                    "",
                    "xml format error: XML write: '#pi' of <r> must hold scalars (got a map)\n",
                ),
            ),
            (
                vec!["-q", "convert", "top.yaml", "-F", "xml"],
                (
                    1,
                    "",
                    "xml format error: XML write: '#comments' at the top level must hold scalars \
                     (got an array)\n",
                ),
            ),
        ],
    );
}

/// Every XML writer refusal names what it got the same way: a string
/// quoted, a number or a boolean by kind and text, anything else by kind.
/// A child entry that is no list is refused, never dropped: before,
/// `r: [{c: 5}]` was written `<r/>`, and the other refusals named a bare
/// type (`(got object)`, `(got int)`) or nothing at all.
#[test]
fn every_xml_writer_refusal_names_what_it_got() {
    let dir = dir_with(&[
        ("top.yaml", "- 1\n"),
        ("root.yaml", "r: 5\n"),
        ("root_item.yaml", "r: [true]\n"),
        ("child.yaml", "r:\n- c: 5\n"),
        ("child_map.yaml", "r:\n- c: {d: x}\n"),
        ("child_null.yaml", "r:\n- c: null\n  d: []\n"),
        ("attr.yaml", "r:\n- \"@attrs\": {a: [1]}\n"),
        (
            "text.yaml",
            "r:\n- \"#text\": null\n  \"#cdata\": [x]\n  e:\n  - \"#text\": {}\n",
        ),
    ]);
    let refused = |message: &'static str| (1, "", message);
    assert_cases(
        &dir,
        &[
            (
                vec!["-q", "convert", "top.yaml", "-F", "xml"],
                refused(
                    "xml format error: XML write: the top-level value must be a map \
                     (got an array)\n",
                ),
            ),
            (
                vec!["-q", "convert", "root.yaml", "-F", "xml"],
                refused(
                    "xml format error: XML write: root 'r' must be a one-element Array \
                     (got a number 5)\n",
                ),
            ),
            (
                vec!["-q", "convert", "root_item.yaml", "-F", "xml"],
                refused(
                    "xml format error: XML write: root element 'r' must be a Map \
                     (got a boolean true)\n",
                ),
            ),
            (
                vec!["-q", "convert", "child.yaml", "-F", "xml"],
                refused(
                    "xml format error: XML write: child 'c' under <r> must be an Array of \
                     element maps (got a number 5)\n",
                ),
            ),
            (
                vec!["-q", "convert", "child_map.yaml", "-F", "xml"],
                refused(
                    "xml format error: XML write: child 'c' under <r> must be an Array of \
                     element maps (got a map)\n",
                ),
            ),
            (
                vec!["-q", "convert", "child_null.yaml", "-F", "xml"],
                ok("<r/>\n"),
            ),
            (
                vec!["-q", "convert", "attr.yaml", "-F", "xml"],
                refused(
                    "xml format error: XML write: attribute 'a' on <r> must be a scalar \
                     (got an array)\n",
                ),
            ),
            (
                vec!["-q", "convert", "text.yaml", "-F", "xml"],
                refused(
                    "xml format error: XML write: '#text' of <e> must be a scalar (got a map)\n",
                ),
            ),
        ],
    );
}

/// The document level: `#xml` fields are written as their scalar text (a
/// YAML `version: 1.1` is a float), top-level PIs and comments as in an
/// element. What the document cannot hold — an `#xml` that is not a map or
/// has a foreign field, a second root, text outside the root — is refused.
#[test]
fn xml_writes_the_declaration_fields_and_refuses_what_a_document_cannot_hold() {
    let dir = dir_with(&[
        (
            "decl.yaml",
            "\"#xml\": {version: 1.1, encoding: UTF-8, standalone: null}\n\
             \"#pi\": [x]\n\"#comments\": [1]\nr:\n- {}\n",
        ),
        ("decl_str.yaml", "\"#xml\": \"1.0\"\nr:\n- {}\n"),
        (
            "decl_key.yaml",
            "\"#xml\": {version: \"1.0\", foo: bar}\nr:\n- {}\n",
        ),
        ("decl_list.yaml", "\"#xml\": {version: [1]}\nr:\n- {}\n"),
        ("two.yaml", "a:\n- {}\nb:\n- {}\n"),
        ("text.yaml", "\"#text\": hi\nr:\n- {}\n"),
        ("empty.yaml", "\"@attrs\": {}\n\"#cdata\": []\nr:\n- {}\n"),
    ]);
    assert_cases(
        &dir,
        &[
            (
                vec!["-q", "convert", "decl.yaml", "-F", "xml"],
                ok("<?xml version=\"1.1\" encoding=\"UTF-8\"?>\n<?x?>\n<!--1-->\n<r/>\n"),
            ),
            (
                vec!["-q", "convert", "decl_str.yaml", "-F", "xml"],
                (
                    1,
                    "",
                    "xml format error: XML write: '#xml' must be a map of declaration fields \
                     (got \"1.0\")\n",
                ),
            ),
            (
                vec!["-q", "convert", "decl_key.yaml", "-F", "xml"],
                (
                    1,
                    "",
                    "xml format error: XML write: '#xml' has no field 'foo' \
                     (only version, encoding, standalone, bom, detected)\n",
                ),
            ),
            (
                vec!["-q", "convert", "decl_list.yaml", "-F", "xml"],
                (
                    1,
                    "",
                    "xml format error: XML write: '#xml' field 'version' must be a scalar \
                     (got an array)\n",
                ),
            ),
            (
                vec!["-q", "convert", "two.yaml", "-F", "xml"],
                (
                    1,
                    "",
                    "xml format error: XML write: a document has one root element; \
                     the top-level map holds 'a' and 'b'\n",
                ),
            ),
            (
                vec!["-q", "convert", "text.yaml", "-F", "xml"],
                (
                    1,
                    "",
                    "xml format error: XML write: '#text' cannot stand at the top level, \
                     outside the root element (got \"hi\")\n",
                ),
            ),
            (
                vec!["-q", "convert", "empty.yaml", "-F", "xml"],
                ok("<r/>\n"),
            ),
        ],
    );
}

/// `(1, "", "xml format error: XML write: <message>\n")`: the XML writer's
/// refusal.
macro_rules! refused {
    ($message:literal) => {
        (
            1,
            "",
            concat!("xml format error: XML write: ", $message, "\n"),
        )
    };
}

/// Names and the declaration: an element or attribute name must be a
/// namespace QName (one `prefix:` at most; `xmlns:p` passes), a PI target an
/// NCName other than `xml`, and the `#xml` fields must fit the declaration
/// grammar — a boolean `standalone` is `yes` / `no`. The writer refuses the
/// rest instead of emitting XML that is not well-formed.
#[test]
fn xml_refuses_names_and_declaration_fields_xml_does_not_allow() {
    let dir = dir_with(&[
        (
            "sa_bool.yaml",
            "\"#xml\": {version: \"1.0\", standalone: true}\nr:\n- {}\n",
        ),
        ("sa_str.yaml", "\"#xml\": {standalone: maybe}\nr:\n- {}\n"),
        ("ver.yaml", "\"#xml\": {version: abc}\nr:\n- {}\n"),
        ("enc.yaml", "\"#xml\": {encoding: \"a b\"}\nr:\n- {}\n"),
        ("colons.yaml", "\"x:y:z\":\n- {}\n"),
        ("lead_colon.yaml", "\":a\":\n- {}\n"),
        ("trail_colon.yaml", "r:\n- \"@attrs\": {\"a:\": 1}\n"),
        (
            "prefixed.yaml",
            "\"svg:r\":\n- \"@attrs\": {\"xmlns:svg\": u, xmlns: d, \"svg:w\": 1}\n",
        ),
        ("pi_colon.yaml", "r:\n- \"#pi\": [\"a:b c\"]\n"),
        ("space.yaml", "\"a b\":\n- {}\n"),
        ("digit.yaml", "r:\n- \"1x\": [{}]\n"),
        ("empty.yaml", "\"\":\n- {}\n"),
        ("attr.yaml", "r:\n- \"@attrs\": {\"a b\": 1}\n"),
        ("pi_num.yaml", "r:\n- \"#pi\": [42]\n"),
        ("pi_empty.yaml", "r:\n- \"#pi\": [\"\"]\n"),
        ("pi_xml.yaml", "\"#pi\": [\"XmL v\"]\nr:\n- {}\n"),
    ]);
    let cases = [
        (
            "sa_bool.yaml",
            ok("<?xml version=\"1.0\" standalone=\"yes\"?>\n<r/>\n"),
        ),
        (
            "sa_str.yaml",
            refused!("'#xml' field 'standalone' must be yes or no (got \"maybe\")"),
        ),
        (
            "ver.yaml",
            refused!("'#xml' field 'version' must be 1. and digits (got \"abc\")"),
        ),
        (
            "enc.yaml",
            refused!("'#xml' field 'encoding' is not an encoding name (got \"a b\")"),
        ),
        (
            "colons.yaml",
            refused!(
                "'x:y:z' is not a valid XML name for an element (a name holds at most one ':', \
                 between a prefix and a local name)"
            ),
        ),
        (
            "lead_colon.yaml",
            refused!(
                "':a' is not a valid XML name for an element (a name holds at most one ':', \
                 between a prefix and a local name)"
            ),
        ),
        (
            "trail_colon.yaml",
            refused!(
                "'a:' is not a valid XML name for an attribute (on <r>) (a name holds at most \
                 one ':', between a prefix and a local name)"
            ),
        ),
        (
            "prefixed.yaml",
            ok("<svg:r xmlns:svg=\"u\" xmlns=\"d\" svg:w=\"1\"/>\n"),
        ),
        (
            "pi_colon.yaml",
            refused!(
                "'a:b' is not a valid XML name for a processing-instruction target \
                 ('#pi' of <r>, a target holds no ':')"
            ),
        ),
        (
            "space.yaml",
            refused!("'a b' is not a valid XML name for an element"),
        ),
        (
            "digit.yaml",
            refused!("'1x' is not a valid XML name for an element"),
        ),
        (
            "empty.yaml",
            refused!("'' is not a valid XML name for an element"),
        ),
        (
            "attr.yaml",
            refused!("'a b' is not a valid XML name for an attribute (on <r>)"),
        ),
        (
            "pi_num.yaml",
            refused!(
                "'42' is not a valid XML name for a processing-instruction target ('#pi' of <r>)"
            ),
        ),
        (
            "pi_empty.yaml",
            refused!(
                "'' is not a valid XML name for a processing-instruction target ('#pi' of <r>)"
            ),
        ),
        (
            "pi_xml.yaml",
            refused!(
                "'XmL' cannot be a processing-instruction target, XML reserves it for the \
                 declaration ('#pi' at the top level)"
            ),
        ),
    ];
    assert_cases(
        &dir,
        &cases.map(|(file, outcome)| (vec!["-q", "convert", file, "-F", "xml"], outcome)),
    );
}

/// Content: a comment is written as it is (XML has no escapes in one) and
/// refused with `--` or a trailing `-`; a PI with `?>` is refused; CDATA
/// with `]]>` is split across adjacent sections; a C0 control other than
/// tab / LF / CR in text or an attribute is refused. An XML file in dq's
/// layout writes back byte for byte.
#[test]
fn xml_refuses_content_xml_cannot_hold_and_round_trips_a_read_tree() {
    let round_trip = "<?xml version=\"1.0\" standalone=\"yes\"?>\n<!--top & <x>-->\n<r a=\"1\">\n  \
                      <!--c < d-->\n  <?pi x?><![CDATA[d]]]]><![CDATA[>e]]>\n  <k>v</k>\n</r>\n";
    let dir = dir_with(&[
        ("comment.yaml", "r:\n- \"#comments\": [\"a < b & c\"]\n"),
        ("dashes.yaml", "r:\n- \"#comments\": [\"a--b\"]\n"),
        ("dash_end.yaml", "\"#comments\": [\"a-\"]\nr:\n- {}\n"),
        ("pi_close.yaml", "r:\n- \"#pi\": [\"t a?>b\"]\n"),
        ("cdata.yaml", "r:\n- \"#cdata\": [\"x]]>y\"]\n"),
        ("ctl_text.yaml", "r:\n- \"#text\": \"a\\x01b\"\n"),
        ("ctl_attr.yaml", "r:\n- \"@attrs\": {a: \"\\a\"}\n"),
        ("ctl_cdata.yaml", "r:\n- \"#cdata\": [\"\\x1f\"]\n"),
        ("rt.xml", round_trip),
    ]);
    let cases = [
        ("comment.yaml", ok("<r><!--a < b & c--></r>\n")),
        (
            "dashes.yaml",
            refused!(
                "a comment in '#comments' of <r> contains '--', which XML does not allow in a comment"
            ),
        ),
        (
            "dash_end.yaml",
            refused!(
                "a comment in '#comments' at the top level ends in '-', which XML does not allow \
                 in a comment"
            ),
        ),
        (
            "pi_close.yaml",
            refused!(
                "a processing instruction in '#pi' of <r> contains '?>', which would end it early"
            ),
        ),
        ("cdata.yaml", ok("<r><![CDATA[x]]]]><![CDATA[>y]]></r>\n")),
        (
            "ctl_text.yaml",
            refused!("'#text' of <r> holds U+0001, a character XML 1.0 does not allow"),
        ),
        (
            "ctl_attr.yaml",
            refused!("attribute 'a' on <r> holds U+0007, a character XML 1.0 does not allow"),
        ),
        (
            "ctl_cdata.yaml",
            refused!("'#cdata' of <r> holds U+001F, a character XML 1.0 does not allow"),
        ),
        ("rt.xml", (0, round_trip, "")),
    ];
    assert_cases(
        &dir,
        &cases.map(|(file, outcome)| (vec!["-q", "convert", file, "-F", "xml"], outcome)),
    );
}

/// `#xml`'s `encoding` names the output encoding: UTF-8 in any spelling
/// stays UTF-8; a label encoding_rs does not know, a character the encoding
/// cannot hold (named with its place) and a `bom` that contradicts the
/// label are refused. A CR in text and a tab / LF / CR in an attribute value
/// are written as character references, so they read back unchanged — and a
/// file holding those references writes back byte for byte.
#[test]
fn xml_writes_the_declared_encoding_and_keeps_whitespace() {
    let refs = "<r a=\"x&#10;y&#9;z&#13;w\">p&#13;q\ns</r>\n";
    let latin = "<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?>\n<r>cafe</r>\n";
    let dir = dir_with(&[
        (
            "utf8.yaml",
            "\"#xml\": {encoding: utf8}\nr:\n- \"#text\": é\n",
        ),
        (
            "latin_ascii.yaml",
            "\"#xml\": {encoding: ISO-8859-1}\nr:\n- \"#text\": cafe\n",
        ),
        (
            "ascii.yaml",
            "\"#xml\": {encoding: US-ASCII}\nr:\n- k: [{\"@attrs\": {a: π}}]\n",
        ),
        (
            "windows.yaml",
            "\"#xml\": {encoding: windows-1252}\n\"#comments\": [π]\nr:\n- {}\n",
        ),
        ("unknown.yaml", "\"#xml\": {encoding: no-such}\nr:\n- {}\n"),
        (
            "bom.yaml",
            "\"#xml\": {encoding: ISO-8859-1, bom: UTF-16LE}\nr:\n- {}\n",
        ),
        (
            "ws.yaml",
            "r:\n- \"@attrs\": {a: \"x\\ny\\tz\\rw\"}\n  \"#text\": \"p\\rq\\ns\"\n",
        ),
        ("refs.xml", refs),
        ("latin.xml", latin),
    ]);
    let cases = [
        (
            "utf8.yaml",
            ok("<?xml version=\"1.0\" encoding=\"utf8\"?>\n<r>é</r>\n"),
        ),
        (
            "latin_ascii.yaml",
            ok("<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?>\n<r>cafe</r>\n"),
        ),
        (
            "ascii.yaml",
            refused!("'#xml' declares encoding \"US-ASCII\", which cannot hold U+03C0 in <k>"),
        ),
        (
            "windows.yaml",
            refused!(
                "'#xml' declares encoding \"windows-1252\", which cannot hold U+03C0 at the top \
                 level"
            ),
        ),
        (
            "unknown.yaml",
            refused!("'#xml' declares encoding \"no-such\", which dq does not know"),
        ),
        (
            "bom.yaml",
            refused!("'#xml' field 'bom' is UTF-16LE but the declared encoding is \"ISO-8859-1\""),
        ),
        ("ws.yaml", ok(refs)),
        ("refs.xml", ok(refs)),
        ("latin.xml", ok(latin)),
    ];
    assert_cases(
        &dir,
        &cases.map(|(file, outcome)| (vec!["-q", "convert", file, "-F", "xml"], outcome)),
    );
}

/// The UTF-16 bytes of `text`, byte order mark first, big- or
/// little-endian.
fn utf16(text: &str, big_endian: bool) -> Vec<u8> {
    std::iter::once(0xFEFF)
        .chain(text.encode_utf16())
        .flat_map(|unit| {
            if big_endian {
                unit.to_be_bytes()
            } else {
                unit.to_le_bytes()
            }
        })
        .collect()
}

/// A file in ISO-8859-1, windows-1252, Shift_JIS or UTF-16 (either byte
/// order, with or without a declaration) reads in its encoding and writes
/// back byte for byte; a tree declaring such an encoding is written in it.
/// Before, every one of these files was a parse error (exit 3).
#[test]
fn xml_reads_and_writes_files_in_their_declared_encoding() {
    let latin =
        b"<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?>\n<r a=\"\xe9\">caf\xe9</r>\n".to_vec();
    let windows =
        b"<?xml version=\"1.0\" encoding=\"windows-1252\"?>\n<r>\x93q\x94 \x80</r>\n".to_vec();
    // "日本" in Shift_JIS.
    let sjis =
        b"<?xml version=\"1.0\" encoding=\"Shift_JIS\"?>\n<r>\x93\xfa\x96\x7b</r>\n".to_vec();
    let le = utf16(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n<r>é ☃</r>\n",
        false,
    );
    let be = utf16(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n<r>é</r>\n",
        true,
    );
    let bare = utf16("<r>é</r>\n", false);
    let dir = dir_with_bytes(&[
        ("latin.xml", &latin),
        ("windows.xml", &windows),
        ("sjis.xml", &sjis),
        ("le.xml", &le),
        ("be.xml", &be),
        ("bare.xml", &bare),
        (
            "latin.yaml",
            "\"#xml\": {encoding: ISO-8859-1}\nr:\n- \"#text\": café\n".as_bytes(),
        ),
        (
            "utf16.yaml",
            "\"#xml\": {encoding: UTF-16}\nr:\n- \"#text\": é\n".as_bytes(),
        ),
    ]);
    let cases: [(&str, &str, &[u8]); 9] = [
        ("latin.xml", "xml", &latin),
        ("windows.xml", "xml", &windows),
        ("sjis.xml", "xml", &sjis),
        ("le.xml", "xml", &le),
        ("be.xml", "xml", &be),
        ("bare.xml", "xml", &bare),
        (
            "le.xml",
            "json",
            "{\n  \"#xml\": {\n    \"version\": \"1.0\",\n    \"encoding\": \"UTF-16\",\n    \
             \"bom\": \"UTF-16LE\"\n  },\n  \"r\": [\n    {\n      \"#text\": \"é ☃\"\n    }\n  \
             ]\n}\n"
                .as_bytes(),
        ),
        (
            "latin.yaml",
            "xml",
            b"<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?>\n<r>caf\xe9</r>\n",
        ),
        (
            "utf16.yaml",
            "xml",
            &utf16(
                "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n<r>é</r>\n",
                false,
            ),
        ),
    ];
    assert_byte_cases(
        &dir,
        &cases.map(|(file, to, expected)| (file, to, 0, expected, "")),
    );
}

/// A temp dir holding `files`, given as bytes.
fn dir_with_bytes(files: &[(&str, &[u8])]) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    for (name, bytes) in files {
        std::fs::write(dir.path().join(name), bytes).expect("write fixture");
    }
    dir
}

/// Run `dq --no-color -q convert <file> -F <to>` in `dir` for each case and
/// compare exit code, stdout as bytes and stderr; report every mismatch.
fn assert_byte_cases(dir: &TempDir, cases: &[(&str, &str, i32, &[u8], &str)]) {
    let mut wrong = Vec::new();
    for &(file, to, code, stdout, stderr) in cases {
        let out = dq()
            .current_dir(dir.path())
            .args(["--no-color", "-q", "convert", file, "-F", to])
            .output()
            .expect("dq runs");
        let actual = (
            out.status.code(),
            out.stdout.as_slice(),
            out.stderr.as_slice(),
        );
        if actual != (Some(code), stdout, stderr.as_bytes()) {
            wrong.push(format!(
                "{file} -F {to}\n  expected {code} {:?} {stderr:?}\n  actual   {:?} {:?} {:?}",
                String::from_utf8_lossy(stdout),
                actual.0,
                String::from_utf8_lossy(actual.1),
                String::from_utf8_lossy(actual.2),
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

/// `US-ASCII` and `ISO-8859-1` follow IANA and the XML spec, not WHATWG
/// (which makes both windows-1252): US-ASCII is strict 7-bit, a byte from
/// 0x80 up a parse error and a non-ASCII character refused on write;
/// ISO-8859-1 maps byte to code point both ways, so 0x80 is U+0080 and €
/// is refused. windows-1252 keeps 0x80 = €.
#[test]
fn xml_reads_us_ascii_strictly_and_iso_8859_1_byte_for_code_point() {
    let latin = b"<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?>\n<r>\x80\xe9</r>\n".to_vec();
    let windows = b"<?xml version=\"1.0\" encoding=\"windows-1252\"?>\n<r>\x80</r>".to_vec();
    let ascii = b"<?xml version=\"1.0\" encoding=\"US-ASCII\"?><r>caf\xe9</r>".to_vec();
    let dir = dir_with_bytes(&[
        ("latin.xml", &latin),
        ("windows.xml", &windows),
        ("ascii.xml", &ascii),
        (
            "latin_c1.yaml",
            b"\"#xml\": {encoding: latin1}\nr:\n- \"#text\": \"\\x80\"\n",
        ),
        (
            "latin_euro.yaml",
            "\"#xml\": {encoding: ISO-8859-1}\nr:\n- \"#text\": €\n".as_bytes(),
        ),
        (
            "ascii_e.yaml",
            "\"#xml\": {encoding: ascii}\nr:\n- \"#text\": é\n".as_bytes(),
        ),
    ]);
    let json = |encoding: &str, text: &str| {
        format!(
            "{{\n  \"#xml\": {{\n    \"version\": \"1.0\",\n    \"encoding\": \"{encoding}\"\n  \
             }},\n  \"r\": [\n    {{\n      \"#text\": \"{text}\"\n    }}\n  ]\n}}\n"
        )
    };
    let latin_json = json("ISO-8859-1", "\u{80}é");
    let windows_json = json("windows-1252", "€");
    let refused = |message: &str| format!("xml format error: XML write: {message}\n");
    let euro = refused("'#xml' declares encoding \"ISO-8859-1\", which cannot hold U+20AC in <r>");
    let e_acute = refused("'#xml' declares encoding \"ascii\", which cannot hold U+00E9 in <r>");
    assert_byte_cases(
        &dir,
        &[
            ("latin.xml", "xml", 0, &latin, ""),
            ("latin.xml", "json", 0, latin_json.as_bytes(), ""),
            ("windows.xml", "json", 0, windows_json.as_bytes(), ""),
            (
                "ascii.xml",
                "json",
                3,
                b"",
                "parse error in ascii.xml: invalid US-ASCII in XML input: byte 0xE9\n  --> ascii.xml:1:48\n",
            ),
            (
                "latin_c1.yaml",
                "xml",
                0,
                b"<?xml version=\"1.0\" encoding=\"latin1\"?>\n<r>\x80</r>\n",
                "",
            ),
            ("latin_euro.yaml", "xml", 1, b"", &euro),
            ("ascii_e.yaml", "xml", 1, b"", &e_acute),
        ],
    );
}

/// Namespace well-formedness: a prefix on an element or attribute name must
/// be declared by an `xmlns:p` on that element or an ancestor (`xml` always
/// is), `xmlns` names only declarations, `xmlns:xml` binds only its fixed
/// URI, nothing else binds the `xml` / `xmlns` URIs, and a 1.0 prefix cannot
/// be undeclared. The writer refuses the rest.
#[test]
fn xml_refuses_names_outside_the_namespaces_in_scope() {
    let dir = dir_with(&[
        (
            "ok.yaml",
            "r:\n- \"@attrs\": {\"xmlns:p\": u}\n  \"p:k\": [{\"@attrs\": {\"p:a\": 1, \"xml:lang\": en}}]\n",
        ),
        ("element.yaml", "\"svg:r\":\n- {}\n"),
        ("attribute.yaml", "r:\n- \"@attrs\": {\"p:a\": 1}\n"),
        (
            "sibling.yaml",
            "r:\n- k: [{\"@attrs\": {\"xmlns:p\": u}}]\n  \"p:x\": [{}]\n",
        ),
        ("xmlns_element.yaml", "\"xmlns:a\":\n- {}\n"),
        (
            "xmlns_xmlns.yaml",
            "r:\n- \"@attrs\": {\"xmlns:xmlns\": u}\n",
        ),
        ("xml_rebound.yaml", "r:\n- \"@attrs\": {\"xmlns:xml\": u}\n"),
        (
            "xml_uri.yaml",
            "r:\n- \"@attrs\": {\"xmlns:p\": \"http://www.w3.org/XML/1998/namespace\"}\n",
        ),
        (
            "xmlns_uri.yaml",
            "r:\n- \"@attrs\": {xmlns: \"http://www.w3.org/2000/xmlns/\"}\n",
        ),
        ("undeclare.yaml", "r:\n- \"@attrs\": {\"xmlns:p\": \"\"}\n"),
    ]);
    let cases = [
        (
            "ok.yaml",
            ok("<r xmlns:p=\"u\">\n  <p:k p:a=\"1\" xml:lang=\"en\"/>\n</r>\n"),
        ),
        (
            "element.yaml",
            refused!(
                "element <svg:r> uses the prefix 'svg', which no 'xmlns:svg' declares in scope"
            ),
        ),
        (
            "attribute.yaml",
            refused!(
                "attribute 'p:a' on <r> uses the prefix 'p', which no 'xmlns:p' declares in scope"
            ),
        ),
        (
            "sibling.yaml",
            refused!("element <p:x> uses the prefix 'p', which no 'xmlns:p' declares in scope"),
        ),
        (
            "xmlns_element.yaml",
            refused!(
                "'xmlns:a' cannot name an element: the xmlns prefix is reserved for namespace \
                 declarations"
            ),
        ),
        (
            "xmlns_xmlns.yaml",
            refused!("'xmlns:xmlns' on <r> cannot be declared: the xmlns prefix is reserved"),
        ),
        (
            "xml_rebound.yaml",
            refused!(
                "'xmlns:xml' on <r> must bind http://www.w3.org/XML/1998/namespace (got \"u\")"
            ),
        ),
        (
            "xml_uri.yaml",
            refused!(
                "'xmlns:p' on <r> binds http://www.w3.org/XML/1998/namespace, which only the xml \
                 prefix may"
            ),
        ),
        (
            "xmlns_uri.yaml",
            refused!("'xmlns' on <r> binds http://www.w3.org/2000/xmlns/, which nothing may"),
        ),
        (
            "undeclare.yaml",
            refused!("'xmlns:p' on <r> is empty: XML namespaces 1.0 cannot undeclare a prefix"),
        ),
    ];
    assert_cases(
        &dir,
        &cases.map(|(file, outcome)| (vec!["-q", "convert", file, "-F", "xml"], outcome)),
    );
}

/// `diff` printed a patch value as serde_json does (`1e+300`, `1.5e+20`)
/// on the console and with `-F json`, as 301 digits with `-F jsonl`, and
/// `--unified` compared serde_json's text. Every JSON spelling of a patch
/// is now the shortest round-trip one; `-F yaml` keeps the YAML writer's.
#[test]
fn diff_spells_a_patch_float_as_its_output_format_does() {
    let dir = dir_with(&[
        ("a.yaml", "a: 1\nb: 3.0\n"),
        ("b.yaml", "a: 1e300\nb: 1.5e20\nc: 1e-7\n"),
    ]);
    let json_patch = "[\n  {\n    \"op\": \"add\",\n    \"path\": \"/c\",\n    \"value\": 1e-7\n  \
                      },\n  {\n    \"op\": \"replace\",\n    \"path\": \"/a\",\n    \
                      \"value\": 1e300\n  },\n  {\n    \"op\": \"replace\",\n    \
                      \"path\": \"/b\",\n    \"value\": 1.5e20\n  }\n]\n";
    let diff = |extra: &[&'static str]| {
        let mut args = vec!["diff", "a.yaml", "b.yaml"];
        args.extend_from_slice(extra);
        args
    };
    assert_cases(
        &dir,
        &[
            (diff(&[]), ok(json_patch)),
            (diff(&["-F", "json"]), ok(json_patch)),
            (
                diff(&["-F", "jsonl"]),
                ok("{\"op\":\"add\",\"path\":\"/c\",\"value\":1e-7}\n\
                    {\"op\":\"replace\",\"path\":\"/a\",\"value\":1e300}\n\
                    {\"op\":\"replace\",\"path\":\"/b\",\"value\":1.5e20}\n"),
            ),
            (
                diff(&["-F", "yaml"]),
                ok(
                    "- op: add\n  path: /c\n  value: 1.0e-7\n- op: replace\n  path: /a\n  \
                    value: 1.0e+300\n- op: replace\n  path: /b\n  value: 1.5e+20\n",
                ),
            ),
            (
                diff(&["--unified"]),
                ok("--- a.yaml\n+++ b.yaml\n@@ -1,4 +1,5 @@\n {\n\
                    -  \"a\": 1,\n-  \"b\": 3.0\n+  \"a\": 1e300,\n+  \"b\": 1.5e20,\n\
                    +  \"c\": 1e-7\n }\n"),
            ),
        ],
    );
}

/// `fmt` with `--indent` / `--sort-keys` and `set --jq` re-emit through
/// the JSON and TOML writers, which printed `1e300` as 301 digits and
/// `-1e-300` as `-0.` and 299 zeros and a 1, nested values included. The
/// `-i` write lands the same bytes in the file.
#[test]
fn fmt_and_set_jq_re_emit_a_float_in_its_shortest_form() {
    let source = "{\"z\": 1e300, \"a\": [1.5e20, 3.0], \"m\": {\"k\": -1e-300}}\n";
    let dir = dir_with(&[
        ("f.json", source),
        ("g.json", source),
        ("s.toml", "a = 1 # kept\n[t]\nb = 2\n"),
    ]);
    let set_json = "{\n  \"z\": 1e300,\n  \"a\": [\n    1.5e20,\n    2.5e-10\n  ],\n  \
                    \"m\": {\n    \"k\": -1e-300\n  },\n  \"n\": 1e-7\n}\n";
    assert_cases(
        &dir,
        &[
            (
                vec!["fmt", "f.json", "--indent", "4", "--sort-keys"],
                ok(
                    "{\n    \"a\": [\n        1.5e20,\n        3.0\n    ],\n    \"m\": {\n        \
                    \"k\": -1e-300\n    },\n    \"z\": 1e300\n}\n",
                ),
            ),
            (
                vec!["fmt", "f.json", "--indent", "0"],
                ok("{\"z\":1e300,\"a\":[1.5e20,3.0],\"m\":{\"k\":-1e-300}}\n"),
            ),
            (
                vec!["set", "f.json", "--jq", ".n = 1e-7 | .a[1] = 2.5e-10"],
                ok(set_json),
            ),
            (
                vec![
                    "set",
                    "s.toml",
                    "--jq",
                    ".t.c = 1e300 | .t.d = [3.0, 1.5e20]",
                ],
                ok("a = 1\n\n[t]\nb = 2\nc = 1e300\nd = [\n    3.0,\n    1.5e20,\n]\n"),
            ),
            (
                vec!["-i", "set", "g.json", "--jq", ".n = 1e-7 | .a[1] = 2.5e-10"],
                ok(""),
            ),
        ],
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("g.json")).expect("read g.json"),
        set_json
    );
}

/// HCL reads big integers and whole floats exactly at any depth: inside
/// labelled blocks, repeated blocks, object attributes with a quoted key,
/// arrays and objects in arrays. Writing keeps a whole float a float,
/// writes blocks back as blocks, and still refuses a big integer, wherever
/// it sits.
#[test]
fn hcl_keeps_big_integers_and_whole_floats_inside_blocks() {
    let blocks = "resource \"aws_x\" \"a\" {\n  big = 18446744073709551617\n  w   = 3.0\n  \
                  e   = 1e16\n  nested = {\n    \"quoted key\" = 2.0\n    \
                  inner = [1.0, 18446744073709551618, { z = -0.0 }]\n  }\n}\n\
                  resource \"aws_x\" \"b\" {\n  w = 4.0\n}\nlocals {\n  n = 5.0\n}\n\
                  locals {\n  m = 7\n}\n";
    // Already laid out as dq writes it.
    let small = "resource \"aws_x\" \"a\" {\n  w = 3.0\n  e = 1e16\n  nested = {\n    \
                 \"quoted key\" = 2.0\n  }\n}\nresource \"aws_x\" \"b\" {\n  w = 4.0\n}\n\
                 locals {\n  n = 5.0\n}\nlocals {\n  m = 7\n}\n";
    let dir = dir_with(&[("h.tf", blocks), ("s.tf", small)]);
    let set = small
        .replace("  w = 4.0\n", "  w = 1e300\n")
        .replace("  m = 7\n", "  m = 7\n  q = 2.0\n");
    assert_cases(
        &dir,
        &[
            (
                vec!["convert", "h.tf", "-F", "json"],
                ok(
                    "{\n  \"resource\": {\n    \"aws_x\": {\n      \"a\": {\n        \
                    \"big\": 18446744073709551617,\n        \"w\": 3.0,\n        \"e\": 1e16,\n        \
                    \"nested\": {\n          \"quoted key\": 2.0,\n          \"inner\": [\n            \
                    1.0,\n            18446744073709551618,\n            {\n              \
                    \"z\": -0.0\n            }\n          ]\n        }\n      },\n      \
                    \"b\": {\n        \"w\": 4.0\n      }\n    }\n  },\n  \"locals\": [\n    \
                    {\n      \"n\": 5.0\n    },\n    {\n      \"m\": 7\n    }\n  ]\n}\n",
                ),
            ),
            (vec!["type", "h.tf", "/locals/0/n"], ok("float\n")),
            (vec!["type", "h.tf", "/locals/1/m"], ok("int\n")),
            (
                vec!["type", "h.tf", "/resource/aws_x/a/nested/quoted key"],
                ok("float\n"),
            ),
            (
                vec!["get", "h.tf", "/resource/aws_x/a/nested/inner/1"],
                ok("18446744073709551618\n"),
            ),
            (
                vec![
                    "query",
                    ".resource.aws_x.a.big == 18446744073709551616",
                    "h.tf",
                ],
                ok("false\n"),
            ),
            (
                vec!["convert", "h.tf", "-F", "hcl"],
                (
                    1,
                    "",
                    "hcl format error: HCL cannot represent arbitrary-precision number \
                     18446744073709551617\n",
                ),
            ),
            (vec!["convert", "s.tf", "-F", "hcl"], ok(small)),
            (
                vec![
                    "set",
                    "s.tf",
                    "--jq",
                    ".locals[1].q = 2.0 | .resource.aws_x.b.w = 1e300",
                ],
                ok(&set),
            ),
        ],
    );
}

/// Blocks repeated with the same type each keep their own number, also
/// when the two literals read as the same `f64`: `0.10000000000000000001`
/// stays exact next to a `0.1`, in either order. Before, the blocks were
/// merged and only the last block's number was left.
#[test]
fn hcl_repeated_blocks_keep_each_blocks_number() {
    let long = "0.10000000000000000001";
    let blocks =
        |first: &str, last: &str| format!("x {{\n  a = {first}\n}}\nx {{\n  a = {last}\n}}\n");
    let dir = dir_with(&[
        ("short_last.tf", &blocks(long, "0.1")),
        ("long_last.tf", &blocks("0.1", long)),
    ]);
    let json = |first: &str, last: &str| {
        format!(
            "{{\n  \"x\": [\n    {{\n      \"a\": {first}\n    }},\n    {{\n      \"a\": {last}\n    \
             }}\n  ]\n}}\n"
        )
    };
    assert_cases(
        &dir,
        &[
            (
                vec!["convert", "short_last.tf", "-F", "json"],
                ok(&json(long, "0.1")),
            ),
            (
                vec!["convert", "long_last.tf", "-F", "json"],
                ok(&json("0.1", long)),
            ),
            (vec!["type", "short_last.tf", "/x/0/a"], ok("float\n")),
            (vec!["type", "short_last.tf", "/x/1/a"], ok("float\n")),
        ],
    );
}

/// HCL number literals read as they are spelled: `- 5`, with a space, is
/// the integer -5, and a parenthesised whole float, `(3.0)` or `(- 2.0)`,
/// stays a float, as `-0.0` does.
#[test]
fn hcl_reads_spaced_and_parenthesised_literals_as_spelled() {
    let dir = dir_with(&[("n.tf", "n = - 5\np = (3.0)\nq = (- 2.0)\nz = -0.0\n")]);
    assert_cases(
        &dir,
        &[
            (
                vec!["convert", "n.tf", "-F", "json"],
                ok("{\n  \"n\": -5,\n  \"p\": 3.0,\n  \"q\": -2.0,\n  \"z\": -0.0\n}\n"),
            ),
            (vec!["type", "n.tf", "/n"], ok("int\n")),
            (vec!["type", "n.tf", "/p"], ok("float\n")),
            (vec!["type", "n.tf", "/q"], ok("float\n")),
            (vec!["type", "n.tf", "/z"], ok("float\n")),
        ],
    );
}

/// A byte order mark and the declared encoding must agree (XML 1.0
/// §4.3.3), and `encoding` must be an encoding name: the reader refuses
/// both (exit 3) as the writer does. A UTF-8 BOM is kept as `#xml.bom` and
/// written back; `bom: UTF-8` needs a UTF-8 declaration or none. A null
/// `@attrs` is no attributes, as every other null is no content.
#[test]
fn xml_refuses_a_bom_its_declaration_contradicts_and_keeps_a_utf8_bom() {
    let utf16_le = |text: &str| utf16(text, false);
    let u16_latin = utf16_le("<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?><r/>");
    let mut u16_be_le = vec![0xFE, 0xFF];
    u16_be_le.extend(
        "<?xml version=\"1.0\" encoding=\"UTF-16LE\"?><r/>"
            .encode_utf16()
            .flat_map(u16::to_be_bytes),
    );
    let utf8_bom = |text: &str| [b"\xEF\xBB\xBF".as_slice(), text.as_bytes()].concat();
    let u8_latin = utf8_bom("<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?><r>é</r>");
    let u8_decl = utf8_bom("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<r>é</r>\n");
    let u8_bare = utf8_bom("<r>é</r>\n");
    let dir = dir_with_bytes(&[
        ("u16_latin.xml", &u16_latin),
        ("u16_be_le.xml", &u16_be_le),
        (
            "spaced.xml",
            b"<?xml version=\"1.0\" encoding=\" ISO-8859-1 \"?><r/>",
        ),
        ("u8_latin.xml", &u8_latin),
        ("u8_decl.xml", &u8_decl),
        ("u8_bare.xml", &u8_bare),
        ("u8.yaml", b"\"#xml\": {bom: UTF-8}\nr:\n- {}\n"),
        (
            "u8_latin.yaml",
            b"\"#xml\": {encoding: ISO-8859-1, bom: UTF-8}\nr:\n- {}\n",
        ),
        ("attrs_null.yaml", b"r:\n- \"@attrs\": null\n"),
    ]);
    // Each is placed at the encoding name.
    let unreadable = |file: &str, message: &str| {
        format!("parse error in {file}: {message}\n  --> {file}:1:31\n")
    };
    let u16_latin_err = unreadable(
        "u16_latin.xml",
        "XML parse error: the file begins with a UTF-16LE byte order mark but declares encoding \"ISO-8859-1\"",
    );
    let u16_be_le_err = unreadable(
        "u16_be_le.xml",
        "XML parse error: the file begins with a UTF-16BE byte order mark but declares encoding \"UTF-16LE\"",
    );
    let spaced_err = unreadable(
        "spaced.xml",
        "XML parse error: the XML declaration's encoding \" ISO-8859-1 \" is not an encoding name",
    );
    let u8_latin_err = unreadable(
        "u8_latin.xml",
        "XML parse error: the file begins with a UTF-8 byte order mark but declares encoding \"ISO-8859-1\"",
    );
    let u8_latin_refused = "xml format error: XML write: '#xml' field 'bom' is UTF-8 but the \
                            declared encoding is \"ISO-8859-1\"\n";
    let u8_json = "{\n  \"#xml\": {\n    \"bom\": \"UTF-8\"\n  },\n  \"r\": [\n    {\n      \
                   \"#text\": \"é\"\n    }\n  ]\n}\n";
    assert_byte_cases(
        &dir,
        &[
            ("u16_latin.xml", "json", 3, b"", &u16_latin_err),
            ("u16_be_le.xml", "json", 3, b"", &u16_be_le_err),
            ("spaced.xml", "json", 3, b"", &spaced_err),
            ("u8_latin.xml", "json", 3, b"", &u8_latin_err),
            ("u8_decl.xml", "xml", 0, &u8_decl, ""),
            ("u8_bare.xml", "xml", 0, &u8_bare, ""),
            ("u8_bare.xml", "json", 0, u8_json.as_bytes(), ""),
            ("u8.yaml", "xml", 0, b"\xEF\xBB\xBF<r/>\n", ""),
            ("u8_latin.yaml", "xml", 1, b"", u8_latin_refused),
            ("attrs_null.yaml", "xml", 0, b"<r/>\n", ""),
        ],
    );
}

/// A float on a tie in its 17th digit (`1658206780088562.2` and `…562.3`
/// both read back as the same `f64`) is spelled with the even digit by
/// every writer, as serde_json does. TOML and TOON spelled it with Rust's
/// `Display`, which picks the odd one.
#[test]
fn a_float_on_a_17th_digit_tie_is_spelled_alike_by_every_writer() {
    let dir = dir_with(&[
        ("tie.json", "{\"v\":1658206780088562.2}"),
        ("list.json", "{\"a\":[1658206780088562.2,2.5,\"x\"]}"),
    ]);
    let convert = |file, to| vec!["-q", "convert", file, "-F", to];
    assert_cases(
        &dir,
        &[
            (
                convert("tie.json", "json"),
                ok("{\"v\":1658206780088562.2}\n"),
            ),
            (convert("tie.json", "yaml"), ok("v: 1658206780088562.2\n")),
            (convert("tie.json", "toml"), ok("v = 1658206780088562.2\n")),
            (convert("tie.json", "hcl"), ok("v = 1658206780088562.2\n")),
            (convert("tie.json", "toon"), ok("v: 1658206780088562.2\n")),
            (
                convert("list.json", "toon"),
                ok("a[3]: 1658206780088562.2,2.5,x\n"),
            ),
        ],
    );
}
