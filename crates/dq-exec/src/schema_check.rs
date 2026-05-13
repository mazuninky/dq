//! JSON Schema 2020-12 compilation + validation for the `Check::Schema`
//! and `Check::SchemaFile` rule variants (M11 Phase 3).
//!
//! The compiled validator is built once at `Evaluator::new` and stored on
//! the [`crate::evaluator::CompiledRule`] so per-file `evaluate_file`
//! calls reuse it without recompilation. The `$ref` resolver is the
//! crate's default registry (no HTTP / file resolvers wired up): per
//! design D2 the schema validates in isolation, with no network or
//! filesystem reads beyond the rule's own sibling `*.schema.json`.
//!
//! ## Path safety for `schema_file`
//!
//! [`resolve_schema_file_path`] canonicalises the rule directory and the
//! requested schema path, then verifies the schema path is a descendant
//! of the rule directory. Absolute paths and `..`-escapes are rejected
//! up front with [`crate::ExecError::SchemaFileAbsolutePath`] /
//! [`crate::ExecError::SchemaFileEscapesRuleDir`].

use camino::{Utf8Path, Utf8PathBuf};

use crate::error::{ExecError, Result};
use crate::ruleset::RuleSource;

/// One compiled schema check, cached on the per-rule
/// [`crate::evaluator::CompiledRule`].
///
/// Cloning this struct is cheap — `jsonschema::Validator` is `Arc`-backed
/// internally — but the runtime never needs to clone it because each
/// [`crate::evaluator::CompiledRule`] is wrapped in [`std::sync::Arc`]
/// already.
#[derive(Debug)]
pub(crate) struct CompiledSchemaCheck {
    /// Compiled JSON Schema 2020-12 validator. Reused across every
    /// `evaluate_file` call against the owning [`crate::Evaluator`].
    pub(crate) validator: jsonschema::Validator,
    /// Optional message prefix — when set, prepended to the
    /// auto-generated `keywordLocation`-based message in each
    /// diagnostic.
    pub(crate) message_prefix: Option<String>,
}

/// Compile an inline `serde_norway::Value` schema into a
/// [`CompiledSchemaCheck`].
///
/// `rule_id` flows into the [`ExecError::SchemaCompile`] payload so the
/// error message names the offending rule.
pub(crate) fn compile_inline(
    rule_id: &str,
    schema_yaml: &serde_norway::Value,
    message_prefix: Option<String>,
) -> Result<CompiledSchemaCheck> {
    let schema_json = yaml_to_serde_json(schema_yaml).map_err(|err| ExecError::SchemaCompile {
        rule_id: rule_id.to_owned(),
        message: format!("could not convert YAML schema to JSON: {err}"),
    })?;
    compile_json(rule_id, &schema_json, message_prefix)
}

/// Compile an already-JSON-shaped schema. Shared by inline and
/// embedded paths.
fn compile_json(
    rule_id: &str,
    schema_json: &serde_json::Value,
    message_prefix: Option<String>,
) -> Result<CompiledSchemaCheck> {
    // Per spec scenario "HTTP $ref rejected at compile-time" we walk
    // the schema up front and reject any `$ref` that points at an
    // external URL. Without this, jsonschema's default registry
    // happily compiles the schema and only fails at validate-time when
    // the `$ref` is actually traversed — which the spec does not
    // accept.
    if let Some(uri) = find_external_ref(schema_json) {
        return Err(ExecError::SchemaCompile {
            rule_id: rule_id.to_owned(),
            message: format!("external $ref is not supported: {uri}"),
        });
    }
    let validator =
        jsonschema::draft202012::new(schema_json).map_err(|err| ExecError::SchemaCompile {
            rule_id: rule_id.to_owned(),
            message: format!("{err}"),
        })?;
    Ok(CompiledSchemaCheck {
        validator,
        message_prefix,
    })
}

