//! `dq rules list` / `dq rules add` — manage rule sources.
//!
//! `list` walks every `@std/<ns>` ruleset plus `<cwd>/.dq/rules/` (if
//! present) and emits a JSON-shaped array through the configured reporter.
//!
//! `add @std/<ns>` materialises the embedded per-file rule YAML under
//! `./.dq/rules/<ns>/`. `--symlink` swaps the copy for a Unix symlink (and
//! falls back to copy on Windows with a warning); `--force` overwrites
//! existing destination files.
//!
//! `add` follows `.dq` and `.dq/rules` where they are links to directories,
//! and no link under `.dq/rules`: one in the way of a file counts as an
//! existing destination, which `--force` replaces and never writes through.
//! What it would write over or remove of its own source it refuses.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use camino::{Utf8Path, Utf8PathBuf};
use dq_exec::{RuleSet, RuleSource, rule_dir};

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
/// - `InvalidInput` (exit 6) for unknown `@std/<ns>` namespaces, for
///   destination collisions (without `--force`), and for a source that is the
///   destination, inside it or around it. Nothing is written then.
/// - `dq_exec::ExecError` for a `.dq` or `.dq/rules` that cannot be resolved
///   or is no directory, and for a source directory that does not load.
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
    let rules_dir = cwd.join(".dq").join("rules");
    if let Some(name) = args.ruleset.strip_prefix("@std/") {
        let files = dq_lint::std_rule_files(name).ok_or_else(|| {
            anyhow::Error::new(InvalidInput::new(
                format!("unknown @std namespace: {name}",),
            ))
        })?;
        let dest = rules_dir.join(name);
        let files: Vec<NewFile> = files
            .iter()
            .map(|(name, text)| NewFile {
                target: dest.join(name),
                from: Origin::Embedded(text),
            })
            .collect();
        rule_dir::project_dir(cwd).map_err(anyhow::Error::new)?;
        return materialise(&dest, &files, args);
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

    // The destination namespace is the path's file stem (file case) or
    // directory name (dir case).
    let (dest, files) = if candidate.is_file() {
        // Text, as a rule is, and readable before anything is removed for it.
        fs::read_to_string(&candidate).map_err(|source| io_error(&candidate, source))?;
        let dest = rules_dir.join(candidate.file_stem().unwrap_or("local"));
        let file = NewFile {
            target: dest.join(candidate.file_name().unwrap_or("rules.yml")),
            from: Origin::File(candidate.clone()),
        };
        (dest, vec![file])
    } else {
        // Directory: copy every entry that matches the rule-file extension
        // allow-list. We reuse `RuleSet::from_path` for parsing-only
        // validation, then iterate the directory ourselves to copy each
        // file verbatim (preserves comments / formatting).
        let _validated = RuleSet::from_path(&candidate).map_err(anyhow::Error::new)?;
        let dest = rules_dir.join(candidate.file_name().unwrap_or("local"));
        let files = directory_files(&candidate, &dest)?;
        (dest, files)
    };
    rule_dir::project_dir(cwd).map_err(anyhow::Error::new)?;
    ensure_apart(&candidate, &dest, &files)?;
    materialise(&dest, &files, args)
}

/// A file `rules add` makes.
struct NewFile {
    /// Where, under the ruleset's directory.
    target: Utf8PathBuf,
    /// What from.
    from: Origin,
}

/// Where the contents of a [`NewFile`] come from.
enum Origin {
    /// The text of an `@std/*` file, which is nowhere on disk to link to.
    Embedded(&'static str),
    /// A file to copy, or to link to under `--symlink`.
    File(Utf8PathBuf),
}

/// The rule files and fixtures a run over `src` would load, links followed,
/// at the same places under `dest`.
fn directory_files(src: &Utf8Path, dest: &Utf8Path) -> anyhow::Result<Vec<NewFile>> {
    let mut files = Vec::new();
    for entry in
        rule_dir::files_under(src, rule_dir::DanglingLinks::Refuse).map_err(anyhow::Error::new)?
    {
        // `RuleSet::from_path` has refused a non-UTF-8 path already.
        let Ok(entry) = Utf8PathBuf::from_path_buf(entry) else {
            continue;
        };
        if !rule_dir::is_rule_file(&entry) && !rule_dir::is_fixture_file(&entry) {
            continue;
        }
        let relative = entry.strip_prefix(src).map_err(|_| {
            anyhow::Error::new(InvalidInput::new(format!(
                "internal: walked path {entry} is not under {src}",
            )))
        })?;
        files.push(NewFile {
            target: dest.join(relative),
            from: Origin::File(entry),
        });
    }
    Ok(files)
}

/// Refuse to add `source` to `dest` where the one, links followed, is the
/// other or inside it, as a whole or by way of a link to one of `files`: the
/// source would be written over, or removed and linked to itself.
///
/// A link in `dest` to a file of the source does not count. That is what
/// `--symlink` makes, and replacing the link leaves the file alone.
fn ensure_apart(source: &Utf8Path, dest: &Utf8Path, files: &[NewFile]) -> anyhow::Result<()> {
    let refused = || {
        anyhow::Error::new(InvalidInput::new(format!(
            "cannot add {source} to {dest}: one is inside the other, or they are the same"
        )))
    };
    let real_source = real_path(source.as_std_path());
    // Where a link at `dest` leads, and where the link itself is.
    let real_dests = [real_path(dest.as_std_path()), own_path(dest.as_std_path())];
    let in_dest = |path: &Path| real_dests.iter().any(|dest| path.starts_with(dest));
    if in_dest(&real_source) || real_dests.iter().any(|dest| dest.starts_with(&real_source)) {
        return Err(refused());
    }
    for file in files {
        let Origin::File(origin) = &file.from else {
            continue;
        };
        let target = own_path(file.target.as_std_path());
        if target.starts_with(&real_source)
            || target == real_path(origin.as_std_path())
            || link_chain(origin.as_std_path())
                .iter()
                .any(|at| in_dest(at))
        {
            return Err(refused());
        }
    }
    Ok(())
}

/// `path` with every link on it followed, as far as there is something.
fn real_path(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| own_path(path))
}

