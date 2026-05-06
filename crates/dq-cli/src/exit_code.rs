//! Exit code constants and the error-to-exit-code mapping.
//!
//! `dq-cli` follows a small fixed set of exit codes so shell users and CI
//! pipelines can branch on the failure reason without parsing stderr.
//! The mapping below downcasts `anyhow::Error` to the [`dq_core::Error`]
//! domain enum and selects the matching constant; anything not produced by
//! `dq-core` falls back to [`GENERIC`].
//!
//! ## Codes
//!
//! - 0 — `SUCCESS` — command completed.
//! - 1 — `GENERIC` — unclassified failure; also used for `exists` returning "false".
//! - 2 — `NOT_FOUND` — JSON Pointer did not address an existing node.
//! - 3 — `PARSE_ERROR` — source could not be parsed (includes Go-template
//!   guard rejections from [`dq_core::Error::TemplatedFile`]).
//! - 4 — `VALIDATE_FAIL` — `validate` reports the document is invalid.
//! - 5 — `IO_ERROR` — read-side IO failure (couldn't read source file).
//! - 6 — `INVALID_INPUT` — bad CLI usage / unsupported flag combination.
//! - 7 — `WRITE_FAILED` — read+resolve+render succeeded but writing the
//!   result to the filesystem failed (atomic-rename collision, missing
//!   textual-edit renderer, document loaded read-only, ...).
//!
//! ## Plugin error mapping (Phase 5 of `add-ir-foundation`)
//!
//! [`dq_plugin::PluginError`] surfaces a stable category string via
//! [`dq_plugin::PluginError::kind_name`] that drives the mapping below:
//!
//! | `kind_name()`        | exit code               |
//! |----------------------|-------------------------|
//! | `feature_disabled`   | `INVALID_INPUT` (6)     |
//! | `disallowed_import`  | `INVALID_INPUT` (6)     |
//! | `schema_version`    | `PARSE_ERROR` (3)       |
//! | `malformed_fix`     | `PARSE_ERROR` (3)       |
//! | `exhausted`         | `VALIDATE_FAIL` (4)     |
//! | `memory`            | `VALIDATE_FAIL` (4)     |
//! | `invoke`            | `VALIDATE_FAIL` (4)     |
//! | `load`              | `VALIDATE_FAIL` (4)     |
//!
//! Note: the `data-query-plugin-abi` spec calls the exit-code-4 family
//! `RUNTIME_ERROR` for plugin errors. The CLI's existing `4` constant is
//! [`VALIDATE_FAIL`] (`document failed a quality gate`), which is the closest
//! existing semantic match — a plugin trap or load failure is "the runtime /
//! document failed a check". We map to [`VALIDATE_FAIL`] rather than
//! introducing a parallel `RUNTIME_ERROR` constant to honour the spec's
//! "exit code 4" intent without breaking the existing exit-code surface.

use dq_core::Error;
use dq_exec::ExecError;
use dq_plugin::PluginError;

use crate::error::{
    BulkPartialFailure, CheckPending, InvalidInput, LintFail, LintWarnStrict, ValidateFail,
};

/// Successful exit.
pub const SUCCESS: i32 = 0;
/// Generic / unclassified error — also used for `exists` returning "false".
pub const GENERIC: i32 = 1;
/// JSON Pointer did not address an existing node.
pub const NOT_FOUND: i32 = 2;
/// Source could not be parsed.
pub const PARSE_ERROR: i32 = 3;
/// `validate` reports the document is invalid.
pub const VALIDATE_FAIL: i32 = 4;
/// I/O failure (file not found, permission denied, broken writer, ...).
pub const IO_ERROR: i32 = 5;
/// Caller-side input error: bad CLI flags, unsupported `-F` value, etc.
pub const INVALID_INPUT: i32 = 6;
/// File parse / resolution succeeded but writing failed (filesystem,
/// permission, atomic rename collision). Distinct from [`IO_ERROR`] (read-side
/// IO) so callers can distinguish "could not read source" from "could not
/// persist result". Also covers the case where the document is loaded
/// read-only (no textual-edit renderer registered for its format).
pub const WRITE_FAILED: i32 = 7;

