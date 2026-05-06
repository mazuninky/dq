//! Autofix runtime — applies `fix.ops` (Phase 4) and `fix.jq` (M10
//! legacy) transforms to a parsed [`Document`].
//!
//! [`Fixer::apply`] walks every rule in the wrapped [`Evaluator`] in
//! declaration order. For each rule that:
//!
//! 1. has a compiled `fix.jq` or `fix.ops` engine,
//! 2. matches the file's format / glob / `match.filter`,
//! 3. has at least one violation reported by `check.jq` against the
//!    current document value,
//!
//! …runs the appropriate fix path. **Precedence**: when both `fix.jq`
//! and `fix.ops` are set on a rule, the OPS branch takes the call and a
//! `tracing::warn!` line records the shadowing.
//!
//! ## OPS branch (Phase 4)
//!
//! 1. Eval the `fix.ops` jq-expression against the current
//!    `serde_json::Value` projection of the document. Wrong-arity
//!    output (zero or 2+) → [`ExecError::FixApply`].
//! 2. Parse the single output as [`EditScript`]. Malformed shapes —
//!    non-array, unsupported `op` (`copy`/`move`/`test`), unknown
//!    field, missing required field — surface as [`ExecError::FixApply`].
//! 3. If `script.is_noop()`, skip this rule (fixed-point reached, but
//!    silent — the rule was already conformant).
//! 4. Clone the document, then `script.apply(&mut working)`. Apply
//!    failures (notably [`dq_core::Error::WriteUnavailable`] when a
//!    prior `fix.jq` rule replaced the document with a value-only one)
//!    are logged and the rule is skipped — they are non-fatal because
//!    the legacy path is the cause, not the offending rule.
//! 5. Idempotency: re-eval `fix.ops` against the post-apply
//!    `working`'s value **before** doing any byte comparison. If the
//!    second `EditScript` is non-empty (`!is_noop()`), the rule is
//!    skipped, the working clone is discarded, and the rule id is
//!    recorded in `FixOutcome::skipped_non_idempotent` — even when the
//!    first apply happened to be byte-stable. The spec's idempotency
//!    contract is a conjunction of `script2.is_noop()` AND
//!    byte-equality on the post-apply doc; both halves must hold.
//! 6. Otherwise (`script2.is_noop()`): if the apply was a byte-noop
//!    against `original_bytes`, skip silently (already conformant). If
//!    the bytes actually changed, adopt `working` into `*doc` and
//!    record the rule id in `FixOutcome::applied_rules`.
//!
//! ## JQ branch (legacy M10, deprecated)
//!
//! Whole-document jq transformation. The output replaces the document
//! tree (via [`Document::value_only`]), which **drops the original
//! bytes and the span map** — subsequent `fix.ops` rules on the same
//! file will see a read-only document and surface
//! [`dq_core::Error::WriteUnavailable`] from
//! [`EditScript::apply`]. The fixer catches that and logs a warn-level
//! skip rather than aborting the whole run.
//!
//! `FixOutcome::legacy_jq_applied` is `true` when at least one rule on
//! the current file took the jq branch; the CLI's `dq fix` handler
//! consumes that flag to decide whether to re-emit the document via
//! `Format::write_with_options` (legacy path) or to take
//! `doc.original_bytes()` directly (OPS-only path, comments preserved).

use std::sync::Arc;

use camino::Utf8Path;
use dq_core::{Document, EditScript};

use crate::error::{ExecError, Result};
use crate::evaluator::{CompiledRule, Evaluator};

/// Result of applying every rule's fix to one document.
///
/// The document itself is mutated in place by [`Fixer::apply`]; this
/// struct only carries the audit fields the CLI consumes.
#[derive(Debug, Clone, Default)]
pub struct FixOutcome {
    /// `true` when at least one fix was applied — i.e.
    /// `applied_rules.is_empty() == false`.
    pub fixed: bool,
    /// Rule ids whose fix produced a different, idempotent result.
    pub applied_rules: Vec<String>,
    /// Rule ids whose fix would have changed the value but was not
    /// idempotent on a second application — skipped at runtime, with
    /// the document restored from the pre-apply clone.
    pub skipped_non_idempotent: Vec<String>,
    /// `true` if any rule on this run took the legacy `fix.jq` path.
    /// The CLI handler consumes this to decide whether the post-fix
    /// `Document::original_bytes()` is canonical (only `fix.ops` ran)
    /// or whether the document needs to be re-emitted through
    /// `Format::write_with_options` (any `fix.jq` ran, dropping spans
    /// and `original_bytes`).
    pub legacy_jq_applied: bool,
}

