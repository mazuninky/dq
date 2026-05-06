//! `dq rules list` / `dq rules add` — manage rule sources.
//!
//! `list` walks every `@std/<ns>` ruleset plus `<cwd>/.dq/rules/` (if
//! present) and emits a JSON-shaped array through the configured reporter.
//!
//! `add @std/<ns>` materialises the embedded per-file rule YAML under
//! `./.dq/rules/<ns>/`. `--symlink` swaps the copy for a Unix symlink (and
//! falls back to copy on Windows with a warning); `--force` overwrites
//! existing destination files.

use std::fs;
use std::io::Write;

use camino::{Utf8Path, Utf8PathBuf};
use dq_exec::{RuleSet, RuleSource};

use crate::cli::{RulesAddArgs, RulesListArgs};
use crate::error::InvalidInput;
use crate::output::Reporter;

/// Run `dq rules list`.
///
/// Builds a `serde_json::Value::Array` of `{id, namespace, severity,
/// source}` rows and reports it through the configured reporter.
///
/// # Errors
///
/// - `InvalidInput` (exit 6) when an unsupported reporter format is set
///   (the `BannedReporter` shapes for hcl / ini / dotenv / csv / tsv /
///   frontmatter / sarif / junit / tap surface here).
/// - `dq_exec::ExecError` for `<cwd>/.dq/rules/` parse or I/O failures.
pub fn run_list(
    list_args: &RulesListArgs,
    reporter: &dyn Reporter,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    // Strip optional @std/ prefix on the filter so `--namespace @std/k8s`
    // and `--namespace k8s` work the same way.
    let filter = list_args
        .namespace
        .as_deref()
        .map(|s| s.strip_prefix("@std/").unwrap_or(s).to_owned());

    let mut rows: Vec<serde_json::Value> = Vec::new();

    for ns in dq_lint::list_std_rulesets() {
        if let Some(want) = &filter
            && want != *ns
        {
            continue;
        }
        let Some(yaml) = dq_lint::std_ruleset(ns) else {
            continue;
        };
        let set = match RuleSet::from_str(yaml, RuleSource::Std(ns)) {
            Ok(s) => s,
            Err(_) => continue,
        };
        for rule in set.rules {
            rows.push(serde_json::json!({
                "id": rule.id,
                "namespace": ns,
                "severity": rule.severity.as_str(),
                "source": format!("@std/{ns}"),
            }));
        }
    }

    // Project-local rules at `<cwd>/.dq/rules/`. The filter doesn't apply
    // here — local rules don't have a synthetic namespace, only their
    // `id` prefix.
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|p| Utf8PathBuf::from_path_buf(p).ok())
        .unwrap_or_else(|| Utf8PathBuf::from("."));
    let project = cwd.join(".dq").join("rules");
    if filter.is_none()
        && project.is_dir()
        && let Ok(set) = RuleSet::from_path(&project)
    {
        for rule in set.rules {
            rows.push(serde_json::json!({
                "id": rule.id,
                "namespace": serde_json::Value::Null,
                "severity": rule.severity.as_str(),
                "source": project.as_str(),
            }));
        }
    }

    let value = serde_json::Value::Array(rows);
    reporter.report(&value, out)?;
    Ok(())
}

/// Run `dq rules add <RULESET>`.
///
/// # Errors
///
/// - `InvalidInput` (exit 6) for unknown `@std/<ns>` namespaces and
///   destination collisions (without `--force`).
/// - `dq_core::Error::Io` (exit 5) when filesystem operations fail.
pub fn run_add(args: &RulesAddArgs) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|p| Utf8PathBuf::from_path_buf(p).ok())
        .unwrap_or_else(|| Utf8PathBuf::from("."));
    run_add_in_cwd(args, &cwd)
}

