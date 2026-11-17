//! Golden-file runner: every fixture under `tests/fixtures/golden/` is fed
//! through `dq paths --no-color` and compared against a snapshot.
//!
//! The point: a regression in any parser (YAML/JSON/TOML/JSONL) changes the
//! output for at least one fixture, and the snapshot diff makes it visible
//! immediately.
//!
//! Adding a new case is a directory-only operation — drop a fixture file in
//! `tests/fixtures/golden/`, run `cargo insta review`, accept the snapshot.
//! No Rust changes required.
//!
//! M1 ships with 21 fixtures covering the four parsers — k8s manifests, helm,
//! github actions, package/eslint/renovate JSON, Cargo/pyproject/basic TOML,
//! plus a JSONL log sample. Coverage will expand naturally in M3+ as more
//! parsers join the registry.

use std::path::PathBuf;

use assert_cmd::Command;

fn dq() -> Command {
    let mut cmd = Command::cargo_bin("dq").expect("dq binary built");
    cmd.env_clear();
    if let Ok(p) = std::env::var("PATH") {
        cmd.env("PATH", p);
    }
    cmd.env("HOME", "/tmp");
    cmd
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
}

/// Subdirectory holding M2 §12.5 round-trip fixtures.
fn roundtrip_dir() -> PathBuf {
    fixtures_dir().join("roundtrip")
}

#[test]
fn golden_paths_for_each_fixture_in_dir() {
    let dir = fixtures_dir();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read_dir({}): {e}", dir.display()))
        .map(|res| res.unwrap_or_else(|e| panic!("read_dir entry in {}: {e}", dir.display())))
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    // Stable iteration order matters: snapshot names are derived from the file
    // name, but the enumerate order also affects which assertion happens
    // first. Sorting also keeps test output diff-friendly.
    entries.sort();

    assert!(
        entries.len() >= 12,
        "expected ≥ 12 golden fixtures, found {}",
        entries.len()
    );

    for fixture in &entries {
        let stem = fixture
            .file_name()
            .and_then(|s| s.to_str())
            .expect("utf-8 fixture name");
        let path = fixture.to_str().expect("utf-8 fixture path");
        let out = dq()
            .args(["paths", path, "--no-color"])
            .output()
            .unwrap_or_else(|e| panic!("spawn dq for {stem}: {e}"));

        let exit = out.status.code();
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();

        let combined = format!(
            "exit_code: {}\n--- stdout ---\n{}--- stderr ---\n{}",
            exit.unwrap_or(-1),
            stdout,
            stderr,
        );
        // Each fixture gets its own snapshot, named after the file stem so
        // adding/removing fixtures is a dir-only operation.
        insta::assert_snapshot!(stem, combined);
    }
}

// ---------------------------------------------------------------------------
// M2 §12.5 — round-trip preservation runner.
//
// For every fixture under `fixtures/golden/roundtrip/`:
//   1. Read the bytes.
//   2. Parse via the format-specific entry point (write-aware where
//      available: `parse_yaml_with_spans` / `parse_json_with_spans` /
//      `Toml::parse`).
//   3. Assert `Document::original_bytes()` is byte-identical to step 1.
//
// This is a property test on real-world-shaped fixtures. A regression in any
// parser's span-tracking shows up immediately as a failed fixture, and the
// failure message names the file so the bug is localized in one read.
// ---------------------------------------------------------------------------

#[test]
fn roundtrip_parse_then_original_bytes_is_byte_identical() {
    let dir = roundtrip_dir();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read_dir({}): {e}", dir.display()))
        .map(|res| res.unwrap_or_else(|e| panic!("read_dir entry in {}: {e}", dir.display())))
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    entries.sort();

    // Spec calls for ≥ 25 round-trip fixtures (8 YAML + 6 JSON + 6 TOML +
    // 5 mixed). Asserting on the directory size catches "someone deleted
    // a fixture" regressions.
    assert!(
        entries.len() >= 25,
        "expected ≥ 25 round-trip fixtures, found {}",
        entries.len(),
    );

    // Track how many fixtures actually exercised the byte-equality path.
    // JSONL is read-only and does not preserve `original_bytes()`, so it's
    // skipped — but we still want it physically present in the directory
    // so the existing M1 `dq paths` golden runner (which scans the parent
    // dir, not this subdir) doesn't break if we ever flatten the layout.
    let mut roundtripped = 0usize;
    let mut skipped_jsonl = 0usize;

    for fixture in &entries {
        let stem = fixture
            .file_name()
            .and_then(|s| s.to_str())
            .expect("utf-8 fixture name");
        let original = std::fs::read(fixture).unwrap_or_else(|e| {
            panic!("read fixture {}: {e}", fixture.display());
        });
        let path = camino::Utf8Path::from_path(fixture).expect("utf-8 fixture path");

        // Pick the parser by extension. `dq_core::detect` is the same
        // dispatcher `dq` uses in production, so a registry regression
        // surfaces here too.
        let format = dq_core::detect(path).unwrap_or_else(|| {
            panic!("no format for fixture `{stem}` (extension not registered in dq_core)");
        });

        // JSONL parsers do not retain original bytes (per `Jsonl::parse`):
        // they're read-only, and their write path renders fresh JSON. Skip
        // the byte-equality assertion for JSONL but track it so we can
        // verify *something* is being skipped (vs. silently degrading to
        // zero coverage).
        if format.name() == "jsonl" {
            // Sanity-check that JSONL still parses without error so the
            // fixture isn't just broken.
            format
                .parse(&original)
                .unwrap_or_else(|e| panic!("JSONL parse failed for `{stem}`: {e}"));
            skipped_jsonl += 1;
            continue;
        }

        // For YAML / JSON, prefer the write-aware entry points so we
        // exercise the span-tracking code path. TOML's generic
        // `Format::parse` is also write-aware (per `Toml::parse` doc).
        let doc_result: dq_core::Result<dq_core::Document> = match format.name() {
            "yaml" => dq_core::parse_yaml_with_spans(&original),
            "json" => dq_core::parse_json_with_spans(&original),
            _ => format.parse(&original),
        };

        let doc = doc_result.unwrap_or_else(|e| {
            panic!(
                "parse failed for `{stem}`: {e}\noriginal:\n{}",
                String::from_utf8_lossy(&original),
            )
        });
        let actual = doc.original_bytes();
        assert_eq!(
            actual,
            original.as_slice(),
            "round-trip mismatch for `{stem}`:\n--- expected (input) ---\n{}\n--- actual (Document::original_bytes) ---\n{}\n",
            String::from_utf8_lossy(&original),
            String::from_utf8_lossy(actual),
        );
        roundtripped += 1;
    }

    assert!(
        roundtripped >= 24,
        "expected ≥ 24 fixtures to actually exercise the round-trip property \
         (rest may be JSONL skips), got roundtripped={roundtripped} skipped_jsonl={skipped_jsonl}",
    );
}
