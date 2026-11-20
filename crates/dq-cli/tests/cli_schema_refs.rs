//! A schema rule's `$ref` resolves inside the schema document and nowhere
//! else.
//!
//! Internal: a fragment (`#/$defs/x`), and a reference that, resolved
//! against the base of the schema it sits in, names the root or a schema's
//! `$id` — `inner.json` beside an embedded `$id: inner.json`, or
//! `https://example.com/s.json#/$defs/x` in a schema whose `$id` is
//! `https://example.com/s.json`. Both used to fail with `external $ref is
//! not supported`.
//!
//! External: everything else — another URI, a `file:` or `http:` one no
//! `$id` names, a `$id` in data, a `$dynamicRef` outside, a `$schema` that
//! is not bundled. Each fails the compile (exit 3) and nothing is fetched:
//! a listening socket gets no connection, and a file the reference names,
//! which holds a schema, is not read.

use std::net::TcpListener;

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

/// A rule `t.<id>` whose check is `schema`, given as YAML block lines
/// indented under `schema:`.
fn schema_rule(id: &str, schema: &str) -> String {
    format!(
        "id: t.{id}\ndescription: x\nseverity: error\nmatch: {{ format: yaml }}\n\
         check:\n  schema:\n{schema}"
    )
}

/// What `dq --no-color -F json check --rule r.yml <doc>` gives in a temp
/// dir holding the rule `rule` and the document `doc`: the exit code,
/// stdout, stderr.
fn check(rule: &str, doc: &str) -> (i32, String, String) {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("r.yml"), rule).expect("write rule");
    std::fs::write(dir.path().join("d.yaml"), doc).expect("write doc");
    run_in(dir.path(), &["check", "--rule", "r.yml", "d.yaml"])
}

/// `dq --no-color -F json <args>` in `cwd`: the exit code, stdout, stderr.
fn run_in(cwd: &std::path::Path, args: &[&str]) -> (i32, String, String) {
    let out = dq()
        .current_dir(cwd)
        .args(["--no-color", "-F", "json"])
        .args(args)
        .output()
        .expect("dq runs");
    (
        out.status.code().expect("exit code"),
        String::from_utf8(out.stdout).expect("UTF-8 stdout"),
        String::from_utf8(out.stderr).expect("UTF-8 stderr"),
    )
}

/// The messages of the JSON report in `stdout`.
fn messages(stdout: &str) -> Vec<String> {
    let report: serde_json::Value = serde_json::from_str(stdout)
        .unwrap_or_else(|err| panic!("no JSON report ({err}): {stdout}"));
    report["diagnostics"]
        .as_array()
        .unwrap_or_else(|| panic!("no diagnostics: {stdout}"))
        .iter()
        .map(|d| d["message"].as_str().expect("a message").to_owned())
        .collect()
}

/// `(3, "", <compile error>)`: rule `t.r` refused for the external `$ref`
/// `uri`.
fn refused(uri: &str) -> (i32, String, String) {
    (
        3,
        String::new(),
        format!("rule t.r schema failed to compile: external $ref is not supported: {uri}\n"),
    )
}

/// Each schema reaches `maximum: 5` through a reference that stays inside
/// the document; `x: 7` breaks it there (exit 4), `x: 3` passes (exit 0).
#[test]
fn a_ref_to_the_schemas_own_or_an_embedded_id_resolves() {
    for (case, schema, path) in [
        (
            "absolute, to the root's own $id",
            "    $id: https://example.com/s.json\n    $defs: { x: { maximum: 5 } }\n    \
             properties: { x: { $ref: 'https://example.com/s.json#/$defs/x' } }\n",
            "$ref",
        ),
        (
            "relative, to an embedded $id",
            "    $id: https://example.com/root.json\n    $defs:\n      \
             inner: { $id: inner.json, maximum: 5 }\n    properties: { x: { $ref: inner.json } }\n",
            "$ref",
        ),
        (
            "relative, with a fragment into the embedded resource",
            "    $id: https://example.com/root.json\n    $defs:\n      \
             inner: { $id: inner.json, $defs: { m: { maximum: 5 } } }\n    \
             properties: { x: { $ref: 'inner.json#/$defs/m' } }\n",
            "$ref",
        ),
        (
            "relative, to an embedded $id under a root without one",
            "    $defs:\n      inner: { $id: inner.json, maximum: 5 }\n    \
             properties: { x: { $ref: inner.json } }\n",
            "$ref",
        ),
        (
            "absolute, to an embedded file: $id",
            "    $id: https://example.com/root.json\n    $defs:\n      \
             a: { $id: 'file:///etc/inner.json', maximum: 5 }\n    \
             properties: { x: { $ref: 'file:///etc/inner.json' } }\n",
            "$ref",
        ),
        (
            "a $dynamicRef to an embedded $id",
            "    $id: https://example.com/root.json\n    $defs:\n      \
             a: { $id: d.json, $dynamicAnchor: m, maximum: 5 }\n    \
             properties: { x: { $dynamicRef: 'd.json#m' } }\n",
            "$dynamicRef",
        ),
    ] {
        let rule = schema_rule("r", schema);
        let (code, stdout, stderr) = check(&rule, "x: 7\n");
        assert_eq!(
            (code, messages(&stdout), stderr.as_str()),
            (
                4,
                vec![format!(
                    "/properties/x/{path}/maximum: 7 is greater than the maximum of 5"
                )],
                "lint reported 1 error-severity diagnostic(s)\n"
            ),
            "{case}"
        );
        let (code, stdout, stderr) = check(&rule, "x: 3\n");
        assert_eq!(
            (code, messages(&stdout), stderr.as_str()),
            (0, Vec::new(), ""),
            "{case}"
        );
    }
}

