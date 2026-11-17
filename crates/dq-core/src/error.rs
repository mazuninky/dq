//! Domain error type for `dq-core`.
//!
//! Variants carry enough structured context that downstream renderers (the
//! CLI's reporters) can produce diagnostics with line/column, did-you-mean
//! suggestions, and per-variant exit codes — see [`Error::kind_name`] for the
//! stable string used by the CLI's exit-code mapping.

use std::ops::Range;

use camino::Utf8PathBuf;
use thiserror::Error;

use crate::document::Value;

/// Why a [`Pointer::resolve`](crate::pointer::Pointer::resolve) failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathErrorKind {
    /// Object was missing the next key segment.
    MissingKey,
    /// Array index was out of bounds.
    OutOfBounds,
    /// Tried to descend into a leaf or used the wrong segment kind for the
    /// container at this level (e.g. a key segment against an array).
    TypeMismatch {
        /// What kind of container the previous segment landed on.
        expected: &'static str,
        /// What we actually found.
        found: &'static str,
    },
}

/// All errors `dq-core` may produce.
#[derive(Debug, Error)]
pub enum Error {
    /// I/O failure while reading a file. The CLI's adapter wraps `std::io::Error`s
    /// in this variant before they reach `dq-core` code paths.
    #[error("I/O error reading '{path}': {source}")]
    Io {
        /// File path being read.
        path: Utf8PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// I/O failure while writing a file (atomic write helper, persist, backup
    /// copy). Distinct from [`Error::Io`] because the CLI maps it to a
    /// different exit code (write-side failure vs. read-side) — see
    /// `add-safe-writes` D8.
    #[error("I/O error writing '{path}': {source}")]
    WriteIo {
        /// Target file path the writer was operating on.
        path: Utf8PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// Source could not be parsed.
    #[error("parse error{at}: {message}", at = file.as_deref().map(|p| format!(" in {p}")).unwrap_or_default())]
    Parse {
        /// File path, if known. `None` when reading from stdin.
        file: Option<Utf8PathBuf>,
        /// 1-based line.
        line: u32,
        /// 1-based column.
        col: u32,
        /// Byte range of the offending construct in the source.
        span: Range<usize>,
        /// A short snippet of source for context.
        snippet: String,
        /// Human-friendly message from the underlying parser.
        message: String,
    },

    /// JSON Pointer did not address an existing node.
    #[error("path '{pointer}' not found (matched up to '{matched_prefix}')")]
    Path {
        /// The pointer the user supplied.
        pointer: String,
        /// The longest matching prefix that did exist.
        matched_prefix: String,
        /// Why navigation stopped.
        kind: PathErrorKind,
        /// Up to three close candidate keys (Levenshtein distance ≤ 2).
        did_you_mean: Vec<String>,
    },

    /// File extension or `-F` value did not match any known parser.
    #[error("unsupported format '{name}'")]
    UnsupportedFormat {
        /// The format name or extension that was rejected.
        name: String,
    },

    /// A format-specific limitation (e.g. JSONL writer asked to emit multi-doc).
    #[error("{format} format error: {message}")]
    Format {
        /// Which format produced the error.
        format: &'static str,
        /// Human-friendly message.
        message: String,
    },

    /// `Document::set_at` / `Document::del_at` was called on a document
    /// that the parser produced without span metadata, or for a format
    /// whose textual-edit renderer has not yet been registered.
    ///
    /// Carries a human-readable reason so the CLI can surface a clear
    /// "this read-only document cannot be written" diagnostic without
    /// having to introspect parser state.
    #[error("write operation unavailable: {reason}")]
    WriteUnavailable {
        /// Why the write could not proceed.
        reason: String,
    },

    /// File contains Go template syntax (Helm/Argo/GitHub Actions) that the
    /// parser cannot safely round-trip without explicit opt-in.
    ///
    /// Surfaced before parsing when `template_guard::detect_templates` finds a
    /// match. The CLI maps this to `PARSE_ERROR` (exit 3) and surfaces both
    /// escape-hatch flags (`--allow-templates`, `--raw-template-strings`) in
    /// the diagnostic message.
    #[error("file contains Go template syntax (line {line}): {snippet}\n  hint: {hint}")]
    TemplatedFile {
        /// 1-based line of the first template marker.
        line: u32,
        /// Source line snippet containing the marker (trimmed to ~80 chars).
        snippet: String,
        /// Human-friendly hint mentioning both escape-hatch flags. Built by
        /// the CLI / caller (kept on the error so downstream JSON output
        /// surfaces it without re-deriving).
        hint: String,
    },

    /// RFC 6902 `test` operation observed a value at `pointer` that did not
    /// match `expected`.
    ///
    /// Per RFC 6902 §5, any operation failing during `apply_patch` aborts the
    /// whole patch — `apply_patch` returns this variant without mutating the
    /// caller's `Document`. The CLI maps this to a dedicated exit code in a
    /// later section of M3; for now `kind_name()` returns `"patch_test_failed"`
    /// and the message includes both the expected and actual values for the
    /// operator's diagnostic output.
    ///
    /// `expected` and `actual` are boxed to keep the `Error` enum's largest
    /// variant compact — `Value` is ~80 bytes, so two inline fields would
    /// roughly double `Error`'s footprint and trigger
    /// `clippy::result_large_err` on every function returning `Result`.
    #[error("RFC 6902 test op failed at {pointer}: expected {expected}, got {actual}")]
    PatchTestFailed {
        /// Canonical RFC 6901 form of the pointer the `test` op targeted.
        pointer: String,
        /// The value the `test` op asserted.
        expected: Box<Value>,
        /// The value actually present at `pointer`.
        actual: Box<Value>,
    },
}

impl Error {
    /// Stable per-variant identifier used by the CLI to map errors to exit codes.
    #[must_use]
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Io { .. } => "io",
            Self::WriteIo { .. } => "write_io",
            Self::Parse { .. } => "parse",
            Self::Path { .. } => "path",
            Self::UnsupportedFormat { .. } => "unsupported_format",
            Self::Format { .. } => "format",
            Self::WriteUnavailable { .. } => "write_unavailable",
            Self::TemplatedFile { .. } => "templated_file",
            Self::PatchTestFailed { .. } => "patch_test_failed",
        }
    }

