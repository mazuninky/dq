//! Glob expansion shared by the bulk driver ([`crate::bulk::expand_glob`])
//! and the lint pipeline (`commands::lint_core::expand_lint_inputs`).
//!
//! The two callers differ only in how they map a failure to an exit code
//! (bulk: I/O error, 5; lint: invalid input, 6), so the matching lives here
//! once and returns a typed [`GlobError`].
//!
//! Matching rules (see `docs/reference/bulk.md` § Glob expansion):
//!
//! - An argument is a glob when it contains `*`, `?`, `[` or `{` **and** no
//!   file or directory exists at that literal path — a shell-expanded path
//!   such as `a[1].yaml` stays a plain path.
//! - `*` and `?` never match `/`; `**` is the one that crosses directories
//!   (`k8s/**/*.yaml` matches `k8s/a.yaml` and `k8s/sub/x.yaml`).
//! - The walk starts at the longest leading run of components free of
//!   metacharacters. A pattern whose first component already holds one
//!   (`2-*.yaml`, `{a,b}/*.yaml`) walks the current directory and matches
//!   the walked paths without their `./` prefix, so the results are spelled
//!   the way the pattern is.

use std::path::Path;

use camino::{Utf8Path, Utf8PathBuf};
use globset::GlobBuilder;
use walkdir::WalkDir;

/// Why a glob produced no files.
#[derive(Debug, thiserror::Error)]
pub(crate) enum GlobError {
    /// The pattern does not compile.
    #[error("invalid glob pattern '{pattern}': {source}")]
    Invalid {
        /// The pattern as given.
        pattern: Utf8PathBuf,
        /// The `globset` parse error.
        #[source]
        source: globset::Error,
    },
    /// The directory the walk starts from does not exist.
    #[error("glob root {root} does not exist")]
    RootMissing {
        /// The walk root derived from the pattern.
        root: Utf8PathBuf,
    },
    /// The walk found no file the pattern matches.
    #[error("glob {pattern} matched zero files")]
    NoMatch {
        /// The pattern as given.
        pattern: Utf8PathBuf,
    },
}

fn is_meta(c: char) -> bool {
    matches!(c, '*' | '?' | '[' | '{')
}

/// Whether `arg` is to be expanded as a glob: it holds a metacharacter and
/// nothing exists at that literal path.
pub(crate) fn is_glob(arg: &Utf8Path) -> bool {
    arg.as_str().chars().any(is_meta) && !arg.as_std_path().exists()
}

/// The walk root of `pattern` — its leading components free of
/// metacharacters — with the number of pattern components it spans, or
/// `None` when the first component already holds one (the walk then starts
/// at `.`).
fn walk_root(pattern: &str) -> Option<(Utf8PathBuf, usize)> {
    let prefix: Vec<&str> = pattern
        .split('/')
        .take_while(|component| !component.chars().any(is_meta))
        .collect();
    if prefix.is_empty() {
        return None;
    }
    let joined = prefix.join("/");
    // An absolute pattern splits into a leading empty component.
    let root = Utf8PathBuf::from(if joined.is_empty() { "/" } else { &joined });
    Some((root, prefix.len()))
}

/// How deep below the walk root a match can lie, or `None` when unbounded
/// (`**` anywhere, or a `{…}` alternation that may hold a `/`).
fn max_depth(pattern: &str, root_components: usize) -> Option<usize> {
    if pattern.contains("**") || pattern.contains('{') {
        return None;
    }
    let total = pattern.split('/').count();
    Some(total.saturating_sub(root_components))
}

