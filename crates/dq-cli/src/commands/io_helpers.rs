//! Shared I/O and conversion helpers used by every Section-5 command handler.
//!
//! The handlers in this module follow a small set of repeated steps — open the
//! file (or stdin), pick a [`Format`] (override > extension), parse to a
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

use camino::Utf8Path;
use dq_core::{Document, Format, Pointer, Segment, Value};

use crate::cli::DocSelector;
use crate::error::{DocOutOfRange, InvalidInput};

/// Load and parse a file (or stdin when `file == "-"`), returning the chosen
/// [`Format`] together with the parsed [`Document`].
///
/// Format selection precedence:
/// 1. `format_override` (the global `-F` flag) wins when set.
/// 2. Otherwise the extension is used via [`dq_core::detect`].
/// 3. Reading from stdin (`-`) without an override is rejected — there is no
///    extension to fall back on.
///
/// `Parse` errors are tagged with the actual file path (or `None` for stdin)
/// so renderers can include it in diagnostics.
///
/// Errors are returned wrapped in `anyhow::Error::new(dq_core::Error::*)` so
/// the top-level exit-code mapper can downcast and pick the right code.
pub(crate) fn load_document_with_path(
    file: &Utf8Path,
    format_override: Option<&str>,
) -> anyhow::Result<(&'static dyn Format, Document)> {
    let format = pick_format(file, format_override)?;
    let bytes = read_bytes(file)?;
    let path_label = if file == "-" {
        None
    } else {
        Some(file.to_path_buf())
    };
    let doc = format
        .parse(&bytes)
        .map_err(|mut e| {
            if let dq_core::Error::Parse { ref mut file, .. } = e
                && file.is_none()
            {
                file.clone_from(&path_label);
            }
            e
        })
        .map_err(anyhow::Error::new)?;
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
) -> anyhow::Result<(&'static dyn Format, Document)> {
    let format = pick_format(file, format_override)?;
    let bytes = read_bytes(file)?;
    let path_label = if file == "-" {
        None
    } else {
        Some(file.to_path_buf())
    };
    let doc_result: dq_core::Result<Document> = match format.name() {
        // Stream-aware: a multi-document stream comes back as one, so the
        // lint pipeline and `dq fix` work on each document by itself.
        "yaml" => dq_core::parse_yaml_stream_with_spans(&bytes),
        "json" => dq_core::parse_json_with_spans(&bytes),
        _ => format.parse(&bytes),
    };
    let doc = doc_result
        .map_err(|mut e| {
            if let dq_core::Error::Parse { ref mut file, .. } = e
                && file.is_none()
            {
                file.clone_from(&path_label);
            }
            e
        })
        .map_err(anyhow::Error::new)?;
    Ok((format, doc))
}

/// Pick a [`Format`] for `file` from an explicit `-F` override or the file's
/// extension.
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
/// - [`InvalidInput`] when the file is `-` (stdin) and no override is provided.
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
        return Err(anyhow::Error::new(InvalidInput::new(
            "stdin requires -F <format> (no extension to detect from)",
        )));
    }
    dq_core::detect(file).ok_or_else(|| {
        let ext = file
            .extension()
            .map(str::to_owned)
            .unwrap_or_else(|| file.to_string());
        anyhow::Error::new(dq_core::Error::UnsupportedFormat { name: ext })
    })
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
/// names is not an error: `Ok(Err(reason))` says why to skip it, and the
/// run goes on with the next file. Over exactly one file — and for every
/// other error — `result` is returned as it is: a single file lacking
/// document N is exit 2, as for every single-file command.
pub(crate) fn skip_out_of_range<T>(
    result: anyhow::Result<T>,
    several: bool,
) -> anyhow::Result<Result<T, String>> {
    match result {
        Ok(value) => Ok(Ok(value)),
        Err(err) if several && err.is::<DocOutOfRange>() => Ok(Err(err.to_string())),
        Err(err) => Err(err),
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
            Ok(_) => panic!("stdin without -F must error"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("stdin"), "got: {err:?}");
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
        let (fmt, doc) = load_document_with_path(&path, None).unwrap();
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