/// Where the entry `path` names is: the links above it followed, and not the
/// entry itself when it is one.
fn own_path(path: &Path) -> PathBuf {
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => real_path(parent).join(name),
        _ => path.to_path_buf(),
    }
}

/// Where `path` is, and each link and the file it leads to when it is a link.
fn link_chain(path: &Path) -> Vec<PathBuf> {
    /// Past the number of links any system follows.
    const MAX_LINKS: usize = 64;
    let mut chain = vec![own_path(path)];
    while chain.len() <= MAX_LINKS
        && let Some(last) = chain.last()
        && let Ok(target) = fs::read_link(last)
    {
        let next = last.parent().unwrap_or(last).join(target);
        chain.push(own_path(&next));
    }
    chain
}

/// Make `files` under `dest`, after everything that could refuse it has been
/// looked at.
fn materialise(dest: &Utf8Path, files: &[NewFile], args: &RulesAddArgs) -> anyhow::Result<()> {
    for path in in_the_way(dest, files, args.force)? {
        remove_entry(&path).map_err(|source| io_error(&path, source))?;
    }
    create_dir(dest)?;
    for file in files {
        if let Some(parent) = file.target.parent() {
            create_dir(parent)?;
        }
        if !(args.symlink && link_to_origin(file)?) {
            let made = match &file.from {
                Origin::Embedded(text) => fs::write(&file.target, text),
                Origin::File(origin) => fs::copy(origin, &file.target).map(|_| ()),
            };
            made.map_err(|source| io_error(&file.target, source))?;
        }
    }
    Ok(())
}

/// The files and links to remove before `files` can be made under `dest`.
///
/// Every step from `dest` down to each file is looked at without following
/// it, so a link to a directory is in the way like a file is, and one that
/// leads nowhere like one that does.
///
/// # Errors
///
/// `InvalidInput` when something is in the way without `force`, and for a
/// file where a directory must be, or the other way round.
fn in_the_way(
    dest: &Utf8Path,
    files: &[NewFile],
    force: bool,
) -> anyhow::Result<BTreeSet<Utf8PathBuf>> {
    let mut remove = BTreeSet::new();
    for file in files {
        let mut steps: Vec<&Utf8Path> = file
            .target
            .ancestors()
            .take_while(|step| step.starts_with(dest))
            .collect();
        steps.reverse();
        for step in steps {
            let meta = match fs::symlink_metadata(step) {
                Ok(meta) => meta,
                Err(err) if err.kind() == io::ErrorKind::NotFound => break,
                Err(err) => return Err(io_error(step, err)),
            };
            let is_target = step == file.target;
            let refused = if meta.is_dir() && !is_target {
                continue;
            } else if meta.is_dir() {
                format!("destination is a directory: {step}")
            } else if !is_target && !meta.is_symlink() {
                format!("destination is not a directory: {step}")
            } else if !force {
                format!("destination already exists: {step} (pass --force to overwrite)")
            } else {
                remove.insert(step.to_path_buf());
                break;
            };
            return Err(anyhow::Error::new(InvalidInput::new(refused)));
        }
    }
    Ok(remove)
}

/// Remove a file, or a link and not what it leads to.
fn remove_entry(path: &Utf8Path) -> io::Result<()> {
    match fs::remove_file(path) {
        // Windows removes a link to a directory as a directory; neither call
        // follows it.
        Err(_) if cfg!(windows) && path.is_symlink() => fs::remove_dir(path),
        removed => removed,
    }
}

/// Make the target of `file` a link to its origin, and say whether it was
/// made.
///
/// On Unix with an on-disk source that is a real symlink. In every other case
/// (embedded `@std/*` source on any platform, or non-Unix platform with any
/// source) there is none to make: the user asked for one and is warned that a
/// copy takes its place.
fn link_to_origin(file: &NewFile) -> anyhow::Result<bool> {
    #[cfg(unix)]
    {
        if let Origin::File(origin) = &file.from {
            std::os::unix::fs::symlink(origin, &file.target)
                .map_err(|source| io_error(&file.target, source))?;
            return Ok(true);
        }
        tracing::warn!(
            target = %file.target,
            "--symlink ignored for embedded @std/* source; copying instead"
        );
    }
    #[cfg(not(unix))]
    tracing::warn!(
        target = %file.target,
        "--symlink not supported on this platform; copying instead"
    );
    Ok(false)
}

fn create_dir(path: &Utf8Path) -> anyhow::Result<()> {
    fs::create_dir_all(path).map_err(|source| io_error(path, source))
}

fn io_error(path: &Utf8Path, source: io::Error) -> anyhow::Error {
    anyhow::Error::new(dq_core::Error::Io {
        path: path.to_path_buf(),
        source,
    })
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
