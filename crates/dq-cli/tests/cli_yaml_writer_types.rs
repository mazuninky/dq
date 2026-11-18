//! The native YAML writer through the real binary (`convert -F yaml`,
//! `set --jq -i`, `fix -i`, `get` / `query -F yaml`): a string that a YAML
//! 1.1 reader (PyYAML; go-yaml as in Kubernetes and Helm) would type —
//! `y`, `on`, `1:20`, `2001-12-14`, `017`, … — is single-quoted, as a value
//! and as a key; a `<<` merge key stays plain; a float in exponent form gets
//! a `.` and a signed exponent (`1e16` → `1.0e+16`); an integer up to
//! `u64::MAX` stays a plain number. dq reads each value back as it was.

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

/// `dq -F yaml convert <file>` in `dir`.
fn to_yaml(dir: &TempDir, file: &str) -> String {
    stdout_of(dir, &["-F", "yaml", "convert", file])
}

/// `yaml` as `dq -F json convert` reads it.
fn read_back(yaml: &str) -> serde_json::Value {
    let dir = dir_with(&[("back.yaml", yaml)]);
    serde_json::from_str(&stdout_of(&dir, &["-F", "json", "convert", "back.yaml"]))
        .expect("stdout is JSON")
}

/// Run `dq -i set --jq '.zz = 1' f.yaml` on `src` and return the file.
fn set_jq_in_place(src: &str) -> String {
    let dir = dir_with(&[("f.yaml", src)]);
    dq().current_dir(dir.path())
        .args(["--no-color", "set", "--jq", ".zz = 1", "-i", "f.yaml"])
        .assert()
        .success()
        .stdout("");
    read(&dir, "f.yaml")
}

/// Strings that a YAML 1.1 reader types, as values and as keys. `<<` is a
/// key over a scalar, so it is no merge.
const TYPED_JSON: &str = r#"{"y": "y", "n": "n", "on": "on", "off": "off", "yes": "yes", "=": "=", "1:20": "1:20", "2001-12-14": "2001-12-14", "1_000": "1_000", "0b1": "0b1", "017": "017", "<<": "<<", "plain": "hello"}"#;

/// What the writer makes of [`TYPED_JSON`].
const TYPED_YAML: &str = "'y': 'y'\n'n': 'n'\n'on': 'on'\n'off': 'off'\n'yes': 'yes'\n\
                          '=': '='\n'1:20': '1:20'\n'2001-12-14': '2001-12-14'\n\
                          '1_000': '1_000'\n'0b1': '0b1'\n'017': '017'\n'<<': '<<'\n\
                          plain: hello\n";

#[test]
fn convert_from_json_quotes_strings_a_yaml_1_1_reader_types() {
    // HEAD ab41673 wrote every one of these plain but `017`.
    let dir = dir_with(&[("t.json", TYPED_JSON)]);
    let out = to_yaml(&dir, "t.json");
    assert_eq!(out, TYPED_YAML);
    // dq reads each back as the same string.
    assert_eq!(
        read_back(&out),
        serde_json::from_str::<serde_json::Value>(TYPED_JSON).expect("JSON")
    );
}

#[test]
fn convert_from_toml_quotes_strings_a_yaml_1_1_reader_types() {
    // A TOML date is a string to dq, and is quoted like one.
    let dir = dir_with(&[(
        "t.toml",
        "\"on\" = \"y\"\nn = \"2001-12-14\"\nd = 2001-12-14\n\"<<\" = \"=\"\n\
         plain = \"hello\"\n[off]\nyes = \"017\"\n",
    )]);
    assert_eq!(
        to_yaml(&dir, "t.toml"),
        "'on': 'y'\n'n': '2001-12-14'\nd: '2001-12-14'\n'<<': '='\nplain: hello\n\
         'off':\n  'yes': '017'\n"
    );
}

#[test]
fn ordinary_and_already_quoted_strings_keep_their_spelling() {
    // The strings serde_norway already quoted come out as HEAD wrote them;
    // `"on"` is single-quoted like any string the writer quotes; the rest
    // are plain, as before — a typed word inside a longer string types
    // nothing.
    let dir = dir_with(&[(
        "q.yaml",
        "a: 'true'\nb: '123'\nc: '0o17'\nd: 'null'\ne: '1.5'\nf: \"on\"\ng: '1:20'\n\
         h: hello\ni: yes please\nj: 1:2:3x\nk: 2001-12-14x\nl: ''\nm: \"a'b\"\nyes no: on off\n",
    )]);
    assert_eq!(
        to_yaml(&dir, "q.yaml"),
        "a: 'true'\nb: '123'\nc: '0o17'\nd: 'null'\ne: '1.5'\nf: 'on'\ng: '1:20'\n\
         h: hello\ni: yes please\nj: 1:2:3x\nk: 2001-12-14x\nl: ''\nm: a'b\nyes no: on off\n"
    );
}

