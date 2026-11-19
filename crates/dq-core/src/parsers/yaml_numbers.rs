//! The YAML reader for numbers serde_norway gets wrong.
//!
//! serde_norway types a plain scalar with Rust's number parsers and hands
//! the visitor no literal: a float literal no `f64` holds
//! ([`Value::from_number_literal`]: `0.1000000000000000000000000001`, or
//! an integer past `u128`) arrives as the nearest `f64`; a float past the
//! `f64` range (`1e400`, where the parse is infinite) as the string
//! `"1e400"`, the same as a quoted `'1e400'`; and a `!!float 1e400` or a
//! `!!int` past `u128` fails the whole file. A mapping key rounded so is
//! rounded in the key too, so two keys that differ can look equal.
//!
//! [`read_exact`] reads such a file twice, side by side: serde_norway's
//! tree ([`Node`]) and saphyr-parser's events, which carry each scalar's
//! literal, style, tag and position. The n-th entry of a mapping or a
//! sequence in the events is the n-th in the tree; an alias repeats the
//! value its anchor got. A scalar whose literal serde_norway read as
//! another value gets the exact one, and the keys of each mapping are
//! checked against each other there, with their exact values, so a repeat
//! is named where it occurs. Wherever the two parsers disagree on the
//! shape, the file is read as serde_norway reads it.
//!
//! The events are walked as they come (only anchored values are kept), and
//! only when [`may_lose_numbers`] finds a literal that can need it.

use std::collections::HashMap;
use std::fmt;

use indexmap::IndexMap;
use saphyr_parser::{Event, Parser as YamlParser, ScalarStyle, Span, StrInput, Tag};

use super::yaml::{
    Entry, KeyKind, Node, is_tagged_number, map_yaml_err, node_to_value, read_nodes,
};
use crate::Result;
use crate::document::{DecimalParts, Value, decimal_identity, float_literal_value, may_exceed_f64};
use crate::error::Error;

