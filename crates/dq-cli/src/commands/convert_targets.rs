//! The files `dq convert -i` is about to write, checked against each other
//! and against the sources before the first one is written.
//!
//! Each source is removed once its target is written, so a target that is a
//! source, or that two sources share, loses data; so does a backup that is a
//! source. Whether two names are one file is for the file system to say: it
//! may ignore case (`A.yaml` and `a.yml` both become `a.json` on macOS) or
//! Unicode normalisation, and a directory may have several spellings. The
//! targets do not exist yet, so the question is put to the sources, which do:
//! `A.json` and `a.json` are one file exactly when the names `A.yaml` and
//! `a.yaml` are. Nothing is created to find out.

#[cfg(unix)]
use std::io;

use camino::{Utf8Path, Utf8PathBuf};

/// Why a run of `convert -i` is refused.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Conflict {
    /// The file would be written over itself.
    Itself {
        /// The source, which is also its own target.
        source: Utf8PathBuf,
    },
    /// The target is there already, and is the source by another name.
    HardLink {
        /// The one name.
        source: Utf8PathBuf,
        /// The other.
        target: Utf8PathBuf,
    },
    /// The source is a link to a file the conversion would replace: its
    /// target, or the backup of that.
    Link {
        /// The link.
        source: Utf8PathBuf,
        /// The target or the backup.
        file: Utf8PathBuf,
    },
    /// Two sources would be written to one file.
    Shared {
        /// The earlier source and its target.
        first: (Utf8PathBuf, Utf8PathBuf),
        /// The later source and its target: the same file, though it may be
        /// spelled differently.
        second: (Utf8PathBuf, Utf8PathBuf),
    },
    /// The backup of a target would replace a source.
    Backup {
        /// The target that is there already.
        target: Utf8PathBuf,
        /// The source at the name of its backup.
        source: Utf8PathBuf,
    },
    /// Files are at the targets already, and nothing says that they may go.
    Existing {
        /// Those targets, one at least, in the order of the run.
        targets: Vec<Utf8PathBuf>,
    },
}

/// How many targets a message names.
const LISTED: usize = 3;

/// `the target 'a' already exists`, `the targets 'a', 'b' and 'c' already
/// exist`, `the targets 'a', 'b' and 5 others already exist`.
fn existing(targets: &[Utf8PathBuf]) -> String {
    let shown = if targets.len() > LISTED {
        LISTED - 1
    } else {
        targets.len()
    };
    let mut names: Vec<String> = targets[..shown].iter().map(|t| format!("'{t}'")).collect();
    match targets.len() - shown {
        0 => {}
        others => names.push(format!("{others} others")),
    }
    match names.as_slice() {
        [init @ .., last] if !init.is_empty() => {
            format!(
                "the targets {} and {last} already exist; pass --overwrite to replace them, \
                 or --backup to keep a copy of each",
                init.join(", ")
            )
        }
        names => format!(
            "the target {} already exists; pass --overwrite to replace it, or --backup to keep \
             a copy",
            names.concat()
        ),
    }
}

impl Conflict {
    /// The message of the refusal; `format` is the label of the output
    /// format.
    pub(super) fn message(&self, format: &str) -> String {
        let what = match self {
            Self::Itself { source } => {
                return format!(
                    "convert -i to {format}: target path '{source}' is identical to source — drop -F or pick a different format"
                );
            }
            Self::HardLink { source, target } => format!(
                "'{source}' and its target '{target}' are two names of one file (a hard link); remove one of them first"
            ),
            Self::Link { source, file } => format!(
                "'{source}' is a symbolic link to '{file}', which the conversion would replace; remove the link first"
            ),
            Self::Shared { first, second } => {
                let (source, target) = first;
                let (other, other_target) = second;
                let also = if target == other_target {
                    String::new()
                } else {
                    format!(" (the same file as '{other_target}' here)")
                };
                format!(
                    "'{source}' and '{other}' would both be written to '{target}'{also}; rename one of them first"
                )
            }
            Self::Backup { target, source } => format!(
                "the backup of '{target}' would replace '{source}', which is to be converted too; rename one of them first"
            ),
            Self::Existing { targets } => existing(targets),
        };
        format!("convert -i to {format}: {what}. Nothing was converted")
    }
}