#[test]
fn set_jq_in_place_quotes_typed_strings_as_values_and_keys() {
    let src = "'y': 'y'\n'on': 'off'\nt: '1:20'\nd: '2001-12-14'\nu: '1_000'\nb: '0b1'\n\
               o: '017'\n'=': '='\n'<<': x\nplain: hello\n";
    // The jq path re-emits the file: every quoted string stays quoted.
    assert_eq!(set_jq_in_place(src), format!("{src}zz: 1\n"));
}

#[test]
fn fix_in_place_through_fix_jq_quotes_typed_strings() {
    // The check fires on a file without `zz`, so the second run skips it.
    const RULE: &str = r#"id: zz-is-set
description: zz must be set
severity: warn
match:
  format: yaml
check:
  jq: 'select(.zz == null) | .'
  message: 'zz is missing'
fix:
  jq: '.zz = "on"'
"#;
    // Two files, so `fix` prints the bulk summary. `g.yaml` holds `yes`
    // plain: dq reads a string, and the writer quotes it.
    let dir = dir_with(&[
        (".dq/rules/zz-is-set.yml", RULE),
        ("f.yaml", "'y': 'n'\nwhen: '1:20'\nk: hello\n"),
        ("g.yaml", "n: yes\n"),
    ]);
    let fix = |summary: &str| {
        dq().current_dir(dir.path())
            .args(["--no-color", "-i", "fix", "f.yaml", "g.yaml"])
            .assert()
            .success()
            .stdout(summary.to_owned());
        (read(&dir, "f.yaml"), read(&dir, "g.yaml"))
    };
    let fixed = (
        "'y': 'n'\nwhen: '1:20'\nk: hello\nzz: 'on'\n".to_owned(),
        "'n': 'yes'\nzz: 'on'\n".to_owned(),
    );
    assert_eq!(fix("Modified: 2, Skipped: 0, Failed: 0\n"), fixed);
    assert_eq!(fix("Modified: 0, Skipped: 2, Failed: 0\n"), fixed);
}

/// A docker-compose / Helm shaped file: merges over a mapping and over a
/// list of mappings, and two `<<` keys a YAML 1.1 reader cannot merge.
const MERGE_YAML: &str = "base: &b\n  image: x\n  restart: on\nsvc:\n  <<: *b\n  port: 1\n\
                          two:\n  <<: [*b, {extra: y}]\nlit:\n  <<: plain\nmixed:\n  <<: [*b, 1]\n";

#[test]
fn merge_key_over_a_map_or_a_list_of_maps_stays_plain() {
    // dq copies the aliased mapping in under a plain `<<`; a merge key stays
    // plain so a YAML 1.1 reader still merges it. Over a scalar, or a list
    // holding a non-mapping, `<<` is no merge and is quoted like any other
    // typed key (a YAML 1.1 reader fails on the plain form).
    assert_eq!(
        set_jq_in_place(MERGE_YAML),
        "base:\n  image: x\n  restart: 'on'\nsvc:\n  <<:\n    image: x\n    restart: 'on'\n  \
         port: 1\ntwo:\n  <<:\n  - image: x\n    restart: 'on'\n  - extra: 'y'\nlit:\n  \
         '<<': plain\nmixed:\n  '<<':\n  - image: x\n    restart: 'on'\n  - 1\nzz: 1\n"
    );
}

#[test]
fn an_integer_up_to_u64_max_stays_a_plain_int() {
    let dir = dir_with(&[(
        "i.json",
        r#"{"max": 18446744073709551615, "past": 18446744073709551616, "neg": -9223372036854775809}"#,
    )]);
    let out = to_yaml(&dir, "i.json");
    // Past `u64` (or below `i64::MIN`) the literal stays a quoted string —
    // a documented limit: dq's YAML reader (serde_norway) rejects a plain
    // `u128` / `i128`, so a plain spelling would make the file unreadable
    // to dq itself (exit 3).
    assert_eq!(
        out,
        "max: 18446744073709551615\npast: '18446744073709551616'\nneg: '-9223372036854775809'\n"
    );
    let back = dir_with(&[("i.yaml", &out)]);
    assert_eq!(stdout_of(&back, &["type", "i.yaml", "/max"]), "int\n");
    assert_eq!(stdout_of(&back, &["type", "i.yaml", "/past"]), "string\n");
    assert_eq!(stdout_of(&back, &["type", "i.yaml", "/neg"]), "string\n");
}

