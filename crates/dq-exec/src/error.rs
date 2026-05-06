//! Domain error type for `dq-exec`.
//!
//! Variants carry enough structured context that downstream renderers (the
//! CLI's reporters and exit-code mapper) can produce diagnostics with a
//! stable category string via [`ExecError::kind_name`].

use camino::Utf8PathBuf;
use thiserror::Error;

/// Result alias for the `dq-exec` crate. Mirrors the `dq-core` /
/// `dq-transform` convention so `?` works ergonomically through the
/// rule-runtime pipeline.
pub type Result<T> = std::result::Result<T, ExecError>;

/// Errors surfaced by the rule runtime — schema parse failures, jq
/// compilation, unknown `@std/` ids, I/O against rule sources, and test
/// fixture problems.
///
/// The variant set is intentionally narrow: each carries the minimum
/// context required for the CLI's reporter and exit-code mapper to render
/// a useful diagnostic. New variants land alongside the spec scenario
/// that requires them.
#[derive(Debug, Error)]
pub enum ExecError {
    /// A rule YAML document failed to deserialize against the [`crate::rule::Rule`]
    /// schema (typo in a field name, missing required field, type mismatch).
    /// `hint` is a short human-readable summary; `source` is the underlying
    /// `serde_yml` error for callers that want the parser-level details.
    #[error("rule parse error: {hint}")]
    Parse {
        /// Underlying `serde_yml` deserialization error.
        #[source]
        source: serde_yml::Error,
        /// Short human-readable summary of what went wrong.
        hint: String,
    },

    /// A rule's `match.filter` or `check.jq` could not be compiled by the
    /// jq engine. The `rule_id` lets the reporter point the user at the
    /// offending rule without re-parsing the source YAML.
    #[error("rule {rule_id} failed to compile: {source}")]
    RuleCompile {
        /// The `id:` field of the offending rule.
        rule_id: String,
        /// Underlying `dq_transform::JqError` (Compile / Runtime / etc.).
        #[source]
        source: dq_transform::JqError,
    },

    /// A `@std/<name>` identifier (or path / id) didn't resolve to any
    /// known ruleset. `did_you_mean` is the (possibly empty) list of
    /// Levenshtein-2 suggestions that the loader computed.
    #[error("unknown rule {id}")]
    UnknownRule {
        /// The unresolved identifier as provided by the caller.
        id: String,
        /// Suggestions for typo-tolerant resolution.
        did_you_mean: Vec<String>,
    },

    /// I/O failure reading a rule file or directory entry. Distinct from
    /// `dq_core::Error::Io` because the failing artifact is a *rule*
    /// source — the exit-code mapper can route this differently.
    #[error("io error reading {path}: {source}")]
    Io {
        /// Path being read when the failure happened.
        path: Utf8PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// A `*.test.yml` fixture had a structural problem (missing `tests:`
    /// array, malformed `expected.violations`, etc.). The `path` points at
    /// the fixture file; the `message` is a fixture-author-facing summary.
    #[error("test fixture {path}: {message}")]
    TestFixture {
        /// Fixture file path.
        path: Utf8PathBuf,
        /// Human-readable description of the structural problem.
        message: String,
    },

    /// A rule's `match.glob` pattern failed to compile in the `globset`
    /// crate. The `rule_id` lets the reporter point the user at the
    /// offending rule without re-parsing the source YAML; `source` carries
    /// the underlying parse error from `globset`.
    #[error("rule {rule_id} has invalid glob pattern: {source}")]
    GlobCompile {
        /// The `id:` field of the offending rule.
        rule_id: String,
        /// Underlying `globset::Error`.
        #[source]
        source: globset::Error,
    },

    /// M10: a rule's `fix.jq` failed at runtime — parse / evaluate /
    /// conversion error, or wrong-arity output (zero or more than one
    /// stream value).
    ///
    /// Compile failures of `fix.jq` are reported as
    /// [`ExecError::RuleCompile`] instead, mirroring the `check.jq`
    /// path — runtime errors and arity violations land here so the CLI's
    /// exit-code mapper can route them distinctly from compile errors.
    ///
    /// Non-idempotent fixes are NOT a hard error — the runtime
    /// [`crate::Fixer`] logs and skips them at `tracing::warn!`.
    #[error("rule {rule_id} fix failed: {message}")]
    FixApply {
        /// The `id:` field of the offending rule.
        rule_id: String,
        /// Human-readable description of the failure (jq runtime error,
        /// wrong-arity output, etc.).
        message: String,
    },
}

impl ExecError {
    /// Stable, lowercase string identifying the error category.
    ///
    /// Used by the CLI's exit-code mapper and by JSON output formats that
    /// want a stable key independent of the diagnostic message. Returns
    /// one of `"parse"`, `"rule_compile"`, `"unknown_rule"`, `"io"`,
    /// `"test_fixture"`, `"glob_compile"`, `"fix_apply"`.
    #[must_use]
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Parse { .. } => "parse",
            Self::RuleCompile { .. } => "rule_compile",
            Self::UnknownRule { .. } => "unknown_rule",
            Self::Io { .. } => "io",
            Self::TestFixture { .. } => "test_fixture",
            Self::GlobCompile { .. } => "glob_compile",
            Self::FixApply { .. } => "fix_apply",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// Force a `serde_yml::Error` so the Parse variant can be constructed
    /// without coupling tests to private parser internals.
    fn sample_yaml_error() -> serde_yml::Error {
        // Parsing a non-mapping into a mapping-shaped target produces a
        // typed error — the exact wording is irrelevant; we just need the
        // value.
        serde_yml::from_str::<std::collections::BTreeMap<String, String>>("- not a map\n")
            .expect_err("parse should fail")
    }

