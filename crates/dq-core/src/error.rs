//! Domain error type for `dq-core`.
//!
//! Variants carry enough structured context that downstream renderers (the
//! CLI's reporters) can produce diagnostics with line/column, did-you-mean
//! suggestions, and per-variant exit codes — see [`Error::kind_name`] for the
//! stable string used by the CLI's exit-code mapping.

use std::borrow::Cow;
use std::ops::Range;
use std::str::Utf8Error;

use camino::Utf8PathBuf;
use thiserror::Error;

use crate::document::{LineBreak, Value, bom_end};

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
    /// An RFC 6902 `move` into one of the children of the value it moves:
    /// its `from` is a proper prefix of its `path` (RFC 6902 §4.4).
    IntoItsOwnChild {
        /// The `from` of the move, in canonical form.
        from: String,
    },
    /// A delete of the root pointer: it names the whole document.
    DeleteRoot,
    /// A `set` of the root pointer of a stream of documents, where it
    /// names no one document.
    StreamRoot,
    /// An insert at the root pointer, which names no item of an array.
    InsertAtRoot,
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

    /// Source could not be parsed. Built by [`Error::parse_at`], so that a
    /// position means the same in every format.
    #[error("parse error{at}: {message}", at = file.as_deref().map(|p| format!(" in {p}")).unwrap_or_default())]
    Parse {
        /// File path, if known. `None` when reading from stdin.
        file: Option<Utf8PathBuf>,
        /// 1-based line; 0 for an error that has no place in the file — a
        /// jq expression that does not compile, whose `col` and `span` are
        /// offsets in the expression. [`Error::place`] tells them apart.
        line: u32,
        /// 1-based column, in bytes; a leading byte order mark is not
        /// counted.
        col: u32,
        /// Byte range of the offending construct in the source, a byte
        /// order mark counted; empty where no construct is known. For XML in
        /// an encoding other than UTF-8 it is of the file's bytes, while
        /// `col` and `snippet` are of the decoded text.
        span: Range<usize>,
        /// The whole source line, without its line break and a leading
        /// byte order mark.
        snippet: String,
        /// Human-friendly message from the underlying parser, without the
        /// position. A place it names for its context (`while parsing a flow
        /// sequence at line 3 column 8`) is one in the source too.
        message: String,
    },

    /// JSON Pointer did not address an existing node — or addressed the
    /// root of a document, or of a stream, that the edit cannot apply to.
    #[error("{}", path_message(pointer, matched_prefix, kind))]
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

    /// File extension or `--input-format` value did not match any known parser.
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

    /// `Document::set_at` / `Document::del_at` could not make the edit
    /// without changing parts of the document other than the addressed
    /// node — deleting a YAML anchor an alias elsewhere refers to, or a
    /// textual edit whose result does not read back as the edited tree.
    /// The document is left as it was.
    #[error("cannot {action} '{pointer}': {reason}")]
    EditRejected {
        /// The operation: `set` or `delete`.
        action: &'static str,
        /// Canonical RFC 6901 form of the pointer the edit targeted.
        pointer: String,
        /// Why, for the user.
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

/// `text` on one line: a line feed as `\x0a` and a carriage return as
/// `\x0d`, as the CLI prints every other control character of a message on
/// the terminal. For a message, or user data quoted in one — a pointer, a
/// key, a path — on its way to the terminal, where a line break of its own
/// could start a line that passes for one of the message's (`dq get x.json
/// $'/a\nY'`). The errors' messages themselves keep the text as it is:
/// machine-readable output carries it. Borrowed when there is none.
#[must_use]
pub fn one_line(text: &str) -> Cow<'_, str> {
    if !text.contains(['\n', '\r']) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(text.replace('\n', "\\x0a").replace('\r', "\\x0d"))
}

/// The bytes of the first sequence of `bytes` that is no UTF-8, as `err`
/// has it.
pub(crate) fn invalid_utf8_span(bytes: &[u8], err: Utf8Error) -> Range<usize> {
    let start = err.valid_up_to();
    start
        ..err
            .error_len()
            .map_or(bytes.len(), |len| start.saturating_add(len))
}

