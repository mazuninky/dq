//! Shared I/O and conversion helpers used by every Section-5 command handler.
//!
//! The handlers in this module follow a small set of repeated steps — open the
//! file (or stdin), pick a [`Format`] (`--input-format` > extension), parse to a
//! [`Document`], and then convert pieces of the parsed `Value` back into a
//! `serde_json::Value` so [`crate::output::Reporter`] can render them. Pulling
//! the boilerplate into one helper module keeps the per-command modules
//! focused on their own logic.
//!
//! Visibility is `pub(super)` everywhere — these helpers are an internal
//! contract between the command modules and are NOT part of the public CLI
//! API. The `super` here refers to `crate::commands` (they're declared via
//! `pub mod io_helpers;`).

use std::fs;
use std::io::Read;

use camino::{Utf8Path, Utf8PathBuf};
use dq_core::template_guard::{TemplateMode, WriteGuard, guarded_parse, parse_for_write};
use dq_core::{Document, Format, Pointer, Segment, Value};

use crate::cli::{Cli, DocSelector};
use crate::error::{DocOutOfRange, DocOutOfRangeEverywhere};

/// Load and parse a file (or stdin when `file == "-"`), returning the chosen
/// [`Format`] together with the parsed [`Document`].
///
/// Format selection is [`pick_format`]'s: `format_override` (the global
/// `--input-format`), else detection; stdin without an override is a usage
/// error.
///
/// `Parse` errors are tagged with the actual file path (or `None` for stdin)
/// so renderers can include it in diagnostics.
///
/// The template guard runs before the parse is used, as `templates` says
/// ([`dq_core::template_guard::guarded_parse`]): by default a file whose
/// templates change what the parser reads is
/// [`dq_core::Error::TemplatedFile`] (exit 3).
///
/// Errors are returned wrapped in `anyhow::Error::new(dq_core::Error::*)` so
/// the top-level exit-code mapper can downcast and pick the right code.
pub(crate) fn load_document_with_path(
    file: &Utf8Path,
    format_override: Option<&str>,
    templates: TemplateMode,
) -> anyhow::Result<(&'static dyn Format, Document)> {
    let format = pick_format(file, format_override)?;
    let bytes = read_bytes(file)?;
    let doc = guarded_parse(&bytes, templates, |b| format.parse(b))
        .map_err(|e| anyhow::Error::new(name_source(e, file)))?;
    Ok((format, doc))
}

/// Load a [`Document`] for the **lint read path**, choosing the
/// span-aware parser when one exists for the format.
///
/// # Why a separate helper
///
/// `loc.pointer` resolution in the evaluator looks up
/// `Ir::line_col_for(&Pointer)`, which only resolves to a `(line, col)`
/// when the parser populated `Provenance::Original { span: Some(_), .. }`
/// for the value at that pointer. The default `Format::parse` for YAML and
/// JSON is span-LESS — it routes through `serde_norway` / `serde_json`'s
/// `Deserializer::from_slice` and produces a `Document` whose `spans` map
/// is empty, which collapses every `loc.pointer` resolution to `(1, 1)`.
/// The write-mode commands (`set`, `del`, `patch`, `merge`) already
/// dispatch through `parse_yaml_with_spans` / `parse_json_with_spans` for
/// the same reason — see e.g. `commands::set::parse_to_document`.
///
/// This helper mirrors [`load_document_with_path`] but routes YAML and
/// JSON through their span-collecting parsers. Every other format falls
/// through to `Format::parse` (span-aware for TOML via `toml_edit`,
/// span-less and read-only for the rest by design — the spec at
/// `add-ir-foundation/specs/data-query-ir/spec.md` documents the
/// fall-through to `(1, 1)` for those formats).
///
/// We deliberately do NOT change the semantics of [`load_document_with_path`]
/// — it is shared with every read command (`get`, `query`, `select`, etc.)
/// where the extra span bookkeeping is pure overhead.
pub(crate) fn load_document_for_lint(
    file: &Utf8Path,
    format_override: Option<&str>,
    templates: TemplateMode,
) -> anyhow::Result<(&'static dyn Format, Document)> {
    let format = pick_format(file, format_override)?;
    let bytes = read_bytes(file)?;
    let doc = parse_for_lint(file, format, &bytes, templates)?;
    Ok((format, doc))
}

