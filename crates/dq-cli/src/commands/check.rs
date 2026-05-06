//! `dq check [RULE] [--inline YAML] FILE...` — run a single rule against files.
//!
//! Sibling to [`crate::commands::lint`]: same evaluation pipeline (via
//! [`crate::commands::lint_core::run_with_rulesets`]), different rule
//! sourcing.
//!
//! Resolution priority for the rule input:
//!
//! 1. `--inline <yaml>` — parse as inline `RuleSet`.
//! 2. positional `<rule>` as a path on disk → `RuleSet::from_path`.
//! 3. positional `<rule>` as a fully-qualified id (e.g. `k8s.no-latest-tag`)
//!    or `@std/<id>` → walk every embedded namespace looking for a matching
//!    rule, build a one-rule [`RuleSet`].
//! 4. otherwise → `InvalidInput` ("provide a rule path or --inline").

use std::io::Write;

use dq_exec::{RuleSet, RuleSource};

use crate::cli::{CheckArgs, Cli};
use crate::commands::lint_core::run_with_rulesets;
use crate::error::InvalidInput;
use crate::output::Reporter;

/// Run the `check` command.
///
/// # Errors
///
/// - [`InvalidInput`] (exit 6) when neither `<rule>` nor `--inline` is set,
///   when a write-mode flag is set, when a glob matches zero files, or when
///   the rule id can't be resolved.
/// - [`crate::error::LintFail`] / [`crate::error::LintWarnStrict`] for
///   non-zero exit codes (see [`crate::commands::lint_core`]).
pub fn run(
    cli: &Cli,
    args: &CheckArgs,
    input_format: Option<&str>,
    reporter: &dyn Reporter,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    cli.ensure_no_write_flags()?;

    if args.rule.is_none() && args.inline.is_none() {
        return Err(anyhow::Error::new(InvalidInput::new(
            "provide a rule path / id or --inline <yaml>",
        )));
    }

    let extra = resolve_rule(args)?;

    run_with_rulesets(
        cli,
        &args.files,
        Vec::new(),
        extra,
        input_format,
        reporter,
        out,
    )
}

/// Build the `extra` rulesets list for the lint pipeline from `--inline`
/// and the positional `<rule>` argument. Returns a single-element vector
/// in all success cases (the `Evaluator` flattens the list).
fn resolve_rule(args: &CheckArgs) -> anyhow::Result<Vec<RuleSet>> {
    if let Some(inline) = args.inline.as_deref() {
        let set = RuleSet::from_str(inline, RuleSource::Inline).map_err(anyhow::Error::new)?;
        return Ok(vec![set]);
    }
    let raw = args.rule.as_deref().expect("guarded above");

    // 1. Try as a path on disk (relative or absolute).
    let candidate = camino::Utf8PathBuf::from(raw);
    if candidate.exists() {
        let set = RuleSet::from_path(&candidate).map_err(anyhow::Error::new)?;
        return Ok(vec![set]);
    }

    // 2. Try as `@std/<ns>.<rule>` or `<ns>.<rule>` lookup. We strip the
    // optional `@std/` prefix and split on the first `.` to get the
    // namespace; the remainder is the rule's `id` field.
    let stripped = raw.strip_prefix("@std/").unwrap_or(raw);
    if let Some(set) = lookup_std_rule(stripped) {
        return Ok(vec![set]);
    }

    // 3. Couldn't resolve.
    let suggestions = collect_known_rule_ids();
    let did_you_mean = closest_matches(raw, &suggestions);
    let mut msg = format!("could not resolve rule '{raw}' as a path or std id");
    if !did_you_mean.is_empty() {
        msg.push_str(&format!(" (did you mean: {})", did_you_mean.join(", ")));
    }
    Err(anyhow::Error::new(InvalidInput::new(msg)))
}

/// Walk every `@std/<ns>` ruleset looking for a rule whose id matches
/// `id`. On match, return a single-rule `RuleSet` so the lint pipeline
/// only fires that rule.
fn lookup_std_rule(id: &str) -> Option<RuleSet> {
    for ns in dq_lint::list_std_rulesets() {
        // Skip namespaces that fail to load or parse rather than aborting the
        // entire lookup — `?` / `.ok()?` here would short-circuit on the FIRST
        // failure, masking rules that live in later namespaces.
        let Some(yaml) = dq_lint::std_ruleset(ns) else {
            continue;
        };
        let Ok(set) = RuleSet::from_str(yaml, RuleSource::Std(ns)) else {
            continue;
        };
        if let Some(rule) = set.rules.iter().find(|r| r.id == id) {
            return Some(RuleSet {
                source: RuleSource::Std(ns),
                rules: vec![rule.clone()],
            });
        }
    }
    None
}