/// Variant of [`run_add`] that takes an explicit cwd. Production code calls
/// [`run_add`]; tests pin a tempdir-based cwd here so they don't have to
/// mutate the process-global `std::env::current_dir()`.
fn run_add_in_cwd(args: &RulesAddArgs, cwd: &Utf8Path) -> anyhow::Result<()> {
    if let Some(name) = args.ruleset.strip_prefix("@std/") {
        let files = dq_lint::std_rule_files(name).ok_or_else(|| {
            anyhow::Error::new(InvalidInput::new(
                format!("unknown @std namespace: {name}",),
            ))
        })?;
        let dest = cwd.join(".dq").join("rules").join(name);
        materialise_files(&dest, files, args.force, args.symlink, None)?;
        return Ok(());
    }

    // Path resolution — file or directory on disk.
    let candidate = if Utf8Path::new(&args.ruleset).is_absolute() {
        Utf8PathBuf::from(&args.ruleset)
    } else {
        cwd.join(&args.ruleset)
    };
    if !candidate.exists() {
        return Err(anyhow::Error::new(InvalidInput::new(format!(
            "ruleset path does not exist: {candidate}"
        ))));
    }

    // Determine the destination namespace from the path's file stem (file
    // case) or directory name (dir case).
    let ns_hint = if candidate.is_file() {
        candidate
            .file_stem()
            .map(str::to_owned)
            .unwrap_or_else(|| "local".to_owned())
    } else {
        candidate
            .file_name()
            .map(str::to_owned)
            .unwrap_or_else(|| "local".to_owned())
    };
    let dest = cwd.join(".dq").join("rules").join(&ns_hint);

    if candidate.is_file() {
        let body = fs::read_to_string(&candidate).map_err(|source| {
            anyhow::Error::new(dq_core::Error::Io {
                path: candidate.clone(),
                source,
            })
        })?;
        let file_name = candidate
            .file_name()
            .map(str::to_owned)
            .unwrap_or_else(|| "rules.yml".to_owned());
        // `body` and `file_name` are owned and live to the end of this scope,
        // which outlives the borrow taken by `pair`. The signature of
        // `materialise_files` accepts `&[(&str, &str)]` with elided lifetimes,
        // so non-`'static` references are fine here.
        let pair: [(&str, &str); 1] = [(file_name.as_str(), body.as_str())];
        materialise_files(
            &dest,
            &pair,
            args.force,
            args.symlink,
            Some(candidate.as_path()),
        )?;
    } else {
        // Directory: copy every entry that matches the rule-file extension
        // allow-list. We reuse `RuleSet::from_path` for parsing-only
        // validation, then iterate the directory ourselves to copy each
        // file verbatim (preserves comments / formatting).
        let _validated = RuleSet::from_path(&candidate).map_err(anyhow::Error::new)?;
        copy_directory(&candidate, &dest, args.force, args.symlink)?;
    }
    Ok(())
}

/// Materialise a list of `(filename, contents)` pairs into `dest`. When
/// `source_origin` is set, `--symlink` makes a symlink to that file rather
/// than writing `contents` (the `@std/*` path leaves it `None` and always
/// copies because the embedded files don't have a stable disk location).
#[cfg_attr(not(unix), allow(unused_variables))]
fn materialise_files(
    dest: &Utf8Path,
    files: &[(&str, &str)],
    force: bool,
    symlink: bool,
    source_origin: Option<&Utf8Path>,
) -> anyhow::Result<()> {
    fs::create_dir_all(dest.as_std_path()).map_err(|source| {
        anyhow::Error::new(dq_core::Error::Io {
            path: dest.to_path_buf(),
            source,
        })
    })?;

    for (name, body) in files {
        let target = dest.join(name);
        if target.exists() && !force {
            return Err(anyhow::Error::new(InvalidInput::new(format!(
                "destination already exists: {target} (pass --force to overwrite)"
            ))));
        }
        if force && target.exists() {
            fs::remove_file(target.as_std_path()).map_err(|source| {
                anyhow::Error::new(dq_core::Error::Io {
                    path: target.clone(),
                    source,
                })
            })?;
        }
        if symlink {
            // On Unix with an on-disk source we create a real symlink. In every
            // other case (embedded @std/* source on any platform, or non-Unix
            // platform with any source) we cannot symlink, so we warn the user
            // unconditionally and fall through to a plain write — they asked
            // for a symlink and need to know they got a copy instead.
            #[cfg(unix)]
            {
                if let Some(origin) = source_origin {
                    std::os::unix::fs::symlink(origin.as_std_path(), target.as_std_path())
                        .map_err(|source| {
                            anyhow::Error::new(dq_core::Error::Io {
                                path: target.clone(),
                                source,
                            })
                        })?;
                    continue;
                }
                tracing::warn!(
                    target = %target,
                    "--symlink ignored for embedded @std/* source; copying instead"
                );
            }
            #[cfg(not(unix))]
            tracing::warn!(
                target = %target,
                "--symlink not supported on this platform; copying instead"
            );
        }
        fs::write(target.as_std_path(), body).map_err(|source| {
            anyhow::Error::new(dq_core::Error::Io {
                path: target.clone(),
                source,
            })
        })?;
    }
    Ok(())
}

