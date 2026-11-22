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
//! What it would write over or remove of its own source it refuses. The
//! source is read and resolved before anything is replaced, so a link that is
//! replaced may be on the way to it.
//!
//! Every file is written in full under a staging name, which the rule walk
//! leaves out ([`rule_dir::is_staging_name`]), before the first is renamed to
//! where it goes. A run that fails or is killed leaves each file as it was or
//! as it should be, and none of them short. That holds file by file: between
//! two renames the rule set is a mix of old and new files, and a rule that
//! moved from one file to another may be in both or in neither until the
//! command is run again.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use camino::{Utf8Component, Utf8Path, Utf8PathBuf};
use dq_exec::rule_dir::{self, WalkedFile};
use dq_exec::{RuleSet, RuleSource};
use tempfile::{NamedTempFile, TempPath};

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
/// - `InvalidInput` (exit 6) for unknown `@std/<ns>` namespaces, for a source
///   whose name makes no directory of its own under `.dq/rules`, for
///   destination collisions (without `--force`), and for a source that is the
///   destination, inside it or around it. Nothing is written then.
/// - `dq_exec::ExecError` for a `.dq` or `.dq/rules` that cannot be resolved
///   or is no directory, and for a source directory that does not load.
/// - `dq_core::Error::Io` (exit 5) when the source cannot be read, or what is
///   at the destination not looked at.
/// - `dq_core::Error::WriteIo` (exit 7) when a file or a directory cannot be
///   made or replaced. It says whether anything was replaced before.
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
        let dest = ruleset_dir(&rules_dir, name, Utf8Path::new(&args.ruleset))?;
        let files: Vec<NewFile> = files
            .iter()
            .map(|(name, text)| NewFile {
                target: dest.join(name),
                from: Origin::Embedded(text),
            })
            .collect();
        rule_dir::project_dir(cwd).map_err(anyhow::Error::new)?;
        ensure_inside(&rules_dir, &dest, &files)?;
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

    // Where the source really is, once and for all: what is removed for it
    // may be a link on the way to it.
    let source = fs::canonicalize(&candidate).map_err(|err| io_error(&candidate, err))?;
    // The destination namespace is the path's file stem (file case) or
    // directory name (dir case).
    let (dest, files) = if candidate.is_file() {
        // Text, as a rule is.
        fs::read_to_string(&candidate).map_err(|source| io_error(&candidate, source))?;
        let name = candidate.file_stem().unwrap_or("local");
        let dest = ruleset_dir(&rules_dir, name, &candidate)?;
        let file = NewFile {
            target: dest.join(candidate.file_name().unwrap_or("rules.yml")),
            from: Origin::File {
                found: candidate.as_std_path().to_path_buf(),
                real: source.clone(),
            },
        };
        (dest, vec![file])
    } else {
        // Directory: copy every entry that matches the rule-file extension
        // allow-list. We reuse `RuleSet::from_walk` for parsing-only
        // validation, then go over the same files ourselves to copy each
        // verbatim (preserves comments / formatting).
        let name = candidate.file_name().unwrap_or("local");
        let dest = ruleset_dir(&rules_dir, name, &candidate)?;
        let walked = rule_dir::files_under(&candidate, rule_dir::DanglingLinks::Refuse)
            .map_err(anyhow::Error::new)?;
        let _validated = RuleSet::from_walk(&candidate, &walked).map_err(anyhow::Error::new)?;
        let files = directory_files(&candidate, walked, &dest)?;
        (dest, files)
    };
    rule_dir::project_dir(cwd).map_err(anyhow::Error::new)?;
    ensure_inside(&rules_dir, &dest, &files)?;
    ensure_apart(&candidate, &source, &dest, &files)?;
    materialise(&dest, &files, args)
}

/// The directory of `rules_dir` that `source` is added as, by its `name`.
///
/// # Errors
///
/// `InvalidInput` when `name` is not exactly one component of a path (the
/// stem of `...yml` is `..`), is nothing but the dots and spaces that Windows
/// drops from the end of one, or is a staging name, whose rules would never
/// be loaded.
fn ruleset_dir(rules_dir: &Utf8Path, name: &str, source: &Utf8Path) -> anyhow::Result<Utf8PathBuf> {
    let mut components = Utf8Path::new(name).components();
    match (components.next(), components.next()) {
        (Some(Utf8Component::Normal(only)), None)
            if only == name
                && !name.trim_matches(['.', ' ']).is_empty()
                && !rule_dir::is_staging_name(name.as_ref()) =>
        {
            Ok(rules_dir.join(name))
        }
        _ => Err(anyhow::Error::new(InvalidInput::new(format!(
            "cannot add {source}: '{name}' does not name a directory for it in .dq/rules"
        )))),
    }
}

