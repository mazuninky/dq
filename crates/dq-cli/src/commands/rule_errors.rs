//! Rules that raise, as `lint`, `check` and `fix` report them.
//!
//! A rule whose `match.filter` or `check.jq` raises a jq error is skipped
//! for the document, so its findings may be missing. The run goes on — every
//! other rule and file is handled and the report written — and then fails:
//! each rule error is a [`RULE_ID`] result of a structured report
//! ([`diagnostic`]) and a line on stderr ([`RuleErrors::log`]), and the run's
//! error is [`RuleErrorFail`] (exit 3). `--allow-rule-errors` keeps all of it
//! off: the rule runtime logs a warning and the run passes.

use camino::{Utf8Path, Utf8PathBuf};
use dq_exec::{Diagnostic, RuleError, Severity};
use indexmap::IndexSet;

use crate::error::RuleErrorFail;

/// The rule id a rule error is reported under.
pub(crate) const RULE_ID: &str = "lint.rule_error";

/// The rule errors of one run, in the order they were recorded. One per
/// file, rule, field and error text, however many documents of a stream
/// raised it.
#[derive(Debug, Default)]
pub(crate) struct RuleErrors {
    errors: IndexSet<(Utf8PathBuf, RuleError)>,
}

impl RuleErrors {
    /// Record that `error` was raised on `file`; `false` when the run
    /// already holds it.
    pub(crate) fn record(&mut self, file: &Utf8Path, error: RuleError) -> bool {
        self.errors.insert((file.to_owned(), error))
    }

    /// Whether no rule raised.
    pub(crate) fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }

    /// Name every rule error on stderr, one line each. At `error` level, so
    /// `-q` shows them too.
    pub(crate) fn log(&self) {
        for (file, error) in &self.errors {
            tracing::error!(
                rule_id = %error.rule_id,
                file = %file,
                field = %error.stage,
                error = %error.error,
                "rule did not run",
            );
        }
    }

    /// The run's error; `None` when no rule raised.
    pub(crate) fn failure(&self) -> Option<RuleErrorFail> {
        let count = self.errors.len();
        (count > 0).then_some(RuleErrorFail { count })
    }
}

/// The result `error` is in a structured report: on `file`, at its start —
/// the error is the rule's, not a place in the file.
pub(crate) fn diagnostic(file: &Utf8Path, error: &RuleError) -> Diagnostic {
    Diagnostic {
        rule_id: RULE_ID.to_owned(),
        severity: Severity::Error,
        message: format!(
            "rule '{}' did not run: {}: {}",
            error.rule_id, error.stage, error.error
        ),
        file: Some(file.to_owned()),
        line: 1,
        col: 1,
        span: None,
        references: Vec::new(),
        fix: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dq_exec::RuleErrorStage;

    fn raised(rule_id: &str, stage: RuleErrorStage) -> RuleError {
        RuleError {
            rule_id: rule_id.to_owned(),
            stage,
            error: "jq runtime error: boom".to_owned(),
        }
    }

    #[test]
    fn one_error_per_file_rule_field_and_text() {
        let mut errors = RuleErrors::default();
        assert!(errors.failure().is_none());
        let [a, b] = ["a.yaml", "b.yaml"].map(Utf8Path::new);
        assert!(errors.record(a, raised("t.r", RuleErrorStage::CheckJq)));
        assert!(!errors.record(a, raised("t.r", RuleErrorStage::CheckJq)));
        assert!(errors.record(a, raised("t.r", RuleErrorStage::MatchFilter)));
        assert!(errors.record(a, raised("t.s", RuleErrorStage::CheckJq)));
        assert!(errors.record(b, raised("t.r", RuleErrorStage::CheckJq)));
        assert_eq!(
            errors.failure().map(|f| f.to_string()).as_deref(),
            Some("4 rule error(s): findings may be missing (--allow-rule-errors to accept)")
        );
    }

    #[test]
    fn the_diagnostic_names_the_rule_the_field_and_the_error() {
        let found = diagnostic(
            Utf8Path::new("a.yaml"),
            &raised("t.r", RuleErrorStage::MatchFilter),
        );
        assert_eq!(
            found.message,
            "rule 't.r' did not run: match.filter: jq runtime error: boom"
        );
        assert_eq!(found.rule_id, "lint.rule_error");
        assert_eq!(found.severity, Severity::Error);
        assert_eq!((found.line, found.col), (1, 1));
    }
}
