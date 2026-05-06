//! `--rules` argument resolution and implicit auto-binding.
//!
//! [`RuleLoader::resolve`] converts the user-facing `--rules` argument
//! list and the discovered-format set into a concrete vector of
//! [`RuleSet`]s ready for the [`crate::evaluator::Evaluator`].
//!
//! ## Resolution rules
//!
//! When `args.rules` is non-empty, each entry resolves as:
//!
//! - Starts with `@std/` → embedded standard ruleset namespace.
//! - Path on disk that exists → file or directory of rules.
//! - Otherwise → [`crate::error::ExecError::UnknownRule`] with a
//!   Levenshtein-2 suggestion list against `@std/*` namespaces.
//!
//! When `args.rules` is empty, the loader auto-binds:
//!
//! - Every `@std/<ns>` whose rules apply to at least one of the
//!   discovered formats.
//! - Project-local rules under `<cwd>/.dq/rules/`, when present.

use camino::{Utf8Path, Utf8PathBuf};
use indexmap::IndexSet;

use crate::error::{ExecError, Result};
use crate::ruleset::RuleSet;

/// Loader inputs — caller-provided context that drives resolution.
#[derive(Debug, Clone)]
pub struct LoaderArgs {
    /// `--rules` arguments as the user typed them. Each entry is one of:
    /// `@std/<ns>`, a path to a file, or a path to a directory.
    pub rules: Vec<String>,
    /// Current working directory. Used for relative path resolution and
    /// for the `<cwd>/.dq/rules/` auto-bind probe.
    pub cwd: Utf8PathBuf,
    /// Format names discovered while scanning the user's input files —
    /// used by the implicit auto-bind path to pick which `@std/*`
    /// namespaces to load. Insertion order is preserved.
    pub discovered_formats: IndexSet<String>,
}

/// Stateless namespace for the resolution helper.
#[derive(Debug, Clone, Copy)]
pub struct RuleLoader;

impl RuleLoader {
    /// Resolve `args` into the rulesets the evaluator should run.
    ///
    /// # Errors
    ///
    /// - [`ExecError::UnknownRule`] when a `--rules` entry doesn't
    ///   resolve. The variant carries a `did_you_mean` list of
    ///   Levenshtein-2 suggestions against the known `@std/*` namespaces.
    /// - [`ExecError::Parse`] / [`ExecError::Io`] when a path-based entry
    ///   exists but fails to load. These propagate up unchanged.
    pub fn resolve(args: &LoaderArgs) -> Result<Vec<RuleSet>> {
        if !args.rules.is_empty() {
            return resolve_explicit(args);
        }
        resolve_implicit(args)
    }
}

/// Resolve the explicit `--rules` form (the list is non-empty).
fn resolve_explicit(args: &LoaderArgs) -> Result<Vec<RuleSet>> {
    let mut out = Vec::new();
    for arg in &args.rules {
        out.push(resolve_one(arg, &args.cwd)?);
    }
    Ok(out)
}

/// Resolve a single user-supplied `--rules` entry.
fn resolve_one(arg: &str, cwd: &Utf8Path) -> Result<RuleSet> {
    if let Some(name) = arg.strip_prefix("@std/") {
        return RuleSet::from_std(name).map_err(|err| match err {
            // Attach typo suggestions when `from_std` couldn't resolve
            // the namespace — the underlying ruleset returns an empty
            // suggestion list because it doesn't know the candidate set.
            ExecError::UnknownRule { id, .. } => ExecError::UnknownRule {
                id,
                did_you_mean: did_you_mean(arg, &std_candidate_strings()),
            },
            other => other,
        });
    }
    let candidate = if Utf8Path::new(arg).is_absolute() {
        Utf8PathBuf::from(arg)
    } else {
        cwd.join(arg)
    };
    if candidate.exists() {
        return RuleSet::from_path(&candidate);
    }
    Err(ExecError::UnknownRule {
        id: arg.to_owned(),
        did_you_mean: did_you_mean(arg, &std_candidate_strings()),
    })
}

