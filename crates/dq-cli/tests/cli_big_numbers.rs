//! Numbers no `i64` / `u64` / `f64` holds, through the real binary.
//!
//! YAML and JSON readers keep such a number exact — an integer past `u64`
//! or below `i64::MIN`, a 42-digit integer, `1e400`, a float literal with
//! more digits than an `f64` (`0.1000000000000000000000000001`) — and every
//! command that reads, rewrites or compares a document keeps it a number of
//! the same value. TOML, HCL and TOON output refuse such a number (exit 1)
//! rather than writing another value.
//!
//! Each test spawns `dq` through `assert_cmd`, so the file can be run
//! against another build by pointing `CARGO_BIN_EXE_dq` at it.

use std::process::Output;

use assert_cmd::Command;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

const U64_PLUS_ONE: &str = "18446744073709551616";
const I64_MIN_MINUS: &str = "-18446744073709551617";
const DIGITS_42: &str = "123456789012345678901234567890123456789012";
const LONG_FLOAT: &str = "0.1000000000000000000000000001";

/// `dq` with a clean environment.
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

fn read(dir: &TempDir, name: &str) -> String {
    std::fs::read_to_string(dir.path().join(name)).expect("read back")
}

/// The names in `dir`, sorted.
fn listing(dir: &TempDir) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir.path())
        .expect("read dir")
        .map(|e| e.expect("entry").file_name().into_string().expect("UTF-8"))
        .collect();
    names.sort();
    names
}

/// `dq --no-color <args>` in `dir`.
fn run(dir: &TempDir, args: &[&str]) -> Output {
    dq().current_dir(dir.path())
        .arg("--no-color")
        .args(args)
        .output()
        .expect("dq runs")
}

