//! Shared lint pipeline implementation for `dq lint` and `dq check`.
//!
//! Pipeline:
//!
//! 1. Expand input glob patterns into a flat list of file paths.
//! 2. For each file, detect the format (extension or `-F` override) and
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
use dq_exec::{Diagnostic, Evaluator, LoaderArgs, RuleLoader, RuleSet, Severity};
use indexmap::IndexSet;
use walkdir::WalkDir;

use crate::cli::Cli;
use crate::commands::io_helpers::{load_document_for_lint, pick_format};
use crate::commands::plugin_loader::{self, LoadedPlugins};
use crate::error::{InvalidInput, LintFail, LintWarnStrict};
use crate::output::Reporter;

/// Run the lint pipeline once a caller has produced the input rulesets.
///
/// `rules_args` is the user-facing `--rules` list. When empty *and* `extra`
/// is empty, the loader falls back to auto-binding `@std/*` namespaces
/// matching the discovered file formats and `<cwd>/.dq/rules/`.
///
/// `extra` carries any pre-built rulesets the caller already resolved
/// (e.g. `--inline` YAML for `check`); they are appended to whatever the
/// loader produces from `rules_args`. A non-empty `extra` does NOT suppress
/// the auto-bind path — callers that want a single-rule run should pass
/// `rules_args` such that the loader either resolves to that rule (path) or
/// supply a non-empty `extra` while keeping `rules_args` empty (the typical
/// `--inline` path uses both: `extra = vec![inline_set]`, `rules_args = []`).
///
/// # Errors
///
/// - [`InvalidInput`] (exit 6) when `--rules` references an unknown
///   `@std/<ns>` or a non-existent path.
/// - `dq_core::Error::*` for the usual file-load failures.
/// - [`LintFail`] (exit 4) when any error-severity diagnostic is emitted.
/// - [`LintWarnStrict`] (exit 1) when warnings are emitted under `--strict`.
pub(crate) fn run_with_rulesets(
    cli: &Cli,
    files: &[Utf8PathBuf],
    rules_args: Vec<String>,
    extra: Vec<RuleSet>,
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
    for file in &expanded {
        let fmt = pick_format(file, input_format)?;
        let format_name = fmt.name().to_owned();
        // Use the lint-specific loader so YAML/JSON are parsed through
        // their span-aware parsers — the resulting `Document` carries a
        // populated provenance map, which `Ir::line_col_for(&Pointer)` (used
        // by the evaluator's `loc.pointer` chain) needs to resolve to a
        // real `(line, col)`. See `load_document_for_lint` for the rationale.
        let (_doc_fmt, doc) = load_document_for_lint(file, input_format)?;
        let ir = doc.as_ir();
        let mut file_diags = evaluator.evaluate_file(file, &ir, &format_name);
        diagnostics.append(&mut file_diags);
        // After the rule engine runs every `@std/*` and `--rules`-loaded
        // rule, give every plugin loaded from `--plugins <DIR>` a chance to
        // contribute its own diagnostics. Plugin invocation errors
        // propagate up — they bubble through `exit_code_for_error` and
        // route via `PluginError::kind_name()`. We do NOT swallow them
        // here, mirroring the rule-engine semantics for buggy rules.
        if let Some(loaded) = plugins.as_ref()
            && let Some(runtime) = loaded.runtime.as_ref()
        {
            for handle in &loaded.handles {
                let mut plugin_diags = runtime
                    .invoke_lint(handle, &ir, Some(file.as_path()))
                    .map_err(anyhow::Error::new)?;
                diagnostics.append(&mut plugin_diags);
            }
        }
    }

    let envelope = build_envelope(&diagnostics);
    reporter.report(&envelope, out)?;

    classify_exit(&diagnostics, cli.strict)
}

/// Expand each input path: if it contains glob metacharacters (`*`/`?`/`[`)
/// walk the matching files; otherwise pass through unchanged. Returns a
/// flat, deterministically-ordered list of files.
///
/// Visibility: `pub(crate)` so the M10 `dq fix` handler can reuse the
/// same read-mode glob expansion semantics (zero-match → InvalidInput).
pub(crate) fn expand_lint_inputs(paths: &[Utf8PathBuf]) -> anyhow::Result<Vec<Utf8PathBuf>> {
    let mut out = Vec::new();
    for p in paths {
        if has_glob_chars(p.as_str()) {
            let matched = expand_glob(p)?;
            out.extend(matched);
        } else {
            out.push(p.clone());
        }
    }
    Ok(out)
}