/// Whether `bytes` hold a literal serde_norway may read as another number
/// than it spells: a decimal literal [`may_exceed_f64`] flags (an integer
/// of 39 digits or more, a float literal no `f64` holds), or, when the
/// text holds the word `float` (a `!!float` tag), an integer of 16 digits
/// or more. `false` means serde_norway reads every number right.
///
/// Every run of digits, `.`, `e`, `E`, `+` and `-` is looked at, wherever
/// it is: a plain number scalar is always such a whole run.
pub(super) fn may_lose_numbers(bytes: &[u8]) -> bool {
    let float_tag = bytes.windows(5).any(|w| w == b"float");
    let in_run = |b: u8| matches!(b, b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-');
    let mut at = 0;
    while at < bytes.len() {
        if !(bytes[at].is_ascii_digit() || bytes[at] == b'.') {
            at += 1;
            continue;
        }
        let start = if at > 0 && matches!(bytes[at - 1], b'-' | b'+') {
            at - 1
        } else {
            at
        };
        let mut end = at;
        while end < bytes.len() && in_run(bytes[end]) {
            end += 1;
        }
        // ASCII only.
        let run = std::str::from_utf8(&bytes[start..end]).unwrap_or("");
        if may_exceed_f64(run)
            || (float_tag
                && DecimalParts::parse(run).is_some_and(|parts| parts.is_integer())
                && run
                    .trim_start_matches(['-', '+'])
                    .trim_start_matches('0')
                    .len()
                    >= 16)
        {
            return true;
        }
        at = end;
    }
    false
}

/// Every document of `bytes` with its exact numbers (see the module docs).
///
/// # Errors
///
/// [`Error::Parse`] when serde_norway fails on `bytes` (a `!!float` /
/// `!!int` number it has no type for aside), or for a mapping key equal to
/// an earlier one — by value or as a [`Value::Map`] key (`1` and `"1"`) —
/// at the repeated key's line and column.
pub(super) fn read_exact(bytes: &[u8]) -> Result<Vec<Value>> {
    let as_serde_norway_reads = || {
        read_nodes(bytes, true)
            .map(|nodes| nodes.into_iter().map(node_to_value).collect())
            .map_err(map_yaml_err)
    };
    let Ok(text) = std::str::from_utf8(bytes) else {
        return as_serde_norway_reads();
    };
    let (nodes, refused) = match read_nodes(bytes, false) {
        Ok(nodes) => (nodes, None),
        Err(err) if is_tagged_number(&err) => {
            let refused = map_yaml_err(err);
            let Some(untagged) = untag_numbers(text) else {
                return Err(refused);
            };
            match read_nodes(untagged.as_bytes(), false) {
                Ok(nodes) => (nodes, Some(refused)),
                // Every position is kept: the error is the file's.
                Err(err) => return Err(map_yaml_err(err)),
            }
        }
        Err(err) => return Err(map_yaml_err(err)),
    };
    let walk = Walk {
        events: YamlParser::new_from_str(text),
        anchors: HashMap::new(),
    };
    match walk.documents(nodes) {
        Ok(values) => Ok(values),
        Err(Stop::Repeated(err)) => Err(err),
        Err(Stop::Disagree) => {
            tracing::debug!("saphyr-parser disagrees with serde_norway; numbers stay as read");
            match refused {
                Some(err) => Err(err),
                None => as_serde_norway_reads(),
            }
        }
    }
}

/// `text` with the tag of every `!!float` / `!!int` scalar serde_norway
/// refuses and [`exact_value`] reads (`!!float 1e400`, a `!!int` past
/// `u128`) blanked out — spaces, so every position stays — for
/// serde_norway to read it untagged; the events of `text` give the value.
/// `None` when there is none, or saphyr-parser fails on `text`.
fn untag_numbers(text: &str) -> Option<String> {
    let mut starts = Vec::new();
    let mut events = YamlParser::new_from_str(text);
    while let Some(item) = events.next_event() {
        let (event, span) = item.ok()?;
        if let Event::Scalar(literal, _, _, Some(tag)) = &event
            && let Some(typing) = core_typing(tag)
            && serde_norway_refuses(literal, typing)
        {
            starts.push(span.start.index());
        }
    }
    if starts.is_empty() {
        return None;
    }
    // Char indices to byte offsets, in one pass: the events come in order.
    let mut offsets = Vec::with_capacity(starts.len());
    let mut wanted = starts.iter().peekable();
    for (char_at, (byte_at, _)) in text.char_indices().enumerate() {
        while wanted.next_if(|&&at| at == char_at).is_some() {
            offsets.push(byte_at);
        }
    }
    let mut out = text.as_bytes().to_vec();
    for at in offsets {
        let tag = tag_range(&out, at)?;
        out[tag].fill(b' ');
    }
    String::from_utf8(out).ok()
}

/// The bytes of the tag among the node properties (`&anchor`, `!tag`) that
/// start at `at`.
fn tag_range(bytes: &[u8], mut at: usize) -> Option<std::ops::Range<usize>> {
    loop {
        while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        let end_of = |from: usize, stop: &dyn Fn(u8) -> bool| {
            (from..bytes.len())
                .find(|&i| stop(bytes[i]))
                .unwrap_or(bytes.len())
        };
        match bytes.get(at)? {
            b'&' => at = end_of(at, &|b: u8| b.is_ascii_whitespace()),
            b'!' if bytes.get(at + 1) == Some(&b'<') => {
                return Some(at..end_of(at, &|b: u8| b == b'>') + 1);
            }
            b'!' => {
                let end = end_of(at, &|b: u8| {
                    b.is_ascii_whitespace() || matches!(b, b',' | b'[' | b']' | b'{' | b'}')
                });
                return Some(at..end);
            }
            _ => return None,
        }
    }
}

/// How a scalar is typed by its tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Typing {
    /// Untagged, or a local tag (`!foo`), which serde_norway types as
    /// untagged: only a plain scalar is a number.
    Plain,
    /// `!!float`: any scalar, integers too, is a float.
    Float,
    /// `!!int`: any scalar.
    Int,
}