/// Collect every known std rule id for the did-you-mean suggestion list.
fn collect_known_rule_ids() -> Vec<String> {
    let mut out = Vec::new();
    for ns in dq_lint::list_std_rulesets() {
        let Some(yaml) = dq_lint::std_ruleset(ns) else {
            continue;
        };
        let Ok(set) = RuleSet::from_str(yaml, RuleSource::Std(ns)) else {
            continue;
        };
        for rule in set.rules {
            out.push(rule.id);
        }
    }
    out
}

/// Levenshtein-distance-2 filter. Mirrors `dq_exec::loader::did_you_mean`'s
/// shape; we keep a local copy because that helper is private to the loader
/// crate.
fn closest_matches(target: &str, candidates: &[String]) -> Vec<String> {
    candidates
        .iter()
        .filter(|c| levenshtein(target, c) <= 2)
        .cloned()
        .collect()
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr: Vec<usize> = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = (curr[j] + 1).min(prev[j + 1] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{InvalidInput, LintFail};
    use crate::output::JsonReporter;
    use camino::Utf8PathBuf;
    use clap::Parser;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_yaml(content: &str) -> NamedTempFile {
        let mut tmp = NamedTempFile::with_suffix(".yaml").expect("tempfile");
        tmp.write_all(content.as_bytes()).expect("write");
        tmp
    }

    #[test]
    fn check_without_rule_or_inline_errors_with_invalid_input() {
        let cli = Cli::try_parse_from(["dq", "check", "f.yaml"]).expect("clap parse");
        let args = CheckArgs {
            rule: None,
            inline: None,
            files: vec![Utf8PathBuf::from("f.yaml")],
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        let err =
            run(&cli, &args, None, &reporter, &mut out).expect_err("missing rule input must error");
        assert!(err.downcast_ref::<InvalidInput>().is_some());
        assert!(
            err.to_string().contains("--inline") || err.to_string().contains("rule path"),
            "error must guide the user, got: {err}"
        );
    }

    #[test]
    fn check_with_inline_rule_emits_lint_fail_for_error_severity() {
        let inline = r#"
id: test.inline
description: emits error
severity: error
match:
  format: yaml
check:
  jq: '.'
  message: 'fires'
"#;
        let doc_tmp = write_yaml("a: 1\n");
        let path = Utf8PathBuf::from_path_buf(doc_tmp.path().to_path_buf()).expect("UTF-8 path");
        let cli = Cli::try_parse_from(["dq", "check", "--inline", inline, path.as_str()])
            .expect("clap parse");
        let args = CheckArgs {
            rule: None,
            inline: Some(inline.to_owned()),
            files: vec![path],
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, &reporter, &mut out)
            .expect_err("error-severity inline rule must produce LintFail");
        assert!(err.downcast_ref::<LintFail>().is_some());
    }

    #[test]
    fn check_with_unknown_rule_id_returns_invalid_input_with_suggestions() {
        let cli = Cli::try_parse_from(["dq", "check", "--rule", "totally-unknown.rule", "f.yaml"])
            .expect("parse");
        let args = CheckArgs {
            rule: Some("totally-unknown.rule".to_owned()),
            inline: None,
            files: vec![Utf8PathBuf::from("f.yaml")],
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        let err =
            run(&cli, &args, None, &reporter, &mut out).expect_err("unknown rule id must error");
        assert!(err.downcast_ref::<InvalidInput>().is_some());
        assert!(
            err.to_string().contains("could not resolve")
                || err.to_string().contains("did you mean"),
            "error must guide the user, got: {err}"
        );
    }

    #[test]
    fn lookup_std_rule_finds_known_rule_id() {
        // The k8s namespace ships `k8s.no-latest-tag`; if the std library
        // ever drops that rule this assertion needs updating, but the
        // shape (single-rule RuleSet for a known id) is stable.
        let set = lookup_std_rule("k8s.no-latest-tag")
            .expect("k8s.no-latest-tag must resolve via lookup_std_rule");
        assert_eq!(set.rules.len(), 1);
        assert_eq!(set.rules[0].id, "k8s.no-latest-tag");
    }

    #[test]
    fn lookup_std_rule_continues_past_earlier_namespaces() {
        // Regression guard for the `?` / `.ok()?` short-circuit bug:
        // `list_std_rulesets()` returns alphabetically-sorted namespaces
        // (`dockerfile`, `github-actions`, `k8s`, `npm`), so resolving an
        // `npm.*` id requires the loop to pass over three earlier
        // namespaces whose rules don't match. If the loop ever short-
        // circuits on a non-matching namespace this test fails.
        let set = lookup_std_rule("npm.has-license")
            .expect("npm.has-license must resolve via lookup_std_rule");
        assert_eq!(set.rules.len(), 1);
        assert_eq!(set.rules[0].id, "npm.has-license");
    }

    #[test]
    fn levenshtein_returns_known_distances() {
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("abc", "abc"), 0);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
    }
}
