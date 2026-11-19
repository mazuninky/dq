//! The native YAML writer: serde_norway's emitter, plus the scalar spellings
//! it cannot choose itself.
//!
//! serde_norway decides a string's style with its own YAML 1.2 resolver, so
//! it writes `1:20`, `y`, `on` or `2001-12-14` plain — and a YAML 1.1 reader
//! (PyYAML; go-yaml as used by Kubernetes and Helm) takes them for an int, a
//! bool, a date. It has no style hook, and it writes a [`Value::BigInt`]
//! past `u64` or a [`Value::BigFloat`] as a string. This module walks the
//! [`Value`] tree itself and hands the emitter, in place of each such
//! scalar:
//!
//! - a call that already yields the right scalar — `serialize_char` (always
//!   single-quoted) for a one-character string, `serialize_u64` (plain) for
//!   a big integer that fits `u64`;
//! - otherwise a *placeholder* — a plain token, swapped for the right text
//!   after emission.
//!
//! A placeholder is the marker `Z<lowercase letters>` padded with `x`. The
//! marker is chosen so that no string, key or number literal of the tree
//! contains it; the emitter copies printable ASCII verbatim and never makes
//! an uppercase `Z` itself (escapes are `\x..` with `0-9A-F`, `\N`, `\L`, …),
//! so the marker occurs in the output only where a placeholder was written.
//! The substitution checks that anyway: it runs only when the marker occurs
//! exactly once per placeholder, at a placeholder. As `Z` occurs in a
//! placeholder only at its start, two occurrences cannot overlap, and the
//! emitter writes scalars in tree order, so the n-th occurrence is the n-th
//! placeholder.
//!
//! A key placeholder is as long as the quoted key it stands for, so the
//! value after it lands in the same column, and the emitter's choice between
//! a simple key and a `? ` complex key (more than 128 bytes of content) is
//! the one it makes for the quoted key. The emitter is set to an unlimited
//! width: it never folds a line, so the column matters to nothing else.

use std::cell::RefCell;
use std::collections::HashSet;

use serde::ser::{Error as _, SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};

use crate::document::{Value, non_finite_literal};

/// Writes `v` as one YAML document — what `serde_norway::to_string` writes,
/// except for the scalars listed in the module docs.
pub(super) fn to_yaml_string(v: &Value) -> Result<String, serde_norway::Error> {
    for marker in free_markers(v).take(MAX_ATTEMPTS) {
        let ctx = Ctx {
            marker,
            fixes: RefCell::new(Vec::new()),
        };
        let out = serde_norway::to_string(&Node {
            value: v,
            ctx: &ctx,
        })?;
        let fixes = ctx.fixes.into_inner();
        if fixes.is_empty() {
            return Ok(out);
        }
        if let Some(done) = substitute(&out, &ctx.marker, &fixes) {
            return Ok(done);
        }
    }
    Err(serde_norway::Error::custom(
        "the YAML writer found its placeholders clash with the output",
    ))
}

/// Whether a YAML 1.1 reader (go-yaml, as in Kubernetes and GitHub
/// Actions; PyYAML) or a YAML 1.2 core / JSON schema one would take the
/// plain scalar `s` for something other than a string — a bool, null,
/// int (`1_000`, `0b1`, `017`, `1:20`), float (`1.`, `1e3`), timestamp
/// (`2001-12-14`), or the `<<` / `=` keys — though dq's own reader may
/// not.
pub(super) fn typed_by_another_schema(s: &str) -> bool {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(concat!(
            r"^(?:",
            r"~|null|Null|NULL",
            r"|y|Y|yes|Yes|YES|n|N|no|No|NO|true|True|TRUE|false|False|FALSE",
            r"|on|On|ON|off|Off|OFF",
            r"|[-+]?0b[01_]+|[-+]?0o?[0-7_]+|[-+]?0x[0-9a-fA-F_]+",
            r"|[-+]?(?:0|[1-9][0-9_]*)(?::[0-5]?[0-9])*",
            r"|[-+]?(?:[0-9][0-9_]*)?\.[0-9_]*(?:[eE][-+]?[0-9]+)?",
            r"|[-+]?[0-9][0-9_]*[eE][-+]?[0-9]+",
            r"|[-+]?[0-9][0-9_]*(?::[0-5]?[0-9])+\.[0-9_]*",
            r"|[-+]?\.(?:inf|Inf|INF)|\.(?:nan|NaN|NAN)",
            r"|[0-9]{4}-[0-9]{2}-[0-9]{2}",
            r"|[0-9]{4}-[0-9]{1,2}-[0-9]{1,2}(?:[Tt]|[ \t]+)[0-9]{1,2}:[0-9]{2}:[0-9]{2}",
            r"(?:\.[0-9]*)?(?:[ \t]*(?:Z|[-+][0-9]{1,2}(?::[0-9]{2})?))?",
            r"|<<|=",
            r")$",
        ))
        .expect("the YAML type regex compiles")
    })
    .is_match(s)
}