/// The typing a core-schema tag gives, `None` for a local tag or another one.
fn core_typing(tag: &Tag) -> Option<Typing> {
    const CORE: &str = "tag:yaml.org,2002:";
    let suffix = match tag.handle.as_str() {
        CORE => tag.suffix.as_str(),
        "" => tag.suffix.strip_prefix(CORE)?,
        _ => return None,
    };
    match suffix {
        "float" => Some(Typing::Float),
        "int" => Some(Typing::Int),
        _ => None,
    }
}

/// The typing of a scalar's tag, `None` when the tag makes it no number.
fn typing(tag: Option<&Tag>) -> Option<Typing> {
    let Some(tag) = tag else {
        return Some(Typing::Plain);
    };
    // serde_norway resolves a tag starting `!` — `!foo`, a bare `!`, a
    // `%TAG` handle naming a local prefix — to a local one.
    let local =
        tag.handle.starts_with('!') || (tag.handle.is_empty() && tag.suffix.starts_with('!'));
    if local {
        Some(Typing::Plain)
    } else {
        core_typing(tag)
    }
}

/// Whether serde_norway fails on a `!!float` / `!!int` `literal` that
/// [`exact_value`] reads: a float past the `f64` range, an integer past
/// `u128` / `i128`.
fn serde_norway_refuses(literal: &str, typing: Typing) -> bool {
    if leading_zero_digits(literal) {
        return false;
    }
    match typing {
        Typing::Float => {
            let unsigned = literal.strip_prefix('+').unwrap_or(literal);
            !unsigned.parse::<f64>().is_ok_and(f64::is_finite)
                && float_literal_value(literal).is_some()
        }
        Typing::Int => {
            let unsigned = literal.strip_prefix('+').unwrap_or(literal);
            DecimalParts::parse(literal).is_some_and(|parts| parts.is_integer())
                && unsigned.parse::<u128>().is_err()
                && literal.parse::<i128>().is_err()
        }
        Typing::Plain => false,
    }
}

/// Digits with a leading zero (`0123`, `-007`): serde_norway reads them as
/// a string, as YAML 1.2 says.
fn leading_zero_digits(literal: &str) -> bool {
    let unsigned = literal.strip_prefix(['-', '+']).unwrap_or(literal);
    unsigned.len() > 1 && unsigned.starts_with('0') && unsigned.bytes().all(|b| b.is_ascii_digit())
}

/// What serde_norway made of a scalar, as far as a number literal goes.
enum SerdeRead<'a> {
    Float(f64),
    Str(&'a str),
    Other,
}

impl<'a> SerdeRead<'a> {
    fn of(node: &'a Node) -> Self {
        match node {
            Node::Number(n) if n.is_f64() => n.as_f64().map_or(Self::Other, Self::Float),
            Node::String(s) => Self::Str(s),
            Node::Tagged(_, inner) => Self::of(inner),
            _ => Self::Other,
        }
    }
}

/// The exact number a scalar is when serde_norway read it as another
/// value: a [`Value::BigInt`] or [`Value::BigFloat`] where `read` is the
/// nearest `f64` or the literal as a string. `None` for every other scalar.
fn exact_value(
    literal: &str,
    style: ScalarStyle,
    tag: Option<&Tag>,
    read: &SerdeRead<'_>,
) -> Option<Value> {
    let typing = typing(tag)?;
    if (typing == Typing::Plain && style != ScalarStyle::Plain) || leading_zero_digits(literal) {
        return None;
    }
    let value = match typing {
        Typing::Float => float_literal_value(literal)?,
        Typing::Int | Typing::Plain => Value::from_number_literal(literal)?,
    };
    match (&value, typing) {
        (Value::BigFloat(_), Typing::Plain | Typing::Float) | (Value::BigInt(_), _) => {}
        _ => return None,
    }
    let read_as = match read {
        SerdeRead::Float(f) => literal
            .strip_prefix('+')
            .unwrap_or(literal)
            .parse::<f64>()
            .is_ok_and(|g| g.to_bits() == f.to_bits()),
        SerdeRead::Str(s) => *s == literal,
        SerdeRead::Other => false,
    };
    read_as.then_some(value)
}