/// Refuse a plan that would write anything but `dest` in `rules_dir` and what
/// is in it.
fn ensure_inside(rules_dir: &Utf8Path, dest: &Utf8Path, files: &[NewFile]) -> anyhow::Result<()> {
    let is_inside = |path: &Utf8Path, dir: &Utf8Path| {
        path.strip_prefix(dir).is_ok_and(|rest| {
            let mut components = rest.components().peekable();
            components.peek().is_some()
                && components.all(|part| matches!(part, Utf8Component::Normal(_)))
        })
    };
    let outside = if is_inside(dest, rules_dir) {
        let mut targets = files.iter().map(|file| file.target.as_path());
        targets.find(|target| !is_inside(target, dest))
    } else {
        Some(dest)
    };
    match outside {
        None => Ok(()),
        Some(path) => Err(anyhow::Error::new(InvalidInput::new(format!(
            "destination is outside {rules_dir}: {path}"
        )))),
    }
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
    File {
        /// Where the source has it, links as they are.
        found: PathBuf,
        /// The file itself, by an absolute path without a link: that is what
        /// is copied, or what the link leads to.
        real: PathBuf,
    },
}

/// The rule files a run over `src` would load of those `walked` in it, and the
/// fixtures there, at the same places under `dest`.
///
/// A rule file that goes by several names is loaded once, so it is there
/// once, under the first of them as for the loader: every name would be a
/// file of its own in `dest`, and report each finding again. A fixture beside
/// one of the other names has no rule to test in `dest` and is left out, with
/// a warning unless the same file is the rule's fixture under its name there.
fn directory_files(
    src: &Utf8Path,
    walked: Vec<WalkedFile>,
    dest: &Utf8Path,
) -> anyhow::Result<Vec<NewFile>> {
    // Every rule file by where it is, and the names it is not added under.
    let mut rules: HashMap<PathBuf, Utf8PathBuf> = HashMap::new();
    let mut other_names: HashMap<Utf8PathBuf, Utf8PathBuf> = HashMap::new();
    let mut found: Vec<(Utf8PathBuf, WalkedFile, PathBuf)> = Vec::new();
    for file in walked {
        // `RuleSet::from_walk` has refused a non-UTF-8 path already.
        let Some(entry) = Utf8Path::from_path(&file.path) else {
            continue;
        };
        let entry = entry.to_path_buf();
        let is_rule = rule_dir::is_rule_file(&entry);
        if !is_rule && !rule_dir::is_fixture_file(&entry) {
            continue;
        }
        let real = file.real().map_err(anyhow::Error::new)?;
        if is_rule {
            if let Some(first) = rules.get(&real) {
                other_names.insert(entry, first.clone());
                continue;
            }
            rules.insert(real.clone(), entry.clone());
        }
        found.push((entry, file, real));
    }
    // A fixture that is left out, by the name its rule is added under.
    let rule_elsewhere = |entry: &Utf8Path| {
        rule_dir::parent_rule_path(entry).and_then(|rule| other_names.get(&rule))
    };
    let fixtures: HashSet<(Utf8PathBuf, &Path)> = found
        .iter()
        .filter_map(|(entry, _, real)| Some((rule_dir::parent_rule_path(entry)?, real.as_path())))
        .collect();
    let mut files = Vec::new();
    for (entry, file, real) in &found {
        if let Some(rule) = rule_elsewhere(entry) {
            if !fixtures.contains(&(rule.clone(), real.as_path())) {
                tracing::warn!(
                    "left out {}: its rule is added as {}, the first of its names",
                    dq_core::one_line(entry.as_str()),
                    dq_core::one_line(rule.as_str())
                );
            }
            continue;
        }
        let relative = entry.strip_prefix(src).map_err(|_| {
            anyhow::Error::new(InvalidInput::new(format!(
                "internal: walked path {entry} is not under {src}",
            )))
        })?;
        files.push(NewFile {
            target: dest.join(relative),
            from: Origin::File {
                real: real.clone(),
                found: file.location.clone(),
            },
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
///
/// `real_source` is where `source` really is.
fn ensure_apart(
    source: &Utf8Path,
    real_source: &Path,
    dest: &Utf8Path,
    files: &[NewFile],
) -> anyhow::Result<()> {
    let refused = || {
        anyhow::Error::new(InvalidInput::new(format!(
            "cannot add {source} to {dest}: one is inside the other, or they are the same"
        )))
    };
    // Where a link at `dest` leads, and where the link itself is.
    let real_dests = [real_path(dest.as_std_path()), own_path(dest.as_std_path())];
    let in_dest = |path: &Path| real_dests.iter().any(|dest| path.starts_with(dest));
    if in_dest(real_source) || real_dests.iter().any(|dest| dest.starts_with(real_source)) {
        return Err(refused());
    }
    for file in files {
        let Origin::File { found, real } = &file.from else {
            continue;
        };
        let target = own_path(file.target.as_std_path());
        if target.starts_with(real_source)
            || target == *real
            || link_chain(found).iter().any(|at| in_dest(at))
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
/// looked at. What else is there stays.
fn materialise(dest: &Utf8Path, files: &[NewFile], args: &RulesAddArgs) -> anyhow::Result<()> {
    let links = links_in_the_way(dest, files, args.force)?;
    let contents = files
        .iter()
        .map(|file| contents(file, args.symlink))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let staged = Staged::write(dest, files, contents, &links)
        .map_err(|failed| failed.into_error(NOTHING_REPLACED))?;
    staged.ensure_distinct()?;
    staged.commit(args.force)
}

/// What a failure ends with that came before the first rename.
const NOTHING_REPLACED: &str = "nothing was replaced";

/// What a file is made with, read.
enum Contents {
    Bytes {
        bytes: Cow<'static, [u8]>,
        /// Those of the file it is a copy of.
        permissions: Option<fs::Permissions>,
    },
    /// A symbolic link there.
    #[cfg(unix)]
    LinkTo(PathBuf),
}

/// What to make `file` with, its origin looked at as far as that takes.
///
/// `symlink` gives a link on Unix with an on-disk source. In every other case
/// (embedded `@std/*` source on any platform, or non-Unix platform with any
/// source) there is none to make: the user asked for one and is warned that a
/// copy takes its place.
fn contents(file: &NewFile, symlink: bool) -> anyhow::Result<Contents> {
    match &file.from {
        Origin::Embedded(text) => {
            if symlink {
                tracing::warn!(
                    target = %file.target,
                    "--symlink ignored for embedded @std/* source; copying instead"
                );
            }
            Ok(Contents::Bytes {
                bytes: Cow::Borrowed(text.as_bytes()),
                permissions: None,
            })
        }
        #[cfg(unix)]
        Origin::File { real, .. } if symlink => {
            fs::File::open(real).map_err(|source| std_io_error(real, source))?;
            Ok(Contents::LinkTo(real.clone()))
        }
        Origin::File { real, .. } => {
            if symlink {
                tracing::warn!(
                    target = %file.target,
                    "--symlink not supported on this platform; copying instead"
                );
            }
            let bytes = fs::read(real).map_err(|source| std_io_error(real, source))?;
            let metadata = fs::metadata(real).map_err(|source| std_io_error(real, source))?;
            Ok(Contents::Bytes {
                bytes: Cow::Owned(bytes),
                permissions: Some(metadata.permissions()),
            })
        }
    }
}

/// A file or directory that could not be written.
struct WriteFailed {
    path: Utf8PathBuf,
    source: io::Error,
}

impl WriteFailed {
    /// For `map_err`, of what is done for `path`.
    fn at(path: &Utf8Path) -> impl FnOnce(io::Error) -> Self {
        let path = path.to_path_buf();
        move |source| Self { path, source }
    }

    /// The error of the run, which ends with `state`: what became of the
    /// rules already there.
    fn into_error(self, state: &str) -> anyhow::Error {
        let Self { path, source } = self;
        let source = io::Error::new(source.kind(), format!("{source}; {state}"));
        anyhow::Error::new(dq_core::Error::WriteIo { path, source })
    }
}

/// Everything a run makes, written under temporary names beside where it
/// goes. Dropped, it leaves none of it.
struct Staged {
    /// The directory of the rule set.
    dest: Utf8PathBuf,
    files: Vec<StagedFile>,
    dirs: Vec<StagedDir>,
}

/// A file, or a link, to rename to its name.
struct StagedFile {
    target: Utf8PathBuf,
    temp: TempPath,
    /// Where it goes: the place of `target`, or in a [`StagedDir`].
    place: PathBuf,
    /// Whether `place` is in a [`StagedDir`].
    in_staged_dir: bool,
}

/// A directory to put in place of a link.
struct StagedDir {
    target: Utf8PathBuf,
    temp: StagingDir,
    /// Where the link is.
    link: PathBuf,
}

impl Staged {
    /// Write `files`, made with `contents`, for `dest`. `links` are the links
    /// to replace by a directory.
    ///
    /// Nothing that is there changes, but directories that are missing are
    /// made: an empty one holds no rule.
    fn write(
        dest: &Utf8Path,
        files: &[NewFile],
        contents: Vec<Contents>,
        links: &BTreeSet<Utf8PathBuf>,
    ) -> Result<Self, WriteFailed> {
        // Resolved before a link on the way is replaced, as the source is.
        let real_dest = own_path(dest.as_std_path());
        let place = |path: &Utf8Path| match path.strip_prefix(dest) {
            Ok(rest) if !rest.as_str().is_empty() => real_dest.join(rest),
            _ => real_dest.clone(),
        };
        let mut staged = Self {
            dest: dest.to_path_buf(),
            files: Vec::new(),
            dirs: Vec::new(),
        };
        if !links.contains(dest) {
            fs::create_dir_all(&real_dest).map_err(WriteFailed::at(dest))?;
        }
        for link in links {
            let place = place(link);
            staged.dirs.push(StagedDir {
                target: link.clone(),
                temp: StagingDir::new_in(parent(&place)).map_err(WriteFailed::at(link))?,
                link: place,
            });
        }
        for (file, contents) in files.iter().zip(contents) {
            let dir = staged.dirs.iter().find_map(|dir| {
                let rest = file.target.strip_prefix(&dir.target).ok()?;
                Some(dir.temp.path.join(rest))
            });
            let in_staged_dir = dir.is_some();
            let place = dir.unwrap_or_else(|| place(&file.target));
            let write = || {
                fs::create_dir_all(parent(&place))?;
                write_beside(&place, &contents)
            };
            staged.files.push(StagedFile {
                temp: write().map_err(WriteFailed::at(&file.target))?,
                target: file.target.clone(),
                place,
                in_staged_dir,
            });
        }
        Ok(staged)
    }

    /// Refuse files whose names are one where they go: a file system that
    /// takes `A.yml` for `a.yml` keeps only the last. Found out by making an
    /// empty file for each beside them.
    ///
    /// # Errors
    ///
    /// `InvalidInput` naming two such files, `dq_core::Error::WriteIo` when
    /// the empty ones cannot be made.
    fn ensure_distinct(&self) -> anyhow::Result<()> {
        let failed = |err| WriteFailed::at(&self.dest)(err).into_error(NOTHING_REPLACED);
        // In the directory of the rule set, as names in it are told apart.
        let inside = match self.dirs.iter().find(|dir| dir.target == self.dest) {
            Some(dir) => dir.temp.path.clone(),
            None => own_path(self.dest.as_std_path()),
        };
        let probe = StagingDir::new_in(&inside).map_err(failed)?;
        for file in &self.files {
            let Ok(name) = file.target.strip_prefix(&self.dest) else {
                continue;
            };
            let path = probe.path.join(name);
            let made = fs::create_dir_all(parent(&path))
                .and_then(|()| fs::File::create_new(&path))
                .and_then(|mut made| made.write_all(name.as_str().as_bytes()));
            match made {
                Ok(()) => {}
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::AlreadyExists | io::ErrorKind::NotADirectory
                    ) =>
                {
                    let other = match fs::read_to_string(&path) {
                        Ok(other) => format!("{other} and {name} are"),
                        Err(_) => format!("{name} and another file's name are"),
                    };
                    return Err(anyhow::Error::new(InvalidInput::new(format!(
                        "{other} one name in {}; {NOTHING_REPLACED}",
                        self.dest
                    ))));
                }
                Err(err) => return Err(failed(err)),
            }
        }
        Ok(())
    }

    /// Rename everything to where it goes; over what is there with `force`.
    fn commit(self, force: bool) -> anyhow::Result<()> {
        let mixed = format!("{} may hold both old and new files", self.dest);
        let mut replaced = false;
        let state = |replaced| if replaced { &mixed } else { NOTHING_REPLACED };
        for file in self.files {
            let StagedFile {
                target,
                temp,
                place,
                in_staged_dir,
            } = file;
            let renamed = if force {
                temp.persist(&place).map_err(|err| err.error)
            } else {
                rename_to_new(temp, &place)
            };
            match renamed {
                Ok(()) => replaced |= !in_staged_dir,
                Err(err) if !force && err.kind() == io::ErrorKind::AlreadyExists => {
                    return Err(anyhow::Error::new(InvalidInput::new(format!(
                        "{}; {}",
                        already_exists(&target),
                        state(replaced)
                    ))));
                }
                Err(err) => return Err(WriteFailed::at(&target)(err).into_error(state(replaced))),
            }
        }
        for dir in self.dirs {
            let target = dir.target.clone();
            let failed = |err, replaced| WriteFailed::at(&target)(err).into_error(state(replaced));
            let aside = dir.set_aside().map_err(|err| failed(err, replaced))?;
            dir.move_in(aside)
                .and_then(|old| {
                    replaced = true;
                    remove_old_link(old)
                })
                .map_err(|err| failed(err, replaced))?;
        }
        Ok(())
    }
}

/// Rename `temp` to `place`, unless something is there.
///
/// # Errors
///
/// `AlreadyExists` then, and any error of the rename.
fn rename_to_new(temp: TempPath, place: &Path) -> io::Result<()> {
    let Err(failed) = temp.persist_noclobber(place) else {
        return Ok(());
    };
    // Not every file system can do it in one step: looked at first, then.
    match fs::symlink_metadata(place) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            failed.path.persist(place).map_err(|err| err.error)
        }
        _ => Err(failed.error),
    }
}

impl StagedDir {
    /// Move the link out of the way, to a name that is no staging name: until
    /// the directory is in its place, the rules behind it are loaded under
    /// that.
    fn set_aside(&self) -> io::Result<TempPath> {
        Ok(tempfile::Builder::new()
            .prefix(".dq-old-")
            .make_in(parent(&self.link), |aside| {
                match fs::symlink_metadata(aside) {
                    Ok(_) => Err(io::ErrorKind::AlreadyExists.into()),
                    Err(_) => fs::rename(&self.link, aside),
                }
            })?
            .into_temp_path())
    }

    /// Put the directory where the link was, which is `aside` now, and the
    /// link back when that fails. Gives `aside`, which is left to remove.
    fn move_in(self, aside: TempPath) -> io::Result<TempPath> {
        let Err(err) = fs::rename(&self.temp.path, &self.link) else {
            return Ok(aside);
        };
        let Err(back) = aside.persist(&self.link) else {
            return Err(err);
        };
        // Better under another name than not at all.
        let left = format!("{err}; the link is now {}", back.path.display());
        let _ = back.path.keep();
        Err(io::Error::new(err.kind(), left))
    }
}

/// Remove the link a directory has taken the place of.
fn remove_old_link(old: TempPath) -> io::Result<()> {
    remove_entry(&old).map_err(|err| {
        let left = format!(
            "{err}; the link it replaces is now {} and may be deleted",
            old.display()
        );
        // Where the message says.
        let _ = old.keep();
        io::Error::new(err.kind(), left)
    })
}

/// A directory for the time of a run, removed with what is in it.
struct StagingDir {
    path: PathBuf,
}

impl StagingDir {
    /// Make one in `dir`.
    fn new_in(dir: &Path) -> io::Result<Self> {
        // `tempdir_in` puts the path it tried in its error.
        let made = temporary()
            .disable_cleanup(true)
            .make_in(dir, |path| fs::create_dir(path))?;
        Ok(Self {
            path: made.path().to_path_buf(),
        })
    }
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        // Nothing is there once it is renamed to where it goes.
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// The names of what is there for the time of a run:
/// [`rule_dir::is_staging_name`].
fn temporary() -> tempfile::Builder<'static, 'static> {
    let mut names = tempfile::Builder::new();
    names
        .prefix(rule_dir::STAGING_PREFIX)
        .rand_bytes(rule_dir::STAGING_RANDOM);
    names
}

/// The directory `place` is in.
fn parent(place: &Path) -> &Path {
    place.parent().unwrap_or(place)
}

/// Make `contents` under a temporary name beside `place`.
fn write_beside(place: &Path, contents: &Contents) -> io::Result<TempPath> {
    match contents {
        Contents::Bytes { bytes, permissions } => {
            let mut temp = create_beside(place, permissions.as_ref())?;
            temp.as_file_mut().write_all(bytes)?;
            Ok(temp.into_temp_path())
        }
        #[cfg(unix)]
        Contents::LinkTo(real) => Ok(temporary()
            .make_in(parent(place), |path| std::os::unix::fs::symlink(real, path))?
            .into_temp_path()),
    }
}

/// Make an empty file under a temporary name beside `place`: with
/// `permissions`, whatever the umask, as [`fs::copy`] does, and else with the
/// mode of any new file (tempfile's own are for the user alone).
///
/// It has them before anything is in it, and never more: what the source
/// keeps from others its copy does too, at any moment.
fn create_beside(
    place: &Path,
    permissions: Option<&fs::Permissions>,
) -> io::Result<NamedTempFile<fs::File>> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if let Some(permissions) = permissions {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        options.mode(permissions.mode() & 0o777);
    }
    let temp = temporary().make_in(parent(place), |path| options.open(path))?;
    if let Some(permissions) = permissions {
        temp.as_file().set_permissions(permissions.clone())?;
    }
    Ok(temp)
}

/// The links under `dest` that stand where a directory must be for `files`.
///
/// Every step from `dest` down to each file is looked at without following
/// it, so a link to a directory is in the way like a file is, and one that
/// leads nowhere like one that does. What is at a file's own place needs no
/// room: the file is renamed over it.
///
/// # Errors
///
/// `InvalidInput` when something is in the way without `force`, and for a
/// file where a directory must be, or the other way round.
fn links_in_the_way(
    dest: &Utf8Path,
    files: &[NewFile],
    force: bool,
) -> anyhow::Result<BTreeSet<Utf8PathBuf>> {
    let mut links = BTreeSet::new();
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
                already_exists(step)
            } else {
                if !is_target {
                    links.insert(step.to_path_buf());
                }
                break;
            };
            return Err(anyhow::Error::new(InvalidInput::new(refused)));
        }
    }
    Ok(links)
}