    /// Build a [`Error::TemplatedFile`] from a `TemplateMarker`.
    ///
    /// `hint` is set to the canonical message mentioning both
    /// `--allow-templates` and `--raw-template-strings`. Callers needing a
    /// custom hint can construct the variant directly.
    #[must_use]
    pub fn templated_file(marker: crate::template_guard::TemplateMarker) -> Self {
        Self::TemplatedFile {
            line: marker.line,
            snippet: marker.snippet,
            hint: "use --allow-templates to parse anyway (formatting may break) \
                   or --raw-template-strings to preserve template values as opaque strings"
                .to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_name_is_stable_per_variant() {
        let io = Error::Io {
            path: Utf8PathBuf::from("/x"),
            source: std::io::Error::other("nope"),
        };
        assert_eq!(io.kind_name(), "io");

        let write_io = Error::WriteIo {
            path: Utf8PathBuf::from("/x"),
            source: std::io::Error::other("nope"),
        };
        assert_eq!(write_io.kind_name(), "write_io");

        let parse = Error::Parse {
            file: None,
            line: 1,
            col: 1,
            span: 0..0,
            snippet: String::new(),
            message: "bad".to_owned(),
        };
        assert_eq!(parse.kind_name(), "parse");

        let path = Error::Path {
            pointer: "/x".to_owned(),
            matched_prefix: String::new(),
            kind: PathErrorKind::MissingKey,
            did_you_mean: Vec::new(),
        };
        assert_eq!(path.kind_name(), "path");

        let unsupported = Error::UnsupportedFormat {
            name: "xml".to_owned(),
        };
        assert_eq!(unsupported.kind_name(), "unsupported_format");

        let format = Error::Format {
            format: "jsonl",
            message: "no".to_owned(),
        };
        assert_eq!(format.kind_name(), "format");

        let write_unavailable = Error::WriteUnavailable {
            reason: "read-only".to_owned(),
        };
        assert_eq!(write_unavailable.kind_name(), "write_unavailable");

        let templated = Error::TemplatedFile {
            line: 1,
            snippet: "tag: {{ .Values.tag }}".to_owned(),
            hint: "hint".to_owned(),
        };
        assert_eq!(templated.kind_name(), "templated_file");

        let patch_test = Error::PatchTestFailed {
            pointer: "/a".to_owned(),
            expected: Box::new(Value::Int(1)),
            actual: Box::new(Value::Int(2)),
        };
        assert_eq!(patch_test.kind_name(), "patch_test_failed");
    }
}