/// Resolve the implicit auto-bind form (the rules list is empty).
fn resolve_implicit(args: &LoaderArgs) -> Result<Vec<RuleSet>> {
    let mut out = Vec::new();
    let mut seen_std: IndexSet<&'static str> = IndexSet::new();

    // Try each known std namespace and keep those whose rules overlap
    // the discovered-format set. `from_std` failures are non-fatal during
    // implicit resolution because the user did not explicitly request
    // the namespace — surfacing a load error here would be surprising.
    for ns in try_std_namespaces() {
        if seen_std.contains(ns) {
            continue;
        }
        let Ok(set) = RuleSet::from_std(ns) else {
            continue;
        };
        if set.rules.iter().any(|r| {
            r.match_
                .format
                .iter()
                .any(|f| args.discovered_formats.contains(f))
        }) {
            seen_std.insert(ns);
            out.push(set);
        }
    }

    // Project-local rules at `<cwd>/.dq/rules/`.
    let project = args.cwd.join(".dq").join("rules");
    if project.is_dir() {
        out.push(RuleSet::from_path(&project)?);
    }

    Ok(out)
}

/// List of known std namespaces, sourced from `dq-lint`'s embedding
/// table. The single source of truth lives in
/// `crates/dq-lint/src/embed.rs` so adding a namespace there propagates
/// here without an edit on the `dq-exec` side.
fn try_std_namespaces() -> &'static [&'static str] {
    dq_lint::list_std_rulesets()
}

/// Render the std namespaces with the `@std/` prefix for did-you-mean
/// candidates.
fn std_candidate_strings() -> Vec<String> {
    try_std_namespaces()
        .iter()
        .map(|ns| format!("@std/{ns}"))
        .collect()
}

/// Compute `Levenshtein(target, c) <= 2` filter over `candidates`.
///
/// Returns the matching candidate strings in input order. Used as the
/// `did_you_mean` payload on [`ExecError::UnknownRule`] so the CLI can
/// suggest typo-fixes (`@std/k8z` → `@std/k8s`).
fn did_you_mean(target: &str, candidates: &[String]) -> Vec<String> {
    candidates
        .iter()
        .filter(|c| levenshtein(target, c) <= 2)
        .cloned()
        .collect()
}

/// Standard two-row Levenshtein distance.
///
/// O(n*m) time, O(min(n,m)) memory. Operates on Unicode scalar values
/// (`char`) so multibyte characters count as one edit.
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
    use crate::ruleset::RuleSource;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    fn tempdir_utf8() -> (TempDir, Utf8PathBuf) {
        let temp = TempDir::new().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("UTF-8 temp path");
        (temp, path)
    }

    const SAMPLE_RULE: &str = r#"
id: a.one
description: x
severity: warn
match:
  format: yaml
check:
  jq: '.'
  message: m