/// Floats serde_norway writes in exponent form, and `-0.0`.
const FLOATS_JSON: &str =
    r#"{"a": 1e16, "b": -1e-7, "c": 1.5e300, "d": 5e-324, "e": 1.7976931348623157e308, "f": -0.0}"#;

#[test]
fn an_exponent_float_gets_a_dot_and_a_signed_exponent() {
    // HEAD ab41673 wrote `1e16`, `-1e-7`, `1.5e300`, `5e-324`,
    // `1.7976931348623157e308` — strings to a YAML 1.1 reader.
    let dir = dir_with(&[("f.json", FLOATS_JSON)]);
    let out = to_yaml(&dir, "f.json");
    assert_eq!(
        out,
        "a: 1.0e+16\nb: -1.0e-7\nc: 1.5e+300\nd: 5.0e-324\ne: 1.7976931348623157e+308\nf: -0.0\n"
    );
    // dq reads each back as the same float.
    let back = dir_with(&[("f.yaml", &out), ("f.json", FLOATS_JSON)]);
    for key in ["/a", "/b", "/c", "/d", "/e", "/f"] {
        assert_eq!(
            stdout_of(&back, &["type", "f.yaml", key]),
            "float\n",
            "{key}"
        );
        assert_eq!(
            stdout_of(&back, &["get", "f.yaml", key]),
            stdout_of(&back, &["get", "f.json", key]),
            "{key}"
        );
    }
}

#[test]
fn a_float_with_a_dot_and_a_signed_exponent_stays_byte_identical() {
    let src = "a: 1.0e+16\nb: -1.0e-7\nc: 1.5e+300\nd: 5.0e-324\ne: 1.7976931348623157e+308\n\
               f: -0.0\ng: 2.5e-7\nh: 0.1\n";
    assert_eq!(set_jq_in_place(src), format!("{src}zz: 1\n"));
}

