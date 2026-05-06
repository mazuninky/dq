//! RFC 6902 JSON Patch — [`PatchOp`] enum and [`apply_patch`] engine.
//!
//! The wire format is the standard tagged JSON form:
//!
//! ```json
//! [
//!   {"op": "add",     "path": "/a/b", "value": 1},
//!   {"op": "remove",  "path": "/a/b"},
//!   {"op": "replace", "path": "/a/b", "value": 1},
//!   {"op": "move",    "from": "/a",   "path": "/b"},
//!   {"op": "copy",    "from": "/a",   "path": "/b"},
//!   {"op": "test",    "path": "/a/b", "value": 1}
//! ]
//! ```
//!
//! `path` and `from` are RFC 6901 JSON Pointers; `value` is an arbitrary JSON
//! value that is converted to a [`Value`] preserving the textual literal of
//! arbitrary-precision numbers (matching the read-pat parsers' big-int / big-
//! float behaviour).
//!
//! [`Value`]: crate::Value

use indexmap::IndexMap;
use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::ser::{SerializeStruct, Serializer};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::Result;
use crate::document::{Document, Value};
use crate::error::{Error, PathErrorKind};
use crate::pointer::{Pointer, Segment};

/// A single RFC 6902 JSON Patch operation.
///
/// The variant set is exactly the six operations defined in RFC 6902 §4. Wire
/// serialization matches the standard `{"op": "...", ...}` form via the
/// [`Serialize`] / [`Deserialize`] impls in this module.
#[derive(Debug, Clone, PartialEq)]
pub enum PatchOp {
    /// `{"op":"add","path":"/a/b","value":...}` — insert or replace at `path`.
    ///
    /// In M3's baseline insertion is limited to existing parent containers
    /// (M2's `set_at` returns `MissingKey` for unknown pointers); full mkdir-p
    /// insertion is a post-M3 follow-up.
    Add {
        /// Target pointer.
        path: Pointer,
        /// Value to insert.
        value: Value,
    },
    /// `{"op":"remove","path":"/a/b"}` — delete the value at `path`.
    Remove {
        /// Target pointer.
        path: Pointer,
    },
    /// `{"op":"replace","path":"/a/b","value":...}` — replace the value at `path`.
    ///
    /// Per RFC 6902 §4.3 `replace` requires the target to exist; missing
    /// targets surface as `Error::Path { kind: MissingKey }`.
    Replace {
        /// Target pointer.
        path: Pointer,
        /// Replacement value.
        value: Value,
    },
    /// `{"op":"move","from":"/a","path":"/b"}` — read at `from`, delete at
    /// `from`, set at `path`.
    Move {
        /// Source pointer.
        from: Pointer,
        /// Destination pointer.
        path: Pointer,
    },
    /// `{"op":"copy","from":"/a","path":"/b"}` — read at `from`, set at
    /// `path`. The source is left in place.
    Copy {
        /// Source pointer.
        from: Pointer,
        /// Destination pointer.
        path: Pointer,
    },
    /// `{"op":"test","path":"/a/b","value":...}` — assert the value at `path`
    /// equals `value`. A mismatch aborts the whole patch.
    Test {
        /// Target pointer.
        path: Pointer,
        /// Expected value.
        value: Value,
    },
}

// ----------------------------- serde plumbing ------------------------------

