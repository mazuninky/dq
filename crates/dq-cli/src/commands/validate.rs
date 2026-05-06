//! `dq validate FILE` — exit 0 when the file parses, exit 4 when it does not.
//!
//! On success, the handler writes nothing and returns `Ok(())`. On parse
//! failure, the structured `dq_core::Error::Parse` is rendered to *stderr*
//! through the supplied [`Reporter`] (so JSON / Console formats both work),
//! and the original error is propagated so the exit-code mapper produces
//! `VALIDATE_FAIL` (4).
//!
//! This handler is the only one whose output goes to stderr — the lib-level
//! dispatcher passes a stderr writer here specifically for that reason.

use std::io::Write;

use camino::Utf8Path;

use crate::cli::{Cli, ValidateArgs};
use crate::error::{InvalidInput, ValidateFail};
use crate::output::{OutputFormat, Reporter};

/// Run the `validate` command.
///
/// # Errors
///
/// - [`InvalidInput`] (exit 6) when any write-mode flag (`-i`, `--diff`,
///   `--backup`) is set — `validate` is a read subcommand.
/// - `dq_core::Error::Parse` (mapped to exit 4 via [`ValidateFail`]) when the
///   file's syntax does not match the chosen format. Other I/O /
///   unsupported-format errors propagate as usual.
///
/// `err` is the writer for diagnostics; the value renders via the supplied
/// reporter so `-F json` produces a structured JSON object instead of the
/// default console rendering.
pub fn run(
    cli: &Cli,
    args: &ValidateArgs,
    input_format: Option<&str>,
    reporter: &dyn Reporter,
    err: &mut dyn Write,
) -> anyhow::Result<()> {
    // M4 §5: `validate --check` is the same shape as plain `validate` —
    // exit 4 on parse failure, exit 0 otherwise. The standard
    // `ensure_no_write_flags` rejects `--check`; `validate` substitutes
    // `ensure_no_write_flags_except_check` so users can run
    // `dq validate --check FILE` in CI alongside `dq fmt --check` /
    // `dq set --check` without the flag being filtered out.
    cli.ensure_no_write_flags_except_check()?;
    // Implementation note: we duplicate `io_helpers::pick_format` and
    // `read_bytes` here in miniature so we can render the parse error
    // through `reporter` to `err` BEFORE returning the error to main.rs.
    // Going through `load_document` would surface the parse error too late
    // (already wrapped in `anyhow::Error`), at which point the reporter has
    // no structured data to format.
    let format = pick_format(&args.file, input_format)?;
    let bytes = read_bytes(&args.file)?;
    match format.parse(&bytes) {
        Ok(_) => Ok(()),
        Err(e) => {
            // Render the structured error to stderr through the reporter,
            // then propagate the error so exit-code mapping picks 4.
            //
            // M6 §5: structured reporters (JSON / YAML / TOML / JSONL /
            // TOON / SARIF) need the canonical `{ "diagnostics": [...] }`
            // shape so SARIF can consume the same value the existing JSON
            // path already produces. The Console branch keeps the bare
            // diagnostic object — the human-readable formatter is unchanged
            // and the existing test fixtures stay byte-identical.
            let json = if matches!(cli.format, OutputFormat::Console) {
                render_parse_error(&e, &args.file)
            } else {
                render_parse_error_diagnostics(&e, &args.file)
            };
            reporter.report(&json, err).ok(); // best-effort — stderr is allowed to fail.
            // Wrap with the file path if the parser left it unset.
            let parse_err = match e {
                dq_core::Error::Parse {
                    file: None,
                    line,
                    col,
                    span,
                    snippet,
                    message,
                } => dq_core::Error::Parse {
                    file: Some(args.file.clone()),
                    line,
                    col,
                    span,
                    snippet,
                    message,
                },
                other => other,
            };
            // Wrap in ValidateFail so the exit-code mapper produces 4 instead of 3.
            Err(anyhow::Error::new(ValidateFail { source: parse_err }))
        }
    }
}

fn pick_format(
    file: &Utf8Path,
    input_format: Option<&str>,
) -> anyhow::Result<&'static dyn dq_core::Format> {
    if let Some(name) = input_format {
        return dq_core::by_name(name).ok_or_else(|| {
            anyhow::Error::new(dq_core::Error::UnsupportedFormat {
                name: name.to_owned(),
            })
        });
    }
    if file == "-" {
        return Err(anyhow::Error::new(InvalidInput::new(
            "stdin requires -F <format> (no extension to detect from)",
        )));
    }
    dq_core::detect(file).ok_or_else(|| {
        let ext = file
            .extension()
            .map(str::to_owned)
            .unwrap_or_else(|| file.to_string());
        anyhow::Error::new(dq_core::Error::UnsupportedFormat { name: ext })
    })
}

fn read_bytes(file: &Utf8Path) -> anyhow::Result<Vec<u8>> {
    if file == "-" {
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut std::io::stdin(), &mut buf).map_err(|source| {
            anyhow::Error::new(dq_core::Error::Io {
                path: camino::Utf8PathBuf::from(":stdin"),
                source,
            })
        })?;
        return Ok(buf);
    }
    std::fs::read(file.as_std_path()).map_err(|source| {
        anyhow::Error::new(dq_core::Error::Io {
            path: file.to_path_buf(),
            source,
        })
    })
}

