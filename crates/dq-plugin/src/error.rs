//! Domain error type for `dq-plugin`.
//!
//! Variants carry enough structured context that downstream renderers (the
//! CLI's reporters and exit-code mapper) can produce diagnostics with a
//! stable category string via [`PluginError::kind_name`].
//!
//! The set of variants is fixed by
//! `openspec/changes/add-ir-foundation/specs/data-query-plugin-abi/spec.md`
//! — the `kind_name()` strings are part of the CLI exit-code contract and
//! must not be renamed.

use camino::Utf8PathBuf;
use thiserror::Error;

/// Result alias for the `dq-plugin` crate. Mirrors the `dq-core` /
/// `dq-exec` / `dq-transform` convention so `?` works ergonomically through
/// the plugin runtime pipeline.
pub type Result<T> = std::result::Result<T, PluginError>;

/// Errors surfaced by the WASM plugin runtime — feature-gating, schema
/// version mismatch, sandbox limits, fix-payload parse failures, and load /
/// invoke failures.
///
/// Each variant carries the minimum context required for the CLI's reporter
/// and exit-code mapper to render a useful diagnostic. The
/// [`PluginError::kind_name`] strings drive exit-code routing per the
/// `data-query-plugin-abi` spec.
#[derive(Debug, Error)]
pub enum PluginError {
    /// The CLI was built without the `plugins` feature, so any attempt to
    /// load or invoke a plugin fails at the API entry point.
    ///
    /// `hint` is the user-facing remediation string surfaced by the
    /// reporter (typically `"rebuild with --features plugins"`).
    #[error("plugins are not enabled in this build; {hint}")]
    FeatureDisabled {
        /// User-facing remediation hint included in the rendered message.
        hint: String,
    },

    /// The plugin declares a WIT package version with a different *major*
    /// component than the host's compiled-against schema. Per semver, major
    /// bumps signal breaking changes; the runtime refuses to load the
    /// plugin rather than silently mis-marshal records.
    #[error(
        "plugin WIT schema version mismatch: plugin requires {plugin_version}, host links {host_version}"
    )]
    SchemaVersion {
        /// `<major>.<minor>.<patch>` declared by the plugin module.
        plugin_version: String,
        /// `<major>.<minor>.<patch>` the host crate was compiled against.
        host_version: String,
    },

    /// The plugin exhausted its per-invocation fuel budget (≈ 100M units,
    /// ~1s of CPU). Surfaced as `wasmtime::Trap::Interrupt` and remapped
    /// here so the CLI can route to a stable exit code.
    #[error("plugin rule {rule_id} exhausted fuel budget")]
    Exhausted {
        /// `rule-id` of the offending plugin invocation.
        rule_id: String,
    },

    /// The plugin tried to allocate beyond the per-instance memory limit
    /// (64 MiB), enforced via `Store::limiter`.
    #[error("plugin rule {rule_id} exceeded memory limit")]
    Memory {
        /// `rule-id` of the offending plugin invocation.
        rule_id: String,
    },

    /// The plugin imports an interface that the runtime does not provide
    /// (typically `wasi_snapshot_preview1`). Plugins are sandboxed: no
    /// filesystem, no network, no process control.
    #[error("plugin imports disallowed interface {interface}")]
    DisallowedImport {
        /// Name of the import the runtime refused to satisfy.
        interface: String,
    },

    /// The plugin's `fix` export returned bytes that did not parse as a
    /// JSON Patch / [`dq_core::EditScript`]. The underlying
    /// `serde_json::Error` is chained via `#[source]` for callers that want
    /// the parser-level details.
    #[error("plugin rule {rule_id} returned malformed fix payload")]
    MalformedFix {
        /// `rule-id` of the offending plugin invocation.
        rule_id: String,
        /// Underlying `serde_json` deserialization error.
        #[source]
        source: serde_json::Error,
    },

    /// `wasmtime::Component::from_file` (or the surrounding load pipeline)
    /// returned an error before the plugin was fully linked. Distinct from
    /// `Invoke` because the failure is attributable to the artifact (bad
    /// magic bytes, malformed component, etc.) rather than to a runtime
    /// trap.
    #[error("failed to load plugin {path}: {message}")]
    Load {
        /// Path to the `.wasm` file that failed to load.
        path: Utf8PathBuf,
        /// Human-readable description of the failure (typically the
        /// upstream `anyhow::Error` rendered to string).
        message: String,
    },

    /// A wasmtime trap not classified as `Exhausted` or `Memory` (a
    /// generic runtime error inside the plugin module). Carries the
    /// rule-id under which the failure happened so the reporter can point
    /// the user at the offending plugin invocation.
    #[error("plugin rule {rule_id} invocation failed: {message}")]
    Invoke {
        /// `rule-id` of the offending plugin invocation.
        rule_id: String,
        /// Human-readable description of the trap.
        message: String,
    },
}