"#;

    #[test]
    fn explicit_path_to_file_loads_ruleset() {
        let (_t, dir) = tempdir_utf8();
        let rule_path = dir.join("alpha.yml");
        std::fs::write(&rule_path, SAMPLE_RULE).expect("write rule");

        let args = LoaderArgs {
            rules: vec![rule_path.to_string()],
            cwd: dir.clone(),
            discovered_formats: IndexSet::new(),
        };
        let sets = RuleLoader::resolve(&args).expect("resolve");
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].rules.len(), 1);
        assert_eq!(sets[0].rules[0].id, "a.one");
    }

    #[test]
    fn explicit_relative_path_resolves_against_cwd() {
        let (_t, dir) = tempdir_utf8();
        std::fs::write(dir.join("alpha.yml"), SAMPLE_RULE).expect("write rule");

        let args = LoaderArgs {
            rules: vec!["alpha.yml".to_owned()],
            cwd: dir.clone(),
            discovered_formats: IndexSet::new(),
        };
        let sets = RuleLoader::resolve(&args).expect("resolve");
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].rules[0].id, "a.one");
    }

    #[test]
    fn explicit_path_to_directory_loads_ruleset() {
        let (_t, dir) = tempdir_utf8();
        let rules_dir = dir.join("rules");
        std::fs::create_dir_all(&rules_dir).expect("mkdir");
        std::fs::write(rules_dir.join("alpha.yml"), SAMPLE_RULE).expect("write rule");

        let args = LoaderArgs {
            rules: vec![rules_dir.to_string()],
            cwd: dir.clone(),
            discovered_formats: IndexSet::new(),
        };
        let sets = RuleLoader::resolve(&args).expect("resolve");
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].rules.len(), 1);
    }

    #[test]
    fn explicit_unknown_rule_returns_error_with_suggestions() {
        let (_t, dir) = tempdir_utf8();
        let args = LoaderArgs {
            rules: vec!["@std/k8z".to_owned()],
            cwd: dir,
            discovered_formats: IndexSet::new(),
        };
        let err = RuleLoader::resolve(&args).expect_err("expected UnknownRule");
        match err {
            ExecError::UnknownRule { id, did_you_mean } => {
                assert_eq!(id, "@std/k8z");
                // Levenshtein("@std/k8z", "@std/k8s") = 1 → must appear.
                assert!(
                    did_you_mean.iter().any(|s| s == "@std/k8s"),
                    "expected suggestion @std/k8s, got: {did_you_mean:?}",
                );
            }
            other => panic!("expected UnknownRule, got {other:?}"),
        }
    }

    #[test]
    fn explicit_unknown_path_falls_through_to_unknown_rule() {
        let (_t, dir) = tempdir_utf8();
        let args = LoaderArgs {
            rules: vec!["nonexistent-rules.yml".to_owned()],
            cwd: dir,
            discovered_formats: IndexSet::new(),
        };
        let err = RuleLoader::resolve(&args).expect_err("expected UnknownRule");
        match err {
            ExecError::UnknownRule { id, .. } => {
                assert_eq!(id, "nonexistent-rules.yml");
            }
            other => panic!("expected UnknownRule, got {other:?}"),
        }
    }

    #[test]
    fn implicit_picks_up_project_local_rules_dir() {
        let (_t, dir) = tempdir_utf8();
        let project = dir.join(".dq").join("rules");
        std::fs::create_dir_all(&project).expect("mkdir");
        std::fs::write(project.join("local.yml"), SAMPLE_RULE).expect("write rule");

        let mut formats = IndexSet::new();
        formats.insert("yaml".to_owned());

        let args = LoaderArgs {
            rules: Vec::new(),
            cwd: dir,
            discovered_formats: formats,
        };
        let sets = RuleLoader::resolve(&args).expect("resolve");
        // With dq-lint wired in, std namespaces that match yaml may also
        // bind alongside the project-local rules. The contract this test
        // pins is: the project-local ruleset is among them.
        assert!(
            sets.iter()
                .any(|s| matches!(&s.source, RuleSource::Local(p) if p.ends_with("rules"))),
            "expected the .dq/rules directory to bind, got: {sets:?}",
        );
    }

    #[test]
    fn implicit_with_no_project_rules_returns_empty() {
        let (_t, dir) = tempdir_utf8();
        let args = LoaderArgs {
            rules: Vec::new(),
            cwd: dir,
            // No discovered formats → no std namespace overlaps → no
            // project rules dir → empty result.
            discovered_formats: IndexSet::new(),
        };
        let sets = RuleLoader::resolve(&args).expect("resolve");
        assert!(sets.is_empty(), "expected zero rulesets, got: {sets:?}");
    }

    #[test]
    fn levenshtein_distance_matches_known_values() {
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("abc", "abc"), 0);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("@std/k8z", "@std/k8s"), 1);
        assert_eq!(levenshtein("@std/dockr", "@std/docker"), 1);
    }

    #[test]
    fn did_you_mean_filters_at_distance_two() {
        let candidates: Vec<String> = vec![
            "@std/k8s".to_owned(),
            "@std/dockerfile".to_owned(),
            "@std/npm".to_owned(),
        ];
        let suggestions = did_you_mean("@std/k8z", &candidates);
        assert_eq!(suggestions, vec!["@std/k8s".to_owned()]);

        let none = did_you_mean("@std/totally-different", &candidates);
        assert!(none.is_empty(), "expected no matches, got: {none:?}");
    }

    /// Implicit binding of `@std/k8s` based on a discovered `yaml`
    /// format. With §4.4 in place, `from_std` resolves through
    /// `dq_lint::std_ruleset`, so this scenario now runs end-to-end.
    #[test]
    fn implicit_auto_binds_std_for_matching_format() {
        let (_t, dir) = tempdir_utf8();
        let mut formats = IndexSet::new();
        formats.insert("yaml".to_owned());
        let args = LoaderArgs {
            rules: Vec::new(),
            cwd: dir,
            discovered_formats: formats,
        };
        let sets = RuleLoader::resolve(&args).expect("resolve");
        assert!(
            sets.iter()
                .any(|s| matches!(s.source, RuleSource::Std("k8s"))),
            "expected @std/k8s to auto-bind for yaml input",
        );
    }

    /// Implicit binding skips namespaces with no overlapping format.
    #[test]
    fn implicit_skips_std_when_no_format_overlap() {
        let (_t, dir) = tempdir_utf8();
        // Discover only `csv`, which no std namespace currently targets.
        let mut formats = IndexSet::new();
        formats.insert("csv".to_owned());
        let args = LoaderArgs {
            rules: Vec::new(),
            cwd: dir,
            discovered_formats: formats,
        };
        let sets = RuleLoader::resolve(&args).expect("resolve");
        assert!(sets.is_empty(), "expected no @std namespaces to bind");
    }
}