/// Whole-document autofix driver layered on top of [`Evaluator`].
///
/// Construct with [`Fixer::new`] from a fully-built `Evaluator`; the
/// fixer borrows the evaluator's compiled rules through cheap [`Arc`]
/// clones, so spinning up a [`Fixer`] for one CLI run is effectively
/// free.
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

    /// Apply every applicable rule's `fix.ops` / `fix.jq` to `doc` and
    /// return the audit outcome.
    ///
    /// `doc` is mutated in place: on a successful apply, the
    /// post-fix document replaces the input. On a non-idempotent or
    /// no-op outcome the document is left unchanged for that rule (or
    /// restored from a working clone in the OPS branch).
    ///
    /// See the module-level docs for the per-rule pipeline and the
    /// precedence rule when both `fix.jq` and `fix.ops` are set.
    ///
    /// # Errors
    ///
    /// - [`ExecError::FixApply`] — the fix expression raised a runtime
    ///   error, produced a wrong-arity output stream, or the
    ///   `fix.ops` output failed to parse as an [`EditScript`].
    pub fn apply(
        &self,
        path: &Utf8Path,
        doc: &mut Document,
        format_name: &str,
    ) -> Result<FixOutcome> {
        let mut applied_rules: Vec<String> = Vec::new();
        let mut skipped_non_idempotent: Vec<String> = Vec::new();
        let mut legacy_jq_applied = false;

        for rule in &self.rules {
            // 1. No fix → report-only rule.
            let has_jq = rule.fix_engine.is_some();
            let has_ops = rule.fix_ops_engine.is_some();
            if !has_jq && !has_ops {
                continue;
            }

            // Project the current document to a serde_json value once
            // per rule — every gate and engine below consumes that
            // shape. We rebuild it each iteration because earlier
            // rules in the loop may have mutated `doc`.
            let value = doc.value().to_serde_json();

            // 2. Match-gate: format / glob / filter.
            if !rule_applies_to(rule, path, &value, format_name) {
                continue;
            }

            // 3. Confirm there is at least one violation. Runtime
            // errors here are tolerated (log + skip) — same contract
            // as the lint evaluator.
            match rule.check_engine.run(&value) {
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

            // 4. Precedence: ops wins over jq.
            if has_ops && has_jq {
                tracing::warn!(
                    rule_id = %rule.rule.id,
                    "fix.jq is shadowed by fix.ops",
                );
            }

            if has_ops {
                apply_ops_branch(
                    rule,
                    path,
                    doc,
                    &value,
                    &mut applied_rules,
                    &mut skipped_non_idempotent,
                )?;
            } else {
                debug_assert!(has_jq);
                if apply_jq_branch(
                    rule,
                    path,
                    doc,
                    &value,
                    &mut applied_rules,
                    &mut skipped_non_idempotent,
                )? {
                    legacy_jq_applied = true;
                }
            }
        }

        Ok(FixOutcome {
            fixed: !applied_rules.is_empty(),
            applied_rules,
            skipped_non_idempotent,
            legacy_jq_applied,
        })
    }
}

/// OPS branch of `Fixer::apply` — see module-level docs for the
/// pipeline. Pushes the rule id into either `applied_rules` or
/// `skipped_non_idempotent` (or neither, on a clean no-op or a
/// non-fatal `WriteUnavailable`).
fn apply_ops_branch(
    rule: &CompiledRule,
    path: &Utf8Path,
    doc: &mut Document,
    value: &serde_json::Value,
    applied_rules: &mut Vec<String>,
    skipped_non_idempotent: &mut Vec<String>,
) -> Result<()> {
    let engine = rule
        .fix_ops_engine
        .as_ref()
        .expect("fix_ops_engine present in OPS branch");

    let ops_value = run_single_output(engine, value, &rule.rule.id, "fix.ops")?;
    let script: EditScript = match serde_json::from_value::<EditScript>(ops_value) {
        Ok(s) => s,
        Err(err) => {
            return Err(ExecError::FixApply {
                rule_id: rule.rule.id.clone(),
                message: format!("malformed fix.ops output: {err}"),
            });
        }
    };

    // Empty patch → already conformant. Skip silently.
    if script.is_noop() {
        return Ok(());
    }

    // Clone the doc so a failed apply leaves `*doc` untouched.
    let mut working = doc.clone();
    if let Err(err) = script.apply(&mut working) {
        // The most likely failure here is `WriteUnavailable` — a
        // prior rule's `fix.jq` replaced the document with a
        // value-only one, dropping spans and `original_bytes`. Log
        // and skip; do NOT surface as `FixApply` because the
        // offending rule is the prior `fix.jq`, not this one.
        if matches!(err, dq_core::Error::WriteUnavailable { .. }) {
            tracing::warn!(
                rule_id = %rule.rule.id,
                error = %err,
                "fix.ops cannot apply: doc is no longer write-aware (likely after a prior fix.jq); skipping",
            );
            return Ok(());
        }
        return Err(ExecError::FixApply {
            rule_id: rule.rule.id.clone(),
            message: format!("fix.ops apply failed: {err}"),
        });
    }

    // Idempotency: re-eval against the post-apply value FIRST, before
    // any byte-equality check. The spec contract is a conjunction —
    // `script2.is_noop()` AND byte-equality must both hold for the
    // rule to be considered idempotent. Skipping the re-eval when
    // bytes happen to match would let a rule whose `fix.ops` always
    // emits a non-empty patch (e.g. `[replace /x = 1]` on a doc that
    // already has `x: 1`) pass as silently-skipped instead of being
    // recorded on `skipped_non_idempotent`.
    let value2 = working.value().to_serde_json();
    let ops_value2 = run_single_output(engine, &value2, &rule.rule.id, "fix.ops")?;
    let script2: EditScript = match serde_json::from_value::<EditScript>(ops_value2) {
        Ok(s) => s,
        Err(err) => {
            return Err(ExecError::FixApply {
                rule_id: rule.rule.id.clone(),
                message: format!("malformed fix.ops output (post-apply): {err}"),
            });
        }
    };
    if !script2.is_noop() {
        tracing::warn!(
            rule_id = %rule.rule.id,
            "fix.ops is non-idempotent; skipping (second application produced non-empty script)",
        );
        skipped_non_idempotent.push(rule.rule.id.clone());
        return Ok(());
    }

    // Second eval was empty — confirm the byte-equality half of the
    // idempotency contract. A byte-stable apply with an empty second
    // script is already-conformant; skip silently.
    if working.original_bytes() == doc.original_bytes() {
        return Ok(());
    }

    // Adopt the post-apply working clone.
    tracing::info!(
        rule_id = %rule.rule.id,
        path = %path,
        "applied fix.ops",
    );
    *doc = working;
    applied_rules.push(rule.rule.id.clone());
    Ok(())
}

