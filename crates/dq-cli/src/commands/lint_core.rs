//! Shared lint pipeline implementation for `dq lint` and `dq check`.
//!
//! Pipeline:
//!
//! 1. Expand input glob patterns into a flat list of file paths.
//! 2. For each file, detect the format (extension or `--input-format`) and
//!    record it in an `IndexSet` of discovered formats.
//! 3. Resolve `--rules` (or auto-bind on empty) into a `Vec<RuleSet>`.
//! 4. Compile every rule via [`Evaluator::new`].
//! 5. Per-file: load + parse the document, evaluate each rule, collect
//!    diagnostics.
//! 6. Hand the canonical `{ "diagnostics": [...] }` shape to the configured
//!    reporter.
//! 7. Compute the lint exit code:
//!    - any `Severity::Error` diagnostic → [`LintFail`] (exit 4).
//!    - any `Severity::Warn` diagnostic AND `cli.strict` → [`LintWarnStrict`]
//!      (exit 1).
//!    - otherwise `Ok(())`.
//!
//! Both `lint` (multi-rule, multi-source) and `check` (single rule from path
//! / `--inline` / std lookup) funnel through [`run_with_rulesets`] — the only
//! difference is how the caller materialises the rulesets list.

use std::io::Write;

use camino::{Utf8Path, Utf8PathBuf};
use dq_exec::{Diagnostic, Evaluator, LoaderArgs, Rule, RuleLoader, RuleSet, Severity};
use indexmap::IndexSet;

use crate::cli::{Cli, DocSelector};
use crate::commands::io_helpers::{
    checked_index, every_file_lacks, load_document_for_lint, pick_format, skip_out_of_range,
};
use crate::commands::plugin_loader::{self, LoadedPlugins};
use crate::error::{InvalidInput, LintFail, LintWarnStrict};
use crate::output::Reporter;

