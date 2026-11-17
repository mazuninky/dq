//! Parsed rule schema.
//!
//! `Rule` is the load-bearing public contract: the first user who writes
//! a rule locks in the `id` / `match` / `check` / `fix` field names for a
//! long time. Schema changes after M8 land as additive `#[serde(default)]`
//! fields. `#[serde(deny_unknown_fields)]` on every struct turns typos
//! into structured errors instead of silent no-ops.
//!
//! Phase 3 of `add-validation-and-extended-formats` reshaped `check` from
//! a single struct into a four-variant [`Check`] enum: `Jq` (legacy),
//! `Schema` / `SchemaFile` (JSON Schema 2020-12), and `Composite`
//! (recursive cross-format checks — Phase 4 turns the parser-side stub
//! into a runtime). The custom [`Deserialize`] impl on [`Check`] enforces
//! mutual exclusion at parse time (per design D1) so rule authors get a
//! crisp error pointing at the offending fields rather than the opaque
//! "no untagged variant matched" message `serde(untagged)` would emit.

use camino::Utf8PathBuf;
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
    /// The violation finder — variant-specific; see [`Check`].
    pub check: Check,
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

/// Autofix payload — either a whole-document jq transform (`jq`,
/// M10 legacy) or a per-violation [`crate::EditScript`]-emitting jq
/// expression (`ops`, Phase 4 of `add-ir-foundation`).
///
/// At least one of `jq` / `ops` MUST be set. Both unset is a parse
/// error surfaced by the custom [`Deserialize`] impl below; both set is
/// allowed (`ops` wins at runtime, with a `tracing::warn!` log line).
///
/// ## `jq` (legacy, deprecated)
///
/// Whole-document jq transformation: the expression is evaluated
/// against the entire parsed document and must produce **exactly one**
/// output that becomes the post-fix document. Comment preservation is
/// **none** — the re-emit path routes through
/// `Format::write_with_options`, same trade-off as `dq set --jq`.
///
/// ## `ops` (Phase 4, preferred)
///
/// jq expression that returns a JSON Patch array (RFC 6902 subset:
/// `add`, `replace`, `remove`). The patch is applied via
/// [`crate::EditScript::apply`] against the parsed document, preserving
/// comments and surrounding bytes. The expression must produce **exactly
/// one** output (the array). Empty arrays are accepted and treated as
/// a no-op.
///
/// ## Idempotency
///
/// The runtime [`crate::Fixer`] requires that applying the fix twice
/// produces the same result as applying it once. Non-idempotent fixes
/// are a rule-author bug and are skipped at runtime with a
/// `tracing::warn!` log line; the document is restored to its pre-apply
/// state for the offending rule.
#[derive(Debug, Clone, PartialEq)]
pub struct RuleFix {
    /// Legacy whole-document jq transform. **Deprecated** — prefer
    /// [`RuleFix::ops`] for new rules so comments are preserved.
    pub jq: Option<String>,
    /// Phase 4 per-violation patch expression. The jq output must be a
    /// JSON Patch array (RFC 6902 subset: `add` / `replace` / `remove`).
    pub ops: Option<String>,
}

/// Wire-shape for [`RuleFix`]. Mirrors the public struct one-for-one;
/// the manual [`Deserialize`] impl on `RuleFix` deserializes through
/// this and then enforces the at-least-one-of validation.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRuleFix {
    #[serde(default)]
    jq: Option<String>,
    #[serde(default)]
    ops: Option<String>,
}

