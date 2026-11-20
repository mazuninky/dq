//! `@std/dockerfile` rules run on Dockerfiles only. `dq lint` of an
//! ordinary YAML file used to print a `check.jq raised a runtime error`
//! warning per rule.

use assert_cmd::Command;
use predicates::prelude::*;
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

#[test]
fn dockerfile_rules_stay_quiet_on_ordinary_yaml() {
    for src in ["name: web\nreplicas: 2\n", "- a\n- b\n", "[]\n"] {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("x.yaml"), src).expect("write fixture");
        dq().current_dir(dir.path())
            .args(["--no-color", "lint", "x.yaml"])
            .assert()
            .stdout(predicate::str::contains("dockerfile").not())
            .stderr(predicate::str::contains("dockerfile").not());
    }
}

#[test]
fn an_ordinary_yaml_mapping_lints_clean_without_a_warning() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("x.yaml"), "name: web\nreplicas: 2\n").expect("write fixture");
    dq().current_dir(dir.path())
        .args(["--no-color", "lint", "x.yaml"])
        .assert()
        .success()
        .stdout("diagnostics: []\n")
        .stderr("");
}

#[test]
fn each_dockerfile_rule_fires_on_a_dockerfile_and_not_on_a_clean_one() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(
        dir.path().join("Dockerfile"),
        "FROM ubuntu:latest\nADD . /app\nRUN [\"sh\", \"-c\", \"curl -sSL https://x.sh | bash\"]\n",
    )
    .expect("write fixture");
    dq().current_dir(dir.path())
        .args(["--no-color", "lint", "Dockerfile"])
        .assert()
        .code(4)
        .stdout(
            predicate::str::contains("dockerfile.no-latest-base-image")
                .and(predicate::str::contains("dockerfile.no-add-use-copy"))
                .and(predicate::str::contains("dockerfile.no-curl-pipe-bash"))
                .and(predicate::str::contains("dockerfile.has-healthcheck")),
        )
        .stderr(predicate::str::contains("WARN").not());

    // Pinned tag, COPY, no pipe into a shell: only the missing HEALTHCHECK.
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(
        dir.path().join("Dockerfile"),
        "FROM alpine:3.20\nCOPY . /app\nRUN echo hi\n",
    )
    .expect("write fixture");
    dq().current_dir(dir.path())
        .args(["--no-color", "lint", "Dockerfile"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("dockerfile.has-healthcheck")
                .and(predicate::str::contains("dockerfile.no-latest-base-image").not())
                .and(predicate::str::contains("dockerfile.no-add-use-copy").not())
                .and(predicate::str::contains("dockerfile.no-curl-pipe-bash").not()),
        )
        .stderr(predicate::str::contains("WARN").not());
}

#[test]
fn dockerfile_rules_still_run_on_a_dockerfile() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(
        dir.path().join("Dockerfile"),
        "FROM alpine:3.20\nADD ./app /app\n",
    )
    .expect("write fixture");
    dq().current_dir(dir.path())
        .args(["--no-color", "lint", "Dockerfile"])
        .assert()
        .stdout(
            predicate::str::contains("dockerfile.no-add-use-copy")
                .and(predicate::str::contains("dockerfile.has-healthcheck")),
        )
        .stderr(predicate::str::contains("WARN").not());
}

// ---------------------------------------------------------------------------
// The rules on real Dockerfile text, through the CLI: a parser that rejects
// or mis-shapes an instruction shows up as a rule verdict here.
// ---------------------------------------------------------------------------

/// `dq lint -F json Dockerfile` over `src`: the exit code and the
/// `(rule_id, message)` of every diagnostic. Asserts that no rule raised a
/// runtime warning.
fn lint(src: &str) -> (i32, Vec<(String, String)>) {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("Dockerfile"), src).expect("write Dockerfile");
    let out = dq()
        .current_dir(dir.path())
        .args(["--no-color", "lint", "-F", "json", "Dockerfile"])
        .output()
        .expect("run dq lint");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("WARN"),
        "rule warning for {src:?}: {stderr}"
    );
    let report: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("lint -F json for {src:?} is not JSON ({e}); stderr: {stderr}"));
    let diags = report["diagnostics"]
        .as_array()
        .expect("diagnostics array")
        .iter()
        .map(|d| {
            (
                d["rule_id"].as_str().expect("rule_id").to_owned(),
                d["message"].as_str().expect("message").to_owned(),
            )
        })
        .collect();
    (out.status.code().expect("exit code"), diags)
}