/// Emissions tried with a marker the tree does not contain. The first one
/// succeeds — the emitter makes no `Z` of its own — so a failure means that
/// reasoning broke, and the writer errs instead of looping.
const MAX_ATTEMPTS: usize = 4;

/// The longest key content the emitter writes as a simple key.
const MAX_SIMPLE_KEY: usize = 128;

/// The marker letters, rare ones first, so the first marker is almost always
/// free.
const LETTERS: &[u8; 26] = b"qjxzvkwyfbgpmhdcultsnreoai";

/// The longest run of lowercase letters after a `Z` that is kept: 26^13
/// windows are more than any tree has `Z`s, so a marker is never longer.
const MAX_RUN: usize = 13;

/// The markers `Z` + `len` letters that no key, string or number literal of
/// `v` contains, in [`LETTERS`] order, for the shortest `len` that leaves
/// one.
///
/// One pass over the tree records, for every `Z`, the lowercase letters
/// after it. `Z` + `w` occurs in the tree exactly when `w` is the first
/// `len` of those letters after some `Z`, so the occupied words of a length
/// are a set no larger than the number of `Z`s, and the first length with a
/// word outside the set is found without scanning the tree again.
fn free_markers(v: &Value) -> impl Iterator<Item = String> {
    let mut runs = Vec::new();
    collect_runs(v, &mut runs);
    let (len, taken) = (1..=MAX_RUN as u32)
        .map(|len| {
            let taken: HashSet<u64> = runs
                .iter()
                .filter(|&&(run, _)| run >= len)
                .map(|&(run, packed)| packed / 26u64.pow(run - len))
                .collect();
            (len, taken)
        })
        .find(|(len, taken)| (taken.len() as u64) < 26u64.pow(*len))
        .expect("more letters than `Z`s in any tree");
    (0..26u64.pow(len)).filter_map(move |index| {
        let mut word = vec![b'Z'];
        let mut packed = 0u64;
        let mut rest = index;
        for _ in 0..len {
            let letter = LETTERS[(rest % 26) as usize];
            rest /= 26;
            word.push(letter);
            packed = packed * 26 + u64::from(letter - b'a');
        }
        (!taken.contains(&packed)).then(|| String::from_utf8(word).expect("ASCII"))
    })
}

/// Appends, for every `Z` in a key, string or number literal of `v`, the
/// number of lowercase letters after it (at most [`MAX_RUN`]) and those
/// letters packed in base 26, first letter most significant.
fn collect_runs(v: &Value, runs: &mut Vec<(u32, u64)>) {
    fn scan(s: &str, runs: &mut Vec<(u32, u64)>) {
        let bytes = s.as_bytes();
        for (at, _) in bytes.iter().enumerate().filter(|&(_, &b)| b == b'Z') {
            let mut packed = 0u64;
            let mut run = 0u32;
            for &b in bytes[at + 1..].iter().take(MAX_RUN) {
                if !b.is_ascii_lowercase() {
                    break;
                }
                packed = packed * 26 + u64::from(b - b'a');
                run += 1;
            }
            runs.push((run, packed));
        }
    }
    match v {
        Value::String(s) | Value::BigInt(s) | Value::BigFloat(s) => scan(s, runs),
        Value::Array(items) => items.iter().for_each(|item| collect_runs(item, runs)),
        Value::Map(map) => {
            for (k, item) in map {
                scan(k, runs);
                collect_runs(item, runs);
            }
        }
        Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) => {}
    }
}

/// One placeholder written and the text that replaces it.
struct Fix {
    placeholder: String,
    text: String,
}

/// The marker of one emission attempt and the placeholders it wrote, in
/// emission order.
struct Ctx {
    marker: String,
    fixes: RefCell<Vec<Fix>>,
}

impl Ctx {
    /// Writes a placeholder `len` bytes long (or the bare marker, if that is
    /// longer) to stand for `text`.
    fn placeholder<S: Serializer>(
        &self,
        serializer: S,
        text: String,
        len: usize,
    ) -> Result<S::Ok, S::Error> {
        let mut placeholder = self.marker.clone();
        while placeholder.len() < len {
            placeholder.push('x');
        }
        let result = serializer.serialize_str(&placeholder);
        self.fixes.borrow_mut().push(Fix { placeholder, text });
        result
    }
}