/// Stdout of a successful `dq --no-color <args>` in `dir`.
fn stdout_of(dir: &TempDir, args: &[&str]) -> String {
    let out = run(dir, args);
    assert!(
        out.status.success(),
        "{args:?}: exit {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("UTF-8 stdout")
}

/// `(exit code, stdout, stderr)` of a failing `dq --no-color <args>`.
fn failure_of(dir: &TempDir, args: &[&str]) -> (Option<i32>, String, String) {
    let out = run(dir, args);
    assert!(!out.status.success(), "{args:?} succeeded");
    (
        out.status.code(),
        String::from_utf8(out.stdout).expect("UTF-8 stdout"),
        String::from_utf8(out.stderr).expect("UTF-8 stderr"),
    )
}

// ---- reading ----------------------------------------------------------

#[test]
fn get_query_and_convert_print_each_big_number_exact() {
    // (file, source, what `get` / `query` print, the JSON and YAML spelling)
    let cases = [
        (
            "u.yaml",
            format!("a: {U64_PLUS_ONE}\n"),
            U64_PLUS_ONE,
            U64_PLUS_ONE,
            U64_PLUS_ONE,
        ),
        (
            "n.yaml",
            format!("a: {I64_MIN_MINUS}\n"),
            I64_MIN_MINUS,
            I64_MIN_MINUS,
            I64_MIN_MINUS,
        ),
        (
            "b.yaml",
            format!("a: {DIGITS_42}\n"),
            DIGITS_42,
            DIGITS_42,
            DIGITS_42,
        ),
        (
            "e.yaml",
            "a: 1e400\n".to_owned(),
            "1e+400",
            "1e400",
            "1.0e+400",
        ),
        (
            "ne.yaml",
            "a: -1e400\n".to_owned(),
            "-1e+400",
            "-1e400",
            "-1.0e+400",
        ),
        (
            "f.json",
            format!("{{\"a\": {LONG_FLOAT}}}"),
            LONG_FLOAT,
            LONG_FLOAT,
            LONG_FLOAT,
        ),
    ];
    for (file, src, printed, json, yaml) in cases {
        let dir = dir_with(&[(file, &src)]);
        assert_eq!(
            stdout_of(&dir, &["get", file, "/a"]),
            format!("{printed}\n")
        );
        assert_eq!(
            stdout_of(&dir, &["query", ".a", file]),
            format!("{printed}\n")
        );
        assert_eq!(stdout_of(&dir, &["query", ".a | type", file]), "number\n");
        let converted = stdout_of(&dir, &["-F", "json", "convert", file]);
        if file.ends_with(".json") {
            // JSON to JSON copies the source bytes.
            assert_eq!(converted, format!("{src}\n"), "{file}");
        } else {
            assert_eq!(converted, format!("{{\n  \"a\": {json}\n}}\n"), "{file}");
        }
        assert_eq!(
            stdout_of(&dir, &["-F", "yaml", "convert", file]),
            format!("a: {yaml}\n"),
            "{file}"
        );
    }
}

/// The scan that decides whether a YAML file needs its numbers re-read
/// looks for 16 digits in a run or a 3-digit exponent: below that every
/// literal fits an `f64` as written, at it some do and some do not.
#[test]
fn a_yaml_float_is_kept_exact_from_16_digits_or_a_3_digit_exponent() {
    let src = "\
# a long id in a comment: 12345678901234567890
d15: 0.123456789012345
d16fit: 1.234567890123457
d16lost: 9999999999999.999
e2: 1.5e99
e3fit: 1e308
e3past: 1.8e308
sub: 4.9e-324
under: 1e-400
id: abc12345678901234567890
ver: 1.2.3.4.5.6.7.8.9.0.1.2.3.4.5.6
";
    let dir = dir_with(&[("f.yaml", src)]);
    for (key, printed) in [
        // Fit an `f64`: printed as the `f64`.
        ("/d15", "0.123456789012345"),
        ("/d16fit", "1.234567890123457"),
        ("/e2", "1.5e+99"),
        ("/e3fit", "1e+308"),
        // Do not: the literal (the nearest `f64` is 9999999999999.998,
        // infinity, 5e-324 and 0.0).
        ("/d16lost", "9999999999999.999"),
        ("/e3past", "1.8e+308"),
        ("/sub", "4.9e-324"),
        ("/under", "1e-400"),
    ] {
        assert_eq!(
            stdout_of(&dir, &["get", "f.yaml", key]),
            format!("{printed}\n"),
            "{key}"
        );
        assert_eq!(
            stdout_of(&dir, &["type", "f.yaml", key]),
            "float\n",
            "{key}"
        );
    }
    // Long digit runs that are no number stay strings.
    for key in ["/id", "/ver"] {
        assert_eq!(
            stdout_of(&dir, &["type", "f.yaml", key]),
            "string\n",
            "{key}"
        );
    }
    assert_eq!(
        stdout_of(&dir, &["get", "f.yaml", "/ver"]),
        "1.2.3.4.5.6.7.8.9.0.1.2.3.4.5.6\n"
    );
}

#[test]
fn aliases_and_local_or_float_tags_keep_a_big_number_and_quotes_keep_a_string() {
    let src = format!(
        "\
anchor: &big {DIGITS_42}
alias: *big
seq: &s [1e400, {LONG_FLOAT}]
seqalias: *s
local: !foo 1e400
float: !!float {LONG_FLOAT}
q1: '{U64_PLUS_ONE}'
q2: \"1e400\"
q3: !!str 1e400
q4: !!str {DIGITS_42}
lz: 000{U64_PLUS_ONE}
{LONG_FLOAT}: k
"
    );
    let dir = dir_with(&[("t.yaml", &src)]);
    assert_eq!(
        stdout_of(&dir, &["-F", "json", "convert", "t.yaml"]),
        format!(
            "\
{{
  \"anchor\": {DIGITS_42},
  \"alias\": {DIGITS_42},
  \"seq\": [
    1e400,
    {LONG_FLOAT}
  ],
  \"seqalias\": [
    1e400,
    {LONG_FLOAT}
  ],
  \"local\": 1e400,
  \"float\": {LONG_FLOAT},
  \"q1\": \"{U64_PLUS_ONE}\",
  \"q2\": \"1e400\",
  \"q3\": \"1e400\",
  \"q4\": \"{DIGITS_42}\",
  \"lz\": \"000{U64_PLUS_ONE}\",
  \"{LONG_FLOAT}\": \"k\"
}}
"
        )
    );
    assert_eq!(
        stdout_of(&dir, &["query", "[.[] | type] | join(\" \")", "t.yaml"]),
        "number number array array number number string string string string string string\n"
    );
    // The big key reads back as written, so a pointer finds it.
    assert_eq!(
        stdout_of(&dir, &["get", "t.yaml", &format!("/{LONG_FLOAT}")]),
        "k\n"
    );
}

