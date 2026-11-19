//! YAML 1.2 parser and writer (single- and multi-document).
//!
//! Numbers keep their exact value, as in the JSON parser: an integer past
//! `i64` is a `Value::BigInt(literal)`, and a float literal no `f64` holds —
//! more digits than it has (`0.1000000000000000000000000001`), or past its
//! range (`1e400`) — a `Value::BigFloat(literal)`. serde_norway's
//! `Value` holds neither, so the tree is read into this module's own
//! [`Node`], which takes the `u128` / `i128` integers serde_norway hands
//! over, and [`super::yaml_numbers`] restores the literals it rounds to an
//! `f64` or reads as a string. M1 is read-only — comment preservation,
//! anchor names, and quote style arrive in M2 with the event-API rewrite.

use std::collections::HashSet;
use std::fmt;
use std::io::Write;

use indexmap::IndexMap;
use serde::Deserialize;
use serde::de::{self, EnumAccess, MapAccess, SeqAccess, VariantAccess, Visitor};
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
        // A byte order mark is not content. serde_norway's scanner reads a
        // leading one as part of the first line, and a second top-level key
        // then fails with "did not find expected <document start>".
        let bytes = bytes.strip_prefix("\u{feff}".as_bytes()).unwrap_or(bytes);
        let mut documents: Vec<Value> = Vec::new();
        for de_result in serde_norway::Deserializer::from_slice(bytes) {
            let yaml = Node::deserialize(de_result).map_err(map_yaml_err)?;
            documents.push(node_to_value(yaml));
        }
        if super::yaml_numbers::may_lose_numbers(bytes) {
            super::yaml_numbers::restore_exact_numbers(bytes, &mut documents);
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
    let s = super::yaml_emit::to_yaml_string(v).map_err(map_yaml_err)?;
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

/// A YAML node as serde_norway reads it — [`YamlValue`]'s shape, plus the
/// integers past `u64` / `i64` that `YamlValue` has no room for (its
/// visitor fails on them with "invalid type: integer as u128").
#[derive(Debug)]
enum Node {
    Null,
    Bool(bool),
    Number(serde_norway::Number),
    /// An integer in `u128` / `i128` range past `u64` / `i64`, in decimal.
    BigInt(String),
    String(String),
    Sequence(Vec<Node>),
    Mapping(Vec<(Node, Node)>),
    Tagged(String, Box<Node>),
}

impl Node {
    /// The [`YamlValue`] this node is, `None` when it is or holds a
    /// [`Node::BigInt`].
    fn to_yaml_value(&self) -> Option<YamlValue> {
        Some(match self {
            Self::Null => YamlValue::Null,
            Self::Bool(b) => YamlValue::Bool(*b),
            Self::Number(n) => YamlValue::Number(n.clone()),
            Self::BigInt(_) => return None,
            Self::String(s) => YamlValue::String(s.clone()),
            Self::Sequence(items) => YamlValue::Sequence(
                items
                    .iter()
                    .map(Self::to_yaml_value)
                    .collect::<Option<_>>()?,
            ),
            Self::Mapping(entries) => YamlValue::Mapping(
                entries
                    .iter()
                    .map(|(k, v)| Some((k.to_yaml_value()?, v.to_yaml_value()?)))
                    .collect::<Option<_>>()?,
            ),
            Self::Tagged(tag, value) => {
                YamlValue::Tagged(Box::new(serde_norway::value::TaggedValue {
                    tag: serde_norway::value::Tag::new(tag.clone()),
                    value: value.to_yaml_value()?,
                }))
            }
        })
    }
}

impl<'de> Deserialize<'de> for Node {
    fn deserialize<D: de::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        deserializer.deserialize_any(NodeVisitor)
    }
}

/// [`YamlValue`]'s visitor, with `visit_u128` / `visit_i128`.
struct NodeVisitor;