/// The bytes of the character at `at` in `bytes`, for a parse error at that
/// character: empty at the end of `bytes`, one byte where there is no valid
/// character.
pub(crate) fn char_span(bytes: &[u8], at: usize) -> Range<usize> {
    let at = at.min(bytes.len());
    let rest = &bytes[at..bytes.len().min(at + char::MAX_LEN_UTF8)];
    let len = rest
        .utf8_chunks()
        .next()
        .and_then(|chunk| chunk.valid().chars().next())
        .map_or(rest.len().min(1), char::len_utf8);
    at..at + len
}

fn saturating_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The lines of a source: they end in `\n` (or `\r\n`), or in a lone `\r`
/// when the source holds no `\n`, as [`LineBreak`] has it for the edits.
struct Lines<'a> {
    bytes: &'a [u8],
    /// The last byte of every line break.
    line_break: u8,
}

impl<'a> Lines<'a> {
    fn of(bytes: &'a [u8]) -> Self {
        let line_break = if LineBreak::cr_only(bytes) {
            b'\r'
        } else {
            b'\n'
        };
        Self { bytes, line_break }
    }

    /// How many line breaks `part` of the source holds.
    fn breaks(&self, part: &[u8]) -> usize {
        part.iter().filter(|&&b| b == self.line_break).count()
    }

    /// Where the line after the one holding byte `at` begins, if any.
    fn next(&self, at: usize) -> Option<usize> {
        let found = self.bytes[at..]
            .iter()
            .position(|&b| b == self.line_break)?;
        Some(at + found + 1)
    }

    /// The bytes of the line holding byte `at`, without its line break and,
    /// on the first line, a byte order mark. A byte of a line break is on
    /// the line it ends.
    fn holding(&self, at: usize) -> Range<usize> {
        let start = self.bytes[..at]
            .iter()
            .rposition(|&b| b == self.line_break)
            .map_or(bom_end(self.bytes), |i| i + 1);
        let from = at.max(start);
        let Some(next) = self.next(from) else {
            return start..self.bytes.len();
        };
        let mut end = next - 1;
        if self.line_break == b'\n' && end > start && self.bytes[end - 1] == b'\r' {
            end -= 1;
        }
        start..end
    }
}

/// The 1-based line and byte column of byte `at` of `bytes`, at most their
/// length, and the bytes of that line, as [`Error::parse_at`] has them.
fn place_of(bytes: &[u8], at: usize) -> (u32, u32, Range<usize>) {
    let lines = Lines::of(bytes);
    let line = lines.holding(at);
    (
        saturating_u32(1 + lines.breaks(&bytes[..line.start])),
        saturating_u32(1 + at.saturating_sub(line.start)),
        line,
    )
}

/// The byte of `bytes` at 1-based line `line`, byte column `col`: the
/// inverse of [`place_of`]. `None` for a line or a column past the end.
fn offset_of(bytes: &[u8], (line, col): (u32, u32)) -> Option<usize> {
    let lines = Lines::of(bytes);
    let mut start = bom_end(bytes);
    for _ in 1..line {
        start = lines.next(start)?;
    }
    let at = start.checked_add(usize::try_from(col.checked_sub(1)?).ok()?)?;
    (at <= bytes.len()).then_some(at)
}

/// `message` without the place at its end and that place, the line and the
/// column a YAML error names for its context: `…, while parsing a flow
/// sequence` and `(3, 8)` for `…, while parsing a flow sequence at line 3
/// column 8`. `None` when it ends with no such place.
pub(crate) fn split_context(message: &str) -> Option<(&str, (u32, u32))> {
    let (text, place) = message.rsplit_once(" at line ")?;
    let (line, col) = place.split_once(" column ")?;
    let place = (line.parse().ok()?, col.parse().ok()?);
    text.contains(", while ").then_some((text, place))
}