fn copy_directory(
    src: &Utf8Path,
    dest: &Utf8Path,
    force: bool,
    symlink: bool,
) -> anyhow::Result<()> {
    fs::create_dir_all(dest.as_std_path()).map_err(|source| {
        anyhow::Error::new(dq_core::Error::Io {
            path: dest.to_path_buf(),
            source,
        })
    })?;
    for entry in walkdir::WalkDir::new(src.as_std_path()).sort_by_file_name() {
        let entry = entry.map_err(|source| {
            anyhow::Error::new(dq_core::Error::Io {
                path: src.to_path_buf(),
                source: source.into(),
            })
        })?;
        if !entry.file_type().is_file() {
            continue;
        }
        let entry_path = match Utf8PathBuf::from_path_buf(entry.path().to_path_buf()) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if entry_path.file_name().is_none() {
            continue;
        }
        if !is_yaml(&entry_path) {
            continue;
        }
        let relative = entry_path.strip_prefix(src).map_err(|_| {
            anyhow::Error::new(InvalidInput::new(format!(
                "internal: walked path {entry_path} is not under {src}",
            )))
        })?;
        let target = dest.join(relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent.as_std_path()).map_err(|source| {
                anyhow::Error::new(dq_core::Error::Io {
                    path: parent.to_path_buf(),
                    source,
                })
            })?;
        }
        if target.exists() && !force {
            return Err(anyhow::Error::new(InvalidInput::new(format!(
                "destination already exists: {target} (pass --force to overwrite)"
            ))));
        }
        if force && target.exists() {
            fs::remove_file(target.as_std_path()).map_err(|source| {
                anyhow::Error::new(dq_core::Error::Io {
                    path: target.clone(),
                    source,
                })
            })?;
        }
        if symlink {
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(entry_path.as_std_path(), target.as_std_path())
                    .map_err(|source| {
                        anyhow::Error::new(dq_core::Error::Io {
                            path: target.clone(),
                            source,
                        })
                    })?;
                continue;
            }
            #[cfg(not(unix))]
            tracing::warn!("--symlink not supported on this platform; copying instead",);
        }
        fs::copy(entry_path.as_std_path(), target.as_std_path()).map_err(|source| {
            anyhow::Error::new(dq_core::Error::Io {
                path: target.clone(),
                source,
            })
        })?;
    }
    Ok(())
}

