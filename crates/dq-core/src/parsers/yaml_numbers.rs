//! The exact numbers serde_norway's YAML reader loses, put back.
//!
//! serde_norway types a plain scalar with Rust's number parsers and hands
//! the visitor no literal: a float literal with more digits than an `f64`
//! holds (`0.1000000000000000000000000001`), or an integer past `u128`,
//! arrives as the nearest `f64`; a float past the `f64` range (`1e400`,
//! where the parse is infinite) arrives as the string `"1e400"`, the same
//! as a quoted `'1e400'`. Neither is the number the file holds.
//!
//! [`restore_exact_numbers`] walks the same source through saphyr-parser's
//! events, which carry each scalar's literal, style and tag, and replaces
//! such a value of the tree with the [`Value::BigInt`] / [`Value::BigFloat`]
//! the literal spells ([`Value::from_number_literal`]); a mapping key is
//! given its literal back. The two trees are walked side by side — the
//! n-th entry of a mapping or a sequence in the events is the n-th in the
//! tree, and an alias repeats its anchor's node, as serde_norway copies it.
//! A value is replaced only where the tree holds what serde_norway makes
//! of that literal (the same `f64` bits, or the literal as a string), and a
//! collection whose length differs from the events' is left alone, so
//! wherever the two parsers disagree, the tree stays as serde_norway read
//! it.
//!
//! The walk is a second parse, so [`may_lose_numbers`] first looks for a
//! literal that can need it: 16 digits in a row (an `f64` holds every
//! 15-digit decimal), or an exponent of three digits.

use std::collections::{HashMap, HashSet};

use saphyr_parser::{Event, Parser as YamlParser, ScalarStyle, Tag};

use crate::document::Value;

/// Whether `bytes` hold a literal serde_norway may read as a number other
/// than the one it spells: 16 or more digits in one run of digits and
/// `.`, or an exponent (`e` / `E` after a digit or `.`) of three or more
/// digits. `false` means [`restore_exact_numbers`] has nothing to do.
pub(super) fn may_lose_numbers(bytes: &[u8]) -> bool {
    let mut digits = 0usize;
    let mut in_number = false;
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'0'..=b'9' => {
                digits += 1;
                in_number = true;
                if digits >= 16 {
                    return true;
                }
            }
            b'.' => in_number = true,
            b'e' | b'E' if in_number => {
                let mut end = at + 1;
                if matches!(bytes.get(end), Some(b'+' | b'-')) {
                    end += 1;
                }
                let start = end;
                while bytes.get(end).is_some_and(u8::is_ascii_digit) {
                    end += 1;
                }
                if end - start >= 3 {
                    return true;
                }
                digits = 0;
                in_number = false;
                at = end;
                continue;
            }
            _ => {
                digits = 0;
                in_number = false;
            }
        }
        at += 1;
    }
    false
}

/// Replace, in `documents` (serde_norway's reading of `bytes`, one value
/// per document), every number serde_norway rounded or read as a string
/// with the exact one its literal spells. Does nothing when saphyr-parser
/// fails on `bytes` or finds another number of documents.
pub(super) fn restore_exact_numbers(bytes: &[u8], documents: &mut [Value]) {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return;
    };
    let Some(shadows) = shadow_documents(text) else {
        tracing::debug!("saphyr-parser disagrees with serde_norway; numbers stay as read");
        return;
    };
    if shadows.len() != documents.len() {
        return;
    }
    for (shadow, document) in shadows.iter().zip(documents) {
        shadow.apply(document);
    }
}

/// A number the reader loses: the literal as written and its exact value.
#[derive(Debug, Clone)]
struct Exact {
    literal: String,
    value: Value,
}

impl Exact {
    /// Whether `tree` is what serde_norway reads this literal as: the
    /// nearest `f64`, or the literal itself as a string.
    fn read_as(&self, tree: &Value) -> bool {
        match tree {
            Value::Float(f) => self
                .literal
                .parse::<f64>()
                .is_ok_and(|g| g.to_bits() == f.to_bits()),
            Value::String(s) => *s == self.literal,
            _ => false,
        }
    }

    /// Whether `key` is what serde_norway makes of this literal as a
    /// mapping key: the nearest `f64` as serde_norway prints it, or the
    /// literal itself.
    fn read_as_key(&self, key: &str) -> bool {
        key == self.literal
            || self
                .literal
                .parse::<f64>()
                .is_ok_and(|f| f.is_finite() && serde_norway::Number::from(f).to_string() == key)
    }
}

/// The structure of one node as the events give it, with the numbers to
/// restore.
#[derive(Debug, Clone)]
enum Shadow {
    /// A scalar, and the number to restore when its literal needs one.
    Scalar(Option<Exact>),
    Sequence(Vec<Shadow>),
    /// Each entry's key, when it is a scalar whose literal needs restoring,
    /// and value.
    Mapping(Vec<(Option<Exact>, Shadow)>),
    /// An alias of an anchor the walk does not know.
    Unknown,
}