/// Parse `bytes`, already read from `file`, as [`load_document_for_lint`]
/// does — for a caller that needs the bytes as well, so `file` is read once
/// (stdin cannot be read twice).
///
/// # Errors
///
/// [`dq_core::Error::TemplatedFile`] (exit 3) as the template guard says;
/// any parse error, naming `file` (none for stdin).
pub(crate) fn parse_for_lint(
    file: &Utf8Path,
    format: &'static dyn Format,
    bytes: &[u8],
    templates: TemplateMode,
) -> anyhow::Result<Document> {
    let doc_result = guarded_parse(bytes, templates, |b| match format.name() {
        // Stream-aware: a multi-document stream comes back as one, so the
        // lint pipeline and `dq fix` work on each document by itself.
        "yaml" => dq_core::parse_yaml_stream_with_spans(b),
        "json" => dq_core::parse_json_with_spans(b),
        _ => format.parse(b),
    });
    doc_result.map_err(|e| anyhow::Error::new(name_source(e, file)))
}

/// `err`, raised by parsing `path`: a parse error that names no file names
/// `path` — none still for stdin (`-`). Any other error as it is.
pub(crate) fn name_source(mut err: dq_core::Error, path: &Utf8Path) -> dq_core::Error {
    if let dq_core::Error::Parse { file, .. } = &mut err
        && file.is_none()
        && path != "-"
    {
        *file = Some(path.to_path_buf());
    }
    err
}

/// Parse `bytes` as `format` into a document an edit can change: YAML
/// stream-aware and JSON through their span-collecting parsers (so `--doc
/// N` tells a stream of documents from a document holding a sequence),
/// every other format through `Format::parse` (write-aware for TOML,
/// rewritten whole for XML, HCL, INI, dotenv and CSV, read-only for the
/// rest). The one parser `set`, `del`, `patch` and `merge` share.
pub(crate) fn parse_for_edit(
    format: &'static dyn Format,
    bytes: &[u8],
) -> dq_core::Result<Document> {
    match format.name() {
        "yaml" => dq_core::parse_yaml_stream_with_spans(bytes),
        "json" => dq_core::parse_json_with_spans(bytes),
        _ => format.parse(bytes),
    }
}

/// Parse `original`, the bytes of `file`, for an edit, behind the write
/// side of the template guard ([`parse_for_write`], as `cli`'s template
/// flags say). A parse error names `file`.
///
/// # Errors
///
/// [`dq_core::Error::TemplatedFile`] (exit 3) for a file whose templates
/// change what the parser reads; any parse error.
pub(crate) fn parse_file_for_edit(
    cli: &Cli,
    format: &'static dyn Format,
    original: &[u8],
    file: &Utf8Path,
) -> anyhow::Result<(Document, WriteGuard)> {
    parse_for_write(original, cli.template_mode(), |b| parse_for_edit(format, b)).map_err(
        |mut e| {
            if let dq_core::Error::Parse {
                file: ref mut at, ..
            } = e
                && at.is_none()
            {
                *at = Some(file.to_path_buf());
            }
            anyhow::Error::new(e)
        },
    )
}

/// The bytes an edit of `document` writes: [`WriteGuard::finish`] over its
/// bytes, with `edited` the pointers the edit changed on purpose and
/// `textual` whether it spliced the text (a write-aware document) rather
/// than rewriting it.
///
/// # Errors
///
/// [`dq_core::Error::TemplatedFile`] (exit 3) when the edit would not keep
/// a template as it was.
pub(crate) fn finish_edit(
    guard: &WriteGuard,
    format: &'static dyn Format,
    document: &Document,
    edited: &[Pointer],
    textual: bool,
) -> anyhow::Result<Vec<u8>> {
    guard
        .finish(document.original_bytes().to_vec(), edited, textual, |b| {
            parse_for_edit(format, b)
        })
        .map_err(anyhow::Error::new)
}

/// Pick a [`Format`] for `file`: the one `--input-format` names
/// (`format_override`), else the one its extension or name implies.
///
/// The one place a command resolves the format it reads a file — or stdin
/// — in; `-F` never reaches it.
///
/// Visibility note: exposed as `pub(crate)` so the write handlers in
/// [`crate::commands::set`] / [`crate::commands::del`] can pre-resolve the
/// format BEFORE parsing — they need the raw bytes for the
/// [`crate::cli::Cli::raw_template_strings`] template guard, which means they
/// cannot route through [`load_document_with_path`] (that helper parses
/// internally).
///
/// # Errors
///
/// - A usage error (exit 2) naming `--input-format` when the file is `-`
///   (stdin) and no override is provided: stdin has no extension.
/// - [`dq_core::Error::UnsupportedFormat`] when neither the override nor the
///   extension resolves to a known format.
pub(crate) fn pick_format(
    file: &Utf8Path,
    format_override: Option<&str>,
) -> anyhow::Result<&'static dyn Format> {
    if let Some(name) = format_override {
        let fmt = dq_core::by_name(name).ok_or_else(|| {
            anyhow::Error::new(dq_core::Error::UnsupportedFormat {
                name: name.to_owned(),
            })
        })?;
        return Ok(fmt);
    }
    if file == "-" {
        return Err(stdin_needs_input_format());
    }
    dq_core::detect(file).ok_or_else(|| {
        let ext = file
            .extension()
            .map(str::to_owned)
            .unwrap_or_else(|| file.to_string());
        anyhow::Error::new(dq_core::Error::UnsupportedFormat { name: ext })
    })
}