/// Serialize each [`PatchOp`] variant as the RFC 6902 tagged map. The
/// `path` / `from` pointers are emitted as RFC 6901 strings via
/// [`Pointer::as_canonical`]. The `value` field is serialized through
/// [`Value`]'s existing [`Serialize`] impl.
impl Serialize for PatchOp {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Add { path, value } => {
                let mut s = serializer.serialize_struct("PatchOp", 3)?;
                s.serialize_field("op", "add")?;
                s.serialize_field("path", &path.as_canonical())?;
                s.serialize_field("value", value)?;
                s.end()
            }
            Self::Remove { path } => {
                let mut s = serializer.serialize_struct("PatchOp", 2)?;
                s.serialize_field("op", "remove")?;
                s.serialize_field("path", &path.as_canonical())?;
                s.end()
            }
            Self::Replace { path, value } => {
                let mut s = serializer.serialize_struct("PatchOp", 3)?;
                s.serialize_field("op", "replace")?;
                s.serialize_field("path", &path.as_canonical())?;
                s.serialize_field("value", value)?;
                s.end()
            }
            Self::Move { from, path } => {
                let mut s = serializer.serialize_struct("PatchOp", 3)?;
                s.serialize_field("op", "move")?;
                s.serialize_field("from", &from.as_canonical())?;
                s.serialize_field("path", &path.as_canonical())?;
                s.end()
            }
            Self::Copy { from, path } => {
                let mut s = serializer.serialize_struct("PatchOp", 3)?;
                s.serialize_field("op", "copy")?;
                s.serialize_field("from", &from.as_canonical())?;
                s.serialize_field("path", &path.as_canonical())?;
                s.end()
            }
            Self::Test { path, value } => {
                let mut s = serializer.serialize_struct("PatchOp", 3)?;
                s.serialize_field("op", "test")?;
                s.serialize_field("path", &path.as_canonical())?;
                s.serialize_field("value", value)?;
                s.end()
            }
        }
    }
}

/// Custom deserialization that reads the RFC 6902 wire shape and converts the
/// `value` / `from` / `path` fields into our domain types ([`Value`] /
/// [`Pointer`]). The intermediate hop through [`serde_json::Value`] preserves
/// arbitrary-precision numbers via [`number_to_value`].
impl<'de> Deserialize<'de> for PatchOp {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(PatchOpVisitor)
    }
}

struct PatchOpVisitor;

impl<'de> Visitor<'de> for PatchOpVisitor {
    type Value = PatchOp;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an RFC 6902 JSON Patch operation object")
    }

    fn visit_map<M>(self, mut map: M) -> std::result::Result<PatchOp, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut op: Option<String> = None;
        let mut path: Option<String> = None;
        let mut from: Option<String> = None;
        let mut value: Option<serde_json::Value> = None;

        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "op" => {
                    if op.is_some() {
                        return Err(de::Error::duplicate_field("op"));
                    }
                    op = Some(map.next_value()?);
                }
                "path" => {
                    if path.is_some() {
                        return Err(de::Error::duplicate_field("path"));
                    }
                    path = Some(map.next_value()?);
                }
                "from" => {
                    if from.is_some() {
                        return Err(de::Error::duplicate_field("from"));
                    }
                    from = Some(map.next_value()?);
                }
                "value" => {
                    if value.is_some() {
                        return Err(de::Error::duplicate_field("value"));
                    }
                    value = Some(map.next_value()?);
                }
                _ => {
                    // RFC 6902 §4: "The presence of any other member in an
                    // operation object MUST be ignored to allow for future
                    // extensions." We pull the value off the iterator (so the
                    // deserializer's state stays consistent) and discard it
                    // without allocating via `IgnoredAny`.
                    let _: de::IgnoredAny = map.next_value()?;
                }
            }
        }

        let op = op.ok_or_else(|| de::Error::missing_field("op"))?;
        let parse_pointer = |raw: &str| -> std::result::Result<Pointer, M::Error> {
            Pointer::parse(raw).map_err(|e| de::Error::custom(format!("invalid pointer: {e}")))
        };

        match op.as_str() {
            "add" => {
                let path = parse_pointer(&path.ok_or_else(|| de::Error::missing_field("path"))?)?;
                let value = value.ok_or_else(|| de::Error::missing_field("value"))?;
                Ok(PatchOp::Add {
                    path,
                    value: serde_json_to_dq_value(&value),
                })
            }
            "remove" => {
                let path = parse_pointer(&path.ok_or_else(|| de::Error::missing_field("path"))?)?;
                Ok(PatchOp::Remove { path })
            }
            "replace" => {
                let path = parse_pointer(&path.ok_or_else(|| de::Error::missing_field("path"))?)?;
                let value = value.ok_or_else(|| de::Error::missing_field("value"))?;
                Ok(PatchOp::Replace {
                    path,
                    value: serde_json_to_dq_value(&value),
                })
            }
            "move" => {
                let from = parse_pointer(&from.ok_or_else(|| de::Error::missing_field("from"))?)?;
                let path = parse_pointer(&path.ok_or_else(|| de::Error::missing_field("path"))?)?;
                Ok(PatchOp::Move { from, path })
            }
            "copy" => {
                let from = parse_pointer(&from.ok_or_else(|| de::Error::missing_field("from"))?)?;
                let path = parse_pointer(&path.ok_or_else(|| de::Error::missing_field("path"))?)?;
                Ok(PatchOp::Copy { from, path })
            }
            "test" => {
                let path = parse_pointer(&path.ok_or_else(|| de::Error::missing_field("path"))?)?;
                let value = value.ok_or_else(|| de::Error::missing_field("value"))?;
                Ok(PatchOp::Test {
                    path,
                    value: serde_json_to_dq_value(&value),
                })
            }
            other => Err(de::Error::unknown_variant(
                other,
                &["add", "remove", "replace", "move", "copy", "test"],
            )),
        }
    }
}

