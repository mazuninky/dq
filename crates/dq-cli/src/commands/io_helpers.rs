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
use std::str::FromStr;

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

/// Convert a [`dq_core::Value`] into a [`serde_json::Value`] so the reporter
/// can render it through one shared code path.
///
/// `BigInt` / `BigFloat` are routed through `serde_json::Number::from_str`,
/// which honours the `serde_json/arbitrary_precision` feature already enabled
/// in the workspace. When the textual literal cannot be parsed as a number
/// (e.g. malformed input), the value falls back to a `String` — losing
/// numeric typing but never panicking.
pub(crate) fn value_to_serde_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int(n) => serde_json::Value::Number((*n).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            // `serde_json::Number::from_f64` returns `None` for NaN / ±Inf —
            // the only `f64` values rejected by the JSON spec. Fall back to a
            // string so the value survives reporting (`"NaN"`, `"inf"`,
            // `"-inf"`) instead of silently becoming `Null`.
            // TODO(M2): this is a stop-gap. Proper non-finite handling
            // requires `dq-core` to track finite-vs-non-finite at parse time
            // (see fix #10's `Error::Parse` span/snippet groundwork) so
            // callers can decide whether to error or coerce per format.
            .unwrap_or_else(|| serde_json::Value::String(f.to_string())),
        Value::BigInt(s) | Value::BigFloat(s) => match serde_json::Number::from_str(s) {
            Ok(n) => serde_json::Value::Number(n),
            Err(_) => serde_json::Value::String(s.clone()),
        },
        Value::String(s) => serde_json::Value::String(s.clone()),
        Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(value_to_serde_json).collect())
        }
        Value::Map(map) => {
            // `preserve_order` is enabled in the workspace, so building a
            // `serde_json::Map` keeps insertion order. We could also use
            // `serde_json::Map::with_capacity`, but the iterator path is
            // cleaner and the perf difference is irrelevant for human-scale
            // documents.
            let mut out = serde_json::Map::new();
            for (k, child) in map {
                out.insert(k.clone(), value_to_serde_json(child));
            }
            serde_json::Value::Object(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dq_core::Value;
    use indexmap::IndexMap;
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
        let path = camino::Utf8PathBuf::from("a.xml");
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

    #[test]
    fn value_to_serde_json_handles_big_int() {
        let v = Value::BigInt("4722366482869645213696".into());
        let j = value_to_serde_json(&v);
        // With arbitrary_precision, the number round-trips as a Number node.
        assert!(j.is_number(), "expected number, got: {j:?}");
        assert_eq!(j.to_string(), "4722366482869645213696");
    }

    #[test]
    fn value_to_serde_json_preserves_map_order() {
        let mut m = IndexMap::new();
        m.insert("z".to_owned(), Value::Int(1));
        m.insert("a".to_owned(), Value::Int(2));
        let j = value_to_serde_json(&Value::Map(m));
        let serde_json::Value::Object(obj) = j else {
            panic!()
        };
        let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["z", "a"]);
    }

    #[test]
    fn value_to_serde_json_non_finite_float_becomes_string() {
        // NaN, +Inf, -Inf cannot be represented as `serde_json::Number` —
        // they would have silently become `Null` before fix #11. Now they
        // survive as strings so the user sees what was in the source data.
        for (input, expected) in [
            (f64::NAN, "NaN"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
        ] {
            let j = value_to_serde_json(&Value::Float(input));
            let serde_json::Value::String(s) = j else {
                panic!("expected string fallback for {expected}, got: {j:?}");
            };
            assert_eq!(s, expected, "wrong fallback string for {expected}");
        }
    }

    #[test]
    fn value_to_serde_json_finite_float_stays_a_number() {
        let j = value_to_serde_json(&Value::Float(3.5));
        assert!(j.is_number(), "finite floats must remain numbers: {j:?}");
    }
}