/// A value of the tree, serialized as `Value`'s own `Serialize` impl does
/// it except for the scalars this module respells.
struct Node<'a> {
    value: &'a Value,
    ctx: &'a Ctx,
}

/// A mapping key and the value it maps to.
struct Key<'a> {
    key: &'a str,
    value: &'a Value,
    ctx: &'a Ctx,
}

impl Serialize for Node<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.value {
            Value::String(s) => match quoted_for_other_readers(s) {
                Some(Quoted::Char(c)) => serializer.serialize_char(c),
                Some(Quoted::Text(text)) => self.ctx.placeholder(serializer, text, 0),
                None => serializer.serialize_str(s),
            },
            Value::BigInt(s) => match plain_u64(s) {
                Some(u) => serializer.serialize_u64(u),
                None if plain_integer(s) => self.ctx.placeholder(serializer, s.clone(), 0),
                None => serializer.serialize_str(s),
            },
            Value::BigFloat(s) => {
                if let Some(f) = non_finite_literal(s) {
                    serializer.serialize_f64(f)
                } else if let Some(text) = yaml11_float(s) {
                    self.ctx.placeholder(serializer, text, 0)
                } else {
                    serializer.serialize_str(s)
                }
            }
            Value::Array(items) => {
                let mut seq = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(&Node {
                        value: item,
                        ctx: self.ctx,
                    })?;
                }
                seq.end()
            }
            Value::Map(map) => {
                let mut m = serializer.serialize_map(Some(map.len()))?;
                for (k, item) in map {
                    m.serialize_entry(
                        &Key {
                            key: k,
                            value: item,
                            ctx: self.ctx,
                        },
                        &Node {
                            value: item,
                            ctx: self.ctx,
                        },
                    )?;
                }
                m.end()
            }
            Value::Float(f) => match yaml11_exponent_float(*f) {
                Some(text) => self.ctx.placeholder(serializer, text, 0),
                None => serializer.serialize_f64(*f),
            },
            Value::Null | Value::Bool(_) | Value::Int(_) => self.value.serialize(serializer),
        }
    }
}

impl Serialize for Key<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if is_merge(self.key, self.value) {
            return serializer.serialize_str(self.key);
        }
        match quoted_for_other_readers(self.key) {
            Some(Quoted::Char(c)) => serializer.serialize_char(c),
            Some(Quoted::Text(text)) => {
                // As long as the quoted key, unless that would cross the
                // simple-key limit the bare content stays under.
                let len = if text.len() <= MAX_SIMPLE_KEY {
                    text.len()
                } else {
                    self.key.len()
                };
                self.ctx.placeholder(serializer, text, len)
            }
            None => serializer.serialize_str(self.key),
        }
    }
}

/// Whether `<<: value` is a merge a YAML 1.1 reader can apply — a mapping
/// or a sequence of mappings, an empty sequence included. Such a key is
/// written plain, as before, so a YAML 1.1 reader merges it.
///
/// That holds whatever the document came from: a `"<<"` key over a mapping
/// in JSON or TOML input is a literal key there, and comes out as a merge
/// to those readers. The writer cannot tell the two apart — dq reads a YAML
/// merge key as the plain key `<<`, with the aliased mapping copied in —
/// and the trade-off favours the common case: quoting the key would turn
/// every merge of a docker-compose or Helm file that `set --jq` or `fix`
/// re-emits into a literal `<<` key. Any other value is no merge (a YAML
/// 1.1 reader fails on it), and the key is quoted.
fn is_merge(key: &str, value: &Value) -> bool {
    key == "<<"
        && match value {
            Value::Map(_) => true,
            Value::Array(items) => items.iter().all(|item| matches!(item, Value::Map(_))),
            _ => false,
        }
}

/// How a string is written in place of the emitter's spelling: one it
/// would write plain though another YAML reader types it, and one holding
/// LS or PS (U+2028, U+2029).
enum Quoted {
    /// One character: `serialize_char` single-quotes it.
    Char(char),
    /// The single-quoted scalar.
    Text(String),
}

