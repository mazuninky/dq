//! `Format` trait — the pluggable parse/write boundary.
//!
//! Each concrete parser (JSON, YAML, TOML, JSONL) lives under
//! [`crate::parsers`] and implements this trait. The dispatcher functions
//! [`detect`] and [`by_name`] return trait objects so callers can stay
//! parser-agnostic — adding a new format requires only registering it in
//! [`crate::parsers::registry`].

use camino::Utf8Path;
use std::io::Write;

use crate::Result;
use crate::WriteOptions;
use crate::document::Document;

/// Pluggable file-format reader/writer.
pub trait Format: Send + Sync {
    /// Stable short name used by the CLI's `-F` flag and error messages.
    fn name(&self) -> &'static str;

    /// File extensions (without the leading `.`) this format claims.
    fn extensions(&self) -> &'static [&'static str];

    /// Parse `bytes` into a [`Document`].
    ///
    /// # Errors
    ///
    /// Returns `Error::Parse` on syntax errors, or `Error::Format` for
    /// format-specific limitations (e.g. JSONL's lack of multi-doc support).
    fn parse(&self, bytes: &[u8]) -> Result<Document>;

    /// Serialize `doc` to `w`.
    ///
    /// # Errors
    ///
    /// Returns `Error::Io` on writer failure, `Error::Format` when the
    /// document cannot be expressed in this format.
    fn write(&self, doc: &Document, w: &mut dyn Write) -> Result<()>;

    /// Serialize `doc` to `w` with explicit re-emit options.
    ///
    /// The default implementation forwards to [`Format::write`], ignoring
    /// `opts` — formats that have not yet implemented an `--sort-keys` /
    /// `--indent` aware writer get the M2 byte-preserving behaviour for free.
    /// Per-format impls override this method to honour `opts.sort_keys` (deep
    /// canonicalize) and, for `json` / `jsonl`, `opts.indent` (compact when
    /// `Some(0)`, n-space pretty when `Some(n)`, default whitespace shape
    /// when `None`).
    ///
    /// # Errors
    ///
    /// Same as [`Format::write`]: `Error::Io` on writer failure,
    /// `Error::Format` when the document cannot be expressed in this format.
    fn write_with_options(
        &self,
        doc: &Document,
        w: &mut dyn Write,
        opts: &WriteOptions,
    ) -> Result<()> {
        let _ = opts;
        self.write(doc, w)
    }
}

/// Filename-based fallback table consulted by [`detect`] when the
/// extension lookup fails. Maps a basename (matched case-insensitively,
/// ASCII only) to the format `name()` to look up in
/// [`crate::parsers::registry`].
///
/// Holding `&'static str` keys (rather than `&'static dyn Format`) keeps
/// this table free of cyclical static initialisation: Stage 2 of M5 will
/// register the seven new formats in `parsers::registry()` and the lookup
/// here will start returning `Some` for `Dockerfile`, `.gitignore` etc.
/// without any further wiring.
const FILENAME_FALLBACK: &[(&str, &str)] = &[
    ("Dockerfile", "dockerfile"),
    ("Containerfile", "dockerfile"),
    (".gitignore", "ignore-list"),
    (".dockerignore", "ignore-list"),
    (".npmignore", "ignore-list"),
    (".eslintignore", "ignore-list"),
    (".env", "dotenv"),
];

/// Look up a format by file extension, falling back to a filename match for
/// dotfiles and extensionless config formats (`.gitignore`, `Dockerfile`,
/// `.env`, …).
///
/// The fallback consults [`FILENAME_FALLBACK`] only after the extension
/// lookup fails; both lookups are ASCII-case-insensitive on the filename
/// and on the extension.
#[must_use]
pub fn detect(path: &Utf8Path) -> Option<&'static dyn Format> {
    let registry = crate::parsers::registry();
    if let Some(ext) = path.extension() {
        let ext = ext.to_ascii_lowercase();
        if let Some(fmt) = registry
            .iter()
            .copied()
            .find(|fmt| fmt.extensions().contains(&ext.as_str()))
        {
            return Some(fmt);
        }
    }
    let basename = path.file_name()?;
    let target = FILENAME_FALLBACK
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(basename))
        .map(|(_, fmt_name)| *fmt_name)?;
    registry.iter().copied().find(|fmt| fmt.name() == target)
}

/// Look up a format by short name (`-F` flag).
#[must_use]
pub fn by_name(name: &str) -> Option<&'static dyn Format> {
    crate::parsers::registry()
        .iter()
        .copied()
        .find(|fmt| fmt.name() == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_picks_format_by_extension() {
        let f = detect(Utf8Path::new("a.json")).expect("json");
        assert_eq!(f.name(), "json");
    }

    #[test]
    fn detect_returns_none_for_unknown_extension() {
        // M11 wired up XML as a registered format; pick an extension that
        // genuinely has no parser to keep the negative-path coverage.
        assert!(detect(Utf8Path::new("a.unknownext")).is_none());
    }

    #[test]
    fn detect_resolves_xml_extension_to_xml_parser() {
        // M11 contract: `pom.xml` (or any `.xml` file) resolves to the
        // registered `XmlFormat` parser.
        let f = detect(Utf8Path::new("pom.xml")).expect("xml registered");
        assert_eq!(f.name(), "xml");
    }

    #[test]
    fn by_name_lookup() {
        assert!(by_name("yaml").is_some());
        assert!(by_name("nope").is_none());
    }

    #[test]
    fn detect_filename_fallback_resolves_after_stage_two_registration() {
        // Stage-2 contract: the seven M5 formats are now registered in
        // `parsers::registry()`, so the filename fallback table resolves
        // each entry to its canonical format.
        let gi = detect(Utf8Path::new(".gitignore")).expect("ignore-list registered");
        assert_eq!(gi.name(), "ignore-list");
        let docker = detect(Utf8Path::new("Dockerfile")).expect("dockerfile registered");
        assert_eq!(docker.name(), "dockerfile");
        let dotenv = detect(Utf8Path::new(".env")).expect("dotenv registered");
        assert_eq!(dotenv.name(), "dotenv");
        // Sanity: a registered format still resolves through the extension
        // path so this test does not mask a regression in `detect` itself.
        assert!(detect(Utf8Path::new("a.json")).is_some());
    }

    #[test]
    fn detect_filename_fallback_is_case_insensitive_lowercase_dockerfile() {
        // `docker build` accepts `dockerfile` (all lowercase) under some
        // configurations; the fallback table should match irrespective of
        // basename case (ASCII-only).
        let docker = detect(Utf8Path::new("dockerfile")).expect("dockerfile registered");
        assert_eq!(docker.name(), "dockerfile");
    }

    #[test]
    fn detect_filename_fallback_is_case_insensitive_uppercase_dotfile() {
        // Case-preserving filesystems (e.g. macOS HFS+/APFS default) can
        // surface dotfiles with arbitrary casing; the fallback should still
        // resolve them.
        let gi = detect(Utf8Path::new(".GITIGNORE")).expect("ignore-list registered");
        assert_eq!(gi.name(), "ignore-list");
    }
}
