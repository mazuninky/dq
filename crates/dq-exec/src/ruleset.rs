//! Multi-rule containers and the three loaders.
//!
//! `RuleSet` is "a vector of `Rule`s with provenance" — the `source` field
//! tells `dq explain` and the loader's de-duplication step where the rules
//! came from. Three loaders cover the inputs the CLI needs:
//!
//! - [`RuleSet::from_str`] — inline YAML (single rule or a `---`-separated stream).
//! - [`RuleSet::from_path`] — single file or directory tree (`.yml` / `.yaml`,
//!   excluding `*.test.yml` / `*.test.yaml` fixtures).
//! - [`RuleSet::from_std`] — `@std/<name>` namespace lookup (M8 stub; the
//!   real `dq-lint` wiring lands in §4.4 of `add-exec-engine/tasks.md`).

use camino::{Utf8Path, Utf8PathBuf};
use serde::Deserialize;
use serde_norway::Value as YamlValue;

use crate::error::{ExecError, Result};
use crate::rule::{Check, CheckParseError, Rule};

/// Provenance marker for a [`RuleSet`].
///
/// Recorded at construction time so the loader's de-duplication step and
/// the CLI's `rules list` / `dq explain` output can attribute each rule
/// back to its origin.
#[derive(Debug, Clone)]
pub enum RuleSource {
    /// Embedded standard ruleset — the `&'static str` is the namespace
    /// name (`"k8s"`, `"dockerfile"`, …) without the `@std/` prefix.
    Std(&'static str),
    /// A rule file or directory on disk.
    Local(Utf8PathBuf),
    /// Inline YAML supplied via `--inline` or a CLI argument.
    Inline,
}

/// A vector of rules with a [`RuleSource`] provenance tag.
///
/// `RuleSet` does no jq compilation by itself — that's the evaluator's
/// job. Construction is cheap enough that the loader can throw away
/// candidates that don't apply to the current run.
#[derive(Debug, Clone)]
pub struct RuleSet {
    /// Where the rules came from.
    pub source: RuleSource,
    /// Parsed rules, in source order.
    pub rules: Vec<Rule>,
}

/// File extension allow-list for `RuleSet::from_path` directory walks.
///
/// `.yml` / `.yaml` are accepted; the suffix-based exclusion list at
/// [`TEST_FIXTURE_SUFFIXES`] keeps `*.test.yml` fixtures out of the
/// ruleset.
const RULE_FILE_EXTENSIONS: &[&str] = &["yml", "yaml"];

/// Suffixes that mark a file as a `dq test` fixture rather than a rule.
///
/// `RuleSet::from_path(<dir>)` skips any file whose name ends with one of
/// these. The fixtures travel with the rules in `dq-lint`'s embedding
/// table; the rule loader must not parse them as rules.
const TEST_FIXTURE_SUFFIXES: &[&str] = &[".test.yml", ".test.yaml"];

impl RuleSet {
    /// Parse `yaml` as a single rule **or** a `---`-separated YAML stream.
    ///
    /// `serde_norway::Deserializer::from_str` walks the document stream; each
    /// document is deserialized as one [`Rule`]. The resulting `RuleSet`
    /// records `source` verbatim (callers usually pass [`RuleSource::Inline`]
    /// or [`RuleSource::Std`]).
    ///
    /// ## Sentinel-recovered structured errors (M11 Phase 3)
    ///
    /// `Deserialize for Check` cannot construct
    /// [`ExecError::CheckMutuallyExclusive`] / [`ExecError::CheckMissing`]
    /// / [`ExecError::CompositeIncomplete`] directly because those need
    /// the rule id and the `Deserialize` impl runs while the id is still
    /// being parsed. Instead, the impl encodes the structured payload
    /// into a sentinel string and emits it through
    /// `serde::de::Error::custom`. This loader runs the typed
    /// deserialize first; on a sentinel-encoded failure it re-parses
    /// the same document as a [`YamlValue`] (which allows it to recover
    /// the rule id) and maps the error to the matching [`ExecError`]
    /// variant. Non-sentinel errors fall through to [`ExecError::Parse`]
    /// unchanged.
    pub fn from_str(yaml: &str, source: RuleSource) -> Result<Self> {
        let mut rules = Vec::new();
        // Collect document spans in two parallel passes: the typed
        // deserialize consumes the Deserializer iterator, so we need
        // to re-iterate when recovering ids on failure. The
        // serde_norway `Deserializer::from_str` borrows the YAML text and
        // re-iteration is cheap.
        for de in serde_norway::Deserializer::from_str(yaml) {
            let parsed: std::result::Result<Rule, serde_norway::Error> = Rule::deserialize(de);
            match parsed {
                Ok(rule) => rules.push(rule),
                Err(err) => {
                    let rule_index = rules.len() + 1;
                    let rule_id = recover_rule_id_at_index(yaml, rule_index);
                    return Err(map_rule_parse_error(err, rule_id, rule_index));
                }
            }
        }
        exact_schemas(yaml, &mut rules);
        Ok(Self { source, rules })
    }