fn quoted_for_other_readers(s: &str) -> Option<Quoted> {
    // The emitter takes LS and PS for line breaks, as YAML 1.1 does: it
    // writes them raw, in a single-quoted or a block scalar, and indents
    // the next line, which a YAML 1.2 reader, for which they are no line
    // breaks, keeps as content. Escaped, every reader reads them alike.
    if s.contains(['\u{2028}', '\u{2029}']) {
        return Some(Quoted::Text(double_quoted(s)));
    }
    if !typed_by_another_schema(s) || !emitted_plain(s) {
        return None;
    }
    let mut chars = s.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Some(Quoted::Char(c));
    }
    Some(Quoted::Text(format!("'{}'", s.replace('\'', "''"))))
}

/// Whether serde_norway writes the string `s` as a plain scalar. Its choice
/// for a one-line, non-empty string is the same as a document, a value or
/// a key, so the string is asked alone.
fn emitted_plain(s: &str) -> bool {
    serde_norway::to_string(s).is_ok_and(|out| out.strip_suffix('\n').is_some_and(|text| text == s))
}

/// The plain scalar dq's YAML writer spells a finite float with — a
/// [`Value::Float`] or a decimal [`Value::BigFloat`]: ryu's shortest
/// round-trip digits, an exponent in the YAML 1.1 shape (`3.0`, `123.25`,
/// `1.0e+300`, `1.5e+20`, `1.0e-7`, `1.0e+400`). `None` for any other
/// value, a NaN or an infinity included.
#[must_use]
pub fn yaml_float_text(value: &Value) -> Option<String> {
    match value {
        Value::Float(f) if f.is_finite() => Some(
            yaml11_exponent_float(*f).unwrap_or_else(|| serde_norway::Number::from(*f).to_string()),
        ),
        Value::BigFloat(s) if non_finite_literal(s).is_none() => yaml11_float(s),
        _ => None,
    }
}

/// The respelling of a finite float serde_norway writes in exponent form
/// that a YAML 1.1 reader takes for a string — no `.` (`1e16`, `-1e-7`) or
/// an unsigned exponent (`1.5e300`): `1.0e+16`, `-1.0e-7`, `1.5e+300`.
/// The digits are serde_norway's (ryu's shortest round-trip), so dq reads
/// the same `f64` back. `None` when serde_norway's spelling stands — any
/// float it writes without an exponent, `-0.0` included.
fn yaml11_exponent_float(f: f64) -> Option<String> {
    // ryu switches to an exponent only at or above 1e16 and below 1e-5;
    // the cheap bounds keep every other float off the second emission.
    let magnitude = f.abs();
    if !f.is_finite() || f == 0.0 || (1e-4..1e15).contains(&magnitude) {
        return None;
    }
    let written = serde_norway::to_string(&f).ok()?;
    let written = written.strip_suffix('\n')?;
    if !written.contains(['e', 'E']) {
        return None;
    }
    yaml11_float(written).filter(|text| text != written)
}

/// The `u64` a big-integer literal spells exactly — a plain integer that
/// dq's reader reads back as the same [`Value::BigInt`], and that
/// serde_norway's emitter writes itself.
fn plain_u64(literal: &str) -> Option<u64> {
    literal
        .parse::<u64>()
        .ok()
        .filter(|u| u.to_string() == literal)
}

/// Whether a big-integer literal is a decimal integer as dq spells one
/// (`-?digits`, no leading zero), written plain: dq's reader reads it back
/// as the same [`Value::BigInt`], a YAML 1.1 reader (PyYAML) as an int.
/// serde_norway's emitter would quote it, as a string its reader types.
fn plain_integer(literal: &str) -> bool {
    matches!(Value::from_number_literal(literal), Some(Value::BigInt(spelled)) if spelled == literal)
}