/// Recursively walk `schema` looking for a `$ref` that points at an
/// external resource (`http://` / `https://` / `file://` / a path with
/// any non-fragment scheme). Returns the offending URI if any.
///
/// Internal `$ref` (`#/...`) and pure JSON-pointer fragments are
/// allowed — those are resolved entirely from within the schema's own
/// `$id` graph and don't trigger the default retriever.
fn find_external_ref(schema: &serde_json::Value) -> Option<&str> {
    fn is_external(uri: &str) -> bool {
        // RFC 3986: a URI with a scheme is `scheme:rest`. We treat
        // anything other than a pure fragment-only reference as
        // potentially external. This is conservative — relative paths
        // like `./other.json` would also be flagged, which matches the
        // spec intent (the validator's default retriever can't fetch
        // them either).
        if uri.starts_with('#') || uri.is_empty() {
            return false;
        }
        // Look for a scheme separator before any path-like char.
        match uri.find(':') {
            Some(idx) => {
                let scheme = &uri[..idx];
                // A scheme is alpha (1+) followed by alphanumerics / +
                // / - / .
                !scheme.is_empty()
                    && scheme
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_alphabetic())
            }
            // Relative paths without a scheme are still external for
            // the default-registry validator's purposes.
            None => true,
        }
    }
    match schema {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(uri)) = map.get("$ref")
                && is_external(uri)
            {
                return Some(uri.as_str());
            }
            for child in map.values() {
                if let Some(found) = find_external_ref(child) {
                    return Some(found);
                }
            }
            None
        }
        serde_json::Value::Array(items) => {
            for item in items {
                if let Some(found) = find_external_ref(item) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

/// Compile a `schema_file:` path into a [`CompiledSchemaCheck`].
///
/// `rule_source` is the provenance of the rule that declared
/// `schema_file:` — used to derive the rule directory for relative-path
/// resolution. Inline rules ([`RuleSource::Inline`]) and `@std/`
/// embedded rulesets ([`RuleSource::Std`]) need a different path: the
/// former isn't supported (no rule directory exists), the latter is
/// handled via the `dq-lint` embedding table by the caller and never
/// reaches this function.
pub(crate) fn compile_from_file(
    rule_id: &str,
    schema_file: &Utf8Path,
    rule_source: &RuleSource,
    message_prefix: Option<String>,
) -> Result<CompiledSchemaCheck> {
    let path = resolve_schema_file_path(rule_id, schema_file, rule_source)?;
    let bytes = std::fs::read(path.as_std_path()).map_err(|err| ExecError::Io {
        path: path.clone(),
        source: err,
    })?;
    let schema_json: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|err| ExecError::SchemaCompile {
            rule_id: rule_id.to_owned(),
            message: format!("could not parse {path} as JSON: {err}"),
        })?;
    compile_json(rule_id, &schema_json, message_prefix)
}

/// Compile a schema sourced from a `&'static str` (the embedded
/// `@std/jsonschema/*` rule library).
///
/// Used when the rule's `RuleSource` is [`RuleSource::Std`]. The bytes
/// come from `dq_lint::std_schema(namespace, file)`; the caller
/// ([`crate::evaluator`]) decides between this entry point and
/// [`compile_from_file`] based on the rule source.
pub(crate) fn compile_from_embedded(
    rule_id: &str,
    schema_text: &str,
    message_prefix: Option<String>,
) -> Result<CompiledSchemaCheck> {
    let schema_json: serde_json::Value =
        serde_json::from_str(schema_text).map_err(|err| ExecError::SchemaCompile {
            rule_id: rule_id.to_owned(),
            message: format!("could not parse embedded schema as JSON: {err}"),
        })?;
    compile_json(rule_id, &schema_json, message_prefix)
}

/// Resolve a relative schema-file path against the rule's directory.
///
/// Returns the canonicalised path on success. On failure, returns one
/// of [`ExecError::SchemaFileAbsolutePath`] /
/// [`ExecError::SchemaFileEscapesRuleDir`] / [`ExecError::Io`].
pub(crate) fn resolve_schema_file_path(
    rule_id: &str,
    schema_file: &Utf8Path,
    rule_source: &RuleSource,
) -> Result<Utf8PathBuf> {
    if schema_file.is_absolute() {
        return Err(ExecError::SchemaFileAbsolutePath {
            rule_id: rule_id.to_owned(),
            path: schema_file.to_path_buf(),
        });
    }
    // Determine the rule directory.
    let rule_path = match rule_source {
        RuleSource::Local(path) => path.clone(),
        RuleSource::Inline | RuleSource::Std(_) => {
            // Inline rules can't address sibling files because there's
            // no parent directory; embedded `@std/` rules go through
            // the dedicated `compile_from_embedded` path. Reaching this
            // arm means a caller mis-routed the rule source — treat it
            // as a SchemaFile escape so the user sees a structured
            // error rather than a panic.
            return Err(ExecError::SchemaFileEscapesRuleDir {
                rule_id: rule_id.to_owned(),
                path: schema_file.to_path_buf(),
            });
        }
    };
    // The rule path may be either a single rule file or a directory
    // (when the loader walked a directory). Use the parent for files.
    let rule_dir = if rule_path.is_dir() {
        rule_path.clone()
    } else {
        rule_path
            .parent()
            .map(Utf8Path::to_path_buf)
            .unwrap_or_else(|| Utf8PathBuf::from("."))
    };
    let canonical_rule_dir = canonicalize_utf8(&rule_dir).map_err(|err| ExecError::Io {
        path: rule_dir,
        source: err,
    })?;
    let candidate = canonical_rule_dir.join(schema_file);
    let canonical_candidate = canonicalize_utf8(&candidate).map_err(|err| ExecError::Io {
        path: candidate.clone(),
        source: err,
    })?;
    if !canonical_candidate.starts_with(&canonical_rule_dir) {
        return Err(ExecError::SchemaFileEscapesRuleDir {
            rule_id: rule_id.to_owned(),
            path: schema_file.to_path_buf(),
        });
    }
    Ok(canonical_candidate)
}

/// Canonicalise a [`Utf8Path`], preserving UTF-8-ness.
fn canonicalize_utf8(path: &Utf8Path) -> std::io::Result<Utf8PathBuf> {
    let canonical = std::fs::canonicalize(path.as_std_path())?;
    Utf8PathBuf::from_path_buf(canonical).map_err(|p| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("non-UTF-8 canonical path: {p:?}"),
        )
    })
}