    /// Load rules from a file or a directory tree.
    ///
    /// Behaviour:
    ///
    /// - When `path` is a file: parse it as a rule (or rule stream).
    /// - When `path` is a directory: walk recursively, picking up every
    ///   `.yml` / `.yaml` file whose name does **not** end with
    ///   `.test.yml` or `.test.yaml`. Each file is parsed independently
    ///   and the rules are concatenated. Directory traversal order
    ///   matches `walkdir`'s default (sort-stable for reproducible
    ///   loads).
    pub fn from_path(path: &Utf8Path) -> Result<Self> {
        let metadata = std::fs::metadata(path).map_err(|err| ExecError::Io {
            path: path.to_path_buf(),
            source: err,
        })?;

        if metadata.is_file() {
            let yaml = std::fs::read_to_string(path).map_err(|err| ExecError::Io {
                path: path.to_path_buf(),
                source: err,
            })?;
            return Self::from_str(&yaml, RuleSource::Local(path.to_path_buf()));
        }

        let mut rules = Vec::new();
        // walkdir default ordering is stable across runs but not sorted —
        // collect entries first, sort by path, then parse so the loader's
        // de-duplication step in §3.3 sees a deterministic order.
        let mut entries: Vec<Utf8PathBuf> = Vec::new();
        for entry in walkdir::WalkDir::new(path.as_std_path()).sort_by_file_name() {
            let entry = entry.map_err(|err| ExecError::Io {
                path: path.to_path_buf(),
                source: err.into(),
            })?;
            if !entry.file_type().is_file() {
                continue;
            }
            let entry_path = entry.path();
            let Ok(utf8) = Utf8PathBuf::from_path_buf(entry_path.to_path_buf()) else {
                // A non-UTF-8 path under a rules directory is an
                // operational anomaly — surface it through the existing
                // Io variant so the user sees the offending path.
                return Err(ExecError::Io {
                    path: path.to_path_buf(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("non-UTF-8 path in rules directory: {entry_path:?}"),
                    ),
                });
            };
            if !is_rule_file(&utf8) {
                continue;
            }
            entries.push(utf8);
        }

        for entry in &entries {
            let yaml = std::fs::read_to_string(entry).map_err(|err| ExecError::Io {
                path: entry.clone(),
                source: err,
            })?;
            let one = Self::from_str(&yaml, RuleSource::Local(entry.clone()))?;
            rules.extend(one.rules);
        }

        Ok(Self {
            source: RuleSource::Local(path.to_path_buf()),
            rules,
        })
    }