/// Refuse, before a write command touches any of `files`, what it cannot
/// do with them: write stdin back (`-i`) or compare it with the file on
/// disk (`--check`) — stdin has neither — and keep a file in its own
/// format while `-F` names another ([`Cli::ensure_format_kept`]).
///
/// Checked over every file up front, so a run that cannot be honoured
/// stops before it writes the first one, `--continue-on-error` or not. A
/// file whose format cannot be resolved is left to the command, which
/// reports it as it always has.
///
/// # Errors
///
/// A usage error (exit 2) for the first file that cannot be honoured.
pub(crate) fn check_write_targets(
    cli: &Cli,
    command: &str,
    files: &[Utf8PathBuf],
    format_override: Option<&str>,
) -> anyhow::Result<()> {
    for file in files {
        if file == "-" && (cli.in_place || cli.check) {
            let flag = if cli.in_place {
                "-i/--in-place"
            } else {
                "--check"
            };
            return Err(crate::cli::usage_error(
                clap::error::ErrorKind::ArgumentConflict,
                format!(
                    "{flag} needs a file on disk; `dq {command} -` reads stdin, which has none"
                ),
            ));
        }
        if let Ok(format) = pick_format(file, format_override) {
            cli.ensure_format_kept(command, file, format)?;
        }
    }
    Ok(())
}

/// The usage error (exit 2) for stdin read without `--input-format`.
pub(crate) fn stdin_needs_input_format() -> anyhow::Error {
    crate::cli::usage_error(
        clap::error::ErrorKind::MissingRequiredArgument,
        "stdin (`-`) has no extension to detect its format from; name it with --input-format <FORMAT>",
    )
}

/// Read every byte of `file` (or stdin when `file == "-"`), wrapping I/O
/// errors in [`dq_core::Error::Io`] so the exit-code mapper produces 5.
///
/// Visibility note: exposed as `pub(crate)` for the same reason as
/// [`pick_format`] — the write handlers need the raw bytes to feed the
/// template guard before parsing.
///
/// # Errors
///
/// Returns [`dq_core::Error::Io`] for any underlying I/O failure (file not
/// found, permission denied, broken pipe on stdin).
pub(crate) fn read_bytes(file: &Utf8Path) -> anyhow::Result<Vec<u8>> {
    if file == "-" {
        let mut buf = Vec::new();
        std::io::stdin().read_to_end(&mut buf).map_err(|source| {
            anyhow::Error::new(dq_core::Error::Io {
                path: camino::Utf8PathBuf::from(":stdin"),
                source,
            })
        })?;
        return Ok(buf);
    }
    fs::read(file.as_std_path()).map_err(|source| {
        anyhow::Error::new(dq_core::Error::Io {
            path: file.to_path_buf(),
            source,
        })
    })
}

/// How many documents `doc` holds: the stream's length for a
/// multi-document stream, 1 for anything else.
pub(crate) fn document_count(doc: &Document) -> usize {
    doc.values().map_or(1, <[Value]>::len)
}

/// Check `--doc N` against `doc`: `Ok(index)` when the file holds that
/// many documents, [`DocOutOfRange`] (exit 2) when it does not.
pub(crate) fn checked_index(
    doc: &Document,
    index: usize,
    file: &Utf8Path,
) -> anyhow::Result<usize> {
    let count = document_count(doc);
    if index < count {
        Ok(index)
    } else {
        Err(anyhow::Error::new(DocOutOfRange {
            index,
            count,
            file: file.to_string(),
        }))
    }
}