/// Convert a `serde_norway::Value` to a `serde_json::Value`.
///
/// `serde_norway`'s `Value` is YAML-shaped: it carries tagged variants and
/// allows non-string mapping keys. The `jsonschema` crate consumes
/// `serde_json::Value`, so we round-trip through `serde_norway::to_string`
/// then `serde_norway::from_str::<serde_json::Value>` — which mirrors
/// what the test runner does for fixture inputs and gives us identical
/// key-coercion semantics.
fn yaml_to_serde_json(
    value: &serde_norway::Value,
) -> std::result::Result<serde_json::Value, String> {
    // Round-trip via the YAML serializer to handle tagged values and
    // non-string keys cleanly. `serde_norway::from_str::<serde_json::Value>`
    // accepts the YAML text and emits a JSON-shaped value (or fails
    // with a typed error if a non-string key sneaks through).
    let yaml_text =
        serde_norway::to_string(value).map_err(|err| format!("yaml serialize: {err}"))?;
    serde_norway::from_str::<serde_json::Value>(&yaml_text)
        .map_err(|err| format!("yaml→json transcode: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ruleset::RuleSource;
    use pretty_assertions::assert_eq;

    #[test]
    fn compile_inline_with_valid_schema_succeeds() {
        let yaml: serde_norway::Value = serde_norway::from_str(
            r#"
type: object
required: [name]
properties:
  name:
    type: string
"#,
        )
        .expect("yaml parse");
        let compiled =
            compile_inline("test.rule", &yaml, Some("prefix: ".to_owned())).expect("compile ok");
        assert_eq!(compiled.message_prefix.as_deref(), Some("prefix: "));
        let valid = serde_json::json!({"name": "x"});
        let invalid = serde_json::json!({});
        assert!(compiled.validator.is_valid(&valid));
        assert!(!compiled.validator.is_valid(&invalid));
    }

    #[test]
    fn compile_inline_rejects_http_ref() {
        // Per spec scenario "HTTP $ref rejected at compile-time": a
        // schema that declares `$ref: https://...` must fail compile
        // because no HTTP resolver is wired up (resolve-http feature
        // is off in our `default-features = false` configuration).
        let yaml: serde_norway::Value = serde_norway::from_str(
            r#"
$ref: "https://json-schema.org/draft/2020-12/schema"
"#,
        )
        .expect("yaml parse");
        let result = compile_inline("test.bad", &yaml, None);
        match result {
            Err(ExecError::SchemaCompile { rule_id, .. }) => {
                assert_eq!(rule_id, "test.bad");
            }
            other => panic!("expected SchemaCompile error, got {other:?}"),
        }
    }

    #[test]
    fn resolve_schema_file_rejects_absolute_path() {
        let source = RuleSource::Local(Utf8PathBuf::from("/tmp/dq-rules/a.yml"));
        let err = resolve_schema_file_path("test.rule", Utf8Path::new("/etc/passwd"), &source)
            .expect_err("absolute path must be rejected");
        match err {
            ExecError::SchemaFileAbsolutePath { rule_id, path } => {
                assert_eq!(rule_id, "test.rule");
                assert_eq!(path, Utf8PathBuf::from("/etc/passwd"));
            }
            other => panic!("expected SchemaFileAbsolutePath, got {other:?}"),
        }
    }

    #[test]
    fn resolve_schema_file_rejects_inline_source() {
        let source = RuleSource::Inline;
        let err = resolve_schema_file_path("test.rule", Utf8Path::new("./x.json"), &source)
            .expect_err("inline source has no rule dir");
        match err {
            ExecError::SchemaFileEscapesRuleDir { rule_id, .. } => {
                assert_eq!(rule_id, "test.rule");
            }
            other => panic!("expected SchemaFileEscapesRuleDir, got {other:?}"),
        }
    }

    #[test]
    fn resolve_schema_file_finds_relative_sibling() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dir_path = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("UTF-8 path");
        let rule_path = dir_path.join("rule.yml");
        std::fs::write(&rule_path, "id: a.b\n").expect("write rule file");
        let schema_path = dir_path.join("shape.schema.json");
        std::fs::write(&schema_path, r#"{"type":"object"}"#).expect("write schema");

        let source = RuleSource::Local(rule_path);
        let resolved =
            resolve_schema_file_path("test.rule", Utf8Path::new("./shape.schema.json"), &source)
                .expect("resolve ok");
        // The canonical resolved path must end in shape.schema.json
        // (canonicalize may resolve symlinks like /private/tmp).
        assert!(resolved.ends_with("shape.schema.json"), "got: {resolved}");
    }

    #[test]
    fn resolve_schema_file_rejects_dotdot_escape() {
        // Layout:
        //   <tmp>/rules/foo/rule.yml
        //   <tmp>/secrets.json
        // schema_file: ../../secrets.json — must be rejected.
        let dir = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("UTF-8 path");
        let foo = root.join("rules").join("foo");
        std::fs::create_dir_all(&foo).expect("create rule dir");
        let rule_path = foo.join("rule.yml");
        std::fs::write(&rule_path, "id: a.b\n").expect("write rule");
        // The escape target must exist for canonicalize to resolve it.
        let secret = root.join("secrets.json");
        std::fs::write(&secret, "{}").expect("write secret");

        let source = RuleSource::Local(rule_path);
        let err =
            resolve_schema_file_path("test.rule", Utf8Path::new("../../secrets.json"), &source)
                .expect_err("escape must be rejected");
        match err {
            ExecError::SchemaFileEscapesRuleDir { rule_id, .. } => {
                assert_eq!(rule_id, "test.rule");
            }
            other => panic!("expected SchemaFileEscapesRuleDir, got {other:?}"),
        }
    }

    #[test]
    fn compile_from_embedded_succeeds_for_basic_schema() {
        let schema_text =
            r#"{"type":"object","required":["name"],"properties":{"name":{"type":"string"}}}"#;
        let compiled = compile_from_embedded("test.rule", schema_text, None).expect("compile ok");
        assert!(
            compiled
                .validator
                .is_valid(&serde_json::json!({"name":"x"}))
        );
        assert!(!compiled.validator.is_valid(&serde_json::json!({})));
    }

    #[test]
    fn compile_from_embedded_rejects_malformed_json() {
        let err = compile_from_embedded("test.rule", "{not json}", None)
            .expect_err("malformed schema must be rejected");
        match err {
            ExecError::SchemaCompile { rule_id, .. } => assert_eq!(rule_id, "test.rule"),
            other => panic!("expected SchemaCompile, got {other:?}"),
        }
    }
}
