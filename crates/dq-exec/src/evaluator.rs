//! Pre-compiled rule runner.
//!
//! [`Evaluator::new`] takes one or more [`crate::ruleset::RuleSet`]s and
//! compiles every rule's `match.filter`, `check.jq`, `loc.pointer`,
//! `loc.file`, and `loc.line` jq expressions, plus its `match.glob`
//! pattern, up-front. [`Evaluator::evaluate_file`] runs the compiled
//! rules against a pre-parsed document IR and emits one [`Diagnostic`]
//! per violation.
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
//! 5. Diagnostic build: render `check.message`, resolve `loc.pointer` /
//!    `loc.file` / `loc.line` overrides, attach severity / references /
//!    fix payload.
//!
//! ## Position metadata
//!
//! Phase 2 of `add-ir-foundation` switched the evaluator to take a
//! borrowed [`dq_core::Ir<'_>`] instead of a `serde_json::Value`. The
//! IR carries the parser's `original_bytes` and a provenance map keyed
//! by canonical RFC 6901 pointer strings. Rules now have a typed
//! `loc.pointer` expression: when it produces a non-empty pointer
//! string, the evaluator looks the pointer up via
//! [`dq_core::Ir::line_col_for`] and resolves `(line, col)` from the
//! source bytes. The legacy `loc.line` path stays as a fallback so M8
//! rules keep working unchanged.
//!
//! ## Robustness
//!
//! Compile-time failures (jq parse errors, glob parse errors) surface as
//! [`crate::error::ExecError`] from [`Evaluator::new`]. Runtime failures
//! during [`Evaluator::evaluate_file`] (a `check.jq` that crashes on a
//! particular input shape, a `loc.line` expression that returns a
//! non-integer, a `loc.pointer` expression that fails to parse) are
//! logged via `tracing::warn!` / `tracing::trace!` and the offending
//! rule falls through to the next step in the chain — the principle is
//! that one badly-written rule must not crash the entire run.

use std::sync::Arc;

use camino::{Utf8Path, Utf8PathBuf};
use dq_core::Pointer;
use globset::GlobMatcher;
use indexmap::IndexSet;

use crate::composite::{
    CompiledCompositeCheck, MAX_EXTRACT_DEPTH, compile_composite, run_composite,
};
use crate::diagnostic::Diagnostic;
use crate::error::{ExecError, Result};
use crate::rule::{Check, Rule};
use crate::ruleset::{RuleSet, RuleSource};
use crate::schema_check::{
    CompiledSchemaCheck, compile_from_embedded, compile_from_file, compile_inline,
};
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
    /// Composite-rule recursion bound (Phase 4 of M11). Defaults to
    /// [`crate::composite::MAX_EXTRACT_DEPTH`] (`= 4`); overridable via
    /// [`Evaluator::with_max_extract_depth`] for unit tests only — the
    /// CLI does not surface this knob.
    max_extract_depth: usize,
}

/// Per-[`Evaluator::new`] cache for compiled jq filters.
///
/// `Evaluator::new` walks every rule's six potential jq-bearing fields
/// (`match.filter`, `loc.pointer`, `loc.file`, `loc.line`, `fix.jq`,
/// `fix.ops`) plus each `check.jq` / composite `extract` and pays
/// `JqEngine::compile` per occurrence. In real rulesets the same
/// expression repeats — `@std/k8s` has ~10 unique `match.filter`s shared
/// across 28 rules — so a per-`Evaluator::new` cache hashed on the
/// expression string deduplicates those compiles down to one
/// `JqEngine` per unique expression, shared through `Arc`.
///
/// ## Invariants
///
/// - **Per-`Evaluator::new`, not global.** The cache is allocated at the
///   start of [`Evaluator::new`] and dropped when it returns; the
///   resulting [`Evaluator`] holds onto its [`Arc<JqEngine>`]s
///   independently. Two consecutive [`Evaluator::new`] calls with the
///   same expression therefore produce *different* `Arc` instances —
///   intentional: a global cache would leak in long-lived processes and
///   would have to thread test isolation through every fixture that
///   pokes at a bad expression on purpose. See
///   [`openspec/changes/perf-jq-compile-cache/design.md`] §3 for the
///   full rationale.
/// - **Exact-string cache key.** No hashing, no whitespace
///   normalisation — `". + 1"` and ".+1" are different keys. The miss
///   cost is one compile; the hit cost is an `Arc::clone` (refcount
///   bump). We do not normalise because two superficially-identical
///   strings could compile against different `defs` chains in the
///   future, and a normalisation step would mask that.
/// - **Concurrent `run()` is safe.** `JqEngine` is `Send + Sync` (the
///   `sync` feature on `jaq-json` swaps the internal `Rc` for `Arc`),
///   so an `Arc<JqEngine>` shared across multiple [`CompiledRule`]s is
///   safe to invoke concurrently from rayon workers.
pub(crate) type JqCache = std::collections::HashMap<String, Arc<JqEngine>>;

/// Look `expr` up in `cache`; compile-on-miss and store; return
/// the (cached-or-fresh) [`Arc<JqEngine>`].
///
/// On compile failure the error is wrapped in [`ExecError::RuleCompile`]
/// tagged with `rule_id` so the caller can surface a useful diagnostic
/// to the user. The cache key is the raw `expr` string; see
/// [`JqCache`] for the invariants this helper enforces.
fn compile_or_cached(cache: &mut JqCache, expr: &str, rule_id: &str) -> Result<Arc<JqEngine>> {
    if let Some(engine) = cache.get(expr) {
        return Ok(Arc::clone(engine));
    }
    let engine = Arc::new(
        JqEngine::compile(expr).map_err(|err| ExecError::RuleCompile {
            rule_id: rule_id.to_string(),
            source: err,
        })?,
    );
    cache.insert(expr.to_string(), Arc::clone(&engine));
    Ok(engine)
}