/// The first conflict among `planned`, the `(source, target)` pairs of a run
/// in order, each target next to its source with another extension; `backup`
/// when the targets that exist are backed up. [`Conflict::Existing`] is not
/// looked for: whether it is one depends on the flags.
///
/// A target that is another source of the run needs no case of its own: it
/// has the extension the targets have, so that source is its own target.
pub(super) fn find_conflict(
    planned: &[(Utf8PathBuf, Utf8PathBuf)],
    backup: bool,
) -> Option<Conflict> {
    let sources = Sources::of(planned);
    sources
        .own_target()
        .or_else(|| sources.linked_target(backup))
        .or_else(|| sources.shared_target())
        .or_else(|| backup.then(|| sources.backed_up_over()).flatten())
}

/// Where a target that exists is backed up.
fn backup_of(target: &Utf8Path) -> Utf8PathBuf {
    Utf8PathBuf::from(format!("{target}.bak"))
}

#[cfg(unix)]
use unix::Sources;

#[cfg(unix)]
mod unix {
    use std::cell::RefCell;
    use std::collections::{BTreeSet, HashMap, HashSet};
    use std::ffi::OsString;
    use std::os::unix::fs::MetadataExt as _;

    use super::*;

    /// What a path names: the device and inode.
    type Entry = (u64, u64);

