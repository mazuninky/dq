//! Parsed rule schema.
//!
//! `Rule` is the load-bearing public contract: the first user who writes
//! a rule locks in the `id` / `match` / `check` / `fix` field names for a
//! long time. Schema changes after M8 land as additive `#[serde(default)]`
//! fields. `#[serde(deny_unknown_fields)]` on every struct turns typos
//! into structured errors instead of silent no-ops.

use serde::{Deserialize, Deserializer};

use crate::diagnostic::Severity;

/// One rule, parsed from a YAML document.
///
/// See `openspec/changes/add-exec-engine/proposal.md` "What Changes" for
/// the full schema. The fields below mirror that document one-for-one.
///
/// M10 typed the `fix` field as [`RuleFix`] — a whole-document jq
/// transformation. See `openspec/changes/add-autofix/`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// Stable identifier — `<namespace>.<rule-name>` by convention. Used
    /// as the lookup key by `dq explain` and the diagnostic's `rule_id`.
    pub id: String,
    /// Multi-line description shown by `dq explain`.
    pub description: String,
    /// Default severity for diagnostics emitted by this rule.
    pub severity: Severity,
    /// Applicability filter — format match, optional jq pre-filter,
    /// optional path glob. Renamed from `match` to `match_` because
    /// `match` is a Rust keyword.
    #[serde(rename = "match")]
    pub match_: RuleMatch,
    /// The violation finder — a jq expression and a message template.
    pub check: RuleCheck,
    /// M10 autofix payload. When present, [`crate::Fixer`] runs the
    /// `fix.jq` transform against the whole document.
    #[serde(default)]
    pub fix: Option<RuleFix>,
    /// External references (URLs, RFC numbers) shown by `dq explain`.
    #[serde(default)]
    pub references: Vec<String>,
    /// Optional override for the per-violation diagnostic location.
    #[serde(default)]
    pub loc: Option<RuleLoc>,
}

/// M10 autofix payload — a whole-document jq transformation.
///
/// The fix is applied at document level: `fix.jq` is evaluated against
/// the entire parsed value and must produce **exactly one** output that
/// becomes the post-fix document. Anything more elaborate (per-violation
/// fixes, an explicit ops vocabulary) is out of scope for M10.
///
/// **Idempotency.** The runtime [`crate::Fixer`] requires that applying
/// `fix.jq` twice produces the same value as applying it once; non-
/// idempotent fixes are a rule-author bug and are skipped at runtime
/// with a `tracing::warn!` log line.
///
/// **Comment preservation.** None — the re-emit path routes through
/// `Format::write_with_options`, same trade-off as `dq set --jq`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleFix {
    /// jq expression run against the whole document. The single output
    /// replaces the document.
    pub jq: String,
}

/// Applicability filter for a rule.
///
/// `format` accepts either a single format name (`format: yaml`) or an
/// array (`format: [yaml, json]`). Both forms deserialize into a
/// `Vec<String>` via [`deserialize_format_list`].
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleMatch {
    /// One or more format names that this rule applies to.
    #[serde(deserialize_with = "deserialize_format_list")]
    pub format: Vec<String>,
    /// Optional jq predicate over the parsed document — the rule applies
    /// when this expression evaluates to a truthy value.
    #[serde(default)]
    pub filter: Option<String>,
    /// Optional shell-glob pattern matched against the file's relative
    /// path.
    #[serde(default)]
    pub glob: Option<String>,
}

/// The violation finder.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleCheck {
    /// jq expression that emits a stream of violation values. Each
    /// emitted value becomes one `Diagnostic`.
    pub jq: String,
    /// Message template — supports `{{ .field }}` substitution from the
    /// violation value (see `crate::template` once it lands).
    pub message: String,
}

/// Optional override for the per-violation diagnostic location.
///
/// Both fields are jq expressions evaluated against the violation value;
/// when set, they replace the default "use the violation node's parser
/// position, falling back to line/col 1" behaviour.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleLoc {
    /// Optional jq expression producing the file path.
    #[serde(default)]
    pub file: Option<String>,
    /// Optional jq expression producing the 1-based line number.
    #[serde(default)]
    pub line: Option<String>,
}