#[test]
fn a_big_float_is_written_as_a_yaml_1_1_float() {
    // `1e400`: no `f64` holds it, so dq keeps the literal (a `BigFloat`);
    // HEAD ab41673 wrote `1e+400`, a string to a YAML 1.1 reader.
    let dir = dir_with(&[("b.json", r#"{"e": 1e400}"#)]);
    let out = to_yaml(&dir, "b.json");
    assert_eq!(out, "e: 1.0e+400\n");
    // dq's own reader cannot give the big float back: serde_norway reads a
    // float past `f64` as a string (documented in `yaml_emit.rs`).
    let back = dir_with(&[("b.yaml", &out)]);
    assert_eq!(stdout_of(&back, &["type", "b.yaml", "/e"]), "string\n");

    // A 30-digit π from a jq literal keeps its digits, plain.
    let pi = "3.14159265358979323846264338327";
    let dir = dir_with(&[("p.yaml", "x: 1\n")]);
    dq().current_dir(dir.path())
        .args(["--no-color", "set", "--jq", &format!(".pi = {pi}")])
        .args(["-i", "p.yaml"])
        .assert()
        .success();
    assert_eq!(read(&dir, "p.yaml"), format!("x: 1\npi: {pi}\n"));
    assert_eq!(stdout_of(&dir, &["type", "p.yaml", "/pi"]), "float\n");
    assert_eq!(
        stdout_of(
            &dir,
            &["query", "-F", "yaml", &format!("{pi}, 1e400"), "p.yaml"]
        ),
        format!("- {pi}\n- 1.0e+400\n")
    );
}

#[test]
fn data_holding_the_placeholder_nonce_is_written_exactly() {
    // `Zq`, `Zj`, `Zx` — the first nonces the writer tries — occur as keys
    // and inside strings, next to values the writer quotes; strings shaped
    // like a placeholder (`Zqxx`, `Zjxx`, `Zqxxxx`) stay as they are.
    let src = r#"{"Zq": "on", "Zqx": "Zj", "Zqxxxx": ["y", "Zqxx", "Zjxx"], "on": "Zqxxxx", "k": "Zj Zqx", "Zjx": "off", "n": 1e16, "Zx": "1:20", "Zjxxx": {"yes": "no"}, "Zz": "Zz"}"#;
    let dir = dir_with(&[("z.json", src)]);
    let out = to_yaml(&dir, "z.json");
    assert_eq!(
        out,
        "Zq: 'on'\nZqx: Zj\nZqxxxx:\n- 'y'\n- Zqxx\n- Zjxx\n'on': Zqxxxx\nk: Zj Zqx\n\
         Zjx: 'off'\n'n': 1.0e+16\nZx: '1:20'\nZjxxx:\n  'yes': 'no'\nZz: Zz\n"
    );
    // dq reads it back as the same data; the float as the same `f64`
    // (`dq -F json` spells it `10000000000000000.0`).
    let mut back = read_back(&out);
    let mut want: serde_json::Value = serde_json::from_str(src).expect("JSON");
    let n = back.as_object_mut().expect("a map").remove("n");
    want.as_object_mut().expect("a map").remove("n");
    assert_eq!(n.and_then(|n| n.as_f64()), Some(1e16));
    assert_eq!(back, want);
}

/// A string of `len` bytes that a YAML 1.1 reader takes for an int
/// (`1_0_0…`) and serde_norway writes plain.
fn int_like_key(len: usize) -> String {
    let mut key = String::from("1");
    while key.len() < len {
        key.push_str(if len - key.len() >= 2 { "_0" } else { "0" });
    }
    key
}

#[test]
fn a_long_quoted_key_keeps_its_value_in_place() {
    // Quoted, a key of 127 bytes is 129: the emitter writes up to 128 bytes
    // of key content as a simple key and longer ones as a `? ` complex key,
    // as it did at HEAD for the plain key. The value after each stays with
    // its key.
    let lens = [126, 127, 128, 129, 130];
    let mut src = serde_json::Map::new();
    let mut want = String::new();
    for len in lens {
        let key = int_like_key(len);
        assert_eq!(key.len(), len);
        src.insert(key.clone(), serde_json::json!(len));
        src.insert(format!("s{len}"), serde_json::json!("y"));
        if len <= 128 {
            want.push_str(&format!("'{key}': {len}\n"));
        } else {
            want.push_str(&format!("? '{key}'\n: {len}\n"));
        }
        want.push_str(&format!("s{len}: 'y'\n"));
    }
    let src = serde_json::Value::Object(src);
    let dir = dir_with(&[("k.json", &src.to_string())]);
    let out = to_yaml(&dir, "k.json");
    assert_eq!(out, want);
    assert_eq!(read_back(&out), src);

    // Over a mapping the value's lines stay under the key too.
    let key = int_like_key(130);
    let dir = dir_with(&[("m.json", &format!(r#"{{"{key}": {{"v": "on"}}, "k": 1}}"#))]);
    assert_eq!(
        to_yaml(&dir, "m.json"),
        format!("? '{key}'\n: v: 'on'\nk: 1\n")
    );
}

#[test]
fn every_document_of_a_stream_is_quoted() {
    let dir = dir_with(&[("m.yaml", "y: on\nb: 1e16\n---\nn: 2001-12-14\nf: -1e-7\n")]);
    assert_eq!(
        to_yaml(&dir, "m.yaml"),
        "'y': 'on'\nb: 1.0e+16\n---\n'n': '2001-12-14'\nf: -1.0e-7\n"
    );
}

#[test]
fn sort_keys_output_is_quoted() {
    let dir = dir_with(&[("s.yaml", "z: 'off'\nb: {y: 1, a: '017'}\na: 1e16\n")]);
    let want = "a: 1.0e+16\nb:\n  a: '017'\n  'y': 1\nz: 'off'\n";
    assert_eq!(
        stdout_of(&dir, &["-F", "yaml", "convert", "--sort-keys", "s.yaml"]),
        want
    );
    assert_eq!(stdout_of(&dir, &["fmt", "--sort-keys", "s.yaml"]), want);
}

#[test]
fn get_and_query_yaml_output_quote_a_root_scalar() {
    let dir = dir_with(&[
        ("r.yaml", "'1:20'\n"),
        (
            "m.yaml",
            "z: 'off'\nc: 'y'\nf: 1e16\nu: 18446744073709551615\n",
        ),
    ]);
    // A root scalar, and scalars addressed by a pointer.
    assert_eq!(to_yaml(&dir, "r.yaml"), "'1:20'\n");
    assert_eq!(
        stdout_of(&dir, &["-F", "yaml", "get", "r.yaml", ""]),
        "'1:20'\n"
    );
    assert_eq!(
        stdout_of(&dir, &["-F", "yaml", "get", "m.yaml", "/z"]),
        "'off'\n"
    );
    assert_eq!(
        stdout_of(&dir, &["-F", "yaml", "get", "m.yaml", "/c"]),
        "'y'\n"
    );
    assert_eq!(
        stdout_of(&dir, &["-F", "yaml", "get", "m.yaml", "/f"]),
        "1.0e+16\n"
    );
    // A query's YAML output is the sequence of its results.
    assert_eq!(
        stdout_of(&dir, &["query", "-F", "yaml", ".z, .c, .f, .u", "m.yaml"]),
        "- 'off'\n- 'y'\n- 1.0e+16\n- 18446744073709551615\n"
    );
    assert_eq!(
        stdout_of(&dir, &["query", "-F", "yaml", "{on: .z}", "m.yaml"]),
        "- 'on': 'off'\n"
    );
}
