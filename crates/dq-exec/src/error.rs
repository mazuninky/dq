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

    /// M11 Phase 3: rule `check` block contains more than one of the
    /// mutually-exclusive variants (`jq`, `schema`, `schema_file`,
    /// `extract`+`nested`). `present_fields` lists the offending field
    /// names so the CLI can render an actionable error.
    #[error("rule {rule_id} check has mutually exclusive fields: {}", present_fields.join(", "))]
    CheckMutuallyExclusive {
        /// The `id:` field of the offending rule.
        rule_id: String,
        /// Names of the discriminator fields that were all set.
        present_fields: Vec<String>,
    },

    /// M11 Phase 3: rule `check` block declares none of the four
    /// variants. The block must contain at least one of `jq`, `schema`,
    /// `schema_file`, or `extract`+`nested`.
    #[error("rule {rule_id} check is empty (need one of jq, schema, schema_file, extract+nested)")]
    CheckMissing {
        /// The `id:` field of the offending rule.
        rule_id: String,
    },

    /// M11 Phase 3: composite check declares only one of `extract` /
    /// `nested`. Both must be set together (per design D3).
    #[error("rule {rule_id} composite check is missing `{missing_field}`")]
    CompositeIncomplete {
        /// The `id:` field of the offending rule.
        rule_id: String,
        /// Which of `extract` / `nested` is missing.
        missing_field: String,
    },

    /// M11 Phase 4 (placeholder, runtime): composite `extract` jq did
    /// not return an array.
    #[error("rule {rule_id} composite extract did not return an array")]
    CompositeExtractNotArray {
        /// The `id:` field of the offending rule.
        rule_id: String,
    },

    /// M11 Phase 4 (placeholder, runtime): composite extract item is
    /// missing one of the required fields (`value`, `format`, `anchor`).
    #[error("rule {rule_id} composite extract item missing field `{missing_field}`")]
    CompositeExtractMalformed {
        /// The `id:` field of the offending rule.
        rule_id: String,
        /// Name of the missing field.
        missing_field: String,
    },

    /// M11 Phase 4 (placeholder, runtime): composite extract item
    /// declared a `format` name that doesn't resolve to a known
    /// [`dq_core::FormatTag`].
    #[error("rule {rule_id} composite extract item names unknown format `{format}`")]
    CompositeExtractUnknownFormat {
        /// The `id:` field of the offending rule.
        rule_id: String,
        /// The unrecognized format name.
        format: String,
    },

    /// M11 Phase 4 (placeholder, runtime): composite recursion exceeded
    /// the configured depth bound (default `MAX_EXTRACT_DEPTH = 4`).
    #[error("rule {rule_id} composite recursion exceeded depth bound (depth={depth}, max={max})")]
    CompositeDepthExceeded {
        /// The `id:` field of the offending rule.
        rule_id: String,
        /// Recursion depth reached when the bound tripped.
        depth: usize,
        /// Configured maximum depth.
        max: usize,
    },

    /// M11 Phase 3: a rule's inline `check.schema` or sibling
    /// `check.schema_file` failed to compile into a
    /// `jsonschema::Validator`. Common causes: malformed schema,
    /// unresolvable `$ref`, unsupported keyword. `message` carries the
    /// upstream error rendered to a string.
    #[error("rule {rule_id} schema failed to compile: {message}")]
    SchemaCompile {
        /// The `id:` field of the offending rule.
        rule_id: String,
        /// Stringified upstream `jsonschema::ValidationError`. Stored as
        /// a plain `String` because `jsonschema::ValidationError`
        /// borrows from the input by default; flattening to a string
        /// at construction time keeps the variant `'static` and
        /// `Send + Sync`.
        message: String,
    },

    /// M11 Phase 3: `check.schema_file` resolved to an absolute path.
    /// Schema-file paths must stay inside the rule directory tree
    /// (per the spec scenario "Absolute path rejected").
    #[error("rule {rule_id} schema_file path is absolute: {path}")]
    SchemaFileAbsolutePath {
        /// The `id:` field of the offending rule.
        rule_id: String,
        /// The offending path verbatim from the rule.
        path: Utf8PathBuf,
    },

    /// M11 Phase 3: `check.schema_file` resolved (after canonicalisation)
    /// to a path outside the rule directory. Per design D2 the schema
    /// file must live as a sibling of the rule's `.yml` source — `..`
    /// escape attempts are rejected.
    #[error("rule {rule_id} schema_file escapes rule directory: {path}")]
    SchemaFileEscapesRuleDir {
        /// The `id:` field of the offending rule.
        rule_id: String,
        /// The offending path verbatim from the rule.
        path: Utf8PathBuf,
    },
}