impl<'de> Deserialize<'de> for RuleFix {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawRuleFix::deserialize(deserializer)?;
        if raw.jq.is_none() && raw.ops.is_none() {
            return Err(serde::de::Error::custom(
                "fix block requires at least one of `jq` or `ops` to be set",
            ));
        }
        Ok(Self {
            jq: raw.jq,
            ops: raw.ops,
        })
    }
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
///
/// Phase 3 of `add-validation-and-extended-formats` turned this into a
/// four-variant enum. Existing `check: { jq: ..., message: ... }` rules
/// continue to parse — they land in [`Check::Jq`]. The `Schema` /
/// `SchemaFile` variants drive JSON Schema 2020-12 validation; the
/// `Composite` variant is the parser-side stub for cross-format checks
/// (Phase 4 fills in the runtime).
///
/// The custom [`Deserialize`] impl below enforces mutual exclusion at
/// parse time: exactly one of `jq` / `schema` / `schema_file` /
/// `extract+nested` MUST be present. Rule authors that mix variants get
/// a [`CheckParseError`] tagged with the offending field names; the
/// loader (`RuleSet::from_str`) maps that to a structured
/// [`crate::error::ExecError`] in a follow-up pass once it has the rule
/// id in hand.
///
/// `PartialEq` is intentionally NOT derived: `Rule` itself has no
/// `PartialEq` impl, and the `serde_norway::Value` carried by `Schema`
/// already provides one if a caller ever needs structural comparison.
#[derive(Debug, Clone)]
pub enum Check {
    /// Variant 1: jq-driven check. The `jq` expression emits a stream
    /// of violation values; each one becomes a [`crate::Diagnostic`]
    /// rendered through `message` (mustache-lite templating).
    Jq {
        /// jq expression that emits a stream of violation values.
        jq: String,
        /// Message template — supports `{{ .field }}` substitution from
        /// the violation value.
        message: String,
    },
    /// Variant 2: inline JSON Schema 2020-12 document. `schema` is an
    /// arbitrary YAML/JSON value that compiles into a
    /// `jsonschema::Validator` at `Evaluator::new`.
    Schema {
        /// Inline JSON Schema 2020-12 document.
        schema: serde_norway::Value,
        /// Optional message prefix — when set, prepended to the
        /// auto-generated `keywordLocation`-based message.
        message: Option<String>,
    },
    /// Variant 3: schema document loaded from a file sibling of the
    /// rule's `.yml` source. The path is resolved relative to the
    /// rule directory; absolute paths and `..` escapes are rejected at
    /// `Evaluator::new`.
    SchemaFile {
        /// Path relative to the rule directory.
        schema_file: Utf8PathBuf,
        /// Optional message prefix — same semantics as
        /// [`Check::Schema::message`].
        message: Option<String>,
    },
    /// Variant 4 (Phase 4 of M11): composite cross-format check.
    /// `extract` returns an array of items the runtime re-parses
    /// according to a per-item `format`; `nested` is recursively
    /// applied to each parsed value.
    ///
    /// Phase 3 ships the parser-side shape only — the evaluator emits a
    /// `tracing::warn!` and returns no diagnostics. The full runtime
    /// (recursion bound, coordinate projection) lands in Phase 4.
    Composite {
        /// jq expression returning an array of `{value, format, anchor}`
        /// items.
        extract: String,
        /// Recursively typed nested rule, applied to each extracted
        /// item.
        nested: Box<Rule>,
        /// Required message template — same `{{ .field }}` semantics
        /// as [`Check::Jq::message`].
        message: String,
    },
}

/// Parser-side error variants emitted by `Deserialize for Check`.
///
/// These are converted to [`crate::error::ExecError`] variants by
/// [`RuleSet::from_str`] (which has the rule id in hand) — the
/// [`Deserialize`] impl can't emit those directly because it doesn't
/// know the id. Surfaced as `serde::de::Error::custom` strings prefixed
/// with `dq:check-error:<kind>:<payload>` so the loader can recover the
/// structured information without a fragile string-parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CheckParseError {
    /// More than one of the four mutually-exclusive variants is set.
    MutuallyExclusive { present_fields: Vec<String> },
    /// None of the four variants is set.
    Missing,
    /// `extract` is set but `nested` is not (or vice versa).
    CompositeIncomplete { missing_field: String },
}

impl CheckParseError {
    /// Sentinel prefix used when round-tripping the error through
    /// `serde::de::Error::custom`. The loader recovers the structured
    /// payload by stripping this prefix.
    pub(crate) const SENTINEL_PREFIX: &'static str = "dq:check-error:";

    /// Encode `self` as a sentinel-prefixed string suitable for
    /// `serde::de::Error::custom`.
    pub(crate) fn to_sentinel(&self) -> String {
        match self {
            Self::MutuallyExclusive { present_fields } => {
                format!(
                    "{}mutually_exclusive:{}",
                    Self::SENTINEL_PREFIX,
                    present_fields.join(",")
                )
            }
            Self::Missing => format!("{}missing", Self::SENTINEL_PREFIX),
            Self::CompositeIncomplete { missing_field } => {
                format!(
                    "{}composite_incomplete:{}",
                    Self::SENTINEL_PREFIX,
                    missing_field
                )
            }
        }
    }