fn has_glob_chars(s: &str) -> bool {
    s.chars().any(|c| matches!(c, '*' | '?' | '[' | '{'))
}

/// Read-mode glob expansion. Walks from the longest non-meta prefix and
/// filters via a compiled `globset::GlobMatcher`. Zero matches is an
/// `InvalidInput` so the exit-code mapper picks 6 — distinct from
/// `bulk::expand_glob`'s "I/O error" mapping because lint is read-only and
/// "no files matched" is a caller-side mistake, not a write failure.
fn expand_glob(pattern: &Utf8Path) -> anyhow::Result<Vec<Utf8PathBuf>> {
    let glob = globset::Glob::new(pattern.as_str()).map_err(|e| {
        anyhow::Error::new(InvalidInput::new(format!(
            "invalid glob pattern '{pattern}': {e}"
        )))
    })?;
    let matcher = glob.compile_matcher();

    let prefix = longest_non_meta_prefix(pattern);
    let walk_root = if prefix.as_str().is_empty() {
        Utf8PathBuf::from(".")
    } else {
        prefix
    };
    if !walk_root.as_std_path().exists() {
        return Err(anyhow::Error::new(InvalidInput::new(format!(
            "glob root {walk_root} does not exist"
        ))));
    }

    let mut matches: Vec<Utf8PathBuf> = WalkDir::new(walk_root.as_std_path())
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| matcher.is_match(entry.path()))
        .filter_map(|entry| Utf8PathBuf::from_path_buf(entry.into_path()).ok())
        .collect();

    if matches.is_empty() {
        return Err(anyhow::Error::new(InvalidInput::new(format!(
            "glob {pattern} matched zero files"
        ))));
    }

    matches.sort();
    Ok(matches)
}

fn longest_non_meta_prefix(pattern: &Utf8Path) -> Utf8PathBuf {
    let s = pattern.as_str();
    let mut prefix_components: Vec<&str> = Vec::new();
    for component in s.split('/') {
        if component
            .chars()
            .any(|c| matches!(c, '*' | '?' | '[' | '{'))
        {
            break;
        }
        prefix_components.push(component);
    }
    if prefix_components.is_empty() {
        return Utf8PathBuf::from(".");
    }
    let joined = prefix_components.join("/");
    if joined.is_empty() {
        Utf8PathBuf::from("/")
    } else {
        Utf8PathBuf::from(joined)
    }
}

/// Build the canonical `{ "diagnostics": [...] }` envelope every lint
/// reporter consumes.
pub(crate) fn build_envelope(diagnostics: &[Diagnostic]) -> serde_json::Value {
    let arr: Vec<serde_json::Value> = diagnostics.iter().map(Diagnostic::to_serde_json).collect();
    serde_json::json!({ "diagnostics": arr })
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

    #[test]
    fn has_glob_chars_detects_metacharacters() {
        assert!(has_glob_chars("foo/*.yaml"));
        assert!(has_glob_chars("a?b"));
        assert!(has_glob_chars("a[bc]d"));
        assert!(!has_glob_chars("plain.yaml"));
        assert!(!has_glob_chars("a/b/c.json"));
    }

    #[test]
    fn has_glob_chars_detects_brace_expansion() {
        // Must agree with `longest_non_meta_prefix`'s metachar set so brace
        // patterns don't slip through `expand_lint_inputs` as literal paths.
        assert!(has_glob_chars("dir/{a,b}/file.yaml"));
    }

    #[test]
    fn longest_non_meta_prefix_walks_until_first_meta() {
        assert_eq!(
            longest_non_meta_prefix(Utf8Path::new("dir/sub/*.yaml")),
            Utf8PathBuf::from("dir/sub")
        );
        assert_eq!(
            longest_non_meta_prefix(Utf8Path::new("*.yaml")),
            Utf8PathBuf::from(".")
        );
    }
}
