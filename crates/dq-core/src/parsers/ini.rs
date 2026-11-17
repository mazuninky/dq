//! INI / Java `.properties` parser and writer (via `rust-ini`).
//!
//! Top-level shape: `Map<section-name, Map<key, String>>`. Sections preserve
//! source order. Keys before the first `[header]` are stored under the
//! empty-string section name `""`. The `:` separator (Java `.properties`)
//! is accepted alongside `=` because `rust-ini` accepts both by default.
//!
//! Comments and the original quote style are NOT preserved through round-trip
//! (per spec D5 — quote preservation is a nice-to-have, not a contract).

use std::io::Write;

use camino::Utf8PathBuf;
use indexmap::IndexMap;
use ini::Ini as RustIni;

use crate::Result;
use crate::WriteOptions;
use crate::document::{Document, FormatTag, Value};
use crate::error::Error;
use crate::format::Format;
use crate::write_options::canonicalize_keys;

/// INI / `.properties` format implementation.
#[derive(Debug, Clone, Copy)]
pub struct Ini;

impl Format for Ini {
    fn name(&self) -> &'static str {
        "ini"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["ini", "properties", "cfg"]
    }

    fn parse(&self, bytes: &[u8]) -> Result<Document> {
        let text = std::str::from_utf8(bytes).map_err(|e| Error::Parse {
            file: None,
            line: 0,
            col: 0,
            span: 0..0,
            snippet: String::new(),
            message: format!("invalid UTF-8 in INI input: {e}"),
        })?;
        let parsed = RustIni::load_from_str(text).map_err(|e| Error::Parse {
            file: None,
            line: e.line as u32,
            col: e.col as u32,
            span: 0..0,
            snippet: String::new(),
            message: e.msg.into_owned(),
        })?;

        let mut top: IndexMap<String, Value> = IndexMap::new();
        for (section_name, props) in parsed.iter() {
            // `rust-ini` always exposes an anonymous (`None`) section even for
            // sources with no anonymous keys. Skip the empty placeholder so
            // `dq paths` doesn't return a phantom "/" pointer and so the
            // top-level map only carries sections the user actually wrote.
            // When the source DOES have anonymous keys, we still surface them
            // under the empty-string key `""`.
            if section_name.is_none() && props.is_empty() {
                continue;
            }
            // The anonymous section is keyed under `""`. `rust-ini` represents
            // it via `None`; collapse to empty-string so callers can address
            // it through a stable pointer.
            let key = section_name.unwrap_or("").to_owned();
            let mut section_map: IndexMap<String, Value> = IndexMap::new();
            for (k, v) in props.iter() {
                section_map.insert(k.to_owned(), Value::String(v.to_owned()));
            }
            // Multiple section blocks with the same name (`rust-ini` does
            // surface them separately) collapse into the same map; the later
            // one wins on key collision, matching the in-memory union shape
            // most readers expect.
            match top.get_mut(&key) {
                Some(Value::Map(existing)) => {
                    for (k, v) in section_map {
                        existing.insert(k, v);
                    }
                }
                _ => {
                    top.insert(key, Value::Map(section_map));
                }
            }
        }
        Ok(Document::value_only(Value::Map(top), FormatTag::Ini))
    }

    fn write(&self, doc: &Document, w: &mut dyn Write) -> Result<()> {
        let Value::Map(top) = doc.value() else {
            return Err(Error::Format {
                format: "ini",
                message: format!(
                    "expected top-level map<section, map<key, string>>, got {}",
                    doc.value().type_name(),
                ),
            });
        };
        let mut out = RustIni::new();
        for (section_name, section_value) in top {
            let Value::Map(section_map) = section_value else {
                return Err(Error::Format {
                    format: "ini",
                    message: format!(
                        "section '{section_name}' must be a map of string values, got {}",
                        section_value.type_name(),
                    ),
                });
            };
            // `None` for the anonymous section, `Some(name)` otherwise.
            let section_arg: Option<&str> = if section_name.is_empty() {
                None
            } else {
                Some(section_name.as_str())
            };
            // `with_section` borrows the Ini and its setter holds a borrow
            // until dropped; iterate inside the same scope.
            let mut setter = out.with_section(section_arg.map(str::to_owned));
            for (key, val) in section_map {
                let v_str = scalar_to_ini_string(section_name, key, val)?;
                setter.set(key.clone(), v_str);
            }
        }
        // `Ini::write_to` requires `W: Write + Sized`; the trait-object
        // writer here isn't sized, so render through an intermediate buffer
        // and copy it out. INI files are small in practice — the buffer
        // round-trip is negligible.
        let mut buf: Vec<u8> = Vec::new();
        out.write_to(&mut buf).map_err(|source| Error::WriteIo {
            path: Utf8PathBuf::from("<ini-writer>"),
            source,
        })?;
        w.write_all(&buf).map_err(|source| Error::WriteIo {
            path: Utf8PathBuf::from("<ini-writer>"),
            source,
        })
    }

    fn write_with_options(
        &self,
        doc: &Document,
        w: &mut dyn Write,
        opts: &WriteOptions,
    ) -> Result<()> {
        // `--indent` is a no-op for INI: the format has no nested indentation
        // to width. Only `--sort-keys` has a meaningful effect, sorting both
        // the section names and the keys within each section alphabetically.
        if !opts.sort_keys {
            return self.write(doc, w);
        }
        // Deep canonicalize: section names sorted alphabetically and, because
        // each section's value is itself a `Value::Map`, the per-section keys
        // are sorted too. We re-emit through `Document::value_only` because
        // the INI writer ignores spans and operates entirely off the value
        // tree — the rendered bytes are determined by `top` alone.
        let canon = canonicalize_keys(doc.value());
        let canon_doc = Document::value_only(canon, FormatTag::Ini);
        self.write(&canon_doc, w)
    }
}