/// A float literal spelled as a float to a YAML 1.1 reader too: a `.` in
/// the mantissa and a signed exponent (`1e400` → `1.0e+400`), which a
/// YAML 1.2 core reader also matches as a float. `None` for anything not
/// shaped `[-+]digits[.digits][e[-+]digits]`.
///
/// dq's own reader reads it back as the same [`Value::BigFloat`] (the same
/// decimal: [`Value::same_data`]), whether it holds more digits than an
/// `f64` or lies past its range.
pub(super) fn yaml11_float(literal: &str) -> Option<String> {
    let (sign, rest) = match literal.as_bytes().first()? {
        b'-' | b'+' => literal.split_at(1),
        _ => ("", literal),
    };
    let (mantissa, exponent) = match rest.find(['e', 'E']) {
        Some(at) => (&rest[..at], Some((&rest[at..=at], &rest[at + 1..]))),
        None => (rest, None),
    };
    let (int, frac) = match mantissa.split_once('.') {
        Some((int, frac)) => (int, Some(frac)),
        None => (mantissa, None),
    };
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if !digits(int) || !frac.is_none_or(digits) || int.len() + frac.map_or(0, str::len) == 0 {
        return None;
    }
    // `.5` needs no `0` in YAML 1.2, but a YAML 1.1 reader (PyYAML) reads
    // `-.5e-3` as a string: a sign wants a digit before the dot.
    let int = if int.is_empty() { "0" } else { int };
    let mut out = format!("{sign}{int}.");
    match frac {
        Some(frac) => out.push_str(frac),
        None => out.push('0'),
    }
    if let Some((e, exp)) = exponent {
        let (exp_sign, exp_digits) = match exp.as_bytes().first()? {
            b'-' | b'+' => exp.split_at(1),
            _ => ("+", exp),
        };
        if exp_digits.is_empty() || !digits(exp_digits) {
            return None;
        }
        out.push_str(e);
        out.push_str(exp_sign);
        out.push_str(exp_digits);
    }
    Some(out)
}

/// The text a collection used as a mapping key becomes: `v` written as
/// compact flow YAML — `[1, 2]`, `{k: v}`, `{a: [1, {b: c}]}`, `[]`, `{}`.
/// A string is quoted when this writer would quote it as a value (`'on'`,
/// `'a: b'`, `''`) or when it holds a flow indicator (`,` `[` `]` `{` `}`),
/// which a plain scalar cannot hold in flow; a number is spelled as dq
/// spells a number key (`1.5`, `1e400`); null is `null`. The text depends
/// on `v` alone: no tag or anchor of the key, nor how it was written.
pub(super) fn flow_key_text(v: &Value) -> String {
    let mut out = String::new();
    write_flow(v, &mut out);
    out
}

fn write_flow(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(i) => out.push_str(&i.to_string()),
        // As serde_norway prints the number key it reads (ryu).
        Value::Float(f) => out.push_str(&serde_norway::Number::from(*f).to_string()),
        Value::BigInt(s) | Value::BigFloat(s) => out.push_str(s),
        Value::String(s) => write_flow_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (at, item) in items.iter().enumerate() {
                if at > 0 {
                    out.push_str(", ");
                }
                write_flow(item, out);
            }
            out.push(']');
        }
        Value::Map(map) => {
            out.push('{');
            for (at, (key, item)) in map.iter().enumerate() {
                if at > 0 {
                    out.push_str(", ");
                }
                write_flow_string(key, out);
                out.push_str(": ");
                write_flow(item, out);
            }
            out.push('}');
        }
    }
}

/// `s` as a flow scalar: plain when this writer writes it plain, it holds
/// no flow indicator and starts with no `?` or `:` (in flow, a key or a
/// value indicator: `[?a]` reads as `[{a: null}]`), else single-quoted,
/// else — for a line break or a character a single-quoted scalar cannot
/// hold — double-quoted.
fn write_flow_string(s: &str, out: &mut String) {
    if !s.contains([',', '[', ']', '{', '}'])
        && !s.starts_with(['?', ':'])
        && !typed_by_another_schema(s)
        && emitted_plain(s)
    {
        out.push_str(s);
    } else if s.chars().all(single_quotable) {
        out.push('\'');
        out.push_str(&s.replace('\'', "''"));
        out.push('\'');
    } else {
        out.push_str(&double_quoted(s));
    }
}

/// `s` as a double-quoted scalar on one line: every character a
/// single-quoted scalar cannot hold escaped — line breaks (`\n`, `\N`,
/// `\L`, `\P`), controls, U+FEFF — so it reads back alike under YAML 1.1
/// and 1.2.
fn double_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            '\u{85}' => out.push_str("\\N"),
            '\u{2028}' => out.push_str("\\L"),
            '\u{2029}' => out.push_str("\\P"),
            c if single_quotable(c) => out.push(c),
            c if u32::from(c) <= 0xFF => out.push_str(&format!("\\x{:02X}", u32::from(c))),
            c if u32::from(c) <= 0xFFFF => out.push_str(&format!("\\u{:04X}", u32::from(c))),
            c => out.push_str(&format!("\\U{:08X}", u32::from(c))),
        }
    }
    out.push('"');
    out
}

/// Whether a single-quoted scalar holds `c` as it is: a printable
/// character (as libyaml, serde_norway's emitter, counts them) that is no
/// line break.
pub(super) fn single_quotable(c: char) -> bool {
    matches!(
        c,
        ' '..='~' | '\u{A0}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..
    ) && !matches!(c, '\u{FEFF}' | '\u{2028}' | '\u{2029}')
}