/// JQ branch (legacy M10) of `Fixer::apply`. Returns `Ok(true)` when
/// the legacy path actually mutated the document (caller should set
/// `legacy_jq_applied`); `Ok(false)` otherwise (non-idempotent skip,
/// no-op identity, etc.).
fn apply_jq_branch(
    rule: &CompiledRule,
    path: &Utf8Path,
    doc: &mut Document,
    value: &serde_json::Value,
    applied_rules: &mut Vec<String>,
    skipped_non_idempotent: &mut Vec<String>,
) -> Result<bool> {
    let engine = rule
        .fix_engine
        .as_ref()
        .expect("fix_engine present in JQ branch");

    // Run fix.jq and require single output.
    let new_value = run_single_output(engine, value, &rule.rule.id, "fix.jq")?;

    // Idempotency check: the second application must produce the same
    // value as the first. Mirrors the M10 byte-equality semantics.
    let new_value_again = run_single_output(engine, &new_value, &rule.rule.id, "fix.jq")?;
    if new_value_again != new_value {
        tracing::warn!(
            rule_id = %rule.rule.id,
            "fix.jq is non-idempotent; skipping (second application produced a different value)",
        );
        skipped_non_idempotent.push(rule.rule.id.clone());
        return Ok(false);
    }

    // No change → already conformant.
    if new_value == *value {
        return Ok(false);
    }

    tracing::debug!(
        rule_id = %rule.rule.id,
        path = %path,
        "applying legacy fix.jq path",
    );

    // Replace the document with a value-only one. **NB**: this drops
    // `original_bytes` and the span map. Subsequent `fix.ops` rules
    // on the same file will surface `WriteUnavailable` from
    // `EditScript::apply`; that is caught and downgraded to a warn in
    // `apply_ops_branch`.
    let new_dq_value = dq_core::Value::from_serde_json(&new_value);
    *doc = Document::value_only(new_dq_value, doc.format());
    applied_rules.push(rule.rule.id.clone());
    Ok(true)
}

/// Mirror of `evaluator::evaluate_one_rule`'s gate, factored down to
/// just the format / glob / filter checks. Returns `true` when the
/// rule applies to `(path, value, format_name)`.
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

