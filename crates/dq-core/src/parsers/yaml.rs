//! YAML 1.2 parser and writer (single- and multi-document).
//!
//! Numbers keep their exact value, as in the JSON parser: an integer past
//! `i64` is a `Value::BigInt(literal)`, and a float literal no `f64` holds —
//! more digits than it has (`0.1000000000000000000000000001`), or past its
//! range (`1e400`) — a `Value::BigFloat(literal)`. serde_norway's
//! `Value` holds neither, so the tree is read into this module's own
//! [`Node`], which takes the `u128` / `i128` integers serde_norway hands
//! over; a file holding a literal serde_norway rounds to an `f64` or
//! reads as a string is read again by [`super::yaml_numbers`], which keeps
//! it. A mapping key that repeats an earlier one — by value, as
//! serde_norway checks it, or once both are the strings a [`Value::Map`]
//! holds (`1` and `"1"`) — fails the file.
//! M1 is read-only — comment preservation, anchor names, and quote style
//! arrive in M2 with the event-API rewrite.

use std::collections::HashSet;
use std::fmt;
use std::hash::BuildHasher as _;
use std::io::Write;
use std::str::Utf8Error;

use indexmap::IndexMap;
use serde::de::{
    self, DeserializeSeed as _, EnumAccess, MapAccess, SeqAccess, VariantAccess, Visitor,
};
use serde_norway::Value as YamlValue;

use crate::Result;
use crate::WriteOptions;
use crate::document::{Document, Value, bom_end};
use crate::error::{Error, invalid_utf8_span, split_context};
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
        let bom = bom_end(bytes);
        // What is read may stand in for the file's own text, byte for byte:
        // an error is placed in the file.
        let text = &bytes[bom..];
        let mut documents =
            read_documents(text).map_err(|e| e.relocated(text, bytes, |at| bom + at))?;
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

/// The value of each document of `bytes`: as serde_norway reads it when
/// no literal can need more than it keeps, else as
/// [`super::yaml_numbers::read_exact`] reads it — also for a key that
/// repeats an earlier one, which that reader names where it repeats, and
/// for a `!!float` / `!!int` number serde_norway refuses.
///
/// A plain scalar under a tag of no schema dq knows — a `%TAG` handle
/// bound to another prefix (`!!str` after `%TAG !! tag:example.com,2000:`),
/// a verbatim `!<tag:example.com,2000:x>` — reads as untagged, as one under
/// a local tag (`!foo`) does ([`super::yaml_numbers::untag_foreign`]). An
/// empty `!!null` node reads as null
/// ([`super::yaml_numbers::untag_empty_nulls`]).
fn read_documents(bytes: &[u8]) -> Result<Vec<Value>> {
    // libyaml reads by the block: it meets bytes that are no UTF-8 before
    // or after another fault, as the size of its buffer has it.
    let text = std::str::from_utf8(bytes).map_err(|e| no_utf8(bytes, e))?;
    let untagged = super::yaml_numbers::untag_foreign(text);
    let text = untagged.as_deref().unwrap_or(text);
    let nulls = super::yaml_numbers::untag_empty_nulls(text);
    let text = nulls.as_deref().unwrap_or(text);
    if !super::yaml_numbers::may_lose_numbers(text.as_bytes()) {
        match read_nodes(text, true) {
            Ok(nodes) => return Ok(nodes.into_iter().map(node_to_value).collect()),
            Err(err) if is_duplicate_key(&err) || is_tagged_number(&err) => {}
            Err(err) => return Err(map_yaml_err(&err, text)),
        }
    }
    super::yaml_numbers::read_exact(text)
}

/// The error for `bytes` that are no UTF-8, at the first that are none, in
/// the words of libyaml's reader, which gives the place of another byte of
/// the sequence for some.
fn no_utf8(bytes: &[u8], err: Utf8Error) -> Error {
    let bad = invalid_utf8_span(bytes, err);
    // The reader fails on the first sequence it is given.
    let message = serde_norway::from_slice::<de::IgnoredAny>(&bytes[bad.start..])
        .err()
        .map_or_else(
            || "invalid UTF-8".to_owned(),
            |err| without_reader_position(&err.to_string()).to_owned(),
        );
    Error::parse_at(bytes, bad, message)
}

