//! JSON Schema 2020-12 compilation + validation for the `Check::Schema`
//! and `Check::SchemaFile` rule variants (M11 Phase 3).
//!
//! The compiled validator is built once at `Evaluator::new` and stored on
//! the [`crate::evaluator::CompiledRule`] so per-file `evaluate_file`
//! calls reuse it without recompilation. External `$ref` is closed off
//! three ways: [`find_external_ref`] rejects any non-fragment `$ref` before
//! the validator is built, the validator is built `offline()` (a retriever
//! that refuses every URI outside the registry), and the `resolve-http` /
//! `resolve-file` features are off. Per design D2 the schema validates in
//! isolation, with no network or filesystem reads beyond the rule's own
//! sibling `*.schema.json`.
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
    // A `$schema` naming a meta-schema the validator does not bundle is
    // an external reference too. jsonschema ≤ 0.34 tried to retrieve it
    // while building the registry and failed the compile; 0.61 with an
    // explicit draft never looks it up and validates as 2020-12 instead.
    // Keep the compile-time rejection — and its wording — here.
    if let Some(message) = find_unresolvable_meta_schema(schema_json) {
        return Err(ExecError::SchemaCompile {
            rule_id: rule_id.to_owned(),
            message,
        });
    }
    // Second layer, independent of Cargo features: `offline()` swaps the
    // default retriever for one that refuses every URI not already in
    // the registry, so even if `resolve-http` / `resolve-file` were ever
    // unified on by another crate in the graph, the validator still
    // cannot reach the network or the filesystem.
    let validator = jsonschema::draft202012::options()
        .offline()
        .build(schema_json)
        .map_err(|err| ExecError::SchemaCompile {
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

/// Find a `$schema` URI that jsonschema 0.34 could not resolve offline and
/// return the compile error it reported for it.
///
/// 0.34 resolved every `$schema` in the document against its registry:
/// URIs under `https://json-schema.org/draft/` / `http://json-schema.org/draft-`
/// (the bundled meta-schemas) and the document's own `$id`s resolve; any
/// other URI went to the retriever, which had no HTTP or file support.
fn find_unresolvable_meta_schema(schema: &serde_json::Value) -> Option<String> {
    let mut ids = Vec::new();
    collect_ids(schema, None, &mut ids);
    let uri = find_meta_schema_uri(schema, None, &|raw, resolved| {
        !(raw.starts_with("https://json-schema.org/draft/")
            || raw.starts_with("http://json-schema.org/draft-")
            || resolved.is_empty()
            || ids.iter().any(|id| id == resolved))
    })?;
    let (normalized, scheme) = normalize_uri(&uri);
    let reason = match scheme.as_deref() {
        Some("http" | "https") => {
            "`resolve-http` feature or a custom resolver is required to resolve external \
             schemas via HTTP"
                .to_owned()
        }
        Some("file") => "`resolve-file` feature or a custom resolver is required to resolve \
                         external schemas via files"
            .to_owned(),
        Some(other) => format!("Unknown scheme {other}"),
        None => "No base URI is available".to_owned(),
    };
    Some(format!(
        "Resource '{normalized}' is not present in a registry and retrieving it failed: {reason}"
    ))
}

/// Every `$id` of `schema` and its subschemas, resolved against the
/// enclosing `$id` and fragment stripped.
fn collect_ids(schema: &serde_json::Value, base: Option<&str>, out: &mut Vec<String>) {
    let serde_json::Value::Object(map) = schema else {
        return;
    };
    let own = map
        .get("$id")
        .and_then(serde_json::Value::as_str)
        .map(|id| resolve_reference(base, strip_fragment(id)));
    if let Some(id) = &own {
        out.push(id.clone());
    }
    let base = own.as_deref().or(base);
    subschemas(map).for_each(|child| collect_ids(child, base, out));
}

/// The first `$schema` of `schema` and its subschemas, resolved against the
/// enclosing `$id`, for which `unresolvable(raw, resolved)` holds.
fn find_meta_schema_uri(
    schema: &serde_json::Value,
    base: Option<&str>,
    unresolvable: &dyn Fn(&str, &str) -> bool,
) -> Option<String> {
    let serde_json::Value::Object(map) = schema else {
        return None;
    };
    let own = map
        .get("$id")
        .and_then(serde_json::Value::as_str)
        .map(|id| resolve_reference(base, strip_fragment(id)));
    let base = own.as_deref().or(base);
    if let Some(raw) = map.get("$schema").and_then(serde_json::Value::as_str) {
        let resolved = resolve_reference(base, strip_fragment(raw));
        if unresolvable(raw, &resolved) {
            return Some(resolved);
        }
    }
    subschemas(map).find_map(|child| find_meta_schema_uri(child, base, unresolvable))
}

/// Resolve `reference` against `base` (RFC 3986 § 5.2, without
/// dot-segment removal); a reference with a scheme, or no base, is
/// returned as is.
fn resolve_reference(base: Option<&str>, reference: &str) -> String {
    let Some(base) = base else {
        return reference.to_owned();
    };
    if reference.is_empty() {
        return base.to_owned();
    }
    if uri_scheme(reference).is_some() {
        return reference.to_owned();
    }
    let Some(scheme) = uri_scheme(base) else {
        return reference.to_owned();
    };
    let after_scheme = &base[scheme.len() + 1..];
    if let Some(network_path) = reference.strip_prefix("//") {
        return format!("{scheme}://{network_path}");
    }
    let (authority, path) = match after_scheme.strip_prefix("//") {
        Some(rest) => {
            let end = rest.find('/').unwrap_or(rest.len());
            (format!("//{}", &rest[..end]), &rest[end..])
        }
        None => (String::new(), after_scheme),
    };
    let path = path.split(['?', '#']).next().unwrap_or("");
    if reference.starts_with('/') {
        return format!("{scheme}:{authority}{reference}");
    }
    let directory = path.rfind('/').map_or("", |idx| &path[..=idx]);
    let directory = if directory.is_empty() && !authority.is_empty() {
        "/"
    } else {
        directory
    };
    format!("{scheme}:{authority}{directory}{reference}")
}

/// The scheme of `uri` when it starts with a valid RFC 3986 scheme.
fn uri_scheme(uri: &str) -> Option<&str> {
    let (scheme, _) = uri.split_once(':')?;
    let valid = scheme
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    valid.then_some(scheme)
}

fn strip_fragment(uri: &str) -> &str {
    uri.split_once('#').map_or(uri, |(base, _)| base)
}

/// `uri` with the scheme and authority lowercased (RFC 3986 § 6.2.2.1),
/// plus the lowercased scheme when there is one.
fn normalize_uri(uri: &str) -> (String, Option<String>) {
    let Some(scheme) = uri_scheme(uri) else {
        return (uri.to_owned(), None);
    };
    let rest = &uri[scheme.len() + 1..];
    let scheme = scheme.to_ascii_lowercase();
    let rest = match rest.strip_prefix("//") {
        Some(after) => {
            let end = after.find('/').unwrap_or(after.len());
            format!("//{}{}", after[..end].to_ascii_lowercase(), &after[end..])
        }
        None => rest.to_owned(),
    };
    (format!("{scheme}:{rest}"), Some(scheme))
}

/// The direct subschemas of a schema object — the keyword table the 0.34
/// registry crawled (`referencing` 0.34 `subresources::object_iter`), and
/// nothing else: data positions (`enum`, `const`, `default`,
/// `examples`, unknown keywords) are not schemas and are skipped.
fn subschemas(
    map: &serde_json::Map<String, serde_json::Value>,
) -> impl Iterator<Item = &serde_json::Value> {
    map.iter().flat_map(|(keyword, value)| {
        let children: Box<dyn Iterator<Item = &serde_json::Value>> = match (keyword.as_str(), value)
        {
            (
                "additionalProperties"
                | "contains"
                | "contentSchema"
                | "else"
                | "if"
                | "items"
                | "not"
                | "propertyNames"
                | "then"
                | "unevaluatedItems"
                | "unevaluatedProperties",
                _,
            ) => Box::new(std::iter::once(value)),
            ("allOf" | "anyOf" | "oneOf" | "prefixItems", serde_json::Value::Array(items)) => {
                Box::new(items.iter())
            }
            (
                "$defs" | "definitions" | "dependentSchemas" | "patternProperties" | "properties",
                serde_json::Value::Object(children),
            ) => Box::new(children.values()),
            _ => Box::new(std::iter::empty()),
        };
        children
    })
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

    /// Compile `schema` inline and return the `SchemaCompile` message,
    /// panicking on any other outcome.
    fn compile_error_message(schema: serde_json::Value) -> String {
        let yaml: serde_norway::Value =
            serde_norway::from_str(&schema.to_string()).expect("json is valid yaml");
        match compile_inline("test.bad", &yaml, None) {
            Err(ExecError::SchemaCompile { rule_id, message }) => {
                assert_eq!(rule_id, "test.bad");
                message
            }
            other => panic!("expected SchemaCompile error, got {other:?}"),
        }
    }

    #[test]
    fn compile_inline_rejects_external_refs_with_stable_message() {
        // http / file / relative-file `$ref` are all rejected at compile
        // time by our own pre-walk, with a message that does not depend
        // on the jsonschema crate's wording.
        for uri in [
            "https://json-schema.org/draft/2020-12/schema",
            "file:///etc/passwd",
            "./other.schema.json",
            "other.schema.json#/$defs/x",
        ] {
            let message = compile_error_message(serde_json::json!({
                "type": "object",
                "properties": {"a": {"$ref": uri}},
            }));
            assert_eq!(message, format!("external $ref is not supported: {uri}"));
        }
    }

    #[test]
    fn compile_inline_rejects_unknown_meta_schema_with_0_34_message() {
        for (uri, expected) in [
            (
                "https://example.com/custom-meta",
                "Resource 'https://example.com/custom-meta' is not present in a registry and \
                 retrieving it failed: `resolve-http` feature or a custom resolver is required \
                 to resolve external schemas via HTTP",
            ),
            (
                "file:///tmp/meta.json",
                "Resource 'file:///tmp/meta.json' is not present in a registry and retrieving \
                 it failed: `resolve-file` feature or a custom resolver is required to resolve \
                 external schemas via files",
            ),
            (
                "meta.json",
                "Resource 'meta.json' is not present in a registry and retrieving it failed: \
                 No base URI is available",
            ),
            (
                "urn:x:y",
                "Resource 'urn:x:y' is not present in a registry and retrieving it failed: \
                 Unknown scheme urn",
            ),
        ] {
            let message = compile_error_message(serde_json::json!({
                "properties": {"a": {"$schema": uri, "type": "string"}},
            }));
            assert_eq!(message, expected, "for $schema {uri}");
        }
    }

    #[test]
    fn compile_inline_ignores_meta_schema_lookalikes_in_data_positions() {
        for schema in [
            serde_json::json!({"enum": [{"$schema": "https://example.com/m"}]}),
            serde_json::json!({"const": {"$schema": "https://example.com/m"}}),
            serde_json::json!({"default": {"$schema": "https://example.com/m"}}),
            serde_json::json!({"x-ext": {"$schema": "https://example.com/m"}}),
        ] {
            let yaml: serde_norway::Value =
                serde_norway::from_str(&schema.to_string()).expect("json is valid yaml");
            assert!(
                compile_inline("test.ok", &yaml, None).is_ok(),
                "{schema} should compile"
            );
        }
    }

    #[test]
    fn compile_inline_accepts_bundled_and_self_identified_meta_schemas() {
        for schema in [
            serde_json::json!({"$schema": "https://json-schema.org/draft/2020-12/schema"}),
            serde_json::json!({"$schema": "http://json-schema.org/draft-07/schema#"}),
            serde_json::json!({"$id": "https://example.com/m", "$schema": "https://example.com/m#"}),
        ] {
            let yaml: serde_norway::Value =
                serde_norway::from_str(&schema.to_string()).expect("json is valid yaml");
            assert!(
                compile_inline("test.ok", &yaml, None).is_ok(),
                "{schema} should compile"
            );
        }
    }

    #[test]
    fn compile_inline_accepts_internal_ref() {
        let yaml: serde_norway::Value = serde_norway::from_str(
            r##"
properties:
  code: {$ref: "#/$defs/code"}
$defs:
  code: {enum: [AA, BB]}
"##,
        )
        .expect("yaml parse");
        let compiled = compile_inline("test.rule", &yaml, None).expect("internal $ref compiles");
        assert!(
            compiled
                .validator
                .is_valid(&serde_json::json!({"code": "AA"}))
        );
        assert!(
            !compiled
                .validator
                .is_valid(&serde_json::json!({"code": "CC"}))
        );
    }

    #[test]
    fn compile_inline_rejects_external_dynamic_ref_at_compile_time() {
        // `$dynamicRef` is not covered by the `$ref` pre-walk; the
        // validator itself must still refuse it at compile time instead
        // of deferring to validate-time or reaching the network.
        let message = compile_error_message(serde_json::json!({
            "$dynamicRef": "https://example.com/remote.json",
        }));
        assert!(
            message.contains("https://example.com/remote.json"),
            "message should name the refused URI, got: {message}"
        );
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

    // --- `$schema` emulation of jsonschema 0.34 -----------------------------
    //
    // Expected values below were checked against a dq binary built on
    // jsonschema 0.34, which resolved `$schema` through its own registry.

    /// The 0.34 compile error for a `$schema` it could not retrieve over HTTP.
    fn http_meta_schema_error(uri: &str) -> String {
        format!(
            "Resource '{uri}' is not present in a registry and retrieving it failed: \
             `resolve-http` feature or a custom resolver is required to resolve external \
             schemas via HTTP"
        )
    }

    /// Compile `schema` inline, panicking with the error when it fails.
    fn assert_compiles(schema: &serde_json::Value) {
        let yaml: serde_norway::Value =
            serde_norway::from_str(&schema.to_string()).expect("json is valid yaml");
        if let Err(err) = compile_inline("test.ok", &yaml, None) {
            panic!("{schema} should compile, got {err:?}");
        }
    }

    #[test]
    fn resolve_reference_network_path_takes_only_the_base_scheme() {
        assert_eq!(
            resolve_reference(Some("https://e.com/root"), "//other.org/m"),
            "https://other.org/m"
        );
    }

    #[test]
    fn resolve_reference_absolute_path_keeps_the_base_authority() {
        assert_eq!(
            resolve_reference(Some("https://e.com/dir/root.json"), "/x"),
            "https://e.com/x"
        );
    }

    #[test]
    fn resolve_reference_against_a_base_without_path_starts_at_root() {
        assert_eq!(
            resolve_reference(Some("https://e.com"), "m.json"),
            "https://e.com/m.json"
        );
    }

    #[test]
    fn resolve_reference_empty_reference_is_the_base() {
        assert_eq!(
            resolve_reference(Some("https://e.com/root"), ""),
            "https://e.com/root"
        );
    }

    #[test]
    fn resolve_reference_relative_path_replaces_the_last_base_segment() {
        assert_eq!(
            resolve_reference(Some("https://e.com/a/root.json"), "m.json"),
            "https://e.com/a/m.json"
        );
        // The base's own query and fragment never leak into the result;
        // the reference's query is kept.
        assert_eq!(
            resolve_reference(Some("https://e.com/a/root.json?v=1#top"), "m.json"),
            "https://e.com/a/m.json"
        );
        assert_eq!(
            resolve_reference(Some("https://e.com/a/root.json"), "m.json?q=1"),
            "https://e.com/a/m.json?q=1"
        );
    }

    #[test]
    fn resolve_reference_keeps_the_reference_fragment_for_the_caller_to_strip() {
        let resolved = resolve_reference(Some("https://e.com/a/root.json"), "m.json#frag");
        assert_eq!(resolved, "https://e.com/a/m.json#frag");
        assert_eq!(strip_fragment(&resolved), "https://e.com/a/m.json");
        assert_eq!(strip_fragment("#"), "");
        assert_eq!(strip_fragment("m.json"), "m.json");
    }

    #[test]
    fn resolve_reference_returns_the_reference_when_it_cannot_be_resolved() {
        // No base at all.
        assert_eq!(resolve_reference(None, "m.json"), "m.json");
        // A reference with its own scheme ignores the base.
        assert_eq!(
            resolve_reference(Some("https://e.com/a/root.json"), "urn:x:y"),
            "urn:x:y"
        );
        // A base without a scheme is not a base URI.
        assert_eq!(resolve_reference(Some("dir/root.json"), "m.json"), "m.json");
        // A base without an authority merges into its (slash-less) path.
        assert_eq!(resolve_reference(Some("urn:x:y"), "m.json"), "urn:m.json");
    }

    #[test]
    #[ignore = "bug: resolve_reference keeps `.`/`..` segments (RFC 3986 § 5.2.4 not applied); \
                jsonschema 0.34 removed them"]
    fn resolve_reference_removes_dot_segments() {
        assert_eq!(
            resolve_reference(Some("https://e.com/a/b/root.json"), "../m.json"),
            "https://e.com/a/m.json"
        );
        assert_eq!(
            resolve_reference(Some("https://e.com/a/b/root.json"), "./m.json"),
            "https://e.com/a/b/m.json"
        );
    }

    #[test]
    fn uri_scheme_accepts_only_rfc_3986_schemes() {
        for (uri, expected) in [
            ("https://e.com", Some("https")),
            ("HTTPS://e.com", Some("HTTPS")),
            ("urn:x:y", Some("urn")),
            ("a1+b-c.d:rest", Some("a1+b-c.d")),
            ("meta.json", None),
            ("//host:80/p", None),
            ("1abc:x", None),
            (":x", None),
            ("ht tp:x", None),
            ("#frag:x", None),
            ("", None),
        ] {
            assert_eq!(uri_scheme(uri), expected, "for {uri:?}");
        }
    }

    #[test]
    fn normalize_uri_lowercases_scheme_and_authority_only() {
        for (uri, expected, scheme) in [
            (
                "HTTPS://Example.COM/A/B",
                "https://example.com/A/B",
                Some("https"),
            ),
            ("HTTP://Example.COM", "http://example.com", Some("http")),
            ("FILE:///Tmp/M.json", "file:///Tmp/M.json", Some("file")),
            ("URN:X:Y", "urn:X:Y", Some("urn")),
            ("meta.json", "meta.json", None),
            ("//Host/Path", "//Host/Path", None),
        ] {
            assert_eq!(
                normalize_uri(uri),
                (expected.to_owned(), scheme.map(str::to_owned)),
                "for {uri:?}"
            );
        }
    }

    #[test]
    fn compile_inline_reports_the_normalized_meta_schema_uri() {
        for (uri, expected) in [
            (
                "https://Example.COM/A/B#frag",
                http_meta_schema_error("https://example.com/A/B"),
            ),
            // The bundled-draft prefixes are matched case-sensitively.
            (
                "HTTPS://JSON-SCHEMA.ORG/draft/2020-12/schema",
                http_meta_schema_error("https://json-schema.org/draft/2020-12/schema"),
            ),
            (
                "URN:X:Y",
                "Resource 'urn:X:Y' is not present in a registry and retrieving it failed: \
                 Unknown scheme urn"
                    .to_owned(),
            ),
            (
                "//host/path",
                "Resource '//host/path' is not present in a registry and retrieving it failed: \
                 No base URI is available"
                    .to_owned(),
            ),
        ] {
            let message = compile_error_message(serde_json::json!({"$schema": uri}));
            assert_eq!(message, expected, "for $schema {uri}");
        }
    }

    #[test]
    fn compile_inline_resolves_meta_schema_against_the_enclosing_id() {
        for (schema, resolved) in [
            (
                serde_json::json!({
                    "$id": "https://e.com/root",
                    "properties": {"a": {"$schema": "//other.org/m"}},
                }),
                "https://other.org/m",
            ),
            (
                serde_json::json!({
                    "$id": "https://e.com/dir/root.json",
                    "properties": {"a": {"$schema": "/x"}},
                }),
                "https://e.com/x",
            ),
            (
                serde_json::json!({
                    "$id": "https://e.com",
                    "properties": {"a": {"$schema": "m.json"}},
                }),
                "https://e.com/m.json",
            ),
            (
                serde_json::json!({
                    "$id": "https://e.com/a/root.json",
                    "properties": {"a": {"$schema": "m.json#frag"}},
                }),
                "https://e.com/a/m.json",
            ),
            (
                serde_json::json!({
                    "$id": "https://e.com/a/root.json",
                    "properties": {"a": {"$schema": "m.json?q=1"}},
                }),
                "https://e.com/a/m.json?q=1",
            ),
            // A sibling `$id` on the same object does not rebase an
            // absolute `$schema`.
            (
                serde_json::json!({
                    "properties": {"a": {
                        "$id": "https://e.com/a",
                        "$schema": "https://example.com/m",
                    }},
                }),
                "https://example.com/m",
            ),
        ] {
            assert_eq!(
                compile_error_message(schema.clone()),
                http_meta_schema_error(resolved),
                "for {schema}"
            );
        }
    }

    #[test]
    fn compile_inline_accepts_meta_schema_naming_a_nested_relative_id() {
        // `$defs/m` registers `meta.json` under the root `$id`, i.e.
        // `https://e.com/meta.json`; a `$schema` naming it relatively or
        // absolutely resolves in-document.
        for reference in ["meta.json", "https://e.com/meta.json"] {
            assert_compiles(&serde_json::json!({
                "$id": "https://e.com/root",
                "$defs": {"m": {"$id": "meta.json"}},
                "properties": {"a": {"$schema": reference}},
            }));
        }
    }

    #[test]
    fn compile_inline_rejects_meta_schema_missing_from_the_nested_ids() {
        let message = compile_error_message(serde_json::json!({
            "$id": "https://e.com/root",
            "$defs": {"m": {"$id": "meta.json"}},
            "properties": {"a": {"$schema": "other.json"}},
        }));
        assert_eq!(message, http_meta_schema_error("https://e.com/other.json"));
    }

    #[test]
    #[ignore = "bug: dot segments in a `$schema` are not removed before the `$id` lookup and \
                the error message (jsonschema 0.34 removed them)"]
    fn compile_inline_meta_schema_resolution_removes_dot_segments() {
        assert_compiles(&serde_json::json!({
            "$id": "https://e.com/a/root.json",
            "$defs": {"m": {"$id": "https://e.com/m.json"}},
            "properties": {"a": {"$schema": "../m.json"}},
        }));
        assert_compiles(&serde_json::json!({
            "$id": "https://e.com/a/root.json",
            "$defs": {"m": {"$id": "https://e.com/a/m.json"}},
            "properties": {"a": {"$schema": "./m.json"}},
        }));
        let message = compile_error_message(serde_json::json!({
            "$id": "https://e.com/a/b/root.json",
            "properties": {"a": {"$schema": "../m.json"}},
        }));
        assert_eq!(message, http_meta_schema_error("https://e.com/a/m.json"));
    }

    #[test]
    fn compile_inline_accepts_empty_and_fragment_only_meta_schema() {
        for schema in [
            serde_json::json!({"$schema": ""}),
            serde_json::json!({"$schema": "#"}),
            serde_json::json!({"$id": "https://e.com/root", "properties": {"a": {"$schema": ""}}}),
            serde_json::json!({"$id": "https://e.com/root", "properties": {"a": {"$schema": "#"}}}),
        ] {
            assert_compiles(&schema);
        }
        // And the schema still validates as 2020-12.
        let yaml: serde_norway::Value =
            serde_norway::from_str(r#"{"$schema": "", "type": "integer"}"#).expect("yaml");
        let compiled = compile_inline("test.ok", &yaml, None).expect("compiles");
        assert!(compiled.validator.is_valid(&serde_json::json!(1)));
        assert!(!compiled.validator.is_valid(&serde_json::json!({"a": 1})));
    }

    #[test]
    fn compile_inline_does_not_walk_the_array_form_of_items() {
        // 0.34 did not crawl `items: [..]` for resources, so a `$schema`
        // inside it is never looked up; the 2020-12 validator rejects the
        // array form itself.
        let message = compile_error_message(serde_json::json!({
            "items": [{"$schema": "https://example.com/m"}],
        }));
        assert!(
            !message.contains("not present in a registry"),
            "array-form `items` must not be walked for `$schema`, got: {message}"
        );
        assert!(
            message.contains(r#"is not of types "boolean", "object""#),
            "expected the validator's own `items` type error, got: {message}"
        );
    }

    #[test]
    fn compile_inline_walks_object_items_and_prefix_items() {
        for schema in [
            serde_json::json!({"items": {"$schema": "https://example.com/m"}}),
            serde_json::json!({"prefixItems": [{"$schema": "https://example.com/m"}]}),
        ] {
            assert_eq!(
                compile_error_message(schema.clone()),
                http_meta_schema_error("https://example.com/m"),
                "for {schema}"
            );
        }
    }

    #[test]
    fn compile_inline_ignores_meta_schema_lookalikes_in_nested_data_positions() {
        for schema in [
            serde_json::json!({"examples": [{"$schema": "https://example.com/m"}]}),
            serde_json::json!({
                "properties": {"p": {"default": {"$schema": "https://example.com/m"}}},
            }),
            serde_json::json!({
                "properties": {"p": {"examples": [{"$schema": "https://example.com/m"}]}},
            }),
            // A property *named* `$schema` is a schema, not a `$schema` keyword.
            serde_json::json!({"properties": {"$schema": {"type": "string"}}}),
        ] {
            assert_compiles(&schema);
        }
    }

    #[test]
    fn compile_inline_walks_subschemas_named_like_data_keywords() {
        // Under `$defs` / `properties` the keys are names, so `enum` and
        // `const` there hold schemas and their `$schema` is looked up.
        for schema in [
            serde_json::json!({"$defs": {"enum": {"$schema": "https://example.com/m"}}}),
            serde_json::json!({"properties": {"enum": {"$schema": "https://example.com/m"}}}),
            serde_json::json!({"properties": {"const": {"$schema": "https://example.com/m"}}}),
        ] {
            assert_eq!(
                compile_error_message(schema.clone()),
                http_meta_schema_error("https://example.com/m"),
                "for {schema}"
            );
        }
    }

    #[test]
    #[ignore = "bug: find_external_ref walks data positions, so a `$ref` key inside \
                `const`/`enum`/`default`/`examples` is rejected as an external $ref \
                (pre-existing, same in the jsonschema 0.34 build)"]
    fn compile_inline_ignores_ref_lookalikes_in_data_positions() {
        for schema in [
            serde_json::json!({"enum": [{"$ref": "https://x.com/y"}]}),
            serde_json::json!({"const": {"$ref": "https://x.com/y"}}),
            serde_json::json!({"default": {"$ref": "https://x.com/y"}}),
            serde_json::json!({"examples": [{"$ref": "https://x.com/y"}]}),
        ] {
            assert_compiles(&schema);
        }
    }
}