impl PluginError {
    /// Stable, lowercase string identifying the error category.
    ///
    /// Used by the CLI's exit-code mapper: `feature_disabled` /
    /// `disallowed_import` route to `InvalidInput` (6); `schema_version` /
    /// `malformed_fix` route to `PARSE_ERROR` (3); `exhausted` / `memory` /
    /// `invoke` / `load` route to `RUNTIME_ERROR` (4). Renaming any of
    /// these strings is a breaking change.
    #[must_use]
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::FeatureDisabled { .. } => "feature_disabled",
            Self::SchemaVersion { .. } => "schema_version",
            Self::Exhausted { .. } => "exhausted",
            Self::Memory { .. } => "memory",
            Self::DisallowedImport { .. } => "disallowed_import",
            Self::MalformedFix { .. } => "malformed_fix",
            Self::Load { .. } => "load",
            Self::Invoke { .. } => "invoke",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::collections::HashSet;

    /// Force a `serde_json::Error` so the `MalformedFix` variant can be
    /// constructed without coupling tests to private parser internals.
    fn sample_serde_json_error() -> serde_json::Error {
        // Parsing an invalid JSON document produces a typed error. Exact
        // wording is irrelevant — we just need an owned value.
        serde_json::from_str::<serde_json::Value>("{not json").expect_err("parse should fail")
    }

    #[test]
    fn kind_name_covers_feature_disabled_variant() {
        let err = PluginError::FeatureDisabled {
            hint: "rebuild with --features plugins".to_owned(),
        };
        assert_eq!(err.kind_name(), "feature_disabled");
        let formatted = format!("{err}");
        assert!(
            formatted.contains("plugins are not enabled"),
            "expected display to mention feature gate, got: {formatted}",
        );
    }

    #[test]
    fn kind_name_covers_schema_version_variant() {
        let err = PluginError::SchemaVersion {
            plugin_version: "2.0.0".to_owned(),
            host_version: "0.1.0".to_owned(),
        };
        assert_eq!(err.kind_name(), "schema_version");
        let formatted = format!("{err}");
        assert!(
            formatted.contains("2.0.0") && formatted.contains("0.1.0"),
            "expected display to mention both versions, got: {formatted}",
        );
    }

    #[test]
    fn kind_name_covers_exhausted_variant() {
        let err = PluginError::Exhausted {
            rule_id: "x.infinite-loop".to_owned(),
        };
        assert_eq!(err.kind_name(), "exhausted");
        let formatted = format!("{err}");
        assert!(
            formatted.contains("x.infinite-loop"),
            "expected display to mention the rule id, got: {formatted}",
        );
    }

    #[test]
    fn kind_name_covers_memory_variant() {
        let err = PluginError::Memory {
            rule_id: "x.allocates-too-much".to_owned(),
        };
        assert_eq!(err.kind_name(), "memory");
        let formatted = format!("{err}");
        assert!(
            formatted.contains("x.allocates-too-much"),
            "expected display to mention the rule id, got: {formatted}",
        );
    }

    #[test]
    fn kind_name_covers_disallowed_import_variant() {
        let err = PluginError::DisallowedImport {
            interface: "wasi_snapshot_preview1".to_owned(),
        };
        assert_eq!(err.kind_name(), "disallowed_import");
        let formatted = format!("{err}");
        assert!(
            formatted.contains("wasi_snapshot_preview1"),
            "expected display to mention the interface, got: {formatted}",
        );
    }

    #[test]
    fn kind_name_covers_malformed_fix_variant() {
        let err = PluginError::MalformedFix {
            rule_id: "x.bad-fix".to_owned(),
            source: sample_serde_json_error(),
        };
        assert_eq!(err.kind_name(), "malformed_fix");
        let formatted = format!("{err}");
        assert!(
            formatted.contains("x.bad-fix"),
            "expected display to mention the rule id, got: {formatted}",
        );
        // `#[source]` chains the underlying serde_json::Error — verify the
        // chain is wired so reporters that walk `error::source()` see it.
        let source: &dyn std::error::Error = &err;
        assert!(
            source.source().is_some(),
            "MalformedFix must chain the serde_json::Error via #[source]",
        );
    }

    #[test]
    fn kind_name_covers_load_variant() {
        let err = PluginError::Load {
            path: Utf8PathBuf::from("/no/such/plugin.wasm"),
            message: "invalid magic bytes".to_owned(),
        };
        assert_eq!(err.kind_name(), "load");
        let formatted = format!("{err}");
        assert!(
            formatted.contains("/no/such/plugin.wasm"),
            "expected display to mention the path, got: {formatted}",
        );
    }

    #[test]
    fn kind_name_covers_invoke_variant() {
        let err = PluginError::Invoke {
            rule_id: "x.traps".to_owned(),
            message: "unreachable executed".to_owned(),
        };
        assert_eq!(err.kind_name(), "invoke");
        let formatted = format!("{err}");
        assert!(
            formatted.contains("x.traps"),
            "expected display to mention the rule id, got: {formatted}",
        );
    }

