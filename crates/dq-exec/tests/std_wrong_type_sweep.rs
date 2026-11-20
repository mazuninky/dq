//! Wrong-type sweep over the embedded `@std` rules: no rule may raise on a
//! document whose fields hold a value of an unexpected type.
//!
//! For every `@std` namespace and every case of every embedded
//! `*.test.yml` fixture, the sweep reads the case's `input:` with dq's own
//! reader for the case's format (as `RuleTester::run_std` does: the case's
//! `format:`, else the first `match.format` of the fixture's rule), then,
//! at every pointer of the document — root included — swaps the node for
//! each of five replacement kinds: a string (`"x"`), a number (`1`),
//! `null`, an empty array and an empty map. A replacement of the kind the
//! node already has (`"x"` for a string) is skipped: it is not a wrong
//! type. Every mutated document goes through [`Evaluator::evaluate`] with
//! every rule of the namespace, and every [`dq_exec::RuleError`] is
//! collected.
//!
//! The mutation is made on the reader's value tree (the IR the evaluator
//! takes), for every format alike: for YAML and JSON it is the tree the
//! reader would produce for the edited text; for Markdown (the CommonMark
//! AST), HCL and Dockerfile (the fixed instruction-list shape) it may be a
//! tree no text spells — a rule reads the IR, and must not raise on any of
//! its shapes.
//!
//! Identical inputs are swept once per namespace. Raises are grouped by
//! (rule, stage, pointer pattern — array indices as `*` —, jq error with
//! the replacement spelled `<value>`); a group lists the replacement kinds
//! that hit it, a count, and the first case that hit it, to reproduce it.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};

use camino::Utf8PathBuf;
use dq_core::{Document, Ir, Segment, Value};
use dq_exec::{Evaluator, RuleSet, RuleSource, RuleTestFile};

/// What replaces a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    String,
    Number,
    Null,
    Array,
    Map,
}

impl Kind {
    const ALL: [Self; 5] = [
        Self::String,
        Self::Number,
        Self::Null,
        Self::Array,
        Self::Map,
    ];

    fn value(self) -> Value {
        match self {
            Self::String => Value::String("x".to_owned()),
            Self::Number => Value::Int(1),
            Self::Null => Value::Null,
            Self::Array => Value::Array(Vec::new()),
            Self::Map => Value::Map(indexmap::IndexMap::new()),
        }
    }

    /// `true` when `node` already has this kind — the replacement would not
    /// be a wrong type.
    fn is_kind_of(self, node: &Value) -> bool {
        matches!(
            (self, node),
            (Self::String, Value::String(_))
                | (
                    Self::Number,
                    Value::Int(_) | Value::BigInt(_) | Value::Float(_) | Value::BigFloat(_)
                )
                | (Self::Null, Value::Null)
                | (Self::Array, Value::Array(_))
                | (Self::Map, Value::Map(_))
        )
    }

    fn name(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Number => "number",
            Self::Null => "null",
            Self::Array => "array",
            Self::Map => "map",
        }
    }
}

/// One group of raises.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    rule: String,
    stage: &'static str,
    pointer: String,
    error: String,
}

#[derive(Debug)]
struct Hit {
    kinds: BTreeSet<Kind>,
    count: usize,
    /// The first case (in namespace, fixture, case order) that hit the
    /// group: its order and `<fixture> :: <case name>`. Kept by order, not
    /// by which worker merged first, so the report is deterministic.
    example: (usize, String),
}

#[derive(Debug, Default)]
struct Sweep {
    hits: BTreeMap<Key, Hit>,
    /// Distinct inputs swept.
    inputs: usize,
    /// Mutated documents evaluated.
    documents: usize,
    /// Cases that could not be read — a fixture bug, reported as such.
    unreadable: Vec<String>,
}

impl Sweep {
    fn merge(&mut self, other: Self) {
        self.inputs += other.inputs;
        self.documents += other.documents;
        self.unreadable.extend(other.unreadable);
        for (key, hit) in other.hits {
            match self.hits.get_mut(&key) {
                Some(mine) => {
                    mine.kinds.extend(hit.kinds);
                    mine.count += hit.count;
                    mine.example = mine.example.clone().min(hit.example);
                }
                None => {
                    self.hits.insert(key, hit);
                }
            }
        }
    }