/// Render a parse error as the bare diagnostic object the Console reporter
/// has emitted since M4 §5. Kept byte-identical to the M5 baseline so the
/// existing console snapshot tests stay green.
fn render_parse_error(err: &dq_core::Error, file: &Utf8Path) -> serde_json::Value {
    if let dq_core::Error::Parse {
        line,
        col,
        snippet,
        message,
        ..
    } = err
    {
        serde_json::json!({
            "kind": "parse",
            "file": file.as_str(),
            "line": *line,
            "col": *col,
            "message": message,
            "snippet": snippet,
        })
    } else {
        serde_json::json!({
            "kind": err.kind_name(),
            "message": err.to_string(),
        })
    }
}

/// Build the structured-reporter shape that wraps a parse error in the
/// canonical `{ "diagnostics": [...] }` envelope. The diagnostic object
/// adds `path` and `severity` aliases so the SARIF reporter (and the
/// future M8 lint reporters) can consume it without a per-format shim;
/// the legacy `file` / `kind` / `message` fields are preserved so the
/// existing JSON-output tests still observe them.
fn render_parse_error_diagnostics(err: &dq_core::Error, file: &Utf8Path) -> serde_json::Value {
    let diagnostic = if let dq_core::Error::Parse {
        line,
        col,
        snippet,
        message,
        ..
    } = err
    {
        serde_json::json!({
            "kind": "parse",
            "file": file.as_str(),
            "path": file.as_str(),
            "line": *line,
            "col": *col,
            "severity": "error",
            "message": message,
            "snippet": snippet,
        })
    } else {
        serde_json::json!({
            "kind": err.kind_name(),
            "path": file.as_str(),
            "severity": "error",
            "message": err.to_string(),
        })
    };
    serde_json::json!({ "diagnostics": [diagnostic] })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::JsonReporter;
    use clap::Parser;
    use tempfile::NamedTempFile;

    fn cli_no_flags(file: &str) -> Cli {
        Cli::try_parse_from(["dq", "validate", file]).expect("clap parse")
    }

    #[test]
    fn validate_succeeds_silently_for_valid_yaml() {
        let mut tmp = NamedTempFile::with_suffix(".yaml").unwrap();
        tmp.write_all(b"a: 1\n").unwrap();
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = ValidateArgs { file: path };
        let reporter = JsonReporter;
        let mut err: Vec<u8> = Vec::new();
        run(&cli, &args, None, &reporter, &mut err).unwrap();
        assert!(err.is_empty(), "expected no stderr, got {err:?}");
    }

    #[test]
    fn validate_fails_with_parse_error_and_renders_to_err() {
        let mut tmp = NamedTempFile::with_suffix(".json").unwrap();
        // Trailing comma — invalid JSON.
        tmp.write_all(b"{\"a\": 1,}").unwrap();
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = ValidateArgs { file: path };
        let reporter = JsonReporter;
        let mut err: Vec<u8> = Vec::new();
        let e = run(&cli, &args, None, &reporter, &mut err).unwrap_err();
        // The handler wraps the parse error in ValidateFail so the exit-code
        // mapper picks VALIDATE_FAIL (4) instead of PARSE_ERROR (3).
        let validate_fail = e
            .downcast_ref::<ValidateFail>()
            .expect("validate must wrap parse errors in ValidateFail");
        assert_eq!(validate_fail.source.kind_name(), "parse");
        let s = String::from_utf8(err).unwrap();
        assert!(
            s.contains("\"kind\": \"parse\""),
            "expected structured parse error in stderr: {s:?}"
        );
    }

    #[test]
    fn validate_rejects_in_place_flag_before_io() {
        let cli = Cli::try_parse_from(["dq", "-i", "validate", "/nope.yaml"]).unwrap();
        let args = ValidateArgs {
            file: camino::Utf8PathBuf::from("/nope.yaml"),
        };
        let reporter = JsonReporter;
        let mut err: Vec<u8> = Vec::new();
        let e = run(&cli, &args, None, &reporter, &mut err).unwrap_err();
        assert!(e.downcast_ref::<InvalidInput>().is_some());
    }

    #[test]
    fn validate_accepts_check_flag() {
        // M4 §5: `--check` is the only write-mode flag tolerated by
        // `validate`. On a valid file the verb still exits 0 silently —
        // tests the contract that `validate --check` does not change the
        // happy-path semantics.
        let mut tmp = NamedTempFile::with_suffix(".yaml").unwrap();
        tmp.write_all(b"a: 1\n").unwrap();
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let cli = Cli::try_parse_from(["dq", "--check", "validate", path.as_str()]).unwrap();
        let args = ValidateArgs { file: path };
        let reporter = JsonReporter;
        let mut err: Vec<u8> = Vec::new();
        run(&cli, &args, None, &reporter, &mut err)
            .expect("validate --check on a valid file must succeed");
        assert!(err.is_empty(), "happy path must not write to stderr");
    }

    #[test]
    fn validate_check_flag_still_returns_validate_fail_on_invalid_file() {
        // The complementary test: `--check` does not mask parse errors —
        // an invalid file still wraps the parse error in `ValidateFail`
        // (mapped to exit 4 by the exit-code mapper).
        let mut tmp = NamedTempFile::with_suffix(".json").unwrap();
        tmp.write_all(b"{\"a\": 1,}").unwrap(); // trailing comma — invalid JSON.
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let cli = Cli::try_parse_from(["dq", "--check", "validate", path.as_str()]).unwrap();
        let args = ValidateArgs { file: path };
        let reporter = JsonReporter;
        let mut err: Vec<u8> = Vec::new();
        let e = run(&cli, &args, None, &reporter, &mut err).unwrap_err();
        assert!(
            e.downcast_ref::<ValidateFail>().is_some(),
            "validate --check must still wrap parse errors in ValidateFail",
        );
    }
}