    /// Decode a sentinel string produced by [`to_sentinel`].
    ///
    /// Returns `None` when `s` is not a sentinel-prefixed
    /// [`CheckParseError`] payload (the loader then treats the error as
    /// a generic `ExecError::Parse`).
    pub(crate) fn from_sentinel(s: &str) -> Option<Self> {
        let body = s.strip_prefix(Self::SENTINEL_PREFIX)?;
        let (kind, rest) = body.split_once(':').unwrap_or((body, ""));
        match kind {
            "mutually_exclusive" => {
                let present_fields = if rest.is_empty() {
                    Vec::new()
                } else {
                    rest.split(',').map(|s| s.to_owned()).collect()
                };
                Some(Self::MutuallyExclusive { present_fields })
            }
            "missing" => Some(Self::Missing),
            "composite_incomplete" => Some(Self::CompositeIncomplete {
                missing_field: rest.to_owned(),
            }),
            _ => None,
        }
    }
}

/// Wire-shape for [`Check`]. Every variant-discriminating field is
/// optional so `serde` can surface "0 or many fields present" as a
/// validation error rather than a missing-field error from the variant
/// itself.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCheck {
    #[serde(default)]
    jq: Option<String>,
    #[serde(default)]
    schema: Option<serde_norway::Value>,
    #[serde(default)]
    schema_file: Option<Utf8PathBuf>,
    #[serde(default)]
    extract: Option<String>,
    #[serde(default)]
    nested: Option<Box<Rule>>,
    #[serde(default)]
    message: Option<String>,
}

impl<'de> Deserialize<'de> for Check {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawCheck::deserialize(deserializer)?;

        // Composite is encoded as `extract + nested`. Either one alone
        // is a `CompositeIncomplete` error; both together count as one
        // discriminator slot for mutual-exclusion bookkeeping.
        let composite_incomplete = match (raw.extract.is_some(), raw.nested.is_some()) {
            (true, false) => Some("nested"),
            (false, true) => Some("extract"),
            _ => None,
        };
        if let Some(missing) = composite_incomplete {
            return Err(serde::de::Error::custom(
                CheckParseError::CompositeIncomplete {
                    missing_field: missing.to_owned(),
                }
                .to_sentinel(),
            ));
        }

        // Count the discriminators that ARE set. `extract+nested`
        // counts once.
        let mut present_fields: Vec<String> = Vec::new();
        if raw.jq.is_some() {
            present_fields.push("jq".to_owned());
        }
        if raw.schema.is_some() {
            present_fields.push("schema".to_owned());
        }
        if raw.schema_file.is_some() {
            present_fields.push("schema_file".to_owned());
        }
        if raw.extract.is_some() {
            // `nested` is paired with `extract` and counted under the
            // same `extract+nested` discriminator slot below.
            present_fields.push("extract".to_owned());
        }

        match present_fields.len() {
            0 => Err(serde::de::Error::custom(
                CheckParseError::Missing.to_sentinel(),
            )),
            1 => build_one_variant(raw),
            _ => Err(serde::de::Error::custom(
                CheckParseError::MutuallyExclusive { present_fields }.to_sentinel(),
            )),
        }
    }
}

/// Build the appropriate [`Check`] variant from a [`RawCheck`] that
/// has already passed the "exactly one discriminator" gate.
fn build_one_variant<E: serde::de::Error>(raw: RawCheck) -> Result<Check, E> {
    if let Some(jq) = raw.jq {
        let message = raw
            .message
            .ok_or_else(|| serde::de::Error::custom("check.jq requires `message`"))?;
        return Ok(Check::Jq { jq, message });
    }
    if let Some(schema) = raw.schema {
        return Ok(Check::Schema {
            schema,
            message: raw.message,
        });
    }
    if let Some(schema_file) = raw.schema_file {
        return Ok(Check::SchemaFile {
            schema_file,
            message: raw.message,
        });
    }
    if let (Some(extract), Some(nested)) = (raw.extract, raw.nested) {
        let message = raw
            .message
            .ok_or_else(|| serde::de::Error::custom("check.extract+nested requires `message`"))?;
        return Ok(Check::Composite {
            extract,
            nested,
            message,
        });
    }
    // The discriminator-counting gate above guarantees this branch is
    // unreachable, but emitting an explicit error keeps the function
    // total without an `unwrap`.
    Err(serde::de::Error::custom(
        "internal: build_one_variant called with no discriminator",
    ))
}

