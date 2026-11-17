//! Integration test for the textual-edit POC. This is the gate referenced
//! in `openspec/changes/add-safe-writes/design.md` D11 criterion 2:
//! single-scalar mutation must be byte-perfect (exactly one line removed
//! and one line inserted) on all 5 representative fixtures.
//!
//! If any fixture FAILs, this test FAILs — that's the spike's go/no-go.

use assert_cmd::Command;
use predicates::prelude::*;

/// `(fixture_filename, pointer, new_value_literal)` triples for the 5
/// fixtures. The `new_value` is passed verbatim — the spike does not do
/// quote-style detection, so for fixture (e) we pass the value
/// already wrapped in double quotes to match the source style.
const CASES: &[(&str, &str, &str)] = &[
    ("a_k8s_with_comments.yaml", "/spec/replicas", "5"),
    ("b_helm_values.yaml", "/image/tag", "v2.0.0"),
    ("c_anchors_and_merge.yaml", "/defaults/timeout", "60"),
    ("d_multi_doc.yaml", "/1/spec/ports/0/port", "8090"),
    ("e_hugo_frontmatter.yaml", "/title", "\"Updated\""),
];

#[test]
fn assert_byte_perfect_5_fixtures() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let mut failures = Vec::new();

    for (fixture, pointer, new_value) in CASES {
        let fixture_path = format!("{manifest_dir}/fixtures/{fixture}");
        let result = Command::cargo_bin("saphyr-spike")
            .expect("binary built")
            .args(["assert-byte-perfect", &fixture_path, pointer, new_value])
            .assert();

        // We don't `.success()` here so we can collect all failures and
        // report them in one go (better signal than the first failure).
        let output = result.get_output();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !output.status.success() || !stdout.contains("PASS") {
            failures.push(format!(
                "  {fixture} {pointer}={new_value}\n    \
                 exit={status:?}\n    \
                 stdout={stdout}\n    \
                 stderr={stderr}",
                status = output.status.code()
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "byte-perfect mutation failed for {} of {} fixtures:\n{}",
        failures.len(),
        CASES.len(),
        failures.join("\n"),
    );
}

/// Sanity: the binary exits non-zero on a missing pointer.
#[test]
fn missing_pointer_exits_nonzero() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let fixture_path = format!("{manifest_dir}/fixtures/a_k8s_with_comments.yaml");

    Command::cargo_bin("saphyr-spike")
        .expect("binary built")
        .args(["mutate", &fixture_path, "/no/such/key", "x"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("pointer not found"));
}
