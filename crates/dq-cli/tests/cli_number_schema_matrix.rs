//! JSON Schema rules, and the jq paths of a rule (`loc:`, the message
//! template, composite `extract:`, a fix's `match.filter`, `dq test`
//! fixtures), over NaN, infinities and big numbers.
//!
//! `cli_number_surfaces.rs` holds one sanity case per behaviour; this file
//! is the matrix around them: every bound against every non-finite and
//! big value, `multipleOf` / `enum` / `const` / `uniqueItems`, `$ref`
//! paths, a `schema_file`, a TOML document, and where `schema_file` is
//! looked up.

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

/// A temp dir holding `files` (parent directories created).
fn dir_with(files: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    for (name, src) in files {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir fixture");
        std::fs::write(path, src).expect("write fixture");
    }
    dir
}

/// `(exit code, stdout, stderr)` of `dq --no-color <args>` in `cwd`.
fn run_in(cwd: &std::path::Path, args: &[&str]) -> (i32, String, String) {
    let out = dq()
        .current_dir(cwd)
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

/// The exit code of `dq -F json <args>` (a `check` or a `lint`) and the
/// messages of its diagnostics — or, when stdout is no report, the
/// output itself.
fn diagnostics_in(cwd: &std::path::Path, args: &[&str]) -> (i32, Result<Vec<String>, String>) {
    let mut full = vec!["-F", "json"];
    full.extend_from_slice(args);
    let (code, stdout, stderr) = run_in(cwd, &full);
    let messages = serde_json::from_str::<serde_json::Value>(&stdout)
        .ok()
        .and_then(|report| {
            report["diagnostics"].as_array().map(|diagnostics| {
                diagnostics
                    .iter()
                    .map(|d| d["message"].as_str().unwrap_or_default().to_owned())
                    .collect()
            })
        })
        .ok_or(format!("stdout: {stdout}stderr: {stderr}"));
    (code, messages)
}

/// [`diagnostics_in`] of a run that must print a report.
fn messages_in(cwd: &std::path::Path, args: &[&str]) -> (i32, Vec<String>) {
    let (code, messages) = diagnostics_in(cwd, args);
    (
        code,
        messages.unwrap_or_else(|output| panic!("{args:?} exit {code}: {output}")),
    )
}

/// A rule checking `doc` against `{properties: {x: <schema>}}`.
fn rule_with_schema(schema: &str) -> String {
    format!(
        "id: t.num\ndescription: x\nseverity: error\nmatch: {{ format: [yaml, toml] }}\n\
         check:\n  schema: {{ properties: {{ x: {schema} }} }}\n"
    )
}

/// `None` for a pass, `Some(message)` for the one diagnostic expected.
type Outcome<'a> = Option<&'a str>;

/// Check every `(schema, x, outcome)` case — `x` the YAML value of `x` —
/// and fail once, listing every case that differs.
fn assert_schema_cases(cases: &[(&str, &str, Outcome<'_>)]) {
    let mut wrong = Vec::new();
    for (schema, x, outcome) in cases {
        let dir = dir_with(&[
            ("rule.yml", &rule_with_schema(schema)),
            ("doc.yaml", &format!("x: {x}\n")),
        ]);
        let expected = match outcome {
            None => (0, Ok(Vec::new())),
            Some(message) => (4, Ok(vec![(*message).to_owned()])),
        };
        let actual = diagnostics_in(dir.path(), &["check", "--rule", "rule.yml", "doc.yaml"]);
        if actual != expected {
            wrong.push(format!(
                "{schema} on x: {x}\n  expected {expected:?}\n  actual   {actual:?}"
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

#[test]
fn every_bound_orders_infinities_and_refuses_nan() {
    use Some as Fails;
    assert_schema_cases(&[
        (
            "{minimum: 0}",
            ".nan",
            Fails(
                "/properties/x/minimum: .nan has no order, so it is not at least the minimum of 0",
            ),
        ),
        ("{minimum: 0}", ".inf", None),
        (
            "{minimum: 0}",
            "-.inf",
            Fails("/properties/x/minimum: -.inf is less than the minimum of 0"),
        ),
        ("{minimum: 0}", "1e400", None),
        (
            "{minimum: 0}",
            "-1e400",
            Fails("/properties/x/minimum: -1e+400 is less than the minimum of 0"),
        ),
        ("{minimum: 0}", "18446744073709551616", None),
        (
            "{maximum: 10}",
            ".nan",
            Fails(
                "/properties/x/maximum: .nan has no order, so it is not at most the maximum of 10",
            ),
        ),
        (
            "{maximum: 10}",
            ".inf",
            Fails("/properties/x/maximum: .inf is greater than the maximum of 10"),
        ),
        ("{maximum: 10}", "-.inf", None),
        ("{maximum: 10}", "-1e400", None),
        (
            "{maximum: 10}",
            "18446744073709551616",
            Fails("/properties/x/maximum: 18446744073709551616 is greater than the maximum of 10"),
        ),
        ("{maximum: 10}", "-18446744073709551617", None),
        (
            "{exclusiveMinimum: 0}",
            ".nan",
            Fails(
                "/properties/x/exclusiveMinimum: .nan has no order, so it is not above the \
                 minimum of 0",
            ),
        ),
        ("{exclusiveMinimum: 0}", ".inf", None),
        (
            "{exclusiveMinimum: 0}",
            "-.inf",
            Fails(
                "/properties/x/exclusiveMinimum: -.inf is less than or equal to the minimum of 0",
            ),
        ),
        ("{exclusiveMinimum: 0}", "1e400", None),
        (
            "{exclusiveMinimum: 0}",
            "-18446744073709551617",
            Fails(
                "/properties/x/exclusiveMinimum: -18446744073709551617 is less than or equal to \
                 the minimum of 0",
            ),
        ),
        (
            "{exclusiveMaximum: 10}",
            ".nan",
            Fails(
                "/properties/x/exclusiveMaximum: .nan has no order, so it is not below the \
                 maximum of 10",
            ),
        ),
        (
            "{exclusiveMaximum: 10}",
            ".inf",
            Fails(
                "/properties/x/exclusiveMaximum: .inf is greater than or equal to the maximum of 10",
            ),
        ),
        ("{exclusiveMaximum: 10}", "-.inf", None),
        (
            "{exclusiveMaximum: 10}",
            "1e400",
            Fails(
                "/properties/x/exclusiveMaximum: 1e+400 is greater than or equal to the maximum \
                 of 10",
            ),
        ),
        ("{exclusiveMaximum: 10}", "-1e400", None),
    ]);
}

#[test]
fn every_bound_compares_a_big_limit_exactly_at_its_edge() {
    use Some as Fails;
    assert_schema_cases(&[
        (
            "{minimum: 18446744073709551617}",
            "18446744073709551617",
            None,
        ),
        (
            "{minimum: 18446744073709551617}",
            "18446744073709551616",
            Fails(
                "/properties/x/minimum: 18446744073709551616 is less than the minimum of \
                 18446744073709551617",
            ),
        ),
        (
            "{exclusiveMaximum: 18446744073709551616}",
            "18446744073709551616",
            Fails(
                "/properties/x/exclusiveMaximum: 18446744073709551616 is greater than or equal \
                 to the maximum of 18446744073709551616",
            ),
        ),
        (
            "{exclusiveMaximum: 18446744073709551617}",
            "18446744073709551616",
            None,
        ),
        (
            "{exclusiveMinimum: -18446744073709551617}",
            "-18446744073709551617",
            Fails(
                "/properties/x/exclusiveMinimum: -18446744073709551617 is less than or equal to \
                 the minimum of -18446744073709551617",
            ),
        ),
        (
            "{exclusiveMinimum: -18446744073709551618}",
            "-18446744073709551617",
            None,
        ),
        // `1e400` is a bound: every f64 is below it, `.inf` past it.
        (
            "{minimum: 1e400}",
            "1.7976931348623157e308",
            Fails(
                "/properties/x/minimum: 1.7976931348623157e+308 is less than the minimum of 1e400",
            ),
        ),
        ("{minimum: 1e400}", ".inf", None),
        ("{exclusiveMaximum: 1e400}", "1e399", None),
        (
            "{exclusiveMaximum: 1e400}",
            "1.0e+400",
            Fails(
                "/properties/x/exclusiveMaximum: 1.0e+400 is greater than or equal to the maximum \
                 of 1e400",
            ),
        ),
    ]);
}

#[test]
fn multiple_of_refuses_non_finite_numbers_and_divides_big_ones_exactly() {
    use Some as Fails;
    let mut cases = Vec::new();
    for divisor in ["2", "0.5"] {
        for (x, spelled) in [(".nan", ".nan"), (".inf", ".inf"), ("-.inf", "-.inf")] {
            cases.push((
                divisor,
                x,
                Some(format!("{spelled} is not a multiple of {divisor}")),
            ));
        }
        cases.push((divisor, "4", None));
        cases.push((divisor, "18446744073709551616", None));
        // 10^400 is whole and even: x / d is an integer.
        cases.push((divisor, "1e400", None));
        cases.push((
            divisor,
            "0.3",
            Some(format!("0.3 is not a multiple of {divisor}")),
        ));
    }
    cases.push(("0.5", "1.5", None));
    cases.push(("2", "1.5", Some("1.5 is not a multiple of 2".to_owned())));
    let schemas: Vec<String> = cases
        .iter()
        .map(|(divisor, _, _)| format!("{{multipleOf: {divisor}}}"))
        .collect();
    let messages: Vec<Option<String>> = cases
        .iter()
        .map(|(_, _, message)| {
            message
                .as_ref()
                .map(|m| format!("/properties/x/multipleOf: {m}"))
        })
        .collect();
    let cases: Vec<(&str, &str, Outcome<'_>)> = cases
        .iter()
        .zip(&schemas)
        .zip(&messages)
        .map(|(((_, x, _), schema), message)| (schema.as_str(), *x, message.as_deref()))
        .collect();
    assert_schema_cases(&cases);
    // A divisor past 64 bits compiles, and divides what it should.
    assert_schema_cases(&[
        (
            "{multipleOf: 18446744073709551616}",
            "18446744073709551616",
            None,
        ),
        (
            "{multipleOf: 18446744073709551616}",
            ".nan",
            Fails("/properties/x/multipleOf: .nan is not a multiple of 18446744073709551616"),
        ),
    ]);
}

#[test]
fn enum_and_const_tell_a_big_number_from_its_nearest_f64() {
    use Some as Fails;
    assert_schema_cases(&[
        (
            "{const: 18446744073709551617}",
            "18446744073709551617",
            None,
        ),
        (
            "{const: 18446744073709551617}",
            "1.8446744073709552e19",
            Fails("/properties/x/const: 18446744073709551617 was expected"),
        ),
        (
            "{const: 1.8446744073709552e19}",
            "18446744073709551617",
            Fails("/properties/x/const: 1.8446744073709552e+19 was expected"),
        ),
        (
            "{enum: [18446744073709551617, a]}",
            "18446744073709551617",
            None,
        ),
        (
            "{enum: [18446744073709551617, a]}",
            "1.8446744073709552e19",
            Fails(
                "/properties/x/enum: 1.8446744073709552e+19 is not one of 18446744073709551617 or \
                 \"a\"",
            ),
        ),
        (
            "{enum: [1.8446744073709552e19]}",
            "18446744073709551617",
            Fails("/properties/x/enum: 18446744073709551617 is not one of 1.8446744073709552e+19"),
        ),
        // The same number spelled two ways is one value.
        (
            "{const: 18446744073709551616}",
            "18446744073709551616.0",
            None,
        ),
        ("{enum: [1e400, x]}", "1.0e+400", None),
        (
            "{enum: [1e400, x]}",
            "1e399",
            Fails("/properties/x/enum: 1e+399 is not one of 1e+400 or \"x\""),
        ),
        (
            "{const: 0.1}",
            "0.10000000000000000001",
            Fails("/properties/x/const: 0.1 was expected"),
        ),
        (
            "{enum: [0.10000000000000000001]}",
            "0.1",
            Fails("/properties/x/enum: 0.1 is not one of 0.10000000000000000001"),
        ),
    ]);
}

#[test]
fn enum_and_const_match_an_infinity_of_its_sign_and_never_a_nan() {
    use Some as Fails;
    assert_schema_cases(&[
        ("{const: .inf}", ".inf", None),
        (
            "{const: .inf}",
            "-.inf",
            Fails("/properties/x/const: .inf was expected"),
        ),
        (
            "{const: .inf}",
            "1e400",
            Fails("/properties/x/const: .inf was expected"),
        ),
        ("{enum: [-.inf]}", "-.inf", None),
        (
            "{enum: [-.inf]}",
            ".nan",
            Fails("/properties/x/enum: .nan is not one of -.inf"),
        ),
        (
            "{const: .nan}",
            ".nan",
            Fails("/properties/x/const: .nan was expected"),
        ),
        (
            "{enum: [.nan]}",
            ".nan",
            Fails("/properties/x/enum: .nan is not one of .nan"),
        ),
        (
            "{enum: [1, 2, 3, 4, 5]}",
            ".nan",
            Fails("/properties/x/enum: .nan is not one of 1, 2 or 3 other candidates"),
        ),
    ]);
}

#[test]
fn unique_items_counts_numbers_by_their_exact_value() {
    use Some as Fails;
    assert_schema_cases(&[
        (
            "{uniqueItems: true}",
            "[.nan, .nan]",
            Fails("/properties/x/uniqueItems: [.nan,.nan] has non-unique elements"),
        ),
        (
            "{uniqueItems: true}",
            "[.nan, 1, .nan]",
            Fails("/properties/x/uniqueItems: [.nan,1,.nan] has non-unique elements"),
        ),
        (
            "{uniqueItems: true}",
            "[1e400, 1.0e+400]",
            Fails("/properties/x/uniqueItems: [1e+400,1.0e+400] has non-unique elements"),
        ),
        ("{uniqueItems: true}", "[.nan, -.inf, .inf]", None),
        (
            "{uniqueItems: true}",
            "[18446744073709551616, 18446744073709551617]",
            None,
        ),
    ]);
}

#[test]
fn a_non_finite_bound_does_not_compile() {
    for (schema, spelled) in [
        ("{maximum: .inf}", ".inf"),
        ("{minimum: -.inf}", "-.inf"),
        ("{exclusiveMaximum: .nan}", ".nan"),
        ("{multipleOf: .inf}", ".inf"),
    ] {
        let dir = dir_with(&[
            ("rule.yml", &rule_with_schema(schema)),
            ("doc.yaml", "x: 1\n"),
        ]);
        let (code, stdout, stderr) =
            run_in(dir.path(), &["check", "--rule", "rule.yml", "doc.yaml"]);
        assert_eq!(
            (code, stdout.as_str(), stderr.as_str()),
            (
                3,
                "",
                format!(
                    "rule t.num schema failed to compile: {spelled} is not of type \"number\"\n"
                )
                .as_str()
            ),
            "{schema}"
        );
    }
}

#[test]
fn a_ref_keeps_its_path_for_every_number_keyword() {
    let rule = "id: t.ref\ndescription: x\nseverity: error\nmatch: { format: yaml }\n\
                check:\n  schema:\n    $defs:\n      big: { const: 18446744073709551616 }\n      \
                inf: { enum: [.inf, 1] }\n      fin: { maximum: 10 }\n      \
                fin2: { maximum: 10 }\n    properties:\n      \
                a: { $ref: '#/$defs/big' }\n      b: { $ref: '#/$defs/inf' }\n      \
                c: { $ref: '#/$defs/fin' }\n      d: { $ref: '#/$defs/fin' }\n      \
                e: { items: { $ref: '#/$defs/fin' } }\n      \
                f: { allOf: [ { $ref: '#/$defs/fin' }, { $ref: '#/$defs/fin2' } ] }\n";
    let dir = dir_with(&[
        ("rule.yml", rule),
        (
            "bad.yaml",
            "a: 18446744073709551617\nb: .nan\nc: .nan\nd: .nan\ne: [.inf, 3, .nan]\nf: .inf\n",
        ),
        (
            "ok.yaml",
            "a: 18446744073709551616\nb: .inf\nc: -.inf\nd: 10\ne: [-.inf, 3]\nf: 1\n",
        ),
    ]);
    let (code, messages) = messages_in(dir.path(), &["check", "--rule", "rule.yml", "bad.yaml"]);
    assert_eq!(
        (code, messages),
        (
            4,
            [
                "/properties/a/$ref/const: 18446744073709551616 was expected",
                "/properties/b/$ref/enum: .nan is not one of .inf or 1",
                "/properties/c/$ref/maximum: .nan has no order, so it is not at most the maximum \
                 of 10",
                "/properties/d/$ref/maximum: .nan has no order, so it is not at most the maximum \
                 of 10",
                "/properties/e/items/$ref/maximum: .inf is greater than the maximum of 10",
                "/properties/e/items/$ref/maximum: .nan has no order, so it is not at most the \
                 maximum of 10",
                // Two identical errors of one node collapse into the first.
                "/properties/f/allOf/0/$ref/maximum: .inf is greater than the maximum of 10",
            ]
            .map(str::to_owned)
            .to_vec()
        )
    );
    assert_eq!(
        messages_in(dir.path(), &["check", "--rule", "rule.yml", "ok.yaml"]),
        (0, Vec::new())
    );
}

const FILE_RULE: &str = "id: t.file\ndescription: x\nseverity: error\n\
                         match: { format: [yaml, toml] }\ncheck:\n  schema_file: s.json\n";

#[test]
fn a_schema_file_reads_non_finite_and_exact_numbers() {
    let schema = r##"{
      "$defs": {"big": {"const": 18446744073709551616}, "fin": {"maximum": 10}},
      "properties": {
        "max": {"maximum": 10},
        "xmin": {"exclusiveMinimum": 0},
        "num": {"type": "number"},
        "int": {"type": "integer"},
        "mul": {"multipleOf": 0.5},
        "en": {"enum": [18446744073709551617, "a"]},
        "ref": {"$ref": "#/$defs/big"},
        "fin": {"$ref": "#/$defs/fin"},
        "uniq": {"uniqueItems": true}
      }
    }"##;
    let dir = dir_with(&[
        ("rule.yml", FILE_RULE),
        ("s.json", schema),
        (
            "bad.yaml",
            "max: .nan\nxmin: -.inf\nnum: x\nint: .inf\nmul: .nan\n\
             en: 1.8446744073709552e19\nref: 18446744073709551617\nfin: .inf\n\
             uniq: [.nan, .nan]\n",
        ),
        (
            "ok.yaml",
            "max: -.inf\nxmin: .inf\nnum: .nan\nint: 1e400\nmul: 1.5\n\
             en: 18446744073709551617\nref: 18446744073709551616\nfin: -.inf\n\
             uniq: [.nan, .inf]\n",
        ),
    ]);
    let (code, messages) = messages_in(dir.path(), &["check", "--rule", "rule.yml", "bad.yaml"]);
    assert_eq!(
        (code, messages),
        (
            4,
            [
                "/properties/max/maximum: .nan has no order, so it is not at most the maximum of \
                 10",
                "/properties/xmin/exclusiveMinimum: -.inf is less than or equal to the minimum of \
                 0",
                "/properties/num/type: \"x\" is not of type \"number\"",
                "/properties/int/type: .inf is not of type \"integer\"",
                "/properties/mul/multipleOf: .nan is not a multiple of 0.5",
                "/properties/en/enum: 1.8446744073709552e+19 is not one of 18446744073709551617 \
                 or \"a\"",
                "/properties/ref/$ref/const: 18446744073709551616 was expected",
                "/properties/fin/$ref/maximum: .inf is greater than the maximum of 10",
                "/properties/uniq/uniqueItems: [.nan,.nan] has non-unique elements",
            ]
            .map(str::to_owned)
            .to_vec()
        )
    );
    assert_eq!(
        messages_in(dir.path(), &["check", "--rule", "rule.yml", "ok.yaml"]),
        (0, Vec::new())
    );
}

#[test]
fn a_toml_nan_and_infinity_are_numbers_to_a_schema() {
    let rule = "id: t.toml\ndescription: x\nseverity: error\nmatch: { format: toml }\n\
                check:\n  schema:\n    properties:\n      n: { type: number, maximum: 10 }\n      \
                i: { maximum: 10 }\n      s: { type: string }\n";
    let dir = dir_with(&[
        ("rule.yml", rule),
        ("bad.toml", "n = nan\ni = inf\ns = -inf\n"),
        ("ok.toml", "n = -inf\ni = 1.5\ns = \"nan\"\n"),
    ]);
    let (code, messages) = messages_in(dir.path(), &["check", "--rule", "rule.yml", "bad.toml"]);
    assert_eq!(
        (code, messages),
        (
            4,
            [
                "/properties/n/maximum: .nan has no order, so it is not at most the maximum of 10",
                "/properties/i/maximum: .inf is greater than the maximum of 10",
                "/properties/s/type: -.inf is not of type \"string\"",
            ]
            .map(str::to_owned)
            .to_vec()
        )
    );
    assert_eq!(
        messages_in(dir.path(), &["check", "--rule", "rule.yml", "ok.toml"]),
        (0, Vec::new())
    );
}

#[test]
fn rule_files_holding_big_numbers_parse() {
    // A stream of rules, the second with a big number in its schema, and
    // a composite whose nested rule holds one.
    let stream = "id: t.first\ndescription: x\nseverity: error\nmatch: { format: yaml }\n\
                  check:\n  jq: 'select(.s == \"bad\") | .s'\n  message: 's is bad'\n---\n\
                  id: t.second\ndescription: x\nseverity: error\nmatch: { format: yaml }\n\
                  check:\n  schema: { properties: { n: { const: 18446744073709551616 } } }\n";
    let composite = "id: t.outer\ndescription: x\nseverity: error\nmatch: { format: yaml }\n\
                     check:\n  extract: '[{value: .inner, format: \"json\", anchor: \"/inner\"}]'\n  \
                     message: inner\n  nested:\n    id: t.inner\n    description: y\n    \
                     severity: error\n    match: { format: json }\n    check:\n      \
                     schema: { properties: { n: { maximum: 18446744073709551616, \
                     enum: [18446744073709551616, 340282366920938463463374607431768211456] } } }\n";
    let dir = dir_with(&[
        (".dq/rules/two.yml", stream),
        ("composite.yml", composite),
        ("ok.yaml", "s: ok\nn: 18446744073709551616\n"),
        ("bad.yaml", "s: bad\nn: 18446744073709551617\n"),
        ("inner-ok.yaml", "inner: '{\"n\": 18446744073709551616}'\n"),
        (
            "inner-bad.yaml",
            "inner: '{\"n\": 340282366920938463463374607431768211456}'\n",
        ),
    ]);
    assert_eq!(
        messages_in(dir.path(), &["lint", "ok.yaml"]),
        (0, Vec::new())
    );
    assert_eq!(
        messages_in(dir.path(), &["lint", "bad.yaml"]),
        (
            4,
            vec![
                "s is bad".to_owned(),
                "/properties/n/const: 18446744073709551616 was expected".to_owned(),
            ]
        )
    );
    let check = |file| vec!["check", "--rule", "composite.yml", file];
    assert_eq!(
        messages_in(dir.path(), &check("inner-ok.yaml")),
        (0, Vec::new())
    );
    assert_eq!(
        messages_in(dir.path(), &check("inner-bad.yaml")),
        (
            4,
            vec![
                "inner: /properties/n/maximum: 340282366920938463463374607431768211456 is greater \
                 than the maximum of 18446744073709551616"
                    .to_owned()
            ]
        )
    );
}

#[test]
fn a_schema_file_is_read_next_to_the_rule_file() {
    let decoy = r#"{"properties": {"x": {"maximum": 100}}}"#;
    let schema = r#"{"properties": {"x": {"maximum": 10}}}"#;
    let dir = dir_with(&[
        ("rule.yml", FILE_RULE),
        ("s.json", schema),
        ("doc.yaml", "x: 20\n"),
        ("rules/rule.yml", FILE_RULE),
        ("rules/s.json", schema),
        ("other/rule.yml", FILE_RULE),
        ("other/s.json", decoy),
    ]);
    let fails = (
        4,
        vec!["/properties/x/maximum: 20 is greater than the maximum of 10".to_owned()],
    );
    let root = dir.path();
    // A bare rule file name, from its own directory.
    assert_eq!(
        messages_in(root, &["check", "--rule", "rule.yml", "doc.yaml"]),
        fails
    );
    assert_eq!(
        messages_in(
            &root.join("rules"),
            &["check", "--rule", "rule.yml", "../doc.yaml"]
        ),
        fails
    );
    // A rule file elsewhere reads the schema next to it, not in the cwd.
    assert_eq!(
        messages_in(root, &["check", "--rule", "rules/rule.yml", "doc.yaml"]),
        fails
    );
    assert_eq!(
        messages_in(root, &["check", "--rule", "other/rule.yml", "doc.yaml"]),
        (0, Vec::new())
    );
}

#[test]
fn loc_expressions_see_a_nan_violation_as_a_number() {
    let jq_rule = "id: t.loc\ndescription: x\nseverity: error\nmatch: { format: yaml }\n\
                   check:\n  jq: '.l[] | select(. != 1)'\n  message: 'bad {{ . }}'\n\
                   loc:\n  pointer: 'if isnan then \"/l/2\" else \"/l/1\" end'\n  \
                   file: 'if isnan then \"nan.yaml\" else \"finite.yaml\" end'\n";
    let line_rule = "id: t.line\ndescription: x\nseverity: error\nmatch: { format: yaml }\n\
                     check:\n  jq: '.l[] | select(. != 1)'\n  message: 'bad {{ . }}'\n\
                     loc:\n  line: 'if isnan then 4 else 3 end'\n";
    let schema_rule = "id: t.sloc\ndescription: x\nseverity: error\nmatch: { format: yaml }\n\
                       check:\n  schema: { properties: { l: { items: { maximum: 10 } } } }\n\
                       loc:\n  pointer: 'if isnan then \"/l/2\" else \"/l/0\" end'\n  \
                       file: 'if isnan then \"nan.yaml\" else \"finite.yaml\" end'\n";
    let dir = dir_with(&[
        ("jq.yml", jq_rule),
        ("line.yml", line_rule),
        ("schema.yml", schema_rule),
        ("doc.yaml", "l:\n  - 1\n  - 20\n  - .nan\n"),
    ]);
    /// `(file, line, col, message)` of every diagnostic.
    fn located(cwd: &std::path::Path, rule: &str) -> (i32, Vec<(String, u64, u64, String)>) {
        let (code, stdout, stderr) =
            run_in(cwd, &["-F", "json", "check", "--rule", rule, "doc.yaml"]);
        let report: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|err| panic!("{err}: {stderr}"));
        let diagnostics = report["diagnostics"]
            .as_array()
            .expect("diagnostics")
            .iter()
            .map(|d| {
                (
                    d["path"].as_str().expect("path").to_owned(),
                    d["line"].as_u64().expect("line"),
                    d["col"].as_u64().expect("col"),
                    d["message"].as_str().expect("message").to_owned(),
                )
            })
            .collect();
        (code, diagnostics)
    }
    let at =
        |file: &str, line, col, message: &str| (file.to_owned(), line, col, message.to_owned());
    assert_eq!(
        located(dir.path(), "jq.yml"),
        (
            4,
            vec![
                at("finite.yaml", 3, 5, "bad 20"),
                at("nan.yaml", 4, 5, "bad .nan"),
            ]
        )
    );
    assert_eq!(
        located(dir.path(), "line.yml"),
        (
            4,
            vec![
                at("doc.yaml", 3, 1, "bad 20"),
                at("doc.yaml", 4, 1, "bad .nan")
            ]
        )
    );
    assert_eq!(
        located(dir.path(), "schema.yml"),
        (
            4,
            vec![
                at(
                    "finite.yaml",
                    2,
                    5,
                    "/properties/l/items/maximum: 20 is greater than the maximum of 10"
                ),
                at(
                    "nan.yaml",
                    4,
                    5,
                    "/properties/l/items/maximum: .nan has no order, so it is not at most the \
                     maximum of 10"
                ),
            ]
        )
    );
}

#[test]
fn a_message_template_spells_nested_non_finite_and_big_numbers() {
    let rule = "id: t.tpl\ndescription: x\nseverity: error\nmatch: { format: yaml }\n\
                check:\n  jq: 'select(.m.a | isnan) | .m'\n  \
                message: 'm is {{ . }}; a is {{ .a }}; b1 is {{ .b.1 }}; d is {{ .c.d }}'\n";
    let dir = dir_with(&[
        ("rule.yml", rule),
        (
            "nan.yaml",
            "m: {a: .nan, b: [.inf, -.inf, 18446744073709551616, 1e400], c: {d: .nan}}\n",
        ),
        ("finite.yaml", "m: {a: 1, b: [], c: {}}\n"),
    ]);
    assert_eq!(
        messages_in(dir.path(), &["check", "--rule", "rule.yml", "nan.yaml"]),
        (
            4,
            vec![
                "m is {\"a\":.nan,\"b\":[.inf,-.inf,18446744073709551616,1e+400],\"c\":{\"d\":.nan}}; \
                 a is .nan; b1 is -.inf; d is .nan"
                    .to_owned()
            ]
        )
    );
    assert_eq!(
        messages_in(dir.path(), &["check", "--rule", "rule.yml", "finite.yaml"]),
        (0, Vec::new())
    );
}

#[test]
fn a_composite_extract_item_whose_value_is_nan_is_malformed_and_says_why() {
    let rule = "id: t.comp\ndescription: x\nseverity: error\nmatch: { format: yaml }\n\
                check:\n  extract: '[{value: .x, format: \"yaml\", anchor: \"/x\"}]'\n  \
                message: m\n  nested:\n    id: t.inner\n    description: y\n    \
                severity: error\n    match: { format: yaml }\n    check:\n      \
                schema: { type: string }\n";
    let dir = dir_with(&[
        ("rule.yml", rule),
        ("nan.yaml", "x: .nan\n"),
        // A string value is reparsed: `v: 1` is a mapping, not a string.
        ("text.yaml", "x: 'v: 1'\n"),
        ("str.yaml", "x: 'hi'\n"),
    ]);
    let check = |file| vec!["check", "--rule", "rule.yml", file];
    assert_eq!(
        messages_in(dir.path(), &check("nan.yaml")),
        (
            4,
            vec!["composite extract item field `value` must be a string, got a number".to_owned()]
        )
    );
    assert_eq!(
        messages_in(dir.path(), &check("text.yaml")),
        (
            4,
            vec!["m: /type: {\"v\":1} is not of type \"string\"".to_owned()]
        )
    );
    assert_eq!(messages_in(dir.path(), &check("str.yaml")), (0, Vec::new()));
}

#[test]
fn a_fix_runs_where_its_filter_returns_a_nan() {
    // jq truthiness: a NaN holds, `false` / `null` do not.
    let rule = "id: t.fix\ndescription: x\nseverity: warn\n\
                match:\n  format: yaml\n  filter: '.x'\n\
                check:\n  jq: 'select(.z != 2) | .'\n  message: 'z is not 2'\nfix:\n  jq: '.z = 2'\n";
    let dir = dir_with(&[
        (".dq/rules/fix.yml", rule),
        ("nan.yaml", "x: .nan\nz: 1\n"),
        ("false.yaml", "x: false\nz: 1\n"),
        ("null.yaml", "x: null\nz: 1\n"),
    ]);
    let (code, stdout, stderr) = run_in(
        dir.path(),
        &["-i", "fix", "nan.yaml", "false.yaml", "null.yaml"],
    );
    assert_eq!(
        (code, stdout.as_str()),
        (0, "Modified: 1, Skipped: 2, Failed: 0\n"),
        "{stderr}"
    );
    let read = |name| std::fs::read_to_string(dir.path().join(name)).expect("read");
    assert_eq!(read("nan.yaml"), "x: .nan\nz: 2\n");
    assert_eq!(read("false.yaml"), "x: false\nz: 1\n");
    assert_eq!(read("null.yaml"), "x: null\nz: 1\n");
}

#[test]
fn dq_test_hands_markdown_hcl_and_dockerfile_values_to_the_rule() {
    let markdown_rule = "id: t.fm\ndescription: x\nseverity: error\nmatch: { format: markdown }\n\
                         check:\n  jq: '.frontmatter.value | select(.x | isnan) | .b'\n  \
                         message: 'x is NaN, b is {{ . }}'\n";
    let markdown_cases = "tests:\n  - name: nan frontmatter\n    input: |\n      ---\n      \
                          x: .nan\n      b: 18446744073709551616\n      ---\n      # T\n    \
                          expected:\n      violations:\n        - rule: t.fm\n          \
                          message_contains: \"x is NaN, b is 18446744073709551616\"\n  \
                          - name: finite frontmatter\n    input: |\n      ---\n      x: 1\n      \
                          b: 2\n      ---\n      # T\n    expected:\n      violations: []\n";
    let hcl_rule = "id: t.hcl\ndescription: x\nseverity: error\nmatch: { format: hcl }\n\
                    check:\n  jq: 'select(.port > 1024) | .port'\n  message: 'port {{ . }}'\n";
    let hcl_cases = "tests:\n  - name: high port\n    input: |\n      port = 8080\n    \
                     expected:\n      violations:\n        - rule: t.hcl\n          \
                     message_contains: \"port 8080\"\n  - name: low port\n    input: |\n      \
                     port = 80\n    expected:\n      violations: []\n";
    let docker_rule = "id: t.df\ndescription: x\nseverity: error\nmatch: { format: dockerfile }\n\
                       check:\n  jq: '.[] | select(.instruction == \"RUN\" and .line > 1)'\n  \
                       message: 'RUN at {{ .line }}'\n";
    let docker_cases = "tests:\n  - name: run after from\n    input: |\n      FROM alpine:3\n      \
                        RUN echo hi\n    expected:\n      violations:\n        - rule: t.df\n          \
                        message_contains: \"RUN at 2\"\n  - name: no run\n    input: |\n      \
                        FROM alpine:3\n    expected:\n      violations: []\n";
    let dir = dir_with(&[
        ("md/fm.yml", markdown_rule),
        ("md/fm.test.yml", markdown_cases),
        ("hcl/port.yml", hcl_rule),
        ("hcl/port.test.yml", hcl_cases),
        ("df/run.yml", docker_rule),
        ("df/run.test.yml", docker_cases),
    ]);
    for (rules, file) in [("md", "fm"), ("hcl", "port"), ("df", "run")] {
        let (code, stdout, stderr) = run_in(dir.path(), &["test", rules]);
        assert_eq!(code, 0, "{rules}: {stdout}{stderr}");
        assert!(
            stdout.ends_with("summary: 2 passed, 0 failed, 0 errored, 2 total\n"),
            "{rules}: {stdout}"
        );
        assert!(
            stdout.contains(&format!("ok {rules}/{file}.test.yml")),
            "{stdout}"
        );
    }
}

#[test]
fn multiple_of_spells_a_big_divisor_as_the_schema_does() {
    use Some as Fails;
    assert_schema_cases(&[(
        "{multipleOf: 18446744073709551616}",
        "4",
        Fails("/properties/x/multipleOf: 4 is not a multiple of 18446744073709551616"),
    )]);
}

#[test]
fn multiple_of_a_divisor_past_f64_holds_no_finite_number_but_zero() {
    use Some as Fails;
    assert_schema_cases(&[
        ("{multipleOf: 1e400}", "0", None),
        (
            "{multipleOf: 1e400}",
            "4",
            Fails("/properties/x/multipleOf: 4 is not a multiple of 1e400"),
        ),
        (
            "{multipleOf: 1e400}",
            "18446744073709551616",
            Fails("/properties/x/multipleOf: 18446744073709551616 is not a multiple of 1e400"),
        ),
    ]);
}

#[test]
fn multiple_of_a_divisor_below_f64_compiles_and_divides_exactly() {
    use Some as Fails;
    assert_schema_cases(&[
        ("{multipleOf: 1e-400}", "2e-400", None),
        ("{multipleOf: 1e-400}", "0.3", None),
        (
            "{multipleOf: 1e-400}",
            "1.5e-400",
            Fails("/properties/x/multipleOf: 1.5e-400 is not a multiple of 1e-400"),
        ),
        ("{multipleOf: 0.1}", "0.3", None),
    ]);
}

#[test]
fn a_schema_file_multiple_of_past_f64_divides_exactly() {
    let rule = "id: t.file\ndescription: x\nseverity: error\nmatch: { format: yaml }\n\
                check:\n  schema_file: s.json\n";
    let schema =
        r#"{"properties": {"big": {"multipleOf": 1e400}, "tiny": {"multipleOf": 1e-400}}}"#;
    let dir = dir_with(&[
        ("rule.yml", rule),
        ("s.json", schema),
        ("ok.yaml", "big: 0\ntiny: 3e-400\n"),
        ("bad.yaml", "big: 4\ntiny: 2.5e-400\n"),
    ]);
    let check = |file| vec!["check", "--rule", "rule.yml", file];
    assert_eq!(messages_in(dir.path(), &check("ok.yaml")), (0, Vec::new()));
    assert_eq!(
        messages_in(dir.path(), &check("bad.yaml")),
        (
            4,
            vec![
                "/properties/big/multipleOf: 4 is not a multiple of 1e400".to_owned(),
                "/properties/tiny/multipleOf: 2.5e-400 is not a multiple of 1e-400".to_owned(),
            ]
        )
    );
}

#[test]
fn dq_test_reads_yaml_and_json_inputs_as_check_does() {
    let rule = "id: t.nan\ndescription: x\nseverity: error\nmatch: { format: [yaml, json] }\n\
                check:\n  jq: 'select((.x | isnan) or .b == 18446744073709551617) | .b'\n  \
                message: 'b is {{ . }}'\n";
    let cases = "tests:\n  - name: yaml nan\n    input: |\n      x: .nan\n      b: 1\n    \
                 expected:\n      violations:\n        - rule: t.nan\n          \
                 message_contains: \"b is 1\"\n  - name: json big\n    format: json\n    \
                 input: '{\"x\": 1, \"b\": 18446744073709551617}'\n    expected:\n      \
                 violations:\n        - rule: t.nan\n          \
                 message_contains: \"b is 18446744073709551617\"\n  - name: finite\n    \
                 input: |\n      x: 1\n      b: 2\n    expected:\n      violations: []\n";
    let dir = dir_with(&[("r/nan.yml", rule), ("r/nan.test.yml", cases)]);
    let (code, stdout, stderr) = run_in(dir.path(), &["test", "r"]);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(
        stdout.ends_with("summary: 3 passed, 0 failed, 0 errored, 3 total\n"),
        "{stdout}"
    );
}

/// `multipleOf: 1e-400` reaches the validator as a stand-in: `multipleOf: 1`
/// next to a hidden keyword holding the literal. The rule author sees only
/// what they wrote: messages and schema paths name `1e-400` where it
/// stands — under `$ref`, `then`, `items`, `prefixItems`,
/// `patternProperties`, `dependentSchemas` — a `const` / `enum` / `default`
/// holding a `multipleOf` object is data compared as written, and a
/// property named `multipleOf` or `const` is a property. Before, every one of these
/// schemas failed to compile (exit 3).
#[test]
fn a_divisor_below_f64_shows_the_rule_author_only_what_they_wrote() {
    use Some as Fails;
    assert_schema_cases(&[
        (
            "{const: {multipleOf: 1e-400}}",
            "{multipleOf: 1e-400}",
            None,
        ),
        (
            "{const: {multipleOf: 1e-400}}",
            "{multipleOf: 2e-400}",
            Fails("/properties/x/const: {\"multipleOf\":1e-400} was expected"),
        ),
        (
            "{enum: [{multipleOf: 1e-400}, 1]}",
            "{multipleOf: 1e-400}",
            None,
        ),
        (
            "{enum: [{multipleOf: 1e-400}]}",
            "{multipleOf: 1}",
            Fails("/properties/x/enum: {\"multipleOf\":1} is not one of {\"multipleOf\":1e-400}"),
        ),
        (
            "{default: {multipleOf: 1e-400}, examples: [{multipleOf: 1e-400}], \
             multipleOf: 1e-400}",
            "1.5e-400",
            Fails("/properties/x/multipleOf: 1.5e-400 is not a multiple of 1e-400"),
        ),
        (
            "{properties: {multipleOf: {multipleOf: 1e-400}}, required: [multipleOf]}",
            "{multipleOf: 3e-400}",
            None,
        ),
        (
            "{properties: {multipleOf: {multipleOf: 1e-400}}}",
            "{multipleOf: 1.5e-400}",
            Fails(
                "/properties/x/properties/multipleOf/multipleOf: 1.5e-400 is not a multiple of \
                 1e-400",
            ),
        ),
        (
            "{additionalProperties: false, properties: {multipleOf: {multipleOf: 1e-400}}}",
            "{multipleOf: 3e-400}",
            None,
        ),
        (
            "{properties: {const: {multipleOf: 1e-400}, enum: {multipleOf: 1e-400}}}",
            "{const: 1.5e-400, enum: 3e-400}",
            Fails(
                "/properties/x/properties/const/multipleOf: 1.5e-400 is not a multiple of 1e-400",
            ),
        ),
        (
            "{$defs: {t: {multipleOf: 1e-400}}, $ref: \"#/properties/x/$defs/t\"}",
            "1.5e-400",
            Fails("/properties/x/$ref/multipleOf: 1.5e-400 is not a multiple of 1e-400"),
        ),
        (
            "{if: {type: number}, then: {multipleOf: 1e-400}}",
            "1.5e-400",
            Fails("/properties/x/then/multipleOf: 1.5e-400 is not a multiple of 1e-400"),
        ),
        (
            "{items: {multipleOf: 1e-400}}",
            "[2e-400, 1.5e-400]",
            Fails("/properties/x/items/multipleOf: 1.5e-400 is not a multiple of 1e-400"),
        ),
        (
            "{prefixItems: [{multipleOf: 1e-400}]}",
            "[1.5e-400]",
            Fails("/properties/x/prefixItems/0/multipleOf: 1.5e-400 is not a multiple of 1e-400"),
        ),
        (
            "{patternProperties: {'^m': {multipleOf: 1e-400}}}",
            "{m: 1.5e-400}",
            Fails(
                "/properties/x/patternProperties/^m/multipleOf: 1.5e-400 is not a multiple of \
                 1e-400",
            ),
        ),
        (
            "{dependentSchemas: {k: {properties: {m: {multipleOf: 1e-400}}}}}",
            "{k: 1, m: 1.5e-400}",
            Fails(
                "/properties/x/dependentSchemas/k/properties/m/multipleOf: 1.5e-400 is not a \
                 multiple of 1e-400",
            ),
        ),
        (
            "{anyOf: [{multipleOf: 1e-400}, {type: string}]}",
            "1.5e-400",
            Fails(
                "/properties/x/anyOf: 1.5e-400 is not valid under any of the schemas listed in \
                 the 'anyOf' keyword",
            ),
        ),
    ]);
}

/// `dq test` reads each fixture input with dq's reader for its format: a
/// YAML stream of several documents is an error naming the count, and TOML
/// is still not a fixture format. Both are errored cases (exit 4), the
/// others in the file still run.
#[test]
fn dq_test_errors_on_a_yaml_stream_and_a_toml_input() {
    let rule = "id: t.nan\ndescription: x\nseverity: error\nmatch: { format: [yaml, toml] }\n\
                check:\n  jq: 'select(.x | isnan) | .x'\n  message: 'x is {{ . }}'\n";
    let cases = "tests:\n  - name: stream\n    input: |\n      x: 1\n      ---\n      \
                 x: .nan\n    expected:\n      violations: []\n  - name: toml\n    \
                 format: toml\n    input: |\n      x = nan\n    expected:\n      \
                 violations: []\n  - name: nan\n    input: |\n      x: .NaN\n    expected:\n      \
                 violations:\n        - rule: t.nan\n          message_contains: \"x is .nan\"\n";
    let dir = dir_with(&[("r/nan.yml", rule), ("r/nan.test.yml", cases)]);
    let (code, stdout, stderr) = run_in(dir.path(), &["test", "r"]);
    assert_eq!(
        (code, stdout.as_str(), stderr.as_str()),
        (
            4,
            "ERROR r/nan.test.yml: stream (yaml parse error: a fixture input holds one document, \
             this one holds 2)\n\
             ERROR r/nan.test.yml: toml (format not yet supported in test runner: toml \
             (supported: yaml, json, markdown, hcl, dockerfile; add it to SUPPORTED in \
             test_runner::parse_input if dq_core::by_name reads it))\n\
             ok r/nan.test.yml: nan\n\
             summary: 1 passed, 0 failed, 2 errored, 3 total\n",
            "lint reported 2 error-severity diagnostic(s)\n"
        )
    );
}

/// A composite `extract` item field that is there but not a string is
/// named with its type — before, any of them read as "missing field"; a
/// field that is absent still is.
#[test]
fn a_composite_extract_item_names_the_field_and_the_type_it_refuses() {
    let rule = |extract: &str| {
        format!(
            "id: t.comp\ndescription: x\nseverity: error\nmatch: {{ format: yaml }}\n\
             check:\n  extract: '{extract}'\n  message: m\n  nested:\n    id: t.inner\n    \
             description: y\n    severity: error\n    match: {{ format: yaml }}\n    check:\n      \
             schema: {{ type: string }}\n"
        )
    };
    let cases = [
        (
            "[{value: .x, format: 1, anchor: \"/x\"}]",
            "field `format` must be a string, got a number",
        ),
        (
            "[{value: .x, format: null, anchor: \"/x\"}]",
            "field `format` must be a string, got null",
        ),
        (
            "[{value: .x, format: \"yaml\", anchor: .n}]",
            "field `anchor` must be a string, got a number",
        ),
        (
            "[{value: .x, format: \"yaml\", anchor: [1]}]",
            "field `anchor` must be a string, got an array",
        ),
        (
            "[{value: .b, format: \"yaml\", anchor: \"/x\"}]",
            "field `value` must be a string, got a number",
        ),
        (
            "[{value: true, format: \"yaml\", anchor: \"/x\"}]",
            "field `value` must be a string, got a bool",
        ),
        (
            "[{value: {a: 1}, format: \"yaml\", anchor: \"/x\"}]",
            "field `value` must be a string, got an object",
        ),
        ("[{value: .x, format: \"yaml\"}]", "missing field `anchor`"),
        (
            "[{format: \"yaml\", anchor: \"/x\"}]",
            "missing field `value`",
        ),
    ];
    let mut wrong = Vec::new();
    for (extract, message) in cases {
        let dir = dir_with(&[
            ("rule.yml", &rule(extract)),
            ("c.yaml", "x: hi\nn: .nan\nb: 18446744073709551617\n"),
        ]);
        let expected = (4, vec![format!("composite extract item {message}")]);
        let actual = messages_in(dir.path(), &["check", "--rule", "rule.yml", "c.yaml"]);
        if actual != expected {
            wrong.push(format!(
                "{extract}\n  expected {expected:?}\n  actual   {actual:?}"
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

#[test]
fn a_divisor_below_f64_is_spelled_as_written_where_a_message_prints_the_schema() {
    use Some as Fails;
    assert_schema_cases(&[
        (
            "{not: {multipleOf: 1e-400}}",
            "2e-400",
            Fails("/properties/x/not: {\"multipleOf\":1e-400} is not allowed for 2e-400"),
        ),
        (
            "{not: {properties: {m: {multipleOf: 3e-400}}}}",
            "{m: 6e-400}",
            Fails(
                "/properties/x/not: {\"properties\":{\"m\":{\"multipleOf\":3e-400}}} is not allowed for {\"m\":6e-400}",
            ),
        ),
    ]);
}

/// A float on a tie in its 17th digit compares by the digits serde_json
/// spells it with (the even one): `1658206780088562.2` is within a maximum
/// of `1658206780088562.25`, and `662936471232937.2` a multiple of `0.2`.
/// Rust's `{:e}` spelled them `…562.3` and `…937.3`.
#[test]
fn a_float_on_a_17th_digit_tie_compares_by_its_even_spelling() {
    use Some as Fails;
    assert_schema_cases(&[
        ("{maximum: 1658206780088562.25}", "1658206780088562.2", None),
        ("{multipleOf: 0.2}", "662936471232937.2", None),
        ("{const: 1658206780088562.2}", "1658206780088562.2", None),
        (
            "{maximum: 1658206780088562.1}",
            "1658206780088562.2",
            Fails(
                "/properties/x/maximum: 1658206780088562.2 is greater than the maximum of \
                 1658206780088562.1",
            ),
        ),
    ]);
}

/// A limit in a message is spelled as the rule writes it, its source
/// text — the rule's numbers are held exactly, so `1e-11` for
/// `0.00000000001` would misquote the rule, and so would serde_json's
/// `1e+2` for `1e2`. Text that is no JSON number (`+5`) keeps the number's
/// own spelling. Before, every limit came out in its shortest spelling,
/// then an exponent as serde_json reads it.
#[test]
fn a_limit_is_quoted_in_a_message_as_the_rule_spells_it() {
    use Some as Fails;
    assert_schema_cases(&[
        (
            "{multipleOf: 0.00000000001}",
            "0.000000000015",
            Fails("/properties/x/multipleOf: 1.5e-11 is not a multiple of 0.00000000001"),
        ),
        (
            "{multipleOf: 2.50}",
            "1",
            Fails("/properties/x/multipleOf: 1 is not a multiple of 2.50"),
        ),
        (
            "{maximum: 0.00000000001}",
            "1",
            Fails("/properties/x/maximum: 1 is greater than the maximum of 0.00000000001"),
        ),
        (
            "{exclusiveMinimum: 1e2}",
            "100",
            Fails(
                "/properties/x/exclusiveMinimum: 100 is less than or equal to the minimum of 1e2",
            ),
        ),
        (
            "{maximum: 1E-1}",
            "1",
            Fails("/properties/x/maximum: 1 is greater than the maximum of 1E-1"),
        ),
        (
            "{multipleOf: 25e-1}",
            "1",
            Fails("/properties/x/multipleOf: 1 is not a multiple of 25e-1"),
        ),
        (
            "{minimum: +5}",
            "1",
            Fails("/properties/x/minimum: 1 is less than the minimum of 5"),
        ),
    ]);
}

/// `dq lint` quotes a limit as the rule file spells it too, in every
/// document of a rule stream under `.dq/rules/`. Before, `0.00000000001`
/// came out as `1e-11` and `2.50` as `2.5`.
#[test]
fn lint_quotes_a_limit_as_each_rule_of_a_stream_spells_it() {
    let stream = "id: t.a\ndescription: x\nseverity: error\nmatch: { format: yaml }\n\
                  check:\n  schema: { properties: { w: { maximum: 0.00000000001 } } }\n---\n\
                  id: t.b\ndescription: x\nseverity: error\nmatch: { format: yaml }\n\
                  check:\n  schema: { properties: { x: { multipleOf: 0.00000000001 }, \
                  y: { maximum: 2.50 } } }\n";
    let dir = dir_with(&[
        (".dq/rules/limits.yml", stream),
        ("doc.yaml", "w: 1\nx: 0.000000000015\ny: 3\n"),
        ("ok.yaml", "w: 0.00000000001\nx: 0.00000000002\ny: 2.5\n"),
    ]);
    assert_eq!(
        messages_in(dir.path(), &["lint", "ok.yaml"]),
        (0, Vec::new())
    );
    assert_eq!(
        messages_in(dir.path(), &["lint", "doc.yaml"]),
        (
            4,
            vec![
                "/properties/w/maximum: 1 is greater than the maximum of 0.00000000001".to_owned(),
                "/properties/x/multipleOf: 1.5e-11 is not a multiple of 0.00000000001".to_owned(),
                "/properties/y/maximum: 3 is greater than the maximum of 2.50".to_owned(),
            ]
        )
    );
}

/// A `schema_file` limit is quoted as the file spells it too, through a
/// `$ref` and in a schema a message prints (`not`) as well. The spelling
/// travels next to the keyword, never into data (`const`). Before, an
/// exponent came out as serde_json reads it, `1e2` as `1e+2`.
#[test]
fn a_schema_file_limit_is_quoted_as_the_file_spells_it() {
    let schema = r##"{
      "$defs": {"m": {"minimum": 5e0}},
      "properties": {
        "a": {"maximum": 1e2},
        "b": {"multipleOf": 2.50},
        "c": {"exclusiveMaximum": 1E-1},
        "d": {"$ref": "#/$defs/m"},
        "e": {"const": {"maximum": 1e2}},
        "f": {"not": {"maximum": 1e2}}
      }
    }"##;
    let dir = dir_with(&[
        ("rule.yml", FILE_RULE),
        ("s.json", schema),
        (
            "bad.yaml",
            "a: 101
b: 1
c: 1
d: 4
f: 5
",
        ),
        (
            "ok.yaml",
            "a: 100
b: 5
c: 0.05
d: 5
e: {maximum: 100}
f: 101
",
        ),
    ]);
    assert_eq!(
        messages_in(dir.path(), &["check", "--rule", "rule.yml", "ok.yaml"]),
        (0, Vec::new())
    );
    assert_eq!(
        messages_in(dir.path(), &["check", "--rule", "rule.yml", "bad.yaml"]),
        (
            4,
            [
                "/properties/a/maximum: 101 is greater than the maximum of 1e2",
                "/properties/b/multipleOf: 1 is not a multiple of 2.50",
                "/properties/c/exclusiveMaximum: 1 is greater than or equal to the maximum of 1E-1",
                "/properties/d/$ref/minimum: 4 is less than the minimum of 5e0",
                "/properties/f/not: {\"maximum\":1e2} is not allowed for 5",
            ]
            .map(str::to_owned)
            .to_vec()
        )
    );
}

/// The text dq keeps inside a schema — a limit's spelling, a tiny
/// divisor's literal and its stand-in, a NaN or an infinity's marker —
/// never reaches output: a schema a message prints (`not`) reads as the
/// rule wrote it, in every report format, in `dq test`, in `-vv` tracing,
/// and in a compile error. Before, a `not` message printed `1e+2` for
/// `1e2`, and a tiny divisor under a keyword that takes no schema (`type`)
/// put its stand-in into the compile error:
/// `{"multipleOf":1,"\u0000dq.multipleOf":"1e-400"} is not valid …`.
#[test]
fn no_text_dq_keeps_in_a_schema_reaches_output() {
    let rule = "id: t.not\ndescription: x\nseverity: error\nmatch: { format: yaml }\n\
                check:\n  schema: { properties: { x: { not: { maximum: 1e2, \
                multipleOf: 1e-400, enum: [.inf, 5], properties: { m: { minimum: 1E1 } } } } } }\n";
    let cases = "tests:\n  - name: shows the message\n    input: |\n      x: 5\n    \
                 expected:\n      violations: []\n";
    let broken = "id: t.bad\ndescription: x\nseverity: error\nmatch: { format: yaml }\n\
                  check:\n  schema: { properties: { x: { type: { maximum: 1e2, \
                  multipleOf: 1e-400 } } } }\n";
    let dir = dir_with(&[
        ("r/not.yml", rule),
        ("r/not.test.yml", cases),
        ("bad.yml", broken),
        (".dq/rules/not.yml", rule),
        ("doc.yaml", "x: 5\n"),
    ]);
    let message = "/properties/x/not: {\"maximum\":1e2,\"multipleOf\":1e-400,\"enum\":[.inf,5],\
                   \"properties\":{\"m\":{\"minimum\":1E1}}} is not allowed for 5";
    assert_eq!(
        messages_in(dir.path(), &["check", "--rule", "r/not.yml", "doc.yaml"]),
        (4, vec![message.to_owned()])
    );
    let runs: [&[&str]; 9] = [
        &["check", "--rule", "r/not.yml", "doc.yaml"],
        &["-F", "yaml", "check", "--rule", "r/not.yml", "doc.yaml"],
        &["-F", "jsonl", "lint", "doc.yaml"],
        &["-F", "toon", "lint", "doc.yaml"],
        &["-vv", "lint", "doc.yaml"],
        &["test", "r"],
        &["-F", "json", "explain", "t.not"],
        &["-F", "json", "rules", "list"],
        &["-vv", "check", "--rule", "bad.yml", "doc.yaml"],
    ];
    for args in runs {
        let (code, stdout, stderr) = run_in(dir.path(), args);
        let output = format!("{stdout}{stderr}");
        for leak in ["\u{0}", "\\u0000", "dq.spelling", "dq.multipleOf", "dq.inf"] {
            assert!(
                !output.contains(leak),
                "{args:?} exit {code} prints {leak:?}: {output}"
            );
        }
    }
    let (code, _, stderr) = run_in(dir.path(), &["check", "--rule", "bad.yml", "doc.yaml"]);
    assert_eq!(code, 3, "{stderr}");
    assert!(
        stderr.contains(
            "{\"maximum\":1e+2,\"multipleOf\":1e-400} is not valid under any of the schemas"
        ),
        "{stderr}"
    );
}

/// A rule `t.<id>` whose check is `schema`.
fn schema_rule(id: &str, schema: &str) -> String {
    format!(
        "id: t.{id}\ndescription: x\nseverity: error\nmatch: {{ format: yaml }}\n\
         check:\n  schema: {schema}\n"
    )
}

/// A limit is quoted as the rule spells it under every keyword that takes
/// a schema — `patternProperties`, `dependentSchemas`, `prefixItems`, a
/// `$defs` entry a `$ref` reaches — and a `not` prints each of them as
/// written. Before (acce170, bbe4e4c), each came out as serde_json reads
/// it: `1e+2`, `1e+1`, `5e+0`.
#[test]
fn a_limit_under_every_keyword_that_takes_a_schema_is_quoted_as_written() {
    let dir = dir_with(&[
        (
            "pp.yml",
            &schema_rule(
                "pp",
                "{ patternProperties: { \"^p\": { exclusiveMinimum: 1e2 } } }",
            ),
        ),
        (
            "ds.yml",
            &schema_rule(
                "ds",
                "{ dependentSchemas: { a: { properties: { b: { minimum: 1E1 } } } } }",
            ),
        ),
        (
            "pi.yml",
            &schema_rule("pi", "{ prefixItems: [ { exclusiveMinimum: 1e2 } ] }"),
        ),
        (
            "df.yml",
            &schema_rule(
                "df",
                "{ $defs: { m: { minimum: 5e0 } }, properties: { d: { $ref: \"#/$defs/m\" } } }",
            ),
        ),
        (
            "nt.yml",
            &schema_rule(
                "nt",
                "{ properties: { x: { not: { patternProperties: { \"^p\": { minimum: 1e2 } }, \
                 prefixItems: [ { maximum: 1E1 } ], dependentSchemas: { a: { minimum: 25e-1 } } } } } }",
            ),
        ),
        ("pp_bad.yaml", "p1: 100\n"),
        ("pp_ok.yaml", "p1: 101\nq: 1\n"),
        ("ds_bad.yaml", "a: 1\nb: 5\n"),
        ("ds_ok.yaml", "b: 5\n"),
        ("pi_bad.yaml", "- 100\n"),
        ("pi_ok.yaml", "- 101\n- 1\n"),
        ("df_bad.yaml", "d: 4\n"),
        ("df_ok.yaml", "d: 5\n"),
        ("nt_bad.yaml", "x: {p: 200, a: 3}\n"),
        ("nt_ok.yaml", "x: {p: 1}\n"),
    ]);
    let not = "/properties/x/not: {\"patternProperties\":{\"^p\":{\"minimum\":1e2}},\
               \"prefixItems\":[{\"maximum\":1E1}],\"dependentSchemas\":{\"a\":{\"minimum\":25e-1}}} \
               is not allowed for {\"p\":200,\"a\":3}";
    for (rule, bad, message) in [
        (
            "pp",
            "pp_bad.yaml",
            "/patternProperties/^p/exclusiveMinimum: 100 is less than or equal to the minimum of 1e2",
        ),
        (
            "ds",
            "ds_bad.yaml",
            "/dependentSchemas/a/properties/b/minimum: 5 is less than the minimum of 1E1",
        ),
        (
            "pi",
            "pi_bad.yaml",
            "/prefixItems/0/exclusiveMinimum: 100 is less than or equal to the minimum of 1e2",
        ),
        (
            "df",
            "df_bad.yaml",
            "/properties/d/$ref/minimum: 4 is less than the minimum of 5e0",
        ),
        ("nt", "nt_bad.yaml", not),
    ] {
        let rule_file = format!("{rule}.yml");
        assert_eq!(
            messages_in(dir.path(), &["check", "--rule", &rule_file, bad]),
            (4, vec![message.to_owned()]),
            "{rule} on {bad}"
        );
        let good = format!("{rule}_ok.yaml");
        assert_eq!(
            messages_in(dir.path(), &["check", "--rule", &rule_file, &good]),
            (0, Vec::new()),
            "{rule} on {good}"
        );
    }
}

/// A `not` prints the data it holds (`const`, `enum`) as data: a big
/// integer digit for digit, an exponent as serde_json spells a number,
/// a map under `enum` with no limit spelling noted into it. Only a limit
/// keyword of a schema gets its source text. Guard: this held before
/// acce170 too.
#[test]
fn a_not_prints_the_data_it_holds_as_data() {
    let dir = dir_with(&[
        (
            "r.yml",
            &schema_rule(
                "nc",
                "{ properties: { x: { not: { const: 1e2 } }, \
                 y: { not: { enum: [18446744073709551617, 1e2, {maximum: 1e2}] } } } }",
            ),
        ),
        ("bad.yaml", "x: 1e2\ny: 18446744073709551617\n"),
        ("ok.yaml", "x: 99\ny: 18446744073709551616\n"),
    ]);
    assert_eq!(
        messages_in(dir.path(), &["check", "--rule", "r.yml", "bad.yaml"]),
        (
            4,
            vec![
                "/properties/x/not: {\"const\":1e+2} is not allowed for 100.0".to_owned(),
                "/properties/y/not: {\"enum\":[18446744073709551617,1e+2,{\"maximum\":1e+2}]} \
                 is not allowed for 18446744073709551617"
                    .to_owned(),
            ]
        )
    );
    assert_eq!(
        messages_in(dir.path(), &["check", "--rule", "r.yml", "ok.yaml"]),
        (0, Vec::new())
    );
}

/// A property, and a document key, named as dq's own spelling note
/// (`"\u0000dq.spelling"`) is an ordinary name: it is checked, its limit
/// quoted as written, and a `not` prints it.
#[test]
fn a_property_named_like_dqs_spelling_note_is_an_ordinary_name() {
    let dir = dir_with(&[
        (
            "r.yml",
            &schema_rule(
                "nul",
                "{ properties: { \"\\0dq.spelling\": { maximum: 1e2 } } }",
            ),
        ),
        (
            "n.yml",
            &schema_rule(
                "nn",
                "{ properties: { x: { not: { properties: { \"\\0dq.spelling\": { maximum: 1e2 } } } } } }",
            ),
        ),
        ("bad.yaml", "\"\\0dq.spelling\": 101\n"),
        ("ok.yaml", "\"\\0dq.spelling\": 100\n"),
        ("n_bad.yaml", "x: {\"\\0dq.spelling\": 5}\n"),
        ("n_ok.yaml", "x: {\"\\0dq.spelling\": 500}\n"),
    ]);
    assert_eq!(
        messages_in(dir.path(), &["check", "--rule", "r.yml", "bad.yaml"]),
        (
            4,
            vec![
                "/properties/\u{0}dq.spelling/maximum: 101 is greater than the maximum of 1e2"
                    .to_owned()
            ]
        )
    );
    assert_eq!(
        messages_in(dir.path(), &["check", "--rule", "r.yml", "ok.yaml"]),
        (0, Vec::new())
    );
    assert_eq!(
        messages_in(dir.path(), &["check", "--rule", "n.yml", "n_bad.yaml"]),
        (
            4,
            vec![
                "/properties/x/not: {\"properties\":{\"\\u0000dq.spelling\":{\"maximum\":1e2}}} \
                 is not allowed for {\"\\u0000dq.spelling\":5}"
                    .to_owned()
            ]
        )
    );
    assert_eq!(
        messages_in(dir.path(), &["check", "--rule", "n.yml", "n_ok.yaml"]),
        (0, Vec::new())
    );
}

/// The schema of an `extract` + `nested` rule quotes its limits as
/// written too, in a bound's message and in a `not`. Before (acce170,
/// bbe4e4c): `1e+2`, `{"minimum":1e+3}`.
#[test]
fn a_nested_rule_quotes_its_limit_as_written() {
    let rule = "id: t.outer\ndescription: x\nseverity: error\nmatch: { format: yaml }\n\
                check:\n  extract: '[{value: .inner, format: \"json\", anchor: \"/inner\"}]'\n  \
                message: inner\n  nested:\n    id: t.inner\n    description: y\n    \
                severity: error\n    match: { format: json }\n    check:\n      \
                schema: { properties: { n: { maximum: 1e2, not: { minimum: 1E3 } } } }\n";
    let dir = dir_with(&[
        ("c.yml", rule),
        ("a.yaml", "inner: '{\"n\": 101}'\n"),
        ("b.yaml", "inner: '{\"n\": 5000}'\n"),
        ("ok.yaml", "inner: '{\"n\": 100}'\n"),
    ]);
    assert_eq!(
        messages_in(dir.path(), &["check", "--rule", "c.yml", "a.yaml"]),
        (
            4,
            vec!["inner: /properties/n/maximum: 101 is greater than the maximum of 1e2".to_owned()]
        )
    );
    assert_eq!(
        messages_in(dir.path(), &["check", "--rule", "c.yml", "b.yaml"]),
        (
            4,
            [
                "inner: /properties/n/not: {\"minimum\":1E3} is not allowed for 5000",
                "inner: /properties/n/maximum: 5000 is greater than the maximum of 1e2",
            ]
            .map(str::to_owned)
            .to_vec()
        )
    );
    assert_eq!(
        messages_in(dir.path(), &["check", "--rule", "c.yml", "ok.yaml"]),
        (0, Vec::new())
    );
}

/// `dq test` matches a fixture's `message_contains` against the limit as
/// the rule writes it: `1e2` passes, serde_json's `1e+2` no longer does.
/// Before (acce170), it was the other way round.
#[test]
fn dq_test_matches_a_limit_as_the_rule_writes_it() {
    let rule = schema_rule("lim", "{ properties: { x: { maximum: 1e2 } } }");
    let cases = |spelled: &str| {
        format!(
            "tests:\n  - name: quotes the limit\n    input: |\n      x: 101\n    expected:\n      \
             violations:\n        - rule: t.lim\n          \
             message_contains: \"greater than the maximum of {spelled}\"\n  \
             - name: within the limit\n    input: |\n      x: 100\n    expected:\n      \
             violations: []\n"
        )
    };
    let dir = dir_with(&[
        ("as_written/lim.yml", &rule),
        ("as_written/lim.test.yml", &cases("1e2")),
        ("serde/lim.yml", &rule),
        ("serde/lim.test.yml", &cases("1e+2")),
    ]);
    let (code, stdout, stderr) = run_in(dir.path(), &["test", "as_written"]);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(stdout.contains("summary: 2 passed, 0 failed"), "{stdout}");
    let (code, stdout, stderr) = run_in(dir.path(), &["test", "serde"]);
    assert_eq!(code, 4, "{stdout}{stderr}");
    assert!(
        stdout.contains(
            "extra: rule=t.lim message=\"/properties/x/maximum: 101 is greater than the \
             maximum of 1e2\" line=1"
        ),
        "{stdout}"
    );
    assert!(stdout.contains("summary: 1 passed, 1 failed"), "{stdout}");
}

/// A project rule under `.dq/rules/` with a `schema_file` quotes its
/// limits as the file spells them, `dq lint` end to end, in a bound and in
/// a `not`. Before (acce170, bbe4e4c): `1e+2`, `{"multipleOf":7e+0}`.
#[test]
fn a_project_rule_quotes_its_schema_file_limit_as_written() {
    let dir = dir_with(&[
        (
            ".dq/rules/file.yml",
            "id: t.file\ndescription: x\nseverity: error\nmatch: { format: yaml }\n\
             check:\n  schema_file: s.json\n",
        ),
        (
            ".dq/rules/s.json",
            r#"{"properties": {"x": {"exclusiveMaximum": 1E2, "not": {"multipleOf": 7e0}}}}"#,
        ),
        ("bound.yaml", "x: 100\n"),
        ("not.yaml", "x: 14\n"),
        ("ok.yaml", "x: 5\n"),
    ]);
    assert_eq!(
        messages_in(dir.path(), &["lint", "bound.yaml"]),
        (
            4,
            vec![
                "/properties/x/exclusiveMaximum: 100 is greater than or equal to the maximum of 1E2"
                    .to_owned()
            ]
        )
    );
    assert_eq!(
        messages_in(dir.path(), &["lint", "not.yaml"]),
        (
            4,
            vec!["/properties/x/not: {\"multipleOf\":7e0} is not allowed for 14".to_owned()]
        )
    );
    assert_eq!(
        messages_in(dir.path(), &["lint", "ok.yaml"]),
        (0, Vec::new())
    );
}

/// Nothing a rule, a schema file or a document holds is read as one of
/// the notes dq keeps in a schema or the markers it renders a NaN or an
/// infinity with: a member named `"\u0000dq.spelling"` or
/// `"\u0000dq.multipleOf"`, a string `"\u0000dq.inf\u0000"`. Each is the
/// user's own — checked, compared and printed as written.
///
/// Before, a spelling member changed the limit a message quotes (`the
/// maximum of garbage`), a `multipleOf` member changed the divisor (`3 is
/// not a multiple of 7`), a `not` printed neither, a schema string matched
/// an infinity, and a document string matched `enum: [.inf]` and printed
/// as `.inf`.
#[test]
fn a_member_named_like_dqs_spelling_note_never_changes_a_message() {
    let notes = r#"{ maximum: 5, multipleOf: 1, "\0dq.spelling": { maximum: "garbage" }, "\0dq.multipleOf": "7" }"#;
    let dir = dir_with(&[
        (
            "r.yml",
            &schema_rule("m", &format!("{{ properties: {{ x: {notes} }} }}")),
        ),
        (
            "not.yml",
            &schema_rule(
                "n",
                "{ properties: { x: { not: { maximum: 50, \"\\0dq.spelling\": { maximum: \"g\" } } } } }",
            ),
        ),
        (
            "inf.yml",
            &schema_rule(
                "i",
                "{ properties: { s: { const: \"\\0dq.inf\\0\" }, x: { enum: [.inf] }, y: { type: number } } }",
            ),
        ),
        (
            ".dq/rules/file.yml",
            "id: t.file\ndescription: x\nseverity: error\nmatch: { format: yaml }\n\
             check:\n  schema_file: s.json\n",
        ),
        (
            ".dq/rules/s.json",
            r#"{"properties": {"f": {"maximum": 1e2, "\u0000dq.spelling": {"maximum": "g"}}}}"#,
        ),
        ("doc.yaml", "x: 6\n"),
        ("three.yaml", "x: 3\n"),
        (
            "strings.yaml",
            "s: .inf\nx: \"\\0dq.inf\\0\"\ny: \"\\0dq.nan\\0\"\n",
        ),
        ("file.yaml", "f: 101\n"),
    ]);
    let nul = "\\u0000";
    assert_eq!(
        messages_in(dir.path(), &["check", "--rule", "r.yml", "doc.yaml"]),
        (
            4,
            vec!["/properties/x/maximum: 6 is greater than the maximum of 5".to_owned()]
        )
    );
    assert_eq!(
        messages_in(dir.path(), &["check", "--rule", "r.yml", "three.yaml"]),
        (0, Vec::new())
    );
    assert_eq!(
        messages_in(dir.path(), &["check", "--rule", "not.yml", "three.yaml"]),
        (
            4,
            vec![format!(
                "/properties/x/not: {{\"maximum\":50,\"{nul}dq.spelling\":{{\"maximum\":\"g\"}}}} \
                 is not allowed for 3"
            )]
        )
    );
    assert_eq!(
        messages_in(dir.path(), &["check", "--rule", "inf.yml", "strings.yaml"]),
        (
            4,
            vec![
                format!("/properties/s/const: \"{nul}dq.inf{nul}\" was expected"),
                format!("/properties/x/enum: \"{nul}dq.inf{nul}\" is not one of .inf"),
                format!("/properties/y/type: \"{nul}dq.nan{nul}\" is not of type \"number\""),
            ]
        )
    );
    assert_eq!(
        messages_in(dir.path(), &["lint", "file.yaml"]),
        (
            4,
            vec!["/properties/f/maximum: 101 is greater than the maximum of 1e2".to_owned()]
        )
    );
}