/// The messages `rule` produced for `src`.
fn messages_of(src: &str, rule: &str) -> Vec<String> {
    lint(src)
        .1
        .into_iter()
        .filter(|(id, _)| id == rule)
        .map(|(_, message)| message)
        .collect()
}

const HEALTHCHECK: &str = "dockerfile.has-healthcheck";
const CURL_PIPE: &str = "dockerfile.no-curl-pipe-bash";
const ADD: &str = "dockerfile.no-add-use-copy";
const LATEST: &str = "dockerfile.no-latest-base-image";

#[test]
fn has_healthcheck_passes_with_any_healthcheck_form() {
    for src in [
        "FROM alpine:3.20\nHEALTHCHECK CMD true\n",
        "FROM alpine:3.20\nHEALTHCHECK --interval=30s CMD curl -f http://localhost/ || exit 1\n",
        "FROM alpine:3.20\nHEALTHCHECK NONE\n",
    ] {
        assert_eq!(
            messages_of(src, HEALTHCHECK),
            Vec::<String>::new(),
            "{src:?}"
        );
    }
}

#[test]
fn has_healthcheck_fires_without_one_and_on_an_onbuild_trigger_only() {
    for src in [
        "FROM alpine:3.20\nRUN echo hi\n",
        "FROM alpine:3.20\nONBUILD HEALTHCHECK CMD true\n",
    ] {
        assert_eq!(
            messages_of(src, HEALTHCHECK),
            ["Dockerfile has no HEALTHCHECK directive"],
            "{src:?}"
        );
    }
}

#[test]
fn no_curl_pipe_bash_fires_on_a_pipe_and_across_a_continuation() {
    for src in [
        "FROM alpine:3.20\nHEALTHCHECK NONE\nRUN curl -fsSL https://x | bash\n",
        "FROM alpine:3.20\nHEALTHCHECK NONE\nRUN curl -fsSL https://x \\\n    | bash\nUSER 1\n",
        "FROM alpine:3.20\nHEALTHCHECK NONE\nRUN <<EOT\ncurl -fsSL https://x | sh\nEOT\n",
    ] {
        assert_eq!(
            messages_of(src, CURL_PIPE),
            ["RUN pipes curl/wget into a shell"],
            "{src:?}"
        );
    }
}

#[test]
fn no_curl_pipe_bash_allows_exec_form_text_and_separate_heredoc_lines() {
    for src in [
        "FROM alpine:3.20\nRUN [\"echo\",\"curl x | bash\"]\n",
        "FROM alpine:3.20\nRUN <<EOT\ncurl -o f https://x\necho done | sh\nEOT\n",
    ] {
        assert_eq!(messages_of(src, CURL_PIPE), Vec::<String>::new(), "{src:?}");
    }
}

#[test]
fn no_add_use_copy_allows_a_url_with_or_without_a_checksum() {
    for src in [
        "FROM alpine:3.20\nADD https://example.com/a.tgz /tmp/\n",
        "FROM alpine:3.20\nADD --checksum=sha256:24454f830cdb571e2c4ad15481119c43b3cafd48dd869a9b2945d1036d1dc68d https://example.com/a.tgz /tmp/\n",
    ] {
        assert_eq!(messages_of(src, ADD), Vec::<String>::new(), "{src:?}");
    }
}

#[test]
fn no_add_use_copy_fires_on_a_local_source() {
    assert_eq!(
        messages_of("FROM alpine:3.20\nRUN echo hi\nADD ./app /app\n", ADD),
        ["ADD used for local copy; prefer COPY"]
    );
}

#[test]
fn no_latest_base_image_allows_scratch_a_stage_name_and_a_digest() {
    let src = "FROM golang:1.22 AS Build\n\
               FROM build\n\
               FROM scratch\n\
               FROM alpine@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n\
               HEALTHCHECK NONE\n";
    assert_eq!(lint(src), (0, Vec::new()));
}