/// Turn a [`DocOutOfRange`] from `result` into a reason to skip the file
/// when the run covers several files.
///
/// Over several files (`several`), a file without the document `--doc N`
/// names is not an error: `Ok(Err(missing))` says why to skip it, and the
/// run goes on with the next file. Over exactly one file — and for every
/// other error — `result` is returned as it is: a single file lacking
/// document N is exit 2, as for every single-file command. A run in which
/// *every* file lacks document N is exit 2 as well —
/// [`DocOutOfRangeEverywhere`] — raised by the caller once it has seen
/// them all ([`every_file_lacks`]).
pub(crate) fn skip_out_of_range<T>(
    result: anyhow::Result<T>,
    several: bool,
) -> anyhow::Result<Result<T, DocOutOfRange>> {
    match result {
        Err(err) if several && err.is::<DocOutOfRange>() => match err.downcast::<DocOutOfRange>() {
            Ok(missing) => Ok(Err(missing)),
            Err(err) => Err(err),
        },
        other => other.map(Ok),
    }
}

/// [`DocOutOfRangeEverywhere`] when each of the `files` files of a run
/// was skipped for lacking the document `--doc N` names — `missing` holds
/// one `(file, reason)` entry per skipped file; `None` when at least one
/// file held it.
pub(crate) fn every_file_lacks<P>(
    missing: &[(P, DocOutOfRange)],
    files: usize,
) -> Option<DocOutOfRangeEverywhere> {
    match missing.first() {
        Some((_, first)) if missing.len() == files => Some(DocOutOfRangeEverywhere {
            index: first.index,
            files,
        }),
        _ => None,
    }
}

/// Resolve `--doc` against `doc`, returning the [`Value`] a read command
/// works on.
///
/// - No `--doc`: document 0 of a multi-document stream, the value of any
///   other file.
/// - `--doc N`: document `N`; a file that is not a stream is document 0.
///   An index past the last document is [`DocOutOfRange`] (exit 2).
/// - `--doc all`: an array of every document — of the one document, for a
///   file that is not a stream — so `/N/...` addresses document `N` the
///   same way whatever the file holds.
pub(crate) fn select_document<'a>(
    doc: &'a Document,
    doc_arg: Option<DocSelector>,
    file: &Utf8Path,
) -> anyhow::Result<std::borrow::Cow<'a, Value>> {
    use std::borrow::Cow;
    match (doc.values(), doc_arg) {
        (_, Some(DocSelector::All)) => Ok(Cow::Owned(Value::Array(
            doc.values()
                .map_or_else(|| vec![doc.value().clone()], <[Value]>::to_vec),
        ))),
        (Some(values), None) => values
            .first()
            .map(Cow::Borrowed)
            .ok_or_else(|| anyhow::anyhow!("multi-document stream is empty")),
        (Some(values), Some(DocSelector::Index(index))) => {
            let index = checked_index(doc, index, file)?;
            Ok(Cow::Borrowed(&values[index]))
        }
        (None, None) => Ok(Cow::Borrowed(doc.value())),
        (None, Some(DocSelector::Index(index))) => {
            checked_index(doc, index, file)?;
            Ok(Cow::Borrowed(doc.value()))
        }
    }
}