    /// Resolve `name` against the embedded `@std/<name>` ruleset table.
    ///
    /// Looks up the namespace in the `dq-lint` crate's embedding table.
    /// On unknown namespaces, returns [`ExecError::UnknownRule`] with an
    /// empty `did_you_mean` (the loader attaches typo suggestions because
    /// it owns the candidate set).
    ///
    /// The `&'static str` for [`RuleSource::Std`] is recovered by finding
    /// the matching entry in `dq_lint::list_std_rulesets()` — that slice
    /// already holds `'static` references, so no allocation or
    /// `Box::leak` is required.
    pub fn from_std(name: &str) -> Result<Self> {
        let static_name: &'static str = dq_lint::list_std_rulesets()
            .iter()
            .find(|n| **n == name)
            .copied()
            .ok_or_else(|| ExecError::UnknownRule {
                id: format!("@std/{name}"),
                did_you_mean: Vec::new(),
            })?;
        let yaml =
            dq_lint::std_ruleset(static_name).expect("namespace verified by list_std_rulesets");
        Self::from_str(yaml, RuleSource::Std(static_name))
    }
}

/// Give every inline `check.schema` of `rules` — parsed from `yaml` by
/// `serde_norway`, which rounds a float to an `f64`, reads `1e400` as a
/// string and refuses an integer past `u128` — the value dq's own YAML
/// reader reads there, every number exact (see
/// [`crate::rule::Check::Schema`]).
///
/// The YAML reader sees the same documents as `serde_norway`; should the
/// two ever disagree on how many there are, or where a schema sits, the
/// rule keeps the schema `serde_norway` read.
///
/// A number keeps the rule's spelling too (`0.00000000001`, `2.50`), so a
/// message prints a limit as the rule writes it: see [`Literals`].
fn exact_schemas(yaml: &str, rules: &mut [Rule]) {
    if !rules.iter().any(has_inline_schema) {
        return;
    }
    let Some(format) = dq_core::by_name("yaml") else {
        return;
    };
    let doc = match format.parse(yaml.as_bytes()) {
        Ok(doc) => doc,
        Err(err) => {
            tracing::debug!(error = %err, "rule schemas keep serde_norway's numbers");
            return;
        }
    };
    let documents = doc
        .values()
        .unwrap_or_else(|| std::slice::from_ref(doc.value()));
    if documents.len() != rules.len() {
        tracing::debug!(
            documents = documents.len(),
            rules = rules.len(),
            "rule schemas keep serde_norway's numbers"
        );
        return;
    }
    let spans = dq_core::parse_yaml_with_spans(yaml.as_bytes()).ok();
    let literals = spans.as_ref().map(|doc| Literals { yaml, doc });
    for (index, (rule, document)) in rules.iter_mut().zip(documents).enumerate() {
        // The span parse keys a stream's documents `/N/...`, a lone one `/...`.
        let mut at = dq_core::Pointer::default();
        if documents.len() > 1 {
            at.push_segment(dq_core::Segment::Index(index));
        }
        exact_schema(rule, document, literals.as_ref(), &mut at);
    }
}

/// The text of every number in a rule file, by where it sits.
struct Literals<'a> {
    yaml: &'a str,
    /// The rule file as the span parse reads it.
    doc: &'a dq_core::Document,
}

impl Literals<'_> {
    /// `json`, a schema read from the rule file at `at`, with every number
    /// spelled as the file spells it — when that text is a JSON number of
    /// the same value. Any other text (`+5`, `.5`, `0x10`) keeps the
    /// number's own spelling.
    fn respell(&self, json: &mut serde_json::Value, at: &mut dq_core::Pointer) {
        match json {
            serde_json::Value::Number(number) => {
                let Some(text) = self
                    .doc
                    .span_at(at)
                    .and_then(|span| self.yaml.get(span.value_range.clone()))
                else {
                    return;
                };
                let Ok(literal) = text.parse::<serde_json::Number>() else {
                    return;
                };
                let read =
                    dq_core::Value::from_serde_json(&serde_json::Value::Number(literal.clone()));
                let held =
                    dq_core::Value::from_serde_json(&serde_json::Value::Number(number.clone()));
                if read.same_number(&held) {
                    *number = literal;
                }
            }
            serde_json::Value::Array(items) => {
                for (index, item) in items.iter_mut().enumerate() {
                    at.push_segment(dq_core::Segment::Index(index));
                    self.respell(item, at);
                    at.pop_segment();
                }
            }
            serde_json::Value::Object(members) => {
                for (key, member) in members.iter_mut() {
                    at.push_segment(dq_core::Segment::Key(key.clone()));
                    self.respell(member, at);
                    at.pop_segment();
                }
            }
            _ => {}
        }
    }
}

/// Whether `rule`, or a rule `nested:` in it, has an inline schema.
fn has_inline_schema(rule: &Rule) -> bool {
    match &rule.check {
        Check::Schema { .. } => true,
        Check::Composite { nested, .. } => has_inline_schema(nested),
        _ => false,
    }
}

/// [`exact_schemas`] for one rule, the YAML reader's value of it, and
/// where that value sits in the rule file.
fn exact_schema(
    rule: &mut Rule,
    document: &dq_core::Value,
    literals: Option<&Literals<'_>>,
    at: &mut dq_core::Pointer,
) {
    let Some(check) = member(document, "check") else {
        return;
    };
    at.push_segment(dq_core::Segment::Key("check".to_owned()));
    match &mut rule.check {
        Check::Schema { schema, .. } => {
            if let Some(exact) = member(check, "schema") {
                *schema = crate::schema_value::schema_json(exact);
                if let Some(literals) = literals {
                    at.push_segment(dq_core::Segment::Key("schema".to_owned()));
                    literals.respell(schema, at);
                    at.pop_segment();
                }
            }
        }
        Check::Composite { nested, .. } => {
            if let Some(nested_document) = member(check, "nested") {
                at.push_segment(dq_core::Segment::Key("nested".to_owned()));
                exact_schema(nested, nested_document, literals, at);
                at.pop_segment();
            }
        }
        _ => {}
    }
    at.pop_segment();
}