    /// The raises, one line per group, grouped by rule, with a count per
    /// rule and per namespace (the rule id's prefix before the first `.`).
    fn report(&self) -> String {
        let mut by_rule: BTreeMap<&str, Vec<(&Key, &Hit)>> = BTreeMap::new();
        for (key, hit) in &self.hits {
            by_rule.entry(&key.rule).or_default().push((key, hit));
        }
        let mut by_namespace: BTreeMap<&str, (usize, usize, usize)> = BTreeMap::new();
        for (rule, groups) in &by_rule {
            let namespace = rule.split('.').next().unwrap_or(rule);
            let entry = by_namespace.entry(namespace).or_default();
            entry.0 += 1;
            entry.1 += groups.len();
            entry.2 += groups.iter().map(|(_, hit)| hit.count).sum::<usize>();
        }
        let raises: usize = self.hits.values().map(|hit| hit.count).sum();
        let mut out = String::new();
        let _ = writeln!(
            out,
            "{raises} raises in {} groups by {} rules, over {} mutated documents from {} inputs",
            self.hits.len(),
            by_rule.len(),
            self.documents,
            self.inputs,
        );
        for (namespace, (rules, groups, count)) in &by_namespace {
            let _ = writeln!(
                out,
                "  {namespace}: {rules} rules, {groups} groups, {count} raises"
            );
        }
        for (rule, groups) in &by_rule {
            let count: usize = groups.iter().map(|(_, hit)| hit.count).sum();
            let _ = writeln!(out, "\n{rule} ({} groups, {count} raises)", groups.len());
            for (key, hit) in groups {
                let kinds: Vec<&str> = hit.kinds.iter().map(|kind| kind.name()).collect();
                let pointer = if key.pointer.is_empty() {
                    "(root)"
                } else {
                    &key.pointer
                };
                let _ = writeln!(
                    out,
                    "  {} {pointer} [{}] x{}: {}  (e.g. {})",
                    key.stage,
                    kinds.join(","),
                    hit.count,
                    key.error,
                    hit.example.1,
                );
            }
        }
        for case in &self.unreadable {
            let _ = writeln!(out, "unreadable fixture case: {case}");
        }
        out
    }
}

/// The pointer with every array index spelled `*`.
fn pointer_pattern(segments: &[Segment]) -> String {
    let mut out = String::new();
    for segment in segments {
        out.push('/');
        match segment {
            Segment::Index(_) => out.push('*'),
            Segment::Key(key) => out.push_str(&key.replace('~', "~0").replace('/', "~1")),
        }
    }
    out
}

/// The node at `segments` (an enumerated, hence existing, pointer).
fn node_mut<'a>(mut value: &'a mut Value, segments: &[Segment]) -> &'a mut Value {
    for segment in segments {
        value = match (value, segment) {
            (Value::Map(map), Segment::Key(key)) => map.get_mut(key).expect("enumerated key"),
            (Value::Array(items), Segment::Index(index)) => {
                items.get_mut(*index).expect("enumerated index")
            }
            (other, segment) => panic!("enumerated pointer {segment:?} misses {other:?}"),
        };
    }
    value
}

/// The jq error as a group key: the replacement value spelled `<value>`
/// (so `cannot index "x" with "spec"` and `cannot index 1 with "spec"` are
/// one group listing both kinds), the other operand of an arithmetic error
/// spelled `<n>` (`cannot calculate 3 - null` and `… 5 - null` differ only
/// by a line number of the input), and the whole cut to a length a report
/// line can carry — an error that quotes a document would print it all.
fn error_key(error: &str, kind: Kind) -> String {
    const MAX: usize = 160;
    let spelling = match kind {
        Kind::String => "\"x\"",
        Kind::Number => "1",
        Kind::Null => "null",
        Kind::Array => "[]",
        Kind::Map => "{}",
    };
    let arithmetic = error.contains("cannot calculate ");
    let mut replaced = false;
    let tokens: Vec<&str> = error
        .split(' ')
        .map(|token| {
            if !replaced && token == spelling {
                replaced = true;
                "<value>"
            } else if arithmetic && token.parse::<f64>().is_ok() {
                "<n>"
            } else {
                token
            }
        })
        .collect();
    let error = tokens.join(" ");
    match error.char_indices().nth(MAX) {
        Some((cut, _)) => format!("{}…", &error[..cut]),
        None => error,
    }
}