/// Replaces the placeholders of `out` with their text, or `None` unless
/// the marker occurs exactly once per placeholder, each time at one.
fn substitute(out: &str, marker: &str, fixes: &[Fix]) -> Option<String> {
    let hits: Vec<usize> = out.match_indices(marker).map(|(at, _)| at).collect();
    if hits.len() != fixes.len() {
        return None;
    }
    let mut done = String::with_capacity(out.len());
    let mut from = 0;
    for (&at, fix) in hits.iter().zip(fixes) {
        let end = at + fix.placeholder.len();
        let whole_token = out
            .as_bytes()
            .get(end)
            .is_none_or(|b| !b.is_ascii_alphanumeric());
        if out.get(at..end) != Some(fix.placeholder.as_str()) || !whole_token {
            return None;
        }
        done.push_str(&out[from..at]);
        done.push_str(&fix.text);
        from = end;
    }
    done.push_str(&out[from..]);
    Some(done)
}

#[cfg(test)]
mod tests {
    use indexmap::IndexMap;

    use crate::format::Format as _;

    use super::*;

    fn yaml(v: &Value) -> String {
        to_yaml_string(v).unwrap()
    }

    fn map(entries: &[(&str, Value)]) -> Value {
        let mut m = IndexMap::new();
        for (k, v) in entries {
            m.insert((*k).to_owned(), v.clone());
        }
        Value::Map(m)
    }

    fn s(text: &str) -> Value {
        Value::String(text.to_owned())
    }

    #[test]
    fn flow_key_text_is_compact_flow_yaml_of_the_value() {
        let nested = map(&[(
            "a",
            Value::Array(vec![Value::Int(1), map(&[("b", s("c"))])]),
        )]);
        assert_eq!(flow_key_text(&nested), "{a: [1, {b: c}]}");
        let scalars = Value::Array(vec![
            s("on"),
            s("a: b"),
            s(""),
            s("x,y"),
            s("it's"),
            s("'q'"),
            s("1e400"),
            Value::BigFloat("1e400".into()),
            Value::BigInt("123456789012345678901234567890".into()),
            Value::Float(1.5),
            Value::Float(1e16),
            Value::Float(-0.0),
            Value::Int(-3),
            Value::Bool(true),
            Value::Null,
            Value::Array(vec![]),
            map(&[]),
        ]);
        assert_eq!(
            flow_key_text(&scalars),
            "['on', 'a: b', '', 'x,y', it's, '''q''', '1e400', 1e400, \
             123456789012345678901234567890, 1.5, 1e16, -0.0, -3, true, null, [], {}]"
        );
        let escaped = Value::Array(vec![s("a\nb"), s("tab\there"), s("\u{7}\"\\")]);
        assert_eq!(
            flow_key_text(&escaped),
            r#"["a\nb", "tab\there", "\x07\"\\"]"#
        );
        assert_eq!(flow_key_text(&map(&[("", Value::Null)])), "{'': null}");
    }

    #[test]
    fn flow_key_text_reads_back_as_the_same_value() {
        let tricky = [
            "?a",
            ":x",
            "?x",
            ":>",
            "?",
            ":",
            "??",
            "::",
            "-",
            "-x",
            "- x",
            "-1",
            "--",
            "---",
            "...",
            "? x",
            ": x",
            "a:b",
            "a: b",
            "a:",
            "a?",
            "#a",
            "a #b",
            "a#b",
            "&a",
            "*a",
            "!a",
            "|",
            ">",
            "%a",
            "@a",
            "`a",
            "'",
            "\"",
            "a'b",
            "a\"b",
            "[",
            "]",
            "{",
            "}",
            ",",
            "a,b",
            "{a}",
            "[a]",
            "a: [b]",
            " a",
            "a ",
            "",
            "~",
            "null",
            "true",
            "on",
            "y",
            "1",
            "1.5",
            "0x1",
            "1e400",
            ".inf",
            "<<",
            "=",
            "2001-12-14",
            "a\nb",
            "a\tb",
            "\u{7}",
            "é",
            "\u{FEFF}x",
            "x\u{85}y",
            "x\u{2028}y",
            "a\\b",
            "\\",
        ];
        for text in tricky {
            for v in [
                Value::Array(vec![s(text)]),
                map(&[(text, s(text))]),
                map(&[("k", Value::Array(vec![s(text), map(&[(text, s(text))])]))]),
            ] {
                let flow = flow_key_text(&v);
                let read = crate::parsers::Yaml
                    .parse(flow.as_bytes())
                    .unwrap_or_else(|err| panic!("{text:?} as {flow:?}: {err}"));
                assert_eq!(read.value(), &v, "{text:?} as {flow:?}");
            }
        }
    }