impl<'de> Visitor<'de> for NodeVisitor {
    type Value = Node;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("any YAML value")
    }

    fn visit_bool<E: de::Error>(self, b: bool) -> std::result::Result<Node, E> {
        Ok(Node::Bool(b))
    }

    fn visit_i64<E: de::Error>(self, i: i64) -> std::result::Result<Node, E> {
        Ok(Node::Number(i.into()))
    }

    fn visit_u64<E: de::Error>(self, u: u64) -> std::result::Result<Node, E> {
        Ok(Node::Number(u.into()))
    }

    fn visit_i128<E: de::Error>(self, i: i128) -> std::result::Result<Node, E> {
        Ok(i64::try_from(i)
            .map_or_else(|_| Node::BigInt(i.to_string()), |i| Node::Number(i.into())))
    }

    fn visit_u128<E: de::Error>(self, u: u128) -> std::result::Result<Node, E> {
        Ok(u64::try_from(u)
            .map_or_else(|_| Node::BigInt(u.to_string()), |u| Node::Number(u.into())))
    }

    fn visit_f64<E: de::Error>(self, f: f64) -> std::result::Result<Node, E> {
        Ok(Node::Number(f.into()))
    }

    fn visit_str<E: de::Error>(self, s: &str) -> std::result::Result<Node, E> {
        Ok(Node::String(s.to_owned()))
    }

    fn visit_string<E: de::Error>(self, s: String) -> std::result::Result<Node, E> {
        Ok(Node::String(s))
    }

    fn visit_unit<E: de::Error>(self) -> std::result::Result<Node, E> {
        Ok(Node::Null)
    }

    fn visit_none<E: de::Error>(self) -> std::result::Result<Node, E> {
        Ok(Node::Null)
    }

    fn visit_some<D: de::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<Node, D::Error> {
        Node::deserialize(deserializer)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut data: A) -> std::result::Result<Node, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = data.next_element::<Node>()? {
            items.push(item);
        }
        Ok(Node::Sequence(items))
    }

    /// Fails on a key seen before in the mapping, as `YamlValue`'s
    /// `Mapping` does, with its message.
    fn visit_map<A: MapAccess<'de>>(self, mut data: A) -> std::result::Result<Node, A::Error> {
        let mut entries = Vec::new();
        // A key holding a big integer is never equal to one without.
        let mut seen: HashSet<YamlValue> = HashSet::new();
        let mut seen_big: HashSet<String> = HashSet::new();
        while let Some(key) = data.next_key::<Node>()? {
            let duplicate = match key.to_yaml_value() {
                Some(plain) => {
                    let message = duplicate_key_message(&plain);
                    (!seen.insert(plain)).then_some(message)
                }
                None => {
                    let message = match &key {
                        Node::BigInt(s) => format!("duplicate entry with key {s}"),
                        _ => "duplicate entry in YAML map".to_owned(),
                    };
                    (!seen_big.insert(format!("{key:?}"))).then_some(message)
                }
            };
            if let Some(message) = duplicate {
                return Err(de::Error::custom(message));
            }
            let value = data.next_value::<Node>()?;
            entries.push((key, value));
        }
        Ok(Node::Mapping(entries))
    }

    fn visit_enum<A: EnumAccess<'de>>(self, data: A) -> std::result::Result<Node, A::Error> {
        let (tag, contents) = data.variant::<String>()?;
        if tag.is_empty() {
            return Err(de::Error::custom("empty YAML tag is not allowed"));
        }
        let value = contents.newtype_variant::<Node>()?;
        Ok(Node::Tagged(tag, Box::new(value)))
    }
}

/// serde_norway's message for a duplicate mapping key.
fn duplicate_key_message(key: &YamlValue) -> String {
    match key {
        YamlValue::Null => "duplicate entry with null key".to_owned(),
        YamlValue::Bool(b) => format!("duplicate entry with key `{b}`"),
        YamlValue::Number(n) => format!("duplicate entry with key {n}"),
        YamlValue::String(s) => format!("duplicate entry with key {s:?}"),
        YamlValue::Sequence(_) | YamlValue::Mapping(_) | YamlValue::Tagged(_) => {
            "duplicate entry in YAML map".to_owned()
        }
    }
}

fn node_to_value(node: Node) -> Value {
    match node {
        Node::Null => Value::Null,
        Node::Bool(b) => Value::Bool(b),
        Node::Number(n) => yaml_number_to_value(&n),
        Node::BigInt(s) => Value::BigInt(s),
        Node::String(s) => Value::String(s),
        Node::Sequence(items) => Value::Array(items.into_iter().map(node_to_value).collect()),
        Node::Mapping(entries) => {
            let mut out = IndexMap::with_capacity(entries.len());
            for (k, val) in entries {
                let key_str = stringify_key(k);
                out.insert(key_str, node_to_value(val));
            }
            Value::Map(out)
        }
        // Tagged scalars (`!!binary`, custom tags) are flattened to their
        // underlying value in M1. Preserving the tag verbatim is a M2
        // round-trip concern.
        Node::Tagged(_, value) => node_to_value(*value),
    }
}

