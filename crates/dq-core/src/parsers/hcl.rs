//! HCL 2 (HashiCorp Configuration Language) parser and writer.
//!
//! M5 v1: read+best-effort write. Comments, operator spacing, and original
//! number-literal formatting are NOT preserved on the write path — see
//! design D1 / D11. Block syntax `block_type "label" { ... }` becomes
//! `Map { block_type: { label: { ... } } }` (one nesting level per label),
//! per the format-support spec scenario "HCL backend block parses to
//! nested map".
//!
//! This parser produces a read-only [`Document`] (no spans, no `set_at`/
//! `del_at` round-trip) — `set_at` against an HCL document returns
//! [`Error::WriteUnavailable`] until a future milestone wires up a
//! span-aware path. For M5 the value-tree edits go through
//! [`Format::write`] re-emission instead.

use std::io::Write;

use camino::Utf8PathBuf;
use hcl::{Block, BlockLabel, Body, Expression, Number, Structure, Value as HclValue};
use indexmap::IndexMap;

use crate::Result;
use crate::document::{Document, FormatTag, Value};
use crate::error::Error;
use crate::format::Format;

/// HCL format implementation.
#[derive(Debug, Clone, Copy)]
pub struct Hcl;

impl Format for Hcl {
    fn name(&self) -> &'static str {
        "hcl"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["hcl", "tf", "tfvars"]
    }

    fn parse(&self, bytes: &[u8]) -> Result<Document> {
        let text = std::str::from_utf8(bytes).map_err(|e| Error::Parse {
            file: None,
            line: 0,
            col: 0,
            span: 0..0,
            snippet: String::new(),
            message: format!("invalid UTF-8 in HCL input: {e}"),
        })?;
        let body: Body = text
            .parse()
            .map_err(|e: hcl::Error| map_hcl_parse_error(e))?;
        let value = body_to_value(&body);
        Ok(Document::value_only(value, FormatTag::Hcl))
    }

    fn write(&self, doc: &Document, w: &mut dyn Write) -> Result<()> {
        // Re-emit through `hcl::to_string` on an `hcl::Value`. Comments are
        // not preserved (M5 v1). Big-numeric scalars (`Value::BigInt` /
        // `Value::BigFloat`) are best-effort emitted as their textual form
        // through `HclValue::String` — the `hcl-rs` `Number` type cannot
        // carry arbitrary-precision literals.
        let hcl_value = value_to_hcl_value(doc.value());
        let serialized = hcl::to_string(&hcl_value).map_err(|e| Error::Format {
            format: "hcl",
            message: format!("failed to serialize HCL: {e}"),
        })?;
        w.write_all(serialized.as_bytes())
            .map_err(|source| Error::Io {
                path: Utf8PathBuf::from("<hcl-writer>"),
                source,
            })
    }
}

/// Convert an `hcl::Body` (top-level or nested block body) into our `Value`.
///
/// Each [`Attribute`] in the body becomes a `key → value` entry in the result
/// map. Each [`Block`] is grouped under its identifier; if the block has
/// labels, one level of nesting is added per label (per the
/// `HCL backend block parses to nested map` spec scenario).
///
/// Multiple blocks sharing the same identifier+label path get their bodies
/// merged into the same map (e.g. two `provider "aws" {}` blocks become a
/// single `{ "provider": { "aws": { ... } } }`); a future milestone may
/// surface duplicates explicitly. For M5 this matches the typical
/// Terraform-style usage where labels are unique per identifier.
fn body_to_value(body: &Body) -> Value {
    let mut out: IndexMap<String, Value> = IndexMap::new();
    for structure in body.iter() {
        match structure {
            Structure::Attribute(attr) => {
                let key = attr.key.as_str().to_owned();
                let val = expression_to_value(&attr.expr);
                out.insert(key, val);
            }
            Structure::Block(block) => insert_block(&mut out, block),
        }
    }
    Value::Map(out)
}

/// Insert a block into `parent` honouring the labels-as-keys nesting rule.
///
/// `block_type "a" "b" {}` becomes `parent[block_type] → Map[a] → Map[b] →
/// body_value`. Existing maps at any depth are reused so multi-block sources
/// merge cleanly.
fn insert_block(parent: &mut IndexMap<String, Value>, block: &Block) {
    let body_value = body_to_value(&block.body);
    let identifier = block.identifier.as_str().to_owned();
    if block.labels.is_empty() {
        // Bare block (no labels): merge into existing entry if present, else
        // insert directly.
        merge_into(parent, identifier, body_value);
        return;
    }
    let labels: Vec<String> = block
        .labels
        .iter()
        .map(|l| match l {
            BlockLabel::Identifier(id) => id.as_str().to_owned(),
            BlockLabel::String(s) => s.clone(),
        })
        .collect();
    // Walk into `parent[identifier]`, creating intermediate maps as needed.
    let entry = parent
        .entry(identifier)
        .or_insert_with(|| Value::Map(IndexMap::new()));
    let mut cursor = entry;
    for (i, label) in labels.iter().enumerate() {
        let is_last = i + 1 == labels.len();
        let map = match cursor {
            Value::Map(m) => m,
            // If the existing entry is not a map, replace it. This is a rare
            // edge-case (an earlier attribute clashed with a block label
            // path) — the block wins because blocks are structural.
            other => {
                *other = Value::Map(IndexMap::new());
                let Value::Map(m) = other else {
                    unreachable!("just assigned a Map");
                };
                m
            }
        };
        if is_last {
            merge_into(map, label.clone(), body_value);
            return;
        }
        let next = map
            .entry(label.clone())
            .or_insert_with(|| Value::Map(IndexMap::new()));
        cursor = next;
    }
}