/// Backward-compat alias for the pre-Phase-3 `RuleCheck` struct name.
///
/// External callers that imported `dq_exec::RuleCheck` still see the
/// type, but it now points at the [`Check`] enum. The alias will be
/// removed in a follow-up cleanup once downstream consumers migrate to
/// the new name.
pub type RuleCheck = Check;

/// Optional override for the per-violation diagnostic location.
///
/// All three fields are jq expressions evaluated against the violation value;
/// when set, they replace the default "use the violation node's parser
/// position, falling back to line/col 1" behaviour.
///
/// **Resolution precedence for `line/col`** (per
/// `data-query-exec`'s "Location override via `loc:`" requirement):
/// `pointer` (preferred) → `line` (deprecated, M8 fallback) → intrinsic
/// (line 1, col 1 today). The `pointer` expression must produce a
/// non-empty RFC 6901 string the evaluator looks up in the input `Ir`'s
/// provenance map; on a successful span lookup the diagnostic's `line` and
/// `col` come from the source bytes. `loc.file` is independent of this
/// chain and continues to override the diagnostic's file path.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleLoc {
    /// Optional jq expression producing an RFC 6901 JSON Pointer string.
    /// Preferred over [`RuleLoc::line`]; see the struct-level rustdoc for
    /// the resolution precedence.
    #[serde(default)]
    pub pointer: Option<String>,
    /// Optional jq expression producing the file path.
    #[serde(default)]
    pub file: Option<String>,
    /// Optional jq expression producing the 1-based line number.
    ///
    /// **Deprecated**: prefer [`RuleLoc::pointer`]. Kept for backwards
    /// compatibility with M8-era rules; removal is deferred to a future
    /// change.
    #[serde(default)]
    pub line: Option<String>,
}

