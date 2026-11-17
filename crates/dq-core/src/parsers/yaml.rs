//! YAML 1.2 parser and writer (single- and multi-document).
//!
//! Number handling matches the JSON parser: integers that overflow `i64` are
//! captured as `Value::BigInt(literal)`, and floats whose `f64` round-trip
//! would be lossy become `Value::BigFloat(literal)`. M1 is read-only — comment
//! preservation, anchor names, and quote style arrive in M2 with the
//! event-API rewrite.

use std::io::Write;
use std::str::FromStr;

use indexmap::IndexMap;
use serde::Deserialize;
use serde_norway::Value as YamlValue;

use crate::Result;
use crate::WriteOptions;
use crate::document::{Document, Value};
use crate::error::Error;
use crate::format::Format;
use crate::write_options::canonicalize_keys;

/// YAML format implementation.
#[derive(Debug, Clone, Copy)]
pub struct Yaml;

impl Format for Yaml {
    fn name(&self) -> &'static str {
        "yaml"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["yaml", "yml"]
    }

    fn parse(&self, bytes: &[u8]) -> Result<Document> {
        let mut documents: Vec<Value> = Vec::new();
        for de_result in serde_norway::Deserializer::from_slice(bytes) {
            let yaml: YamlValue = YamlValue::deserialize(de_result).map_err(map_yaml_err)?;
            documents.push(yaml_value_to_value(yaml));
        }
        match documents.len() {
            0 => Ok(Document::value_only(
                Value::Null,
                crate::document::FormatTag::Yaml,
            )),
            1 => Ok(Document::value_only(
                documents.remove(0),
                crate::document::FormatTag::Yaml,
            )),
            _ => Ok(Document::multi_value_only(
                documents,
                crate::document::FormatTag::Yaml,
            )),
        }
    }

    fn write(&self, doc: &Document, w: &mut dyn Write) -> Result<()> {
        if let Some(values) = doc.values() {
            for (i, v) in values.iter().enumerate() {
                if i > 0 {
                    // YAML stream document separator.
                    write_io(w, b"---\n")?;
                }
                write_one(v, w)?;
            }
            Ok(())
        } else {
            write_one(doc.value(), w)
        }
    }

    fn write_with_options(
        &self,
        doc: &Document,
        w: &mut dyn Write,
        opts: &WriteOptions,
    ) -> Result<()> {
        // YAML ignores `opts.indent` in M4 (per design D6 — the
        // `serde_norway` emitter does not expose an indent knob and the
        // event-API rewrite that would land it is deferred to M5+). The CLI
        // dispatch layer is responsible for emitting a `tracing::warn!`
        // when a user passes `--indent` with a YAML output; we keep the
        // core silent so it does not pull in `tracing` for a UX concern.
        if !opts.sort_keys {
            return self.write(doc, w);
        }
        if let Some(values) = doc.values() {
            for (i, v) in values.iter().enumerate() {
                if i > 0 {
                    write_io(w, b"---\n")?;
                }
                let canon = canonicalize_keys(v);
                write_one(&canon, w)?;
            }
            Ok(())
        } else {
            let canon = canonicalize_keys(doc.value());
            write_one(&canon, w)
        }
    }
}

fn write_one(v: &Value, w: &mut dyn Write) -> Result<()> {
    let s = serde_norway::to_string(v).map_err(map_yaml_err)?;
    write_io(w, s.as_bytes())
}

fn map_yaml_err(e: serde_norway::Error) -> Error {
    let location = e.location();
    Error::Parse {
        file: None,
        line: location.as_ref().map(|l| l.line() as u32).unwrap_or(0),
        col: location.as_ref().map(|l| l.column() as u32).unwrap_or(0),
        span: 0..0,
        snippet: String::new(),
        message: e.to_string(),
    }
}

fn yaml_value_to_value(v: YamlValue) -> Value {
    match v {
        YamlValue::Null => Value::Null,
        YamlValue::Bool(b) => Value::Bool(b),
        YamlValue::Number(n) => yaml_number_to_value(&n),
        YamlValue::String(s) => Value::String(s),
        YamlValue::Sequence(items) => {
            Value::Array(items.into_iter().map(yaml_value_to_value).collect())
        }
        YamlValue::Mapping(map) => {
            let mut out = IndexMap::with_capacity(map.len());
            for (k, val) in map {
                let key_str = stringify_key(k);
                out.insert(key_str, yaml_value_to_value(val));
            }
            Value::Map(out)
        }
        // Tagged scalars (`!!binary`, custom tags) are flattened to their
        // underlying value in M1. Preserving the tag verbatim is a M2
        // round-trip concern.
        YamlValue::Tagged(tagged) => yaml_value_to_value(tagged.value),
    }
}

fn stringify_key(k: YamlValue) -> String {
    match k {
        YamlValue::String(s) => s,
        YamlValue::Bool(b) => b.to_string(),
        YamlValue::Number(n) => n.to_string(),
        YamlValue::Null => String::new(),
        // Composite keys (sequence/mapping/tagged) are rare in real configs;
        // M1 surfaces them as their textual form rather than failing.
        other => format!("{other:?}"),
    }
}

fn yaml_number_to_value(n: &serde_norway::Number) -> Value {
    if let Some(i) = n.as_i64() {
        return Value::Int(i);
    }
    if n.is_f64()
        && let Some(f) = n.as_f64()
    {
        let literal = n.to_string();
        if f.is_finite() && f64_round_trip_ok(f, &literal) {
            return Value::Float(f);
        }
        return Value::BigFloat(literal);
    }
    // u64 that exceeds i64::MAX or other big-int forms: keep textual literal.
    Value::BigInt(n.to_string())
}

fn f64_round_trip_ok(f: f64, literal: &str) -> bool {
    let formatted = f.to_string();
    f64::from_str(&formatted)
        .ok()
        .zip(f64::from_str(literal).ok())
        .is_some_and(|(a, b)| a.to_bits() == b.to_bits())
}

fn write_io(w: &mut dyn Write, bytes: &[u8]) -> Result<()> {
    w.write_all(bytes).map_err(|source| Error::Io {
        path: camino::Utf8PathBuf::from("<yaml-writer>"),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Document {
        Yaml.parse(s.as_bytes()).unwrap()
    }

    #[test]
    fn parse_basic_mapping_preserves_order() {
        let doc = parse("z: 1\na: 2\nm: 3\n");
        let Value::Map(m) = doc.value() else { panic!() };
        let keys: Vec<&str> = m.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["z", "a", "m"]);
    }

    #[test]
    fn parse_multi_doc_yields_multi() {
        let doc = parse("---\na: 1\n---\nb: 2\n");
        let docs = doc
            .values()
            .unwrap_or_else(|| panic!("expected multi, got: {doc:?}"));
        assert_eq!(docs.len(), 2);
    }

    #[test]
    fn parse_scalar_types() {
        let doc = parse("a: true\nb: 1\nc: 1.5\nd: ~\ne: hello\n");
        let Value::Map(m) = doc.value() else { panic!() };
        assert_eq!(m.get("a"), Some(&Value::Bool(true)));
        assert_eq!(m.get("b"), Some(&Value::Int(1)));
        assert_eq!(m.get("d"), Some(&Value::Null));
        assert_eq!(m.get("e"), Some(&Value::String("hello".into())));
    }

    #[test]
    fn write_round_trip_basic_mapping() {
        let doc = parse("a: 1\nb: 2\n");
        let mut buf: Vec<u8> = Vec::new();
        Yaml.write(&doc, &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        let again = Yaml.parse(out.as_bytes()).unwrap();
        assert_eq!(doc, again);
    }
}