#[test]
fn no_latest_base_image_fires_on_an_untagged_image_behind_a_registry_port() {
    let (code, diags) = lint("FROM localhost:5000/img\nFROM alpine\nHEALTHCHECK NONE\n");
    assert_eq!(code, 4, "an error-severity diagnostic exits 4");
    assert_eq!(
        diags,
        [
            (
                LATEST.to_owned(),
                "FROM line uses :latest (or no tag): localhost:5000/img".to_owned()
            ),
            (
                LATEST.to_owned(),
                "FROM line uses :latest (or no tag): alpine".to_owned()
            ),
        ]
    );
}

// ---------------------------------------------------------------------------
// Positions: a Dockerfile finding points at the instruction it is about —
// the line the instruction starts on (not a comment above it, not a
// continuation or heredoc line below it) and the column of its keyword.
// Every finding used to report line 1, column 1.
// ---------------------------------------------------------------------------

/// A directive, a blank line, an indented `ADD`, a `RUN` continued over two
/// lines and a `RUN` with a heredoc body.
const POSITIONED: &str = "# syntax=docker/dockerfile:1\n\
                          FROM ubuntu:latest\n\
                          \n  ADD ./app /app\n\
                          RUN apk add curl && \\\n    curl -sSL https://x | bash\n\
                          RUN <<EOT\ncurl -fsSL https://x | sh\nEOT\n";

/// `dq lint --no-color [-F <format>] Dockerfile` over `src`.
fn lint_output(src: &str, format: Option<&str>) -> std::process::Output {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("Dockerfile"), src).expect("write Dockerfile");
    let mut args = vec!["--no-color", "lint"];
    if let Some(format) = format {
        args.extend(["-F", format]);
    }
    args.push("Dockerfile");
    dq().current_dir(dir.path())
        .args(&args)
        .output()
        .expect("run dq lint")
}

#[test]
fn console_output_reports_each_finding_at_its_instruction() {
    let out = lint_output(POSITIONED, None);
    assert_eq!(out.status.code(), Some(4), "an error-severity finding");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "diagnostics: [\
         {path: Dockerfile, line: 1, col: 1, message: Dockerfile has no HEALTHCHECK directive, severity: info, rule_id: dockerfile.has-healthcheck, references: [https://docs.docker.com/engine/reference/builder/#healthcheck]}, \
         {path: Dockerfile, line: 4, col: 3, message: ADD used for local copy; prefer COPY, severity: warn, rule_id: dockerfile.no-add-use-copy, references: [https://docs.docker.com/develop/develop-images/dockerfile_best-practices/#add-or-copy]}, \
         {path: Dockerfile, line: 5, col: 1, message: RUN pipes curl/wget into a shell, severity: warn, rule_id: dockerfile.no-curl-pipe-bash, references: [https://www.idontplaydarts.com/2016/04/detecting-curl-pipe-bash-server-side/]}, \
         {path: Dockerfile, line: 7, col: 1, message: RUN pipes curl/wget into a shell, severity: warn, rule_id: dockerfile.no-curl-pipe-bash, references: [https://www.idontplaydarts.com/2016/04/detecting-curl-pipe-bash-server-side/]}, \
         {path: Dockerfile, line: 2, col: 1, message: FROM line uses :latest (or no tag): ubuntu:latest, severity: error, rule_id: dockerfile.no-latest-base-image, references: [https://docs.docker.com/develop/develop-images/dockerfile_best-practices/]}\
         ]\n"
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "lint reported 1 error-severity diagnostic(s)\n"
    );
}

/// `(rule_id, line, col, message)` of every diagnostic in a `-F json` report.
fn json_positions(stdout: &[u8]) -> Vec<(String, u64, u64, String)> {
    let report: serde_json::Value = serde_json::from_slice(stdout).expect("lint -F json is JSON");
    report["diagnostics"]
        .as_array()
        .expect("diagnostics array")
        .iter()
        .map(|d| {
            (
                d["rule_id"].as_str().expect("rule_id").to_owned(),
                d["line"].as_u64().expect("line"),
                d["col"].as_u64().expect("col"),
                d["message"].as_str().expect("message").to_owned(),
            )
        })
        .collect()
}