/// A duplicate key past `u64` fails as any duplicate does, in
/// serde_norway's words; two spellings of one big float are two keys (dq
/// keys are strings).
#[test]
fn a_duplicate_big_key_fails_like_any_duplicate_key() {
    let dir = dir_with(&[
        (
            "big.yaml",
            "x:\n  18446744073709551616: a\n  18446744073709551616: b\n",
        ),
        ("small.yaml", "x:\n  1: a\n  1: b\n"),
        ("same.yaml", "1e400: a\n1e400: b\n"),
        ("spellings.yaml", "1e400: a\n1.0e+400: b\n"),
    ]);
    for (file, key) in [("big.yaml", U64_PLUS_ONE), ("small.yaml", "1")] {
        let (code, stdout, stderr) = failure_of(&dir, &["get", file, "/x"]);
        assert_eq!(code, Some(3), "{file}");
        assert_eq!(stdout, "", "{file}");
        assert_eq!(
            stderr,
            format!(
                "parse error in {file}: x: duplicate entry with key {key} at line 2 column 3\n"
            )
        );
    }
    let (code, _, stderr) = failure_of(&dir, &["get", "same.yaml", "/"]);
    assert_eq!(code, Some(3));
    assert_eq!(
        stderr,
        "parse error in same.yaml: duplicate entry with key \"1e400\"\n"
    );
    assert_eq!(
        stdout_of(&dir, &["-F", "json", "-q", "convert", "spellings.yaml"]),
        "{\n  \"1e400\": \"a\",\n  \"1.0e+400\": \"b\"\n}\n"
    );
}

#[test]
fn every_document_of_a_stream_keeps_its_big_numbers() {
    let src = format!(
        "---\na: 1e400  # c0\nb: {LONG_FLOAT}\n---\nx: 1\ny: {DIGITS_42}\nu: {U64_PLUS_ONE}\n"
    );
    let dir = dir_with(&[("s.yaml", &src)]);
    assert_eq!(
        stdout_of(&dir, &["--doc", "0", "get", "s.yaml", "/b"]),
        format!("{LONG_FLOAT}\n")
    );
    assert_eq!(
        stdout_of(&dir, &["--doc", "1", "get", "s.yaml", "/y"]),
        format!("{DIGITS_42}\n")
    );
    assert_eq!(
        stdout_of(
            &dir,
            &["--doc", "1", "-F", "json", "-q", "convert", "s.yaml"]
        ),
        format!("{{\n  \"x\": 1,\n  \"y\": {DIGITS_42},\n  \"u\": {U64_PLUS_ONE}\n}}\n")
    );
    // A textual edit of one document keeps the other byte for byte.
    assert_eq!(
        stdout_of(&dir, &["set", "s.yaml", "/1/z", "1"]),
        format!("{src}z: 1\n")
    );
    // A jq edit of one document re-emits the stream; the other document's
    // numbers stay exact.
    assert_eq!(
        stdout_of(&dir, &["--doc", "1", "set", "--jq", ".z = 1", "s.yaml"]),
        format!(
            "a: 1.0e+400\nb: {LONG_FLOAT}\n---\nx: 1\n'y': {DIGITS_42}\nu: {U64_PLUS_ONE}\nz: 1\n"
        )
    );
}

// ---- rewriting --------------------------------------------------------

const BIG_YAML: &str = "\
# head
u: 18446744073709551616  # u
n: -18446744073709551617
b: 123456789012345678901234567890123456789012
e: 1e400
ne: -1e400
f: 0.1000000000000000000000000001
x: 1
";