/// Run the lint pipeline once a caller has produced the input rulesets.
///
/// `rules_args` is the user-facing `--rules` list. When empty *and* the
/// caller has not opted out via `suppress_auto_bind`, the loader falls back
/// to auto-binding `@std/*` namespaces matching the discovered file formats
/// plus `<cwd>/.dq/rules/`.
///
/// `extra` carries any pre-built rulesets the caller already resolved
/// (e.g. `--inline` YAML or the single-rule [`RuleSet`] for `check`); they
/// are appended to whatever the loader produces from `rules_args`. A
/// non-empty `extra` does NOT, by itself, suppress the auto-bind path:
/// callers must set `suppress_auto_bind = true` to skip it. The current
/// subcommand wiring is:
///
/// - [`crate::commands::lint::run`] → `suppress_auto_bind = false`
///   (preserves the user-facing "no `--rules` ⇒ pick sensible defaults"
///   behaviour).
/// - [`crate::commands::check::run`] → `suppress_auto_bind = true` (the
///   user has already named exactly one rule via `--rule` / `--inline`;
///   auto-binding would silently run unrelated `@std/*` rules alongside).
///
/// # Errors
///
/// - [`InvalidInput`] (exit 6) when `--rules` references an unknown
///   `@std/<ns>` or a non-existent path.
/// - `dq_core::Error::*` for the usual file-load failures.
/// - [`LintFail`] (exit 4) when any error-severity diagnostic is emitted.
/// - [`LintWarnStrict`] (exit 1) when warnings are emitted under `--strict`.
// 8 args is one over clippy's default `too_many_arguments` threshold. The
// alternative would be introducing a `RulesetSpec { rules_args, extra,
// suppress_auto_bind }` struct purely for this internal `pub(crate)` helper
// that only `lint::run` / `check::run` / `fix::run` call. Not worth the
// indirection — `suppress_auto_bind` carries enough behavioural weight on
// its own that callers should see it spelled out at the call site.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_with_rulesets(
    cli: &Cli,
    files: &[Utf8PathBuf],
    rules_args: Vec<String>,
    extra: Vec<RuleSet>,
    suppress_auto_bind: bool,
    input_format: Option<&str>,
    reporter: &dyn Reporter,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    cli.ensure_no_write_flags()?;

    let expanded = expand_lint_inputs(files)?;
    if expanded.is_empty() {
        // Reporter still gets called with an empty diagnostics array so
        // structured consumers see a well-formed envelope.
        let envelope = serde_json::json!({ "diagnostics": [] });
        reporter.report(&envelope, out)?;
        return Ok(());
    }

    // Detect formats up front so the loader can decide which `@std/*`
    // namespaces to auto-bind.
    let mut discovered_formats: IndexSet<String> = IndexSet::new();
    for file in &expanded {
        let fmt = pick_format(file, input_format)?;
        discovered_formats.insert(fmt.name().to_owned());
    }

    let cwd = std::env::current_dir()
        .ok()
        .and_then(|p| Utf8PathBuf::from_path_buf(p).ok())
        .unwrap_or_else(|| Utf8PathBuf::from("."));

    let loader_args = LoaderArgs {
        rules: rules_args,
        cwd,
        discovered_formats: discovered_formats.clone(),
        suppress_auto_bind,
    };
    let mut rulesets = RuleLoader::resolve(&loader_args).map_err(anyhow::Error::new)?;
    rulesets.extend(extra);

    let evaluator = Evaluator::new(rulesets).map_err(anyhow::Error::new)?;

    // Phase 5 (`add-ir-foundation`): load plugins from `--plugins <DIR>` once
    // up front and reuse the runtime / handles across every file. When the
    // flag is absent or the directory contains no `*.wasm` files, `plugins`
    // is `None` and the per-file loop skips the plugin invocation entirely.
    let plugins: Option<LoadedPlugins> = match cli.plugins.as_deref() {
        Some(dir) => {
            let loaded = plugin_loader::load_all(dir)?;
            if loaded.handles.is_empty() {
                None
            } else {
                Some(loaded)
            }
        }
        None => None,
    };

    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let several = expanded.len() > 1;
    let mut missing = Vec::new();
    for file in &expanded {
        let fmt = pick_format(file, input_format)?;
        let format_name = fmt.name().to_owned();
        // Use the lint-specific loader so YAML/JSON are parsed through
        // their span-aware parsers — the resulting `Document` carries a
        // populated provenance map, which `Ir::line_col_for(&Pointer)` (used
        // by the evaluator's `loc.pointer` chain) needs to resolve to a
        // real `(line, col)`. See `load_document_for_lint` for the rationale.
        let (_doc_fmt, doc) = load_document_for_lint(file, input_format, cli.template_mode())?;
        // Each document of a multi-document stream is linted as a document
        // of its own — `match.filter`, `check`, `loc.*`, composite
        // `extract` and plugins all see one document, as they would in a
        // file of its own. `--doc N` keeps only document N.
        // Over several files, one without document N is skipped with a
        // note on stderr; a single file without it — or a run where no
        // file holds it — is exit 2.
        let only = match cli.doc {
            Some(DocSelector::Index(index)) => {
                match skip_out_of_range(checked_index(&doc, index, file), several)? {
                    Ok(index) => Some(index),
                    Err(reason) => {
                        missing.push((file, reason));
                        continue;
                    }
                }
            }
            Some(DocSelector::All) | None => None,
        };
        match doc.stream_documents() {
            Some(parts) => {
                for part in parts.iter().filter(|p| only.is_none_or(|i| p.index() == i)) {
                    lint_document(
                        &evaluator,
                        plugins.as_ref(),
                        file,
                        &part.as_ir(),
                        &format_name,
                        &mut diagnostics,
                    )?;
                }
            }
            None => lint_document(
                &evaluator,
                plugins.as_ref(),
                file,
                &doc.as_ir(),
                &format_name,
                &mut diagnostics,
            )?,
        }
    }

    if let Some(err) = every_file_lacks(&missing, expanded.len()) {
        return Err(anyhow::Error::new(err));
    }
    for (file, reason) in &missing {
        tracing::warn!("skipped {file}: {reason}");
    }

    let mut envelope = build_envelope(&diagnostics);
    if reporter.wants_rule_catalog() {
        envelope["rules"] = rule_catalog(evaluator.rules());
    }
    reporter.report(&envelope, out)?;

    classify_exit(&diagnostics, cli.strict)
}