/// One distinct fixture input of a namespace, read.
struct Case {
    /// Position in (namespace, fixture, case) order.
    order: usize,
    namespace: usize,
    path: Utf8PathBuf,
    format: String,
    doc: Document,
    /// `<fixture> :: <case name>`.
    example: String,
}

/// Sweep every mutation of `case.doc` through `evaluator`.
fn sweep_case(evaluator: &Evaluator, case: &Case, sweep: &mut Sweep) {
    let ir = case.doc.as_ir();
    let root = ir.value();
    sweep.inputs += 1;
    for (pointer, _) in dq_core::enumerate_pointers(&case.doc) {
        let segments = pointer.segments();
        for kind in Kind::ALL {
            let mut mutated = root.clone();
            let node = node_mut(&mut mutated, segments);
            if kind.is_kind_of(node) {
                continue;
            }
            *node = kind.value();
            let mutated_ir = Ir::new(&mutated, ir.provenance(), ir.format());
            let evaluation = evaluator.evaluate(&case.path, &mutated_ir, &case.format);
            sweep.documents += 1;
            for error in evaluation.rule_errors {
                let key = Key {
                    rule: error.rule_id,
                    stage: error.stage.field(),
                    pointer: pointer_pattern(segments),
                    error: error_key(&error.error, kind),
                };
                let hit = sweep.hits.entry(key).or_insert_with(|| Hit {
                    kinds: BTreeSet::new(),
                    count: 0,
                    example: (case.order, case.example.clone()),
                });
                hit.kinds.insert(kind);
                hit.count += 1;
            }
        }
    }
}

/// Read a case's `input:` with dq's reader for `format`, one document.
fn read_input(input: &str, format: &str) -> Result<Document, String> {
    let reader = dq_core::by_name(format).ok_or_else(|| format!("no reader for {format}"))?;
    let doc = reader
        .parse(input.as_bytes())
        .map_err(|err| format!("{format} parse error: {err}"))?;
    if doc.is_multi() {
        return Err(format!("{format} input holds more than one document"));
    }
    Ok(doc)
}

/// Read every distinct fixture input of `@std/<namespace>` (`namespace`
/// is its position in `namespaces`), in fixture-file and case order, with
/// the format `RuleTester::run_std` reads it in. Inputs that do not read
/// go to `unreadable`.
fn read_cases(
    namespaces: &[&'static str],
    namespace: usize,
    cases: &mut Vec<Case>,
    unreadable: &mut Vec<String>,
) {
    let ns = namespaces[namespace];
    let rule_files = dq_lint::std_rule_files(ns).unwrap_or_default();
    let mut fixtures: Vec<(&str, &str)> = dq_lint::std_test_files(ns).unwrap_or_default().to_vec();
    fixtures.sort_unstable_by_key(|(name, _)| *name);
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for (name, text) in fixtures {
        let path = Utf8PathBuf::from(format!("@std/{ns}/{name}"));
        let rule_name = name.replace(".test.yml", ".yml");
        let rule_text = rule_files
            .iter()
            .find(|(file, _)| *file == rule_name)
            .map(|(_, text)| *text)
            .unwrap_or_else(|| panic!("{path}: no sibling rule {rule_name}"));
        let rule = RuleSet::from_str(rule_text, RuleSource::Std(ns))
            .unwrap_or_else(|err| panic!("{path}: sibling rule loads: {err}"));
        let default_format = rule
            .rules
            .first()
            .and_then(|rule| rule.match_.format.first())
            .cloned()
            .unwrap_or_else(|| "yaml".to_owned());
        let file: RuleTestFile = serde_norway::from_str(text)
            .unwrap_or_else(|err| panic!("{path}: fixture parses: {err}"));
        for case in file.tests {
            let format = case.format.unwrap_or_else(|| default_format.clone());
            if !seen.insert((format.clone(), case.input.clone())) {
                continue;
            }
            let example = format!("{path} :: {}", case.name);
            match read_input(&case.input, &format) {
                Ok(doc) => cases.push(Case {
                    order: cases.len(),
                    namespace,
                    path: path.clone(),
                    format,
                    doc,
                    example,
                }),
                Err(err) => unreadable.push(format!("{example}: {err}")),
            }
        }
    }
}

/// Every `@std` rule, in every namespace, tolerates a field of the wrong
/// type anywhere in a document its fixtures describe.
///
/// The distinct inputs of all namespaces are shared out between one
/// worker thread per available core; each input is swept through every
/// rule of its namespace. The report does not depend on the scheduling.
#[test]
fn no_std_rule_raises_on_a_wrong_type_field() {
    let namespaces = dq_lint::list_std_rulesets();
    let evaluators: Vec<Evaluator> = namespaces
        .iter()
        .map(|namespace| {
            let ruleset = RuleSet::from_std(namespace).expect("embedded namespace loads");
            Evaluator::new(vec![ruleset]).expect("embedded namespace compiles")
        })
        .collect();
    let mut cases = Vec::new();
    let mut sweep = Sweep::default();
    for namespace in 0..namespaces.len() {
        read_cases(namespaces, namespace, &mut cases, &mut sweep.unreadable);
    }

    let next = AtomicUsize::new(0);
    let workers = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    let sweeps: Vec<Sweep> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut mine = Sweep::default();
                    while let Some(case) = cases.get(next.fetch_add(1, Ordering::Relaxed)) {
                        sweep_case(&evaluators[case.namespace], case, &mut mine);
                    }
                    mine
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("sweep worker panicked"))
            .collect()
    });
    for one in sweeps {
        sweep.merge(one);
    }
    assert!(sweep.documents > 0, "the sweep evaluated no document");
    assert!(
        sweep.hits.is_empty() && sweep.unreadable.is_empty(),
        "@std rules raise on wrong-type fields:\n{}",
        sweep.report(),
    );
}

