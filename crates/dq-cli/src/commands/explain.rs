//! `dq explain RULE_ID` — print a rule's description, severity, and references.
//!
//! Walks every embedded `@std/<ns>` ruleset plus `<cwd>/.dq/rules/` (when
//! present) looking for a rule with `id == args.rule_id`. The optional
//! `@std/` prefix on the input is stripped before comparison so users can
//! type either `k8s.no-latest-tag` or `@std/k8s.no-latest-tag`.
//!
//! Output formats: `console` (default human-readable) and `json`. Other
//! formats produce an `InvalidInput` rejection.

use std::io::Write;

use camino::Utf8PathBuf;
use dq_exec::{Rule, RuleSet, RuleSource};

use crate::cli::{Cli, ExplainArgs};
use crate::error::InvalidInput;
use crate::output::{OutputFormat, Reporter};

/// Run the `explain` command.
///
/// # Errors
///
/// - [`InvalidInput`] (exit 6) when the rule id can't be resolved or the
///   chosen output format is unsupported.
pub fn run(
    cli: &Cli,
    args: &ExplainArgs,
    reporter: &dyn Reporter,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    let target = args.rule_id.strip_prefix("@std/").unwrap_or(&args.rule_id);
    let mut all_rule_ids: Vec<String> = Vec::new();
    let mut found: Option<Rule> = None;

    for ns in dq_lint::list_std_rulesets() {
        let Some(yaml) = dq_lint::std_ruleset(ns) else {
            continue;
        };
        let Ok(set) = RuleSet::from_str(yaml, RuleSource::Std(ns)) else {
            continue;
        };
        for rule in &set.rules {
            all_rule_ids.push(rule.id.clone());
            if rule.id == target && found.is_none() {
                found = Some(rule.clone());
            }
        }
    }

    // Walk `<cwd>/.dq/rules/` if present.
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|p| Utf8PathBuf::from_path_buf(p).ok())
        .unwrap_or_else(|| Utf8PathBuf::from("."));
    let project = cwd.join(".dq").join("rules");
    if project.is_dir()
        && let Ok(set) = RuleSet::from_path(&project)
    {
        for rule in &set.rules {
            all_rule_ids.push(rule.id.clone());
            if rule.id == target && found.is_none() {
                found = Some(rule.clone());
            }
        }
    }

    let Some(rule) = found else {
        let did_you_mean = closest_matches(target, &all_rule_ids);
        let mut msg = format!("unknown rule '{target}'");
        if !did_you_mean.is_empty() {
            msg.push_str(&format!(" (did you mean: {})", did_you_mean.join(", ")));
        }
        return Err(anyhow::Error::new(InvalidInput::new(msg)));
    };

    match cli.format {
        OutputFormat::Console => render_console(&rule, out)?,
        OutputFormat::Json => {
            let value = serde_json::json!({
                "id": rule.id,
                "description": rule.description,
                "severity": rule.severity.as_str(),
                "references": rule.references,
            });
            reporter.report(&value, out)?;
        }
        other => {
            return Err(anyhow::Error::new(InvalidInput::new(format!(
                "format '{other:?}' is not supported by `dq explain` (use console / json)"
            ))));
        }
    }
    Ok(())
}

fn render_console(rule: &Rule, out: &mut dyn Write) -> std::io::Result<()> {
    writeln!(out, "Rule: {}", rule.id)?;
    writeln!(out, "Severity: {}", rule.severity.as_str())?;
    writeln!(out)?;
    writeln!(out, "{}", rule.description.trim_end())?;
    if !rule.references.is_empty() {
        writeln!(out)?;
        writeln!(out, "References:")?;
        for r in &rule.references {
            writeln!(out, "  - {r}")?;
        }
    }
    Ok(())
}

/// Levenshtein-distance-2 filter — see [`crate::commands::check`] for the
/// shared shape. Local copy because the helper is not exported from
/// `dq-exec`.
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
    use crate::output::JsonReporter;
    use clap::Parser;

    fn cli_console() -> Cli {
        Cli::try_parse_from(["dq", "explain", "x"]).expect("clap parse")
    }

    fn cli_json() -> Cli {
        Cli::try_parse_from(["dq", "-F", "json", "explain", "x"]).expect("clap parse")
    }

    #[test]
    fn explain_known_std_rule_renders_console_with_id_and_severity() {
        let cli = cli_console();
        let args = ExplainArgs {
            rule_id: "k8s.no-latest-tag".to_owned(),
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, &reporter, &mut out).expect("known rule must succeed");
        let s = String::from_utf8(out).expect("utf8");
        assert!(s.contains("Rule: k8s.no-latest-tag"));
        assert!(s.contains("Severity:"));
    }

    #[test]
    fn explain_with_at_std_prefix_resolves_known_rule() {
        let cli = cli_console();
        let args = ExplainArgs {
            rule_id: "@std/k8s.no-latest-tag".to_owned(),
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, &reporter, &mut out).expect("@std-prefixed rule must resolve");
        let s = String::from_utf8(out).expect("utf8");
        assert!(s.contains("k8s.no-latest-tag"));
    }

    #[test]
    fn explain_unknown_rule_returns_invalid_input_with_suggestions() {
        let cli = cli_console();
        let args = ExplainArgs {
            rule_id: "k8s.no-latest-tg".to_owned(), // typo
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, &reporter, &mut out).expect_err("unknown rule must error");
        assert!(err.downcast_ref::<InvalidInput>().is_some());
        let s = err.to_string();
        assert!(
            s.contains("unknown rule") || s.contains("did you mean"),
            "error must guide the user, got: {s}"
        );
    }

    #[test]
    fn explain_json_format_emits_structured_envelope() {
        let cli = cli_json();
        let args = ExplainArgs {
            rule_id: "k8s.no-latest-tag".to_owned(),
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, &reporter, &mut out).expect("json render must succeed");
        let parsed: serde_json::Value = serde_json::from_slice(&out).expect("valid json");
        assert_eq!(parsed["id"], "k8s.no-latest-tag");
        assert!(parsed["description"].is_string());
        assert!(parsed["severity"].is_string());
        assert!(parsed["references"].is_array());
    }

    #[test]
    fn explain_yaml_format_returns_invalid_input() {
        let cli = Cli::try_parse_from(["dq", "-F", "yaml", "explain", "k8s.no-latest-tag"])
            .expect("parse");
        let args = ExplainArgs {
            rule_id: "k8s.no-latest-tag".to_owned(),
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, &reporter, &mut out).expect_err("yaml is unsupported");
        assert!(err.downcast_ref::<InvalidInput>().is_some());
    }
}