/// Deserialize `format:` accepting either a single string or an array.
///
/// `serde_norway`'s native enum deserializer can't cleanly express
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
        serde_norway::from_str(yaml)
            .unwrap_or_else(|err| panic!("parse failed: {err}\n----\n{yaml}"))
    }

    fn parse_err(yaml: &str) -> serde_norway::Error {
        serde_norway::from_str::<Rule>(yaml).expect_err("expected parse failure")
    }

    /// Extract a [`CheckParseError`] from a serde error message that
    /// embeds a sentinel string. Returns `None` if the error is not
    /// sentinel-encoded (i.e. it's a regular YAML / serde error).
    fn check_parse_error_from(err: &serde_norway::Error) -> Option<CheckParseError> {
        let formatted = format!("{err}");
        // The formatted error usually wraps the custom message in extra
        // context; scan for the sentinel prefix anywhere in the string.
        let idx = formatted.find(CheckParseError::SENTINEL_PREFIX)?;
        let tail = &formatted[idx..];
        // The sentinel runs until the next whitespace or `\n` (serde
        // appends source-position info on a separate line).
        let end = tail.find(['\n', ' ']).unwrap_or(tail.len());
        CheckParseError::from_sentinel(&tail[..end])
    }

    #[test]
    fn parses_minimal_rule_into_check_jq_variant() {
        let rule = parse(MINIMAL_RULE);
        assert_eq!(rule.id, "k8s.no-latest-tag");
        assert_eq!(rule.severity, Severity::Error);
        match &rule.check {
            Check::Jq { jq, message } => {
                assert!(jq.contains(":latest"));
                assert!(message.contains("uses :latest tag"));
            }
            other => panic!("expected Check::Jq, got {other:?}"),
        }
        assert!(rule.fix.is_none());
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

    // -- Check enum variant parsing -------------------------------------

    #[test]
    fn parses_check_schema_inline() {
        let yaml = r#"
id: a.b
description: x
severity: error
match:
  format: yaml
check:
  schema:
    type: object
    required: [name]
"#;
        let rule = parse(yaml);
        match rule.check {
            Check::Schema { schema, message } => {
                assert!(message.is_none());
                let mapping = schema.as_mapping().expect("schema is a mapping");
                assert!(mapping.contains_key("type"));
                assert!(mapping.contains_key("required"));
            }
            other => panic!("expected Check::Schema, got {other:?}"),
        }
    }

    #[test]
    fn parses_check_schema_with_message_prefix() {
        let yaml = r#"
id: a.b
description: x
severity: error
match:
  format: yaml
check:
  schema:
    type: object
  message: "shape mismatch: "
"#;
        let rule = parse(yaml);
        match rule.check {
            Check::Schema { message, .. } => {
                assert_eq!(message.as_deref(), Some("shape mismatch: "));
            }
            other => panic!("expected Check::Schema, got {other:?}"),
        }
    }

    #[test]
    fn parses_check_schema_file() {
        let yaml = r#"
id: a.b
description: x
severity: error
match:
  format: yaml
check:
  schema_file: ./shape.schema.json
"#;
        let rule = parse(yaml);
        match rule.check {
            Check::SchemaFile {
                schema_file,
                message,
            } => {
                assert_eq!(schema_file, Utf8PathBuf::from("./shape.schema.json"));
                assert!(message.is_none());
            }
            other => panic!("expected Check::SchemaFile, got {other:?}"),
        }
    }

    #[test]
    fn parses_check_composite_stub() {
        let yaml = r#"
id: a.b
description: x
severity: error
match:
  format: markdown
check:
  extract: '.. | objects | select(.type == "code")'
  nested:
    id: a.b.inner
    description: inner
    severity: error
    match:
      format: yaml
    check:
      jq: '.'
      message: 'm'
  message: "outer fail"
"#;
        let rule = parse(yaml);
        match rule.check {
            Check::Composite {
                extract,
                nested,
                message,
            } => {
                assert!(extract.contains("type"));
                assert_eq!(nested.id, "a.b.inner");
                assert_eq!(message, "outer fail");
            }
            other => panic!("expected Check::Composite, got {other:?}"),
        }
    }

    // -- Mutual-exclusion error cases -----------------------------------

    #[test]
    fn rejects_check_jq_plus_schema() {
        let yaml = r#"
id: a.b
description: x
severity: error
match:
  format: yaml
check:
  jq: '.'
  message: m
  schema:
    type: object
"#;
        let err = parse_err(yaml);
        let kind = check_parse_error_from(&err)
            .unwrap_or_else(|| panic!("expected sentinel CheckParseError, got: {err}"));
        match kind {
            CheckParseError::MutuallyExclusive { present_fields } => {
                assert!(present_fields.contains(&"jq".to_owned()));
                assert!(present_fields.contains(&"schema".to_owned()));
            }
            other => panic!("expected MutuallyExclusive, got {other:?}"),
        }
    }

    #[test]
    fn rejects_check_schema_plus_schema_file() {
        let yaml = r#"
id: a.b
description: x
severity: error
match:
  format: yaml
check:
  schema:
    type: object
  schema_file: ./x.json
"#;
        let err = parse_err(yaml);
        let kind = check_parse_error_from(&err)
            .unwrap_or_else(|| panic!("expected sentinel CheckParseError, got: {err}"));
        match kind {
            CheckParseError::MutuallyExclusive { present_fields } => {
                assert!(present_fields.contains(&"schema".to_owned()));
                assert!(present_fields.contains(&"schema_file".to_owned()));
            }
            other => panic!("expected MutuallyExclusive, got {other:?}"),
        }
    }

    #[test]
    fn rejects_check_jq_plus_extract() {
        let yaml = r#"
id: a.b
description: x
severity: error
match:
  format: yaml
check:
  jq: '.'
  message: m
  extract: '.'
  nested:
    id: a.b.inner
    description: i
    severity: warn
    match:
      format: yaml
    check:
      jq: '.'
      message: m2
"#;
        let err = parse_err(yaml);
        let kind = check_parse_error_from(&err)
            .unwrap_or_else(|| panic!("expected sentinel CheckParseError, got: {err}"));
        match kind {
            CheckParseError::MutuallyExclusive { present_fields } => {
                assert!(present_fields.contains(&"jq".to_owned()));
                assert!(present_fields.contains(&"extract".to_owned()));
            }
            other => panic!("expected MutuallyExclusive, got {other:?}"),
        }
    }

    #[test]
    fn rejects_all_four_variants_present() {
        let yaml = r#"
id: a.b
description: x
severity: error
match:
  format: yaml
check:
  jq: '.'
  message: m
  schema: {type: object}
  schema_file: ./x.json
  extract: '.'
  nested:
    id: a.b.inner
    description: i
    severity: warn
    match:
      format: yaml
    check:
      jq: '.'
      message: m
"#;
        let err = parse_err(yaml);
        let kind = check_parse_error_from(&err)
            .unwrap_or_else(|| panic!("expected sentinel CheckParseError, got: {err}"));
        match kind {
            CheckParseError::MutuallyExclusive { present_fields } => {
                assert!(present_fields.contains(&"jq".to_owned()));
                assert!(present_fields.contains(&"schema".to_owned()));
                assert!(present_fields.contains(&"schema_file".to_owned()));
                assert!(present_fields.contains(&"extract".to_owned()));
            }
            other => panic!("expected MutuallyExclusive, got {other:?}"),
        }
    }

    #[test]
    fn rejects_empty_check_block() {
        let yaml = r#"
id: a.b
description: x
severity: error
match:
  format: yaml
check: {}
"#;
        let err = parse_err(yaml);
        let kind = check_parse_error_from(&err)
            .unwrap_or_else(|| panic!("expected sentinel CheckParseError, got: {err}"));
        assert!(matches!(kind, CheckParseError::Missing));
    }

    #[test]
    fn rejects_extract_without_nested() {
        let yaml = r#"
id: a.b
description: x
severity: error
match:
  format: yaml
check:
  extract: '.'
  message: m
"#;
        let err = parse_err(yaml);
        let kind = check_parse_error_from(&err)
            .unwrap_or_else(|| panic!("expected sentinel CheckParseError, got: {err}"));
        match kind {
            CheckParseError::CompositeIncomplete { missing_field } => {
                assert_eq!(missing_field, "nested");
            }
            other => panic!("expected CompositeIncomplete, got {other:?}"),
        }
    }

    #[test]
    fn rejects_nested_without_extract() {
        let yaml = r#"
id: a.b
description: x
severity: error
match:
  format: yaml
check:
  nested:
    id: a.b.inner
    description: i
    severity: warn
    match:
      format: yaml
    check:
      jq: '.'
      message: m2
  message: m
"#;
        let err = parse_err(yaml);
        let kind = check_parse_error_from(&err)
            .unwrap_or_else(|| panic!("expected sentinel CheckParseError, got: {err}"));
        match kind {
            CheckParseError::CompositeIncomplete { missing_field } => {
                assert_eq!(missing_field, "extract");
            }
            other => panic!("expected CompositeIncomplete, got {other:?}"),
        }
    }

    // -- Existing fix / loc tests -- migrated to the Check enum shape ---

    #[test]
    fn parses_fix_jq_field() {
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
        assert_eq!(fix.jq.as_deref(), Some(".fixed = true"));
        assert!(fix.ops.is_none());
    }

    #[test]
    fn parses_fix_ops_field() {
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
  ops: '[{"op":"replace","path":"/x","value":1}]'
"#;
        let rule = parse(yaml);
        let fix = rule.fix.expect("fix should parse");
        assert!(fix.jq.is_none());
        assert_eq!(
            fix.ops.as_deref(),
            Some(r#"[{"op":"replace","path":"/x","value":1}]"#)
        );
    }

    #[test]
    fn parses_fix_with_both_jq_and_ops() {
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
  jq: 'A'
  ops: 'B'
"#;
        let rule = parse(yaml);
        let fix = rule.fix.expect("fix should parse");
        assert_eq!(fix.jq.as_deref(), Some("A"));
        assert_eq!(fix.ops.as_deref(), Some("B"));
    }

    #[test]
    fn rejects_unknown_field_in_fix() {
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
    fn rejects_fix_missing_jq_and_ops() {
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
        let err = parse_err(yaml);
        let formatted = format!("{err}");
        assert!(
            formatted.contains("jq") && formatted.contains("ops"),
            "expected error to mention both `jq` and `ops`, got: {formatted}",
        );
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

    // -- Sentinel encode / decode round-trip ----------------------------

    #[test]
    fn check_parse_error_sentinel_round_trip() {
        let inputs = [
            CheckParseError::Missing,
            CheckParseError::MutuallyExclusive {
                present_fields: vec!["jq".to_owned(), "schema".to_owned()],
            },
            CheckParseError::CompositeIncomplete {
                missing_field: "nested".to_owned(),
            },
        ];
        for input in &inputs {
            let encoded = input.to_sentinel();
            let decoded = CheckParseError::from_sentinel(&encoded)
                .unwrap_or_else(|| panic!("decode failed for {encoded}"));
            assert_eq!(&decoded, input);
        }
    }

    #[test]
    fn check_parse_error_sentinel_rejects_non_sentinel_strings() {
        assert!(CheckParseError::from_sentinel("plain serde error").is_none());
        assert!(CheckParseError::from_sentinel("dq:other-error:foo").is_none());
        assert!(CheckParseError::from_sentinel("dq:check-error:bogus_kind").is_none());
    }
}