fn found(rule: &str, line: u64, col: u64, message: &str) -> (String, u64, u64, String) {
    (rule.to_owned(), line, col, message.to_owned())
}

#[test]
fn json_output_reports_each_finding_at_its_instruction() {
    let out = lint_output(POSITIONED, Some("json"));
    assert_eq!(out.status.code(), Some(4));
    assert_eq!(
        json_positions(&out.stdout),
        [
            found(HEALTHCHECK, 1, 1, "Dockerfile has no HEALTHCHECK directive"),
            found(ADD, 4, 3, "ADD used for local copy; prefer COPY"),
            found(CURL_PIPE, 5, 1, "RUN pipes curl/wget into a shell"),
            found(CURL_PIPE, 7, 1, "RUN pipes curl/wget into a shell"),
            found(
                LATEST,
                2,
                1,
                "FROM line uses :latest (or no tag): ubuntu:latest"
            ),
        ]
    );
}

#[test]
fn sarif_output_reports_each_finding_at_its_instruction() {
    let out = lint_output(POSITIONED, Some("sarif"));
    assert_eq!(out.status.code(), Some(4));
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("SARIF is JSON");
    let regions: Vec<(String, u64, u64)> = report["runs"][0]["results"]
        .as_array()
        .expect("results array")
        .iter()
        .map(|r| {
            let region = &r["locations"][0]["physicalLocation"]["region"];
            (
                r["message"]["text"].as_str().expect("text").to_owned(),
                region["startLine"].as_u64().expect("startLine"),
                region["startColumn"].as_u64().expect("startColumn"),
            )
        })
        .collect();
    let region = |text: &str, line, col| (text.to_owned(), line, col);
    assert_eq!(
        regions,
        [
            region("Dockerfile has no HEALTHCHECK directive", 1, 1),
            region("ADD used for local copy; prefer COPY", 4, 3),
            region("RUN pipes curl/wget into a shell", 5, 1),
            region("RUN pipes curl/wget into a shell", 7, 1),
            region("FROM line uses :latest (or no tag): ubuntu:latest", 2, 1),
        ]
    );
}

#[test]
fn positions_hold_under_escape_crlf_and_a_bom() {
    // A BOM, `# escape=` with a backtick continuation, CRLF line ends.
    let src = "\u{feff}# escape=`\r\nFROM alpine\r\nRUN echo a `\r\n  && curl x | sh\r\n\r\nADD a.txt /a\r\nHEALTHCHECK NONE\r\n";
    let out = lint_output(src, Some("json"));
    assert_eq!(out.status.code(), Some(4));
    assert_eq!(
        json_positions(&out.stdout),
        [
            found(ADD, 6, 1, "ADD used for local copy; prefer COPY"),
            found(CURL_PIPE, 3, 1, "RUN pipes curl/wget into a shell"),
            found(LATEST, 2, 1, "FROM line uses :latest (or no tag): alpine"),
        ]
    );
}

#[test]
fn a_schema_finding_inside_an_instruction_points_at_its_arguments() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::create_dir_all(dir.path().join(".dq/rules")).expect("rules dir");
    std::fs::write(
        dir.path().join(".dq/rules/short-env.yml"),
        "id: user.short-env\n\
         description: ENV values stay short\n\
         severity: warn\n\
         match:\n  format: [dockerfile]\n\
         check:\n  schema:\n    type: array\n    items:\n      properties:\n        arguments:\n          additionalProperties:\n            maxLength: 3\n",
    )
    .expect("write rule");
    std::fs::write(
        dir.path().join("Dockerfile"),
        "FROM alpine:3.20\nHEALTHCHECK NONE\n  ENV A=1 LONG=abcdef\n",
    )
    .expect("write Dockerfile");
    let out = dq()
        .current_dir(dir.path())
        .args(["--no-color", "lint", "-F", "json", "Dockerfile"])
        .output()
        .expect("run dq lint");
    assert_eq!(
        json_positions(&out.stdout),
        [found(
            "user.short-env",
            3,
            7,
            "/items/properties/arguments/additionalProperties/maxLength: \"abcdef\" is longer than 3 characters"
        )]
    );
}