/// Why the walk stopped.
enum Stop {
    /// The events and serde_norway's tree differ in shape.
    Disagree,
    /// A mapping key repeats an earlier one.
    Repeated(Error),
}

type Step<T> = std::result::Result<T, Stop>;

/// A number key's identity: equal for keys of the same value that print
/// apart (`-0.0` and `0.0`, `1e400` and `1.0e+400`).
#[derive(Debug, PartialEq, Eq, Hash)]
enum NumberId {
    Float(u64),
    Decimal { float: bool, value: String },
}

impl NumberId {
    fn of_value(value: &Value) -> Option<Self> {
        match value {
            Value::Float(f) => Some(Self::Float(if *f == 0.0 { 0 } else { f.to_bits() })),
            Value::BigFloat(s) => {
                decimal_identity(s).map(|value| Self::Decimal { float: true, value })
            }
            Value::BigInt(s) => decimal_identity(s).map(|value| Self::Decimal {
                float: false,
                value,
            }),
            _ => None,
        }
    }

    fn of_node(node: &Node) -> Option<Self> {
        match node {
            Node::Number(n) if n.is_f64() => {
                n.as_f64().and_then(|f| Self::of_value(&Value::Float(f)))
            }
            _ => None,
        }
    }
}

/// Where a node is, as serde_norway names it in an error: `a.b[0]`.
enum Path<'a> {
    Root,
    Seq { parent: &'a Path<'a>, index: usize },
    Map { parent: &'a Path<'a>, key: &'a str },
    Unknown { parent: &'a Path<'a> },
}

impl fmt::Display for Path<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        struct Parent<'a>(&'a Path<'a>);
        impl fmt::Display for Parent<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                match self.0 {
                    Path::Root => Ok(()),
                    path => write!(f, "{path}."),
                }
            }
        }
        match self {
            Self::Root => f.write_str("."),
            Self::Seq { parent, index } => write!(f, "{parent}[{index}]"),
            Self::Map { parent, key } => write!(f, "{}{key}", Parent(parent)),
            Self::Unknown { parent } => write!(f, "{}?", Parent(parent)),
        }
    }
}

/// The error for a key that repeats an earlier one, worded as
/// serde_norway words it, at the repeated key.
fn repeated(path: &Path<'_>, message: &str, span: &Span) -> Stop {
    let line = span.start.line();
    let col = span.start.col() + 1;
    let prefix = match path {
        Path::Root => String::new(),
        path => format!("{path}: "),
    };
    Stop::Repeated(Error::Parse {
        file: None,
        line: u32::try_from(line).unwrap_or(u32::MAX),
        col: u32::try_from(col).unwrap_or(u32::MAX),
        span: 0..0,
        snippet: String::new(),
        message: format!("{prefix}{message} at line {line} column {col}"),
    })
}

/// The events left to walk and the values of the anchors seen so far.
struct Walk<'t> {
    events: YamlParser<'t, StrInput<'t>>,
    anchors: HashMap<usize, Value>,
}

impl<'t> Walk<'t> {
    fn next(&mut self) -> Step<(Event<'t>, Span)> {
        match self.events.next_event() {
            Some(Ok(item)) => Ok(item),
            _ => Err(Stop::Disagree),
        }
    }

    /// One value per document, `nodes` being serde_norway's.
    fn documents(mut self, nodes: Vec<Node>) -> Step<Vec<Value>> {
        let mut nodes = nodes.into_iter();
        let mut values = Vec::new();
        loop {
            match self.next()?.0 {
                Event::StreamStart | Event::Nothing => {}
                Event::StreamEnd => {
                    return match nodes.next() {
                        None => Ok(values),
                        Some(_) => Err(Stop::Disagree),
                    };
                }
                Event::DocumentStart(_) => {
                    let node = nodes.next().ok_or(Stop::Disagree)?;
                    values.push(self.node(node, &Path::Root)?);
                    if !matches!(self.next()?.0, Event::DocumentEnd) {
                        return Err(Stop::Disagree);
                    }
                }
                _ => return Err(Stop::Disagree),
            }
        }
    }

