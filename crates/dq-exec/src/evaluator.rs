//! Pre-compiled rule runner.
//!
//! [`Evaluator::new`] takes one or more [`crate::ruleset::RuleSet`]s and
//! compiles every rule's `match.filter`, `check.jq`, `loc.file`, and
//! `loc.line` jq expressions, plus its `match.glob` pattern, up-front.
//! [`Evaluator::evaluate_file`] runs the compiled rules against a
//! pre-parsed document value and emits one [`Diagnostic`] per violation.
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
//! 5. Diagnostic build: render `check.message`, resolve `loc.file` /
//!    `loc.line` overrides, attach severity / references / fix payload.
//!
//! ## Position metadata
//!
//! `Evaluator::evaluate_file` receives a `serde_json::Value` (the
//! `dq-core` value adapter strips parser-provided byte spans before
//! handing the value here). Without a parsed-position pipeline, the
//! evaluator defaults `line` and `col` to `1`. Rules that need a real
//! position derive it via the `loc.line` jq override. Wiring the
//! span-preserving variant through the value adapter is M9+ work.
//!
//! ## Robustness
//!
//! Compile-time failures (jq parse errors, glob parse errors) surface as
//! [`crate::error::ExecError`] from [`Evaluator::new`]. Runtime failures
//! during [`Evaluator::evaluate_file`] (a `check.jq` that crashes on a
//! particular input shape, a `loc.line` expression that returns a
//! non-integer) are logged via `tracing::warn!` and the offending rule
//! is skipped for that file — the principle is that one badly-written
//! rule must not crash the entire run.

use std::sync::Arc;

use camino::{Utf8Path, Utf8PathBuf};
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

    /// Run every applicable rule against `value` and collect the
    /// resulting diagnostics.
    ///
    /// `format_name` is the canonical format name of the parsed document
    /// (e.g. `"yaml"`, `"json"`). Only rules whose `match.format` list
    /// includes that name are considered.
    #[must_use]
    pub fn evaluate_file(
        &self,
        path: &Utf8Path,
        value: &serde_json::Value,
        format_name: &str,
    ) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for rule in &self.rules {
            evaluate_one_rule(rule, path, value, format_name, &mut diagnostics);
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
    let (loc_file_engine, loc_line_engine) = match rule.loc.as_ref() {
        Some(loc) => {
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
            (file, line)
        }
        None => (None, None),
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
        loc_file_engine,
        loc_line_engine,
        fix_engine,
    })
}

/// Run the per-rule pipeline against `(path, value, format_name)` and
/// push any resulting diagnostics into `out`.
fn evaluate_one_rule(
    rule: &CompiledRule,
    path: &Utf8Path,
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
        out.push(build_diagnostic(rule, path, violation));
    }
}

/// Render a diagnostic for one violation value.
fn build_diagnostic(
    rule: &CompiledRule,
    path: &Utf8Path,
    violation: &serde_json::Value,
) -> Diagnostic {
    let message = template::render(&rule.rule.check.message, violation);
    let file = resolve_loc_file(rule, path, violation);
    let line = resolve_loc_line(rule, violation);
    Diagnostic {
        rule_id: rule.rule.id.clone(),
        severity: rule.rule.severity,
        message,
        file,
        line,
        col: 1,
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

/// Resolve the `line` field. Default is `1`; a `loc.line` jq override
/// must produce a positive integer to take effect.
fn resolve_loc_line(rule: &CompiledRule, violation: &serde_json::Value) -> u32 {
    if let Some(engine) = rule.loc_line_engine.as_ref() {
        match engine.run(violation) {
            Ok(out) => {
                if let Some(first) = out.first() {
                    if let Some(n) = first.as_u64()
                        && n >= 1
                        && n <= u64::from(u32::MAX)
                    {
                        return u32::try_from(n).unwrap_or(1);
                    } else if let Some(n) = first.as_i64()
                        && n >= 1
                        && n <= i64::from(u32::MAX)
                    {
                        return u32::try_from(n).unwrap_or(1);
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
    1
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
        let diags = eval.evaluate_file(&path, &value, "yaml");
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
        let diags =
            eval.evaluate_file(&Utf8PathBuf::from("doc.json"), &json!({"name": ""}), "json");
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
        let diags = eval.evaluate_file(&Utf8PathBuf::from("d.yaml"), &value, "yaml");
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
        let diags = eval.evaluate_file(&Utf8PathBuf::from("d.yaml"), &json!({"a": 1}), "yaml");
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
        let diags = eval.evaluate_file(&Utf8PathBuf::from("d.yaml"), &json!({"a": 1}), "yaml");
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
        let diags = eval.evaluate_file(&Utf8PathBuf::from("d.yaml"), &json!({"a": 1}), "yaml");
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
        let diags = eval.evaluate_file(&Utf8PathBuf::from("d.yaml"), &json!({"a": 1}), "yaml");
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
        let matched = eval.evaluate_file(&Utf8PathBuf::from("dir/foo.yaml"), &json!({}), "yaml");
        assert_eq!(matched.len(), 1, "expected glob match for dir/foo.yaml");

        let skipped = eval.evaluate_file(&Utf8PathBuf::from("dir/bar.yaml"), &json!({}), "yaml");
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
        let diags = eval.evaluate_file(&Utf8PathBuf::from("d.yaml"), &value, "yaml");
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
        let diags = eval.evaluate_file(&Utf8PathBuf::from("real.yaml"), &json!({}), "yaml");
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
        let diags = eval.evaluate_file(&Utf8PathBuf::from("real.yaml"), &json!({}), "yaml");
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
        let diags = eval.evaluate_file(&Utf8PathBuf::from("d.yaml"), &json!("a-string"), "yaml");
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
        let diags =
            cloned.evaluate_file(&Utf8PathBuf::from("d.yaml"), &json!({"name": ""}), "yaml");
        assert_eq!(diags.len(), 1);
    }
}