/// Run a fix engine against `value`, requiring exactly one output.
///
/// `which` is `"fix.jq"` or `"fix.ops"` — used to make the error
/// messages name the correct engine path.
fn run_single_output(
    engine: &dq_transform::JqEngine,
    value: &serde_json::Value,
    rule_id: &str,
    which: &str,
) -> Result<serde_json::Value> {
    let outputs = engine.run(value).map_err(|err| ExecError::FixApply {
        rule_id: rule_id.to_owned(),
        message: format!("{which} runtime error: {err}"),
    })?;
    match outputs.len() {
        1 => {
            // Move out of the single-element vec without cloning.
            let mut iter = outputs.into_iter();
            Ok(iter.next().expect("len == 1 guaranteed above"))
        }
        0 => Err(ExecError::FixApply {
            rule_id: rule_id.to_owned(),
            message: format!("{which} produced 0 outputs (expected exactly 1)"),
        }),
        n => Err(ExecError::FixApply {
            rule_id: rule_id.to_owned(),
            message: format!("{which} produced {n} outputs (expected exactly 1)"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ruleset::{RuleSet, RuleSource};
    use camino::Utf8PathBuf;
    use dq_core::FormatTag;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn evaluator_from_yaml(yaml: &str) -> Evaluator {
        let set = RuleSet::from_str(yaml, RuleSource::Inline).expect("parse ruleset");
        Evaluator::new(vec![set]).expect("compile evaluator")
    }

    /// Build a value-only document from a `serde_json::Value`. Suitable
    /// for the legacy `fix.jq` tests that don't care about
    /// span-aware byte preservation — the second-pass `fix.ops` tests
    /// build write-aware documents through the YAML parser instead.
    fn value_only_yaml_doc(v: &serde_json::Value) -> Document {
        Document::value_only(dq_core::Value::from_serde_json(v), FormatTag::Yaml)
    }

    /// Build a write-aware YAML doc by parsing real bytes. Required
    /// for `fix.ops` tests because `EditScript::apply` needs spans.
    fn parse_yaml(bytes: &[u8]) -> Document {
        dq_core::parse_yaml_with_spans(bytes).expect("parse yaml fixture")
    }

    /// Build a write-aware JSON doc through the span-aware parser.
    fn parse_json(bytes: &[u8]) -> Document {
        dq_core::parse_json_with_spans(bytes).expect("parse json fixture")
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
        let mut doc = value_only_yaml_doc(&json!({"name": "x"}));
        let out = fixer
            .apply(&path, &mut doc, "yaml")
            .expect("fix should run");
        assert!(out.fixed);
        assert!(out.legacy_jq_applied);
        assert_eq!(out.applied_rules, vec!["test.set-fixed".to_owned()]);
        assert_eq!(doc.value().to_serde_json()["fixed"], true);
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
        let initial = json!({"name": "x"});
        let mut doc = value_only_yaml_doc(&initial);
        let out = fixer
            .apply(&path, &mut doc, "yaml")
            .expect("fix should run");
        assert!(!out.fixed, "non-idempotent fix must not be applied");
        assert!(out.applied_rules.is_empty());
        assert_eq!(out.skipped_non_idempotent, vec!["test.bad-fix".to_owned()]);
        assert_eq!(doc.value().to_serde_json(), initial, "value untouched");
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
        let initial = json!({"a": 1});
        let mut doc = value_only_yaml_doc(&initial);
        let out = fixer.apply(&path, &mut doc, "yaml").expect("fix runs");
        assert!(!out.fixed);
        assert!(out.applied_rules.is_empty());
        assert_eq!(doc.value().to_serde_json(), initial);
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
        let initial = json!({"fixed": true});
        let mut doc = value_only_yaml_doc(&initial);
        let out = fixer.apply(&path, &mut doc, "yaml").expect("fix runs");
        assert!(!out.fixed, "no violation → no fix");
        assert_eq!(doc.value().to_serde_json(), initial);
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
        let initial = json!({"a": 1});
        let mut doc = value_only_yaml_doc(&initial);
        let out = fixer.apply(&path, &mut doc, "yaml").expect("fix runs");
        assert!(!out.fixed, "identity fix doesn't change anything");
        assert_eq!(doc.value().to_serde_json(), initial);
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
        let mut doc = value_only_yaml_doc(&json!({}));
        let out = fixer.apply(&path, &mut doc, "yaml").expect("fix runs");
        assert!(out.fixed);
        assert_eq!(
            out.applied_rules,
            vec!["aaa.first".to_owned(), "bbb.second".to_owned()],
        );
        assert_eq!(doc.value().to_serde_json()["a"], 1);
        assert_eq!(doc.value().to_serde_json()["b"], 2);
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
        let mut doc = value_only_yaml_doc(&json!([1, 2, 3]));
        let err = fixer
            .apply(&path, &mut doc, "yaml")
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
        let initial = json!({"a": 1});
        let mut doc = value_only_yaml_doc(&initial);
        let out = fixer.apply(&path, &mut doc, "yaml").expect("fix runs");
        assert!(!out.fixed);
        assert_eq!(doc.value().to_serde_json(), initial);
    }

    // --------------------------- Phase 4: ops branch -------------------------

    #[test]
    fn fixer_applies_idempotent_fix_ops_to_doc() {
        // Phase 4 sanity: rule with `fix.ops` only. The rule fires
        // when `.x != 5`; the patch replaces `/x` with `5`. A second
        // application sees `.x == 5` and emits an empty patch (no-op).
        let yaml = r#"
id: test.ops-replace-x
description: ensure /x equals 5
severity: warn
match:
  format: yaml
check:
  jq: 'select(.x != 5)'
  message: 'x is not 5'
fix:
  ops: 'if .x != 5 then [{"op":"replace","path":"/x","value":5}] else [] end'
"#;
        let eval = evaluator_from_yaml(yaml);
        let fixer = Fixer::new(&eval);
        let path = Utf8PathBuf::from("doc.yaml");
        let mut doc = parse_yaml(b"x: 3\n");
        let pre_bytes = doc.original_bytes().to_vec();
        let out = fixer
            .apply(&path, &mut doc, "yaml")
            .expect("fix.ops should run");
        assert!(out.fixed);
        assert!(
            !out.legacy_jq_applied,
            "ops-only rule must NOT set legacy_jq_applied",
        );
        assert_eq!(out.applied_rules, vec!["test.ops-replace-x".to_owned()]);
        assert!(out.skipped_non_idempotent.is_empty());
        assert_eq!(doc.value().to_serde_json()["x"], 5);
        assert_ne!(
            doc.original_bytes(),
            pre_bytes.as_slice(),
            "fix.ops must mutate original_bytes via EditScript::apply",
        );
        // Comment-preservation property: byte-level edit, surrounding
        // bytes (the trailing newline) preserved.
        assert!(doc.original_bytes().ends_with(b"\n"));
    }

    #[test]
    fn fixer_skips_malformed_fix_ops_with_fix_apply_error() {
        // The `copy` op is rejected by `EditScript`'s deserializer.
        // Per spec, this surfaces as `ExecError::FixApply` with the
        // rule id.
        let yaml = r#"
id: test.ops-malformed
description: emits an unsupported `copy` op
severity: warn
match:
  format: yaml
check:
  jq: '.'
  message: 'always fires'
fix:
  ops: '[{"op":"copy","from":"/a","path":"/b"}]'
"#;
        let eval = evaluator_from_yaml(yaml);
        let fixer = Fixer::new(&eval);
        let path = Utf8PathBuf::from("doc.yaml");
        let mut doc = parse_yaml(b"a: 1\nb: 2\n");
        let err = fixer
            .apply(&path, &mut doc, "yaml")
            .expect_err("malformed fix.ops must fail");
        match err {
            ExecError::FixApply { rule_id, message } => {
                assert_eq!(rule_id, "test.ops-malformed");
                assert!(
                    message.contains("malformed") || message.contains("copy"),
                    "expected message to mention the malformed shape, got: {message}",
                );
            }
            other => panic!("expected FixApply, got {other:?}"),
        }
    }

    #[test]
    fn fixer_ops_with_jq_fallback_uses_ops() {
        // Spec scenario: when both fix.jq and fix.ops are set, ops
        // wins. We pick ops/jq that produce visibly different output
        // (`/x = 99` vs `/x = 1`) so the assertion is unambiguous.
        let yaml = r#"
id: test.ops-shadows-jq
description: ops wins over jq
severity: warn
match:
  format: yaml
check:
  jq: 'select(.x != 99)'
  message: 'x is not 99'
fix:
  jq: '.x = 1'
  ops: 'if .x != 99 then [{"op":"replace","path":"/x","value":99}] else [] end'
"#;
        let eval = evaluator_from_yaml(yaml);
        let fixer = Fixer::new(&eval);
        let path = Utf8PathBuf::from("doc.yaml");
        let mut doc = parse_yaml(b"x: 3\n");
        let out = fixer
            .apply(&path, &mut doc, "yaml")
            .expect("ops branch runs");
        assert!(out.fixed);
        assert!(
            !out.legacy_jq_applied,
            "ops-precedence rule must NOT set legacy_jq_applied",
        );
        assert_eq!(
            doc.value().to_serde_json()["x"],
            99,
            "ops result (99) must win, not jq result (1)",
        );
    }

    #[test]
    fn fixer_ops_after_jq_skips_with_warn_not_error() {
        // Spec contract: when a prior rule's `fix.jq` swaps the doc
        // for a value-only one, a later rule's `fix.ops` cannot
        // apply. The fixer must skip and log, NOT surface FixApply.
        let yaml = r#"
id: aaa.legacy-jq
description: legacy jq fix runs first
severity: warn
match:
  format: yaml
check:
  jq: 'select(.legacy != true)'
  message: 'm'
fix:
  jq: '.legacy = true'
---
id: bbb.ops-second
description: ops fix that would run after jq dropped spans
severity: warn
match:
  format: yaml
check:
  jq: '.'
  message: 'm'
fix:
  ops: '[{"op":"replace","path":"/legacy","value":false}]'
"#;
        let eval = evaluator_from_yaml(yaml);
        let fixer = Fixer::new(&eval);
        let path = Utf8PathBuf::from("doc.yaml");
        let mut doc = parse_yaml(b"x: 1\n");
        let out = fixer
            .apply(&path, &mut doc, "yaml")
            .expect("post-jq ops skip must NOT surface FixApply");
        assert!(out.fixed, "the legacy jq rule must have applied");
        assert!(out.legacy_jq_applied);
        assert_eq!(out.applied_rules, vec!["aaa.legacy-jq".to_owned()]);
        // `/legacy = true` came from the jq path; the ops rule was
        // skipped silently when it couldn't apply.
        assert_eq!(doc.value().to_serde_json()["legacy"], true);
    }

    #[test]
    fn fixer_ops_idempotency_fixed_point_check() {
        // Phase 4 spec scenario: a non-idempotent ops expression is
        // rejected, the document is restored, and the rule id is in
        // `skipped_non_idempotent`. We use `[{"op":"replace","path":"/c","value":(.c // 0)+1}]`
        // which always emits a non-empty patch, so the second
        // application produces a non-noop and the rule is rejected.
        let yaml = r#"
id: test.ops-counter
description: increments a counter on every apply
severity: warn
match:
  format: yaml
check:
  jq: '.'
  message: 'always fires'
fix:
  ops: '[{"op":"replace","path":"/c","value":((.c // 0) + 1)}]'
"#;
        let eval = evaluator_from_yaml(yaml);
        let fixer = Fixer::new(&eval);
        let path = Utf8PathBuf::from("doc.yaml");
        let initial_bytes = b"c: 0\n".to_vec();
        let mut doc = parse_yaml(&initial_bytes);
        let out = fixer
            .apply(&path, &mut doc, "yaml")
            .expect("non-idempotent ops must NOT raise FixApply");
        assert!(!out.fixed, "non-idempotent fix must not be applied");
        assert!(out.applied_rules.is_empty());
        assert_eq!(
            out.skipped_non_idempotent,
            vec!["test.ops-counter".to_owned()]
        );
        assert_eq!(
            doc.original_bytes(),
            initial_bytes.as_slice(),
            "doc must be restored to pre-apply state on non-idempotency",
        );
    }

    #[test]
    fn fixer_ops_empty_patch_is_silent_noop() {
        // Empty array: doc was already conformant. Skip silently —
        // not in `applied_rules`, not in `skipped_non_idempotent`.
        let yaml = r#"
id: test.ops-empty
description: emits an empty patch
severity: warn
match:
  format: yaml
check:
  jq: '.'
  message: 'always fires'
fix:
  ops: '[]'
"#;
        let eval = evaluator_from_yaml(yaml);
        let fixer = Fixer::new(&eval);
        let path = Utf8PathBuf::from("doc.yaml");
        let initial_bytes = b"x: 1\n".to_vec();
        let mut doc = parse_yaml(&initial_bytes);
        let out = fixer.apply(&path, &mut doc, "yaml").expect("noop ops");
        assert!(!out.fixed);
        assert!(out.applied_rules.is_empty());
        assert!(out.skipped_non_idempotent.is_empty());
        assert_eq!(doc.original_bytes(), initial_bytes.as_slice());
    }

    #[test]
    fn fixer_ops_on_json_doc_works() {
        // Sanity: span-aware JSON parser + ops branch end-to-end.
        let yaml = r#"
id: test.json-ops
description: replace /x in JSON
severity: warn
match:
  format: json
check:
  jq: 'select(.x != 5)'
  message: 'x'
fix:
  ops: 'if .x != 5 then [{"op":"replace","path":"/x","value":5}] else [] end'
"#;
        let eval = evaluator_from_yaml(yaml);
        let fixer = Fixer::new(&eval);
        let path = Utf8PathBuf::from("doc.json");
        let mut doc = parse_json(br#"{"x":3}"#);
        let out = fixer.apply(&path, &mut doc, "json").expect("json ops fix");
        assert!(out.fixed);
        assert!(!out.legacy_jq_applied);
        assert_eq!(doc.value().to_serde_json()["x"], 5);
    }

    // --------------------- Phase 4 (task 4.7) — gap fillers --------------------

    /// Phase 4 / spec scenario "Both `fix.jq` and `fix.ops` set — `ops` wins".
    ///
    /// The existing `fixer_ops_with_jq_fallback_uses_ops` only asserts that
    /// the OPS result lands on the value; this one tightens the contract by
    /// pinning `applied_rules` exactly, asserting `legacy_jq_applied ==
    /// false` (the OPS-only audit flag), and confirming the JQ-side
    /// alternative is **not** what made it into the bytes (a byte-level
    /// search rules out the JQ branch having silently applied first).
    ///
    /// Log-side assertion: the spec also requires a `tracing::warn!` line
    /// "fix.jq is shadowed by fix.ops" — without `tracing-test` wired into
    /// dev-deps that side is left unpinned (gap documented in the task
    /// report). Pinning the `FixOutcome` is sufficient to detect any
    /// regression of the precedence contract.
    #[test]
    fn fixer_shadowing_ops_wins_over_jq_with_audit_pinned() {
        // Pick OPS / JQ paths that emit visibly different bytes so the
        // assertion is unambiguous.
        let yaml = r#"
id: test.ops-shadow-jq
description: ops wins; jq is shadowed
severity: warn
match:
  format: yaml
check:
  jq: 'select(.x != 99)'
  message: 'x is not 99'
fix:
  jq: '.x = 1'
  ops: 'if .x != 99 then [{"op":"replace","path":"/x","value":99}] else [] end'
"#;
        let eval = evaluator_from_yaml(yaml);
        let fixer = Fixer::new(&eval);
        let path = Utf8PathBuf::from("doc.yaml");
        let mut doc = parse_yaml(b"x: 3\n");
        let out = fixer
            .apply(&path, &mut doc, "yaml")
            .expect("ops branch wins");

        // FixOutcome contract: the OPS rule is the only one applied,
        // `legacy_jq_applied` stays false (proof that the JQ branch was
        // skipped, not just byte-overwritten).
        assert!(out.fixed);
        assert_eq!(out.applied_rules, vec!["test.ops-shadow-jq".to_owned()]);
        assert!(
            !out.legacy_jq_applied,
            "ops-precedence rule must NOT mark legacy_jq_applied; got out={out:?}",
        );
        assert!(out.skipped_non_idempotent.is_empty());

        // Value-side: the OPS result wins (99), not the JQ result (1).
        assert_eq!(doc.value().to_serde_json()["x"], 99);

        // Byte-level: confirm the OPS path actually went through
        // `EditScript::apply` against `original_bytes` rather than the
        // legacy value-only swap. The bytes should contain `99`, NOT `1`,
        // and the trailing newline of the input should be preserved
        // (proof that EditScript spliced bytes rather than re-emitting).
        let bytes = doc.original_bytes();
        let s = std::str::from_utf8(bytes).expect("yaml bytes are utf-8");
        assert!(
            s.contains("99"),
            "bytes must contain ops result, got: {s:?}"
        );
        assert!(
            !s.contains("x: 1\n") && !s.contains("x: 1 ") && !s.starts_with("x: 1"),
            "bytes must NOT contain the JQ-side result ('x: 1'), got: {s:?}",
        );
        assert!(
            s.ends_with('\n'),
            "trailing newline preserved by byte-spliced edit, got: {s:?}",
        );
    }

    /// Two OPS rules both fire; declaration order is `aaa` then `bbb`.
    /// Mirror of `fixer_runs_rules_in_evaluator_order` but for the OPS
    /// path. The first rule's byte-edit must visibly affect the second
    /// rule's input (i.e. they are sequenced, not parallelised).
    #[test]
    fn fixer_ops_runs_rules_in_evaluator_order() {
        let yaml = r#"
id: aaa.first
description: set /a to 1
severity: warn
match:
  format: yaml
check:
  jq: 'select(.a != 1)'
  message: 'a'
fix:
  ops: 'if .a != 1 then [{"op":"replace","path":"/a","value":1}] else [] end'
---
id: bbb.second
description: set /b to 2
severity: warn
match:
  format: yaml
check:
  jq: 'select(.b != 2)'
  message: 'b'
fix:
  ops: 'if .b != 2 then [{"op":"replace","path":"/b","value":2}] else [] end'
"#;
        let eval = evaluator_from_yaml(yaml);
        let fixer = Fixer::new(&eval);
        let path = Utf8PathBuf::from("doc.yaml");
        let mut doc = parse_yaml(b"a: 0\nb: 0\n");
        let out = fixer.apply(&path, &mut doc, "yaml").expect("ops fixes run");

        assert!(out.fixed);
        // Declaration order is the contract: aaa.first before bbb.second.
        assert_eq!(
            out.applied_rules,
            vec!["aaa.first".to_owned(), "bbb.second".to_owned()],
        );
        assert!(
            !out.legacy_jq_applied,
            "OPS-only run must not set legacy_jq_applied; got out={out:?}",
        );
        assert!(out.skipped_non_idempotent.is_empty());

        // Both edits visible in the value AND in the bytes.
        let v = doc.value().to_serde_json();
        assert_eq!(v["a"], 1);
        assert_eq!(v["b"], 2);
        let s = std::str::from_utf8(doc.original_bytes()).expect("utf-8");
        assert!(
            s.contains("a: 1") && s.contains("b: 2"),
            "post-fix bytes must reflect both rules in order, got: {s:?}",
        );
    }

    /// Phase 4 coexistence — OPS rule fires first against a YAML doc and
    /// produces a span-aware byte edit; a later JQ rule fires on the same
    /// doc and drops to the value-only path. `applied_rules` must list
    /// both; `legacy_jq_applied` is true (the CLI consumer uses that to
    /// decide whether to re-emit).
    #[test]
    fn fixer_mixed_ops_then_jq_coexist_in_one_run() {
        // Order matters: OPS first (mutates bytes, spans intact), then
        // JQ (drops spans). Reversed order is a separate spec scenario
        // already covered by `fixer_ops_after_jq_skips_with_warn_not_error`.
        let yaml = r#"
id: aaa.ops-first
description: ops runs first (byte-level)
severity: warn
match:
  format: yaml
check:
  jq: 'select(.x != 5)'
  message: 'x'
fix:
  ops: 'if .x != 5 then [{"op":"replace","path":"/x","value":5}] else [] end'
---
id: bbb.jq-second
description: legacy jq runs second (drops spans)
severity: warn
match:
  format: yaml
check:
  jq: 'select(.legacy != true)'
  message: 'm'
fix:
  jq: '.legacy = true'
"#;
        let eval = evaluator_from_yaml(yaml);
        let fixer = Fixer::new(&eval);
        let path = Utf8PathBuf::from("doc.yaml");
        let mut doc = parse_yaml(b"x: 3\n");
        let out = fixer
            .apply(&path, &mut doc, "yaml")
            .expect("mixed ops+jq run");

        assert!(out.fixed);
        // Both rules applied, in declaration order.
        assert_eq!(
            out.applied_rules,
            vec!["aaa.ops-first".to_owned(), "bbb.jq-second".to_owned()],
        );
        assert!(
            out.legacy_jq_applied,
            "any JQ rule on a run must set legacy_jq_applied; got out={out:?}",
        );
        assert!(out.skipped_non_idempotent.is_empty());

        // Both edits land on the value tree; the CLI consumer would re-emit
        // the doc through the format writer because legacy_jq_applied=true.
        let v = doc.value().to_serde_json();
        assert_eq!(v["x"], 5);
        assert_eq!(v["legacy"], true);
    }

    /// Match-gate skips an OPS rule cleanly — `applied_rules` empty, doc
    /// bytes byte-identical to the input. Mirrors the JQ-side
    /// `fixer_skips_rule_when_format_does_not_match` for the OPS branch.
    #[test]
    fn fixer_ops_skipped_when_format_does_not_match() {
        let yaml = r#"
id: test.json-only-ops
description: only json
severity: warn
match:
  format: json
check:
  jq: '.'
  message: 'fires'
fix:
  ops: '[{"op":"replace","path":"/x","value":1}]'
"#;
        let eval = evaluator_from_yaml(yaml);
        let fixer = Fixer::new(&eval);
        let path = Utf8PathBuf::from("doc.yaml");
        let initial_bytes = b"x: 0\n".to_vec();
        let mut doc = parse_yaml(&initial_bytes);
        let out = fixer
            .apply(&path, &mut doc, "yaml")
            .expect("format mismatch — clean skip");

        assert!(!out.fixed);
        assert!(out.applied_rules.is_empty());
        assert!(!out.legacy_jq_applied);
        assert!(out.skipped_non_idempotent.is_empty());
        assert_eq!(
            doc.original_bytes(),
            initial_bytes.as_slice(),
            "format-mismatch skip must leave bytes untouched",
        );
        assert_eq!(doc.value().to_serde_json()["x"], 0);
    }

    /// Defensive corner case — the spec's `fix.ops` idempotency contract
    /// is a conjunction of (a) byte-equality after second apply AND (b)
    /// `script2.is_noop()` on the second eval. This test pins the
    /// `script2.is_noop()` arm: a rule whose `fix.ops` always emits the
    /// same non-empty `[replace /x = 1]` patch, run against `x: 1` where
    /// the patch is a byte-noop on apply but the second-eval script is
    /// non-empty.
    ///
    /// Per spec, `skipped_non_idempotent` MUST contain the rule id.
    #[test]
    fn fixer_ops_idempotency_requires_second_eval_to_be_noop() {
        // Always emits a non-empty patch; on `x: 1` the apply is a
        // byte-noop, but `script2.is_noop()` is false.
        let yaml = r#"
id: test.ops-always-replace
description: emits a non-empty patch even on conformant input
severity: warn
match:
  format: yaml
check:
  jq: '.'
  message: 'fires'
fix:
  ops: '[{"op":"replace","path":"/x","value":1}]'
"#;
        let eval = evaluator_from_yaml(yaml);
        let fixer = Fixer::new(&eval);
        let path = Utf8PathBuf::from("doc.yaml");
        let initial_bytes = b"x: 1\n".to_vec();
        let mut doc = parse_yaml(&initial_bytes);
        let out = fixer
            .apply(&path, &mut doc, "yaml")
            .expect("non-idempotent ops must NOT raise FixApply");

        // Spec contract: the rule id is on skipped_non_idempotent.
        assert!(!out.fixed);
        assert!(out.applied_rules.is_empty());
        assert_eq!(
            out.skipped_non_idempotent,
            vec!["test.ops-always-replace".to_owned()],
            "spec requires script2.is_noop() check; non-empty 2nd script → skipped_non_idempotent",
        );
        // Document bytes must be untouched.
        assert_eq!(doc.original_bytes(), initial_bytes.as_slice());
    }
}