// --------------------- serde_json -> dq Value bridge -----------------------

// NOTE: this helper duplicates `crates/dq-cli/src/commands/set.rs` for now;
// extracting a shared helper is a deliberately-deferred refactor (see the
// M3 §1 prompt — the duplication is bounded and the shared module is not
// yet load-bearing).

/// Convert a [`serde_json::Value`] into a [`Value`], preserving big-int and
/// big-float literals.
fn serde_json_to_dq_value(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => number_to_value(n),
        serde_json::Value::String(s) => Value::String(s.clone()),
        serde_json::Value::Array(items) => {
            Value::Array(items.iter().map(serde_json_to_dq_value).collect())
        }
        serde_json::Value::Object(map) => {
            let mut out = IndexMap::with_capacity(map.len());
            for (k, child) in map {
                out.insert(k.clone(), serde_json_to_dq_value(child));
            }
            Value::Map(out)
        }
    }
}

fn number_to_value(n: &serde_json::Number) -> Value {
    // Mirror `dq_cli::commands::set::number_to_value`: with serde_json's
    // arbitrary_precision feature on, `n.to_string()` returns the original
    // textual literal verbatim, which lets us pick the right precision-
    // preserving variant rather than collapsing through `as_i64`/`as_f64`.
    let literal = n.to_string();
    if let Ok(i) = literal.parse::<i64>() {
        return Value::Int(i);
    }
    if literal.contains('.') || literal.contains('e') || literal.contains('E') {
        if let Ok(f) = f64::from_str(&literal)
            && f.is_finite()
            && literal_round_trips_to(&literal, f)
        {
            return Value::Float(f);
        }
        return Value::BigFloat(literal);
    }
    Value::BigInt(literal)
}

fn literal_round_trips_to(literal: &str, f: f64) -> bool {
    f64::from_str(literal).is_ok_and(|parsed| parsed.to_bits() == f.to_bits())
}

// ------------------------------- engine -----------------------------------

/// Apply an RFC 6902 patch to `doc` atomically.
///
/// The function clones `doc` once, applies every op to the clone in order,
/// and only commits the result back into `doc` on success. Any error during
/// any op (including a failing `test`) leaves `doc` byte-identical to its
/// pre-call state — see [Decision D2 in the M3 design][D2].
///
/// # Errors
///
/// - [`Error::PatchTestFailed`] when a `test` op observes a mismatched value.
/// - [`Error::Path`] for missing pointers, type mismatches, or out-of-bounds
///   indices encountered during any op.
/// - [`Error::WriteUnavailable`] when the document was loaded read-only.
///
/// [D2]: ../../../openspec/changes/add-bulk-and-ci/design.md
pub fn apply_patch(doc: &mut Document, ops: &[PatchOp]) -> Result<()> {
    let mut working = doc.clone();
    for op in ops {
        apply_one(&mut working, op)?;
    }
    *doc = working;
    Ok(())
}