impl ExecError {
    /// Stable, lowercase string identifying the error category.
    ///
    /// Used by the CLI's exit-code mapper and by JSON output formats that
    /// want a stable key independent of the diagnostic message. Each
    /// variant resolves to a snake-case identifier that is part of the
    /// public CLI contract.
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
            Self::CheckMutuallyExclusive { .. } => "check_mutually_exclusive",
            Self::CheckMissing { .. } => "check_missing",
            Self::CompositeIncomplete { .. } => "composite_incomplete",
            Self::CompositeExtractNotArray { .. } => "composite_extract_not_array",
            Self::CompositeExtractMalformed { .. } => "composite_extract_malformed",
            Self::CompositeExtractUnknownFormat { .. } => "composite_extract_unknown_format",
            Self::CompositeDepthExceeded { .. } => "composite_depth_exceeded",
            Self::SchemaCompile { .. } => "schema_compile",
            Self::SchemaFileAbsolutePath { .. } => "schema_file_absolute_path",
            Self::SchemaFileEscapesRuleDir { .. } => "schema_file_escapes_rule_dir",
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

    #[test]
    fn kind_name_covers_check_mutually_exclusive_variant() {
        let err = ExecError::CheckMutuallyExclusive {
            rule_id: "a.b".to_owned(),
            present_fields: vec!["jq".to_owned(), "schema".to_owned()],
        };
        assert_eq!(err.kind_name(), "check_mutually_exclusive");
        let formatted = format!("{err}");
        assert!(formatted.contains("a.b"), "got: {formatted}");
        assert!(formatted.contains("jq"), "got: {formatted}");
        assert!(formatted.contains("schema"), "got: {formatted}");
    }

    #[test]
    fn kind_name_covers_check_missing_variant() {
        let err = ExecError::CheckMissing {
            rule_id: "a.b".to_owned(),
        };
        assert_eq!(err.kind_name(), "check_missing");
    }

    #[test]
    fn kind_name_covers_composite_incomplete_variant() {
        let err = ExecError::CompositeIncomplete {
            rule_id: "a.b".to_owned(),
            missing_field: "nested".to_owned(),
        };
        assert_eq!(err.kind_name(), "composite_incomplete");
        let formatted = format!("{err}");
        assert!(formatted.contains("nested"), "got: {formatted}");
    }

    #[test]
    fn kind_name_covers_composite_extract_not_array_variant() {
        let err = ExecError::CompositeExtractNotArray {
            rule_id: "a.b".to_owned(),
        };
        assert_eq!(err.kind_name(), "composite_extract_not_array");
    }

    #[test]
    fn kind_name_covers_composite_extract_malformed_variant() {
        let err = ExecError::CompositeExtractMalformed {
            rule_id: "a.b".to_owned(),
            missing_field: "anchor".to_owned(),
        };
        assert_eq!(err.kind_name(), "composite_extract_malformed");
    }

    #[test]
    fn kind_name_covers_composite_extract_unknown_format_variant() {
        let err = ExecError::CompositeExtractUnknownFormat {
            rule_id: "a.b".to_owned(),
            format: "klingon".to_owned(),
        };
        assert_eq!(err.kind_name(), "composite_extract_unknown_format");
    }

    #[test]
    fn kind_name_covers_composite_depth_exceeded_variant() {
        let err = ExecError::CompositeDepthExceeded {
            rule_id: "a.b".to_owned(),
            depth: 5,
            max: 4,
        };
        assert_eq!(err.kind_name(), "composite_depth_exceeded");
    }

    #[test]
    fn kind_name_covers_schema_compile_variant() {
        let err = ExecError::SchemaCompile {
            rule_id: "js.bad".to_owned(),
            message: "unresolvable $ref".to_owned(),
        };
        assert_eq!(err.kind_name(), "schema_compile");
        let formatted = format!("{err}");
        assert!(formatted.contains("js.bad"), "got: {formatted}");
    }

    #[test]
    fn kind_name_covers_schema_file_absolute_path_variant() {
        let err = ExecError::SchemaFileAbsolutePath {
            rule_id: "a.b".to_owned(),
            path: Utf8PathBuf::from("/etc/passwd"),
        };
        assert_eq!(err.kind_name(), "schema_file_absolute_path");
    }

    #[test]
    fn kind_name_covers_schema_file_escapes_rule_dir_variant() {
        let err = ExecError::SchemaFileEscapesRuleDir {
            rule_id: "a.b".to_owned(),
            path: Utf8PathBuf::from("../../../etc/passwd"),
        };
        assert_eq!(err.kind_name(), "schema_file_escapes_rule_dir");
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