fn stringify_key(k: Node) -> String {
    match k {
        Node::String(s) | Node::BigInt(s) => s,
        Node::Bool(b) => b.to_string(),
        Node::Number(n) => n.to_string(),
        Node::Null => String::new(),
        // Composite keys (sequence/mapping/tagged) are rare in real configs;
        // M1 surfaces them as their textual form rather than failing.
        other => match other.to_yaml_value() {
            Some(plain) => format!("{plain:?}"),
            None => format!("{other:?}"),
        },
    }
}

/// The [`Value`] of a number serde_norway read: an `f64` it rounded a long
/// literal to stays that `f64` here — [`super::yaml_numbers`] puts the
/// literal back. A NaN or an infinity keeps serde_norway's spelling
/// (`.nan`, `-.inf`) as a [`Value::BigFloat`].
fn yaml_number_to_value(n: &serde_norway::Number) -> Value {
    if let Some(i) = n.as_i64() {
        return Value::Int(i);
    }
    if n.is_f64()
        && let Some(f) = n.as_f64()
    {
        if f.is_finite() {
            return Value::Float(f);
        }
        return Value::BigFloat(n.to_string());
    }
    // u64 that exceeds i64::MAX: keep the textual literal.
    Value::BigInt(n.to_string())
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
    fn parse_skips_a_byte_order_mark_before_several_keys() {
        let doc = parse("\u{feff}a: 1\nb: 2\n");
        let Value::Map(m) = doc.value() else { panic!() };
        let keys: Vec<&str> = m.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["a", "b"]);
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
    fn parse_reads_integers_past_u64_and_i64() {
        let doc = parse(
            "a: 18446744073709551616\nb: -9223372036854775809\nc: 0x1ffffffffffffffff\n18446744073709551616: k\n",
        );
        let Value::Map(m) = doc.value() else { panic!() };
        assert_eq!(m["a"], Value::BigInt("18446744073709551616".into()));
        assert_eq!(m["b"], Value::BigInt("-9223372036854775809".into()));
        assert_eq!(m["c"], Value::BigInt("36893488147419103231".into()));
        assert_eq!(m["18446744073709551616"], Value::String("k".into()));
    }

    #[test]
    fn parse_fails_on_a_duplicate_key_as_serde_norway_does() {
        for (text, message) in [
            ("a: 1\na: 2\n", "duplicate entry with key \"a\""),
            ("1: 1\n1: 2\n", "duplicate entry with key 1"),
            (
                "18446744073709551616: 1\n18446744073709551616: 2\n",
                "duplicate entry with key 18446744073709551616",
            ),
        ] {
            let err = Yaml.parse(text.as_bytes()).unwrap_err();
            let Error::Parse { message: got, .. } = err else {
                panic!("{err:?}")
            };
            assert!(got.starts_with(message), "{got}");
        }
        // Word for word, position included.
        for text in [
            "a: 1\na: 2\n",
            "x:\n  - {~: 1, null: 2}\n",
            "[1]: 1\n[1]: 2\n",
            "a: [1, !x {}]\n",
        ] {
            let ours = Yaml
                .parse(text.as_bytes())
                .map(|_| ())
                .map_err(|e| e.to_string());
            let theirs = serde_norway::from_str::<YamlValue>(text)
                .map(|_| ())
                .map_err(|e| map_yaml_err(e).to_string());
            assert_eq!(ours, theirs, "{text}");
        }
        // A big-integer key and the same digits quoted are two keys.
        assert!(
            Yaml.parse(b"18446744073709551616: 1\n'18446744073709551616': 2\n")
                .is_ok()
        );
    }

    #[test]
    fn parse_keeps_tags_flattened_and_composite_keys_as_text() {
        let doc = parse("a: !foo 18446744073709551616\n? [1, 2]\n: b\n");
        let Value::Map(m) = doc.value() else { panic!() };
        assert_eq!(m["a"], Value::BigInt("18446744073709551616".into()));
        let keys: Vec<&str> = m.keys().map(String::as_str).collect();
        assert_eq!(keys[1], "Sequence [Number(1), Number(2)]");
    }

    #[test]
    fn write_quotes_keys_that_would_read_back_as_null() {
        // `""`, `~` and `null` keys all read as the empty key; `"null"`
        // and `"~"` stay strings.
        let doc = parse("\"\": 1\n\"null\": 2\n\"~\": 3\n");
        let mut buf: Vec<u8> = Vec::new();
        Yaml.write(&doc, &mut buf).unwrap();
        assert_eq!(
            String::from_utf8(buf.clone()).unwrap(),
            "'': 1\n'null': 2\n'~': 3\n"
        );
        assert_eq!(Yaml.parse(&buf).unwrap().value(), doc.value());
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