    /// A run, with what each source is, links not followed (a link is
    /// another file than the one it points to; hard links of a file are one),
    /// and the directory it is in, whatever that was called. `None` for a
    /// source that is not there: it is reported when it is read.
    pub(super) struct Sources<'a> {
        planned: &'a [(Utf8PathBuf, Utf8PathBuf)],
        entries: Vec<Option<Entry>>,
        directories: Vec<Option<Entry>>,
        /// The names in the directories that had to be listed: each is once,
        /// however many of its files are hard links.
        names: RefCell<HashMap<Entry, HashSet<OsString>>>,
    }

    fn entry(path: &Utf8Path) -> io::Result<Entry> {
        let found = std::fs::symlink_metadata(path.as_std_path())?;
        Ok((found.dev(), found.ino()))
    }

    fn parent(path: &Utf8Path) -> &Utf8Path {
        match path.parent() {
            Some(parent) if parent != "" => parent,
            _ => Utf8Path::new("."),
        }
    }

    /// The part of the file name of `path` that conversion replaces: its
    /// extension with the dot, or nothing.
    fn suffix(path: &Utf8Path) -> String {
        path.extension()
            .map_or_else(String::new, |extension| format!(".{extension}"))
    }

    /// The names the directory of `path` lists; none when it cannot be read.
    fn listed_beside(path: &Utf8Path) -> HashSet<OsString> {
        std::fs::read_dir(parent(path).as_std_path())
            .into_iter()
            .flatten()
            .filter_map(|entry| Some(entry.ok()?.file_name()))
            .collect()
    }

    /// What is left of `stem` in every spelling some file system takes for
    /// the same: the same key for all of those, and for others besides.
    ///
    /// File systems differ in case folding (full folding spells `ß` as `ss`
    /// and `ﬁ` as `fi`) and normalisation (`é` as `e` and a mark, the Kelvin
    /// sign as `K`). So only what no such rule makes or takes part in stays:
    /// ASCII digits and some letters, in one case, without a mark.
    pub(super) fn key(stem: &str) -> String {
        let mut key = String::new();
        let mut characters = stem.chars().peekable();
        while let Some(c) = characters.next() {
            let marked = characters
                .peek()
                .is_some_and(|next| ('\u{300}'..='\u{36f}').contains(next));
            let c = c.to_ascii_lowercase();
            if !marked && (c.is_ascii_digit() || "bcdeghjmopqruvwxyz".contains(c)) {
                key.push(c);
            }
        }
        key
    }

    impl<'a> Sources<'a> {
        pub(super) fn of(planned: &'a [(Utf8PathBuf, Utf8PathBuf)]) -> Self {
            let mut directories: HashMap<&Utf8Path, Option<Entry>> = HashMap::new();
            Self {
                planned,
                entries: planned
                    .iter()
                    .map(|(source, _)| entry(source).ok())
                    .collect(),
                directories: planned
                    .iter()
                    .map(|(source, _)| {
                        let parent = parent(source);
                        *directories.entry(parent).or_insert_with(|| {
                            let found = std::fs::metadata(parent.as_std_path()).ok()?;
                            Some((found.dev(), found.ino()))
                        })
                    })
                    .collect(),
                names: RefCell::default(),
            }
        }

        /// Whether `directory`, in which `path` is, lists its name as it is
        /// spelled.
        fn is_listed(&self, path: &Utf8Path, directory: Entry) -> bool {
            let mut names = self.names.borrow_mut();
            let names = names
                .entry(directory)
                .or_insert_with(|| listed_beside(path));
            path.file_name()
                .is_some_and(|name| names.contains(std::ffi::OsStr::new(name)))
        }

        pub(super) fn own_target(&self) -> Option<Conflict> {
            self.planned
                .iter()
                .zip(self.entries.iter().zip(&self.directories))
                .find_map(|((source, target), (found, directory))| {
                    let source = source.clone();
                    if source == *target {
                        return Some(Conflict::Itself { source });
                    }
                    if found.is_none() || entry(target).ok() != *found {
                        return None;
                    }
                    // Another spelling of the one name, or another name.
                    let listed = directory.is_some_and(|dir| self.is_listed(target, dir));
                    Some(if listed {
                        let target = target.clone();
                        Conflict::HardLink { source, target }
                    } else {
                        Conflict::Itself { source }
                    })
                })
        }

        /// The first source that is a link to its own target, or with
        /// `backup` to the backup of that.
        pub(super) fn linked_target(&self, backup: bool) -> Option<Conflict> {
            self.planned
                .iter()
                .zip(&self.entries)
                .find_map(|((source, target), found)| {
                    let file = std::fs::metadata(source.as_std_path()).ok()?;
                    let file = Some((file.dev(), file.ino()));
                    if file == *found {
                        return None; // no link
                    }
                    entry(target).ok()?;
                    let backup = backup.then(|| backup_of(target));
                    [Some(target.clone()), backup]
                        .into_iter()
                        .flatten()
                        .find(|path| entry(path).ok() == file)
                        .map(|file| Conflict::Link {
                            source: source.clone(),
                            file,
                        })
                })
        }

        /// The suffixes of the sources in a directory whose stems may be one
        /// to the file system, by the directory and the [`key`] of the stem.
        pub(super) fn suffixes(&self) -> HashMap<(Entry, String), BTreeSet<String>> {
            let mut suffixes: HashMap<_, BTreeSet<String>> = HashMap::new();
            for ((source, _), directory) in self.planned.iter().zip(&self.directories) {
                if let Some(directory) = directory {
                    let key = key(source.file_stem().unwrap_or_default());
                    let bucket = suffixes.entry((*directory, key)).or_default();
                    bucket.insert(suffix(source));
                }
            }
            suffixes
        }

        /// The first two sources that share a target, by the order of the
        /// later: an earlier source with the stem of this one answers to this
        /// one's spelling of it.
        pub(super) fn shared_target(&self) -> Option<Conflict> {
            let suffixes = self.suffixes();
            let mut seen = HashMap::new();
            for (later, (source, _)) in self.planned.iter().enumerate() {
                let (Some(found), Some(directory)) = (self.entries[later], self.directories[later])
                else {
                    continue;
                };
                let stem = source.file_stem().unwrap_or_default();
                for suffix in suffixes.get(&(directory, key(stem))).into_iter().flatten() {
                    let probe = parent(source).join(format!("{stem}{suffix}"));
                    if let Some(first) = self.named(&probe, directory, &seen) {
                        return Some(Conflict::Shared {
                            first: self.planned[first].clone(),
                            second: self.planned[later].clone(),
                        });
                    }
                }
                seen.entry(found).or_insert_with(Vec::new).push(later);
            }
            None
        }

        /// The first target that is there already and whose backup is a
        /// source.
        pub(super) fn backed_up_over(&self) -> Option<Conflict> {
            let mut all: HashMap<Entry, Vec<usize>> = HashMap::new();
            for (index, found) in self.entries.iter().enumerate() {
                if let Some(found) = found {
                    all.entry(*found).or_default().push(index);
                }
            }
            self.planned
                .iter()
                .zip(&self.directories)
                .find_map(|((_, target), directory)| {
                    entry(target).ok()?;
                    let backup = backup_of(target);
                    let source = self.named(&backup, (*directory)?, &all)?;
                    Some(Conflict::Backup {
                        target: target.clone(),
                        source: self.planned[source].0.clone(),
                    })
                })
        }

        /// The source among `known` that `probe`, a path in `directory`, is a
        /// name of.
        fn named(
            &self,
            probe: &Utf8Path,
            directory: Entry,
            known: &HashMap<Entry, Vec<usize>>,
        ) -> Option<usize> {
            let known = known.get(&entry(probe).ok()?)?;
            // A name that is there as it is spelled, and is not the source's,
            // is a hard link: another file.
            let listed = self.is_listed(probe, directory);
            known.iter().copied().find(|&index| {
                self.directories[index] == Some(directory)
                    && (!listed || self.planned[index].0.file_name() == probe.file_name())
            })
        }
    }
}