/// `message` without the place of an error of libyaml's reader at its end,
/// ` at position N`.
fn without_reader_position(message: &str) -> &str {
    split_reader_position(message).map_or(message, |(problem, _)| problem)
}

/// `message` split at the byte an error of libyaml's reader names at its
/// end, ` at position N`.
fn split_reader_position(message: &str) -> Option<(&str, usize)> {
    let (problem, offset) = message.rsplit_once(" at position ")?;
    Some((problem, offset.parse().ok()?))
}

fn write_one(v: &Value, w: &mut dyn Write) -> Result<()> {
    // An emitter error is about no source: it has no position.
    let s = super::yaml_emit::to_yaml_string(v).map_err(|e| map_yaml_err(&e, ""))?;
    write_io(w, s.as_bytes())
}

/// `err`, which serde_norway raised on `text`, as an [`Error::Parse`]: at
/// the start of `text` when it has no place (an alias expanded too often),
/// its message without that place. A place the message gives for its
/// context (`while parsing a flow sequence at line 3 column 8`) is another
/// one and stays, with the line and the byte column it has in `text`.
///
/// The place is libyaml's mark, whose index counts bytes; its line and
/// column would not do ([`libyaml_offset`]). An error of libyaml's reader (a
/// control character) leaves the mark at the start and gives the byte in
/// its message alone.
pub(super) fn map_yaml_err(err: &serde_norway::Error, text: &str) -> Error {
    let bytes = text.as_bytes();
    let message = err.to_string();
    let Some(location) = err.location() else {
        return Error::parse_at(bytes, 0..0, message);
    };
    let (line, col) = (location.line(), location.column());
    if (line, col) == (1, 1)
        && let Some((problem, at)) = split_reader_position(&message)
    {
        return Error::parse_at(bytes, at..at, problem);
    }
    let own = format!(" at line {line} column {col}");
    let message = match message.strip_suffix(&own) {
        Some(problem) => problem.to_owned(),
        None => message.replacen(&format!("{own}, "), ", ", 1),
    };
    let (problem, context) = match split_context(&message) {
        Some((problem, place)) => (problem, libyaml_offset(text, place)),
        None => (message.as_str(), None),
    };
    let at = location.index();
    Error::parse_in_context(bytes, at..at, problem, context)
}

/// The byte of `text` at the 1-based line and column of a libyaml mark: a
/// line ends at `\r\n`, `\n`, `\r`, NEL, LS or PS, and a column is a
/// character. `None` when `text` has no such place.
fn libyaml_offset(text: &str, (line, col): (u32, u32)) -> Option<usize> {
    let (mut at_line, mut at_col) = (1, 1);
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        if (at_line, at_col) == (line, col) {
            return Some(at);
        }
        let ends_line = match c {
            '\r' => !matches!(chars.peek(), Some((_, '\n'))),
            '\n' | '\u{85}' | '\u{2028}' | '\u{2029}' => true,
            _ => false,
        };
        if ends_line {
            at_line += 1;
            at_col = 1;
        } else {
            at_col += 1;
        }
    }
    ((at_line, at_col) == (line, col)).then_some(text.len())
}

/// Read every document of `text` into [`Node`]s. `check_keys` fails on a
/// mapping key that may repeat one before it ([`NodeVisitor`]).
pub(super) fn read_nodes(
    text: &str,
    check_keys: bool,
) -> std::result::Result<Vec<Node>, serde_norway::Error> {
    serde_norway::Deserializer::from_str(text)
        .map(|document| NodeSeed { check_keys }.deserialize(document))
        .collect()
}

/// Whether serde_norway failed on a mapping key equal to one before it —
/// as [`NodeVisitor`] tells it, or by its own check.
pub(super) fn is_duplicate_key(err: &serde_norway::Error) -> bool {
    err.to_string().contains("duplicate entry")
}