    /// The value of the next node, serde_norway's `node`.
    fn node(&mut self, node: Node, path: &Path<'_>) -> Step<Value> {
        let (event, _) = self.next()?;
        self.node_from(event, node, path)
    }

    /// The value of the node that starts with `event`, its events consumed.
    fn node_from(&mut self, event: Event<'t>, node: Node, path: &Path<'_>) -> Step<Value> {
        let mut node = node;
        while let Node::Tagged(_, inner) = node {
            node = *inner;
        }
        let (value, anchor) = match event {
            Event::Scalar(literal, style, anchor, tag) => {
                let exact = exact_value(&literal, style, tag.as_deref(), &SerdeRead::of(&node));
                let value = match (exact, node) {
                    (Some(exact), _) => exact,
                    (
                        None,
                        node @ (Node::Null
                        | Node::Bool(_)
                        | Node::Number(_)
                        | Node::BigInt(_)
                        | Node::String(_)),
                    ) => node_to_value(node),
                    _ => return Err(Stop::Disagree),
                };
                (value, anchor)
            }
            Event::Alias(id) => return self.anchors.get(&id).cloned().ok_or(Stop::Disagree),
            Event::SequenceStart(anchor, _) => {
                let Node::Sequence(items) = node else {
                    return Err(Stop::Disagree);
                };
                let mut values = Vec::with_capacity(items.len());
                for (index, item) in items.into_iter().enumerate() {
                    values.push(self.node(
                        item,
                        &Path::Seq {
                            parent: path,
                            index,
                        },
                    )?);
                }
                if !matches!(self.next()?.0, Event::SequenceEnd) {
                    return Err(Stop::Disagree);
                }
                (Value::Array(values), anchor)
            }
            Event::MappingStart(anchor, _) => {
                let Node::Mapping(entries) = node else {
                    return Err(Stop::Disagree);
                };
                (self.mapping(entries, path)?, anchor)
            }
            _ => return Err(Stop::Disagree),
        };
        // saphyr-parser numbers anchors from 1; 0 is a node without one.
        if anchor != 0 {
            self.anchors.insert(anchor, value.clone());
        }
        Ok(value)
    }

    /// The map of `entries`, failing on a key that repeats an earlier one.
    fn mapping(&mut self, entries: Vec<Entry>, path: &Path<'_>) -> Step<Value> {
        let mut out: IndexMap<String, Value> = IndexMap::with_capacity(entries.len());
        let mut kinds: Vec<KeyKind> = Vec::with_capacity(entries.len());
        let mut numbers: HashMap<NumberId, usize> = HashMap::new();
        for entry in entries {
            let Entry {
                key,
                kind,
                key_node,
                value,
            } = entry;
            let (event, span) = self.next()?;
            let (key, kind, id, key_text) = match event {
                Event::Scalar(literal, style, anchor, tag) => {
                    let read = match &key_node {
                        Some(node) => SerdeRead::of(node),
                        None => SerdeRead::Str(&key),
                    };
                    let exact = exact_value(&literal, style, tag.as_deref(), &read);
                    let (key, kind, id, anchored) = match exact {
                        Some(exact) => (
                            literal.to_string(),
                            KeyKind::Number,
                            NumberId::of_value(&exact),
                            exact,
                        ),
                        None => {
                            let id = key_node.as_ref().and_then(NumberId::of_node);
                            let anchored = match key_node {
                                Some(node) => node_to_value(node),
                                None => Value::String(key.clone()),
                            };
                            (key, kind, id, anchored)
                        }
                    };
                    if anchor != 0 {
                        self.anchors.insert(anchor, anchored);
                    }
                    (key, kind, id, Some(literal))
                }
                Event::Alias(id) => {
                    let anchored = self.anchors.get(&id).ok_or(Stop::Disagree)?;
                    match anchored {
                        Value::BigInt(s) | Value::BigFloat(s) => (
                            s.clone(),
                            KeyKind::Number,
                            NumberId::of_value(anchored),
                            None,
                        ),
                        anchored => {
                            let id = NumberId::of_value(anchored);
                            (key, kind, id, None)
                        }
                    }
                }
                event => {
                    let node = key_node.ok_or(Stop::Disagree)?;
                    self.node_from(event, node, &Path::Unknown { parent: path })?;
                    (key, kind, None, None)
                }
            };
            let first = out
                .get_index_of(&key)
                .or_else(|| id.as_ref().and_then(|id| numbers.get(id).copied()));
            if let Some(first) = first {
                let (first_key, _) = out.get_index(first).ok_or(Stop::Disagree)?;
                return Err(repeated(
                    path,
                    &kinds[first].duplicate_message(first_key),
                    &span,
                ));
            }
            let value = match key_text.as_deref() {
                Some(text) => self.node(
                    value,
                    &Path::Map {
                        parent: path,
                        key: text,
                    },
                )?,
                None => self.node(value, &Path::Unknown { parent: path })?,
            };
            if let Some(id) = id {
                numbers.insert(id, out.len());
            }
            kinds.push(kind);
            out.insert(key, value);
        }
        if !matches!(self.next()?.0, Event::MappingEnd) {
            return Err(Stop::Disagree);
        }
        Ok(Value::Map(out))
    }
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