/// A reference no `$id` of the document names is external, however close
/// it looks to one that is.
#[test]
fn a_ref_no_embedded_id_names_is_refused() {
    for (case, schema, uri) in [
        (
            "relative, beside a different embedded $id",
            "    $id: https://example.com/root.json\n    $defs:\n      \
             inner: { $id: inner.json, maximum: 5 }\n    properties: { x: { $ref: other.json } }\n",
            "other.json",
        ),
        (
            "absolute, on the root's host",
            "    $id: https://example.com/root.json\n    \
             properties: { x: { $ref: 'https://example.com/other.json' } }\n",
            "https://example.com/other.json",
        ),
        (
            "relative, where the embedded $id resolves elsewhere",
            "    $id: https://example.com/root.json\n    $defs:\n      \
             inner: { $id: 'https://elsewhere.org/inner.json', maximum: 5 }\n    \
             properties: { x: { $ref: inner.json } }\n",
            "inner.json",
        ),
        (
            "to a $id in data, which names no resource",
            "    $id: https://example.com/root.json\n    \
             x-data: { $id: inner.json, maximum: 5 }\n    properties: { x: { $ref: inner.json } }\n",
            "inner.json",
        ),
        (
            "relative, under a root without $id",
            "    properties: { x: { $ref: other.json } }\n",
            "other.json",
        ),
        (
            "file:, no $id names it",
            "    $id: https://example.com/root.json\n    \
             properties: { x: { $ref: 'file:///etc/passwd' } }\n",
            "file:///etc/passwd",
        ),
        (
            "a $dynamicRef outside",
            "    $id: https://example.com/root.json\n    \
             properties: { x: { $dynamicRef: 'https://example.com/other.json' } }\n",
            "https://example.com/other.json",
        ),
    ] {
        assert_eq!(
            check(&schema_rule("r", schema), "x: 7\n"),
            refused(uri),
            "{case}"
        );
    }
}

/// A `$ref`, a `$dynamicRef` and a `$schema` to a socket that listens, and
/// a relative `$ref` under a root `$id` on it: each fails the compile, and
/// the socket gets no connection.
#[test]
fn an_external_ref_never_reaches_the_network() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a local port");
    listener.set_nonblocking(true).expect("non-blocking");
    let host = format!("http://{}", listener.local_addr().expect("local addr"));
    let meta_schema = format!(
        "Resource '{host}/meta' is not present in a registry and retrieving it failed: \
         `resolve-http` feature or a custom resolver is required to resolve external schemas \
         via HTTP"
    );
    for (schema, expected) in [
        (
            format!("    properties: {{ x: {{ $ref: '{host}/s.json#/$defs/x' }} }}\n"),
            refused(&format!("{host}/s.json#/$defs/x")),
        ),
        (
            format!("    properties: {{ x: {{ $dynamicRef: '{host}/d.json' }} }}\n"),
            refused(&format!("{host}/d.json")),
        ),
        (
            format!(
                "    $id: '{host}/root.json'\n    properties: {{ x: {{ $ref: other.json }} }}\n"
            ),
            refused("other.json"),
        ),
        (
            format!("    properties: {{ x: {{ $schema: '{host}/meta', type: string }} }}\n"),
            (
                3,
                String::new(),
                format!("rule t.r schema failed to compile: {meta_schema}\n"),
            ),
        ),
    ] {
        assert_eq!(
            check(&schema_rule("r", &schema), "x: 7\n"),
            expected,
            "{schema}"
        );
    }
    match listener.accept() {
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
        Ok((_, peer)) => panic!("dq connected to {host} from {peer}"),
        Err(err) => panic!("accept failed: {err}"),
    }
}

/// A `$ref` to a file that exists and holds a schema — next to a
/// `schema_file` sidecar, by relative path and by `file:` URI — fails the
/// compile: had dq read it, the rule would compile.
#[test]
fn an_external_ref_never_reads_a_file() {
    let dir = TempDir::new().expect("tempdir");
    let other = dir.path().join("other.schema.json");
    std::fs::write(&other, r#"{"maximum": 5}"#).expect("write other schema");
    let other_uri = format!("file://{}", other.display());
    std::fs::write(dir.path().join("d.yaml"), "x: 7\n").expect("write doc");
    for (name, sidecar, uri) in [
        (
            "relative",
            r#"{"properties": {"x": {"$ref": "other.schema.json"}}}"#.to_owned(),
            "other.schema.json".to_owned(),
        ),
        (
            "file-uri",
            format!(r#"{{"properties": {{"x": {{"$ref": "{other_uri}"}}}}}}"#),
            other_uri.clone(),
        ),
        (
            "file-base",
            format!(
                r#"{{"$id": "file://{}/root.json", "properties": {{"x": {{"$ref": "other.schema.json"}}}}}}"#,
                dir.path().display()
            ),
            "other.schema.json".to_owned(),
        ),
    ] {
        std::fs::write(dir.path().join(format!("{name}.schema.json")), sidecar)
            .expect("write sidecar");
        std::fs::write(
            dir.path().join("r.yml"),
            format!(
                "id: t.r\ndescription: x\nseverity: error\nmatch: {{ format: yaml }}\n\
                 check:\n  schema_file: {name}.schema.json\n"
            ),
        )
        .expect("write rule");
        assert_eq!(
            run_in(dir.path(), &["check", "--rule", "r.yml", "d.yaml"]),
            refused(&uri),
            "{name}"
        );
    }
}