/// Whether serde_norway refused a `!!float` / `!!int` scalar — maybe a
/// number past the `f64` range or past `u128`, which it has no type for.
pub(super) fn is_tagged_number(err: &serde_norway::Error) -> bool {
    let message = err.to_string();
    message.contains("invalid value: string")
        && (message.contains("expected a float") || message.contains("expected an integer"))
}

/// A YAML node as serde_norway reads it — [`YamlValue`]'s shape, plus the
/// integers past `u64` / `i64` that `YamlValue` has no room for (its
/// visitor fails on them with "invalid type: integer as u128"), with each
/// mapping key stringified as [`Value::Map`] holds it.
#[derive(Debug, Clone)]
pub(super) enum Node {
    Null,
    Bool(bool),
    Number(serde_norway::Number),
    /// An integer in `u128` / `i128` range past `u64` / `i64`, in decimal.
    BigInt(String),
    String(String),
    Sequence(Vec<Node>),
    Mapping(Vec<Entry>),
    Tagged(String, Box<Node>),
}

/// A mapping entry of a [`Node`].
#[derive(Debug, Clone)]
pub(super) struct Entry {
    /// The key as the [`Value::Map`] key.
    pub(super) key: String,
    pub(super) kind: KeyKind,
    /// The key node, `None` for a string key (`key` is the string).
    pub(super) key_node: Option<Node>,
    pub(super) value: Node,
}

/// What a mapping key is, for the message naming it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KeyKind {
    Null,
    Bool,
    String,
    Number,
    /// A collection.
    Other,
}

impl KeyKind {
    /// serde_norway's message for a key equal to an earlier one, `key`
    /// being the earlier one as [`Entry::key`] holds it.
    pub(super) fn duplicate_message(self, key: &str) -> String {
        match self {
            Self::Null => "duplicate entry with null key".to_owned(),
            Self::Bool => format!("duplicate entry with key `{key}`"),
            Self::String => format!("duplicate entry with key {key:?}"),
            Self::Number => format!("duplicate entry with key {key}"),
            Self::Other => "duplicate entry in YAML map".to_owned(),
        }
    }
}

impl Node {
    /// The [`YamlValue`] this node is, `None` when it is or holds a
    /// [`Node::BigInt`].
    pub(super) fn to_yaml_value(&self) -> Option<YamlValue> {
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
                    .map(|entry| {
                        let key = match &entry.key_node {
                            Some(key) => key.to_yaml_value()?,
                            None => YamlValue::String(entry.key.clone()),
                        };
                        Some((key, entry.value.to_yaml_value()?))
                    })
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

    /// The key a [`Value::Map`] holds for this key node, and its kind: a
    /// string as it is, a number as serde_norway prints it, `null` as the
    /// empty key; a collection as its flow text
    /// ([`super::yaml_emit::flow_key_text`]). A tag is dropped, as it is
    /// from a value: `!foo a` is the key `a`.
    fn stringify_key(&self) -> (String, KeyKind) {
        match self {
            Self::String(s) => (s.clone(), KeyKind::String),
            Self::BigInt(s) => (s.clone(), KeyKind::Number),
            Self::Bool(b) => (b.to_string(), KeyKind::Bool),
            Self::Number(n) => (n.to_string(), KeyKind::Number),
            Self::Null => (String::new(), KeyKind::Null),
            Self::Tagged(_, inner) => inner.stringify_key(),
            collection @ (Self::Sequence(_) | Self::Mapping(_)) => (
                super::yaml_emit::flow_key_text(&node_to_value(collection.clone())),
                KeyKind::Other,
            ),
        }
    }
}

/// Deserializes a [`Node`] with [`NodeVisitor`].
#[derive(Clone, Copy)]
struct NodeSeed {
    check_keys: bool,
}

impl<'de> de::DeserializeSeed<'de> for NodeSeed {
    type Value = Node;