/// Map an `anyhow::Error` to the matching exit-code constant.
///
/// The function checks for [`ValidateFail`] first (validate-time parse
/// failures map to [`VALIDATE_FAIL`] instead of [`PARSE_ERROR`]), then for
/// [`InvalidInput`] (caller-side input errors map to [`INVALID_INPUT`] instead
/// of the [`GENERIC`] catch-all), and finally downcasts to [`dq_core::Error`]
/// for everything else. Errors that are not produced by `dq-core` (or are
/// wrapped in a way that hides them from `downcast_ref`) collapse to
/// [`GENERIC`].
#[must_use]
pub fn exit_code_for_error(err: &anyhow::Error) -> i32 {
    if err.downcast_ref::<ValidateFail>().is_some() {
        return VALIDATE_FAIL;
    }
    // M8 §6.11: lint engine markers. `LintFail` shares VALIDATE_FAIL with
    // `validate` ("document failed a quality gate" family). `LintWarnStrict`
    // shares GENERIC with `exists` returning false ("warnings under
    // --strict" — semantically a yes/no answered "no").
    if err.downcast_ref::<LintFail>().is_some() {
        return VALIDATE_FAIL;
    }
    if err.downcast_ref::<LintWarnStrict>().is_some() {
        return GENERIC;
    }
    if err.downcast_ref::<InvalidInput>().is_some() {
        return INVALID_INPUT;
    }
    // Bulk-mode markers from the M3 §3 driver. `CheckPending` is the
    // `--check` "changes pending" signal (semantically a yes/no question
    // answered with "no, things would change") so it shares the GENERIC=1
    // family with `exists` returning false. `BulkPartialFailure` is the
    // `--continue-on-error` aggregator — at least one file in the run
    // failed to write, so we surface the standard write-failure code.
    if err.downcast_ref::<CheckPending>().is_some() {
        return GENERIC;
    }
    if err.downcast_ref::<BulkPartialFailure>().is_some() {
        return WRITE_FAILED;
    }
    // Phase 5 (`add-ir-foundation`): dq-plugin runtime errors. The mapping
    // is documented in the module-level "Plugin error mapping" table above.
    // Note that `exhausted` / `memory` / `invoke` / `load` map to
    // `VALIDATE_FAIL` (4) rather than a dedicated `RUNTIME_ERROR` constant —
    // see the module docs for the rationale.
    if let Some(plugin_err) = err.downcast_ref::<PluginError>() {
        return match plugin_err.kind_name() {
            "feature_disabled" | "disallowed_import" => INVALID_INPUT,
            "schema_version" | "malformed_fix" => PARSE_ERROR,
            "exhausted" | "memory" | "invoke" | "load" => VALIDATE_FAIL,
            _ => GENERIC,
        };
    }
    // M8 §6.11: dq-exec rule-runtime errors. Map them to the same families
    // the dq-core errors use so callers can branch uniformly.
    if let Some(exec) = err.downcast_ref::<ExecError>() {
        return match exec.kind_name() {
            // Schema parse failures, jq compile failures, and glob compile
            // failures all share the PARSE_ERROR family — the user's input
            // (rule YAML / jq / glob) was syntactically wrong.
            "parse" | "rule_compile" | "glob_compile" => PARSE_ERROR,
            // Unknown `@std/<ns>` / unresolvable rule path — caller-side
            // input error, same family as `unsupported_format`.
            "unknown_rule" => INVALID_INPUT,
            "io" => IO_ERROR,
            // Fixture authoring errors (missing tests:, malformed expected
            // entries) — caller-side input.
            "test_fixture" => INVALID_INPUT,
            // M10: a `fix.jq` runtime / wrong-arity error means the rule
            // author shipped a buggy autofix — the user's source data is
            // fine. PARSE_ERROR is the closest match (jq evaluator-side
            // failure).
            "fix_apply" => PARSE_ERROR,
            _ => GENERIC,
        };
    }
    let Some(domain) = err.downcast_ref::<Error>() else {
        return GENERIC;
    };
    match domain.kind_name() {
        "io" => IO_ERROR,
        "write_io" | "write_unavailable" => WRITE_FAILED,
        "parse" | "templated_file" => PARSE_ERROR,
        "path" => NOT_FOUND,
        "unsupported_format" => INVALID_INPUT,
        "format" => GENERIC,
        // RFC 6902 `test` op failure: per the M3 cli-shell spec, this maps
        // to GENERIC (exit 1) — semantically "the assertion did not hold",
        // not a parse / IO / path failure.
        "patch_test_failed" => GENERIC,
        // `kind_name` is exhaustive today, but the match keeps the door open
        // for additional variants without breaking the binary.
        _ => GENERIC,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dq_core::PathErrorKind;
    use std::ops::Range;

    fn io_err() -> Error {
        Error::Io {
            path: camino::Utf8PathBuf::from("/nope"),
            source: std::io::Error::other("nope"),
        }
    }

    fn write_io_err() -> Error {
        Error::WriteIo {
            path: camino::Utf8PathBuf::from("/nope"),
            source: std::io::Error::other("nope"),
        }
    }

    fn write_unavailable_err() -> Error {
        Error::WriteUnavailable {
            reason: "format does not support textual edit".to_owned(),
        }
    }

    fn templated_file_err() -> Error {
        Error::TemplatedFile {
            line: 12,
            snippet: "tag: {{ .Values.tag }}".to_owned(),
            hint: "use --allow-templates ...".to_owned(),
        }
    }

    fn parse_err() -> Error {
        Error::Parse {
            file: None,
            line: 1,
            col: 1,
            span: Range { start: 0, end: 0 },
            snippet: String::new(),
            message: "bad".to_owned(),
        }
    }

    fn path_err() -> Error {
        Error::Path {
            pointer: "/x".to_owned(),
            matched_prefix: String::new(),
            kind: PathErrorKind::MissingKey,
            did_you_mean: Vec::new(),
        }
    }

    fn unsupported_err() -> Error {
        Error::UnsupportedFormat {
            name: "xml".to_owned(),
        }
    }

    fn format_err() -> Error {
        Error::Format {
            format: "jsonl",
            message: "boom".to_owned(),
        }
    }

    #[test]
    fn maps_io_to_io_error() {
        assert_eq!(
            exit_code_for_error(&anyhow::Error::from(io_err())),
            IO_ERROR
        );
    }

    #[test]
    fn maps_write_io_to_write_failed() {
        assert_eq!(
            exit_code_for_error(&anyhow::Error::from(write_io_err())),
            WRITE_FAILED
        );
    }

    #[test]
    fn maps_write_unavailable_to_write_failed() {
        assert_eq!(
            exit_code_for_error(&anyhow::Error::from(write_unavailable_err())),
            WRITE_FAILED
        );
    }

    #[test]
    fn maps_templated_file_to_parse_error() {
        assert_eq!(
            exit_code_for_error(&anyhow::Error::from(templated_file_err())),
            PARSE_ERROR
        );
    }

    #[test]
    fn maps_parse_to_parse_error() {
        assert_eq!(
            exit_code_for_error(&anyhow::Error::from(parse_err())),
            PARSE_ERROR
        );
    }

    #[test]
    fn maps_path_to_not_found() {
        assert_eq!(
            exit_code_for_error(&anyhow::Error::from(path_err())),
            NOT_FOUND
        );
    }

    #[test]
    fn maps_unsupported_format_to_invalid_input() {
        assert_eq!(
            exit_code_for_error(&anyhow::Error::from(unsupported_err())),
            INVALID_INPUT
        );
    }

    #[test]
    fn maps_format_to_generic() {
        assert_eq!(
            exit_code_for_error(&anyhow::Error::from(format_err())),
            GENERIC
        );
    }

    #[test]
    fn maps_unknown_anyhow_to_generic() {
        let err = anyhow::anyhow!("disk full");
        assert_eq!(exit_code_for_error(&err), GENERIC);
    }

    #[test]
    fn maps_invalid_input_marker_to_invalid_input() {
        let err = anyhow::Error::new(InvalidInput::new("missing -F for stdin"));
        assert_eq!(exit_code_for_error(&err), INVALID_INPUT);
    }

    #[test]
    fn maps_validate_fail_takes_precedence_over_invalid_input() {
        // A ValidateFail (from `dq validate`) must beat any other downcast —
        // it always means the *document* failed to validate.
        let err = anyhow::Error::new(ValidateFail {
            source: parse_err(),
        });
        assert_eq!(exit_code_for_error(&err), VALIDATE_FAIL);
    }

    #[test]
    fn write_failed_constant_is_seven() {
        // Stable contract — the integer value is part of the public API
        // and shell scripts may branch on it. Pin it down so accidental
        // renumbering breaks the build instead of silently diverging.
        assert_eq!(WRITE_FAILED, 7);
    }

    #[test]
    fn maps_check_pending_marker_to_generic() {
        // `--check` reports "at least one file would be modified" through
        // the `CheckPending` marker — semantically a yes/no question
        // answered with "no, things would change", which matches the same
        // exit-code family `exists` uses for "false".
        let err = anyhow::Error::new(crate::error::CheckPending { count: 3 });
        assert_eq!(exit_code_for_error(&err), GENERIC);
    }

    #[test]
    fn maps_bulk_partial_failure_marker_to_write_failed() {
        // `--continue-on-error` aggregator: per the spec, any per-file
        // failure cause collapses to WRITE_FAILED so CI scripts can branch
        // on a single code.
        let err = anyhow::Error::new(crate::error::BulkPartialFailure { failed_count: 2 });
        assert_eq!(exit_code_for_error(&err), WRITE_FAILED);
    }

    #[test]
    fn maps_patch_test_failed_to_generic() {
        // RFC 6902 `test` op failure → exit 1, per the M3 cli-shell spec.
        // Distinct from PARSE_ERROR (file is fine) and NOT_FOUND (the
        // pointer resolved — its value just didn't match the expectation).
        let err = anyhow::Error::new(Error::PatchTestFailed {
            pointer: "/a".to_owned(),
            expected: Box::new(dq_core::Value::Int(1)),
            actual: Box::new(dq_core::Value::Int(2)),
        });
        assert_eq!(exit_code_for_error(&err), GENERIC);
    }

    // M8 §6.11 — lint engine markers and dq-exec error mapping.

    #[test]
    fn maps_lint_fail_marker_to_validate_fail() {
        let err = anyhow::Error::new(LintFail { count: 3 });
        assert_eq!(exit_code_for_error(&err), VALIDATE_FAIL);
    }

    #[test]
    fn maps_lint_warn_strict_marker_to_generic() {
        let err = anyhow::Error::new(LintWarnStrict { count: 2 });
        assert_eq!(exit_code_for_error(&err), GENERIC);
    }

    #[test]
    fn lint_fail_takes_precedence_over_invalid_input() {
        // If the lint handler has already classified the failure as
        // "lint reported error-severity diagnostics", that wins over a
        // generic InvalidInput marker that might otherwise be present.
        let err = anyhow::Error::new(LintFail { count: 1 });
        assert_eq!(exit_code_for_error(&err), VALIDATE_FAIL);
    }

    #[test]
    fn maps_exec_unknown_rule_to_invalid_input() {
        let err = anyhow::Error::new(ExecError::UnknownRule {
            id: "@std/nope".to_owned(),
            did_you_mean: vec![],
        });
        assert_eq!(exit_code_for_error(&err), INVALID_INPUT);
    }

    #[test]
    fn maps_exec_io_to_io_error() {
        let err = anyhow::Error::new(ExecError::Io {
            path: camino::Utf8PathBuf::from("/no/such"),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "missing"),
        });
        assert_eq!(exit_code_for_error(&err), IO_ERROR);
    }

    #[test]
    fn maps_exec_test_fixture_to_invalid_input() {
        let err = anyhow::Error::new(ExecError::TestFixture {
            path: camino::Utf8PathBuf::from("rules/x.test.yml"),
            message: "missing tests array".to_owned(),
        });
        assert_eq!(exit_code_for_error(&err), INVALID_INPUT);
    }

    #[test]
    fn maps_exec_fix_apply_to_parse_error() {
        // M10 §6: `fix.jq` runtime / arity errors share the PARSE_ERROR
        // family because they're a rule-author bug at the jq layer —
        // distinct from IO failures or document-level lint findings.
        let err = anyhow::Error::new(ExecError::FixApply {
            rule_id: "test.bad-fix".to_owned(),
            message: "fix.jq produced 0 outputs".to_owned(),
        });
        assert_eq!(exit_code_for_error(&err), PARSE_ERROR);
    }

    // Phase 5 of `add-ir-foundation` — `dq_plugin::PluginError` mapping.
    // Each `kind_name()` string is part of the public CLI contract; the
    // assertions below pin down which exit-code constant each variant
    // resolves to.

    fn malformed_fix_serde_error() -> serde_json::Error {
        // Force a typed serde_json::Error so we can construct
        // `PluginError::MalformedFix` without depending on private parser
        // internals.
        serde_json::from_str::<serde_json::Value>("{not json").expect_err("parse must fail")
    }

    #[test]
    fn maps_plugin_feature_disabled_to_invalid_input() {
        let err = anyhow::Error::new(PluginError::FeatureDisabled {
            hint: "rebuild with --features plugins".to_owned(),
        });
        assert_eq!(exit_code_for_error(&err), INVALID_INPUT);
    }

    #[test]
    fn maps_plugin_disallowed_import_to_invalid_input() {
        let err = anyhow::Error::new(PluginError::DisallowedImport {
            interface: "wasi_snapshot_preview1".to_owned(),
        });
        assert_eq!(exit_code_for_error(&err), INVALID_INPUT);
    }

    #[test]
    fn maps_plugin_schema_version_to_parse_error() {
        let err = anyhow::Error::new(PluginError::SchemaVersion {
            plugin_version: "2.0.0".to_owned(),
            host_version: "0.1.0".to_owned(),
        });
        assert_eq!(exit_code_for_error(&err), PARSE_ERROR);
    }

    #[test]
    fn maps_plugin_malformed_fix_to_parse_error() {
        let err = anyhow::Error::new(PluginError::MalformedFix {
            rule_id: "x.bad-fix".to_owned(),
            source: malformed_fix_serde_error(),
        });
        assert_eq!(exit_code_for_error(&err), PARSE_ERROR);
    }

    #[test]
    fn maps_plugin_exhausted_to_validate_fail() {
        let err = anyhow::Error::new(PluginError::Exhausted {
            rule_id: "x.infinite-loop".to_owned(),
        });
        assert_eq!(exit_code_for_error(&err), VALIDATE_FAIL);
    }

    #[test]
    fn maps_plugin_memory_to_validate_fail() {
        let err = anyhow::Error::new(PluginError::Memory {
            rule_id: "x.allocates-too-much".to_owned(),
        });
        assert_eq!(exit_code_for_error(&err), VALIDATE_FAIL);
    }

    #[test]
    fn maps_plugin_invoke_to_validate_fail() {
        let err = anyhow::Error::new(PluginError::Invoke {
            rule_id: "x.traps".to_owned(),
            message: "unreachable executed".to_owned(),
        });
        assert_eq!(exit_code_for_error(&err), VALIDATE_FAIL);
    }

    #[test]
    fn maps_plugin_load_to_validate_fail() {
        let err = anyhow::Error::new(PluginError::Load {
            path: camino::Utf8PathBuf::from("/no/such/plugin.wasm"),
            message: "invalid magic bytes".to_owned(),
        });
        assert_eq!(exit_code_for_error(&err), VALIDATE_FAIL);
    }
}