#[test]
fn set_jq_keeps_the_untouched_big_numbers_exact() {
    let dir = dir_with(&[
        ("big.yaml", BIG_YAML),
        (
            "big.json",
            "{\"u\": 18446744073709551616, \"f\": 0.1000000000000000000000000001, \"e\": 1e400}\n",
        ),
    ]);
    assert_eq!(
        stdout_of(&dir, &["set", "--jq", ".y = 1", "big.yaml"]),
        format!(
            "u: {U64_PLUS_ONE}\n'n': {I64_MIN_MINUS}\nb: {DIGITS_42}\ne: 1.0e+400\nne: -1.0e+400\nf: {LONG_FLOAT}\nx: 1\n'y': 1\n"
        )
    );
    assert_eq!(
        stdout_of(&dir, &["set", "--jq", ".y = 1", "big.json"]),
        format!(
            "{{\n  \"u\": {U64_PLUS_ONE},\n  \"f\": {LONG_FLOAT},\n  \"e\": 1e+400,\n  \"y\": 1\n}}"
        )
    );
    // In place, and read back as the same numbers.
    dq().current_dir(dir.path())
        .args(["--no-color", "set", "--jq", ".y = 1", "-i", "big.yaml"])
        .assert()
        .success()
        .stdout("");
    for (key, printed) in [
        ("/u", U64_PLUS_ONE),
        ("/n", I64_MIN_MINUS),
        ("/b", DIGITS_42),
        ("/e", "1.0e+400"),
        ("/f", LONG_FLOAT),
    ] {
        assert_eq!(
            stdout_of(&dir, &["get", "big.yaml", key]),
            format!("{printed}\n")
        );
    }
}

#[test]
fn textual_set_and_del_next_to_big_numbers_keep_the_file_and_its_comments() {
    let dir = dir_with(&[("big.yaml", BIG_YAML)]);
    assert_eq!(
        stdout_of(&dir, &["set", "big.yaml", "/x", "2"]),
        BIG_YAML.replace("x: 1\n", "x: 2\n")
    );
    assert_eq!(
        stdout_of(&dir, &["del", "big.yaml", "/x"]),
        BIG_YAML.replace("x: 1\n", "")
    );
    dq().current_dir(dir.path())
        .args(["--no-color", "del", "-i", "big.yaml", "/x"])
        .assert()
        .success();
    assert_eq!(read(&dir, "big.yaml"), BIG_YAML.replace("x: 1\n", ""));
}

#[test]
fn set_writes_a_big_number_as_a_number() {
    let dir = dir_with(&[("s.yaml", "x: 1  # c\n")]);
    for (value, written, type_name) in [
        (U64_PLUS_ONE, U64_PLUS_ONE, "int\n"),
        ("1e400", "1.0e+400", "float\n"),
        (
            "0.1000000000000000000000000002",
            "0.1000000000000000000000000002",
            "float\n",
        ),
    ] {
        assert_eq!(
            stdout_of(&dir, &["set", "s.yaml", "/x", value]),
            format!("x: {written}  # c\n"),
            "{value}"
        );
        let edited = dir_with(&[("s.yaml", "x: 1  # c\n")]);
        dq().current_dir(edited.path())
            .args(["--no-color", "set", "-i", "s.yaml", "/x", value])
            .assert()
            .success()
            .stdout("");
        assert_eq!(read(&edited, "s.yaml"), format!("x: {written}  # c\n"));
        assert_eq!(stdout_of(&edited, &["type", "s.yaml", "/x"]), type_name);
    }
}