/// The message of an [`Error::Path`].
fn path_message(pointer: &str, matched_prefix: &str, kind: &PathErrorKind) -> String {
    match kind {
        PathErrorKind::DeleteRoot => format!(
            "cannot delete the root '{pointer}': it is the whole document; delete a value \
             inside it, or one document of a stream as /N"
        ),
        PathErrorKind::StreamRoot => format!(
            "cannot set the root '{pointer}' of a stream of documents: set one document \
             as /N, or pass --doc N"
        ),
        PathErrorKind::InsertAtRoot => format!(
            "cannot insert at the root '{pointer}': it is the whole document, no item of \
             an array; name an index of an array, as /a/0"
        ),
        PathErrorKind::IntoItsOwnChild { from } => format!(
            "cannot move '{from}' to '{pointer}': a value cannot move into one of its own \
             children"
        ),
        _ => format!("path '{pointer}' not found (matched up to '{matched_prefix}')"),
    }
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
            Self::EditRejected { .. } => "edit_rejected",
            Self::TemplatedFile { .. } => "templated_file",
            Self::PatchTestFailed { .. } => "patch_test_failed",
        }
    }

    /// An [`Error::Parse`] at byte `span` of the source `bytes`, with no
    /// file: the line, the byte column and the source line are those of
    /// `span.start`. A `span` past the end of `bytes` is clamped to it.
    ///
    /// Lines end in `\n` or `\r\n` — or in a lone `\r`, in bytes with no
    /// `\n` at all. The end of the source is one past its last byte: column
    /// 1 of an empty last line when the source ends with a line break. A
    /// leading byte order mark counts in `span` only. Bytes that are no
    /// UTF-8 stand as U+FFFD in the snippet; the position counts them.
    #[must_use]
    pub fn parse_at(bytes: &[u8], span: Range<usize>, message: impl Into<String>) -> Self {
        let start = span.start.min(bytes.len());
        let end = span.end.clamp(start, bytes.len());
        let (line, col, text) = place_of(bytes, start);
        Self::Parse {
            file: None,
            line,
            col,
            span: start..end,
            snippet: String::from_utf8_lossy(&bytes[text]).into_owned(),
            message: message.into(),
        }
    }

    /// [`Error::parse_at`], the message ending with the place of byte
    /// `context` (` at line 3 column 8`) — when there is one, and it is not
    /// the place of the error itself.
    pub(crate) fn parse_in_context(
        bytes: &[u8],
        span: Range<usize>,
        message: &str,
        context: Option<usize>,
    ) -> Self {
        let mut err = Self::parse_at(bytes, span, message);
        if let Self::Parse {
            line, col, message, ..
        } = &mut err
            && let Some(at) = context
        {
            let (at_line, at_col, _) = place_of(bytes, at.min(bytes.len()));
            if (at_line, at_col) != (*line, *col) {
                message.push_str(&format!(" at line {at_line} column {at_col}"));
            }
        }
        err
    }

    /// Where a file does not parse: the 1-based line and byte column of an
    /// [`Error::Parse`], whose message names neither. `None` for an error
    /// that has no place in a file — any other, and an [`Error::Parse`] at
    /// line 0.
    #[must_use]
    pub fn place(&self) -> Option<(u32, u32)> {
        match self {
            Self::Parse { line, col, .. } if *line > 0 => Some((*line, *col)),
            _ => None,
        }
    }

    /// The message, followed by the [`Error::place`] where there is one, for
    /// a text that shows the error in the middle of another.
    pub(crate) fn with_place(&self) -> String {
        match self.place() {
            Some((line, col)) => format!("{self} (line {line}, column {col})"),
            None => self.to_string(),
        }
    }

    /// The [`Error::Parse`] for `bytes`, a `what` input (`JSONL`), that are
    /// no UTF-8, at the first byte that is none.
    pub(crate) fn invalid_utf8(bytes: &[u8], err: Utf8Error, what: &str) -> Self {
        let message = match err.error_len() {
            Some(len) => {
                format!("invalid UTF-8 in {what} input: invalid utf-8 sequence of {len} bytes")
            }
            None => format!("invalid UTF-8 in {what} input: incomplete utf-8 byte sequence"),
        };
        Self::parse_at(bytes, invalid_utf8_span(bytes, err), message)
    }

    /// Whether the line, the column and the source line of an
    /// [`Error::Parse`] are those of its span in `read`, the bytes its
    /// parser was handed. Not those of XML read in an encoding other than
    /// UTF-8, which are of the decoded text — but where the two have them
    /// alike. No other error has a place.
    pub(crate) fn is_placed_in(&self, read: &[u8]) -> bool {
        let Self::Parse {
            line,
            col,
            span,
            snippet,
            ..
        } = self
        else {
            return false;
        };
        let (at_line, at_col, text) = place_of(read, span.start.min(read.len()));
        (at_line, at_col) == (*line, *col) && String::from_utf8_lossy(&read[text]) == *snippet
    }

    /// The error of a parser that read `read`, a part of the source `bytes`
    /// or a rewritten form of it: a [`Error::Parse`] placed anew, `to_source`
    /// being the byte of `bytes` for each byte of `read`, and so is the
    /// context its message names the place of. Any other error as it is.
    pub(crate) fn relocated(
        self,
        read: &[u8],
        bytes: &[u8],
        to_source: impl Fn(usize) -> usize,
    ) -> Self {
        match self {
            Self::Parse {
                file,
                span,
                message,
                ..
            } => {
                let (text, context) = match split_context(&message) {
                    Some((text, place)) => (text, offset_of(read, place).map(&to_source)),
                    None => (message.as_str(), None),
                };
                let span = to_source(span.start)..to_source(span.end);
                let mut err = Self::parse_in_context(bytes, span, text, context);
                if let Self::Parse { file: at, .. } = &mut err {
                    *at = file;
                }
                err
            }
            other => other,
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
    fn one_line_escapes_line_breaks_only() {
        assert!(matches!(one_line("/a/b"), Cow::Borrowed("/a/b")));
        assert_eq!(one_line("/a\nY\r\tz"), "/a\\x0aY\\x0d\tz");
    }

    #[test]
    fn a_message_keeps_the_user_data_it_quotes_as_it_is() {
        // Machine-readable output (`validate -F json`) carries the message
        // as it is; the terminal escapes it (`dq::render_error`).
        let err = Error::Format {
            format: "csv",
            message: "row 0 key 'k\nz': nested".into(),
        };
        assert_eq!(
            err.to_string(),
            "csv format error: row 0 key 'k\nz': nested"
        );
    }

    /// `(line, col, span, snippet)` of a parse error.
    fn position(err: Error) -> (u32, u32, Range<usize>, String) {
        match err {
            Error::Parse {
                file: None,
                line,
                col,
                span,
                snippet,
                message,
            } => {
                assert_eq!(message, "m");
                (line, col, span, snippet)
            }
            other => panic!("expected a parse error with no file, got {other:?}"),
        }
    }

    fn at(bytes: &[u8], at: usize) -> (u32, u32, String) {
        let (line, col, span, snippet) = position(Error::parse_at(bytes, at..at, "m"));
        assert_eq!(span, at..at);
        (line, col, snippet)
    }

    #[test]
    fn parse_at_counts_lines_and_byte_columns() {
        for (bytes, offset, line, col, snippet) in [
            // LF: a byte, a line break, the first byte of the next line.
            (&b"ab\ncd\n"[..], 1, 1, 2, "ab"),
            (b"ab\ncd\n", 2, 1, 3, "ab"),
            (b"ab\ncd\n", 3, 2, 1, "cd"),
            // CRLF: both bytes of a break are on the line they end.
            (b"ab\r\ncd\r\n", 2, 1, 3, "ab"),
            (b"ab\r\ncd\r\n", 3, 1, 4, "ab"),
            (b"ab\r\ncd\r\n", 5, 2, 2, "cd"),
            (b"a\r\r\n", 0, 1, 1, "a\r"),
            // A lone CR breaks lines in a file with no LF only.
            (b"ab\rcd\r", 4, 2, 2, "cd"),
            (b"ab\rcd\r", 2, 1, 3, "ab"),
            (b"ab\rcd\nx", 4, 1, 5, "ab\rcd"),
            // Columns count bytes.
            ("я=\u{1f600}x".as_bytes(), 7, 1, 8, "я=\u{1f600}x"),
            // An empty line.
            (b"a\n\nb", 2, 2, 1, ""),
        ] {
            assert_eq!(
                at(bytes, offset),
                (line, col, snippet.to_owned()),
                "{bytes:?} at {offset}"
            );
        }
    }

    #[test]
    fn parse_at_the_end_of_the_source_is_one_past_the_last_byte() {
        assert_eq!(at(b"", 0), (1, 1, String::new()));
        assert_eq!(at(b"a\nbc", 4), (2, 3, "bc".to_owned()));
        assert_eq!(at(b"a\nbc\n", 5), (3, 1, String::new()));
        assert_eq!(at(b"a\r\n", 3), (2, 1, String::new()));
        assert_eq!(at(b"a\rb\r", 4), (3, 1, String::new()));
        assert_eq!(at(b"\n", 1), (2, 1, String::new()));
    }

    #[test]
    fn parse_at_counts_no_byte_order_mark() {
        let bytes = "\u{feff}ab\ncd".as_bytes();
        // On the mark and past it; later lines are as ever.
        assert_eq!(at(bytes, 0), (1, 1, "ab".to_owned()));
        assert_eq!(at(bytes, 2), (1, 1, "ab".to_owned()));
        assert_eq!(at(bytes, 4), (1, 2, "ab".to_owned()));
        assert_eq!(at(bytes, 7), (2, 2, "cd".to_owned()));
        assert_eq!(at("\u{feff}".as_bytes(), 3), (1, 1, String::new()));
    }

    #[test]
    fn parse_at_clamps_its_span() {
        let spans = |span| position(Error::parse_at(b"a\nbc", span, "m"));
        assert_eq!(spans(2..9), (2, 1, 2..4, "bc".to_owned()));
        assert_eq!(spans(7..9), (2, 3, 4..4, "bc".to_owned()));
        let reversed = Range { start: 3, end: 1 };
        assert_eq!(spans(reversed), (2, 2, 3..3, "bc".to_owned()));
        assert_eq!(spans(usize::MAX..usize::MAX), (2, 3, 4..4, "bc".to_owned()));
        assert_eq!(spans(0..4), (1, 1, 0..4, "a".to_owned()));
    }

    #[test]
    fn parse_at_places_bytes_that_are_no_utf8() {
        assert_eq!(at(b"a\n\xFF\xFEb", 4), (2, 3, "\u{fffd}\u{fffd}b".into()));
        let bad = b"a\n\xC3(".to_vec();
        let err = std::str::from_utf8(&bad).expect_err("no UTF-8");
        let (line, col, span, snippet) = match Error::invalid_utf8(&bad, err, "INI") {
            Error::Parse {
                line,
                col,
                span,
                snippet,
                message,
                ..
            } => {
                assert_eq!(
                    message,
                    "invalid UTF-8 in INI input: invalid utf-8 sequence of 1 bytes"
                );
                (line, col, span, snippet)
            }
            other => panic!("{other:?}"),
        };
        assert_eq!((line, col, span), (2, 1, 2..3));
        assert_eq!(snippet, "\u{fffd}(");
        // Cut short: the rest of the bytes.
        let cut = "a\u{20ac}".as_bytes();
        let cut = &cut[..cut.len() - 1];
        let err = std::str::from_utf8(cut).expect_err("no UTF-8");
        assert!(matches!(
            Error::invalid_utf8(cut, err, "INI"),
            Error::Parse { col: 2, span, message, .. }
                if span == (1..3) && message.ends_with("incomplete utf-8 byte sequence")
        ));
    }

    #[test]
    fn an_error_is_placed_in_the_bytes_its_position_is_of() {
        for (bytes, at) in [
            (&b"a\n b"[..], 3),
            (b"a\n b", 99),
            ("\u{feff}я\r\nb".as_bytes(), 5),
            // The snippet has U+FFFD for each, as the bytes have.
            (b"a\n\xFF\xFEb", 4),
            (b"", 0),
        ] {
            assert!(
                Error::parse_at(bytes, at..at, "m").is_placed_in(bytes),
                "{bytes:?}"
            );
        }
        // Of the text the bytes decode to: another column, another line,
        // the same column of another source line.
        let bytes = b"a\n\xFF\xFEb";
        for (text, at) in [("a\nяюb", 6), ("a\n\nb", 4), ("a\nяb", 4)] {
            let mut err = Error::parse_at(text.as_bytes(), at..at, "m");
            if let Error::Parse { span, .. } = &mut err {
                *span = 4..4;
            }
            assert!(!err.is_placed_in(bytes), "{text:?}");
        }
        let other = Error::WriteUnavailable {
            reason: "r".to_owned(),
        };
        assert!(!other.is_placed_in(b"a"));
    }

    #[test]
    fn relocated_places_a_parse_error_in_the_source_around_it() {
        let inner = Error::parse_at(b"x: [", 3..4, "m");
        let outer = b"---\nx: [\n---\n";
        assert_eq!(
            position(inner.relocated(b"x: [", outer, |at| 4 + at)),
            (2, 4, 7..8, "x: [".to_owned())
        );
        let other = Error::WriteUnavailable {
            reason: "r".to_owned(),
        };
        assert_eq!(other.place(), None);
        assert_eq!(other.with_place(), "write operation unavailable: r");
        assert_eq!(
            other.relocated(b"x", outer, |at| at).to_string(),
            "write operation unavailable: r"
        );
    }

    #[test]
    fn relocated_moves_the_place_of_a_context() {
        let message = |read: &[u8], message: &str, to: &dyn Fn(usize) -> usize| {
            let err = Error::parse_at(read, read.len()..read.len(), message);
            match err.relocated(read, "---\nя: 1\nb: [x\n---\n".as_bytes(), to) {
                Error::Parse { message, .. } => message,
                other => panic!("{other:?}"),
            }
        };
        let read = "я: 1\nb: [x\n".as_bytes();
        assert_eq!(
            message(read, "x, while parsing a y at line 2 column 4", &|at| at
                + 4),
            "x, while parsing a y at line 3 column 4"
        );
        // Dropped: at the place of the error; no place in what was read.
        for (text, to) in [
            (
                "x, while parsing a y at line 2 column 4",
                &(|_| 0_usize) as &dyn Fn(usize) -> usize,
            ),
            ("x, while parsing a y at line 9 column 4", &|at| at + 4),
            ("x, while parsing a y at line 3 column 9", &|at| at + 4),
        ] {
            assert_eq!(message(read, text, to), "x, while parsing a y", "{text}");
        }
        // No such place.
        for text in [
            "x",
            "x at line 2 column 4",
            "x, while scanning at line a column b",
            "k, while at line 2 column 4: duplicate entry with key 1",
        ] {
            assert_eq!(message(read, text, &|at| at + 4), text);
        }
    }

    #[test]
    fn offset_of_finds_the_byte_of_a_place() {
        for text in [
            "",
            "a\nя: x\n\nb",
            "a\r\nяb\r\n",
            "a\rяb\r",
            "\u{feff}a\nb\n",
        ] {
            let bytes = text.as_bytes();
            for at in bom_end(bytes)..=bytes.len() {
                let (line, col, _) = place_of(bytes, at);
                assert_eq!(offset_of(bytes, (line, col)), Some(at), "{text:?} at {at}");
            }
        }
        assert_eq!(offset_of(b"a\nb", (3, 1)), None);
        assert_eq!(offset_of(b"a\nb", (2, 3)), None);
        assert_eq!(offset_of(b"a\nb", (1, 0)), None);
    }

    #[test]
    fn place_is_that_of_a_parse_error_in_a_file() {
        let err = Error::parse_at(b"a\nb", 2..3, "m");
        assert_eq!(err.place(), Some((2, 1)));
        assert_eq!(err.with_place(), "parse error: m (line 2, column 1)");
        // A jq expression that does not compile.
        let jq = Error::Parse {
            file: None,
            line: 0,
            col: 4,
            span: 4..4,
            snippet: String::new(),
            message: "m".to_owned(),
        };
        assert_eq!(jq.place(), None);
        assert_eq!(jq.with_place(), "parse error: m");
    }

    #[test]
    fn char_span_covers_one_character() {
        let text = "aя\u{1f600}".as_bytes();
        assert_eq!(char_span(text, 0), 0..1);
        assert_eq!(char_span(text, 1), 1..3);
        assert_eq!(char_span(text, 3), 3..7);
        assert_eq!(char_span(text, 7), 7..7);
        assert_eq!(char_span(text, 99), 7..7);
        assert_eq!(char_span(b"\xFFa", 0), 0..1);
        assert_eq!(char_span(text, 2), 2..3);
    }

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

        let edit_rejected = Error::EditRejected {
            action: "delete",
            pointer: "/a".to_owned(),
            reason: "it holds an anchor".to_owned(),
        };
        assert_eq!(edit_rejected.kind_name(), "edit_rejected");
        assert_eq!(
            edit_rejected.to_string(),
            "cannot delete '/a': it holds an anchor"
        );

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