/// What is said of a destination that is there without `--force`.
fn already_exists(path: &Utf8Path) -> String {
    format!("destination already exists: {path} (pass --force to overwrite)")
}

/// Remove a file, or a link and not what it leads to.
fn remove_entry(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        // Windows removes a link to a directory as a directory; neither call
        // follows it.
        Err(_) if cfg!(windows) && path.is_symlink() => fs::remove_dir(path),
        removed => removed,
    }
}

fn std_io_error(path: &Path, source: io::Error) -> anyhow::Error {
    io_error(Utf8Path::new(&path.to_string_lossy()), source)
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
    use pretty_assertions::assert_eq;
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
    fn a_ruleset_directory_is_one_component_under_the_rules() {
        let dir = |name| ruleset_dir(Utf8Path::new("r"), name, Utf8Path::new("src"));
        for name in ["k8s", ".yml", "..a", "a.", "a b", "local", ".dq-rules"] {
            assert_eq!(dir(name).expect(name), Utf8Path::new("r").join(name));
        }
        for name in [
            ".dq-abcdef",
            "",
            ".",
            "..",
            "...",
            ".. ",
            " ",
            "a/b",
            "/",
            "/a",
            "../a",
            "a/..",
            "./a",
            "a/",
            "a/.",
        ] {
            let err = dir(name).expect_err(name);
            assert!(err.downcast_ref::<InvalidInput>().is_some(), "{name:?}");
        }
        if cfg!(windows) {
            for name in ["a\\b", "c:", "c:a"] {
                dir(name).expect_err(name);
            }
        }
    }

    #[test]
    fn a_plan_stays_under_the_rules() {
        let plan = |dest: &str, target: &str| {
            let file = NewFile {
                target: Utf8PathBuf::from(target),
                from: Origin::Embedded(""),
            };
            ensure_inside(Utf8Path::new("q/rules"), Utf8Path::new(dest), &[file])
        };
        plan("q/rules/a", "q/rules/a/b.yml").expect("inside");
        plan("q/rules/a", "q/rules/a/b/c.yml").expect("inside");
        for (dest, target) in [
            ("q/rules", "q/rules/b.yml"),
            ("q/rules/..", "q/rules/../b.yml"),
            ("q/rules/a/..", "q/rules/a/../b.yml"),
            ("q/rules/a", "q/rules/a"),
            ("q/rules/a", "q/rules/a/../b.yml"),
            ("q/rules/a", "q/rules/b/b.yml"),
            ("q/other", "q/other/b.yml"),
        ] {
            let err = plan(dest, target).expect_err(target);
            assert!(err.downcast_ref::<InvalidInput>().is_some(), "{target}");
        }
        // A plan without a file makes `dest` too.
        let rules = Utf8Path::new("q/rules");
        ensure_inside(rules, rules, &[]).expect_err("the rules themselves");
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

    /// `a.yml`, `b.yml` and `c.yml` from embedded text, staged in `cwd` over
    /// old ones.
    fn staged(cwd: &Utf8Path) -> Staged {
        let dest = cwd.join(".dq/rules/set");
        fs::create_dir_all(&dest).expect("mkdir");
        let files: Vec<NewFile> = ["a.yml", "b.yml", "c.yml"]
            .map(|name| {
                fs::write(dest.join(name), "old").expect("write");
                NewFile {
                    target: dest.join(name),
                    from: Origin::Embedded("new"),
                }
            })
            .into();
        let contents = files
            .iter()
            .map(|file| contents(file, false).expect("contents"))
            .collect();
        match Staged::write(&dest, &files, contents, &BTreeSet::new()) {
            Ok(staged) => staged,
            Err(err) => panic!("{}", err.into_error("staging")),
        }
    }

    /// The names in `dir`, in order.
    fn names(dir: &Utf8Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .expect("read dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .into_string()
                    .expect("UTF-8")
            })
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_rename_that_fails_leaves_each_file_old_or_new() {
        for (blocked, replaced) in [("a.yml", 0), ("b.yml", 1), ("c.yml", 2)] {
            let (_t, cwd) = temp_cwd();
            let staged = staged(&cwd);
            let dest = cwd.join(".dq/rules/set");
            // Nothing is in place before all of it is written.
            assert_eq!(names(&dest).len(), 6);
            // What no file is renamed over.
            fs::remove_file(dest.join(blocked)).expect("rm");
            fs::create_dir_all(dest.join(blocked).join("x")).expect("mkdir");
            let err = staged.commit(true).expect_err(blocked);
            assert_eq!(crate::exit_code::exit_code_for_error(&err), 7);
            let text = err.to_string();
            let named = format!("I/O error writing '{}': ", dest.join(blocked));
            assert!(text.starts_with(&named), "{text}");
            let state = match replaced {
                0 => "; nothing was replaced".to_owned(),
                _ => format!("; {dest} may hold both old and new files"),
            };
            assert!(text.ends_with(&state), "{text}");
            assert_eq!(names(&dest), ["a.yml", "b.yml", "c.yml"]);
            for (at, name) in ["a.yml", "b.yml", "c.yml"].iter().enumerate() {
                if *name != blocked {
                    let text = fs::read_to_string(dest.join(name)).expect("read");
                    let expected = if at < replaced { "new" } else { "old" };
                    assert_eq!(text, expected, "{blocked}: {name}");
                }
            }
        }
    }

    #[test]
    fn what_is_staged_and_dropped_leaves_nothing() {
        let (_t, cwd) = temp_cwd();
        drop(staged(&cwd));
        let dest = cwd.join(".dq/rules/set");
        assert_eq!(names(&dest), ["a.yml", "b.yml", "c.yml"]);
        staged(&cwd).commit(true).expect("commit");
        assert_eq!(names(&dest), ["a.yml", "b.yml", "c.yml"]);
        for name in names(&dest) {
            assert_eq!(fs::read_to_string(dest.join(name)).expect("read"), "new");
        }
    }

    #[test]
    fn a_file_that_appears_in_between_is_not_replaced_without_force() {
        for (blocked, before) in [("a.yml", false), ("b.yml", true)] {
            let (_t, cwd) = temp_cwd();
            let dest = cwd.join(".dq/rules/set");
            let staged = staged(&cwd);
            for name in ["a.yml", "b.yml", "c.yml"] {
                fs::remove_file(dest.join(name)).expect("rm");
            }
            fs::write(dest.join(blocked), "mine").expect("write");
            let err = staged.commit(false).expect_err(blocked);
            assert_eq!(crate::exit_code::exit_code_for_error(&err), 6);
            let state = if before {
                format!("{dest} may hold both old and new files")
            } else {
                "nothing was replaced".to_owned()
            };
            assert_eq!(
                err.to_string(),
                format!(
                    "destination already exists: {} (pass --force to overwrite); {state}",
                    dest.join(blocked)
                )
            );
            let text = fs::read_to_string(dest.join(blocked)).expect("read");
            assert_eq!(text, "mine");
            assert_eq!(dest.join("a.yml").exists(), true);
            assert_eq!(dest.join("c.yml").exists(), false);
        }
        // With nothing there every file is put in place.
        let (_t, cwd) = temp_cwd();
        let staged = staged(&cwd);
        let dest = cwd.join(".dq/rules/set");
        for name in names(&dest).iter().filter(|name| name.ends_with(".yml")) {
            fs::remove_file(dest.join(name)).expect("rm");
        }
        staged.commit(false).expect("commit");
        assert_eq!(names(&dest), ["a.yml", "b.yml", "c.yml"]);
    }

    #[test]
    fn names_that_are_one_to_the_file_system_are_refused() {
        for (one, two) in [
            ("a.yml", "A.yml"),
            ("sub/a.yml", "Sub/a.yml"),
            ("a/b/c.yml", "A/B/C.yml"),
            ("d.yml", "D.yml/e.yml"),
            ("D.yml/e.yml", "d.yml"),
        ] {
            let (_t, cwd) = temp_cwd();
            fs::write(cwd.join("probe"), "").expect("write");
            let folds = cwd.join("PROBE").exists();
            let dest = cwd.join(".dq/rules/set");
            let files = [one, two].map(|name| NewFile {
                target: dest.join(name),
                from: Origin::Embedded("new"),
            });
            let args = |force| RulesAddArgs {
                ruleset: "set".to_owned(),
                force,
                symlink: false,
            };
            for force in [false, true] {
                let made = materialise(&dest, &files, &args(force));
                if !folds {
                    // Two names, two files.
                    made.expect("distinct");
                    fs::remove_dir_all(&dest).expect("rm");
                    continue;
                }
                let err = made.expect_err(two);
                let text = err.to_string();
                // A file in place of a directory is seen when it is staged.
                let code = crate::exit_code::exit_code_for_error(&err);
                assert!(code == 6 || text.contains("I/O error writing"), "{text}");
                if !one.contains("D.yml") && !two.contains("D.yml") {
                    let expected =
                        format!("{one} and {two} are one name in {dest}; nothing was replaced");
                    assert_eq!((code, text), (6, expected));
                }
                for entry in walkdir::WalkDir::new(&dest) {
                    let entry = entry.expect("entry");
                    assert!(entry.file_type().is_dir(), "{entry:?}");
                    let name = entry.file_name().to_string_lossy().into_owned();
                    assert!(!name.starts_with(".dq-"), "{name}");
                }
            }
        }
    }

    /// `.dq/rules/set`, a link to `../../old`, and what is staged to replace
    /// it.
    #[cfg(unix)]
    fn staged_over_a_link(cwd: &Utf8Path) -> (Utf8PathBuf, Staged) {
        let rules = cwd.join(".dq/rules");
        fs::create_dir_all(&rules).expect("mkdir");
        let dest = rules.join("set");
        std::os::unix::fs::symlink("../../old", &dest).expect("link");
        let file = NewFile {
            target: dest.join("a.yml"),
            from: Origin::Embedded("new"),
        };
        let links = links_in_the_way(&dest, std::slice::from_ref(&file), true).expect("links");
        assert_eq!(links, BTreeSet::from([dest.clone()]));
        let new = vec![contents(&file, false).expect("contents")];
        let staged = Staged::write(&dest, &[file], new, &links)
            .map_err(|err| err.into_error("staging"))
            .expect("staged");
        (rules, staged)
    }

    #[cfg(unix)]
    #[test]
    fn a_link_is_put_back_when_the_directory_is_gone() {
        let (_t, cwd) = temp_cwd();
        let (rules, mut staged) = staged_over_a_link(&cwd);
        staged.files.clear();
        fs::remove_dir_all(&staged.dirs[0].temp.path).expect("rm");
        let err = staged.commit(true).expect_err("gone");
        assert_eq!(crate::exit_code::exit_code_for_error(&err), 7);
        assert_eq!(
            err.to_string(),
            format!(
                "I/O error writing '{rules}/set': No such file or directory (os error 2); \
                 nothing was replaced"
            )
        );
        assert_eq!(names(&rules), ["set"]);
        let target = fs::read_link(rules.join("set")).expect("the link");
        assert_eq!(target, Path::new("../../old"));
    }

    #[cfg(unix)]
    #[test]
    fn a_link_that_cannot_be_put_back_is_named() {
        let (_t, cwd) = temp_cwd();
        let (rules, mut staged) = staged_over_a_link(&cwd);
        staged.files.clear();
        let dir = staged.dirs.pop().expect("one");
        let aside = dir.set_aside().expect("aside");
        let name = aside.file_name().expect("a name").to_owned();
        // The walk goes on loading what is behind it.
        assert!(!rule_dir::is_staging_name(&name), "{name:?}");
        assert_eq!(names(&rules).len(), 2);
        // Over this neither goes.
        fs::create_dir_all(rules.join("set/x")).expect("mkdir");
        let err = dir.move_in(aside).expect_err("in the way").to_string();
        let left = fs::canonicalize(&rules).expect("real").join(&name);
        assert!(
            err.ends_with(&format!("; the link is now {}", left.display())),
            "{err}"
        );
        assert_eq!(names(&rules), [&name.into_string().expect("UTF-8"), "set"]);
        assert_eq!(fs::read_link(left).expect("link"), Path::new("../../old"));
    }

    #[cfg(unix)]
    #[test]
    fn an_old_link_that_cannot_be_removed_is_named() {
        use std::os::unix::fs::PermissionsExt;
        let (_t, cwd) = temp_cwd();
        let (rules, mut staged) = staged_over_a_link(&cwd);
        let file = staged.files.pop().expect("one");
        file.temp.persist(&file.place).expect("persist");
        let dir = staged.dirs.pop().expect("one");
        let old = dir.set_aside().expect("aside");
        let old = dir.move_in(old).expect("in place");
        let at = old.to_path_buf();
        let mode = |mode| {
            fs::set_permissions(&rules, fs::Permissions::from_mode(mode)).expect("chmod");
        };
        mode(0o555);
        let root = fs::write(rules.join("probe"), "").is_ok();
        let removed = remove_old_link(old);
        mode(0o755);
        if root {
            // Root writes everywhere.
            return;
        }
        let err = removed.expect_err("not writable").to_string();
        assert_eq!(
            err,
            format!(
                "Permission denied (os error 13); the link it replaces is now {} and may be \
                 deleted",
                at.display()
            )
        );
        assert_eq!(fs::read_link(&at).expect("link"), Path::new("../../old"));
        let text = fs::read_to_string(rules.join("set/a.yml")).expect("read");
        assert_eq!(text, "new");
    }

    #[cfg(unix)]
    #[test]
    fn a_staged_file_has_its_mode_before_its_content() {
        use std::os::unix::fs::PermissionsExt;
        let (_t, cwd) = temp_cwd();
        let place = cwd.join("a.yml");
        let mode_of = |path: &Path| {
            let metadata = fs::metadata(path).expect("metadata");
            metadata.permissions().mode() & 0o777
        };
        for mode in [0o600, 0o640, 0o444, 0o400, 0o666, 0o777] {
            let permissions = fs::Permissions::from_mode(mode);
            let mut temp = create_beside(place.as_std_path(), Some(&permissions)).expect("create");
            assert_eq!(mode_of(temp.path()), mode, "{mode:o}");
            // As fs::copy leaves it.
            let source = cwd.join("source");
            fs::write(&source, "text").expect("write");
            fs::set_permissions(&source, permissions).expect("chmod");
            fs::copy(&source, cwd.join("copy")).expect("copy");
            assert_eq!(mode_of(cwd.join("copy").as_std_path()), mode, "{mode:o}");
            for name in ["source", "copy"] {
                fs::remove_file(cwd.join(name)).expect("rm");
            }
            // Open for writing, whatever it says.
            temp.as_file_mut().write_all(b"text").expect("write");
        }
        fs::write(cwd.join("plain"), "").expect("write");
        let temp = create_beside(place.as_std_path(), None).expect("create");
        assert_eq!(
            mode_of(temp.path()),
            mode_of(cwd.join("plain").as_std_path())
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_link_that_cannot_be_replaced_stays() {
        use std::os::unix::fs::PermissionsExt;
        let (_t, cwd) = temp_cwd();
        let (rules, staged) = staged_over_a_link(&cwd);
        let dest = rules.join("set");
        let mode = |mode| {
            fs::set_permissions(&rules, fs::Permissions::from_mode(mode)).expect("chmod");
        };
        mode(0o555);
        let root = fs::write(rules.join("probe"), "").is_ok();
        let committed = staged.commit(true);
        mode(0o755);
        if root {
            // Root writes everywhere.
            return;
        }
        let err = committed.expect_err("not writable").to_string();
        assert!(err.ends_with("; nothing was replaced"), "{err}");
        let target = fs::read_link(&dest).expect("the link");
        assert_eq!(target, Path::new("../../old"));
    }
}