fn apply_one(doc: &mut Document, op: &PatchOp) -> Result<()> {
    match op {
        PatchOp::Add { path, value } => doc.set_at(path, value.clone()),
        PatchOp::Remove { path } => doc.del_at(path),
        PatchOp::Replace { path, value } => {
            // RFC 6902 §4.3: target MUST exist. M2's `set_at` already returns
            // MissingKey for unknown pointers, so a pre-check via
            // `read_value_at` would be redundant — except `read_value_at` also
            // catches the case where the pointer addresses the inside of a
            // scalar (a TypeMismatch we'd otherwise surface from `set_at`'s
            // walk). Reading first costs ~one extra walk per op but unifies
            // the error shape across all five non-scalar variants.
            read_value_at(doc, path)?;
            doc.set_at(path, value.clone())
        }
        PatchOp::Move { from, path } => {
            let v = read_value_at(doc, from)?;
            doc.del_at(from)?;
            doc.set_at(path, v)
        }
        PatchOp::Copy { from, path } => {
            let v = read_value_at(doc, from)?;
            doc.set_at(path, v)
        }
        PatchOp::Test { path, value } => {
            let actual = read_value_at(doc, path)?;
            if &actual != value {
                return Err(Error::PatchTestFailed {
                    pointer: path.as_canonical(),
                    expected: Box::new(value.clone()),
                    actual: Box::new(actual),
                });
            }
            Ok(())
        }
    }
}