/// The pointer a `set` / `del` at `pointer` edits once `--doc N`
/// (`index`) is applied: `/N` + `pointer` in a multi-document stream,
/// `pointer` itself in any other file (where only `N == 0` exists).
/// Without `--doc` the pointer is taken as given — a stream is then
/// addressed as `/N/...`, as it always was.
pub(crate) fn pointer_in_document(
    doc: &Document,
    index: Option<usize>,
    pointer: &Pointer,
    file: &Utf8Path,
) -> anyhow::Result<Pointer> {
    let Some(index) = index else {
        return Ok(pointer.clone());
    };
    let index = checked_index(doc, index, file)?;
    if !doc.is_multi() {
        return Ok(pointer.clone());
    }
    // A `Key` segment, like the `/1` a user types: the edit is then the
    // very one `set FILE /1/...` makes.
    let mut segments = vec![Segment::Key(index.to_string())];
    segments.extend_from_slice(pointer.segments());
    Ok(Pointer::new(segments))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dq_core::Value;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn pick_format_uses_override_when_set() {
        let path = camino::Utf8PathBuf::from("does-not-matter.txt");
        let fmt = pick_format(&path, Some("json")).expect("override should win");
        assert_eq!(fmt.name(), "json");
    }

    #[test]
    fn pick_format_rejects_stdin_without_override() {
        let path = camino::Utf8PathBuf::from("-");
        // `pick_format` returns `Result<&dyn Format, anyhow::Error>` —
        // `&dyn Format` is not `Debug`, so we cannot call `.unwrap_err()`
        // directly. Match instead.
        let err = match pick_format(&path, None) {
            Ok(_) => panic!("stdin without --input-format must error"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("--input-format"), "got: {err:?}");
        assert_eq!(crate::exit_code::exit_code_for_error(&err), 2);
    }

    #[test]
    fn pick_format_falls_back_to_extension() {
        let path = camino::Utf8PathBuf::from("a.yaml");
        let fmt = pick_format(&path, None).expect("yaml extension should detect");
        assert_eq!(fmt.name(), "yaml");
    }

    #[test]
    fn pick_format_unknown_extension_errors() {
        // M11 wired up `.xml` as a registered format; pick a genuinely
        // unknown extension to keep the negative-path coverage. Any ext
        // not claimed by a registered `Format` works — `.unknownext` is
        // chosen for clarity.
        let path = camino::Utf8PathBuf::from("a.unknownext");
        let err = match pick_format(&path, None) {
            Ok(_) => panic!("unknown extension must error"),
            Err(e) => e,
        };
        let domain = err
            .downcast_ref::<dq_core::Error>()
            .expect("should downcast to dq_core::Error");
        assert_eq!(domain.kind_name(), "unsupported_format");
    }

    #[test]
    fn load_document_with_path_reads_a_yaml_file() {
        let mut tmp = NamedTempFile::with_suffix(".yaml").unwrap();
        writeln!(tmp, "a: 1").unwrap();
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let (fmt, doc) = load_document_with_path(&path, None, TemplateMode::Guard).unwrap();
        assert_eq!(fmt.name(), "yaml");
        let Value::Map(m) = doc.value() else { panic!() };
        assert_eq!(m.get("a"), Some(&Value::Int(1)));
    }

    fn file() -> &'static Utf8Path {
        Utf8Path::new("x.yaml")
    }

    #[test]
    fn select_document_default_is_index_zero() {
        let doc = Document::multi(vec![Value::Int(1), Value::Int(2)]);
        let v = select_document(&doc, None, file()).unwrap();
        assert_eq!(*v, Value::Int(1));
    }

    #[test]
    fn select_document_index_picks_that_document() {
        let doc = Document::multi(vec![Value::Int(1), Value::Int(2)]);
        let v = select_document(&doc, Some(DocSelector::Index(1)), file()).unwrap();
        assert_eq!(*v, Value::Int(2));
    }

    #[test]
    fn select_document_all_wraps_in_array() {
        let doc = Document::multi(vec![Value::Int(1), Value::Int(2)]);
        let v = select_document(&doc, Some(DocSelector::All), file()).unwrap();
        assert_eq!(*v, Value::Array(vec![Value::Int(1), Value::Int(2)]));
    }

    #[test]
    fn select_document_all_wraps_a_single_document_too() {
        let doc = Document::single(Value::Int(7));
        let v = select_document(&doc, Some(DocSelector::All), file()).unwrap();
        assert_eq!(*v, Value::Array(vec![Value::Int(7)]));
    }

    #[test]
    fn select_document_single_is_document_zero() {
        let doc = Document::single(Value::Int(7));
        let v = select_document(&doc, Some(DocSelector::Index(0)), file()).unwrap();
        assert_eq!(*v, Value::Int(7));
    }

    #[test]
    fn select_document_out_of_range_is_doc_out_of_range() {
        let single = Document::single(Value::Int(7));
        let err = select_document(&single, Some(DocSelector::Index(3)), file()).unwrap_err();
        let out = err.downcast_ref::<DocOutOfRange>().expect("DocOutOfRange");
        assert_eq!((out.index, out.count), (3, 1));

        let multi = Document::multi(vec![Value::Int(1), Value::Int(2)]);
        let err = select_document(&multi, Some(DocSelector::Index(2)), file()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "--doc 2 is out of range: x.yaml holds 2 document(s) (--doc 0 to 1)"
        );
    }

    #[test]
    fn pointer_in_document_prefixes_the_index_in_a_stream_only() {
        let pointer = Pointer::parse("/a").unwrap();
        let multi = Document::multi(vec![Value::Int(1), Value::Int(2)]);
        let rebased = pointer_in_document(&multi, Some(1), &pointer, file()).unwrap();
        assert_eq!(rebased.as_canonical(), "/1/a");
        let as_given = pointer_in_document(&multi, None, &pointer, file()).unwrap();
        assert_eq!(as_given.as_canonical(), "/a");

        let single = Document::single(Value::Int(7));
        let same = pointer_in_document(&single, Some(0), &pointer, file()).unwrap();
        assert_eq!(same.as_canonical(), "/a");
        let err = pointer_in_document(&single, Some(1), &pointer, file()).unwrap_err();
        assert!(
            err.downcast_ref::<DocOutOfRange>().is_some(),
            "got: {err:?}"
        );
    }
}
