//! Pre-compiled rule runner.
//!
//! [`Evaluator::new`] takes one or more [`crate::ruleset::RuleSet`]s and
//! compiles every rule's `match.filter`, `check.jq`, `loc.pointer`,
//! `loc.file`, and `loc.line` jq expressions, plus its `match.glob`
//! pattern, up-front. [`Evaluator::evaluate_file`] runs the compiled
//! rules against a pre-parsed document IR and emits one [`Diagnostic`]
//! per violation.
//!
//! ## Per-rule pipeline
//!
//! For each rule, in declaration order:
//!
//! 1. Format match: `format_name` must appear in `match.format`.
//! 2. Glob match: when `match.glob` is set, the file path must match.
//! 3. Filter match: when `match.filter` is set, run it and require a
//!    truthy first output (anything non-null / non-`false`).
//! 4. Check eval: run `check.jq`; each emitted value is one violation.
//! 5. Diagnostic build: render `check.message`, resolve `loc.pointer` /
//!    `loc.file` / `loc.line` overrides, attach severity / references /
//!    fix payload.
//!
//! ## Position metadata
//!
//! Phase 2 of `add-ir-foundation` switched the evaluator to take a
//! borrowed [`dq_core::Ir<'_>`] instead of a `serde_json::Value`. The
//! IR carries the parser's `original_bytes` and a provenance map keyed
//! by canonical RFC 6901 pointer strings. Rules now have a typed
//! `loc.pointer` expression: when it produces a non-empty pointer
//! string, the evaluator looks the pointer up via
//! [`dq_core::Ir::line_col_for`] and resolves `(line, col)` from the
//! source bytes. The legacy `loc.line` path stays as a fallback so M8
//! rules keep working unchanged.
//!
//! ## Robustness
//!
//! Compile-time failures (jq parse errors, glob parse errors) surface as
//! [`crate::error::ExecError`] from [`Evaluator::new`]. Runtime failures
//! during [`Evaluator::evaluate_file`] (a `check.jq` that crashes on a
//! particular input shape, a `loc.line` expression that returns a
//! non-integer, a `loc.pointer` expression that fails to parse) are
//! logged via `tracing::warn!` / `tracing::trace!` and the offending
//! rule falls through to the next step in the chain — the principle is
//! that one badly-written rule must not crash the entire run.

use std::sync::Arc;

use camino::{Utf8Path, Utf8PathBuf};
use dq_core::Pointer;
use globset::GlobMatcher;

use crate::diagnostic::Diagnostic;
use crate::error::{ExecError, Result};
use crate::rule::Rule;
use crate::ruleset::RuleSet;
use crate::template;
use dq_transform::JqEngine;

/// Pre-compiled rule runner — see the module-level docs for the pipeline.
///
/// `Evaluator` is `Send + Sync + Clone`. The clone is cheap: each
/// compiled rule is wrapped in an [`Arc`], so cloning the evaluator only
/// bumps a refcount per rule.
#[derive(Debug, Clone)]
pub struct Evaluator {
    rules: Vec<Arc<CompiledRule>>,
}

/// One rule with all its jq filters and glob matcher pre-compiled.
///
/// Stored behind [`Arc`] in the [`Evaluator`] so cloning the evaluator
/// (e.g. one clone per rayon worker in a future parallel evaluation
/// path) is cheap.
pub(crate) struct CompiledRule {
    pub(crate) rule: Rule,
    pub(crate) filter_engine: Option<JqEngine>,
    pub(crate) check_engine: JqEngine,
    pub(crate) glob_matcher: Option<GlobMatcher>,
    pub(crate) loc_file_engine: Option<JqEngine>,
    pub(crate) loc_line_engine: Option<JqEngine>,
    /// Phase 2 of `add-ir-foundation`: pre-compiled `loc.pointer` jq
    /// expression. Populated when the rule's `loc:` block declares a
    /// `pointer:` field. Consumed by [`resolve_loc_position`] which
    /// walks the new `loc.pointer → loc.line → intrinsic` chain.
    pub(crate) loc_pointer_engine: Option<JqEngine>,
    /// M10 — pre-compiled `fix.jq` engine. Populated when the rule
    /// declares a `fix:` block; consumed by [`crate::Fixer`].
    pub(crate) fix_engine: Option<JqEngine>,
}

impl std::fmt::Debug for CompiledRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledRule")
            .field("rule_id", &self.rule.id)
            .field("has_filter", &self.filter_engine.is_some())
            .field("has_glob", &self.glob_matcher.is_some())
            .field("has_loc_pointer", &self.loc_pointer_engine.is_some())
            .field("has_loc_file", &self.loc_file_engine.is_some())
            .field("has_loc_line", &self.loc_line_engine.is_some())
            .field("has_fix", &self.fix_engine.is_some())
            .finish()
    }
}