impl Shadow {
    /// Put the restored numbers into `tree`, the same node as serde_norway
    /// read it.
    fn apply(&self, tree: &mut Value) {
        match (self, tree) {
            (Self::Scalar(Some(exact)), tree) => {
                if exact.read_as(tree) {
                    *tree = exact.value.clone();
                }
            }
            (Self::Sequence(items), Value::Array(values)) if items.len() == values.len() => {
                for (item, value) in items.iter().zip(values) {
                    item.apply(value);
                }
            }
            (Self::Mapping(entries), Value::Map(map)) if entries.len() == map.len() => {
                for ((_, shadow), value) in entries.iter().zip(map.values_mut()) {
                    shadow.apply(value);
                }
                restore_keys(entries, map);
            }
            _ => {}
        }
    }
}

/// Give back the literal of every key serde_norway rounded to an `f64`
/// (`0.1000000000000000000000000001:` became the key `0.1`) — unless that
/// would make two keys equal.
fn restore_keys(entries: &[(Option<Exact>, Shadow)], map: &mut indexmap::IndexMap<String, Value>) {
    let renamed: Vec<Option<String>> = entries
        .iter()
        .zip(map.keys())
        .map(|((exact, _), key)| {
            exact
                .as_ref()
                .filter(|exact| *key != exact.literal && exact.read_as_key(key))
                .map(|exact| exact.literal.clone())
        })
        .collect();
    if renamed.iter().all(Option::is_none) {
        return;
    }
    let keys: Vec<&str> = renamed
        .iter()
        .zip(map.keys())
        .map(|(new, old)| new.as_deref().unwrap_or(old))
        .collect();
    let unique: HashSet<&str> = keys.iter().copied().collect();
    if unique.len() != keys.len() {
        return;
    }
    let old = std::mem::take(map);
    for ((key, value), new) in old.into_iter().zip(renamed) {
        map.insert(new.unwrap_or(key), value);
    }
}

/// One [`Shadow`] per document of `text`; `None` when saphyr-parser fails
/// on it or its events have a shape this walk does not expect.
fn shadow_documents(text: &str) -> Option<Vec<Shadow>> {
    let mut events = Vec::new();
    let mut parser = YamlParser::new_from_str(text);
    while let Some(item) = parser.next_event() {
        let (event, _) = item.ok()?;
        events.push(event);
    }
    let mut walk = Walk {
        events: events.into_iter(),
        anchors: HashMap::new(),
    };
    let mut documents = Vec::new();
    loop {
        match walk.events.next()? {
            Event::StreamStart | Event::Nothing => {}
            Event::StreamEnd => return Some(documents),
            Event::DocumentStart(_) => {
                let first = walk.events.next()?;
                documents.push(walk.node(first)?);
                if !matches!(walk.events.next()?, Event::DocumentEnd) {
                    return None;
                }
            }
            _ => return None,
        }
    }
}

/// The events left to walk and the nodes of the anchors seen so far.
struct Walk<'input> {
    events: std::vec::IntoIter<Event<'input>>,
    anchors: HashMap<usize, Shadow>,
}

impl Walk<'_> {
    /// The node that starts with `event`, the events up to its end consumed.
    fn node(&mut self, event: Event<'_>) -> Option<Shadow> {
        let (shadow, anchor) = match event {
            Event::Scalar(literal, style, anchor, tag) => (
                Shadow::Scalar(exact_number(&literal, style, tag.as_deref())),
                anchor,
            ),
            Event::Alias(id) => {
                return Some(self.anchors.get(&id).cloned().unwrap_or(Shadow::Unknown));
            }
            Event::SequenceStart(anchor, _) => {
                let mut items = Vec::new();
                loop {
                    match self.events.next()? {
                        Event::SequenceEnd => break,
                        event => items.push(self.node(event)?),
                    }
                }
                (Shadow::Sequence(items), anchor)
            }
            Event::MappingStart(anchor, _) => {
                let mut entries = Vec::new();
                loop {
                    let key = match self.events.next()? {
                        Event::MappingEnd => break,
                        event => self.node(event)?,
                    };
                    let key = match key {
                        Shadow::Scalar(exact) => exact,
                        _ => None,
                    };
                    let value = self.events.next()?;
                    entries.push((key, self.node(value)?));
                }
                (Shadow::Mapping(entries), anchor)
            }
            _ => return None,
        };
        // saphyr-parser numbers anchors from 1; 0 is a node without one.
        if anchor != 0 {
            self.anchors.insert(anchor, shadow.clone());
        }
        Some(shadow)
    }
}