    #[test]
    fn escapes_line_and_paragraph_separators_in_double_quotes() {
        let v = map(&[
            ("\u{2028}", s("a\u{2029}\nb")),
            ("c", Value::Array(vec![s("\u{2028}")])),
        ]);
        assert_eq!(yaml(&v), "\"\\L\": \"a\\P\\nb\"\nc:\n- \"\\L\"\n");
    }

    #[test]
    fn quotes_strings_a_yaml_1_1_reader_types() {
        let v = map(&[("a", s("1:20")), ("b", s("y")), ("c", s("2001-12-14"))]);
        assert_eq!(yaml(&v), "a: '1:20'\nb: 'y'\nc: '2001-12-14'\n");
    }

    #[test]
    fn quotes_such_keys_and_keeps_the_value_column() {
        let v = map(&[("on", s("x")), ("y", Value::Int(1)), ("plain", s("on"))]);
        assert_eq!(yaml(&v), "'on': x\n'y': 1\nplain: 'on'\n");
    }

    #[test]
    fn keeps_a_merge_key_plain_and_quotes_any_other() {
        let inner = map(&[("x", Value::Int(1))]);
        let merge = map(&[("<<", inner.clone()), ("k", Value::Int(2))]);
        assert_eq!(yaml(&merge), "<<:\n  x: 1\nk: 2\n");
        let list = map(&[("<<", Value::Array(vec![inner])), ("k", Value::Int(2))]);
        assert_eq!(yaml(&list), "<<:\n- x: 1\nk: 2\n");
        let scalar = map(&[("<<", s("x")), ("k", s("<<"))]);
        assert_eq!(yaml(&scalar), "'<<': x\nk: '<<'\n");
    }

    #[test]
    fn leaves_strings_serde_norway_already_quotes() {
        let v = map(&[("a", s("0o17")), ("b", s("true")), ("c", s("hello"))]);
        assert_eq!(yaml(&v), serde_norway::to_string(&v).unwrap());
    }

    #[test]
    fn writes_a_big_integer_in_u64_plain() {
        let v = map(&[("c", Value::BigInt("18446744073709551615".into()))]);
        assert_eq!(yaml(&v), "c: 18446744073709551615\n");
    }

    #[test]
    fn writes_a_big_integer_past_u64_plain_and_reads_it_back() {
        let v = map(&[
            ("c", Value::BigInt("123456789012345678901234567890".into())),
            ("d", Value::BigInt("-9223372036854775809".into())),
            // Not a literal dq spells: written as the string it may be.
            ("e", Value::BigInt("+18446744073709551616".into())),
        ]);
        let out = yaml(&v);
        assert_eq!(
            out,
            "c: 123456789012345678901234567890\nd: -9223372036854775809\ne: '+18446744073709551616'\n"
        );
        let back = crate::parsers::Yaml.parse(out.as_bytes()).unwrap();
        let Value::Map(m) = back.value() else {
            panic!()
        };
        assert_eq!(m["c"], v_at(&v, "c"));
        assert_eq!(m["d"], v_at(&v, "d"));
    }

    fn v_at(v: &Value, key: &str) -> Value {
        let Value::Map(m) = v else { panic!() };
        m[key].clone()
    }

    #[test]
    fn a_big_float_reads_back_as_the_same_number() {
        for literal in ["1e400", "-1.5E-400", "3.14159265358979323846264338327"] {
            let v = map(&[("f", Value::BigFloat(literal.into()))]);
            let back = crate::parsers::Yaml.parse(yaml(&v).as_bytes()).unwrap();
            assert!(back.value().same_data(&v), "{literal}: {:?}", back.value());
        }
    }

    #[test]
    fn writes_an_exponent_float_with_a_dot_and_a_signed_exponent() {
        let floats = [
            1e16,
            -1e16,
            1e-7,
            -1e-7,
            1.5e300,
            1.5e-7,
            2.5e-300,
            f64::MAX,
        ];
        let v = Value::Array(floats.iter().copied().map(Value::Float).collect());
        let out = yaml(&v);
        assert_eq!(
            out,
            "- 1.0e+16\n- -1.0e+16\n- 1.0e-7\n- -1.0e-7\n- 1.5e+300\n- 1.5e-7\n\
             - 2.5e-300\n- 1.7976931348623157e+308\n"
        );
        let Value::Array(back) = crate::parsers::Yaml
            .parse(out.as_bytes())
            .unwrap()
            .value()
            .clone()
        else {
            panic!("an array");
        };
        for (f, read) in floats.iter().zip(back) {
            let Value::Float(read) = read else {
                panic!("{f} read back as {read:?}");
            };
            assert_eq!(read.to_bits(), f.to_bits(), "{f}");
        }
    }

