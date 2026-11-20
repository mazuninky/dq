//! End-to-end CLI smoke tests for the M11 XML format.
//!
//! These spawn the real `dq` binary via `assert_cmd` so they catch contract
//! breaks the in-process tests miss: clap's parser plumbing, the binary's
//! own stderr renderer, exit-code mapping. Mirror of `cli_smoke.rs`.
//!
//! Sanity coverage only — comprehensive testing (golden-file runner,
//! snapshot tests for `convert -F xml` outputs, property tests for
//! round-trip) is the responsibility of a follow-up
//! `rust-cli-test-writer` task.

use std::path::PathBuf;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::NamedTempFile;

/// Path to a fixture file relative to `crates/dq-cli/`.
fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Build a `dq` command with a clean environment. Same discipline as
/// `cli_smoke.rs::dq` — wipes inherited env so the developer's `NO_COLOR`,
/// `CLICOLOR_FORCE`, `RUST_LOG` cannot influence the test.
fn dq() -> Command {
    let mut cmd = Command::cargo_bin("dq").expect("dq binary built");
    cmd.env_clear();
    if let Ok(p) = std::env::var("PATH") {
        cmd.env("PATH", p);
    }
    cmd.env("HOME", "/tmp");
    cmd
}

#[test]
fn smoke_get_pom_xml_project_version_returns_expected_string() {
    // Spec scenario: `dq get pom.xml /project/version` returns "1.2.3".
    // Per the conventional-key mapping, single-element children are wrapped
    // in a one-element Array, so the *real* path is `/project/0/version/0`.
    // The pointer below addresses the array index and the inner #text
    // through which the string surfaces in console output.
    let out = dq()
        .args([
            "get",
            fixture("pom.xml").to_str().unwrap(),
            "/project/0/version/0/#text",
            "--no-color",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).to_string();
    assert!(
        stdout.contains("1.2.3"),
        "stdout must contain version '1.2.3', got: {stdout:?}",
    );
}

#[test]
fn smoke_paths_pom_xml_lists_project_pointer() {
    // `dq paths` must list pointer entries for the XML conventional-key
    // shape. Pinning a couple of well-known pointers keeps the format
    // dispatcher honest without hard-coding the entire enumeration.
    let out = dq()
        .args(["paths", fixture("pom.xml").to_str().unwrap(), "--no-color"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).to_string();
    assert!(
        stdout.contains("/project"),
        "/project pointer must appear in `paths` output, got:\n{stdout}",
    );
    // /#xml carries the parsed declaration (version/encoding/standalone).
    assert!(
        stdout.contains("/#xml"),
        "/#xml declaration pointer must appear in `paths` output, got:\n{stdout}",
    );
}

#[test]
fn smoke_convert_json_to_xml_emits_well_formed_xml() {
    // Spec scenario `convert -F xml is accepted`: produces XML with the
    // root element matching the JSON input shape.
    //
    // We seed a JSON file whose top-level shape matches what the XML
    // writer expects (one root tag wrapped in a one-element Array). The
    // writer's contract is documented in `dq-core/src/parsers/xml.rs`:
    // the top-level map must carry exactly one element-shaped key.
    use std::io::Write;
    let mut json = NamedTempFile::with_suffix(".json").expect("tempfile");
    // {"root": [{"#text": "hello"}]} → <root>hello</root>.
    // Use `br##"..."##` because the literal contains `"#text"` which
    // would otherwise close a single-hash raw string prematurely.
    json.write_all(br##"{"root": [{"#text": "hello"}]}"##)
        .expect("write json");
    let path = json.into_temp_path();

    let out = dq()
        .args(["convert", path.to_str().unwrap(), "-F", "xml", "--no-color"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).to_string();
    assert!(
        stdout.contains("<root>hello</root>") || stdout.contains("<root>hello</root>\n"),
        "convert -F xml must emit well-formed XML for the seeded JSON, got: {stdout:?}",
    );
}

#[test]
fn smoke_convert_dockerfile_format_target_is_rejected_at_clap_layer() {
    // Sanity check that `-F xml` did not accidentally reopen
    // `-F dockerfile` or `-F ignore-list` — those remain rejected at the
    // clap parse step (exit 6) per the spec's MODIFIED requirement.
    dq().args([
        "convert",
        fixture("pom.xml").to_str().unwrap(),
        "-F",
        "dockerfile",
        "--no-color",
    ])
    .assert()
    .failure()
    .stderr(
        predicate::str::contains("invalid value").or(predicate::str::contains("possible values")),
    );
}

#[test]
fn smoke_convert_yaml_to_xml_exit_code() {
    // Selecting `-F xml` from a YAML source goes through the XML writer;
    // this smoke test pins the exit-code envelope (success or
    // `Error::Format` mapped to GENERIC=1) so we don't accidentally
    // regress to a panic / clap-rejection when the XML format set evolves.
    //
    // We deliberately do NOT assert on stderr `tracing::warn!` output —
    // the subscriber is wired up via `tracing_subscriber::fmt`, but
    // pinning its format (color codes, env-filter shape, line layout) is
    // fragile. The exit-code envelope is the stable contract worth pinning.
    let out = dq()
        .args([
            "convert",
            fixture("server_config.yaml").to_str().unwrap(),
            "-F",
            "xml",
            "--no-color",
            // Verbose so any tracing::warn! lines (e.g. lossy-format
            // notice) reach stderr — exit-code semantics are unchanged.
            "-v",
        ])
        .assert();
    // The convert can either succeed (XML writer accepts the shape) or
    // fail with a structured `Error::Format` if the YAML tree doesn't
    // fit the conventional-key shape (one root tag wrapped in a one-
    // element Array). `Error::Format` maps to GENERIC=1 — see
    // `dq-cli/src/exit_code.rs`. Either outcome is acceptable for this
    // smoke test — what we *don't* want is a panic, exit 6 (clap
    // rejection), or an UnsupportedFormat (also 6).
    let status = out.get_output().status.code().unwrap_or(-1);
    assert!(
        matches!(status, 0 | 1 | 7),
        "expected exit 0/1/7 (success or format/write error), got {status}",
    );
}

// --- UTF-8 BOM on XML input ------------------------------------------------
//
// The BOM is kept as `#xml.bom: UTF-8` on read and written back, so the
// file round-trips byte for byte. Expected bytes were checked against the
// dq build on quick-xml 0.40, BOM aside.

/// An XML document with a non-ASCII text node, without its BOM.
const XML_BODY: &[u8] = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<cfg name=\"dq\">\n  <port>8080</port>\n  <tag>caf\xC3\xA9</tag>\n</cfg>\n";

const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

/// `XML_BODY` as written by the XML writer (`convert -F xml`, `fmt -i`),
/// after the BOM when the source had one.
const XML_WRITTEN: &[u8] = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<cfg name=\"dq\">\n  \
<port>8080</port>\n  <tag>caf\xC3\xA9</tag>\n</cfg>\n";

/// Write `XML_BODY`, prefixed with a UTF-8 BOM when `bom` is set, to
/// `cfg.xml` in a fresh tempdir.
fn xml_input(bom: bool) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cfg.xml");
    let mut bytes = if bom { UTF8_BOM.to_vec() } else { Vec::new() };
    bytes.extend_from_slice(XML_BODY);
    std::fs::write(&path, bytes).expect("write xml input");
    (dir, path)
}

/// Stdout of a successful `dq convert <path> -F <format> --no-color`.
fn convert_stdout(path: &std::path::Path, format: &str) -> Vec<u8> {
    let out = dq()
        .args([
            "convert",
            path.to_str().unwrap(),
            "-F",
            format,
            "--no-color",
        ])
        .assert()
        .success()
        .stderr("");
    out.get_output().stdout.clone()
}

#[test]
fn convert_utf8_bom_xml_to_json_reads_the_data_and_names_the_bom() {
    let (_dir, with_bom) = xml_input(true);
    let expected = r##"{
  "#xml": {
    "version": "1.0",
    "encoding": "UTF-8",
    "bom": "UTF-8"
  },
  "cfg": [
    {
      "@attrs": {
        "name": "dq"
      },
      "port": [
        {
          "#text": "8080"
        }
      ],
      "tag": [
        {
          "#text": "café"
        }
      ]
    }
  ]
}
"##;
    let stdout = convert_stdout(&with_bom, "json");
    assert_eq!(String::from_utf8(stdout.clone()).expect("utf-8"), expected);

    let (_dir, without_bom) = xml_input(false);
    assert_eq!(
        String::from_utf8(convert_stdout(&without_bom, "json")).expect("utf-8"),
        expected.replace(",\n    \"bom\": \"UTF-8\"", ""),
        "the BOM must change nothing but `#xml.bom`"
    );
}

#[test]
fn convert_utf8_bom_xml_to_xml_writes_the_bom_back() {
    let (_dir, with_bom) = xml_input(true);
    let stdout = convert_stdout(&with_bom, "xml");
    assert_eq!(
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&[UTF8_BOM, XML_WRITTEN].concat())
    );
    let (_dir, without_bom) = xml_input(false);
    assert_eq!(
        String::from_utf8_lossy(&convert_stdout(&without_bom, "xml")),
        String::from_utf8_lossy(XML_WRITTEN)
    );
}

#[test]
fn fmt_in_place_keeps_the_utf8_bom_of_xml() {
    let (_dir, with_bom) = xml_input(true);
    dq().args(["fmt", with_bom.to_str().unwrap(), "-i", "--no-color"])
        .assert()
        .success();
    let written = std::fs::read(&with_bom).expect("read rewritten file");
    assert_eq!(
        String::from_utf8_lossy(&written),
        String::from_utf8_lossy(&[UTF8_BOM, XML_WRITTEN].concat())
    );
}