    fn error(yaml: &str) -> String {
        match crate::parsers::Yaml.parse(yaml.as_bytes()) {
            Err(Error::Parse { message, .. }) => message,
            other => panic!("{other:?}"),
        }
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
    fn may_lose_numbers_flags_only_literals_an_f64_misreads() {
        for yes in [
            "a: 0.1000000000000000000000000001\n",
            "a: 0.10000000000000004\n",
            "a: 1e400\n",
            "a: 1e-400\n",
            "a: 123456789012345678901234567890123456789\n",
            "a: !!float 18446744073709551617\n",
            "[-1E+400]",
        ] {
            assert!(may_lose_numbers(yes.as_bytes()), "{yes}");
        }
        for no in [
            // An `f64` holds each; integers to `u128` serde_norway keeps.
            "a: 0.10000000000000001\n",
            "a: 0.30000000000000004\n",
            "a: 1.5e300\n",
            "ns: 1700000000123456789\n",
            "id: 12345678901234567890123456789012345678\n",
            "a: 1.2.3\nrule400: x\nuuid: 123e4567-e89b-12d3-a456-426614174000\n",
            "a: 18446744073709551617\n",
        ] {
            assert!(!may_lose_numbers(no.as_bytes()), "{no}");
        }
    }

    #[test]
    fn reads_long_floats_huge_integers_and_floats_past_f64() {
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
    fn reads_through_aliases_local_tags_and_core_tags() {
        let big = "123456789012345678901234567890123456789012";
        let text = format!(
            "a: &x [1e400]\nb: *x\nc: !num 1e400\nd: !!float \"0.10000000000000000000001\"\n\
             e: !!float 1e400\nf: !!float \"1e400\"\ng: !!int {big}\nh: !<tag:yaml.org,2002:int> \"{big}\"\n\
             i: !!float 18446744073709551617\nj: &y !!float -1e400\nk: *y\n"
        );
        let one = Value::Array(vec![Value::BigFloat("1e400".into())]);
        let f = |s: &str| Value::BigFloat(s.into());
        assert_eq!(
            read(&text),
            map(&[
                ("a", one.clone()),
                ("b", one),
                ("c", f("1e400")),
                ("d", f("0.10000000000000000000001")),
                ("e", f("1e400")),
                ("f", f("1e400")),
                ("g", Value::BigInt(big.into())),
                ("h", Value::BigInt(big.into())),
                ("i", f("18446744073709551617")),
                ("j", f("-1e400")),
                ("k", f("-1e400")),
            ])
        );
        // What serde_norway refuses for another reason stays refused.
        assert!(error("a: !!float 1e400\nb: !!int x\n").contains("expected an integer"));
        assert!(error("a: !!int 0123\n").contains("expected an integer"));
    }

    #[test]
    fn keys_keep_their_literal_and_repeat_by_exact_value() {
        let text = "0.10000000000000000000001: a\n0.10000000000000000000002: b\n1e400: c\n";
        let Value::Map(m) = read(text) else { panic!() };
        let keys: Vec<&str> = m.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "0.10000000000000000000001",
                "0.10000000000000000000002",
                "1e400"
            ]
        );
        let big = "123456789012345678901234567890123456789012";
        assert_eq!(
            error(&format!("x:\n  {big}: a\n  {big}: b\n")),
            format!("x: duplicate entry with key {big} at line 3 column 3")
        );
        assert_eq!(
            error("1e400: a\n1.0e+400: b\n"),
            "duplicate entry with key 1e400 at line 2 column 1"
        );
    }