/// The member `key` of a map value.
fn member<'v>(value: &'v dq_core::Value, key: &str) -> Option<&'v dq_core::Value> {
    match value {
        dq_core::Value::Map(map) => map.get(key),
        _ => None,
    }
}

/// Recover the `id:` field for the rule at `rule_index` (1-based) in
/// the YAML document stream `yaml`.
///
/// Walks the same `serde_norway::Deserializer::from_str` iterator as
/// `from_str` does, materialising each document into a [`YamlValue`]
/// instead of a typed [`Rule`]. Returns `None` when the document is
/// not a mapping, when `id` is missing / non-string, or when the index
/// is out of range. The caller falls back to `"<unknown>"` in that case
/// so the error message still has a placeholder.
fn recover_rule_id_at_index(yaml: &str, rule_index: usize) -> Option<String> {
    for (i, de) in serde_norway::Deserializer::from_str(yaml).enumerate() {
        if i + 1 != rule_index {
            // Skip preceding documents — we only need the failing one.
            // `YamlValue::deserialize` consumes the deserializer
            // unconditionally, which is fine: we ignore the result.
            let _ = YamlValue::deserialize(de);
            continue;
        }
        let value = YamlValue::deserialize(de).ok()?;
        let mapping = value.as_mapping()?;
        let id_value = mapping.get(YamlValue::String("id".to_owned()))?;
        return id_value.as_str().map(str::to_owned);
    }
    None
}

/// Translate a `serde_norway::Error` produced by `Rule::deserialize` into
/// the appropriate [`ExecError`] variant.
///
/// Sentinel-encoded [`CheckParseError`] payloads become structured
/// [`ExecError::CheckMutuallyExclusive`] / [`ExecError::CheckMissing`]
/// / [`ExecError::CompositeIncomplete`] errors. Everything else falls
/// through to [`ExecError::Parse`].
fn map_rule_parse_error(
    err: serde_norway::Error,
    rule_id: Option<String>,
    rule_index: usize,
) -> ExecError {
    let formatted = format!("{err}");
    if let Some(parsed) = extract_sentinel(&formatted) {
        let rule_id = rule_id.unwrap_or_else(|| "<unknown>".to_owned());
        return match parsed {
            CheckParseError::MutuallyExclusive { present_fields } => {
                ExecError::CheckMutuallyExclusive {
                    rule_id,
                    present_fields,
                }
            }
            CheckParseError::Missing => ExecError::CheckMissing { rule_id },
            CheckParseError::CompositeIncomplete { missing_field } => {
                ExecError::CompositeIncomplete {
                    rule_id,
                    missing_field,
                }
            }
        };
    }
    ExecError::Parse {
        hint: format!("rule #{rule_index} failed to parse: {err}"),
        source: err,
    }
}

/// Scan a formatted serde error message for the `dq:check-error:` sentinel
/// emitted by `Deserialize for Check`. Returns the structured payload
/// if found, otherwise `None`.
fn extract_sentinel(formatted: &str) -> Option<CheckParseError> {
    let idx = formatted.find(CheckParseError::SENTINEL_PREFIX)?;
    let tail = &formatted[idx..];
    // The sentinel runs until the next whitespace character — serde
    // typically appends source-position info on the same line after a
    // space, so we cut there.
    let end = tail.find(['\n', ' ']).unwrap_or(tail.len());
    CheckParseError::from_sentinel(&tail[..end])
}