impl Evaluator {
    /// Compile every rule across the given rulesets up-front.
    ///
    /// On any compile failure (jq compile error in `match.filter`,
    /// `check.jq`, `loc.file`, or `loc.line`; or glob parse error in
    /// `match.glob`), returns the corresponding [`ExecError`] tagged with
    /// the offending rule id so the CLI can point the user at the rule.
    ///
    /// # Errors
    ///
    /// - [`ExecError::RuleCompile`] when a jq expression fails to compile.
    /// - [`ExecError::GlobCompile`] when a `match.glob` pattern fails to
    ///   compile.
    pub fn new(rulesets: Vec<RuleSet>) -> Result<Self> {
        let mut compiled = Vec::new();
        for set in rulesets {
            for rule in set.rules {
                compiled.push(Arc::new(compile_rule(rule)?));
            }
        }
        Ok(Self { rules: compiled })
    }

    /// Run every applicable rule against `ir` and collect the resulting
    /// diagnostics.
    ///
    /// `format_name` is the canonical format name of the parsed document
    /// (e.g. `"yaml"`, `"json"`). Only rules whose `match.format` list
    /// includes that name are considered.
    ///
    /// Phase 2 of `add-ir-foundation` switched the input from
    /// `&serde_json::Value` to a borrowed [`dq_core::Ir<'_>`] so the
    /// evaluator can resolve `loc.pointer` against the input's provenance
    /// map / source bytes via [`dq_core::Ir::line_col_for`]. Internally
    /// the evaluator still feeds jq through `serde_json::Value` (jaq's
    /// native shape), but the borrowed `Ir` carries the metadata needed
    /// for span-aware diagnostic positions without forcing every caller
    /// onto a span-preserving jaq fork.
    #[must_use]
    pub fn evaluate_file(
        &self,
        path: &Utf8Path,
        ir: &dq_core::Ir<'_>,
        format_name: &str,
    ) -> Vec<Diagnostic> {
        // Convert once at the boundary: jaq still consumes
        // `serde_json::Value` (the native shape), so we materialise it
        // here and pass it to every per-rule jq engine. The `Ir` itself
        // is forwarded into per-rule helpers so they retain access to
        // [`dq_core::Ir::line_col_for`] for `loc.pointer` resolution.
        let value = ir.value().to_serde_json();
        let mut diagnostics = Vec::new();
        for rule in &self.rules {
            evaluate_one_rule(rule, path, ir, &value, format_name, &mut diagnostics);
        }
        diagnostics
    }

    /// Iterate over the rules compiled into this evaluator.
    ///
    /// Order matches the declaration order in the input rulesets — useful
    /// for `dq rules list` and `dq explain` callers that want a stable
    /// listing.
    pub fn rules(&self) -> impl Iterator<Item = &Rule> + '_ {
        self.rules.iter().map(|r| &r.rule)
    }

    /// Number of compiled rules — convenience for tests and reporters
    /// that want a count without iterating.
    #[must_use]
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// Slice of pre-compiled rules.
    ///
    /// Crate-internal accessor used by [`crate::Fixer`] so the autofix
    /// driver can re-run `match.format` / `match.glob` / `match.filter`
    /// gates and the `check.jq` "violation present?" check before
    /// applying `fix.jq`. Public iteration over `Rule` is exposed via
    /// [`Evaluator::rules`].
    #[must_use]
    pub(crate) fn compiled_rules(&self) -> &[Arc<CompiledRule>] {
        &self.rules
    }
}

/// Compile one [`Rule`] into a [`CompiledRule`] — runs jq compile and
/// glob compile up front.
fn compile_rule(rule: Rule) -> Result<CompiledRule> {
    let filter_engine = match rule.match_.filter.as_deref() {
        Some(expr) => Some(
            JqEngine::compile(expr).map_err(|err| ExecError::RuleCompile {
                rule_id: rule.id.clone(),
                source: err,
            })?,
        ),
        None => None,
    };
    let check_engine = JqEngine::compile(&rule.check.jq).map_err(|err| ExecError::RuleCompile {
        rule_id: rule.id.clone(),
        source: err,
    })?;
    let glob_matcher = match rule.match_.glob.as_deref() {
        Some(pattern) => {
            let glob = globset::Glob::new(pattern).map_err(|err| ExecError::GlobCompile {
                rule_id: rule.id.clone(),
                source: err,
            })?;
            Some(glob.compile_matcher())
        }
        None => None,
    };
    let (loc_pointer_engine, loc_file_engine, loc_line_engine) = match rule.loc.as_ref() {
        Some(loc) => {
            let pointer = match loc.pointer.as_deref() {
                Some(expr) => {
                    Some(
                        JqEngine::compile(expr).map_err(|err| ExecError::RuleCompile {
                            rule_id: rule.id.clone(),
                            source: err,
                        })?,
                    )
                }
                None => None,
            };
            let file = match loc.file.as_deref() {
                Some(expr) => {
                    Some(
                        JqEngine::compile(expr).map_err(|err| ExecError::RuleCompile {
                            rule_id: rule.id.clone(),
                            source: err,
                        })?,
                    )
                }
                None => None,
            };
            let line = match loc.line.as_deref() {
                Some(expr) => {
                    Some(
                        JqEngine::compile(expr).map_err(|err| ExecError::RuleCompile {
                            rule_id: rule.id.clone(),
                            source: err,
                        })?,
                    )
                }
                None => None,
            };
            (pointer, file, line)
        }
        None => (None, None, None),
    };
    // M10: compile `fix.jq` alongside the other engines so per-file
    // autofix runs don't pay re-compilation cost. Compile-time failures
    // surface here as the same `RuleCompile` shape the lint runtime uses.
    let fix_engine = match rule.fix.as_ref() {
        Some(fix) => Some(
            JqEngine::compile(&fix.jq).map_err(|err| ExecError::RuleCompile {
                rule_id: rule.id.clone(),
                source: err,
            })?,
        ),
        None => None,
    };

    Ok(CompiledRule {
        rule,
        filter_engine,
        check_engine,
        glob_matcher,
        loc_pointer_engine,
        loc_file_engine,
        loc_line_engine,
        fix_engine,
    })
}