    /// Sanity-check that no two variants share the same `kind_name()`.
    /// Collisions would silently break the CLI exit-code mapper.
    #[test]
    fn no_two_variants_share_a_kind_name() {
        let names = [
            PluginError::FeatureDisabled {
                hint: String::new(),
            }
            .kind_name(),
            PluginError::SchemaVersion {
                plugin_version: String::new(),
                host_version: String::new(),
            }
            .kind_name(),
            PluginError::Exhausted {
                rule_id: String::new(),
            }
            .kind_name(),
            PluginError::Memory {
                rule_id: String::new(),
            }
            .kind_name(),
            PluginError::DisallowedImport {
                interface: String::new(),
            }
            .kind_name(),
            PluginError::MalformedFix {
                rule_id: String::new(),
                source: sample_serde_json_error(),
            }
            .kind_name(),
            PluginError::Load {
                path: Utf8PathBuf::new(),
                message: String::new(),
            }
            .kind_name(),
            PluginError::Invoke {
                rule_id: String::new(),
                message: String::new(),
            }
            .kind_name(),
        ];
        let unique: HashSet<&'static str> = names.iter().copied().collect();
        assert_eq!(
            unique.len(),
            names.len(),
            "every variant must return a distinct kind_name(), got: {names:?}",
        );
    }

    /// Spec scenario "kind_name covers every variant" (Task 5.14).
    ///
    /// One consolidated assertion that exercises BOTH spec contracts in a
    /// single test:
    ///
    /// 1. `kind_name()` is unique across every variant (`HashSet::len() ==
    ///    variant count`); collisions would silently break the CLI exit-code
    ///    mapper.
    /// 2. The set of returned strings equals the canonical eight names listed
    ///    in `data-query-plugin-abi/spec.md` Requirement
    ///    "`PluginError` exposes `kind_name()` for stable exit-code mapping".
    ///
    /// The per-variant tests above cover individual `Display` formatting and
    /// per-variant `kind_name()` strings; this test covers the *aggregate*
    /// contract that they must collectively satisfy. If a new variant is
    /// added and this test isn't updated, either the uniqueness assertion or
    /// the spec-canonical-set assertion fires before the gap reaches users.
    #[test]
    fn kind_name_covers_every_variant_uniquely_and_matches_spec() {
        let names: [&'static str; 8] = [
            PluginError::FeatureDisabled {
                hint: String::new(),
            }
            .kind_name(),
            PluginError::SchemaVersion {
                plugin_version: String::new(),
                host_version: String::new(),
            }
            .kind_name(),
            PluginError::Exhausted {
                rule_id: String::new(),
            }
            .kind_name(),
            PluginError::Memory {
                rule_id: String::new(),
            }
            .kind_name(),
            PluginError::DisallowedImport {
                interface: String::new(),
            }
            .kind_name(),
            PluginError::MalformedFix {
                rule_id: String::new(),
                source: sample_serde_json_error(),
            }
            .kind_name(),
            PluginError::Load {
                path: Utf8PathBuf::new(),
                message: String::new(),
            }
            .kind_name(),
            PluginError::Invoke {
                rule_id: String::new(),
                message: String::new(),
            }
            .kind_name(),
        ];
        let set: HashSet<&'static str> = names.iter().copied().collect();
        assert_eq!(
            set.len(),
            8,
            "every variant must return a distinct kind_name(); duplicates: {names:?}",
        );
        let canonical: HashSet<&'static str> = [
            "feature_disabled",
            "schema_version",
            "exhausted",
            "memory",
            "disallowed_import",
            "malformed_fix",
            "load",
            "invoke",
        ]
        .into_iter()
        .collect();
        assert_eq!(
            set, canonical,
            "kind_name() set must match the eight canonical names from \
             data-query-plugin-abi/spec.md; got {set:?} expected {canonical:?}",
        );
    }

    /// Sanity-check that every canonical name from the spec is covered. If
    /// the spec gains a new variant and we forget to add it here, this test
    /// fails before the CLI exit-code mapper sees the gap.
    #[test]
    fn kind_name_set_matches_spec_canonical_names() {
        let names: HashSet<&'static str> = [
            PluginError::FeatureDisabled {
                hint: String::new(),
            }
            .kind_name(),
            PluginError::SchemaVersion {
                plugin_version: String::new(),
                host_version: String::new(),
            }
            .kind_name(),
            PluginError::Exhausted {
                rule_id: String::new(),
            }
            .kind_name(),
            PluginError::Memory {
                rule_id: String::new(),
            }
            .kind_name(),
            PluginError::DisallowedImport {
                interface: String::new(),
            }
            .kind_name(),
            PluginError::MalformedFix {
                rule_id: String::new(),
                source: sample_serde_json_error(),
            }
            .kind_name(),
            PluginError::Load {
                path: Utf8PathBuf::new(),
                message: String::new(),
            }
            .kind_name(),
            PluginError::Invoke {
                rule_id: String::new(),
                message: String::new(),
            }
            .kind_name(),
        ]
        .into_iter()
        .collect();
        let expected: HashSet<&'static str> = [
            "feature_disabled",
            "schema_version",
            "exhausted",
            "memory",
            "disallowed_import",
            "malformed_fix",
            "load",
            "invoke",
        ]
        .into_iter()
        .collect();
        assert_eq!(names, expected);
    }
}
