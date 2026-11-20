//! A schema member or a string that imitates one of dq's own notes never
//! changes what dq reports.
//!
//! dq knows things about a schema its JSON cannot hold: the source text of
//! a limit (`1E2`, which serde_json reads as `1e+2`), where a YAML schema
//! holds a NaN or an infinity, a `multipleOf` divisor too small for an
//! `f64`. It used to keep them inside the schema, as members named
//! `"\u0000dq.spelling"` and `"\u0000dq.multipleOf"` and as strings
//! `"\u0000dq.inf\u0000"`, so a rule that wrote such a member forged the
//! note: `maximum: 5` plus a spelling member printed `greater than the
//! maximum of garbage`. Now the notes sit beside the schema, by place, and
//! only the validator's private copy holds them in band under a tag drawn
//! at random per process.
//!
//! Each case here writes the old member names, and a member shaped like the
//! new tagged text (`"\u0000dq<32 hex digits>.spelling"`), next to a real
//! limit dq does keep a note for — the same schema object, so the same
//! RFC 6901 place — and asserts the message quotes the limit as the user
//! wrote it (`1E2`, `0.20`), prints the forged members as the user's own
//! data where a message prints a schema (`not`), and exits with the
//! documented code (4 for a violation, 0 for none). The positions: a
//! project rule under `.dq/rules/`, a project rule shaped like the `@std`
//! ones (a `schema_file` sidecar), the `nested:` rule of a composite, and a
//! limit behind a `$ref`, a nested `$id` base and an `$anchor`.

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

/// The exit code of `dq --no-color -F json <args>` in `cwd` and the
/// messages of the report it prints. Panics when stdout is no report or
/// stderr is not the summary a run of that many violations writes.
fn messages_in(cwd: &std::path::Path, args: &[&str]) -> (i32, Vec<String>) {
    let out = dq()
        .current_dir(cwd)
        .args(["--no-color", "-F", "json"])
        .args(args)
        .output()
        .expect("dq runs");
    let code = out.status.code().expect("exit code");
    let stdout = String::from_utf8(out.stdout).expect("UTF-8 stdout");
    let stderr = String::from_utf8(out.stderr).expect("UTF-8 stderr");
    let report: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|err| panic!("{args:?} exit {code}: no JSON report ({err}): {stdout}"));
    let messages = report["diagnostics"]
        .as_array()
        .unwrap_or_else(|| panic!("{args:?} exit {code}: no diagnostics: {stdout}"))
        .iter()
        .map(|d| d["message"].as_str().expect("a message").to_owned())
        .collect::<Vec<String>>();
    // A violation is summed up on stderr; a clean run writes nothing there.
    let summary = if messages.is_empty() {
        String::new()
    } else {
        format!(
            "lint reported {} error-severity diagnostic(s)\n",
            messages.len()
        )
    };
    assert_eq!(stderr, summary, "{args:?} exit {code}: stderr");
    (code, messages)
}

/// `(4, messages)`, a report of violations.
fn fails(messages: &[&str]) -> (i32, Vec<String>) {
    (4, messages.iter().map(|m| (*m).to_owned()).collect())
}

/// `(0, [])`, a clean report.
fn passes() -> (i32, Vec<String>) {
    (0, Vec::new())
}

/// A member name shaped like the text dq puts in band now: `\0dq`, a tag
/// of 32 hex digits, `.spelling`. The tag is not the one this process
/// drew, so the member is the rule's own. As YAML double-quoted text.
const TAGGED_YAML: &str = r"\0dq0123456789abcdef0123456789abcdef.spelling";

/// [`TAGGED_YAML`] as JSON text, and as a message prints the name.
const TAGGED_JSON: &str = r"\u0000dq0123456789abcdef0123456789abcdef.spelling";

/// A rule `t.<id>` whose check is `schema`, given as YAML block lines
/// indented under `schema:`.
fn schema_rule(id: &str, schema: &str) -> String {
    format!(
        "id: t.{id}\ndescription: x\nseverity: error\nmatch: {{ format: yaml }}\n\
         check:\n  schema:\n{schema}"
    )
}

/// The schema every inline case forges notes into: a real limit spelled
/// unlike serde_json (`1E2`, `0.20`, `1E1`) and, in the same schema
/// object, a member under each old note name and one shaped like the new
/// tagged one, each saying something else.
fn forged_schema() -> String {
    format!(
        r#"    properties:
      x:
        maximum: 1E2
        multipleOf: 0.20
        "\0dq.spelling": {{ maximum: garbage, multipleOf: junk }}
        "\0dq.multipleOf": "7"
        "{TAGGED_YAML}": {{ maximum: forged }}
      y:
        not:
          minimum: 1E1
          "\0dq.spelling": {{ minimum: g }}
          "{TAGGED_YAML}": {{ minimum: f }}
"#
    )
}