/// Without file identities: paths that are spelled alike.
#[cfg(not(unix))]
struct Sources<'a>(&'a [(Utf8PathBuf, Utf8PathBuf)]);

#[cfg(not(unix))]
impl<'a> Sources<'a> {
    fn of(planned: &'a [(Utf8PathBuf, Utf8PathBuf)]) -> Self {
        Self(planned)
    }

    fn own_target(&self) -> Option<Conflict> {
        let (source, _) = self.0.iter().find(|(source, target)| source == target)?;
        Some(Conflict::Itself {
            source: source.clone(),
        })
    }

    fn linked_target(&self, backup: bool) -> Option<Conflict> {
        self.0.iter().find_map(|(source, target)| {
            let source_is_link = source.as_std_path().symlink_metadata().ok()?.is_symlink();
            let file = source.as_std_path().canonicalize().ok()?;
            let backup = backup.then(|| backup_of(target));
            [Some(target.clone()), backup]
                .into_iter()
                .flatten()
                .find(|path| {
                    source_is_link
                        && target.exists()
                        && path.as_std_path().canonicalize().ok().as_ref() == Some(&file)
                })
                .map(|file| Conflict::Link {
                    source: source.clone(),
                    file,
                })
        })
    }

    fn shared_target(&self) -> Option<Conflict> {
        self.0.iter().enumerate().find_map(|(later, second)| {
            let first = self.0[..later].iter().find(|first| first.1 == second.1)?;
            Some(Conflict::Shared {
                first: first.clone(),
                second: second.clone(),
            })
        })
    }

