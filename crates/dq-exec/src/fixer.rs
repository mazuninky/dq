//! M10 autofix runtime — applies `fix.jq` transforms to a parsed value.
//!
//! [`Fixer::apply`] walks every rule in the wrapped [`Evaluator`] in
//! declaration order and, for each rule that:
//!
//! 1. has a compiled `fix.jq` engine,
//! 2. matches the file's format / glob / `match.filter`,
//! 3. has at least one violation reported by `check.jq` against the
//!    current value,
//!
//! …runs `fix.jq` against the current value. The output replaces the
//! current value if (a) the fix is idempotent (a second application
//! produces the same result) and (b) the post-fix value differs from the
//! pre-fix value. Non-idempotent fixes are skipped at runtime with a
//! `tracing::warn!` log line — they are a rule-author bug, not a hard
//! failure of the run.
//!
//! ## Single-output rule
//!
//! `fix.jq` must produce **exactly one** output, mirroring the
//! `dq set --jq` contract. Zero-output (`empty`) and multi-output (`.[]`)
//! filters surface as [`ExecError::FixApply`] with a wrong-arity message
//! so the rule author can fix the expression.
//!
//! ## Comment preservation
//!
//! None — the CLI's `dq fix` handler re-emits via
//! `Format::write_with_options`, same comment-loss trade-off as
//! `dq set --jq`. The handler module-doc reiterates this for end users.

use std::sync::Arc;

use camino::Utf8Path;

use crate::error::{ExecError, Result};
use crate::evaluator::{CompiledRule, Evaluator};

/// Result of applying every rule's `fix.jq` to one document.
///
/// `fixed` is `true` iff at least one rule's fix produced a value that
/// differed from the input AND was idempotent. `applied_rules` lists the
/// rule ids whose fix actually changed the document; `skipped_non_idempotent`
/// lists the ids whose fix changed the value once but not on the second
/// application (rule-author bug, surfaced to the caller for telemetry /
/// log output).
#[derive(Debug, Clone)]
pub struct FixOutcome {
    /// `true` when at least one fix was applied — i.e.
    /// `applied_rules.is_empty() == false`.
    pub fixed: bool,
    /// Document value after every applicable rule's fix has been applied
    /// (or the input value if no fix changed anything).
    pub new_value: serde_json::Value,
    /// Rule ids whose `fix.jq` produced a different, idempotent result.
    pub applied_rules: Vec<String>,
    /// Rule ids whose `fix.jq` would have changed the value but was not
    /// idempotent on a second application — skipped at runtime.
    pub skipped_non_idempotent: Vec<String>,
}

/// Whole-document autofix driver layered on top of [`Evaluator`].
///
/// Construct with [`Fixer::new`] from a fully-built `Evaluator`; the
/// fixer borrows the evaluator's compiled rules through cheap
/// [`Arc`] clones, so spinning up a [`Fixer`] for one CLI run is
/// effectively free.
#[derive(Debug, Clone)]
pub struct Fixer {
    rules: Vec<Arc<CompiledRule>>,
}

impl Fixer {
    /// Build a `Fixer` from an existing [`Evaluator`].
    ///
    /// The fixer shares the evaluator's pre-compiled rules — no
    /// re-compilation, no extra allocation per rule.
    #[must_use]
    pub fn new(evaluator: &Evaluator) -> Self {
        Self {
            rules: evaluator.compiled_rules().to_vec(),
        }
    }