/// Lint one document — a whole file, or one document of a stream — with
/// the rule engine and then every loaded plugin, appending to
/// `diagnostics`.
///
/// After the rule engine runs every `@std/*` and `--rules`-loaded rule,
/// every plugin loaded from `--plugins <DIR>` gets a chance to contribute
/// its own diagnostics. Plugin invocation errors propagate — they bubble
/// through `exit_code_for_error` and route via `PluginError::kind_name()`;
/// they are not swallowed, mirroring the rule-engine semantics for buggy
/// rules.
fn lint_document(
    evaluator: &Evaluator,
    plugins: Option<&LoadedPlugins>,
    file: &Utf8Path,
    ir: &dq_core::Ir<'_>,
    format_name: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> anyhow::Result<()> {
    let mut found = evaluator.evaluate_file(file, ir, format_name);
    diagnostics.append(&mut found);
    if let Some(loaded) = plugins
        && let Some(runtime) = loaded.runtime.as_ref()
    {
        for handle in &loaded.handles {
            let mut plugin_diags = runtime
                .invoke_lint(handle, ir, Some(file))
                .map_err(anyhow::Error::new)?;
            diagnostics.append(&mut plugin_diags);
        }
    }
    Ok(())
}

/// Expand each input path: a glob ([`crate::glob::is_glob`]) becomes the
/// files it matches; anything else passes through unchanged. Returns a
/// flat, deterministically-ordered list of files.
///
/// A glob failure (invalid pattern, missing walk root, zero matches) is an
/// `InvalidInput` so the exit-code mapper picks 6 — distinct from
/// `bulk::expand_glob`'s "I/O error" mapping because lint is read-only and
/// "no files matched" is a caller-side mistake, not a write failure.
///
/// Visibility: `pub(crate)` so the M10 `dq fix` handler can reuse the
/// same read-mode glob expansion semantics (zero-match → InvalidInput).
pub(crate) fn expand_lint_inputs(paths: &[Utf8PathBuf]) -> anyhow::Result<Vec<Utf8PathBuf>> {
    let mut out = Vec::new();
    for p in paths {
        if crate::glob::is_glob(p) {
            let matched = crate::glob::expand(p)
                .map_err(|err| anyhow::Error::new(InvalidInput::new(err.to_string())))?;
            out.extend(matched);
        } else {
            out.push(p.clone());
        }
    }
    Ok(out)
}

/// Build the canonical `{ "diagnostics": [...] }` envelope every lint
/// reporter consumes.
pub(crate) fn build_envelope(diagnostics: &[Diagnostic]) -> serde_json::Value {
    let arr: Vec<serde_json::Value> = diagnostics.iter().map(Diagnostic::to_serde_json).collect();
    serde_json::json!({ "diagnostics": arr })
}

/// The rule catalogue a reporter asks for through
/// [`Reporter::wants_rule_catalog`]: `[{ "id", "description",
/// "references" }]`, one entry per loaded rule, in load order.
///
/// A diagnostic whose id is not here — a plugin's, a composite's
/// `<outer>.parse-failed`, a nested rule's — is still reported; the
/// reporter describes it by id alone.
pub(crate) fn rule_catalog<'a>(rules: impl Iterator<Item = &'a Rule>) -> serde_json::Value {
    rules
        .map(|rule| {
            serde_json::json!({
                "id": rule.id,
                "description": rule.description,
                "references": rule.references,
            })
        })
        .collect()
}

/// Compute the exit-code marker for a finished run.
pub(crate) fn classify_exit(diagnostics: &[Diagnostic], strict: bool) -> anyhow::Result<()> {
    let error_count = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .count();
    if error_count > 0 {
        return Err(anyhow::Error::new(LintFail { count: error_count }));
    }
    if strict {
        let warn_count = diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Warn)
            .count();
        if warn_count > 0 {
            return Err(anyhow::Error::new(LintWarnStrict { count: warn_count }));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use dq_exec::diagnostic::Severity as Sev;

    fn diag_with_severity(severity: Sev) -> Diagnostic {
        Diagnostic {
            rule_id: "test.rule".to_owned(),
            severity,
            message: "msg".to_owned(),
            file: Some(Utf8PathBuf::from("x.yaml")),
            line: 1,
            col: 1,
            span: None,
            references: Vec::new(),
            fix: None,
        }
    }

    #[test]
    fn classify_exit_returns_ok_when_no_diagnostics() {
        let res = classify_exit(&[], false);
        assert!(res.is_ok());
    }

    #[test]
    fn classify_exit_returns_lint_fail_for_error_severity() {
        let diags = vec![diag_with_severity(Sev::Error)];
        let err = classify_exit(&diags, false).unwrap_err();
        let fail = err
            .downcast_ref::<LintFail>()
            .expect("error severity must produce LintFail");
        assert_eq!(fail.count, 1);
    }

    #[test]
    fn classify_exit_ignores_warn_without_strict() {
        let diags = vec![diag_with_severity(Sev::Warn), diag_with_severity(Sev::Info)];
        let res = classify_exit(&diags, false);
        assert!(
            res.is_ok(),
            "warn-only without --strict must succeed, got: {res:?}"
        );
    }

    #[test]
    fn classify_exit_returns_warn_strict_under_strict() {
        let diags = vec![diag_with_severity(Sev::Warn)];
        let err = classify_exit(&diags, true).unwrap_err();
        let warn = err
            .downcast_ref::<LintWarnStrict>()
            .expect("warn under --strict must produce LintWarnStrict");
        assert_eq!(warn.count, 1);
    }

    #[test]
    fn classify_exit_error_takes_precedence_over_strict_warn() {
        // When both error and warn are present under --strict, the error
        // marker wins (exit 4 beats exit 1).
        let diags = vec![
            diag_with_severity(Sev::Error),
            diag_with_severity(Sev::Warn),
        ];
        let err = classify_exit(&diags, true).unwrap_err();
        assert!(err.downcast_ref::<LintFail>().is_some());
        assert!(err.downcast_ref::<LintWarnStrict>().is_none());
    }

    #[test]
    fn build_envelope_wraps_in_diagnostics_array() {
        let diags = vec![diag_with_severity(Sev::Error)];
        let envelope = build_envelope(&diags);
        let arr = envelope
            .get("diagnostics")
            .expect("envelope must carry diagnostics")
            .as_array()
            .expect("diagnostics must be an array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["severity"], "error");
    }
}