    #[test]
    fn leaves_floats_written_without_an_exponent_as_head_wrote_them() {
        let v = Value::Array(
            [
                0.0,
                -0.0,
                0.1,
                -2.25,
                1e15,
                9999999999999998.0,
                1e-5,
                123456.789,
            ]
            .into_iter()
            .map(Value::Float)
            .collect(),
        );
        assert_eq!(yaml(&v), serde_norway::to_string(&v).unwrap());
    }

    #[test]
    fn writes_a_big_float_as_a_yaml_1_1_float() {
        let v = Value::Array(vec![
            Value::BigFloat("1e400".into()),
            Value::BigFloat("-1.5E-400".into()),
            Value::BigFloat("3.14159265358979323846264338327".into()),
            Value::BigFloat(".nan".into()),
        ]);
        assert_eq!(
            yaml(&v),
            "- 1.0e+400\n- -1.5E-400\n- 3.14159265358979323846264338327\n- .nan\n"
        );
    }

    #[test]
    fn leaves_a_big_float_of_another_shape_as_head_wrote_it() {
        let v = Value::BigFloat("Infinity".into());
        assert_eq!(yaml(&v), serde_norway::to_string(&v).unwrap());
    }

    #[test]
    fn avoids_a_marker_the_tree_contains() {
        let v = map(&[("Zq", s("Zj Zqx")), ("k", s("on"))]);
        assert_eq!(yaml(&v), "Zq: Zj Zqx\nk: 'on'\n");
    }

    #[test]
    fn substitute_refuses_a_stray_marker() {
        let fixes = [Fix {
            placeholder: "Zq".into(),
            text: "'y'".into(),
        }];
        assert_eq!(substitute("a: Zq\nb: Zq\n", "Zq", &fixes), None);
        assert_eq!(substitute("a: Zqa\n", "Zq", &fixes), None);
        assert_eq!(
            substitute("a: Zq\n", "Zq", &fixes).as_deref(),
            Some("a: 'y'\n")
        );
    }

    #[test]
    fn free_markers_take_the_first_free_word_of_the_shortest_free_length() {
        let none = Value::Null;
        let first: Vec<String> = free_markers(&none).take(2).collect();
        assert_eq!(first, ["Zq", "Zj"]);
        // Every `Z` + one letter is taken: the marker gets two letters, and
        // `Zqq` (inside `Zqqx`) is taken too.
        let mut words: Vec<Value> = LETTERS
            .iter()
            .map(|&b| Value::String(format!("Z{}", char::from(b))))
            .collect();
        words.push(s("Zqqx"));
        let first: Vec<String> = free_markers(&Value::Array(words)).take(2).collect();
        assert_eq!(first, ["Zjq", "Zxq"]);
    }

    #[test]
    fn yaml11_float_spells_the_dot_and_the_exponent_sign() {
        assert_eq!(yaml11_float("1e400").as_deref(), Some("1.0e+400"));
        assert_eq!(yaml11_float(".5e-3").as_deref(), Some("0.5e-3"));
        assert_eq!(yaml11_float("-.5e-3").as_deref(), Some("-0.5e-3"));
        assert_eq!(yaml11_float("+.5").as_deref(), Some("+0.5"));
        assert_eq!(yaml11_float("+7").as_deref(), Some("+7.0"));
        assert_eq!(yaml11_float("1.e5").as_deref(), Some("1.e+5"));
        assert_eq!(yaml11_float("e5"), None);
        assert_eq!(yaml11_float("1e"), None);
        assert_eq!(yaml11_float("1_0.5"), None);
    }

    #[test]
    fn typed_by_another_schema_matches_yaml_1_1_scalars() {
        for typed in [
            "y",
            "on",
            "1:20",
            "1_000",
            "0b1",
            "017",
            "2001-12-14",
            "<<",
            "=",
        ] {
            assert!(typed_by_another_schema(typed), "{typed}");
        }
        for plain in ["yes please", "hello", "1:2:3x", ""] {
            assert!(!typed_by_another_schema(plain), "{plain}");
        }
    }
}