/// Merge `incoming` into `parent[key]`. If `parent[key]` already holds a Map
/// and `incoming` is a Map, their entries are union'd (right-biased on key
/// collision); otherwise `incoming` replaces the previous value.
fn merge_into(parent: &mut IndexMap<String, Value>, key: String, incoming: Value) {
    // Two-pass to satisfy the borrow checker: peek to decide the branch,
    // then take action. `parent.contains_key` is a cheap hash lookup.
    let can_merge = matches!(
        (parent.get(&key), &incoming),
        (Some(Value::Map(_)), Value::Map(_)),
    );
    if can_merge {
        let Some(Value::Map(existing)) = parent.get_mut(&key) else {
            unreachable!("checked above")
        };
        let Value::Map(new_map) = incoming else {
            unreachable!("checked above")
        };
        for (k, v) in new_map {
            existing.insert(k, v);
        }
        return;
    }
    parent.insert(key, incoming);
}

/// Convert an HCL expression into our `Value`.
///
/// Uses the existing `From<Expression> for HclValue` impl from `hcl-rs` then
/// maps `HclValue → Value`. Template-expression nodes (heredocs, `${...}`
/// strings, etc.) are stringified through `HclValue::String` by that impl —
/// our parser inherits that behaviour.
fn expression_to_value(expr: &Expression) -> Value {
    let hcl_v: HclValue = expr.clone().into();
    hcl_value_to_value(hcl_v)
}

fn hcl_value_to_value(v: HclValue) -> Value {
    match v {
        HclValue::Null => Value::Null,
        HclValue::Bool(b) => Value::Bool(b),
        HclValue::Number(n) => hcl_number_to_value(&n),
        HclValue::String(s) => Value::String(s),
        HclValue::Array(items) => Value::Array(items.into_iter().map(hcl_value_to_value).collect()),
        HclValue::Object(map) => {
            let mut out = IndexMap::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k, hcl_value_to_value(v));
            }
            Value::Map(out)
        }
    }
}

/// Map an `hcl_primitives::Number` into our `Value` numeric variants.
///
/// Integer-shaped numbers go to `Value::Int` when they fit `i64`, otherwise
/// `Value::BigInt(stringified)`. Float-shaped numbers go to `Value::Float`;
/// non-finite floats are not produced by the parser (HCL grammar rejects
/// them) so we don't need a `BigFloat` fallback here.
fn hcl_number_to_value(n: &Number) -> Value {
    if let Some(i) = n.as_i64() {
        return Value::Int(i);
    }
    if n.is_u64()
        && let Some(u) = n.as_u64()
    {
        // u64 that doesn't fit i64 — preserve as BigInt literal so callers
        // don't lose precision.
        return Value::BigInt(u.to_string());
    }
    if let Some(f) = n.as_f64() {
        return Value::Float(f);
    }
    // Fallback: stringify the number's textual form. The `Number` type
    // implements Display so this is non-lossy in practice.
    Value::BigFloat(n.to_string())
}

/// Convert our `Value` back into `hcl::Value` for the write path.
///
/// `Value::BigInt` / `Value::BigFloat` are emitted through their textual form
/// as `HclValue::String` (best-effort — the `hcl-rs` Number type cannot
/// carry arbitrary-precision literals). The write path documents this v1
/// limitation in the module comment.
fn value_to_hcl_value(v: &Value) -> HclValue {
    match v {
        Value::Null => HclValue::Null,
        Value::Bool(b) => HclValue::Bool(*b),
        Value::Int(n) => HclValue::Number(Number::from(*n)),
        Value::Float(n) => Number::from_f64(*n).map_or(HclValue::Null, HclValue::Number),
        Value::BigInt(s) | Value::BigFloat(s) => HclValue::String(s.clone()),
        Value::String(s) => HclValue::String(s.clone()),
        Value::Array(items) => HclValue::Array(items.iter().map(value_to_hcl_value).collect()),
        Value::Map(map) => {
            let mut out = hcl::Map::with_capacity(map.len());
            for (k, val) in map {
                out.insert(k.clone(), value_to_hcl_value(val));
            }
            HclValue::Object(out)
        }
    }
}

/// Map an `hcl::Error` into our structured `Error::Parse`.
///
/// The `Parse` variant exposes `Location { line, column }`; other error
/// variants don't carry position info, so we anchor at `0/0` for them.
fn map_hcl_parse_error(e: hcl::Error) -> Error {
    let (line, col, message) = match &e {
        hcl::Error::Parse(parse_err) => {
            let loc = parse_err.location();
            (loc.line() as u32, loc.column() as u32, e.to_string())
        }
        other => (0, 0, other.to_string()),
    };
    Error::Parse {
        file: None,
        line,
        col,
        span: 0..0,
        snippet: String::new(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_attribute() {
        let doc = Hcl.parse(b"key = \"value\"\n").expect("parse simple");
        match doc.value() {
            Value::Map(m) => assert_eq!(m.get("key"), Some(&Value::String("value".into()))),
            other => panic!("expected map, got: {other:?}"),
        }
    }

    #[test]
    fn parse_block_with_label_nests() {
        let doc = Hcl
            .parse(b"backend \"s3\" { region = \"us-east-1\" }\n")
            .expect("parse block");
        let Value::Map(top) = doc.value() else {
            panic!("expected top map")
        };
        let Some(Value::Map(backend)) = top.get("backend") else {
            panic!("expected backend map")
        };
        let Some(Value::Map(s3)) = backend.get("s3") else {
            panic!("expected s3 map")
        };
        assert_eq!(s3.get("region"), Some(&Value::String("us-east-1".into())));
    }

    #[test]
    fn parse_invalid_returns_parse_error() {
        let err = Hcl.parse(b"this = =\n").expect_err("must parse-error");
        assert_eq!(err.kind_name(), "parse");
    }
}