/// Sweep one inline YAML rule over one YAML input.
fn sweep_inline(rule: &str, input: &str) -> Sweep {
    let ruleset = RuleSet::from_str(rule, RuleSource::Inline).expect("inline rule loads");
    let evaluator = Evaluator::new(vec![ruleset]).expect("inline rule compiles");
    let case = Case {
        order: 0,
        namespace: 0,
        path: Utf8PathBuf::from("input.yaml"),
        format: "yaml".to_owned(),
        doc: read_input(input, "yaml").expect("input reads"),
        example: "inline".to_owned(),
    };
    let mut sweep = Sweep::default();
    sweep_case(&evaluator, &case, &mut sweep);
    sweep
}

const INPUT: &str = "metadata:\n  name: web\n";

/// The sweep catches a rule that raises on a wrong type: one that pipes
/// `.metadata.name` into `ascii_downcase` raises once the name is not a
/// string, and the report names the rule, the stage and the pointer.
#[test]
fn the_sweep_reports_a_rule_that_raises_on_a_wrong_type() {
    let rule = r#"
id: sweep.fragile
description: raises unless .metadata.name is a string
severity: error
match:
  format: yaml
check:
  jq: '.metadata.name | ascii_downcase | select(. == "forbidden") | {}'
  message: forbidden name
"#;
    let sweep = sweep_inline(rule, INPUT);
    let at_name: Vec<(&Key, &Hit)> = sweep
        .hits
        .iter()
        .filter(|(key, _)| {
            key.rule == "sweep.fragile"
                && key.stage == "check.jq"
                && key.pointer == "/metadata/name"
        })
        .collect();
    let kinds: BTreeSet<Kind> = at_name
        .iter()
        .flat_map(|(_, hit)| hit.kinds.iter().copied())
        .collect();
    assert!(
        kinds.contains(&Kind::Number) && kinds.contains(&Kind::Array),
        "expected raises at /metadata/name for a number and an array, got:\n{}",
        sweep.report(),
    );
    assert!(
        !kinds.contains(&Kind::String),
        "a string at /metadata/name is the right type"
    );
    let report = sweep.report();
    assert!(
        report.contains("sweep.fragile (") && report.contains("check.jq /metadata/name ["),
        "report:\n{report}"
    );
}

/// The paired negative: the same check written to tolerate any type
/// raises on none of the mutations the sweep makes.
#[test]
fn the_sweep_passes_a_rule_that_tolerates_wrong_types() {
    let rule = r#"
id: sweep.tolerant
description: reads .metadata.name only when it is a string
severity: error
match:
  format: yaml
check:
  jq: '.metadata?.name? | strings | ascii_downcase | select(. == "forbidden") | {}'
  message: forbidden name
"#;
    let sweep = sweep_inline(rule, INPUT);
    // Root, /metadata and /metadata/name; each takes the four kinds it
    // does not already have.
    assert_eq!(sweep.documents, 12, "mutations evaluated");
    assert!(sweep.hits.is_empty(), "report:\n{}", sweep.report());
}