/// Convert a leaf value into the string `rust-ini` writes verbatim.
///
/// INI is fundamentally `string → string`; numeric / boolean scalars in the
/// in-memory tree (typically introduced by callers converting from another
/// format) are stringified through `Display`. Containers (Map / Array) are
/// not representable in INI and surface as `Error::Format`.
fn scalar_to_ini_string(section: &str, key: &str, val: &Value) -> Result<String> {
    match val {
        Value::String(s) => Ok(s.clone()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Int(n) => Ok(n.to_string()),
        Value::Float(n) => Ok(n.to_string()),
        Value::BigInt(s) | Value::BigFloat(s) => Ok(s.clone()),
        Value::Null => Ok(String::new()),
        Value::Array(_) | Value::Map(_) => Err(Error::Format {
            format: "ini",
            message: format!(
                "section '{section}' key '{key}': nested {} cannot be serialized to INI",
                val.type_name(),
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_section_and_key() {
        let doc = Ini.parse(b"[s]\nk = v\n").expect("parse simple ini");
        let Value::Map(top) = doc.value() else {
            panic!("expected top map")
        };
        let Some(Value::Map(s)) = top.get("s") else {
            panic!("expected section map for 's'")
        };
        assert_eq!(s.get("k"), Some(&Value::String("v".into())));
    }

    #[test]
    fn parse_anonymous_section_keyed_under_empty_string() {
        let doc = Ini
            .parse(b"foo = bar\n[s]\nk = v\n")
            .expect("parse with anon section");
        let Value::Map(top) = doc.value() else {
            panic!()
        };
        let Some(Value::Map(anon)) = top.get("") else {
            panic!("anonymous section missing")
        };
        assert_eq!(anon.get("foo"), Some(&Value::String("bar".into())));
    }

    #[test]
    fn write_round_trips_section_order() {
        let doc = Ini
            .parse(b"[c]\nx = 1\n[a]\ny = 2\n[b]\nz = 3\n")
            .expect("parse");
        let mut buf: Vec<u8> = Vec::new();
        Ini.write(&doc, &mut buf).expect("write");
        let out = String::from_utf8(buf).expect("utf8");
        // Section headers should appear in source order.
        let pos_c = out.find("[c]").expect("[c] missing");
        let pos_a = out.find("[a]").expect("[a] missing");
        let pos_b = out.find("[b]").expect("[b] missing");
        assert!(pos_c < pos_a, "c before a");
        assert!(pos_a < pos_b, "a before b");
    }
}