    /// Apply every applicable rule's `fix.jq` to `value` and return the
    /// outcome.
    ///
    /// Pipeline (per rule, in evaluator order):
    ///
    /// 1. Skip if the rule has no `fix.jq` engine (report-only).
    /// 2. Skip if `format_name` / `match.glob` / `match.filter` reject
    ///    the file.
    /// 3. Skip if `check.jq` finds no violations on the current value
    ///    (no point applying a fix that doesn't apply).
    /// 4. Run `fix.jq` against the current value. Wrong-arity output
    ///    (zero or 2+) → [`ExecError::FixApply`].
    /// 5. **Idempotency check**: run `fix.jq` against the post-fix value.
    ///    If `out2 != out1`, log a warning and skip the rule (do NOT
    ///    apply the fix). Push the id to `skipped_non_idempotent`.
    /// 6. Otherwise, if `current != out1`, set `current = out1` and
    ///    push the id to `applied_rules`.
    ///
    /// # Errors
    ///
    /// - [`ExecError::FixApply`] — `fix.jq` raised a runtime error or
    ///   produced a wrong-arity output stream. Compile failures of
    ///   `fix.jq` surface earlier (at [`Evaluator::new`] time) as
    ///   [`ExecError::RuleCompile`].
    pub fn apply(
        &self,
        path: &Utf8Path,
        value: &serde_json::Value,
        format_name: &str,
    ) -> Result<FixOutcome> {
        let mut current = value.clone();
        let mut applied_rules: Vec<String> = Vec::new();
        let mut skipped_non_idempotent: Vec<String> = Vec::new();

        for rule in &self.rules {
            // 1. No fix → report-only rule.
            let Some(fix_engine) = rule.fix_engine.as_ref() else {
                continue;
            };

            // 2. Match-gate: format / glob / filter.
            if !rule_applies_to(rule, path, &current, format_name) {
                continue;
            }

            // 3. Confirm there is at least one violation. Runtime errors
            // here are tolerated (log + skip) — same contract as the
            // lint evaluator.
            match rule.check_engine.run(&current) {
                Ok(violations) if violations.is_empty() => continue,
                Ok(_) => {}
                Err(err) => {
                    tracing::warn!(
                        rule_id = %rule.rule.id,
                        error = %err,
                        "check.jq raised a runtime error before fix; skipping rule",
                    );
                    continue;
                }
            }

            // 4. Run fix.jq.
            let new_value = run_single_output(fix_engine, &current, &rule.rule.id)?;

            // 5. Idempotency check.
            let new_value_again = run_single_output(fix_engine, &new_value, &rule.rule.id)?;
            if new_value_again != new_value {
                tracing::warn!(
                    rule_id = %rule.rule.id,
                    "fix is non-idempotent; skipping (second application produced a different value)",
                );
                skipped_non_idempotent.push(rule.rule.id.clone());
                continue;
            }

            // 6. Adopt the post-fix value if it actually changed.
            if new_value != current {
                tracing::info!(
                    rule_id = %rule.rule.id,
                    path = %path,
                    "applied fix",
                );
                current = new_value;
                applied_rules.push(rule.rule.id.clone());
            }
        }

        Ok(FixOutcome {
            fixed: !applied_rules.is_empty(),
            new_value: current,
            applied_rules,
            skipped_non_idempotent,
        })
    }
}

/// Mirror of `evaluator::evaluate_one_rule`'s gate, factored down to
/// just the format / glob / filter checks. Returns `true` when the rule
/// applies to `(path, value, format_name)`.
fn rule_applies_to(
    rule: &CompiledRule,
    path: &Utf8Path,
    value: &serde_json::Value,
    format_name: &str,
) -> bool {
    if !rule.rule.match_.format.iter().any(|f| f == format_name) {
        return false;
    }
    if let Some(matcher) = rule.glob_matcher.as_ref()
        && !matcher.is_match(path.as_str())
    {
        return false;
    }
    if let Some(filter) = rule.filter_engine.as_ref() {
        match filter.run(value) {
            Ok(out) => {
                let Some(first) = out.first() else {
                    return false;
                };
                if matches!(
                    first,
                    serde_json::Value::Bool(false) | serde_json::Value::Null
                ) {
                    return false;
                }
            }
            Err(err) => {
                tracing::warn!(
                    rule_id = %rule.rule.id,
                    error = %err,
                    "match.filter raised a runtime error; skipping rule for fix",
                );
                return false;
            }
        }
    }
    true
}