    #[test]
    fn a_key_repeated_after_stringification_fails_where_it_repeats() {
        for (text, message) in [
            (
                "18446744073709551616: a\n\"18446744073709551616\": b\n",
                "duplicate entry with key 18446744073709551616 at line 2 column 1",
            ),
            (
                "1: a\n\"1\": b\n",
                "duplicate entry with key 1 at line 2 column 1",
            ),
            (
                "a: 1\nb:\n  true: x\n  'true': y\n",
                "b: duplicate entry with key `true` at line 4 column 3",
            ),
            (
                "~: a\n\"\": b\n",
                "duplicate entry with null key at line 2 column 1",
            ),
            (
                "- {\"1\": a, 1: b}\n",
                ".[0]: duplicate entry with key \"1\" at line 1 column 12",
            ),
            (
                "-0.0: a\n0.0: b\n",
                "duplicate entry with key -0.0 at line 2 column 1",
            ),
            (
                "a: 1\na: 2\n",
                "duplicate entry with key \"a\" at line 2 column 1",
            ),
        ] {
            assert_eq!(error(text), message, "{text}");
        }
    }

    #[test]
    fn reads_each_document_of_a_stream() {
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
    fn leaves_leading_zero_digits_and_floats_an_f64_holds_alone() {
        let text = "a: 01234567890123456789\nb: 0.30000000000000004\nc: 1.5e300\nd: 1e400\n";
        assert_eq!(
            read(text),
            map(&[
                ("a", Value::String("01234567890123456789".into())),
                ("b", Value::Float(0.30000000000000004)),
                ("c", Value::Float(1.5e300)),
                ("d", Value::BigFloat("1e400".into())),
            ])
        );
    }

    #[test]
    fn a_float_tagged_integer_stays_exact_and_writes_back_a_float() {
        let doc = crate::parsers::Yaml
            .parse(b"a: !!float 18446744073709551617\n")
            .unwrap();
        let mut yaml = Vec::new();
        crate::parsers::Yaml.write(&doc, &mut yaml).unwrap();
        assert_eq!(
            String::from_utf8(yaml).unwrap(),
            "a: 18446744073709551617.0\n"
        );
        assert_eq!(
            doc.value().to_serde_json().to_string(),
            r#"{"a":18446744073709551617.0}"#
        );
        let mut json = Vec::new();
        crate::parsers::Json.write(&doc, &mut json).unwrap();
        assert!(
            String::from_utf8(json)
                .unwrap()
                .contains("18446744073709551617.0"),
            "JSON"
        );
    }

    #[test]
    fn tag_range_finds_the_tag_among_the_properties() {
        let text = b"&a !!float 1e400";
        assert_eq!(tag_range(text, 0), Some(3..10));
        assert_eq!(tag_range(b"!<tag:x> 1", 0), Some(0..8));
        assert_eq!(tag_range(b"[!!int 1,", 1), Some(1..6));
        assert_eq!(tag_range(b"1", 0), None);
    }
}