/// Run the per-rule pipeline against `(path, ir, value, format_name)` and
/// push any resulting diagnostics into `out`.
///
/// `value` is the result of `ir.value().to_serde_json()` — pre-computed
/// once per file by the caller so each rule's jq engines can reuse it
/// without paying for the conversion N times.
fn evaluate_one_rule(
    rule: &CompiledRule,
    path: &Utf8Path,
    ir: &dq_core::Ir<'_>,
    value: &serde_json::Value,
    format_name: &str,
    out: &mut Vec<Diagnostic>,
) {
    // 1. Format match.
    if !rule.rule.match_.format.iter().any(|f| f == format_name) {
        return;
    }

    // 2. Glob match.
    if let Some(matcher) = rule.glob_matcher.as_ref()
        && !matcher.is_match(path.as_str())
    {
        return;
    }

    // 3. Filter match.
    if let Some(filter) = rule.filter_engine.as_ref() {
        match filter.run(value) {
            Ok(out) => {
                let Some(first) = out.first() else {
                    return;
                };
                if matches!(
                    first,
                    serde_json::Value::Bool(false) | serde_json::Value::Null
                ) {
                    return;
                }
            }
            Err(err) => {
                tracing::warn!(
                    rule_id = %rule.rule.id,
                    error = %err,
                    "match.filter raised a runtime error; skipping rule for this file",
                );
                return;
            }
        }
    }

    // 4. Check eval.
    let violations = match rule.check_engine.run(value) {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(
                rule_id = %rule.rule.id,
                error = %err,
                "check.jq raised a runtime error; skipping rule for this file",
            );
            return;
        }
    };

    // 5. Build a diagnostic per violation.
    for violation in &violations {
        out.push(build_diagnostic(rule, path, ir, violation));
    }
}

/// Render a diagnostic for one violation value.
fn build_diagnostic(
    rule: &CompiledRule,
    path: &Utf8Path,
    ir: &dq_core::Ir<'_>,
    violation: &serde_json::Value,
) -> Diagnostic {
    let message = template::render(&rule.rule.check.message, violation);
    let file = resolve_loc_file(rule, path, violation);
    let (line, col) = resolve_loc_position(rule, ir, violation);
    Diagnostic {
        rule_id: rule.rule.id.clone(),
        severity: rule.rule.severity,
        message,
        file,
        line,
        col,
        span: None,
        references: rule.rule.references.clone(),
        fix: rule.rule.fix.clone(),
    }
}

/// Resolve the `file` field for the diagnostic. When `loc.file` is set
/// and produces a non-empty string, use that; otherwise default to the
/// file under check.
fn resolve_loc_file(
    rule: &CompiledRule,
    path: &Utf8Path,
    violation: &serde_json::Value,
) -> Option<Utf8PathBuf> {
    if let Some(engine) = rule.loc_file_engine.as_ref() {
        match engine.run(violation) {
            Ok(out) => {
                if let Some(serde_json::Value::String(s)) = out.first()
                    && !s.is_empty()
                {
                    return Some(Utf8PathBuf::from(s));
                }
            }
            Err(err) => {
                tracing::warn!(
                    rule_id = %rule.rule.id,
                    error = %err,
                    "loc.file raised a runtime error; falling back to file under check",
                );
            }
        }
    }
    Some(path.to_path_buf())
}