    /// Force a `dq_transform::JqError::Compile` so the RuleCompile variant
    /// can be constructed without coupling to jq engine internals.
    fn sample_jq_error() -> dq_transform::JqError {
        dq_transform::JqEngine::compile("...invalid syntax(((")
            .expect_err("compile should fail with a syntax error")
    }

    #[test]
    fn kind_name_covers_parse_variant() {
        let err = ExecError::Parse {
            source: sample_yaml_error(),
            hint: "missing 'id' field".to_owned(),
        };
        assert_eq!(err.kind_name(), "parse");
    }

    #[test]
    fn kind_name_covers_rule_compile_variant() {
        let err = ExecError::RuleCompile {
            rule_id: "k8s.no-latest-tag".to_owned(),
            source: sample_jq_error(),
        };
        assert_eq!(err.kind_name(), "rule_compile");
    }

    #[test]
    fn kind_name_covers_unknown_rule_variant() {
        let err = ExecError::UnknownRule {
            id: "@std/k8z".to_owned(),
            did_you_mean: vec!["@std/k8s".to_owned()],
        };
        assert_eq!(err.kind_name(), "unknown_rule");
    }

    #[test]
    fn kind_name_covers_io_variant() {
        let err = ExecError::Io {
            path: Utf8PathBuf::from("/no/such/file.yml"),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "missing"),
        };
        assert_eq!(err.kind_name(), "io");
    }

    #[test]
    fn kind_name_covers_test_fixture_variant() {
        let err = ExecError::TestFixture {
            path: Utf8PathBuf::from("rules/k8s/no-latest-tag.test.yml"),
            message: "missing 'tests' array".to_owned(),
        };
        assert_eq!(err.kind_name(), "test_fixture");
    }

    #[test]
    fn kind_name_covers_glob_compile_variant() {
        // `globset::Glob::new` rejects unbalanced `[` brackets — use that
        // to manufacture a real `globset::Error` without coupling to the
        // crate's private internals.
        let glob_err = globset::Glob::new("[unbalanced").expect_err("expected glob parse failure");
        let err = ExecError::GlobCompile {
            rule_id: "k8s.no-latest-tag".to_owned(),
            source: glob_err,
        };
        assert_eq!(err.kind_name(), "glob_compile");
        let formatted = format!("{err}");
        assert!(
            formatted.contains("k8s.no-latest-tag"),
            "expected display to mention the rule id, got: {formatted}",
        );
    }

    #[test]
    fn kind_name_covers_fix_apply_variant() {
        let err = ExecError::FixApply {
            rule_id: "k8s.no-latest-tag".to_owned(),
            message: "fix.jq produced 2 outputs (expected 1)".to_owned(),
        };
        assert_eq!(err.kind_name(), "fix_apply");
        let formatted = format!("{err}");
        assert!(
            formatted.contains("k8s.no-latest-tag"),
            "expected display to mention the rule id, got: {formatted}",
        );
        assert!(
            formatted.contains("fix"),
            "expected display to call out fix failure, got: {formatted}",
        );
    }

    /// Sanity-check the `Display` impl carries the rule id so consumers
    /// that just `format!("{err}")` get a useful message.
    #[test]
    fn display_for_unknown_rule_includes_id() {
        let err = ExecError::UnknownRule {
            id: "@std/k8z".to_owned(),
            did_you_mean: vec!["@std/k8s".to_owned()],
        };
        let formatted = format!("{err}");
        assert!(
            formatted.contains("@std/k8z"),
            "expected display to mention the id, got: {formatted}",
        );
    }
}