/// Run a `fix.jq` engine against `value`, requiring exactly one output.
///
/// Wraps the underlying [`dq_transform::JqError`] in [`ExecError::FixApply`]
/// with the offending rule id so the CLI can render a useful message.
fn run_single_output(
    engine: &dq_transform::JqEngine,
    value: &serde_json::Value,
    rule_id: &str,
) -> Result<serde_json::Value> {
    let outputs = engine.run(value).map_err(|err| ExecError::FixApply {
        rule_id: rule_id.to_owned(),
        message: format!("fix.jq runtime error: {err}"),
    })?;
    match outputs.len() {
        1 => {
            // Reborrow the only element instead of cloning — the slice
            // owns the values, so we move out by index.
            let mut iter = outputs.into_iter();
            Ok(iter.next().expect("len == 1 guaranteed above"))
        }
        0 => Err(ExecError::FixApply {
            rule_id: rule_id.to_owned(),
            message: "fix.jq produced 0 outputs (expected exactly 1)".to_owned(),
        }),
        n => Err(ExecError::FixApply {
            rule_id: rule_id.to_owned(),
            message: format!("fix.jq produced {n} outputs (expected exactly 1)"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ruleset::{RuleSet, RuleSource};
    use camino::Utf8PathBuf;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn evaluator_from_yaml(yaml: &str) -> Evaluator {
        let set = RuleSet::from_str(yaml, RuleSource::Inline).expect("parse ruleset");
        Evaluator::new(vec![set]).expect("compile evaluator")
    }

    #[test]
    fn fixer_applies_idempotent_jq_to_doc() {
        // Rule fires (`.fixed != true`) and the fix sets `.fixed = true`.
        // A second application is a no-op (still `.fixed == true`), so
        // the outcome must include the rule id in `applied_rules`.
        let yaml = r#"
id: test.set-fixed
description: ensure .fixed is true
severity: warn
match:
  format: yaml
check:
  jq: 'select(.fixed != true) | .'
  message: 'not fixed'
fix:
  jq: '.fixed = true'
"#;
        let eval = evaluator_from_yaml(yaml);
        let fixer = Fixer::new(&eval);
        let path = Utf8PathBuf::from("doc.yaml");
        let value = json!({"name": "x"});
        let out = fixer.apply(&path, &value, "yaml").expect("fix should run");
        assert!(out.fixed);
        assert_eq!(out.applied_rules, vec!["test.set-fixed".to_owned()]);
        assert_eq!(out.new_value["fixed"], true);
        assert!(out.skipped_non_idempotent.is_empty());
    }

    #[test]
    fn fixer_skips_non_idempotent_rule_with_warn_log() {
        // `.counter += 1` is not idempotent: applying it twice produces
        // a different value than once. The fixer must not adopt the
        // change AND must report the rule id in `skipped_non_idempotent`.
        let yaml = r#"
id: test.bad-fix
description: non-idempotent fix
severity: warn
match:
  format: yaml
check:
  jq: '.'
  message: 'always fires'
fix:
  jq: '.counter = (.counter // 0) + 1'
"#;
        let eval = evaluator_from_yaml(yaml);
        let fixer = Fixer::new(&eval);
        let path = Utf8PathBuf::from("doc.yaml");
        let value = json!({"name": "x"});
        let out = fixer.apply(&path, &value, "yaml").expect("fix should run");
        assert!(!out.fixed, "non-idempotent fix must not be applied");
        assert!(out.applied_rules.is_empty());
        assert_eq!(out.skipped_non_idempotent, vec!["test.bad-fix".to_owned()],);
        assert_eq!(out.new_value, value, "value must be untouched");
    }

    #[test]
    fn fixer_skips_rule_without_fix_section() {
        // A rule that fires but ships no `fix:` block — Fixer must
        // produce `fixed = false` and an unchanged value.
        let yaml = r#"
id: test.report-only
description: no fix
severity: warn
match:
  format: yaml
check:
  jq: '.'
  message: 'fires'
"#;
        let eval = evaluator_from_yaml(yaml);
        let fixer = Fixer::new(&eval);
        let path = Utf8PathBuf::from("doc.yaml");
        let value = json!({"a": 1});
        let out = fixer.apply(&path, &value, "yaml").expect("fix runs");
        assert!(!out.fixed);
        assert!(out.applied_rules.is_empty());
        assert_eq!(out.new_value, value);
    }

    #[test]
    fn fixer_skips_rule_when_check_jq_finds_no_violations() {
        // `select(.fixed != true)` produces zero outputs when `.fixed`
        // is already true — no violation, no fix.
        let yaml = r#"
id: test.skip-check
description: only fire when not fixed
severity: warn
match:
  format: yaml
check:
  jq: 'select(.fixed != true) | .'
  message: 'not fixed'
fix:
  jq: '.fixed = true'
"#;
        let eval = evaluator_from_yaml(yaml);
        let fixer = Fixer::new(&eval);
        let path = Utf8PathBuf::from("doc.yaml");
        let value = json!({"fixed": true});
        let out = fixer.apply(&path, &value, "yaml").expect("fix runs");
        assert!(!out.fixed, "no violation → no fix");
        assert_eq!(out.new_value, value);
    }

    #[test]
    fn fixer_returns_unchanged_when_fix_is_a_noop() {
        // The fix is idempotent and matches but doesn't change the
        // value — `fixed` must remain false because nothing changed.
        let yaml = r#"
id: test.noop-fix
description: identity fix
severity: warn
match:
  format: yaml
check:
  jq: '.'
  message: 'always fires'
fix:
  jq: '.'
"#;
        let eval = evaluator_from_yaml(yaml);
        let fixer = Fixer::new(&eval);
        let path = Utf8PathBuf::from("doc.yaml");
        let value = json!({"a": 1});
        let out = fixer.apply(&path, &value, "yaml").expect("fix runs");
        assert!(!out.fixed, "identity fix doesn't change anything");
        assert_eq!(out.new_value, value);
        assert!(out.applied_rules.is_empty());
    }

    #[test]
    fn fixer_runs_rules_in_evaluator_order() {
        // Two fixes that both match: the first sets `.a = 1`, the
        // second sets `.b = 2`. Both must end up in `applied_rules`
        // in declaration order, and the final value carries both.
        let yaml = r#"
id: aaa.first
description: set a
severity: warn
match:
  format: yaml
check:
  jq: 'select(.a != 1)'
  message: 'a'
fix:
  jq: '.a = 1'
---
id: bbb.second
description: set b
severity: warn
match:
  format: yaml
check:
  jq: 'select(.b != 2)'
  message: 'b'
fix:
  jq: '.b = 2'
"#;
        let eval = evaluator_from_yaml(yaml);
        let fixer = Fixer::new(&eval);
        let path = Utf8PathBuf::from("doc.yaml");
        let value = json!({});
        let out = fixer.apply(&path, &value, "yaml").expect("fix runs");
        assert!(out.fixed);
        assert_eq!(
            out.applied_rules,
            vec!["aaa.first".to_owned(), "bbb.second".to_owned()],
        );
        assert_eq!(out.new_value["a"], 1);
        assert_eq!(out.new_value["b"], 2);
    }

    #[test]
    fn fixer_rejects_multi_output_fix_jq() {
        // `.[]` produces one value per array element — wrong arity for
        // a whole-document fix. The fixer must surface `FixApply` with
        // the rule id.
        let yaml = r#"
id: test.bad-arity
description: multi-output fix
severity: warn
match:
  format: yaml
check:
  jq: '.'
  message: 'fires'
fix:
  jq: '.[]'
"#;
        let eval = evaluator_from_yaml(yaml);
        let fixer = Fixer::new(&eval);
        let path = Utf8PathBuf::from("doc.yaml");
        let value = json!([1, 2, 3]);
        let err = fixer
            .apply(&path, &value, "yaml")
            .expect_err("multi-output fix must fail");
        match err {
            ExecError::FixApply { rule_id, message } => {
                assert_eq!(rule_id, "test.bad-arity");
                assert!(
                    message.contains("3") || message.contains("multi"),
                    "expected arity message, got: {message}",
                );
            }
            other => panic!("expected FixApply, got {other:?}"),
        }
    }

    #[test]
    fn fixer_skips_rule_when_format_does_not_match() {
        let yaml = r#"
id: test.json-only
description: only json
severity: warn
match:
  format: json
check:
  jq: '.'
  message: 'fires'
fix:
  jq: '.fixed = true'
"#;
        let eval = evaluator_from_yaml(yaml);
        let fixer = Fixer::new(&eval);
        let path = Utf8PathBuf::from("doc.yaml");
        let value = json!({"a": 1});
        let out = fixer.apply(&path, &value, "yaml").expect("fix runs");
        assert!(!out.fixed);
        assert_eq!(out.new_value, value);
    }
}