/// The exact number a scalar spells when serde_norway reads it as another
/// value: a plain scalar, untagged or under a local tag (`!foo`, which
/// serde_norway types as untagged), or any scalar tagged `!!float`.
fn exact_number(literal: &str, style: ScalarStyle, tag: Option<&Tag>) -> Option<Exact> {
    let float_only = match tag {
        None => false,
        Some(tag) if tag.handle == "!" && !tag.suffix.is_empty() => false,
        Some(tag) if tag.is_yaml_core_schema() && tag.suffix == "float" => true,
        Some(_) => return None,
    };
    if !float_only && style != ScalarStyle::Plain {
        return None;
    }
    // serde_norway reads digits with a leading zero (`0123`) as a string.
    let unsigned = literal.strip_prefix(['-', '+']).unwrap_or(literal);
    if unsigned.len() > 1
        && unsigned.starts_with('0')
        && unsigned.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let value = Value::from_number_literal(literal)?;
    let lost = match value {
        Value::BigFloat(_) => true,
        Value::BigInt(_) => !float_only,
        _ => false,
    };
    lost.then(|| Exact {
        literal: literal.to_owned(),
        value,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::Format as _;

    fn read(yaml: &str) -> Value {
        crate::parsers::Yaml
            .parse(yaml.as_bytes())
            .unwrap()
            .value()
            .clone()
    }

    fn map(entries: &[(&str, Value)]) -> Value {
        Value::Map(
            entries
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
        )
    }

    #[test]
    fn may_lose_numbers_wants_16_digits_or_a_3_digit_exponent() {
        for yes in [
            "a: 1234567890123456\n",
            "a: 1e400\n",
            "a: 0.5E-308\n",
            "a: .1e+300",
        ] {
            assert!(may_lose_numbers(yes.as_bytes()), "{yes}");
        }
        for no in [
            "a: 123456789012345\n",
            "a: 1.5e99\n",
            "rule400: x\n",
            "a: 1.2.3\n",
        ] {
            assert!(!may_lose_numbers(no.as_bytes()), "{no}");
        }
    }

    #[test]
    fn restores_long_floats_huge_integers_and_floats_past_f64() {
        let big = "123456789012345678901234567890123456789012";
        let text = format!("a: 0.1000000000000000000000000001\nb: {big}\nc: 1e400\nd: -1E-400\n");
        assert_eq!(
            read(&text),
            map(&[
                (
                    "a",
                    Value::BigFloat("0.1000000000000000000000000001".into())
                ),
                ("b", Value::BigInt(big.into())),
                ("c", Value::BigFloat("1e400".into())),
                ("d", Value::BigFloat("-1E-400".into())),
            ])
        );
    }

    #[test]
    fn keeps_quoted_and_str_tagged_literals_strings() {
        let text = "a: '1e400'\nb: \"0.1000000000000000000000000001\"\nc: !!str 1e400\n";
        assert_eq!(
            read(text),
            map(&[
                ("a", Value::String("1e400".into())),
                ("b", Value::String("0.1000000000000000000000000001".into())),
                ("c", Value::String("1e400".into())),
            ])
        );
    }

    #[test]
    fn restores_through_aliases_local_tags_and_float_tags() {
        let text =
            "a: &x [1e400]\nb: *x\nc: !num 1e400\nd: !!float \"0.10000000000000000000001\"\n";
        let one = Value::Array(vec![Value::BigFloat("1e400".into())]);
        assert_eq!(
            read(text),
            map(&[
                ("a", one.clone()),
                ("b", one),
                ("c", Value::BigFloat("1e400".into())),
                ("d", Value::BigFloat("0.10000000000000000000001".into())),
            ])
        );
    }

    #[test]
    fn gives_a_rounded_key_its_literal_back() {
        let text = "0.1000000000000000000000000001: a\n1e400: b\n";
        let Value::Map(m) = read(text) else { panic!() };
        let keys: Vec<&str> = m.keys().map(String::as_str).collect();
        assert_eq!(keys, ["0.1000000000000000000000000001", "1e400"]);
    }

    #[test]
    fn restores_each_document_of_a_stream() {
        let doc = crate::parsers::Yaml
            .parse(b"---\na: 1e400\n---\n- 18446744073709551616\n- 0.12345678901234567890\n")
            .unwrap();
        assert_eq!(
            doc.values().unwrap(),
            [
                map(&[("a", Value::BigFloat("1e400".into()))]),
                Value::Array(vec![
                    Value::BigInt("18446744073709551616".into()),
                    Value::BigFloat("0.12345678901234567890".into()),
                ]),
            ]
        );
    }

    #[test]
    fn leaves_leading_zero_digits_and_exact_floats_alone() {
        let text = "a: 01234567890123456789\nb: 0.30000000000000004\nc: 1.5e300\n";
        assert_eq!(
            read(text),
            map(&[
                ("a", Value::String("01234567890123456789".into())),
                ("b", Value::Float(0.30000000000000004)),
                ("c", Value::Float(1.5e300)),
            ])
        );
    }
}