/// Walk `doc.value()` to the value at `pointer`, returning a clone.
///
/// Distinct from `Pointer::resolve` because `resolve` borrows from `Value`,
/// which we cannot return through `&Document` without lifetime gymnastics in
/// the patch engine. Cloning the addressed value is fine: it happens once per
/// `test` / `move` / `copy` op, and the values are small at the leaf.
pub(super) fn read_value_at(doc: &Document, pointer: &Pointer) -> Result<Value> {
    let mut current: &Value = doc.value();
    let mut matched: Vec<Segment> = Vec::new();
    for seg in pointer.segments() {
        match (current, seg) {
            (Value::Map(map), Segment::Key(k)) => match map.get(k) {
                Some(v) => {
                    current = v;
                    matched.push(Segment::Key(k.clone()));
                }
                None => {
                    return Err(Error::Path {
                        pointer: pointer.as_canonical(),
                        matched_prefix: Pointer::new(matched).as_canonical(),
                        kind: PathErrorKind::MissingKey,
                        did_you_mean: Vec::new(),
                    });
                }
            },
            (Value::Array(items), Segment::Index(i)) => match items.get(*i) {
                Some(v) => {
                    current = v;
                    matched.push(Segment::Index(*i));
                }
                None => {
                    return Err(Error::Path {
                        pointer: pointer.as_canonical(),
                        matched_prefix: Pointer::new(matched).as_canonical(),
                        kind: PathErrorKind::OutOfBounds,
                        did_you_mean: Vec::new(),
                    });
                }
            },
            (Value::Array(items), Segment::Key(k)) => {
                // Numeric keys parsed as `Segment::Key` (RFC 6901 default) —
                // coerce when the container is an array.
                if let Ok(idx) = k.parse::<usize>() {
                    match items.get(idx) {
                        Some(v) => {
                            current = v;
                            matched.push(Segment::Index(idx));
                            continue;
                        }
                        None => {
                            return Err(Error::Path {
                                pointer: pointer.as_canonical(),
                                matched_prefix: Pointer::new(matched).as_canonical(),
                                kind: PathErrorKind::OutOfBounds,
                                did_you_mean: Vec::new(),
                            });
                        }
                    }
                }
                return Err(Error::Path {
                    pointer: pointer.as_canonical(),
                    matched_prefix: Pointer::new(matched).as_canonical(),
                    kind: PathErrorKind::TypeMismatch {
                        expected: "array index",
                        found: "non-numeric key",
                    },
                    did_you_mean: Vec::new(),
                });
            }
            (other, _) => {
                return Err(Error::Path {
                    pointer: pointer.as_canonical(),
                    matched_prefix: Pointer::new(matched).as_canonical(),
                    kind: PathErrorKind::TypeMismatch {
                        expected: "object or array",
                        found: other.type_name(),
                    },
                    did_you_mean: Vec::new(),
                });
            }
        }
    }
    Ok(current.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{FormatTag, SpanContext, SpanMap, ValueSpan};
    use indexmap::IndexMap;
    use pretty_assertions::assert_eq;

    /// Build a single-line block-mapping span — same shape used by the
    /// `document::tests` helpers.
    fn block_map_span(
        value_start: usize,
        value_end: usize,
        line_start: usize,
        line_end: usize,
    ) -> ValueSpan {
        ValueSpan {
            value_range: value_start..value_end,
            line_range: line_start..line_end,
            indent: 0,
            context: SpanContext::BlockMapValue,
        }
    }

    fn map_one(key: &str, value: Value) -> Value {
        let mut m = IndexMap::new();
        m.insert(key.to_owned(), value);
        Value::Map(m)
    }

    /// Build a write-aware YAML document with `key: <int>\n` shape.
    fn yaml_doc_one(key: &str, n: i64) -> Document {
        let bytes = format!("{key}: {n}\n").into_bytes();
        let value_start = key.len() + 2;
        let value_end = bytes.len() - 1;
        let mut spans = SpanMap::new();
        spans.insert(
            format!("/{key}"),
            block_map_span(value_start, value_end, 0, bytes.len()),
        );
        Document::with_spans(map_one(key, Value::Int(n)), bytes, spans, FormatTag::Yaml)
    }

    // ---- serde wire-format round-trip ----

    #[test]
    fn deserialize_six_op_kinds() {
        let raw = r#"[
            {"op":"add","path":"/a","value":1},
            {"op":"remove","path":"/a"},
            {"op":"replace","path":"/a","value":2},
            {"op":"move","from":"/a","path":"/b"},
            {"op":"copy","from":"/a","path":"/b"},
            {"op":"test","path":"/a","value":1}
        ]"#;
        let ops: Vec<PatchOp> = serde_json::from_str(raw).expect("RFC 6902 wire form parses");
        assert_eq!(ops.len(), 6);
        assert!(matches!(ops[0], PatchOp::Add { .. }));
        assert!(matches!(ops[1], PatchOp::Remove { .. }));
        assert!(matches!(ops[2], PatchOp::Replace { .. }));
        assert!(matches!(ops[3], PatchOp::Move { .. }));
        assert!(matches!(ops[4], PatchOp::Copy { .. }));
        assert!(matches!(ops[5], PatchOp::Test { .. }));
    }

    #[test]
    fn serialize_round_trip_via_serde_json() {
        // Serializing then deserializing each variant must produce an
        // equivalent op (PartialEq) — pinning the wire-format contract from
        // both directions in one test.
        let ops = vec![
            PatchOp::Add {
                path: Pointer::parse("/a").unwrap(),
                value: Value::Int(1),
            },
            PatchOp::Remove {
                path: Pointer::parse("/a").unwrap(),
            },
            PatchOp::Test {
                path: Pointer::parse("/a").unwrap(),
                value: Value::Bool(true),
            },
        ];
        let json = serde_json::to_string(&ops).unwrap();
        let round_tripped: Vec<PatchOp> = serde_json::from_str(&json).unwrap();
        assert_eq!(ops, round_tripped);
    }

    // ---- apply_patch happy paths ----

    #[test]
    fn apply_patch_replace_succeeds() {
        let mut doc = yaml_doc_one("a", 3);
        let ops = vec![PatchOp::Replace {
            path: Pointer::parse("/a").unwrap(),
            value: Value::Int(5),
        }];
        apply_patch(&mut doc, &ops).expect("replace succeeds");
        assert_eq!(doc.original_bytes(), b"a: 5\n");
        match doc.value() {
            Value::Map(m) => assert_eq!(m.get("a"), Some(&Value::Int(5))),
            other => panic!("expected map, got {other:?}"),
        }
    }

    #[test]
    fn apply_patch_remove_succeeds() {
        let mut doc = yaml_doc_one("a", 3);
        let ops = vec![PatchOp::Remove {
            path: Pointer::parse("/a").unwrap(),
        }];
        apply_patch(&mut doc, &ops).expect("remove succeeds");
        assert_eq!(doc.original_bytes(), b"");
        match doc.value() {
            Value::Map(m) => assert!(m.is_empty()),
            other => panic!("expected empty map, got {other:?}"),
        }
    }

    // ---- apply_patch atomicity on test failure ----

    #[test]
    fn apply_patch_test_failure_rolls_back() {
        // Two-op patch: a successful replace followed by a failing test must
        // leave the document byte-identical to its pre-call state. This is
        // the central RFC 6902 §5 contract.
        let mut doc = yaml_doc_one("a", 3);
        let original_bytes = doc.original_bytes().to_vec();
        let ops = vec![
            PatchOp::Replace {
                path: Pointer::parse("/a").unwrap(),
                value: Value::Int(5),
            },
            PatchOp::Test {
                path: Pointer::parse("/a").unwrap(),
                // Wrong expected — after the replace, /a is 5, not 99.
                value: Value::Int(99),
            },
        ];
        let err = apply_patch(&mut doc, &ops).unwrap_err();
        match err {
            Error::PatchTestFailed {
                pointer,
                expected,
                actual,
            } => {
                assert_eq!(pointer, "/a");
                assert_eq!(*expected, Value::Int(99));
                assert_eq!(*actual, Value::Int(5));
            }
            other => panic!("expected PatchTestFailed, got {other:?}"),
        }
        // Document must be byte-identical to its pre-call state.
        assert_eq!(
            doc.original_bytes(),
            original_bytes.as_slice(),
            "atomicity contract: failed patch must not mutate original_bytes",
        );
    }

    #[test]
    fn apply_patch_test_success_passes_through() {
        // A passing `test` op is a no-op; it must not mutate the document.
        let mut doc = yaml_doc_one("a", 3);
        let ops = vec![PatchOp::Test {
            path: Pointer::parse("/a").unwrap(),
            value: Value::Int(3),
        }];
        apply_patch(&mut doc, &ops).expect("matching test op passes");
        assert_eq!(doc.original_bytes(), b"a: 3\n");
    }

    // ---- read_value_at ----

    #[test]
    fn read_value_at_missing_key_surfaces_path_error() {
        let doc = yaml_doc_one("a", 3);
        let err = read_value_at(&doc, &Pointer::parse("/missing").unwrap()).unwrap_err();
        match err {
            Error::Path { kind, .. } => assert_eq!(kind, PathErrorKind::MissingKey),
            other => panic!("expected Path/MissingKey, got {other:?}"),
        }
    }

    // ---- replace on missing key returns MissingKey (and doc is untouched) ----

    #[test]
    fn deserialize_ignores_unknown_fields_per_rfc_6902() {
        // RFC 6902 §4 explicitly requires unknown members of an operation
        // object to be ignored, leaving room for future extensions and
        // implementation-specific annotations. Pre-fix: the deserializer
        // returned `unknown_field` and rejected the patch outright.
        let json = r#"[{"op":"replace","path":"/x","value":1,"comment":"audit","_meta":{"author":"alice"}}]"#;
        let ops: Vec<PatchOp> = serde_json::from_str(json).expect(
            "RFC 6902 deserializer must silently ignore unknown fields like 'comment' / '_meta'",
        );
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            PatchOp::Replace { path, value } => {
                assert_eq!(path.as_canonical(), "/x");
                assert_eq!(*value, Value::Int(1));
            }
            other => panic!("expected Replace, got: {other:?}"),
        }
    }

    #[test]
    fn apply_patch_replace_missing_key_does_not_mutate() {
        // M2 baseline: `set_at` returns MissingKey for unknown pointers; the
        // patch engine inherits that. The clone-on-apply contract (D2) means
        // the original document stays byte-identical.
        let mut doc = yaml_doc_one("a", 3);
        let original = doc.original_bytes().to_vec();
        let ops = vec![PatchOp::Replace {
            path: Pointer::parse("/missing").unwrap(),
            value: Value::Int(5),
        }];
        let err = apply_patch(&mut doc, &ops).unwrap_err();
        match err {
            Error::Path { kind, .. } => assert_eq!(kind, PathErrorKind::MissingKey),
            other => panic!("expected Path/MissingKey, got {other:?}"),
        }
        assert_eq!(doc.original_bytes(), original.as_slice());
    }
}