/// Returns `true` for `*.yml` / `*.yaml` files that aren't `*.test.yml`
/// / `*.test.yaml` fixtures.
fn is_rule_file(path: &Utf8Path) -> bool {
    let Some(file_name) = path.file_name() else {
        return false;
    };
    if TEST_FIXTURE_SUFFIXES
        .iter()
        .any(|suffix| file_name.ends_with(suffix))
    {
        return false;
    }
    let Some(extension) = path.extension() else {
        return false;
    };
    RULE_FILE_EXTENSIONS.contains(&extension)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    /// Three minimal rules used by stream-parsing and directory-walk
    /// tests — kept as one constant so each test reads short.
    const RULE_A: &str = "id: a.one\ndescription: A1\nseverity: error\nmatch:\n  format: yaml\ncheck:\n  jq: '.'\n  message: m\n";
    const RULE_B: &str = "id: a.two\ndescription: A2\nseverity: warn\nmatch:\n  format: json\ncheck:\n  jq: '.'\n  message: m\n";
    const RULE_C: &str = "id: a.three\ndescription: A3\nseverity: info\nmatch:\n  format: yaml\ncheck:\n  jq: '.'\n  message: m\n";

    #[test]
    fn from_str_parses_single_rule() {
        let set = RuleSet::from_str(RULE_A, RuleSource::Inline).expect("parse single rule");
        assert_eq!(set.rules.len(), 1);
        assert_eq!(set.rules[0].id, "a.one");
        assert!(matches!(set.source, RuleSource::Inline));
    }

    #[test]
    fn from_str_keeps_the_exact_numbers_of_a_schema() {
        let stream = format!(
            "{RULE_A}---\n\
             id: n.one\ndescription: x\nseverity: error\nmatch: {{ format: yaml }}\n\
             check:\n  schema: {{ const: 18446744073709551616, maximum: 1e400, \
             minimum: 0.10000000000000000001, enum: [.inf], multipleOf: 2.50 }}\n---\n\
             id: n.two\ndescription: x\nseverity: error\nmatch: {{ format: yaml }}\n\
             check:\n  extract: '[]'\n  message: m\n  nested:\n    id: n.inner\n    \
             description: x\n    severity: error\n    match: {{ format: json }}\n    \
             check:\n      schema: {{ const: 340282366920938463463374607431768211456, \
             items: [{{ maximum: 0.00000000001 }}, {{ minimum: +5 }}] }}\n"
        );
        let set = RuleSet::from_str(&stream, RuleSource::Inline).expect("parse rule stream");
        let Check::Schema { schema, .. } = &set.rules[1].check else {
            panic!("expected a schema check: {:?}", set.rules[1].check);
        };
        assert_eq!(
            schema.to_string(),
            r#"{"const":18446744073709551616,"maximum":1e+400,"minimum":0.10000000000000000001,"enum":["\u0000dq.inf\u0000"],"multipleOf":2.50}"#
        );
        let Check::Composite { nested, .. } = &set.rules[2].check else {
            panic!("expected a composite check: {:?}", set.rules[2].check);
        };
        let Check::Schema { schema, .. } = &nested.check else {
            panic!("expected a nested schema check: {:?}", nested.check);
        };
        assert_eq!(
            schema.to_string(),
            r#"{"const":340282366920938463463374607431768211456,"items":[{"maximum":0.00000000001},{"minimum":5}]}"#
        );
    }

    #[test]
    fn from_str_parses_three_rule_stream() {
        let stream = format!("{RULE_A}---\n{RULE_B}---\n{RULE_C}");
        let set = RuleSet::from_str(&stream, RuleSource::Inline).expect("parse rule stream");
        let ids: Vec<&str> = set.rules.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["a.one", "a.two", "a.three"]);
    }

    #[test]
    fn from_str_propagates_parse_error_with_hint() {
        // A rule with an unknown top-level field — `deny_unknown_fields`
        // turns this into a `serde_norway::Error` that `from_str` wraps in
        // the `Parse` variant. The `hint` field must surface the rule
        // index so authors with a 40-rule file know which entry failed.
        let bad = format!("{RULE_A}---\n{RULE_B}---\nbogus_field: 1\n");
        let err = RuleSet::from_str(&bad, RuleSource::Inline)
            .expect_err("parse should fail on bogus_field");
        match &err {
            ExecError::Parse { hint, .. } => {
                assert!(
                    hint.contains("rule #3"),
                    "hint must mention which rule failed, got: {hint}",
                );
            }
            other => panic!("expected ExecError::Parse, got {other:?}"),
        }
    }

    #[test]
    fn from_path_reads_single_file() {
        let temp = TempDir::new().expect("tempdir");
        let path =
            Utf8PathBuf::from_path_buf(temp.path().join("rule.yml")).expect("temp path is UTF-8");
        std::fs::write(&path, RULE_A).expect("write rule file");

        let set = RuleSet::from_path(&path).expect("load single-file ruleset");
        assert_eq!(set.rules.len(), 1);
        assert_eq!(set.rules[0].id, "a.one");
        match &set.source {
            RuleSource::Local(p) => assert_eq!(p, &path),
            other => panic!("expected Local source, got {other:?}"),
        }
    }

    #[test]
    fn from_path_walks_directory_excluding_test_fixtures() {
        let temp = TempDir::new().expect("tempdir");
        let dir = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("UTF-8 temp path");

        // Two real rules + one test fixture (must be excluded).
        std::fs::write(dir.join("alpha.yml"), RULE_A).expect("write alpha");
        std::fs::write(dir.join("beta.yaml"), RULE_B).expect("write beta");
        std::fs::write(dir.join("alpha.test.yml"), "tests: []\n").expect("write fixture");

        let set = RuleSet::from_path(&dir).expect("load directory ruleset");
        let ids: Vec<&str> = set.rules.iter().map(|r| r.id.as_str()).collect();
        // walkdir's `sort_by_file_name` orders alpha.yml before beta.yaml,
        // and the .test.yml file is filtered out before parsing.
        assert_eq!(ids, vec!["a.one", "a.two"]);
    }

    #[test]
    fn from_path_walks_nested_subdirectories() {
        let temp = TempDir::new().expect("tempdir");
        let dir = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("UTF-8 temp path");

        let nested = dir.join("nested");
        std::fs::create_dir_all(&nested).expect("create nested");
        std::fs::write(dir.join("top.yml"), RULE_A).expect("write top");
        std::fs::write(nested.join("inner.yml"), RULE_B).expect("write inner");

        let set = RuleSet::from_path(&dir).expect("load nested ruleset");
        assert_eq!(set.rules.len(), 2);
        let ids: Vec<&str> = set.rules.iter().map(|r| r.id.as_str()).collect();
        // sort_by_file_name orders nested/inner.yml after top.yml because
        // the walk descends after emitting siblings; we just assert both
        // are present.
        assert!(ids.contains(&"a.one"), "expected a.one in {ids:?}");
        assert!(ids.contains(&"a.two"), "expected a.two in {ids:?}");
    }

    #[test]
    fn from_path_returns_io_error_for_missing_file() {
        let temp = TempDir::new().expect("tempdir");
        let path =
            Utf8PathBuf::from_path_buf(temp.path().join("missing.yml")).expect("UTF-8 temp path");
        let err = RuleSet::from_path(&path).expect_err("expected IO error");
        match err {
            ExecError::Io { path: p, .. } => {
                assert_eq!(p, path);
            }
            other => panic!("expected ExecError::Io, got {other:?}"),
        }
    }

    #[test]
    fn from_std_loads_known_namespace_from_dq_lint() {
        let set = RuleSet::from_std("k8s").expect("k8s namespace must resolve");
        assert!(
            !set.rules.is_empty(),
            "expected at least one rule in the k8s std ruleset",
        );
        match set.source {
            RuleSource::Std(ns) => assert_eq!(ns, "k8s"),
            other => panic!("expected RuleSource::Std(\"k8s\"), got {other:?}"),
        }
    }

    #[test]
    fn from_std_returns_unknown_rule_for_missing_namespace() {
        // The loader owns the did_you_mean payload; `from_std` returns
        // an empty list and lets the caller attach suggestions.
        let err = RuleSet::from_std("nope").expect_err("from_std must return UnknownRule");
        match err {
            ExecError::UnknownRule { id, did_you_mean } => {
                assert_eq!(id, "@std/nope");
                assert!(did_you_mean.is_empty());
            }
            other => panic!("expected ExecError::UnknownRule, got {other:?}"),
        }
    }

    #[test]
    fn is_rule_file_accepts_yml_and_yaml_excludes_test_fixtures() {
        // Direct unit test of the helper. Each branch corresponds to a
        // case that the directory walk above relies on; testing them in
        // isolation makes regressions point at the helper rather than the
        // walker.
        assert!(is_rule_file(Utf8Path::new("rule.yml")));
        assert!(is_rule_file(Utf8Path::new("rule.yaml")));
        assert!(!is_rule_file(Utf8Path::new("rule.test.yml")));
        assert!(!is_rule_file(Utf8Path::new("rule.test.yaml")));
        assert!(!is_rule_file(Utf8Path::new("README.md")));
        assert!(!is_rule_file(Utf8Path::new("no-extension")));
    }
}