/// The `not` message [`forged_schema`] gives for `y: 20`: the forged
/// members printed as the rule's own.
fn forged_not_message(prefix: &str) -> String {
    format!(
        "{prefix}/properties/y/not: {{\"minimum\":1E1,\"\\u0000dq.spelling\":{{\"minimum\":\"g\"}},\
         \"{TAGGED_JSON}\":{{\"minimum\":\"f\"}}}} is not allowed for 20"
    )
}

/// Baseline: a project rule under `.dq/rules/`, `dq lint` end to end. The
/// forged members sit in the very schema object whose `maximum` and
/// `multipleOf` dq notes the spelling of. Before 66c9db3 the divisor member
/// replaced the divisor: `101 is not a multiple of 7`.
#[test]
fn a_project_rule_ignores_members_named_like_dqs_notes() {
    let dir = dir_with(&[
        (
            ".dq/rules/forged.yml",
            &schema_rule("forged", &forged_schema()),
        ),
        ("big.yaml", "x: 101\n"),
        ("frac.yaml", "x: 0.3\n"),
        ("not.yaml", "y: 20\n"),
        ("ok.yaml", "x: 0.4\ny: 5\n"),
    ]);
    assert_eq!(
        messages_in(dir.path(), &["lint", "big.yaml"]),
        fails(&["/properties/x/maximum: 101 is greater than the maximum of 1E2"])
    );
    assert_eq!(
        messages_in(dir.path(), &["lint", "frac.yaml"]),
        fails(&["/properties/x/multipleOf: 0.3 is not a multiple of 0.20"])
    );
    assert_eq!(
        messages_in(dir.path(), &["lint", "not.yaml"]),
        fails(&[&forged_not_message("")])
    );
    assert_eq!(messages_in(dir.path(), &["lint", "ok.yaml"]), passes());
}

/// A project rule shaped like the `@std` schema rules — a `match.filter`
/// and a `./` sidecar `schema_file` declaring 2020-12 — whose sidecar
/// forges notes next to limits it reaches through a `$ref`, through a
/// fragment resolved against a nested `$id`, and through an `$anchor`. The
/// sidecar's notes come from its JSON text, by place from its root. Before
/// 66c9db3 the limit behind the nested `$id` printed `inner-garbage`.
#[test]
fn a_schema_file_sidecar_ignores_members_named_like_dqs_notes() {
    let rule = "id: t.values\ndescription: x\nseverity: error\n\
                match:\n  format: yaml\n  filter: 'type == \"object\"'\n\
                check:\n  schema_file: ./values.schema.json\n";
    let schema = format!(
        r##"{{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "$defs": {{
    "port": {{"maximum": 65535E0, "\u0000dq.spelling": {{"maximum": "garbage"}}}},
    "inner": {{
      "$id": "inner.json",
      "$defs": {{
        "port": {{"maximum": 0.20, "\u0000dq.spelling": {{"maximum": "inner-garbage"}}}}
      }},
      "properties": {{"y": {{"$ref": "#/$defs/port"}}}}
    }},
    "count": {{
      "$anchor": "count",
      "multipleOf": 0.50,
      "\u0000dq.spelling": {{"multipleOf": "junk"}},
      "\u0000dq.multipleOf": "7",
      "{TAGGED_JSON}": {{"multipleOf": "forged"}}
    }}
  }},
  "properties": {{
    "port": {{"$ref": "#/$defs/port"}},
    "inner": {{"$ref": "#/$defs/inner"}},
    "replicas": {{"$ref": "#count"}}
  }}
}}"##
    );
    let dir = dir_with(&[
        (".dq/rules/values.yml", rule),
        (".dq/rules/values.schema.json", &schema),
        ("port.yaml", "port: 70000\n"),
        ("inner.yaml", "inner: { y: 1 }\n"),
        ("replicas.yaml", "replicas: 0.7\n"),
        ("ok.yaml", "port: 65535\ninner: { y: 0.2 }\nreplicas: 3.5\n"),
    ]);
    assert_eq!(
        messages_in(dir.path(), &["lint", "port.yaml"]),
        fails(&["/properties/port/$ref/maximum: 70000 is greater than the maximum of 65535E0"])
    );
    assert_eq!(
        messages_in(dir.path(), &["lint", "inner.yaml"]),
        fails(&[
            "/properties/inner/$ref/properties/y/$ref/maximum: 1 is greater than the maximum of 0.20"
        ])
    );
    assert_eq!(
        messages_in(dir.path(), &["lint", "replicas.yaml"]),
        fails(&["/properties/replicas/$ref/multipleOf: 0.7 is not a multiple of 0.50"])
    );
    assert_eq!(messages_in(dir.path(), &["lint", "ok.yaml"]), passes());
}