/// Pre-compiled `check` block, dispatched on at evaluate-time.
///
/// Each variant of [`Check`] turns into the matching variant here:
/// `Jq` carries a [`JqEngine`] and a message template; the schema
/// variants carry a [`CompiledSchemaCheck`] (validator + optional
/// message prefix); `Composite` is a Phase 4 placeholder that emits no
/// diagnostics.
pub(crate) enum CompiledCheck {
    /// Variant 1 — jq-driven check. The legacy path; existing rules
    /// continue to use this.
    Jq {
        /// Compiled jq evaluator for `check.jq`. Shared via [`Arc`] so
        /// two rules with the same `check.jq` expression amortise one
        /// compile — see [`JqCache`].
        engine: Arc<JqEngine>,
        /// Message template — substitution happens via
        /// [`crate::template::render`].
        message: String,
    },
    /// Variants 2 / 3 — JSON Schema 2020-12 (inline or sibling file).
    /// Both compile through [`crate::schema_check`] into the same
    /// [`CompiledSchemaCheck`] shape, so the runtime dispatch arm is
    /// shared.
    Schema(CompiledSchemaCheck),
    /// Variant 4 — composite (Phase 4). Carries a recursively-compiled
    /// inner rule plus the compiled `extract` jq filter; the runtime
    /// arm dispatches into [`crate::composite::run_composite`].
    Composite(Box<CompiledCompositeCheck>),
}

impl std::fmt::Debug for CompiledCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Jq { message, .. } => f
                .debug_struct("Jq")
                .field("message", message)
                .finish_non_exhaustive(),
            Self::Schema(s) => f
                .debug_struct("Schema")
                .field("message_prefix", &s.message_prefix)
                .finish_non_exhaustive(),
            Self::Composite(c) => f
                .debug_struct("Composite")
                .field("message_prefix", &c.message_prefix)
                .finish_non_exhaustive(),
        }
    }
}

/// One rule with all its jq filters and glob matcher pre-compiled.
///
/// Stored behind [`Arc`] in the [`Evaluator`] so cloning the evaluator
/// (e.g. one clone per rayon worker in a future parallel evaluation
/// path) is cheap.
pub(crate) struct CompiledRule {
    pub(crate) rule: Rule,
    pub(crate) filter_engine: Option<Arc<JqEngine>>,
    pub(crate) check: CompiledCheck,
    pub(crate) glob_matcher: Option<GlobMatcher>,
    pub(crate) loc_file_engine: Option<Arc<JqEngine>>,
    pub(crate) loc_line_engine: Option<Arc<JqEngine>>,
    /// Phase 2 of `add-ir-foundation`: pre-compiled `loc.pointer` jq
    /// expression. Populated when the rule's `loc:` block declares a
    /// `pointer:` field. Consumed by [`resolve_loc_position`] which
    /// walks the new `loc.pointer → loc.line → intrinsic` chain.
    pub(crate) loc_pointer_engine: Option<Arc<JqEngine>>,
    /// M10 — pre-compiled `fix.jq` engine. Populated when the rule
    /// declares a `fix:` block with a `jq:` field; consumed by
    /// [`crate::Fixer`] as the legacy whole-document transform path.
    pub(crate) fix_engine: Option<Arc<JqEngine>>,
    /// Phase 4 — pre-compiled `fix.ops` engine. Populated when the rule
    /// declares a `fix:` block with an `ops:` field; consumed by
    /// [`crate::Fixer`] as the per-violation [`dq_core::EditScript`]
    /// vocabulary path. When both `fix.jq` and `fix.ops` are set, the
    /// fixer prefers `fix.ops` and logs a `tracing::warn!` shadowing
    /// notice. See `data-query-exec` Requirement "`Fixer` runtime".
    pub(crate) fix_ops_engine: Option<Arc<JqEngine>>,
}

impl CompiledRule {
    /// Whether the rule declares a `loc.pointer` or `loc.line` override
    /// that should drive the diagnostic's `(line, col)` instead of the
    /// intrinsic span lookup.
    ///
    /// Used by [`run_schema_check`] to decide whether to fall back to
    /// `Ir::line_col_for(instance_path)` when no override is set —
    /// preserving the pre-Phase-2 schema behaviour for rules that did
    /// not opt into the chain.
    pub(crate) fn has_position_override(&self) -> bool {
        self.loc_pointer_engine.is_some() || self.loc_line_engine.is_some()
    }
}