    fn deserialize<D: de::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<Node, D::Error> {
        deserializer.deserialize_any(NodeVisitor {
            check_keys: self.check_keys,
        })
    }
}

/// [`YamlValue`]'s visitor, with `visit_u128` / `visit_i128`.
///
/// With `check_keys`, a mapping fails on a key that may repeat an earlier
/// one: the same value, or the same [`Value::Map`] key after
/// stringification (`1` and `"1"`, `~` and `""`). The check compares
/// hashes, so it may fail on two keys that differ; the exact check —
/// [`super::yaml_numbers::read_exact`] — decides, and names the key where
/// it repeats.
struct NodeVisitor {
    check_keys: bool,
}

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
        NodeSeed {
            check_keys: self.check_keys,
        }
        .deserialize(deserializer)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut data: A) -> std::result::Result<Node, A::Error> {
        let seed = NodeSeed {
            check_keys: self.check_keys,
        };
        let mut items = Vec::new();
        while let Some(item) = data.next_element_seed(seed)? {
            items.push(item);
        }
        Ok(Node::Sequence(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut data: A) -> std::result::Result<Node, A::Error> {
        let seed = NodeSeed {
            check_keys: self.check_keys,
        };
        let mut entries = Vec::new();
        let hasher = std::collections::hash_map::RandomState::new();
        let mut seen: HashSet<u64> = HashSet::new();
        while let Some(key) = data.next_key_seed(seed)? {
            let (text, kind) = key.stringify_key();
            if self.check_keys {
                let mut repeats = !seen.insert(hasher.hash_one(("text", &text)));
                // Equal values that print apart: `-0.0` and `0.0`, and
                // collections or tagged keys holding them, or maps in
                // another order.
                if let Node::Number(n) = &key
                    && let Some(f) = n.as_f64()
                    && n.is_f64()
                {
                    let bits = if f == 0.0 { 0 } else { f.to_bits() };
                    repeats |= !seen.insert(hasher.hash_one(("float", bits)));
                } else if matches!(key, Node::Tagged(..) | Node::Sequence(_) | Node::Mapping(_))
                    && let Some(plain) = key.to_yaml_value()
                {
                    repeats |= !seen.insert(hasher.hash_one(("value", plain)));
                }
                if repeats {
                    return Err(de::Error::custom(kind.duplicate_message(&text)));
                }
            }
            let value = data.next_value_seed(seed)?;
            let key_node = (kind != KeyKind::String).then_some(key);
            entries.push(Entry {
                key: text,
                kind,
                key_node,
                value,
            });
        }
        Ok(Node::Mapping(entries))
    }

    fn visit_enum<A: EnumAccess<'de>>(self, data: A) -> std::result::Result<Node, A::Error> {
        let (tag, contents) = data.variant::<String>()?;
        if tag.is_empty() {
            return Err(de::Error::custom("empty YAML tag is not allowed"));
        }
        let value = contents.newtype_variant_seed(NodeSeed {
            check_keys: self.check_keys,
        })?;
        Ok(Node::Tagged(tag, Box::new(value)))
    }
}

/// The [`Value`] of a node serde_norway read: numbers as
/// [`number_value`] makes them, tags dropped.
pub(super) fn node_to_value(node: Node) -> Value {
    match node {
        Node::Null => Value::Null,
        Node::Bool(b) => Value::Bool(b),
        Node::Number(n) => number_value(&n),
        Node::BigInt(s) => Value::BigInt(s),
        Node::String(s) => Value::String(s),
        Node::Sequence(items) => Value::Array(items.into_iter().map(node_to_value).collect()),
        Node::Mapping(entries) => {
            // The keys are apart: `read_nodes` checked them.
            let mut out = IndexMap::with_capacity(entries.len());
            for entry in entries {
                out.insert(entry.key, node_to_value(entry.value));
            }
            Value::Map(out)
        }
        // Tagged scalars (`!!binary`, custom tags) are flattened to their
        // underlying value in M1. Preserving the tag verbatim is a M2
        // round-trip concern.
        Node::Tagged(_, value) => node_to_value(*value),
    }
}

