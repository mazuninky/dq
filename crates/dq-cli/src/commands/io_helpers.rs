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
use dq_core::{Document, Format, Value};

use crate::error::InvalidInput;

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
/// JSON is span-LESS — it routes through `serde_yml` / `serde_json`'s
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
        "yaml" => dq_core::parse_yaml_with_spans(&bytes),
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

/// Resolve `cli.doc` (`"all"`, `"<idx>"`, or `None`) against `doc`, returning
/// the [`Value`] view to operate on.
///
/// - Single-document `Document` → its `value()` regardless of `doc_arg`.
/// - Multi-document `Document`: `Some("all")` → array of every doc,
///   `Some(n)` or `None` → the requested index (default 0). Out-of-range
///   index returns a structured anyhow error mapped to exit code 1.
pub(crate) fn select_document<'a>(
    doc: &'a Document,
    doc_arg: Option<&str>,
) -> anyhow::Result<std::borrow::Cow<'a, Value>> {
    use std::borrow::Cow;
    if let Some(values) = doc.values() {
        match doc_arg {
            Some("all") => Ok(Cow::Owned(Value::Array(values.to_vec()))),
            Some(s) => {
                let idx: usize = s.parse().map_err(|_| {
                    anyhow::anyhow!("--doc must be 'all' or a non-negative integer, got {s:?}")
                })?;
                values.get(idx).map(Cow::Borrowed).ok_or_else(|| {
                    anyhow::anyhow!(
                        "--doc index {} out of range (have {} document(s))",
                        idx,
                        values.len()
                    )
                })
            }
            None => values
                .first()
                .map(Cow::Borrowed)
                .ok_or_else(|| anyhow::anyhow!("multi-document stream is empty")),
        }
    } else {
        Ok(Cow::Borrowed(doc.value()))
    }
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

    #[test]
    fn select_document_default_is_index_zero() {
        let doc = Document::multi(vec![Value::Int(1), Value::Int(2)]);
        let v = select_document(&doc, None).unwrap();
        assert_eq!(*v, Value::Int(1));
    }

    #[test]
    fn select_document_all_wraps_in_array() {
        let doc = Document::multi(vec![Value::Int(1), Value::Int(2)]);
        let v = select_document(&doc, Some("all")).unwrap();
        let Value::Array(items) = v.as_ref() else {
            panic!("expected array, got {v:?}");
        };
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn select_document_single_ignores_doc_arg() {
        let doc = Document::single(Value::Int(7));
        let v = select_document(&doc, Some("3")).unwrap();
        assert_eq!(*v, Value::Int(7));
    }
}