    fn backed_up_over(&self) -> Option<Conflict> {
        self.0.iter().find_map(|(_, target)| {
            let backup = backup_of(target);
            let (source, _) = self.0.iter().find(|(source, _)| *source == backup)?;
            target.exists().then(|| Conflict::Backup {
                target: target.clone(),
                source: source.clone(),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(root: &Utf8Path, sources: &[&str]) -> Vec<(Utf8PathBuf, Utf8PathBuf)> {
        sources
            .iter()
            .map(|source| {
                let source = root.join(source);
                let target = source.with_extension("json");
                (source, target)
            })
            .collect()
    }

    /// A directory holding a file for each of `names`.
    fn directory_of(names: &[&str]) -> (tempfile::TempDir, Utf8PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("utf-8");
        for name in names {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(&path, "a: 1\n").expect("seed");
        }
        (dir, root)
    }

    /// The names of the two sources in conflict, from `root`.
    fn conflict(root: &Utf8Path, sources: &[&str]) -> Option<(String, String)> {
        let name = |path: &Utf8Path| path.strip_prefix(root).expect("in root").to_string();
        find_conflict(&plan(root, sources), true).map(|conflict| match conflict {
            Conflict::Itself { source } => (name(&source), name(&source)),
            Conflict::HardLink { source, target } | Conflict::Backup { target, source } => {
                (name(&target), name(&source))
            }
            Conflict::Link { source, file } => (name(&file), name(&source)),
            Conflict::Shared { first, second } => (name(&first.0), name(&second.0)),
            Conflict::Existing { .. } => unreachable!("not looked for"),
        })
    }

    fn pair(first: &str, second: &str) -> Option<(String, String)> {
        Some((first.to_owned(), second.to_owned()))
    }

    #[test]
    fn one_stem_in_one_directory_is_one_target() {
        let (_dir, root) = directory_of(&["a.yaml", "a.yml", "b.yaml", "sub/a.yml", "a.b.yml"]);
        assert_eq!(
            conflict(&root, &["a.yaml", "b.yaml", "a.yml"]),
            pair("a.yaml", "a.yml")
        );
        assert_eq!(
            conflict(&root, &["a.yml", "b.yaml", "a.yaml"]),
            pair("a.yml", "a.yaml")
        );
        assert_eq!(
            conflict(&root, &["a.b.yml", "a.yaml", "b.yaml", "sub/a.yml"]),
            None
        );
    }

    #[test]
    fn a_source_that_is_its_own_target_is_found_wherever_it_stands() {
        let (_dir, root) = directory_of(&["a.yaml", "b.json"]);
        assert_eq!(
            conflict(&root, &["a.yaml", "b.json"]),
            pair("b.json", "b.json")
        );
        assert_eq!(
            conflict(&root, &["a.yaml", "a.json"]),
            pair("a.json", "a.json"),
            "also one that is not there"
        );
    }

    #[test]
    fn a_source_that_is_not_there_is_left_to_the_reader() {
        let (_dir, root) = directory_of(&["a.yaml"]);
        assert_eq!(conflict(&root, &["a.yaml", "gone.yaml"]), None);
    }

    #[test]
    fn names_are_one_where_the_file_system_takes_them_for_one() {
        // Case, and an accented letter in one code point or in two.
        for (one, other) in [("A", "a"), ("\u{e9}", "e\u{301}")] {
            let (first, second) = (format!("{one}.yaml"), format!("{other}.yml"));
            let (_dir, root) = directory_of(&[&first, &second]);
            let same = root.join(format!("{other}.yaml")).exists();

            let found = conflict(&root, &[&first, &second]);

            assert_eq!(found, same.then_some((first, second)), "one name: {same}");
        }
        if cfg!(target_os = "macos") {
            let (_dir, root) = directory_of(&["A.yaml"]);
            assert!(root.join("a.yaml").exists(), "the default macOS volume");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_is_one_under_every_name() {
        let (_dir, root) = directory_of(&["sub/a.yaml", "sub/a.yml", "sub/b.yml"]);
        std::os::unix::fs::symlink("sub", root.join("link")).expect("symlink");
        for other in ["link/a.yml", "sub/../sub/a.yml", "link/a.yaml"] {
            assert_eq!(
                conflict(&root, &["sub/a.yaml", "sub/b.yml", other]),
                pair("sub/a.yaml", other)
            );
        }
        assert_eq!(conflict(&root, &["sub/a.yaml", "link/b.yml"]), None);
    }

    #[cfg(unix)]
    #[test]
    fn hard_links_are_files_of_their_own_and_links_are_not_followed() {
        let (_dir, root) = directory_of(&["a.yaml", "sub/x.yaml"]);
        for name in ["b.yaml", "d.yml", "sub/a.yaml"] {
            std::fs::hard_link(root.join("a.yaml"), root.join(name)).expect("link");
        }
        std::os::unix::fs::symlink("a.yaml", root.join("e.yml")).expect("symlink");
        assert_eq!(
            conflict(&root, &["a.yaml", "b.yaml", "d.yml", "e.yml", "sub/a.yaml"]),
            None
        );
        // Found among them all the same.
        std::fs::hard_link(root.join("a.yaml"), root.join("a.yml")).expect("link");
        assert_eq!(
            conflict(&root, &["a.yaml", "b.yaml", "a.yml"]),
            pair("a.yaml", "a.yml")
        );
    }

    #[test]
    fn the_message_names_both_sources_and_the_target() {
        let shared = |second: &str| Conflict::Shared {
            first: ("A.yaml".into(), "A.json".into()),
            second: (second.into(), Utf8Path::new(second).with_extension("json")),
        };
        assert_eq!(
            shared("A.yml").message("json"),
            "convert -i to json: 'A.yaml' and 'A.yml' would both be written to 'A.json'; \
             rename one of them first. Nothing was converted"
        );
        assert_eq!(
            shared("a.yml").message("json"),
            "convert -i to json: 'A.yaml' and 'a.yml' would both be written to 'A.json' \
             (the same file as 'a.json' here); rename one of them first. Nothing was converted"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_target_that_is_a_hard_link_of_its_source_is_named_so() {
        let (_dir, root) = directory_of(&["a.yaml"]);
        std::fs::hard_link(root.join("a.yaml"), root.join("a.json")).expect("link");
        let planned = plan(&root, &["a.yaml"]);
        let conflict = find_conflict(&planned, false).expect("a conflict");
        assert_eq!(
            conflict.message("json"),
            format!(
                "convert -i to json: '{root}/a.yaml' and its target '{root}/a.json' are two names \
                 of one file (a hard link); remove one of them first. Nothing was converted"
            )
        );
    }

    #[test]
    fn a_backup_that_is_a_source_is_found_wherever_it_stands() {
        let (_dir, root) =
            directory_of(&["a.yaml", "a.json", "a.json.bak", "b.yaml", "b.json.bak"]);
        for sources in [["a.yaml", "a.json.bak"], ["a.json.bak", "a.yaml"]] {
            assert_eq!(conflict(&root, &sources), pair("a.json", "a.json.bak"));
            assert_eq!(find_conflict(&plan(&root, &sources), false), None);
        }
        // There is no `b.json` to back up.
        assert_eq!(conflict(&root, &["b.yaml", "b.json.bak"]), None);
        let spelled = Conflict::Backup {
            target: "a.json".into(),
            source: "a.json.bak".into(),
        };
        assert_eq!(
            spelled.message("json"),
            "convert -i to json: the backup of 'a.json' would replace 'a.json.bak', which is to \
             be converted too; rename one of them first. Nothing was converted"
        );
    }

    #[test]
    fn the_message_names_the_targets_that_exist_up_to_three() {
        let message = |targets: &[&str]| {
            Conflict::Existing {
                targets: targets.iter().map(Utf8PathBuf::from).collect(),
            }
            .message("json")
        };
        assert_eq!(
            message(&["a.json"]),
            "convert -i to json: the target 'a.json' already exists; pass --overwrite to replace \
             it, or --backup to keep a copy. Nothing was converted"
        );
        for (targets, named) in [
            (&["a", "b"][..], "'a' and 'b'"),
            (&["a", "b", "c"], "'a', 'b' and 'c'"),
            (&["a", "b", "c", "d"], "'a', 'b' and 2 others"),
            (&["a", "b", "c", "d", "e"], "'a', 'b' and 3 others"),
        ] {
            assert_eq!(
                message(targets),
                format!(
                    "convert -i to json: the targets {named} already exist; pass --overwrite to \
                     replace them, or --backup to keep a copy of each. Nothing was converted"
                )
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_link_to_a_file_the_conversion_would_replace_is_found() {
        let (_dir, root) = directory_of(&["a.json", "a.json.bak", "other.json", "sub/a.json"]);
        for (link, to, expected) in [
            ("a.yaml", "a.json", pair("a.json", "a.yaml")),
            ("a.yaml", "b.lnk", pair("a.json", "a.yaml")),
            ("a.yaml", "a.json.bak", pair("a.json.bak", "a.yaml")),
            ("a.yaml", "other.json", None),
            ("a.yaml", "sub/a.json", None),
            ("a.yaml", "nowhere", None),
            // There is no `n.json` to replace.
            ("n.yaml", "a.json", None),
        ] {
            std::os::unix::fs::symlink("a.json", root.join("b.lnk")).expect("symlink");
            std::os::unix::fs::symlink(to, root.join(link)).expect("symlink");
            assert_eq!(conflict(&root, &[link]), expected, "{to}");
            std::fs::remove_file(root.join("b.lnk")).expect("rm");
            std::fs::remove_file(root.join(link)).expect("rm");
        }
        // Without a backup that file stays.
        std::os::unix::fs::symlink("a.json.bak", root.join("a.yaml")).expect("symlink");
        assert_eq!(find_conflict(&plan(&root, &["a.yaml"]), false), None);
        let spelled = Conflict::Link {
            source: "a.yaml".into(),
            file: "a.json".into(),
        };
        assert_eq!(
            spelled.message("json"),
            "convert -i to json: 'a.yaml' is a symbolic link to 'a.json', which the conversion \
             would replace; remove the link first. Nothing was converted"
        );
    }

    /// The names of one directory say nothing of another.
    #[cfg(unix)]
    #[test]
    fn hard_links_of_several_directories_are_files_of_their_own() {
        let (_dir, root) = directory_of(&["one/a.yaml", "two/d.yaml"]);
        let links = [
            ("one/a.yaml", "one/b.yml"),
            ("one/a.yaml", "two/e.yaml"),
            ("two/d.yaml", "two/f.yml"),
            ("two/d.yaml", "one/g.yaml"),
        ];
        for (file, link) in links {
            std::fs::hard_link(root.join(file), root.join(link)).expect("link");
        }
        let sources = [
            "one/a.yaml",
            "one/b.yml",
            "one/g.yaml",
            "two/d.yaml",
            "two/e.yaml",
            "two/f.yml",
        ];
        assert_eq!(conflict(&root, &sources), None);
    }

    /// Listing the directory for each pair takes minutes.
    #[cfg(unix)]
    #[test]
    fn many_hard_links_of_one_file_are_compared_at_once() {
        let names: Vec<String> = (0..200)
            .flat_map(|n| [format!("f{n}.yaml"), format!("f{n}x.yml")])
            .collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let (_dir, root) = directory_of(&["f0.yaml", "other/f0.yml"]);
        for name in &names[1..] {
            std::fs::hard_link(root.join("f0.yaml"), root.join(name)).expect("link");
        }
        let start = std::time::Instant::now();

        assert_eq!(conflict(&root, &names), None);

        let mut names = names;
        std::fs::hard_link(root.join("f0.yaml"), root.join("f7.yml")).expect("link");
        names.push("f7.yml");
        assert_eq!(conflict(&root, &names), pair("f7.yaml", "f7.yml"));
        let took = start.elapsed();
        assert!(took < std::time::Duration::from_secs(10), "{took:?}");
    }

    #[cfg(unix)]
    #[test]
    fn stems_that_may_be_one_have_one_key() {
        for (one, other) in [
            ("ABC", "abc"),
            ("\u{e9}t\u{e9}", "e\u{301}te\u{301}"),
            ("a\u{e9}b", "ae\u{301}b"),
            ("stra\u{df}e", "STRASSE"),
            ("\u{fb01}x", "fix"),
            ("\u{212a}9", "k9"),
            ("\u{17f}et", "set"),
            ("\u{130}d", "i\u{307}d"),
            ("\u{1e96}2", "h\u{331}2"),
        ] {
            assert_eq!(unix::key(one), unix::key(other), "{one} {other}");
        }
        assert_ne!(unix::key("config-1"), unix::key("config-2"));
    }

    /// One probe for a file among many that cannot be its namesakes.
    #[cfg(unix)]
    #[test]
    fn only_the_suffixes_of_possible_namesakes_are_probed() {
        let names: Vec<String> = (0..500).map(|n| format!("f{n}.x{n}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let (_dir, root) = directory_of(&["sub/a"]);
        let planned = plan(&root, &names);
        let buckets = unix::Sources::of(&planned).suffixes();
        assert_eq!(buckets.len(), names.len());
        assert!(buckets.values().all(|suffixes| suffixes.len() == 1));
        let planned = plan(&root, &["a.yaml", "a.yml", "A.x", "sub/a.y", "b.z"]);
        let mut sizes: Vec<usize> = unix::Sources::of(&planned)
            .suffixes()
            .values()
            .map(std::collections::BTreeSet::len)
            .collect();
        sizes.sort_unstable();
        assert_eq!(sizes, [1, 1, 3]);
    }
}