/// The `nested:` rule of an `extract` + `nested` composite, whose schema
/// is a schema check: its notes are read from its place in the rule file
/// (`/check/nested/check/schema`), and the composite's message prefix
/// comes first. Before 66c9db3: `101 is not a multiple of 7`.
#[test]
fn a_nested_rule_ignores_members_named_like_dqs_notes() {
    let nested_schema: String = forged_schema()
        .lines()
        .map(|line| format!("      {line}\n"))
        .collect();
    let rule = format!(
        "id: t.outer\ndescription: x\nseverity: error\nmatch: {{ format: yaml }}\n\
         check:\n  extract: '[{{value: .inner, format: \"json\", anchor: \"/inner\"}}]'\n  \
         message: inner\n  nested:\n    id: t.inner\n    description: y\n    \
         severity: error\n    match: {{ format: json }}\n    check:\n      schema:\n\
         {nested_schema}"
    );
    let dir = dir_with(&[
        ("c.yml", &rule),
        ("big.yaml", "inner: '{\"x\": 101}'\n"),
        ("frac.yaml", "inner: '{\"x\": 0.3}'\n"),
        ("not.yaml", "inner: '{\"y\": 20}'\n"),
        ("ok.yaml", "inner: '{\"x\": 0.4, \"y\": 5}'\n"),
    ]);
    let check = |doc: &str| messages_in(dir.path(), &["check", "--rule", "c.yml", doc]);
    assert_eq!(
        check("big.yaml"),
        fails(&["inner: /properties/x/maximum: 101 is greater than the maximum of 1E2"])
    );
    assert_eq!(
        check("frac.yaml"),
        fails(&["inner: /properties/x/multipleOf: 0.3 is not a multiple of 0.20"])
    );
    assert_eq!(check("not.yaml"), fails(&[&forged_not_message("inner: ")]));
    assert_eq!(check("ok.yaml"), passes());
}

/// An inline schema whose limits sit behind a `$ref`: one to `$defs`, one
/// through a nested schema with its own `$id`, whose `#/$defs/lim` is that
/// schema's `$defs` and not the root's (both hold a `lim`, each spelled
/// and forged differently), and one to an `$anchor`. dq keys a note by its
/// place from the schema's root, where jsonschema's own locations would
/// mix the two `lim`s up. Before 66c9db3 the limit behind the nested `$id`
/// printed `inner-garbage`.
#[test]
fn a_limit_behind_a_ref_ignores_members_named_like_dqs_notes() {
    let schema = format!(
        r#"    $defs:
      lim: {{ maximum: 1E3, "\0dq.spelling": {{ maximum: root-garbage }} }}
      inner:
        $id: inner.json
        $defs:
          lim: {{ maximum: 0.20, "\0dq.spelling": {{ maximum: inner-garbage }} }}
        properties:
          y: {{ $ref: '#/$defs/lim' }}
      anchored:
        $anchor: low
        minimum: 1E1
        "\0dq.spelling": {{ minimum: g }}
        "{TAGGED_YAML}": {{ minimum: f }}
    properties:
      w: {{ $ref: '#/$defs/lim' }}
      x: {{ $ref: '#/$defs/inner' }}
      z: {{ $ref: '#low' }}
"#
    );
    let dir = dir_with(&[
        ("r.yml", &schema_rule("ref", &schema)),
        ("w.yaml", "w: 1001\n"),
        ("x.yaml", "x: { y: 1 }\n"),
        ("z.yaml", "z: 5\n"),
        ("ok.yaml", "w: 1000\nx: { y: 0.2 }\nz: 10\n"),
    ]);
    let check = |doc: &str| messages_in(dir.path(), &["check", "--rule", "r.yml", doc]);
    assert_eq!(
        check("w.yaml"),
        fails(&["/properties/w/$ref/maximum: 1001 is greater than the maximum of 1E3"])
    );
    assert_eq!(
        check("x.yaml"),
        fails(&[
            "/properties/x/$ref/properties/y/$ref/maximum: 1 is greater than the maximum of 0.20"
        ])
    );
    assert_eq!(
        check("z.yaml"),
        fails(&["/properties/z/$ref/minimum: 5 is less than the minimum of 1E1"])
    );
    assert_eq!(check("ok.yaml"), passes());
}