impl std::fmt::Debug for CompiledRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledRule")
            .field("rule_id", &self.rule.id)
            .field("check", &self.check)
            .field("has_filter", &self.filter_engine.is_some())
            .field("has_glob", &self.glob_matcher.is_some())
            .field("has_loc_pointer", &self.loc_pointer_engine.is_some())
            .field("has_loc_file", &self.loc_file_engine.is_some())
            .field("has_loc_line", &self.loc_line_engine.is_some())
            .field("has_fix_jq", &self.fix_engine.is_some())
            .field("has_fix_ops", &self.fix_ops_engine.is_some())
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
        // Per-invocation jq compile cache. Lives only for the duration
        // of this `Evaluator::new` call so that two consecutive calls
        // don't share state (see [`JqCache`] for the full rationale).
        // Rules within a single ruleset OR across rulesets in the same
        // call get deduplication; rules across separate evaluators do
        // not.
        let mut cache: JqCache = JqCache::new();
        for set in rulesets {
            // Each ruleset's source flows into per-rule compilation so
            // schema_file resolution and embedded-schema lookups can
            // pick the right strategy per rule.
            let source = set.source;
            for rule in set.rules {
                compiled.push(Arc::new(compile_rule_to_depth(
                    rule,
                    &source,
                    0,
                    MAX_EXTRACT_DEPTH,
                    &mut cache,
                )?));
            }
        }
        Ok(Self {
            rules: compiled,
            max_extract_depth: MAX_EXTRACT_DEPTH,
        })
    }

    /// Override the composite-rule recursion bound (Phase 4 of M11).
    ///
    /// Intended for unit tests that exercise the depth-exceeded branch
    /// without authoring 4 levels of self-similar composite rules. The
    /// production default is [`crate::composite::MAX_EXTRACT_DEPTH`]
    /// (`= 4`); the CLI does not surface this knob.
    ///
    /// Returns `self` for chaining: `Evaluator::new(...)?.with_max_extract_depth(2)`.
    #[must_use]
    pub fn with_max_extract_depth(mut self, depth: usize) -> Self {
        self.max_extract_depth = depth;
        self
    }

    /// Composite-rule recursion bound currently in effect — used by the
    /// per-evaluate dispatch and exposed for tests that pin the
    /// builder-method override.
    #[must_use]
    pub fn max_extract_depth(&self) -> usize {
        self.max_extract_depth
    }

    /// Run every applicable rule against `ir` and collect the resulting
    /// diagnostics.
    ///
    /// `format_name` is the canonical format name of the parsed document
    /// (e.g. `"yaml"`, `"json"`). Only rules whose `match.format` list
    /// includes that name are considered.
    ///
    /// Phase 2 of `add-ir-foundation` switched the input from
    /// `&serde_json::Value` to a borrowed [`dq_core::Ir<'_>`] so the
    /// evaluator can resolve `loc.pointer` against the input's provenance
    /// map / source bytes via [`dq_core::Ir::line_col_for`]. Internally
    /// the evaluator still feeds jq through `serde_json::Value` (jaq's
    /// native shape), but the borrowed `Ir` carries the metadata needed
    /// for span-aware diagnostic positions without forcing every caller
    /// onto a span-preserving jaq fork.
    #[must_use]
    pub fn evaluate_file(
        &self,
        path: &Utf8Path,
        ir: &dq_core::Ir<'_>,
        format_name: &str,
    ) -> Vec<Diagnostic> {
        // Convert once at the boundary: jaq still consumes
        // `serde_json::Value` (the native shape), so we materialise it
        // here and pass it to every per-rule jq engine. The `Ir` itself
        // is forwarded into per-rule helpers so they retain access to
        // [`dq_core::Ir::line_col_for`] for `loc.pointer` resolution.
        let value = ir.value().to_serde_json();
        let mut diagnostics = Vec::new();
        for rule in &self.rules {
            evaluate_one_rule_at_depth(
                rule,
                path,
                ir,
                &value,
                format_name,
                0,
                self.max_extract_depth,
                &mut diagnostics,
            );
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

/// Compile one [`Rule`] into a [`CompiledRule`] at the given recursion
/// depth.
///
/// `current_depth` is the position of `rule` in the composite chain
/// (0 = top level, 1 = nested inside a top-level composite, …). The
/// helper bumps `current_depth` when descending into a `Check::Composite`
/// `nested` rule and surfaces
/// [`crate::ExecError::CompositeDepthExceeded`] if the chain would
/// exceed `max_depth` at compile time. Production callers (the
/// [`Evaluator::new`] entry point and the recursive
/// [`crate::composite::compile_composite`]) drive this through
/// [`crate::composite::MAX_EXTRACT_DEPTH`]; tests can dial it down via
/// [`Evaluator::with_max_extract_depth`].
pub(crate) fn compile_rule_to_depth(
    rule: Rule,
    source: &RuleSource,
    current_depth: usize,
    max_depth: usize,
    cache: &mut JqCache,
) -> Result<CompiledRule> {
    let filter_engine = match rule.match_.filter.as_deref() {
        Some(expr) => Some(compile_or_cached(cache, expr, &rule.id)?),
        None => None,
    };
    let check = compile_check(&rule, source, current_depth, max_depth, cache)?;
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
    let (loc_pointer_engine, loc_file_engine, loc_line_engine) = match rule.loc.as_ref() {
        Some(loc) => {
            let pointer = match loc.pointer.as_deref() {
                Some(expr) => Some(compile_or_cached(cache, expr, &rule.id)?),
                None => None,
            };
            let file = match loc.file.as_deref() {
                Some(expr) => Some(compile_or_cached(cache, expr, &rule.id)?),
                None => None,
            };
            let line = match loc.line.as_deref() {
                Some(expr) => Some(compile_or_cached(cache, expr, &rule.id)?),
                None => None,
            };
            (pointer, file, line)
        }
        None => (None, None, None),
    };
    // M10 + Phase 4: compile both `fix.jq` and `fix.ops` (each
    // `Option<String>`) up-front so per-file autofix runs don't pay
    // re-compilation cost. Compile-time failures surface here as the
    // same `RuleCompile` shape the lint runtime uses.
    let fix_engine = match rule.fix.as_ref().and_then(|f| f.jq.as_deref()) {
        Some(expr) => Some(compile_or_cached(cache, expr, &rule.id)?),
        None => None,
    };
    let fix_ops_engine = match rule.fix.as_ref().and_then(|f| f.ops.as_deref()) {
        Some(expr) => Some(compile_or_cached(cache, expr, &rule.id)?),
        None => None,
    };

    Ok(CompiledRule {
        rule,
        filter_engine,
        check,
        glob_matcher,
        loc_pointer_engine,
        loc_file_engine,
        loc_line_engine,
        fix_engine,
        fix_ops_engine,
    })
}

/// Compile the `check` block of a rule into a [`CompiledCheck`].
///
/// Routes the schema-bearing variants through [`crate::schema_check`],
/// the jq variant through [`JqEngine::compile`], and the composite
/// variant through [`crate::composite::compile_composite`] which
/// recursively compiles the `nested` rule and bumps the depth counter.
fn compile_check(
    rule: &Rule,
    source: &RuleSource,
    current_depth: usize,
    max_depth: usize,
    cache: &mut JqCache,
) -> Result<CompiledCheck> {
    match &rule.check {
        Check::Jq { jq, message } => {
            let engine = compile_or_cached(cache, jq, &rule.id)?;
            Ok(CompiledCheck::Jq {
                engine,
                message: message.clone(),
            })
        }
        Check::Schema { schema, message } => {
            let compiled = compile_inline(&rule.id, schema, message.clone())?;
            Ok(CompiledCheck::Schema(compiled))
        }
        Check::SchemaFile {
            schema_file,
            message,
        } => {
            // For `@std/<ns>` rulesets, the schema file is embedded in
            // dq-lint and accessed via `dq_lint::std_schema(ns, file)`.
            // For local rules, we resolve the path against the rule
            // directory.
            if let RuleSource::Std(namespace) = source {
                let key = schema_file.as_str();
                // Strip any leading `./` so callers can write the
                // sibling path either way.
                let key = key.strip_prefix("./").unwrap_or(key);
                let text = dq_lint::std_schema(namespace, key).ok_or_else(|| {
                    ExecError::SchemaCompile {
                        rule_id: rule.id.clone(),
                        message: format!(
                            "embedded schema not found in @std/{namespace}: {schema_file}"
                        ),
                    }
                })?;
                let compiled = compile_from_embedded(&rule.id, text, message.clone())?;
                Ok(CompiledCheck::Schema(compiled))
            } else {
                let compiled = compile_from_file(&rule.id, schema_file, source, message.clone())?;
                Ok(CompiledCheck::Schema(compiled))
            }
        }
        Check::Composite {
            extract,
            nested,
            message,
        } => {
            // Phase 4: recursively compile the nested rule and the
            // extract jq expression. Depth bumps inside
            // `compile_composite` so the entire chain is validated up
            // front; depth-exceeded surfaces as
            // `ExecError::CompositeDepthExceeded`.
            let compiled = compile_composite(
                &rule.id,
                extract,
                nested,
                message,
                source,
                current_depth,
                max_depth,
                cache,
            )?;
            Ok(CompiledCheck::Composite(Box::new(compiled)))
        }
    }
}

/// Run the per-rule pipeline against `(path, ir, value, format_name)` and
/// push any resulting diagnostics into `out`. `current_depth` /
/// `max_depth` carry the composite-recursion budget so a `Composite`
/// dispatch can recurse into its `nested` rule and trip the bound
/// without crashing.
///
/// `value` is the result of `ir.value().to_serde_json()` — pre-computed
/// once per file by the caller so each rule's jq engines can reuse it
/// without paying for the conversion N times.
///
/// Crate-internal: [`crate::composite::run_composite`] re-enters this
/// helper on the `nested` rule with `current_depth + 1`, so the composite
/// runtime can pin diagnostics to the inner rule's identity while the
/// outer evaluator keeps the depth budget honest.
#[allow(clippy::too_many_arguments)]
pub(crate) fn evaluate_one_rule_at_depth(
    rule: &CompiledRule,
    path: &Utf8Path,
    ir: &dq_core::Ir<'_>,
    value: &serde_json::Value,
    format_name: &str,
    current_depth: usize,
    max_depth: usize,
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

    // 4. Check eval — dispatch on the compiled check variant.
    match &rule.check {
        CompiledCheck::Jq { engine, message } => {
            let violations = match engine.run(value) {
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
            for violation in &violations {
                out.push(build_jq_diagnostic(rule, path, ir, violation, message));
            }
        }
        CompiledCheck::Schema(compiled) => {
            run_schema_check(rule, compiled, path, ir, value, out);
        }
        CompiledCheck::Composite(compiled) => {
            run_composite(
                rule,
                compiled,
                path,
                ir,
                value,
                current_depth,
                max_depth,
                out,
            );
        }
    }
}

/// Render a diagnostic for one jq-driven violation value.
fn build_jq_diagnostic(
    rule: &CompiledRule,
    path: &Utf8Path,
    ir: &dq_core::Ir<'_>,
    violation: &serde_json::Value,
    message_template: &str,
) -> Diagnostic {
    let message = template::render(message_template, violation);
    let file = resolve_loc_file(rule, path, violation);
    let (line, col) = resolve_loc_position(rule, ir, violation);
    Diagnostic {
        rule_id: rule.rule.id.clone(),
        severity: rule.rule.severity,
        message,
        file,
        line,
        col,
        span: None,
        references: rule.rule.references.clone(),
        fix: rule.rule.fix.clone(),
    }
}

/// Run a JSON Schema check and append one diagnostic per validation
/// error.
///
/// Each error's `instance_path` is parsed as an RFC 6901 [`Pointer`];
/// when the IR has a span for that pointer, the diagnostic's
/// `(line, col)` come from `Ir::line_col_for`. The message format is
/// `<message_prefix><keyword_location>: <error>` so reporters can show
/// both the schema location and the human-readable explanation.
///
/// `loc.file`, `loc.pointer`, and `loc.line` overrides on the rule are
/// honored via the same [`resolve_loc_file`] / [`resolve_loc_position`]
/// helpers that the jq path uses. The `violation` argument passed to
/// those helpers is the JSON value at the schema error's
/// `instance_path` (or `Null` when the pointer is unresolvable), so any
/// jq expressions in the `loc.*` fields run against meaningful context.
fn run_schema_check(
    rule: &CompiledRule,
    compiled: &CompiledSchemaCheck,
    path: &Utf8Path,
    ir: &dq_core::Ir<'_>,
    value: &serde_json::Value,
    out: &mut Vec<Diagnostic>,
) {
    // Dedup key per `run_schema_check` invocation. JSON Schemas may
    // declare the same constraint at multiple schema locations
    // (e.g. a top-level `required` plus an `allOf`/`oneOf` subschema
    // that repeats it), and the validator dutifully yields one error
    // per location. To the user, two errors with the same
    // `(rule_id, instance_path, error_text)` are indistinguishable —
    // collapse them to one diagnostic while preserving emission order.
    //
    // Scope: one invocation only. Never share across files or rules,
    // because two legitimately-distinct rules can produce the same
    // `(instance_path, error_text)` against the same file.
    let mut seen: IndexSet<(String, String, String)> = IndexSet::new();
    for error in compiled.validator.iter_errors(value) {
        let pointer_str = error.instance_path.as_str();
        // The validator's error text (without the schema_path prefix)
        // is what the user sees as the actual violation. Use it for
        // dedup so duplicates at different `schema_path` locations
        // collapse, while distinct violations at the same path
        // (different `error_text`) are preserved.
        let error_text = error.to_string();
        let dedup_key = (
            rule.rule.id.clone(),
            pointer_str.to_owned(),
            error_text.clone(),
        );
        if !seen.insert(dedup_key) {
            continue;
        }

        // Lift the offending sub-value out of the document so any
        // `loc.*` jq expressions run against the same shape an
        // equivalent jq check would have produced. `serde_json::Value::pointer`
        // accepts the same RFC 6901 string the validator produced.
        let violation = value
            .pointer(pointer_str)
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let file = resolve_loc_file(rule, path, &violation);
        let (line, col) = if rule.has_position_override() {
            // The user opted into overrides; trust whatever the chain
            // returned, including the (1, 1) default that signals an
            // override miss.
            resolve_loc_position(rule, ir, &violation)
        } else {
            // No override set — fall back to the intrinsic
            // `instance_path → span` lookup the schema check used
            // before Phase 2.
            match Pointer::parse(pointer_str) {
                Ok(pointer) => ir.line_col_for(&pointer).unwrap_or((1, 1)),
                Err(parse_err) => {
                    tracing::trace!(
                        rule_id = %rule.rule.id,
                        pointer = %pointer_str,
                        error = %parse_err,
                        "schema instance_path failed to parse as RFC 6901; defaulting to (1, 1)",
                    );
                    (1, 1)
                }
            }
        };
        let prefix = compiled.message_prefix.as_deref().unwrap_or("");
        let keyword = error.schema_path.as_str();
        let message = format!("{prefix}{keyword}: {error_text}");
        out.push(Diagnostic {
            rule_id: rule.rule.id.clone(),
            severity: rule.rule.severity,
            message,
            file,
            line,
            col,
            span: None,
            references: rule.rule.references.clone(),
            fix: rule.rule.fix.clone(),
        });
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

/// Resolve the diagnostic's `(line, col)` per the new Phase 2 chain:
/// `loc.pointer` → `loc.line` → intrinsic.
///
/// 1. **`loc.pointer`** (preferred): evaluate the jq expression. If the
///    first output is a non-empty string AND parses as a [`Pointer`] AND
///    [`dq_core::Ir::line_col_for`] resolves to `Some((line, col))`,
///    return it. Any failure (jq runtime error, non-string output, empty
///    string, parse failure, span miss, no source bytes on the IR) falls
///    through to step 2.
/// 2. **`loc.line`** (deprecated, M8 fallback): evaluate the jq
///    expression and coerce the first output to a positive `u32`. On
///    success, return `(line, 1)`. On any failure or out-of-range
///    integer, fall through to step 3. `col` is hard-coded to `1` in the
///    legacy path because `loc.line` was only ever a line override —
///    matching the M8 semantics byte-for-byte.
/// 3. **Default**: `(1, 1)`.
///
/// Each step in the chain emits a `tracing::trace!` describing which
/// branch won — useful for rule authors debugging why their `loc.pointer`
/// did not resolve. Failures inside `loc.pointer` are deliberately *not*
/// `warn!`-level: a missing span is a normal fall-through case (the rule
/// emits a synthesised pointer like `/missing` for a violation that was
/// not present in the source).
fn resolve_loc_position(
    rule: &CompiledRule,
    ir: &dq_core::Ir<'_>,
    violation: &serde_json::Value,
) -> (u32, u32) {
    // Step 1: loc.pointer.
    if let Some(engine) = rule.loc_pointer_engine.as_ref() {
        match engine.run(violation) {
            Ok(out) => {
                if let Some(first) = out.first() {
                    if let serde_json::Value::String(s) = first
                        && !s.is_empty()
                    {
                        match Pointer::parse(s) {
                            Ok(pointer) => match ir.line_col_for(&pointer) {
                                Some((line, col)) => {
                                    tracing::trace!(
                                        rule_id = %rule.rule.id,
                                        pointer = %s,
                                        line,
                                        col,
                                        "loc.pointer resolved span via Ir::line_col_for",
                                    );
                                    return (line, col);
                                }
                                None => {
                                    tracing::trace!(
                                        rule_id = %rule.rule.id,
                                        pointer = %s,
                                        "loc.pointer parsed but no span on Ir; falling through",
                                    );
                                }
                            },
                            Err(err) => {
                                tracing::trace!(
                                    rule_id = %rule.rule.id,
                                    pointer = %s,
                                    error = %err,
                                    "loc.pointer output failed to parse; falling through",
                                );
                            }
                        }
                    } else {
                        tracing::trace!(
                            rule_id = %rule.rule.id,
                            "loc.pointer first output was not a non-empty string; falling through",
                        );
                    }
                } else {
                    tracing::trace!(
                        rule_id = %rule.rule.id,
                        "loc.pointer produced empty output stream; falling through",
                    );
                }
            }
            Err(err) => {
                // Runtime errors here are normal fall-through territory
                // (the violation may not have the field the expression
                // expects). Logged at `trace!` rather than `warn!` to
                // avoid noise; the legacy `loc.line` warning policy is
                // preserved below for backward compatibility.
                tracing::trace!(
                    rule_id = %rule.rule.id,
                    error = %err,
                    "loc.pointer raised a runtime error; falling through",
                );
            }
        }
    }

    // Step 2: loc.line (M8 legacy path).
    if let Some(engine) = rule.loc_line_engine.as_ref() {
        match engine.run(violation) {
            Ok(out) => {
                if let Some(first) = out.first() {
                    if let Some(n) = first.as_u64()
                        && n >= 1
                        && n <= u64::from(u32::MAX)
                    {
                        let line = u32::try_from(n).unwrap_or(1);
                        tracing::trace!(
                            rule_id = %rule.rule.id,
                            line,
                            "loc.line legacy path resolved",
                        );
                        return (line, 1);
                    } else if let Some(n) = first.as_i64()
                        && n >= 1
                        && n <= i64::from(u32::MAX)
                    {
                        let line = u32::try_from(n).unwrap_or(1);
                        tracing::trace!(
                            rule_id = %rule.rule.id,
                            line,
                            "loc.line legacy path resolved",
                        );
                        return (line, 1);
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

    // Step 3: default.
    (1, 1)
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

    /// Build an [`OwnedIr`] for the test path so existing call sites can
    /// keep constructing a `serde_json::Value` and feed the evaluator
    /// through the new Phase 2 IR signature without rewriting every
    /// fixture. The returned `OwnedIr` carries an empty
    /// [`dq_core::ProvenanceMap`] (no spans available — these tests
    /// pre-date span propagation, so `loc.pointer` resolution will
    /// correctly fall through to the legacy `loc.line` / default path).
    fn ir_for_test(value: &serde_json::Value, format: &str) -> dq_core::OwnedIr {
        use dq_core::FormatTag;
        let dq_value = dq_core::Value::from_serde_json(value);
        let format_tag = match format {
            "yaml" => FormatTag::Yaml,
            "json" => FormatTag::Json,
            _ => FormatTag::Json,
        };
        dq_core::OwnedIr::new(dq_value, dq_core::ProvenanceMap::new(), format_tag)
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
        let owned = ir_for_test(&value, "yaml");
        let diags = eval.evaluate_file(&path, &owned.to_borrowed(), "yaml");
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
        let value = json!({"name": ""});
        let owned = ir_for_test(&value, "json");
        let diags =
            eval.evaluate_file(&Utf8PathBuf::from("doc.json"), &owned.to_borrowed(), "json");
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
        let owned = ir_for_test(&value, "yaml");
        let diags = eval.evaluate_file(&Utf8PathBuf::from("d.yaml"), &owned.to_borrowed(), "yaml");
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
        let value = json!({"a": 1});
        let owned = ir_for_test(&value, "yaml");
        let diags = eval.evaluate_file(&Utf8PathBuf::from("d.yaml"), &owned.to_borrowed(), "yaml");
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
        let value = json!({"a": 1});
        let owned = ir_for_test(&value, "yaml");
        let diags = eval.evaluate_file(&Utf8PathBuf::from("d.yaml"), &owned.to_borrowed(), "yaml");
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
        let value = json!({"a": 1});
        let owned = ir_for_test(&value, "yaml");
        let diags = eval.evaluate_file(&Utf8PathBuf::from("d.yaml"), &owned.to_borrowed(), "yaml");
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
        let value = json!({"a": 1});
        let owned = ir_for_test(&value, "yaml");
        let diags = eval.evaluate_file(&Utf8PathBuf::from("d.yaml"), &owned.to_borrowed(), "yaml");
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
        let value = json!({});
        let owned = ir_for_test(&value, "yaml");
        let matched = eval.evaluate_file(
            &Utf8PathBuf::from("dir/foo.yaml"),
            &owned.to_borrowed(),
            "yaml",
        );
        assert_eq!(matched.len(), 1, "expected glob match for dir/foo.yaml");

        let skipped = eval.evaluate_file(
            &Utf8PathBuf::from("dir/bar.yaml"),
            &owned.to_borrowed(),
            "yaml",
        );
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
        let owned = ir_for_test(&value, "yaml");
        let diags = eval.evaluate_file(&Utf8PathBuf::from("d.yaml"), &owned.to_borrowed(), "yaml");
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
        let value = json!({});
        let owned = ir_for_test(&value, "yaml");
        let diags = eval.evaluate_file(
            &Utf8PathBuf::from("real.yaml"),
            &owned.to_borrowed(),
            "yaml",
        );
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
        let value = json!({});
        let owned = ir_for_test(&value, "yaml");
        let diags = eval.evaluate_file(
            &Utf8PathBuf::from("real.yaml"),
            &owned.to_borrowed(),
            "yaml",
        );
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
        let value = json!("a-string");
        let owned = ir_for_test(&value, "yaml");
        let diags = eval.evaluate_file(&Utf8PathBuf::from("d.yaml"), &owned.to_borrowed(), "yaml");
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
        let value = json!({"name": ""});
        let owned = ir_for_test(&value, "yaml");
        let diags =
            cloned.evaluate_file(&Utf8PathBuf::from("d.yaml"), &owned.to_borrowed(), "yaml");
        assert_eq!(diags.len(), 1);
    }

    // -- Phase 2: `loc.pointer` chain tests -------------------------------
    //
    // These pin the four spec scenarios under "Location override via `loc:`"
    // (the `loc.file` scenario is already covered by
    // `loc_file_jq_override_replaces_path` above):
    //
    //   1. `loc.pointer` resolves to span line/col.
    //   2. `loc.pointer` falls through to `loc.line` when span is missing.
    //   3. `loc.line`-only override (legacy path) still works after Phase 2.
    //   4. `loc:` absent → default (1, 1).
    //
    // The first test reaches into `dq_core` and constructs a provenance map
    // by hand so the test owns the byte offset / span / pointer mapping
    // without depending on a real parser (the `tests/` integration test
    // pins the same chain against a real YAML parse).

    /// Build a 1-indexed `(line, col)` → byte-offset lookup against `bytes`.
    /// Mirrors the Phase 2 evaluator helper byte-for-byte; used to point a
    /// constructed `ValueSpan` at a known position.
    fn byte_offset(bytes: &[u8], line: u32, col: u32) -> usize {
        let mut cur_line: u32 = 1;
        let mut cur_col: u32 = 1;
        for (i, &b) in bytes.iter().enumerate() {
            if cur_line == line && cur_col == col {
                return i;
            }
            if b == b'\n' {
                cur_line = cur_line.saturating_add(1);
                cur_col = 1;
            } else {
                cur_col = cur_col.saturating_add(1);
            }
        }
        bytes.len()
    }

    /// Construct synthetic source bytes with `name: web` placed at the
    /// requested 1-indexed (line, col). Returned tuple is
    /// `(bytes, span_start)` — span_start is the byte offset corresponding
    /// to the requested `(line, col)`. Tests then assert the diagnostic's
    /// resolved position equals the requested `(line, col)`.
    fn synthesize_bytes_with_span_at(line: u32, col: u32) -> (Vec<u8>, usize) {
        // Pad with N-1 newlines, then col-1 spaces, then the value `web`.
        let line_padding = (line.saturating_sub(1)) as usize;
        let col_padding = (col.saturating_sub(1)) as usize;
        let mut bytes: Vec<u8> = Vec::with_capacity(line_padding + col_padding + 4);
        bytes.extend(std::iter::repeat_n(b'\n', line_padding));
        bytes.extend(std::iter::repeat_n(b' ', col_padding));
        bytes.extend_from_slice(b"web\n");
        let start = byte_offset(&bytes, line, col);
        (bytes, start)
    }

    #[test]
    fn loc_pointer_resolves_to_span_line_and_col() {
        // Spec scenario 1: `loc.pointer` resolves to span line.
        // Build an `Ir<'_>` whose provenance map carries an `Original` entry
        // for `/spec/containers/0` with a span pointing at byte offset
        // corresponding to (line 12, col 5). Then run a rule whose
        // `loc.pointer` jq emits `"/spec/containers/0"` and assert the
        // diagnostic resolved to (12, 5).
        use dq_core::document::spans::{SpanContext, ValueSpan};
        use dq_core::{FormatTag, Pointer, Provenance, ProvenanceMap, Value};

        let yaml = r#"
id: test.loc-pointer-span
description: x
severity: error
match:
  format: yaml
check:
  jq: '{"idx": 0}'
  message: 'msg'
loc:
  pointer: '"/spec/containers/" + (.idx|tostring)'
"#;
        let eval = evaluator_from_yaml(yaml);

        let (bytes, start) = synthesize_bytes_with_span_at(12, 5);
        let pointer = Pointer::parse("/spec/containers/0").expect("pointer parses");
        let mut provenance = ProvenanceMap::new();
        provenance.insert(
            pointer.as_canonical(),
            Provenance::original(
                pointer.clone(),
                Some(ValueSpan {
                    value_range: start..(start + 3),
                    line_range: 0..0,
                    indent: 0,
                    context: SpanContext::BlockMapValue,
                }),
            ),
        );
        // The actual `Value` shape is irrelevant — `loc.pointer` is the
        // jq-emitted string `/spec/containers/0`, looked up against the
        // provenance map. The check.jq emits a synthetic violation
        // independent of the document.
        let value = Value::Map(indexmap::IndexMap::new());
        let ir = dq_core::Ir::with_bytes(&value, &provenance, FormatTag::Yaml, &bytes);

        let diags = eval.evaluate_file(&Utf8PathBuf::from("d.yaml"), &ir, "yaml");
        assert_eq!(diags.len(), 1, "expected one diagnostic, got: {diags:?}");
        assert_eq!(
            diags[0].line, 12,
            "loc.pointer must resolve span line via Ir::line_col_for"
        );
        assert_eq!(
            diags[0].col, 5,
            "loc.pointer must resolve span col via Ir::line_col_for"
        );
    }

    #[test]
    fn loc_pointer_falls_through_to_loc_line_when_span_missing() {
        // Spec scenario 2: `loc.pointer` falls through to `loc.line` when
        // the span is missing. The IR has NO entry for `/missing`, so the
        // pointer resolution returns `None` and the chain falls through
        // to `loc.line`, which evaluates `.line` against `{"line": 7}`.
        let yaml = r#"
id: test.loc-pointer-fallthrough
description: x
severity: error
match:
  format: yaml
check:
  jq: '{"line": 7}'
  message: 'msg'
loc:
  pointer: '"/missing"'
  line: '.line'
"#;
        let eval = evaluator_from_yaml(yaml);
        // Empty provenance map means `/missing` lookup yields `None`; the
        // evaluator MUST then fall through to `loc.line`.
        let value = json!({});
        let owned = ir_for_test(&value, "yaml");
        let diags = eval.evaluate_file(&Utf8PathBuf::from("d.yaml"), &owned.to_borrowed(), "yaml");
        assert_eq!(diags.len(), 1, "expected one diagnostic, got: {diags:?}");
        assert_eq!(
            diags[0].line, 7,
            "loc.pointer with no span must fall through to loc.line",
        );
    }

    #[test]
    fn loc_line_jq_override_alone_still_works() {
        // Spec scenario 3 (legacy path): `loc.line` jq-only override still
        // works after the Phase 2 chain refactor. Pins backward
        // compatibility — M8-era rules with no `loc.pointer` must be
        // unaffected by the new chain.
        let yaml = r#"
id: test.loc-line-only
description: x
severity: error
match:
  format: yaml
check:
  jq: '{"position": {"line": 42}}'
  message: 'msg'
loc:
  line: '.position.line'
"#;
        let eval = evaluator_from_yaml(yaml);
        let value = json!({});
        let owned = ir_for_test(&value, "yaml");
        let diags = eval.evaluate_file(&Utf8PathBuf::from("d.yaml"), &owned.to_borrowed(), "yaml");
        assert_eq!(diags.len(), 1, "expected one diagnostic, got: {diags:?}");
        assert_eq!(
            diags[0].line, 42,
            "legacy `loc.line` jq override must still resolve",
        );
        assert_eq!(diags[0].col, 1, "legacy loc.line path hard-codes col=1");
    }

    #[test]
    fn loc_block_absent_uses_default_line_one() {
        // Spec scenario 4: `loc:` absent → default (1, 1).
        // Mirrors `evaluator_matches_format_and_emits_one_diagnostic` but
        // pins the contract explicitly — a regression that, say, started
        // returning `(0, 0)` on an absent `loc:` block would surface here
        // even if the broader smoke test continued to pass.
        let yaml = r#"
id: test.loc-absent
description: x
severity: error
match:
  format: yaml
check:
  jq: '.'
  message: 'msg'
"#;
        let eval = evaluator_from_yaml(yaml);
        let value = json!({"a": 1});
        let owned = ir_for_test(&value, "yaml");
        let diags = eval.evaluate_file(&Utf8PathBuf::from("d.yaml"), &owned.to_borrowed(), "yaml");
        assert_eq!(diags.len(), 1, "expected one diagnostic, got: {diags:?}");
        assert_eq!(
            (diags[0].line, diags[0].col),
            (1, 1),
            "absent `loc:` block must default to (1, 1)",
        );
    }

    #[test]
    fn schema_check_deduplicates_identical_diagnostics_per_path() {
        // Regression: jsonschema validators yield one error per schema
        // location, so a schema that declares the same `required`
        // constraint twice (once at the top level, once inside an
        // `allOf` subschema) produced two duplicate diagnostics for a
        // single missing-property violation — the user-visible bug
        // reported as `jsonschema.kubernetes-crd-shape` emitting two
        // identical "metadata is required" rows.
        //
        // After the fix, `run_schema_check` keys each emitted
        // diagnostic by `(rule_id, instance_path, error_text)` per
        // invocation and skips repeats, so the same violation yields
        // exactly one diagnostic regardless of how many schema
        // locations declared the constraint.
        let yaml = r#"
id: test.schema-dedup
description: x
severity: error
match:
  format: json
check:
  schema:
    type: object
    required: [metadata]
    allOf:
      - required: [metadata]
"#;
        let eval = evaluator_from_yaml(yaml);
        let value = json!({});
        let owned = ir_for_test(&value, "json");
        let diags = eval.evaluate_file(&Utf8PathBuf::from("d.json"), &owned.to_borrowed(), "json");
        assert_eq!(
            diags.len(),
            1,
            "duplicate diagnostics for the same (instance_path, error_text) must collapse, got: {diags:?}",
        );
        assert_eq!(diags[0].rule_id, "test.schema-dedup");
        assert!(
            diags[0].message.to_lowercase().contains("metadata"),
            "expected the surviving diagnostic to mention the missing property, got: {}",
            diags[0].message,
        );
    }

    // --- perf-jq-compile-cache regression tests ----------------------------
    //
    // The cache is invisible from `Evaluator`'s public API; we reach into
    // `compiled_rules()` (a crate-internal accessor already in use by the
    // `Fixer`) and use `Arc::ptr_eq` to assert the cache identity contract:
    //
    //   - Two rules with the *same* `match.filter` share one `JqEngine`
    //     instance (cache hit).
    //   - Two rules with *different* `match.filter`s have distinct
    //     instances (cache key is exact-string).
    //   - Two `Evaluator::new` calls do not share state (per-invocation
    //     cache lifetime — see `JqCache` doc-comment).

    #[test]
    fn cache_dedupes_identical_filters() {
        // Two rules with byte-identical `match.filter` strings must
        // resolve to the same `Arc<JqEngine>` instance after compile —
        // this is the headline cache hit case.
        let yaml = r#"
id: rule_a
description: a
severity: warn
match:
  format: yaml
  filter: '.kind == "Deployment"'
check:
  jq: 'true'
  message: 'a'
---
id: rule_b
description: b
severity: warn
match:
  format: yaml
  filter: '.kind == "Deployment"'
check:
  jq: 'true'
  message: 'b'
"#;
        let eval = evaluator_from_yaml(yaml);
        let compiled = eval.compiled_rules();
        assert_eq!(compiled.len(), 2, "expected two compiled rules");
        let a = compiled[0]
            .filter_engine
            .as_ref()
            .expect("rule_a has filter_engine");
        let b = compiled[1]
            .filter_engine
            .as_ref()
            .expect("rule_b has filter_engine");
        assert!(
            Arc::ptr_eq(a, b),
            "identical match.filter expressions must share one Arc<JqEngine> instance",
        );
    }

    #[test]
    fn cache_does_not_collapse_different_filters() {
        // Sanity: two rules with *different* filter strings get distinct
        // `Arc<JqEngine>` instances. Pins that the cache key is the
        // exact-string and does not, e.g., glob-collapse.
        let yaml = r#"
id: rule_a
description: a
severity: warn
match:
  format: yaml
  filter: '.kind == "Deployment"'
check:
  jq: 'true'
  message: 'a'
---
id: rule_b
description: b
severity: warn
match:
  format: yaml
  filter: '.kind == "Service"'
check:
  jq: 'true'
  message: 'b'
"#;
        let eval = evaluator_from_yaml(yaml);
        let compiled = eval.compiled_rules();
        assert_eq!(compiled.len(), 2, "expected two compiled rules");
        let a = compiled[0]
            .filter_engine
            .as_ref()
            .expect("rule_a has filter_engine");
        let b = compiled[1]
            .filter_engine
            .as_ref()
            .expect("rule_b has filter_engine");
        assert!(
            !Arc::ptr_eq(a, b),
            "different match.filter expressions must NOT share an Arc<JqEngine> instance",
        );
    }

    #[test]
    fn cache_does_not_persist_across_evaluator_news() {
        // Two `Evaluator::new` calls with the same filter must produce
        // distinct `Arc<JqEngine>` instances — the cache is lexically
        // scoped to a single `Evaluator::new`, not global per-process.
        // This pins the design decision documented in `JqCache`.
        let yaml = r#"
id: rule_a
description: a
severity: warn
match:
  format: yaml
  filter: '.kind == "Deployment"'
check:
  jq: 'true'
  message: 'a'
"#;
        let eval_one = evaluator_from_yaml(yaml);
        let eval_two = evaluator_from_yaml(yaml);
        let a = eval_one.compiled_rules()[0]
            .filter_engine
            .as_ref()
            .expect("first evaluator has filter_engine");
        let b = eval_two.compiled_rules()[0]
            .filter_engine
            .as_ref()
            .expect("second evaluator has filter_engine");
        assert!(
            !Arc::ptr_eq(a, b),
            "cache must not persist across two `Evaluator::new` invocations",
        );
    }
}