/// Expand `pattern` into the sorted list of files it matches.
///
/// The caller decides whether the argument is a glob at all ([`is_glob`]).
///
/// # Errors
///
/// - [`GlobError::Invalid`] when the pattern does not compile.
/// - [`GlobError::RootMissing`] when the walk root does not exist.
/// - [`GlobError::NoMatch`] when no file matches.
pub(crate) fn expand(pattern: &Utf8Path) -> Result<Vec<Utf8PathBuf>, GlobError> {
    let matcher = GlobBuilder::new(pattern.as_str())
        .literal_separator(true)
        .build()
        .map_err(|source| GlobError::Invalid {
            pattern: pattern.to_owned(),
            source,
        })?
        .compile_matcher();

    let root = walk_root(pattern.as_str());
    let (walk_from, root_components) = match &root {
        Some((root, components)) => (root.clone(), *components),
        None => (Utf8PathBuf::from("."), 0),
    };
    if !walk_from.as_std_path().exists() {
        return Err(GlobError::RootMissing { root: walk_from });
    }

    let mut walker = WalkDir::new(walk_from.as_std_path());
    if let Some(depth) = max_depth(pattern.as_str(), root_components) {
        walker = walker.max_depth(depth);
    }
    let mut matches: Vec<Utf8PathBuf> = walker
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| {
            let path = entry.into_path();
            // A cwd-relative pattern is matched against `2-a.yaml`, not the
            // `./2-a.yaml` the walk from `.` yields.
            let candidate = if root.is_none() {
                path.strip_prefix(".").map(Path::to_path_buf).ok()?
            } else {
                path
            };
            Utf8PathBuf::from_path_buf(candidate).ok()
        })
        .filter(|path| matcher.is_match(path.as_std_path()))
        .collect();

    if matches.is_empty() {
        return Err(GlobError::NoMatch {
            pattern: pattern.to_owned(),
        });
    }
    // Deterministic ordering: per-file output follows matched-file order.
    matches.sort();
    Ok(matches)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn seed(root: &Utf8Path, rel: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"x: 1\n").unwrap();
    }

    /// A tempdir and its path with forward slashes, so patterns built on
    /// it work on Windows too.
    fn temp_root() -> (tempfile::TempDir, Utf8PathBuf) {
        let dir = tempdir().unwrap();
        let root = Utf8PathBuf::from(dir.path().to_str().unwrap().replace('\\', "/"));
        (dir, root)
    }

    fn names(root: &Utf8Path, pattern: &str) -> Vec<String> {
        expand(&Utf8PathBuf::from(format!("{root}/{pattern}")))
            .unwrap()
            .iter()
            .map(|p| p.as_str()[root.as_str().len() + 1..].replace('\\', "/"))
            .collect()
    }

    #[test]
    fn walk_root_stops_at_the_first_meta_component() {
        assert_eq!(walk_root("dir/sub/*.yaml"), Some(("dir/sub".into(), 2)));
        assert_eq!(walk_root("/*.yaml"), Some(("/".into(), 1)));
        assert_eq!(walk_root("./*.yaml"), Some((".".into(), 1)));
        assert_eq!(walk_root("*.yaml"), None);
        assert_eq!(walk_root("{a,b}/x.yaml"), None);
    }

    #[test]
    fn max_depth_is_bounded_without_double_star_or_braces() {
        assert_eq!(max_depth("k8s/*.yaml", 1), Some(1));
        assert_eq!(max_depth("*.yaml", 0), Some(1));
        assert_eq!(max_depth("/*.yaml", 1), Some(1));
        assert_eq!(max_depth("k8s/*/x.yaml", 1), Some(2));
        assert_eq!(max_depth("k8s/**/*.yaml", 1), None);
        assert_eq!(max_depth("k8s/{a,b/c}.yaml", 1), None);
    }

    #[test]
    fn star_stays_in_its_directory_and_double_star_crosses() {
        let (_dir, root) = temp_root();
        for rel in ["a.yaml", "b.json", "sub/c.yaml", "sub/deep/d.yaml"] {
            seed(&root, rel);
        }
        assert_eq!(names(&root, "*.yaml"), ["a.yaml"]);
        assert_eq!(names(&root, "*/*.yaml"), ["sub/c.yaml"]);
        assert_eq!(
            names(&root, "**/*.yaml"),
            ["a.yaml", "sub/c.yaml", "sub/deep/d.yaml"]
        );
        assert_eq!(names(&root, "s?b/[c].yaml"), ["sub/c.yaml"]);
        assert_eq!(names(&root, "{a,b}.*"), ["a.yaml", "b.json"]);
    }

    #[test]
    fn is_glob_takes_an_existing_path_literally() {
        let (_dir, root) = temp_root();
        seed(&root, "a[1].yaml");
        assert!(!is_glob(&root.join("a[1].yaml")));
        assert!(is_glob(&root.join("a[2].yaml")));
        assert!(!is_glob(Utf8Path::new("/no/such/plain.yaml")));
    }

    /// `globset` honours backslash escapes on Unix only.
    #[cfg(unix)]
    #[test]
    fn an_escaped_bracket_matches_literally() {
        let (_dir, root) = temp_root();
        seed(&root, "a[1].yaml");
        seed(&root, "a1.yaml");
        assert_eq!(names(&root, r"a\[1\].yaml"), ["a[1].yaml"]);
    }

    #[test]
    fn errors_name_their_cause() {
        let (_dir, root) = temp_root();
        let invalid = Utf8PathBuf::from(format!("{root}/[unbalanced"));
        assert!(matches!(expand(&invalid), Err(GlobError::Invalid { .. })));
        let missing = Utf8PathBuf::from(format!("{root}/nope/*.yaml"));
        assert!(matches!(
            expand(&missing),
            Err(GlobError::RootMissing { .. })
        ));
        let none = Utf8PathBuf::from(format!("{root}/*.yaml"));
        assert!(matches!(expand(&none), Err(GlobError::NoMatch { .. })));
    }
}