/// Resolve the diagnostic's `(line, col)` per the new Phase 2 chain:
/// `loc.pointer` → `loc.line` → intrinsic.
///
/// 1. **`loc.pointer`** (preferred): evaluate the jq expression. If the
///    first output is a non-empty string AND parses as a [`Pointer`] AND
///    [`dq_core::Ir::line_col_for`] resolves to `Some((line, col))`,
///    return it. Any failure (jq runtime error, non-string output, empty
///    string, parse failure, span miss, no source bytes on the IR) falls
///    through to step 2.
/// 2. **`loc.line`** (deprecated, M8 fallback): evaluate the jq
///    expression and coerce the first output to a positive `u32`. On
///    success, return `(line, 1)`. On any failure or out-of-range
///    integer, fall through to step 3. `col` is hard-coded to `1` in the
///    legacy path because `loc.line` was only ever a line override —
///    matching the M8 semantics byte-for-byte.
/// 3. **Default**: `(1, 1)`.
///
/// Each step in the chain emits a `tracing::trace!` describing which
/// branch won — useful for rule authors debugging why their `loc.pointer`
/// did not resolve. Failures inside `loc.pointer` are deliberately *not*
/// `warn!`-level: a missing span is a normal fall-through case (the rule
/// emits a synthesised pointer like `/missing` for a violation that was
/// not present in the source).
fn resolve_loc_position(
    rule: &CompiledRule,
    ir: &dq_core::Ir<'_>,
    violation: &serde_json::Value,
) -> (u32, u32) {
    // Step 1: loc.pointer.
    if let Some(engine) = rule.loc_pointer_engine.as_ref() {
        match engine.run(violation) {
            Ok(out) => {
                if let Some(first) = out.first() {
                    if let serde_json::Value::String(s) = first
                        && !s.is_empty()
                    {
                        match Pointer::parse(s) {
                            Ok(pointer) => match ir.line_col_for(&pointer) {
                                Some((line, col)) => {
                                    tracing::trace!(
                                        rule_id = %rule.rule.id,
                                        pointer = %s,
                                        line,
                                        col,
                                        "loc.pointer resolved span via Ir::line_col_for",
                                    );
                                    return (line, col);
                                }
                                None => {
                                    tracing::trace!(
                                        rule_id = %rule.rule.id,
                                        pointer = %s,
                                        "loc.pointer parsed but no span on Ir; falling through",
                                    );
                                }
                            },
                            Err(err) => {
                                tracing::trace!(
                                    rule_id = %rule.rule.id,
                                    pointer = %s,
                                    error = %err,
                                    "loc.pointer output failed to parse; falling through",
                                );
                            }
                        }
                    } else {
                        tracing::trace!(
                            rule_id = %rule.rule.id,
                            "loc.pointer first output was not a non-empty string; falling through",
                        );
                    }
                } else {
                    tracing::trace!(
                        rule_id = %rule.rule.id,
                        "loc.pointer produced empty output stream; falling through",
                    );
                }
            }
            Err(err) => {
                // Runtime errors here are normal fall-through territory
                // (the violation may not have the field the expression
                // expects). Logged at `trace!` rather than `warn!` to
                // avoid noise; the legacy `loc.line` warning policy is
                // preserved below for backward compatibility.
                tracing::trace!(
                    rule_id = %rule.rule.id,
                    error = %err,
                    "loc.pointer raised a runtime error; falling through",
                );
            }
        }
    }

    // Step 2: loc.line (M8 legacy path).
    if let Some(engine) = rule.loc_line_engine.as_ref() {
        match engine.run(violation) {
            Ok(out) => {
                if let Some(first) = out.first() {
                    if let Some(n) = first.as_u64()
                        && n >= 1
                        && n <= u64::from(u32::MAX)
                    {
                        let line = u32::try_from(n).unwrap_or(1);
                        tracing::trace!(
                            rule_id = %rule.rule.id,
                            line,
                            "loc.line legacy path resolved",
                        );
                        return (line, 1);
                    } else if let Some(n) = first.as_i64()
                        && n >= 1
                        && n <= i64::from(u32::MAX)
                    {
                        let line = u32::try_from(n).unwrap_or(1);
                        tracing::trace!(
                            rule_id = %rule.rule.id,
                            line,
                            "loc.line legacy path resolved",
                        );
                        return (line, 1);
                    }
                }
            }
            Err(err) => {
                tracing::warn!(
                    rule_id = %rule.rule.id,
                    error = %err,
                    "loc.line raised a runtime error; defaulting to line 1",
                );
            }
        }
    }

    // Step 3: default.
    (1, 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::Severity;
    use crate::ruleset::RuleSource;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    /// Build a minimal ruleset by parsing one or more YAML rule documents.
    fn ruleset_from_yaml(yaml: &str) -> RuleSet {
        RuleSet::from_str(yaml, RuleSource::Inline).expect("parse ruleset for test")
    }

    fn evaluator_from_yaml(yaml: &str) -> Evaluator {
        Evaluator::new(vec![ruleset_from_yaml(yaml)]).expect("build evaluator")
    }

    /// Build an [`OwnedIr`] for the test path so existing call sites can
    /// keep constructing a `serde_json::Value` and feed the evaluator
    /// through the new Phase 2 IR signature without rewriting every
    /// fixture. The returned `OwnedIr` carries an empty
    /// [`dq_core::ProvenanceMap`] (no spans available — these tests
    /// pre-date span propagation, so `loc.pointer` resolution will
    /// correctly fall through to the legacy `loc.line` / default path).
    fn ir_for_test(value: &serde_json::Value, format: &str) -> dq_core::OwnedIr {
        use dq_core::FormatTag;
        let dq_value = dq_core::Value::from_serde_json(value);
        let format_tag = match format {
            "yaml" => FormatTag::Yaml,
            "json" => FormatTag::Json,
            _ => FormatTag::Json,
        };
        dq_core::OwnedIr::new(dq_value, dq_core::ProvenanceMap::new(), format_tag)
    }

    const RULE_NAME_NOT_EMPTY: &str = r#"
id: test.name-not-empty
description: name must not be empty
severity: error
match:
  format: yaml
check:
  jq: 'select(.name == "") | .'
  message: 'name is empty'
"#;

    #[test]
    fn evaluator_matches_format_and_emits_one_diagnostic() {
        let eval = evaluator_from_yaml(RULE_NAME_NOT_EMPTY);
        let path = Utf8PathBuf::from("doc.yaml");
        let value = json!({"name": ""});
        let owned = ir_for_test(&value, "yaml");
        let diags = eval.evaluate_file(&path, &owned.to_borrowed(), "yaml");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].rule_id, "test.name-not-empty");
        assert_eq!(diags[0].severity, Severity::Error);
        assert_eq!(diags[0].message, "name is empty");
        assert_eq!(diags[0].file.as_ref().unwrap(), &path);
        assert_eq!(diags[0].line, 1);
        assert_eq!(diags[0].col, 1);
    }

    #[test]
    fn evaluator_skips_rules_with_non_matching_format() {
        let eval = evaluator_from_yaml(RULE_NAME_NOT_EMPTY);
        let value = json!({"name": ""});
        let owned = ir_for_test(&value, "json");
        let diags =
            eval.evaluate_file(&Utf8PathBuf::from("doc.json"), &owned.to_borrowed(), "json");
        assert!(diags.is_empty(), "expected no diagnostics, got: {diags:?}");
    }

    #[test]
    fn evaluator_template_substitutes_violation_field() {
        let yaml = r#"
id: test.template
description: x
severity: warn
match:
  format: yaml
check:
  jq: '.containers[]'
  message: "container '{{ .name }}' uses image {{ .image }}"
"#;
        let eval = evaluator_from_yaml(yaml);
        let value = json!({"containers": [{"name": "web", "image": "nginx:latest"}]});
        let owned = ir_for_test(&value, "yaml");
        let diags = eval.evaluate_file(&Utf8PathBuf::from("d.yaml"), &owned.to_borrowed(), "yaml");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].message, "container 'web' uses image nginx:latest");
    }

    #[test]
    fn filter_returning_false_skips_check() {
        let yaml = r#"
id: test.filter-false
description: x
severity: error
match:
  format: yaml
  filter: 'false'
check:
  jq: '.'
  message: 'should not fire'
"#;
        let eval = evaluator_from_yaml(yaml);
        let value = json!({"a": 1});
        let owned = ir_for_test(&value, "yaml");
        let diags = eval.evaluate_file(&Utf8PathBuf::from("d.yaml"), &owned.to_borrowed(), "yaml");
        assert!(diags.is_empty());
    }

    #[test]
    fn filter_returning_null_skips_check() {
        let yaml = r#"
id: test.filter-null
description: x
severity: error
match:
  format: yaml
  filter: 'null'
check:
  jq: '.'
  message: 'should not fire'
"#;
        let eval = evaluator_from_yaml(yaml);
        let value = json!({"a": 1});
        let owned = ir_for_test(&value, "yaml");
        let diags = eval.evaluate_file(&Utf8PathBuf::from("d.yaml"), &owned.to_borrowed(), "yaml");
        assert!(diags.is_empty());
    }

    #[test]
    fn filter_returning_true_runs_check() {
        let yaml = r#"
id: test.filter-true
description: x
severity: error
match:
  format: yaml
  filter: 'true'
check:
  jq: '.'
  message: 'fires'
"#;
        let eval = evaluator_from_yaml(yaml);
        let value = json!({"a": 1});
        let owned = ir_for_test(&value, "yaml");
        let diags = eval.evaluate_file(&Utf8PathBuf::from("d.yaml"), &owned.to_borrowed(), "yaml");
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn filter_returning_string_runs_check() {
        // Any non-null, non-false output is truthy (matches D5 contract).
        let yaml = r#"
id: test.filter-string
description: x
severity: error
match:
  format: yaml
  filter: '"yes"'
check:
  jq: '.'
  message: 'fires'
"#;
        let eval = evaluator_from_yaml(yaml);
        let value = json!({"a": 1});
        let owned = ir_for_test(&value, "yaml");
        let diags = eval.evaluate_file(&Utf8PathBuf::from("d.yaml"), &owned.to_borrowed(), "yaml");
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn glob_match_filters_by_path() {
        let yaml = r#"
id: test.glob
description: x
severity: error
match:
  format: yaml
  glob: '**/foo.yaml'
check:
  jq: '.'
  message: 'fires'
"#;
        let eval = evaluator_from_yaml(yaml);
        let value = json!({});
        let owned = ir_for_test(&value, "yaml");
        let matched = eval.evaluate_file(
            &Utf8PathBuf::from("dir/foo.yaml"),
            &owned.to_borrowed(),
            "yaml",
        );
        assert_eq!(matched.len(), 1, "expected glob match for dir/foo.yaml");

        let skipped = eval.evaluate_file(
            &Utf8PathBuf::from("dir/bar.yaml"),
            &owned.to_borrowed(),
            "yaml",
        );
        assert!(skipped.is_empty(), "expected glob to skip dir/bar.yaml");
    }

    #[test]
    fn check_emitting_multiple_violations_produces_multiple_diagnostics() {
        let yaml = r#"
id: test.multi
description: x
severity: warn
match:
  format: yaml
check:
  jq: '.containers[]'
  message: 'name={{ .name }}'
"#;
        let eval = evaluator_from_yaml(yaml);
        let value = json!({"containers": [{"name": "a"}, {"name": "b"}, {"name": "c"}]});
        let owned = ir_for_test(&value, "yaml");
        let diags = eval.evaluate_file(&Utf8PathBuf::from("d.yaml"), &owned.to_borrowed(), "yaml");
        assert_eq!(diags.len(), 3);
        let messages: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(messages, vec!["name=a", "name=b", "name=c"]);
    }

    #[test]
    fn loc_file_jq_override_replaces_path() {
        let yaml = r#"
id: test.loc-file
description: x
severity: error
match:
  format: yaml
check:
  jq: '.'
  message: 'msg'
loc:
  file: '"override.yaml"'
"#;
        let eval = evaluator_from_yaml(yaml);
        let value = json!({});
        let owned = ir_for_test(&value, "yaml");
        let diags = eval.evaluate_file(
            &Utf8PathBuf::from("real.yaml"),
            &owned.to_borrowed(),
            "yaml",
        );
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].file.as_ref().unwrap(),
            &Utf8PathBuf::from("override.yaml")
        );
    }

    #[test]
    fn loc_line_jq_override_sets_line_number() {
        let yaml = r#"
id: test.loc-line
description: x
severity: error
match:
  format: yaml
check:
  jq: '.'
  message: 'msg'
loc:
  line: '42'
"#;
        let eval = evaluator_from_yaml(yaml);
        let value = json!({});
        let owned = ir_for_test(&value, "yaml");
        let diags = eval.evaluate_file(
            &Utf8PathBuf::from("real.yaml"),
            &owned.to_borrowed(),
            "yaml",
        );
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].line, 42);
    }

    #[test]
    fn unknown_jq_function_produces_rule_compile_error() {
        let yaml = r#"
id: test.bad-jq
description: x
severity: error
match:
  format: yaml
check:
  jq: 'no_such_function'
  message: 'msg'
"#;
        let err = Evaluator::new(vec![ruleset_from_yaml(yaml)]).expect_err("expected RuleCompile");
        match err {
            ExecError::RuleCompile { rule_id, .. } => {
                assert_eq!(rule_id, "test.bad-jq");
            }
            other => panic!("expected RuleCompile, got {other:?}"),
        }
    }

    #[test]
    fn invalid_glob_produces_glob_compile_error() {
        let yaml = r#"
id: test.bad-glob
description: x
severity: error
match:
  format: yaml
  glob: '[unbalanced'
check:
  jq: '.'
  message: 'msg'
"#;
        let err = Evaluator::new(vec![ruleset_from_yaml(yaml)]).expect_err("expected GlobCompile");
        match err {
            ExecError::GlobCompile { rule_id, .. } => {
                assert_eq!(rule_id, "test.bad-glob");
            }
            other => panic!("expected GlobCompile, got {other:?}"),
        }
    }

    #[test]
    fn check_runtime_error_is_logged_and_skipped_not_panic() {
        // `.foo + 1` against a string crashes at runtime — the evaluator
        // must not panic; instead it logs a warning and skips the rule
        // for this file.
        let yaml = r#"
id: test.runtime-fail
description: x
severity: error
match:
  format: yaml
check:
  jq: '. + 1'
  message: 'msg'
"#;
        let eval = evaluator_from_yaml(yaml);
        let value = json!("a-string");
        let owned = ir_for_test(&value, "yaml");
        let diags = eval.evaluate_file(&Utf8PathBuf::from("d.yaml"), &owned.to_borrowed(), "yaml");
        // No diagnostics emitted — but no panic either.
        assert!(diags.is_empty());
    }

    #[test]
    fn rules_iterator_yields_rules_in_declaration_order() {
        let yaml = r#"
id: alpha.one
description: x
severity: warn
match:
  format: yaml
check:
  jq: '.'
  message: 'm'
---
id: beta.two
description: x
severity: warn
match:
  format: yaml
check:
  jq: '.'
  message: 'm'
"#;
        let eval = evaluator_from_yaml(yaml);
        let ids: Vec<&str> = eval.rules().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["alpha.one", "beta.two"]);
        assert_eq!(eval.rule_count(), 2);
    }

    #[test]
    fn assert_evaluator_send_sync_clone() {
        fn require_send_sync_clone<T: Send + Sync + Clone>(_: &T) {}
        let eval = evaluator_from_yaml(RULE_NAME_NOT_EMPTY);
        require_send_sync_clone(&eval);
        // Cloning bumps the per-rule Arc refcount; both clones still run.
        let cloned = eval.clone();
        let value = json!({"name": ""});
        let owned = ir_for_test(&value, "yaml");
        let diags =
            cloned.evaluate_file(&Utf8PathBuf::from("d.yaml"), &owned.to_borrowed(), "yaml");
        assert_eq!(diags.len(), 1);
    }

    // -- Phase 2: `loc.pointer` chain tests -------------------------------
    //
    // These pin the four spec scenarios under "Location override via `loc:`"
    // (the `loc.file` scenario is already covered by
    // `loc_file_jq_override_replaces_path` above):
    //
    //   1. `loc.pointer` resolves to span line/col.
    //   2. `loc.pointer` falls through to `loc.line` when span is missing.
    //   3. `loc.line`-only override (legacy path) still works after Phase 2.
    //   4. `loc:` absent → default (1, 1).
    //
    // The first test reaches into `dq_core` and constructs a provenance map
    // by hand so the test owns the byte offset / span / pointer mapping
    // without depending on a real parser (the `tests/` integration test
    // pins the same chain against a real YAML parse).

    /// Build a 1-indexed `(line, col)` → byte-offset lookup against `bytes`.
    /// Mirrors the Phase 2 evaluator helper byte-for-byte; used to point a
    /// constructed `ValueSpan` at a known position.
    fn byte_offset(bytes: &[u8], line: u32, col: u32) -> usize {
        let mut cur_line: u32 = 1;
        let mut cur_col: u32 = 1;
        for (i, &b) in bytes.iter().enumerate() {
            if cur_line == line && cur_col == col {
                return i;
            }
            if b == b'\n' {
                cur_line = cur_line.saturating_add(1);
                cur_col = 1;
            } else {
                cur_col = cur_col.saturating_add(1);
            }
        }
        bytes.len()
    }

    /// Construct synthetic source bytes with `name: web` placed at the
    /// requested 1-indexed (line, col). Returned tuple is
    /// `(bytes, span_start)` — span_start is the byte offset corresponding
    /// to the requested `(line, col)`. Tests then assert the diagnostic's
    /// resolved position equals the requested `(line, col)`.
    fn synthesize_bytes_with_span_at(line: u32, col: u32) -> (Vec<u8>, usize) {
        // Pad with N-1 newlines, then col-1 spaces, then the value `web`.
        let line_padding = (line.saturating_sub(1)) as usize;
        let col_padding = (col.saturating_sub(1)) as usize;
        let mut bytes: Vec<u8> = Vec::with_capacity(line_padding + col_padding + 4);
        bytes.extend(std::iter::repeat_n(b'\n', line_padding));
        bytes.extend(std::iter::repeat_n(b' ', col_padding));
        bytes.extend_from_slice(b"web\n");
        let start = byte_offset(&bytes, line, col);
        (bytes, start)
    }

    #[test]
    fn loc_pointer_resolves_to_span_line_and_col() {
        // Spec scenario 1: `loc.pointer` resolves to span line.
        // Build an `Ir<'_>` whose provenance map carries an `Original` entry
        // for `/spec/containers/0` with a span pointing at byte offset
        // corresponding to (line 12, col 5). Then run a rule whose
        // `loc.pointer` jq emits `"/spec/containers/0"` and assert the
        // diagnostic resolved to (12, 5).
        use dq_core::document::spans::{SpanContext, ValueSpan};
        use dq_core::{FormatTag, Pointer, Provenance, ProvenanceMap, Value};

        let yaml = r#"
id: test.loc-pointer-span
description: x
severity: error
match:
  format: yaml
check:
  jq: '{"idx": 0}'
  message: 'msg'
loc:
  pointer: '"/spec/containers/" + (.idx|tostring)'
"#;
        let eval = evaluator_from_yaml(yaml);

        let (bytes, start) = synthesize_bytes_with_span_at(12, 5);
        let pointer = Pointer::parse("/spec/containers/0").expect("pointer parses");
        let mut provenance = ProvenanceMap::new();
        provenance.insert(
            pointer.as_canonical(),
            Provenance::Original {
                pointer: pointer.clone(),
                span: Some(ValueSpan {
                    value_range: start..(start + 3),
                    line_range: 0..0,
                    indent: 0,
                    context: SpanContext::BlockMapValue,
                }),
            },
        );
        // The actual `Value` shape is irrelevant — `loc.pointer` is the
        // jq-emitted string `/spec/containers/0`, looked up against the
        // provenance map. The check.jq emits a synthetic violation
        // independent of the document.
        let value = Value::Map(indexmap::IndexMap::new());
        let ir = dq_core::Ir::with_bytes(&value, &provenance, FormatTag::Yaml, &bytes);

        let diags = eval.evaluate_file(&Utf8PathBuf::from("d.yaml"), &ir, "yaml");
        assert_eq!(diags.len(), 1, "expected one diagnostic, got: {diags:?}");
        assert_eq!(
            diags[0].line, 12,
            "loc.pointer must resolve span line via Ir::line_col_for"
        );
        assert_eq!(
            diags[0].col, 5,
            "loc.pointer must resolve span col via Ir::line_col_for"
        );
    }

    #[test]
    fn loc_pointer_falls_through_to_loc_line_when_span_missing() {
        // Spec scenario 2: `loc.pointer` falls through to `loc.line` when
        // the span is missing. The IR has NO entry for `/missing`, so the
        // pointer resolution returns `None` and the chain falls through
        // to `loc.line`, which evaluates `.line` against `{"line": 7}`.
        let yaml = r#"
id: test.loc-pointer-fallthrough
description: x
severity: error
match:
  format: yaml
check:
  jq: '{"line": 7}'
  message: 'msg'
loc:
  pointer: '"/missing"'
  line: '.line'
"#;
        let eval = evaluator_from_yaml(yaml);
        // Empty provenance map means `/missing` lookup yields `None`; the
        // evaluator MUST then fall through to `loc.line`.
        let value = json!({});
        let owned = ir_for_test(&value, "yaml");
        let diags = eval.evaluate_file(&Utf8PathBuf::from("d.yaml"), &owned.to_borrowed(), "yaml");
        assert_eq!(diags.len(), 1, "expected one diagnostic, got: {diags:?}");
        assert_eq!(
            diags[0].line, 7,
            "loc.pointer with no span must fall through to loc.line",
        );
    }

    #[test]
    fn loc_line_jq_override_alone_still_works() {
        // Spec scenario 3 (legacy path): `loc.line` jq-only override still
        // works after the Phase 2 chain refactor. Pins backward
        // compatibility — M8-era rules with no `loc.pointer` must be
        // unaffected by the new chain.
        let yaml = r#"
id: test.loc-line-only
description: x
severity: error
match:
  format: yaml
check:
  jq: '{"position": {"line": 42}}'
  message: 'msg'
loc:
  line: '.position.line'
"#;
        let eval = evaluator_from_yaml(yaml);
        let value = json!({});
        let owned = ir_for_test(&value, "yaml");
        let diags = eval.evaluate_file(&Utf8PathBuf::from("d.yaml"), &owned.to_borrowed(), "yaml");
        assert_eq!(diags.len(), 1, "expected one diagnostic, got: {diags:?}");
        assert_eq!(
            diags[0].line, 42,
            "legacy `loc.line` jq override must still resolve",
        );
        assert_eq!(diags[0].col, 1, "legacy loc.line path hard-codes col=1");
    }

    #[test]
    fn loc_block_absent_uses_default_line_one() {
        // Spec scenario 4: `loc:` absent → default (1, 1).
        // Mirrors `evaluator_matches_format_and_emits_one_diagnostic` but
        // pins the contract explicitly — a regression that, say, started
        // returning `(0, 0)` on an absent `loc:` block would surface here
        // even if the broader smoke test continued to pass.
        let yaml = r#"
id: test.loc-absent
description: x
severity: error
match:
  format: yaml
check:
  jq: '.'
  message: 'msg'
"#;
        let eval = evaluator_from_yaml(yaml);
        let value = json!({"a": 1});
        let owned = ir_for_test(&value, "yaml");
        let diags = eval.evaluate_file(&Utf8PathBuf::from("d.yaml"), &owned.to_borrowed(), "yaml");
        assert_eq!(diags.len(), 1, "expected one diagnostic, got: {diags:?}");
        assert_eq!(
            (diags[0].line, diags[0].col),
            (1, 1),
            "absent `loc:` block must default to (1, 1)",
        );
    }
}