/// Deserialize `format:` accepting either a single string or an array.
///
/// `serde_yml`'s native enum deserializer can't cleanly express
/// "string-or-list" in a single field, so we hand-roll a `Visitor` that
/// accepts both shapes.
fn deserialize_format_list<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::{self, SeqAccess, Visitor};
    use std::fmt;

    struct FormatVisitor;

    impl<'de> Visitor<'de> for FormatVisitor {
        type Value = Vec<String>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a string or an array of strings naming a format")
        }

        fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
            Ok(vec![value.to_owned()])
        }

        fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
            Ok(vec![value])
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut formats = Vec::new();
            while let Some(value) = seq.next_element::<String>()? {
                formats.push(value);
            }
            Ok(formats)
        }
    }

    deserializer.deserialize_any(FormatVisitor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// A minimal valid rule that every test starts from. Each test mutates
    /// one field to exercise its specific failure mode.
    const MINIMAL_RULE: &str = r#"
id: k8s.no-latest-tag
description: |
  Detects Kubernetes containers with the :latest tag.
severity: error
match:
  format: yaml
check:
  jq: '.spec.template.spec.containers[]? | select(.image | test(":latest$"))'
  message: "Container '{{ .name }}' uses :latest tag"
"#;

    fn parse(yaml: &str) -> Rule {
        serde_yml::from_str(yaml).unwrap_or_else(|err| panic!("parse failed: {err}\n----\n{yaml}"))
    }

    fn parse_err(yaml: &str) -> serde_yml::Error {
        serde_yml::from_str::<Rule>(yaml).expect_err("expected parse failure")
    }

    #[test]
    fn parses_minimal_rule() {
        let rule = parse(MINIMAL_RULE);
        assert_eq!(rule.id, "k8s.no-latest-tag");
        assert_eq!(rule.severity, Severity::Error);
        assert_eq!(rule.match_.format, vec!["yaml".to_owned()]);
        assert_eq!(rule.match_.filter, None);
        assert_eq!(rule.match_.glob, None);
        assert!(rule.check.jq.contains(":latest"));
        assert!(rule.fix.is_none());
        assert!(rule.references.is_empty());
        assert!(rule.loc.is_none());
    }

    #[test]
    fn rejects_unknown_top_level_field() {
        let yaml = format!("{MINIMAL_RULE}custom_top_level: nope\n");
        let err = parse_err(&yaml);
        let formatted = format!("{err}");
        assert!(
            formatted.contains("custom_top_level") || formatted.contains("unknown"),
            "expected error to mention the offending field, got: {formatted}",
        );
    }

    #[test]
    fn rejects_unknown_field_in_match() {
        let yaml = r#"
id: a.b
description: x
severity: error
match:
  format: yaml
  bogus: 1
check:
  jq: '.'
  message: m
"#;
        let err = parse_err(yaml);
        let formatted = format!("{err}");
        assert!(
            formatted.contains("bogus") || formatted.contains("unknown"),
            "expected error to mention the offending field, got: {formatted}",
        );
    }

    #[test]
    fn rejects_unknown_field_in_check() {
        let yaml = r#"
id: a.b
description: x
severity: error
match:
  format: yaml
check:
  jq: '.'
  message: m
  side_effects: yes
"#;
        let err = parse_err(yaml);
        let formatted = format!("{err}");
        assert!(
            formatted.contains("side_effects") || formatted.contains("unknown"),
            "expected error to mention the offending field, got: {formatted}",
        );
    }

    #[test]
    fn rejects_unknown_field_in_loc() {
        let yaml = r#"
id: a.b
description: x
severity: error
match:
  format: yaml
check:
  jq: '.'
  message: m
loc:
  file: '.path'
  weird: 1
"#;
        let err = parse_err(yaml);
        let formatted = format!("{err}");
        assert!(
            formatted.contains("weird") || formatted.contains("unknown"),
            "expected error to mention the offending field, got: {formatted}",
        );
    }

    #[test]
    fn accepts_format_as_single_string() {
        let rule = parse(MINIMAL_RULE);
        assert_eq!(rule.match_.format, vec!["yaml".to_owned()]);
    }

    #[test]
    fn accepts_format_as_array() {
        let yaml = r#"
id: a.b
description: x
severity: warn
match:
  format: [yaml, json]
check:
  jq: '.'
  message: m
"#;
        let rule = parse(yaml);
        assert_eq!(
            rule.match_.format,
            vec!["yaml".to_owned(), "json".to_owned()],
        );
    }

    #[test]
    fn rejects_rule_missing_id() {
        let yaml = r#"
description: x
severity: error
match:
  format: yaml
check:
  jq: '.'
  message: m
"#;
        parse_err(yaml);
    }

    #[test]
    fn rejects_rule_missing_description() {
        let yaml = r#"
id: a.b
severity: error
match:
  format: yaml
check:
  jq: '.'
  message: m
"#;
        parse_err(yaml);
    }

    #[test]
    fn rejects_rule_missing_severity() {
        let yaml = r#"
id: a.b
description: x
match:
  format: yaml
check:
  jq: '.'
  message: m
"#;
        parse_err(yaml);
    }

    #[test]
    fn rejects_rule_missing_match() {
        let yaml = r#"
id: a.b
description: x
severity: error
check:
  jq: '.'
  message: m
"#;
        parse_err(yaml);
    }

    #[test]
    fn rejects_rule_missing_check() {
        let yaml = r#"
id: a.b
description: x
severity: error
match:
  format: yaml
"#;
        parse_err(yaml);
    }

    #[test]
    fn parses_fix_jq_field() {
        // M10: `fix:` is now a typed struct with a single `jq` field.
        // The jq expression is the whole-document transform that
        // `Fixer::apply` runs.
        let yaml = r#"
id: a.b
description: x
severity: warn
match:
  format: yaml
check:
  jq: '.'
  message: m
fix:
  jq: '.fixed = true'
"#;
        let rule = parse(yaml);
        let fix = rule.fix.expect("fix should parse");
        assert_eq!(fix.jq, ".fixed = true");
    }

    #[test]
    fn rejects_unknown_field_in_fix() {
        // `deny_unknown_fields` on `RuleFix` rejects forward-incompatible
        // ops vocabularies that pre-M10 rules might still ship — the
        // loader catches the typo at parse time instead of silently
        // ignoring the field.
        let yaml = r#"
id: a.b
description: x
severity: warn
match:
  format: yaml
check:
  jq: '.'
  message: m
fix:
  jq: '.'
  kind: replace
"#;
        let err = parse_err(yaml);
        let formatted = format!("{err}");
        assert!(
            formatted.contains("kind") || formatted.contains("unknown"),
            "expected error to mention the offending field, got: {formatted}",
        );
    }

    #[test]
    fn rejects_fix_missing_jq() {
        // `jq` is required — there is no other field on `RuleFix` so an
        // empty / partial map must fail.
        let yaml = r#"
id: a.b
description: x
severity: warn
match:
  format: yaml
check:
  jq: '.'
  message: m
fix: {}
"#;
        parse_err(yaml);
    }

    #[test]
    fn parses_full_rule_with_loc_and_references() {
        let yaml = r#"
id: k8s.no-latest-tag
description: |
  Detects Kubernetes containers with the :latest tag.
severity: error
match:
  format: yaml
  filter: '.kind == "Deployment"'
  glob: '**/*.yaml'
check:
  jq: '.spec.template.spec.containers[]?'
  message: "Container '{{ .name }}'"
references:
  - https://kubernetes.io/docs/concepts/containers/images/
loc:
  file: '.path'
  line: '.position.line'
"#;
        let rule = parse(yaml);
        assert_eq!(rule.references.len(), 1);
        let loc = rule.loc.as_ref().expect("loc should parse");
        assert_eq!(loc.file.as_deref(), Some(".path"));
        assert_eq!(loc.line.as_deref(), Some(".position.line"));
        assert_eq!(
            rule.match_.filter.as_deref(),
            Some(".kind == \"Deployment\"")
        );
        assert_eq!(rule.match_.glob.as_deref(), Some("**/*.yaml"));
    }
}