fn is_yaml(p: &Utf8Path) -> bool {
    matches!(p.extension(), Some("yml" | "yaml"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::JsonReporter;
    use camino::Utf8PathBuf;
    use tempfile::TempDir;

    #[test]
    fn list_emits_known_std_rules() {
        let list_args = RulesListArgs { namespace: None };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        run_list(&list_args, &reporter, &mut out).expect("list must succeed");
        let parsed: serde_json::Value = serde_json::from_slice(&out).expect("valid json");
        let arr = parsed.as_array().expect("top-level array");
        // The k8s std namespace ships at least the no-latest-tag rule.
        assert!(
            arr.iter()
                .any(|r| r["id"] == "k8s.no-latest-tag" && r["namespace"] == "k8s"),
            "expected k8s.no-latest-tag in output, got: {parsed}",
        );
    }

    #[test]
    fn list_with_namespace_filter_only_returns_matching_rows() {
        let list_args = RulesListArgs {
            namespace: Some("k8s".to_owned()),
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        run_list(&list_args, &reporter, &mut out).expect("list filtered");
        let parsed: serde_json::Value = serde_json::from_slice(&out).expect("json");
        let arr = parsed.as_array().expect("array");
        assert!(!arr.is_empty(), "k8s filter must produce at least one row");
        for row in arr {
            assert_eq!(row["namespace"], "k8s");
        }
    }

    fn temp_cwd() -> (TempDir, Utf8PathBuf) {
        let temp = TempDir::new().expect("tempdir");
        let cwd = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("UTF-8 path");
        (temp, cwd)
    }

    #[test]
    fn add_at_std_writes_files_under_dot_dq_rules_ns() {
        let (_t, cwd) = temp_cwd();
        let args = RulesAddArgs {
            ruleset: "@std/k8s".to_owned(),
            force: false,
            symlink: false,
        };
        run_add_in_cwd(&args, &cwd).expect("add must succeed");
        let dest = cwd.join(".dq").join("rules").join("k8s");
        assert!(dest.is_dir(), "destination directory must exist");
        assert!(
            dest.join("no-latest-tag.yml").is_file(),
            "rule file must be materialised",
        );
    }

    #[test]
    fn add_unknown_std_namespace_returns_invalid_input() {
        let (_t, cwd) = temp_cwd();
        let args = RulesAddArgs {
            ruleset: "@std/totally-missing".to_owned(),
            force: false,
            symlink: false,
        };
        let err = run_add_in_cwd(&args, &cwd).expect_err("unknown std must error");
        assert!(err.downcast_ref::<InvalidInput>().is_some());
    }

    #[test]
    fn add_path_directory_preserves_nested_subdirs() {
        // Two source files share the same basename in different subdirs.
        // Without preserving the source-relative path they collide on the
        // destination (one overwrites the other or hits the --force guard).
        let (_t, cwd) = temp_cwd();
        let src = cwd.join("rules-src");
        let sub1 = src.join("sub1");
        let sub2 = src.join("sub2");
        fs::create_dir_all(sub1.as_std_path()).expect("mkdir sub1");
        fs::create_dir_all(sub2.as_std_path()).expect("mkdir sub2");
        let yaml1 = "id: nested.foo-one\n\
                     description: nested-one\n\
                     severity: info\n\
                     match:\n  format: yaml\n\
                     check:\n  jq: '.'\n  message: ok\n";
        let yaml2 = "id: nested.foo-two\n\
                     description: nested-two\n\
                     severity: info\n\
                     match:\n  format: yaml\n\
                     check:\n  jq: '.'\n  message: ok\n";
        fs::write(sub1.join("foo.yml").as_std_path(), yaml1).expect("write sub1/foo.yml");
        fs::write(sub2.join("foo.yml").as_std_path(), yaml2).expect("write sub2/foo.yml");

        let args = RulesAddArgs {
            ruleset: src.as_str().to_owned(),
            force: false,
            symlink: false,
        };
        run_add_in_cwd(&args, &cwd).expect("add path-based ruleset must succeed");

        let dest = cwd.join(".dq").join("rules").join("rules-src");
        assert!(
            dest.join("sub1").join("foo.yml").is_file(),
            "sub1/foo.yml must be preserved under its subdir",
        );
        assert!(
            dest.join("sub2").join("foo.yml").is_file(),
            "sub2/foo.yml must be preserved under its subdir",
        );
    }

    #[test]
    fn add_single_file_path_writes_to_dest_namespace() {
        // Single-file path source — exercises the call site that was leaking
        // strings via `Box::leak`. The fix replaces the leak with stack-borrowed
        // refs into the owned `body` / `file_name`; this test asserts the file
        // still lands at `<cwd>/.dq/rules/<file_stem>/<file_name>`.
        let (_t, cwd) = temp_cwd();
        let yaml = "id: local.foo\n\
                    description: local-test\n\
                    severity: info\n\
                    match:\n  format: yaml\n\
                    check:\n  jq: '.'\n  message: ok\n";
        let src = cwd.join("local-rule.yml");
        fs::write(src.as_std_path(), yaml).expect("write source rule");

        let args = RulesAddArgs {
            ruleset: src.as_str().to_owned(),
            force: false,
            symlink: false,
        };
        run_add_in_cwd(&args, &cwd).expect("single-file add must succeed");

        let dest = cwd
            .join(".dq")
            .join("rules")
            .join("local-rule")
            .join("local-rule.yml");
        assert!(dest.is_file(), "rule file must be materialised at {dest}");
        let written = fs::read_to_string(dest.as_std_path()).expect("read materialised file");
        assert_eq!(written, yaml, "materialised contents must match source");
    }

    #[test]
    fn add_at_std_without_force_rejects_existing_file() {
        let (_t, cwd) = temp_cwd();
        // First add succeeds.
        let args = RulesAddArgs {
            ruleset: "@std/k8s".to_owned(),
            force: false,
            symlink: false,
        };
        run_add_in_cwd(&args, &cwd).expect("first add must succeed");
        // Second add (no --force) must reject.
        let err = run_add_in_cwd(&args, &cwd).expect_err("second add must reject");
        assert!(err.downcast_ref::<InvalidInput>().is_some());
        assert!(err.to_string().contains("--force"));
        // With --force it succeeds.
        let forced = RulesAddArgs {
            ruleset: "@std/k8s".to_owned(),
            force: true,
            symlink: false,
        };
        run_add_in_cwd(&forced, &cwd).expect("forced add must succeed");
    }
}