/// The [`Value`] of a number serde_norway read: an `f64` it parsed a
/// literal to stays that `f64` here — [`super::yaml_numbers`] reads the
/// literals no `f64` holds exactly. A NaN or an infinity keeps
/// serde_norway's spelling (`.nan`, `-.inf`) as a [`Value::BigFloat`].
fn number_value(n: &serde_norway::Number) -> Value {
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
    fn parse_fails_on_a_duplicate_key_at_the_repeated_key() {
        // Worded as serde_norway words it, naming the first key, at the
        // second one.
        for (text, message, line, col) in [
            ("a: 1\na: 2\n", "duplicate entry with key \"a\"", 2, 1),
            ("1: 1\n1: 2\n", "duplicate entry with key 1", 2, 1),
            (
                "18446744073709551616: 1\n18446744073709551616: 2\n",
                "duplicate entry with key 18446744073709551616",
                2,
                1,
            ),
            (
                "18446744073709551616: 1\n'18446744073709551616': 2\n",
                "duplicate entry with key 18446744073709551616",
                2,
                1,
            ),
            (
                "x:\n  - {~: 1, null: 2}\n",
                "x[0]: duplicate entry with null key",
                2,
                12,
            ),
            ("[1]: 1\n[1]: 2\n", "duplicate entry in YAML map", 2, 1),
            (
                "y: {true: 1, 'true': 2}\n",
                "y: duplicate entry with key `true`",
                1,
                14,
            ),
        ] {
            let err = Yaml.parse(text.as_bytes()).unwrap_err();
            let Error::Parse {
                message: got,
                line: got_line,
                col: got_col,
                ..
            } = err
            else {
                panic!("{err:?}")
            };
            assert_eq!(
                (got.as_str(), got_line, got_col),
                (message, line, col),
                "{text}"
            );
        }
        // Any other error is serde_norway's, word for word.
        let text = "a: [1, !x {}]\nb: !!int x\n";
        let ours = Yaml
            .parse(text.as_bytes())
            .map(|_| ())
            .map_err(|e| e.to_string());
        let theirs = serde_norway::from_str::<YamlValue>(text)
            .map(|_| ())
            .map_err(|e| map_yaml_err(&e, text).to_string());
        assert_eq!(ours, theirs);
    }

    /// `(line, col, span, snippet, message)` of the error `bytes` fail with.
    fn parse_error(bytes: &[u8]) -> (u32, u32, std::ops::Range<usize>, String, String) {
        match Yaml.parse(bytes).expect_err("must fail") {
            Error::Parse {
                file: None,
                line,
                col,
                span,
                snippet,
                message,
            } => (line, col, span, snippet, message),
            other => panic!("expected a parse error, got {other:?}"),
        }
    }

    #[test]
    fn a_parse_error_is_at_the_byte_libyaml_marks() {
        let token =
            "found character that cannot start any token, while scanning for the next token";
        for (text, line, col, span, snippet, message) in [
            // Columns in bytes; later documents; a byte order mark, CRLF.
            ("я: @x\n", 1, 5, 4..4, "я: @x", token),
            ("a: 1\n---\nя: @x\n", 3, 5, 13..13, "я: @x", token),
            ("\u{feff}a: 1\r\nb: @x\r\n", 2, 4, 12..12, "b: @x", token),
            ("a: 1\rb: @x\r", 2, 4, 8..8, "b: @x", token),
            // A line break to libyaml alone.
            ("a: \"x\u{2028}y\"\nb: @x\n", 2, 4, 14..14, "b: @x", token),
            // A context position stays; the end of the input.
            (
                "a:\n  b: [1",
                2,
                8,
                10..10,
                "  b: [1",
                "did not find expected ',' or ']', while parsing a flow sequence at line 2 column 6",
            ),
            // libyaml's reader gives the byte in its message alone.
            (
                "я: 1\nb: \u{1}\n",
                2,
                4,
                9..9,
                "b: \u{1}",
                "control characters are not allowed",
            ),
            // The key of a second entry, without the blank after it.
            (
                "я: {\"ю\": 1, \"ю\" : 2}\n",
                1,
                15,
                14..18,
                "я: {\"ю\": 1, \"ю\" : 2}",
                "я: duplicate entry with key \"ю\"",
            ),
            // Read with the tag rewritten: the file's own line is shown.
            (
                "%TAG !! tag:x.org,2000:\n---\nя: !!str 1\nb: @x\n",
                4,
                4,
                43..43,
                "b: @x",
                token,
            ),
        ] {
            assert_eq!(
                parse_error(text.as_bytes()),
                (line, col, span, snippet.to_owned(), message.to_owned()),
                "{text:?}"
            );
        }
        assert_eq!(
            parse_error(b"a: 1\nb: x\xFF\n"),
            (
                2,
                5,
                9..10,
                "b: x\u{fffd}".to_owned(),
                "invalid leading UTF-8 octet".to_owned()
            )
        );
    }

    #[test]
    fn libyaml_offset_counts_lines_and_columns_as_libyaml_does() {
        let text = "я1\r\nb\rc\u{85}d\u{2028}e\u{2029}f";
        for (place, at) in [
            ((1, 1), Some(0)),
            ((1, 2), Some(2)),
            ((2, 1), text.find('b')),
            ((3, 1), text.find('c')),
            ((4, 1), text.find('d')),
            ((5, 1), text.find('e')),
            ((6, 1), text.find('f')),
            ((6, 2), Some(text.len())),
            ((6, 3), None),
            ((7, 1), None),
            ((3, 9), None),
        ] {
            assert_eq!(libyaml_offset(text, place), at, "{place:?}");
        }
    }

    #[test]
    fn a_place_in_bytes_that_are_no_utf8_is_the_first_of_them() {
        // libyaml reads the second key first.
        let mut bytes = b"a: 1\n".repeat(5000);
        bytes.extend(b"b: \xE9 x\n");
        assert_eq!(
            parse_error(&bytes),
            (
                5001,
                4,
                25_003..25_004,
                "b: \u{fffd} x".to_owned(),
                "invalid trailing UTF-8 octet".to_owned()
            )
        );
    }

    #[test]
    fn parse_tells_keys_apart_that_hash_alike_only() {
        // `0.1` and `"0.1"` print alike; `0.10` is the same `f64`.
        assert!(Yaml.parse(b"0.1: a\n'0.1': b\n").is_err());
        assert!(Yaml.parse(b"0.1: a\n0.10: b\n").is_err());
        let doc = parse("a: 1\nb: 2\n'1': 3\n2.5: 4\n");
        let Value::Map(m) = doc.value() else { panic!() };
        assert_eq!(m.len(), 4);
    }

    #[test]
    fn parse_keeps_tags_flattened_and_composite_keys_as_text() {
        let doc = parse("a: !foo 18446744073709551616\n? [1, 2]\n: b\n");
        let Value::Map(m) = doc.value() else { panic!() };
        assert_eq!(m["a"], Value::BigInt("18446744073709551616".into()));
        let keys: Vec<&str> = m.keys().map(String::as_str).collect();
        assert_eq!(keys[1], "[1, 2]");
    }

    #[test]
    fn a_collection_key_writes_back_as_the_same_string_key() {
        let src = "[1, 2]: a\n? {k: [on, 'a: b']}\n: b\n!foo t: c\n";
        let doc = parse(src);
        let Value::Map(m) = doc.value() else { panic!() };
        let keys: Vec<&str> = m.keys().map(String::as_str).collect();
        assert_eq!(keys, ["[1, 2]", "{k: ['on', 'a: b']}", "t"]);
        let mut out = Vec::new();
        Yaml.write(&doc, &mut out).unwrap();
        let written = String::from_utf8(out).unwrap();
        assert!(written.starts_with("'[1, 2]': a\n"), "{written}");
        assert_eq!(parse(&written).value(), doc.value(), "{written}");
        let spans = crate::parsers::yaml_spans::parse_yaml_with_spans(src.as_bytes()).unwrap();
        assert_eq!(spans.value(), doc.value());
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