/// `dq set h.json /b 0.1000000000000000000000000002` wrote `0.1` and
/// exited 0: the literal was read as the nearest `f64`, and the read-back
/// check compared that `f64` with itself.
#[test]
fn set_on_json_writes_a_float_literal_longer_than_f64_as_written() {
    let src = "{\"a\": 1, \"b\": 0.1}\n";
    let want = "{\"a\": 1, \"b\": 0.1000000000000000000000000002}\n";
    let dir = dir_with(&[("h.json", src)]);
    assert_eq!(
        stdout_of(
            &dir,
            &["set", "h.json", "/b", "0.1000000000000000000000000002"]
        ),
        want
    );
    dq().current_dir(dir.path())
        .args([
            "--no-color",
            "set",
            "-i",
            "h.json",
            "/b",
            "0.1000000000000000000000000002",
        ])
        .assert()
        .success()
        .stdout("");
    assert_eq!(read(&dir, "h.json"), want);
    assert_eq!(
        stdout_of(&dir, &["get", "h.json", "/b"]),
        "0.1000000000000000000000000002\n"
    );
}

// ---- refusing ---------------------------------------------------------

#[test]
fn toml_hcl_and_toon_output_refuse_a_number_they_cannot_hold() {
    let dir = dir_with(&[
        ("n.json", "{\"a\": [18446744073709551616], \"b\": 1}\n"),
        ("e.json", "{\"a\": 1e400}\n"),
        ("f.json", "{\"a\": 0.1000000000000000000000000001}\n"),
        ("b.yaml", "a: 123456789012345678901234567890123456789012\n"),
    ]);
    for (format, name) in [("toml", "TOML"), ("hcl", "HCL"), ("toon", "TOON")] {
        for (file, literal) in [
            ("n.json", U64_PLUS_ONE),
            ("e.json", "1e+400"),
            ("f.json", LONG_FLOAT),
            ("b.yaml", DIGITS_42),
        ] {
            let (code, stdout, stderr) = failure_of(&dir, &["-q", "-F", format, "convert", file]);
            assert_eq!(code, Some(1), "{format} {file}");
            assert_eq!(stdout, "", "{format} {file}");
            assert_eq!(
                stderr,
                format!(
                    "{format} format error: {name} cannot represent arbitrary-precision number {literal}\n"
                ),
            );
        }
    }
    let (code, stdout, stderr) = failure_of(&dir, &["query", "-F", "toon", ".", "e.json"]);
    assert_eq!(code, Some(1));
    assert_eq!(stdout, "");
    assert_eq!(
        stderr,
        "toon format error: TOON cannot represent arbitrary-precision number 1e+400\n"
    );
}

/// `convert -i` writes the converted file and removes the source; a
/// refused number leaves the source as it was and writes nothing.
#[test]
fn convert_in_place_to_toml_or_hcl_writes_nothing_on_a_big_number() {
    let src = "{\"a\": 18446744073709551616}\n";
    for format in ["toml", "hcl"] {
        let dir = dir_with(&[("n.json", src)]);
        let (code, stdout, _) = failure_of(&dir, &["-F", format, "convert", "-i", "n.json"]);
        assert_eq!(code, Some(1), "{format}");
        assert_eq!(stdout, "", "{format}");
        assert_eq!(listing(&dir), ["n.json"], "{format}");
        assert_eq!(read(&dir, "n.json"), src, "{format}");
    }
}

// ---- comparing --------------------------------------------------------

#[test]
fn diff_prints_big_numbers_as_numbers() {
    let dir = dir_with(&[
        ("d1.json", "{\"a\": 1, \"b\": 2}\n"),
        (
            "d2.json",
            "{\"a\": 0.1000000000000000000000000002, \"b\": 2, \"c\": 18446744073709551616, \"e\": 1e400}\n",
        ),
        ("d1.yaml", "a: 1\n"),
        ("d2.yaml", "a: 18446744073709551616\nb: 1e400\n"),
    ]);
    assert_eq!(
        stdout_of(&dir, &["diff", "d1.json", "d2.json"]),
        "\
[
  {
    \"op\": \"add\",
    \"path\": \"/c\",
    \"value\": 18446744073709551616
  },
  {
    \"op\": \"add\",
    \"path\": \"/e\",
    \"value\": 1e+400
  },
  {
    \"op\": \"replace\",
    \"path\": \"/a\",
    \"value\": 0.1000000000000000000000000002
  }
]
"
    );
    assert_eq!(
        stdout_of(&dir, &["diff", "d1.yaml", "d2.yaml"]),
        "\
[
  {
    \"op\": \"add\",
    \"path\": \"/b\",
    \"value\": 1e+400
  },
  {
    \"op\": \"replace\",
    \"path\": \"/a\",
    \"value\": 18446744073709551616
  }
]
"
    );
    // Two spellings of one number are no difference.
    let same = dir_with(&[("x.yaml", "a: 1e400\n"), ("y.json", "{\"a\": 1.0e+400}\n")]);
    assert_eq!(stdout_of(&same, &["diff", "x.yaml", "y.json"]), "[]\n");
}

