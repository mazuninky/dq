//! Golden output of `dq -F toon convert` on top of `toon-format` 0.7, which
//! emits TOON spec v4.
//!
//! The bump from `toon-format` 0.4 changed the empty-array form, the quoting
//! rules and control-character escapes. Each test spawns the real binary and
//! compares stdout byte for byte; the comment next to each case shows what
//! 0.4 printed.

use std::io::Write as _;

use assert_cmd::Command;
use tempfile::{NamedTempFile, TempPath};

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

/// `dq -F toon convert` of the JSON document `json`; asserts exit 0 and
/// returns stdout.
fn to_toon(json: &str) -> String {
    let mut tmp = NamedTempFile::with_suffix(".json").expect("tempfile");
    tmp.write_all(json.as_bytes()).expect("write tempfile");
    let path: TempPath = tmp.into_temp_path();
    let out = dq()
        .args([
            "-F",
            "toon",
            "convert",
            path.to_str().unwrap(),
            "--no-color",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(out).expect("TOON output is UTF-8")
}

#[test]
fn toon_empty_array_is_bracket_pair() {
    // 0.4: `roles[0]:`.
    assert_eq!(to_toon(r#"{"roles": []}"#), "roles: []\n");
}

#[test]
fn toon_nested_uniform_array_gets_tabular_header() {
    // Unchanged from 0.4; pinned so a writer change cannot drift silently.
    assert_eq!(
        to_toon(r#"{"team": {"members": [{"id": 1, "name": "ann"}, {"id": 2, "name": "bob"}]}}"#),
        "team:\n  members[2]{id,name}:\n    1,ann\n    2,bob\n",
    );
}

#[test]
fn toon_quotes_values_and_keys_that_v4_reserves() {
    // 0.4 left all three unquoted: `tag: #bob`, `score: +1`, `ключ: v`.
    // The pair: a plain ASCII key and value stay bare.
    assert_eq!(
        to_toon(r##"{"tag": "#bob", "score": "+1", "ключ": "v", "name": "bob"}"##),
        "tag: \"#bob\"\nscore: \"+1\"\n\"ключ\": v\nname: bob\n",
    );
}

#[test]
fn toon_escapes_control_characters() {
    // 0.4 wrote U+0001 as a raw byte; v4 escapes it as `\u0001`. Tab, line
    // feed, carriage return, quote and backslash use their short escapes.
    assert_eq!(
        to_toon(r#"{"ctl": "a\tb\nc\u0001d\"e\\f\r"}"#),
        "ctl: \"a\\tb\\nc\\u0001d\\\"e\\\\f\\r\"\n",
    );
}
