//! The skill as the binary carries it.
//!
//! `skills/dq/SKILL.md` and `skills/dq/references/*.md`, embedded at compile
//! time; `evals/` stays in the repository. A missing entry would leave the
//! file out of every installation without a word, so a test compares
//! [`FILES`] with the directory. Keep the list sorted by path.

/// One file of the skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkillFile {
    /// Path inside the skill directory, components separated by `/`.
    pub path: &'static str,
    /// What the file holds; written as it is, byte for byte.
    pub text: &'static str,
}

/// Every file of the embedded skill.
const FILES: &[SkillFile] = &[
    SkillFile {
        path: "SKILL.md",
        text: include_str!("../../../../skills/dq/SKILL.md"),
    },
    SkillFile {
        path: "references/formats.md",
        text: include_str!("../../../../skills/dq/references/formats.md"),
    },
    SkillFile {
        path: "references/linting.md",
        text: include_str!("../../../../skills/dq/references/linting.md"),
    },
    SkillFile {
        path: "references/pointers.md",
        text: include_str!("../../../../skills/dq/references/pointers.md"),
    },
];

/// The skill of one dq version: what `install` and `update` write.
///
/// A value, not a global, so that a test can stand for an older or a newer
/// dq.
#[derive(Debug, Clone, Copy)]
pub struct Edition {
    /// The dq version the files belong to.
    pub version: &'static str,
    /// The files, sorted by path.
    pub files: &'static [SkillFile],
}

impl Edition {
    /// The skill this binary carries.
    #[must_use]
    pub fn embedded() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            files: FILES,
        }
    }

    /// What this edition has at `path`, `None` when it has no such file.
    #[must_use]
    pub fn text(&self, path: &str) -> Option<&'static str> {
        self.files
            .iter()
            .find(|file| file.path == path)
            .map(|file| file.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// The `*.md` files of `dir`, as paths starting with `prefix`.
    fn markdown_in(dir: &std::path::Path, prefix: &str) -> Vec<String> {
        std::fs::read_dir(dir)
            .expect("skill directory")
            .map(|entry| {
                let name = entry.expect("entry").file_name();
                format!("{prefix}{}", name.to_str().expect("UTF-8 name"))
            })
            .filter(|name| name.ends_with(".md"))
            .collect()
    }

    #[test]
    fn the_embedded_files_are_those_of_the_directory() {
        let skill = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/dq");
        let mut on_disk = markdown_in(&skill, "");
        on_disk.extend(markdown_in(&skill.join("references"), "references/"));
        on_disk.sort();
        let edition = Edition::embedded();
        let embedded: Vec<&str> = edition.files.iter().map(|file| file.path).collect();
        assert_eq!(embedded, on_disk);
        for file in edition.files {
            let bytes = std::fs::read(skill.join(file.path)).expect("skill file");
            assert_eq!(file.text.as_bytes(), bytes, "{}", file.path);
        }
    }

    /// Every file of the skill but `evals/`, whatever its name and however
    /// deep: one that is no `*.md` would be left out of the list above.
    #[test]
    fn nothing_of_the_directory_but_the_evals_is_left_out() {
        let skill = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/dq");
        let mut on_disk: Vec<String> = walkdir::WalkDir::new(&skill)
            .into_iter()
            .filter_entry(|entry| {
                !matches!(entry.file_name().to_str(), Some("evals" | ".DS_Store"))
            })
            .map(|entry| entry.expect("entry"))
            .filter(|entry| !entry.file_type().is_dir())
            .map(|entry| {
                let path = entry.path().strip_prefix(&skill).expect("inside");
                path.to_str().expect("UTF-8").to_owned()
            })
            .collect();
        on_disk.sort();
        let edition = Edition::embedded();
        let embedded: Vec<&str> = edition.files.iter().map(|file| file.path).collect();
        assert_eq!(embedded, on_disk);
        assert!(embedded.is_sorted(), "{embedded:?}");
    }

    /// What is installed names no other way to install it.
    #[test]
    fn the_skill_names_no_other_installer() {
        let skill = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills");
        for entry in walkdir::WalkDir::new(skill) {
            let entry = entry.expect("entry");
            assert_ne!(entry.file_name(), "skill.json");
            if !entry.file_type().is_file() || entry.file_name() == ".DS_Store" {
                continue;
            }
            let text = std::fs::read_to_string(entry.path())
                .expect("text")
                .to_lowercase();
            for other in ["npx", "skills.sh", "skills add"] {
                assert!(!text.contains(other), "{}: {other}", entry.path().display());
            }
        }
    }

    /// The front matter by which an agent finds the skill, under the name of
    /// its directory.
    #[test]
    fn the_skill_has_the_name_of_its_directory() {
        let text = Edition::embedded().text("SKILL.md").expect("SKILL.md");
        let front = text.strip_prefix("---\n").expect("front matter");
        let front = front.split("\n---\n").next().expect("its end");
        assert!(front.lines().any(|line| line == "name: dq"), "{front}");
        assert!(
            front.lines().any(|line| line.starts_with("description:")),
            "{front}"
        );
    }

    /// The compiler needs `skills/` next to `crates/`, also in an image, and
    /// before it runs.
    #[test]
    fn the_images_are_built_with_the_skill() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let read = |name: &str| std::fs::read_to_string(root.join(name)).expect(name);
        for name in ["Dockerfile", "Dockerfile.scratch"] {
            let text = read(name);
            let line = |wanted: &str| {
                text.lines()
                    .position(|line| line.starts_with(wanted))
                    .unwrap_or_else(|| panic!("{name} has no line {wanted:?}"))
            };
            assert!(
                line("COPY skills ./skills") < line("RUN cargo build"),
                "{name}"
            );
        }
        for line in read(".dockerignore").lines() {
            let pattern = line.trim().trim_matches('/');
            assert!(
                !matches!(
                    pattern,
                    "skills" | "skills/*" | "skills/**" | "**/*.md" | "**/references" | "*"
                ),
                ".dockerignore leaves the skill out: {line}"
            );
        }
    }

    #[test]
    fn text_finds_a_file_by_its_whole_path() {
        let edition = Edition::embedded();
        assert!(edition.text("SKILL.md").is_some());
        assert!(edition.text("formats.md").is_none());
    }
}