#[test]
fn a_patch_test_op_compares_a_big_number_by_its_value() {
    let dir = dir_with(&[("p.yaml", "a: 1e400\n"), ("p.json", "{\"a\": 1e400}\n")]);
    let ops = |value: &str| {
        format!(
            r#"[{{"op":"test","path":"/a","value":{value}}},{{"op":"replace","path":"/a","value":2}}]"#
        )
    };
    assert_eq!(
        stdout_of(&dir, &["patch", "p.yaml", &ops("1e+400")]),
        "a: 2\n"
    );
    assert_eq!(
        stdout_of(&dir, &["patch", "p.json", &ops("1.0e+400")]),
        "{\"a\": 2}\n"
    );
    for (file, got) in [("p.yaml", "1e400"), ("p.json", "1e+400")] {
        let (code, stdout, stderr) = failure_of(&dir, &["patch", file, &ops("1e401")]);
        assert_eq!(code, Some(1), "{file}");
        assert_eq!(stdout, "", "{file}");
        assert_eq!(
            stderr,
            format!("RFC 6902 test op failed at /a: expected 1e+401, got {got}\n")
        );
    }
}

// ---- other readers ----------------------------------------------------

/// The YAML writer's big numbers load in PyYAML (a YAML 1.1 reader) with
/// the type dq gives them: past `u64` an int, past `f64` a float, a
/// number-like string a string. A one-off oracle: skipped, with a notice,
/// where `python3` with PyYAML is missing.
#[test]
fn pyyaml_reads_the_yaml_writers_big_numbers_with_their_types() {
    let probe = std::process::Command::new("python3")
        .args(["-I", "-c", "import yaml"])
        .output();
    if !probe.is_ok_and(|out| out.status.success()) {
        eprintln!("skipped: python3 with PyYAML is not available");
        return;
    }
    let dir = dir_with(&[(
        "py.json",
        "{\"u\": 18446744073709551616, \"n\": -9223372036854775809, \"b\": 123456789012345678901234567890123456789012, \"e\": 1e400, \"ne\": -1e400, \"f\": 0.1000000000000000000000000001, \"t\": 1e-400, \"s1\": \"18446744073709551616\", \"s2\": \"1e400\", \"s3\": \"1.0e+400\", \"s4\": \"0.1000000000000000000000000001\"}\n",
    )]);
    let yaml = stdout_of(&dir, &["-F", "yaml", "convert", "py.json"]);
    std::fs::write(dir.path().join("py.yaml"), &yaml).expect("write YAML");
    let out = std::process::Command::new("python3")
        .args([
            "-I",
            "-c",
            "import sys, yaml\n\
             d = yaml.safe_load(open(sys.argv[1]))\n\
             print(' '.join(k + '=' + type(v).__name__ for k, v in d.items()))\n\
             print(d['u'], d['n'], d['b'])",
        ])
        .arg(dir.path().join("py.yaml"))
        .output()
        .expect("python3 runs");
    assert!(
        out.status.success(),
        "{yaml}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8(out.stdout).expect("UTF-8"),
        format!(
            "u=int n=int b=int e=float ne=float f=float t=float s1=str s2=str s3=str s4=str\n\
             {U64_PLUS_ONE} -9223372036854775809 {DIGITS_42}\n"
        ),
        "{yaml}"
    );
}