/// Where a YAML schema holds an infinity, the note says so by place, and
/// the string `".inf"`, the old marker `"\u0000dq.inf\u0000"` and a string
/// shaped like the new tagged marker stay strings — at a place of their
/// own (`b`) and next to a real infinity in one `enum` (`c`).
#[test]
fn a_string_shaped_like_an_infinity_note_stays_a_string() {
    let tagged = r"\0dq0123456789abcdef0123456789abcdef.inf\0";
    let schema = format!(
        "    properties:\n      a: {{ const: .inf }}\n      b: {{ const: \".inf\" }}\n      \
         c: {{ enum: [.inf, \".inf\", \"\\0dq.inf\\0\", \"{tagged}\"] }}\n"
    );
    let dir = dir_with(&[
        ("r.yml", &schema_rule("inf", &schema)),
        ("ok.yaml", "a: .inf\nb: \".inf\"\nc: .inf\n"),
        ("strings.yaml", "c: \"\\0dq.inf\\0\"\n"),
        ("tagged.yaml", &format!("c: \"{tagged}\"\n")),
        ("a.yaml", "a: \".inf\"\n"),
        ("b.yaml", "b: .inf\n"),
        ("c.yaml", "c: -.inf\n"),
    ]);
    let check = |doc: &str| messages_in(dir.path(), &["check", "--rule", "r.yml", doc]);
    assert_eq!(check("ok.yaml"), passes());
    assert_eq!(check("strings.yaml"), passes());
    assert_eq!(check("tagged.yaml"), passes());
    assert_eq!(
        check("a.yaml"),
        fails(&["/properties/a/const: .inf was expected"])
    );
    assert_eq!(
        check("b.yaml"),
        fails(&["/properties/b/const: \".inf\" was expected"])
    );
    assert_eq!(
        check("c.yaml"),
        fails(&["/properties/c/enum: -.inf is not one of .inf, \".inf\" or 2 other candidates"])
    );
}

/// BUG: a `$ref` may point anywhere in the schema document, and the
/// validator reads its target as a schema; but dq notes a limit's spelling
/// only under the keywords that take a schema (`properties`, `$defs`, …),
/// so a limit a `$ref` reaches elsewhere (`#/x-limits/a`) is quoted as
/// serde_json reads it, `1e+2`, in a rule and in a `schema_file` alike.
/// The forged member next to it changes nothing now; before 66c9db3 the
/// message quoted it (`the maximum of g`).
#[test]
#[ignore = "BUG: a limit a $ref reaches outside the schema keywords prints 1e+2, not 1E2"]
fn a_limit_a_ref_reaches_outside_the_schema_keywords_is_quoted_as_written() {
    let schema = "    x-limits:\n      a: { maximum: 1E2, \"\\0dq.spelling\": { maximum: g } }\n    \
                  properties:\n      x: { $ref: '#/x-limits/a' }\n";
    let dir = dir_with(&[
        ("r.yml", &schema_rule("ext", schema)),
        (
            "file.yml",
            "id: t.file\ndescription: x\nseverity: error\nmatch: { format: yaml }\n\
             check:\n  schema_file: s.json\n",
        ),
        (
            "s.json",
            r##"{"x-limits": {"a": {"maximum": 1E2, "\u0000dq.spelling": {"maximum": "g"}}},
                 "properties": {"x": {"$ref": "#/x-limits/a"}}}"##,
        ),
        ("doc.yaml", "x: 101\n"),
    ]);
    let message = "/properties/x/$ref/maximum: 101 is greater than the maximum of 1E2";
    assert_eq!(
        messages_in(dir.path(), &["check", "--rule", "r.yml", "doc.yaml"]),
        fails(&[message])
    );
    assert_eq!(
        messages_in(dir.path(), &["check", "--rule", "file.yml", "doc.yaml"]),
        fails(&[message])
    );
}

/// BUG: a limit a YAML rule reaches through an alias (`*lim`) has no
/// source text at the alias's place, so dq neither notes its spelling nor
/// keeps its literal: `maximum: 1E2` prints as `100.0`, in a bound's
/// message and in a `not`.
#[test]
#[ignore = "BUG: a limit behind a YAML alias prints 100.0, not 1E2"]
fn a_limit_behind_a_yaml_alias_is_quoted_as_written() {
    let schema = "    $defs:\n      lim: &lim { maximum: 1E2 }\n    properties:\n      \
                  x: *lim\n      y: { not: *lim }\n";
    let dir = dir_with(&[
        ("r.yml", &schema_rule("alias", schema)),
        ("x.yaml", "x: 101\n"),
        ("y.yaml", "y: 5\n"),
    ]);
    let check = |doc: &str| messages_in(dir.path(), &["check", "--rule", "r.yml", doc]);
    assert_eq!(
        check("x.yaml"),
        fails(&["/properties/x/maximum: 101 is greater than the maximum of 1E2"])
    );
    assert_eq!(
        check("y.yaml"),
        fails(&["/properties/y/not: {\"maximum\":1E2} is not allowed for 5"])
    );
}
