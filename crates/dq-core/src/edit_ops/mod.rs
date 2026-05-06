//! Edit-op vocabulary for the `Fixer` runtime and WASM plugins.
//!
//! [`EditOp`] / [`EditScript`] are a strict RFC 6902 JSON Patch *subset*
//! limited to the three operations that fix-engines need: `add`, `replace`,
//! and `remove`. The wire format mirrors RFC 6902 exactly so any tool that
//! consumes JSON Patch can produce or consume an [`EditScript`]:
//!
//! ```json
//! [
//!   {"op": "add",     "path": "/x",  "value": 42},
//!   {"op": "replace", "path": "/y",  "value": 7},
//!   {"op": "remove",  "path": "/z"}
//! ]
//! ```
//!
//! # Relationship to [`PatchOp`]
//!
//! This crate already exposes [`PatchOp`] (in [`crate::transform::patch`]) for
//! the user-facing `dq patch` engine, which implements the full RFC 6902
//! op-set including `move`/`copy`/`test`. [`EditOp`] is a *deliberately*
//! separate type that:
//!
//! - **Is stricter than `PatchOp`**: deserialization rejects unknown fields
//!   under `deny_unknown_fields`, whereas `PatchOp` ignores them per
//!   RFC 6902 §4 to permit forward-compatible extensions (audit annotations,
//!   etc.) on user input. `EditOp` is produced by typed callers (rule
//!   engines, plugins) where unknown fields signal a bug, not extension.
//! - **Excludes `move`/`copy`/`test`**: these are not needed for the M11
//!   per-violation fix scenario and would require redundant atomicity
//!   plumbing in the apply path.
//!
//! Convergence between the two types is left to a future change. They
//! coexist as separate types per design decision D3 in
//! `openspec/changes/add-ir-foundation/design.md`.
//!
//! # Layering on top of `Document::set_at` / `del_at`
//!
//! [`EditScript::apply`] is a vocabulary *layer over* [`Document::set_at`]
//! and [`Document::del_at`]; those primitives already drive the per-format
//! [`ScalarRenderer`] / [`InsertionRenderer`] machinery that preserves
//! comments and surrounding whitespace, regenerate the [`SpanMap`], and
//! refresh the provenance side-channel. Refactoring `set_at` / `del_at` to
//! delegate to `EditScript::apply` of a single op would create a recursive
//! call loop (`set_at` → `EditScript::apply` → `set_at`) and was
//! deliberately not done in Phase 3 — `EditScript::apply` builds *on top of*
//! these primitives, not the other way around.
//!
//! [`PatchOp`]: crate::transform::PatchOp
//! [`Document::set_at`]: crate::document::Document::set_at
//! [`Document::del_at`]: crate::document::Document::del_at
//! [`ScalarRenderer`]: crate::textual_edit
//! [`InsertionRenderer`]: crate::textual_edit
//! [`SpanMap`]: crate::document

use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::ser::{SerializeStruct, Serializer};
use serde::{Deserialize, Serialize};
use std::fmt;

use crate::Result;
use crate::document::{Document, Value};
use crate::pointer::Pointer;

/// A single fix-vocabulary edit operation.
///
/// Subset of RFC 6902 — only `add`, `replace`, and `remove` are modelled.
/// `move`, `copy`, and `test` live exclusively on [`PatchOp`] (the
/// `dq patch` engine). See the module-level docs for the rationale.
///
/// [`PatchOp`]: crate::transform::PatchOp
#[derive(Debug, Clone, PartialEq)]
pub enum EditOp {
    /// `{"op":"add","path":"/a/b","value":...}` — insert (or replace) the
    /// value at `path`. The Phase 3 baseline uses [`Document::set_at`]
    /// underneath; mkdir-p semantics for paths whose parent does not exist
    /// are inherited from `set_at` (currently surfaces as
    /// [`crate::Error::Path`] / `MissingKey`).
    ///
    /// [`Document::set_at`]: crate::document::Document::set_at
    Add {
        /// Target pointer.
        path: Pointer,
        /// Value to insert.
        value: Value,
    },
    /// `{"op":"replace","path":"/a/b","value":...}` — replace the value at
    /// `path`. Inherits the same `MissingKey` behaviour as
    /// [`Document::set_at`] when `path` does not exist.
    ///
    /// [`Document::set_at`]: crate::document::Document::set_at
    Replace {
        /// Target pointer.
        path: Pointer,
        /// Replacement value.
        value: Value,
    },
    /// `{"op":"remove","path":"/a/b"}` — delete the value at `path` via
    /// [`Document::del_at`].
    ///
    /// [`Document::del_at`]: crate::document::Document::del_at
    Remove {
        /// Target pointer.
        path: Pointer,
    },
}

/// An ordered sequence of [`EditOp`]s.
///
/// Apply via [`EditScript::apply`]; serialize to / deserialize from a JSON
/// Patch array via the [`Serialize`] / [`Deserialize`] impls in this module.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EditScript(Vec<EditOp>);

impl EditScript {
    /// Construct an empty script. Equivalent to `EditScript::default()`.
    #[must_use]
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// Append `op` to the end of the script.
    pub fn push(&mut self, op: EditOp) {
        self.0.push(op);
    }

    /// Borrow the underlying op slice.
    #[must_use]
    pub fn ops(&self) -> &[EditOp] {
        &self.0
    }

    /// Returns true when the script holds no ops.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Number of ops in the script.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Idempotency-check helper for use at `Fixer` call sites.
    ///
    /// Returns `true` iff [`Self::ops`] is empty. This is a *syntactic*
    /// check — a script of `[Replace /x 0]` against a document where `/x`
    /// already equals `0` is **not** considered a no-op by this method.
    /// Callers wanting byte-equality after apply must compare
    /// [`Document::original_bytes`] before and after themselves.
    ///
    /// [`Document::original_bytes`]: crate::document::Document::original_bytes
    #[must_use]
    pub fn is_noop(&self) -> bool {
        self.0.is_empty()
    }

    /// Apply every op to `doc` in declaration order.
    ///
    /// Each op is dispatched to [`Document::set_at`] (for [`EditOp::Add`] /
    /// [`EditOp::Replace`]) or [`Document::del_at`] (for
    /// [`EditOp::Remove`]). Each individual op is atomic; the *script* is
    /// not.
    ///
    /// # Errors
    ///
    /// Returns the first op's [`crate::Error`] on failure. **The document is
    /// left in a partially-applied state** — every op before the failing one
    /// has already been committed to `original_bytes`, the in-memory tree,
    /// the [`SpanMap`], and the provenance side-channel.
    ///
    /// Callers wanting all-or-nothing semantics for a multi-op script must
    /// clone the document before invoking `apply`. This is exactly what
    /// `Fixer::apply` does in Phase 4 of the IR-foundation change.
    ///
    /// [`Document::set_at`]: crate::document::Document::set_at
    /// [`Document::del_at`]: crate::document::Document::del_at
    /// [`SpanMap`]: crate::document
    pub fn apply(&self, doc: &mut Document) -> Result<()> {
        for op in &self.0 {
            match op {
                EditOp::Add { path, value } | EditOp::Replace { path, value } => {
                    doc.set_at(path, value.clone())?;
                }
                EditOp::Remove { path } => {
                    doc.del_at(path)?;
                }
            }
        }
        Ok(())
    }
}

impl From<Vec<EditOp>> for EditScript {
    fn from(ops: Vec<EditOp>) -> Self {
        Self(ops)
    }
}

impl IntoIterator for EditScript {
    type Item = EditOp;
    type IntoIter = std::vec::IntoIter<EditOp>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl FromIterator<EditOp> for EditScript {
    fn from_iter<T: IntoIterator<Item = EditOp>>(iter: T) -> Self {
        Self(iter.into_iter().collect())
    }
}

// ----------------------------- serde plumbing ------------------------------

/// Emit each [`EditOp`] variant as the standard RFC 6902 tagged map. The
/// `path` pointer is rendered as an RFC 6901 string via
/// [`Pointer::as_canonical`]; the `value` field is serialized through
/// [`Value`]'s existing [`Serialize`] impl.
impl Serialize for EditOp {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Add { path, value } => {
                let mut s = serializer.serialize_struct("EditOp", 3)?;
                s.serialize_field("op", "add")?;
                s.serialize_field("path", &path.as_canonical())?;
                s.serialize_field("value", value)?;
                s.end()
            }
            Self::Replace { path, value } => {
                let mut s = serializer.serialize_struct("EditOp", 3)?;
                s.serialize_field("op", "replace")?;
                s.serialize_field("path", &path.as_canonical())?;
                s.serialize_field("value", value)?;
                s.end()
            }
            Self::Remove { path } => {
                let mut s = serializer.serialize_struct("EditOp", 2)?;
                s.serialize_field("op", "remove")?;
                s.serialize_field("path", &path.as_canonical())?;
                s.end()
            }
        }
    }
}

/// Deserialize the RFC 6902 tagged-map shape into an [`EditOp`].
///
/// Stricter than the [`PatchOp`] deserializer: unknown fields produce an
/// error (per `deny_unknown_fields` in the spec), and the only accepted ops
/// are `add` / `replace` / `remove`. Unsupported ops like `copy` / `move` /
/// `test` surface a custom error message that *names the unsupported op* so
/// users see "unsupported op `copy`" rather than a generic
/// `unknown_variant`.
///
/// Numbers in the `value` field round-trip through
/// [`Value::from_serde_json`] so big-int / big-float literals are preserved
/// — same path used by the [`PatchOp`] deserializer.
///
/// [`PatchOp`]: crate::transform::PatchOp
impl<'de> Deserialize<'de> for EditOp {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(EditOpVisitor)
    }
}

struct EditOpVisitor;

impl<'de> Visitor<'de> for EditOpVisitor {
    type Value = EditOp;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .write_str("a JSON Patch (RFC 6902) operation object with op = add | replace | remove")
    }

    fn visit_map<M>(self, mut map: M) -> std::result::Result<EditOp, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut op: Option<String> = None;
        let mut path: Option<String> = None;
        let mut value: Option<serde_json::Value> = None;
        // `from` is the RFC 6902 field used by `copy` / `move`. It is *not*
        // valid on `EditOp`'s subset (`add` / `replace` / `remove`), but we
        // accept it through the loop so that the post-loop op dispatch can
        // surface the spec-mandated `unsupported op` error when it appears
        // alongside `op: copy` / `op: move`. For legitimate ops we re-enforce
        // `deny_unknown_fields` after dispatch by rejecting `from` explicitly.
        let mut from_seen = false;

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
                "value" => {
                    if value.is_some() {
                        return Err(de::Error::duplicate_field("value"));
                    }
                    value = Some(map.next_value()?);
                }
                "from" => {
                    // Consume the value to keep the map iterator advancing;
                    // the actual rejection happens in the post-loop dispatch
                    // so the error names `copy` / `move` rather than `from`.
                    let _: serde::de::IgnoredAny = map.next_value()?;
                    from_seen = true;
                }
                other => {
                    // `deny_unknown_fields` semantics for genuinely unknown
                    // keys — distinguishes `EditOp` from the looser `PatchOp`
                    // (which ignores extras per RFC 6902 §4).
                    return Err(de::Error::unknown_field(other, &["op", "path", "value"]));
                }
            }
        }

        let op = op.ok_or_else(|| de::Error::missing_field("op"))?;
        let parse_pointer = |raw: &str| -> std::result::Result<Pointer, M::Error> {
            Pointer::parse(raw).map_err(|e| de::Error::custom(format!("invalid pointer: {e}")))
        };

        match op.as_str() {
            "add" => {
                if from_seen {
                    return Err(de::Error::unknown_field("from", &["op", "path", "value"]));
                }
                let path = parse_pointer(&path.ok_or_else(|| de::Error::missing_field("path"))?)?;
                let value = value.ok_or_else(|| de::Error::missing_field("value"))?;
                Ok(EditOp::Add {
                    path,
                    value: Value::from_serde_json(&value),
                })
            }
            "replace" => {
                if from_seen {
                    return Err(de::Error::unknown_field("from", &["op", "path", "value"]));
                }
                let path = parse_pointer(&path.ok_or_else(|| de::Error::missing_field("path"))?)?;
                let value = value.ok_or_else(|| de::Error::missing_field("value"))?;
                Ok(EditOp::Replace {
                    path,
                    value: Value::from_serde_json(&value),
                })
            }
            "remove" => {
                if from_seen {
                    return Err(de::Error::unknown_field("from", &["op", "path", "value"]));
                }
                let path = parse_pointer(&path.ok_or_else(|| de::Error::missing_field("path"))?)?;
                Ok(EditOp::Remove { path })
            }
            other => Err(de::Error::custom(format!(
                "unsupported op `{other}`; only add/replace/remove are accepted"
            ))),
        }
    }
}

/// Serialize an [`EditScript`] as a flat JSON array of op objects.
impl Serialize for EditScript {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

/// Deserialize an [`EditScript`] from a JSON array of op objects.
impl<'de> Deserialize<'de> for EditScript {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let ops = Vec::<EditOp>::deserialize(deserializer)?;
        Ok(Self(ops))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;
    use crate::document::{FormatTag, SpanContext, SpanMap, ValueSpan};
    use crate::error::PathErrorKind;
    use indexmap::IndexMap;
    use pretty_assertions::assert_eq;

    /// Build a single-line block-mapping `ValueSpan` covering
    /// `[value_start..value_end)` for the value and
    /// `[line_start..line_end)` for the physical line. Mirrors the helper
    /// in `document::tests` so the in-memory fixture matches the shape a
    /// real YAML parser would produce.
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

    /// Build a write-aware YAML document with shape `key: <int>\n` and a
    /// populated `SpanMap` for `/key`.
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

    /// Build a write-aware YAML doc with two single-byte int keys
    /// `{ka: na, kb: nb}\n` and a populated `SpanMap` for both. The value
    /// for each key is restricted to a single ASCII digit so the byte
    /// arithmetic for `value_range` is unambiguous (no surprise multibyte
    /// shifts) — the renderer's promotion rules only kick in for
    /// width-changing replacements, which the multi-op tests below
    /// exercise explicitly.
    fn yaml_doc_two(ka: &str, na: i64, kb: &str, nb: i64) -> Document {
        assert!(
            (0..=9).contains(&na) && (0..=9).contains(&nb),
            "yaml_doc_two only supports single-digit values: got na={na}, nb={nb}",
        );
        let line_a = format!("{ka}: {na}\n");
        let line_b = format!("{kb}: {nb}\n");
        let bytes = format!("{line_a}{line_b}").into_bytes();
        let a_value_start = ka.len() + 2;
        let a_value_end = a_value_start + 1;
        let a_line_end = line_a.len();
        let b_value_start = a_line_end + kb.len() + 2;
        let b_value_end = b_value_start + 1;
        let b_line_end = bytes.len();
        let mut spans = SpanMap::new();
        spans.insert(
            format!("/{ka}"),
            block_map_span(a_value_start, a_value_end, 0, a_line_end),
        );
        spans.insert(
            format!("/{kb}"),
            block_map_span(b_value_start, b_value_end, a_line_end, b_line_end),
        );
        let mut value = IndexMap::new();
        value.insert(ka.to_owned(), Value::Int(na));
        value.insert(kb.to_owned(), Value::Int(nb));
        Document::with_spans(Value::Map(value), bytes, spans, FormatTag::Yaml)
    }

    #[test]
    fn new_script_is_empty_and_noop() {
        // Pin both `is_empty` and `is_noop` to the empty case — the spec's
        // "Empty script is noop" scenario.
        let s = EditScript::new();
        assert!(s.is_empty());
        assert!(s.is_noop());
        assert_eq!(s.len(), 0);
        assert!(s.ops().is_empty());
    }

    #[test]
    fn serde_round_trip_two_op_script() {
        // A two-op Replace + Remove script must serialize to the canonical
        // RFC 6902 array shape and deserialize back to a structurally
        // identical script.
        let script = EditScript::from(vec![
            EditOp::Replace {
                path: Pointer::parse("/a/b").unwrap(),
                value: Value::Int(1),
            },
            EditOp::Remove {
                path: Pointer::parse("/c").unwrap(),
            },
        ]);
        let json = serde_json::to_value(&script).expect("script serializes");
        // Pin the wire shape: array of two tagged objects.
        assert_eq!(
            json,
            serde_json::json!([
                {"op": "replace", "path": "/a/b", "value": 1},
                {"op": "remove",  "path": "/c"},
            ]),
            "wire format must match RFC 6902 JSON Patch exactly",
        );
        let round_tripped: EditScript =
            serde_json::from_value(json).expect("script deserializes back");
        assert_eq!(round_tripped, script);
    }

    #[test]
    fn apply_replace_patches_bytes_and_value() {
        // Happy-path apply on a write-aware YAML doc: a single Replace
        // must update both the in-memory tree and the source bytes via
        // the registered YAML scalar renderer (mirroring
        // `document::tests::set_at_in_span_replaces_value_bytes_with_yaml_renderer`).
        let mut doc = yaml_doc_one("a", 3);
        let script = EditScript::from(vec![EditOp::Replace {
            path: Pointer::parse("/a").unwrap(),
            value: Value::Int(5),
        }]);
        script
            .apply(&mut doc)
            .expect("Replace via EditScript apply");
        assert_eq!(
            doc.original_bytes(),
            b"a: 5\n",
            "Replace must splice exactly the value range, leaving every other byte untouched",
        );
        match doc.value() {
            Value::Map(m) => assert_eq!(m.get("a"), Some(&Value::Int(5))),
            other => panic!("expected map, got: {other:?}"),
        }
    }

    // ----------------------- spec-anchored unit tests ------------------------

    /// Spec scenario "Round-trip via `IntoIterator` / `FromIterator`" — taking
    /// an EditScript apart through `into_iter` and re-collecting through
    /// `FromIterator` SHALL yield an equivalent script. Exercises both the
    /// `IntoIterator<Item = EditOp>` and `FromIterator<EditOp>` impls in one
    /// shot to pin the round-trip contract.
    #[test]
    fn into_iter_collect_round_trip_yields_identical_script() {
        let original = EditScript::from(vec![
            EditOp::Add {
                path: Pointer::parse("/a").unwrap(),
                value: Value::Bool(true),
            },
            EditOp::Replace {
                path: Pointer::parse("/b").unwrap(),
                value: Value::Int(42),
            },
            EditOp::Remove {
                path: Pointer::parse("/c").unwrap(),
            },
        ]);
        // Snapshot ops slice for the post-collect equality check; can't
        // borrow `original` after `into_iter`.
        let original_ops = original.ops().to_vec();
        let collected: EditScript = original.into_iter().collect();
        assert_eq!(
            collected.ops(),
            original_ops.as_slice(),
            "FromIterator<EditOp> + IntoIterator must round-trip the op sequence in order",
        );
    }

    /// Spec scenario "Serialize replace op as JSON Patch" — the wire form for
    /// a single Replace op must be the exact RFC 6902 tagged map. This test
    /// asserts on `serde_json::to_value(&op)` (not just round-trip) so any
    /// future serializer drift gets caught at the JSON-shape level.
    #[test]
    fn serialize_replace_op_emits_canonical_json_patch_object() {
        let op = EditOp::Replace {
            path: Pointer::parse("/a/b").unwrap(),
            value: Value::Int(1),
        };
        let json = serde_json::to_value(&op).expect("Replace op serializes");
        assert_eq!(
            json,
            serde_json::json!({"op": "replace", "path": "/a/b", "value": 1}),
            "single-op wire form must match RFC 6902 tagged map exactly",
        );
    }

    /// Spec scenario "Deserialize JSON Patch array as EditScript" — parsing
    /// the literal RFC 6902 array MUST produce a script of two ops in
    /// declaration order: `Add` then `Remove`. Pins both the array-level
    /// deserialization path and the per-variant tagging contract.
    #[test]
    fn deserialize_json_patch_array_yields_add_then_remove() {
        let raw = r#"[{"op":"add","path":"/x","value":42},{"op":"remove","path":"/y"}]"#;
        let script: EditScript =
            serde_json::from_str(raw).expect("RFC 6902 array deserializes into EditScript");
        assert_eq!(script.len(), 2, "two-op array must round-trip to two ops");
        match &script.ops()[0] {
            EditOp::Add { path, value } => {
                assert_eq!(path.as_canonical(), "/x");
                assert_eq!(*value, Value::Int(42));
            }
            other => panic!("expected ops[0] = Add, got: {other:?}"),
        }
        match &script.ops()[1] {
            EditOp::Remove { path } => assert_eq!(path.as_canonical(), "/y"),
            other => panic!("expected ops[1] = Remove, got: {other:?}"),
        }
    }

    /// Spec scenario "Unsupported op fails with structured error" — `copy` /
    /// `move` / `test` must be rejected with an error message that names the
    /// unsupported op. Uses the literal spec input
    /// `{"op":"copy","from":"/a","path":"/b"}`: the visitor accepts `from`
    /// through the map loop and dispatches on `op` so the error names `copy`
    /// rather than reporting `from` as an unknown field.
    #[test]
    fn deserialize_copy_op_reports_unsupported_op_by_name() {
        let raw = r#"[{"op":"copy","from":"/a","path":"/b"}]"#;
        let err = serde_json::from_str::<EditScript>(raw)
            .expect_err("`copy` must be rejected by the EditOp deserializer");
        let msg = err.to_string();
        assert!(
            msg.contains("copy"),
            "error message must name the unsupported op `copy`; got: {msg}",
        );
        assert!(
            msg.contains("unsupported op"),
            "error message must declare the failure as an unsupported op; got: {msg}",
        );
    }

    /// `move` is unsupported per the spec, with the same op-name-in-message
    /// contract as `copy`. Pinned separately so a regression that whitelists
    /// only `copy` (or `test`) still fails here.
    #[test]
    fn deserialize_move_op_is_rejected_with_unsupported_op_name() {
        let raw = r#"[{"op":"move","path":"/b"}]"#;
        let err = serde_json::from_str::<EditScript>(raw).expect_err("`move` must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("move"),
            "error message must name the unsupported op `move`; got: {msg}",
        );
    }

    /// `test` is unsupported per the spec — third of the RFC 6902 ops not in
    /// the EditOp subset. Same contract as `copy` / `move`. The wire form
    /// for `test` always carries `value`, so the iteration through the
    /// visitor reaches the post-loop op-name match without tripping the
    /// `deny_unknown_fields` branch.
    #[test]
    fn deserialize_test_op_is_rejected_with_unsupported_op_name() {
        let raw = r#"[{"op":"test","path":"/a","value":1}]"#;
        let err = serde_json::from_str::<EditScript>(raw).expect_err("`test` must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("test"),
            "error message must name the unsupported op `test`; got: {msg}",
        );
    }

    /// `EditOp` is stricter than RFC 6902's "ignore unknown fields" — the
    /// spec explicitly mandates `serde(deny_unknown_fields)` semantics. This
    /// distinguishes `EditOp` from the looser `PatchOp`, which silently
    /// ignores extra fields per RFC 6902 §4 (see the corresponding test
    /// `deserialize_ignores_unknown_fields_per_rfc_6902` in
    /// `transform/patch.rs`). Pinning the contract here protects against
    /// accidental relaxation back to RFC behaviour.
    #[test]
    fn deserialize_rejects_unknown_fields_on_op_object() {
        let raw = r#"[{"op":"add","path":"/x","value":1,"foo":2}]"#;
        let err = serde_json::from_str::<EditScript>(raw)
            .expect_err("unknown field `foo` must be rejected (deny_unknown_fields semantics)");
        let msg = err.to_string();
        assert!(
            msg.contains("foo") || msg.contains("unknown field"),
            "error must identify the unknown field; got: {msg}",
        );
    }

    /// Spec scenario "Single-op script is not noop by this definition" —
    /// `is_noop()` is a strict syntactic emptiness check. A `Remove` of a
    /// pointer that does not exist in *any* document is still a non-empty
    /// script, and so `is_noop()` SHALL return `false`. Pins the documented
    /// "syntactic, not semantic" contract on the helper.
    #[test]
    fn single_op_script_is_not_noop_regardless_of_target() {
        let script = EditScript::from(vec![EditOp::Remove {
            path: Pointer::parse("/p").unwrap(),
        }]);
        assert!(
            !script.is_noop(),
            "a single-op script is non-empty, so is_noop() must be false even if `/p` is absent",
        );
        assert_eq!(script.len(), 1);
        assert!(!script.is_empty());
    }

    /// `EditScript` derives `Default`. The spec requirement section pins
    /// `derive(... Default)` explicitly; pin the runtime behaviour as well so
    /// a future #[derive] removal is caught here, not in a downstream caller.
    #[test]
    fn default_script_equals_new_and_is_empty() {
        let default = EditScript::default();
        assert_eq!(
            default,
            EditScript::new(),
            "EditScript::default() must equal EditScript::new()",
        );
        assert!(default.is_empty());
        assert!(default.is_noop());
        assert_eq!(default.len(), 0);
    }

    /// `From<Vec<EditOp>>` round-trip — pin the explicit conversion impl that
    /// the spec uses throughout (`EditScript::from(vec![...])`). Asserts both
    /// the `ops()` slice equality and that `len()` matches the input vector
    /// length, so a buggy impl that drops or duplicates ops trips here.
    #[test]
    fn from_vec_round_trip_preserves_ops_and_len() {
        let ops = vec![
            EditOp::Add {
                path: Pointer::parse("/x").unwrap(),
                value: Value::String("a".into()),
            },
            EditOp::Replace {
                path: Pointer::parse("/y").unwrap(),
                value: Value::Bool(false),
            },
        ];
        let script = EditScript::from(ops.clone());
        assert_eq!(script.len(), ops.len());
        assert_eq!(script.ops(), ops.as_slice());
    }

    /// Round-trip Add — spec text "`Add` and `Replace` variants additionally
    /// carry `value`" must hold for `Add` too, not just Replace (which the
    /// existing `serde_round_trip_two_op_script` covers indirectly through
    /// the array path). Pins the wire shape for `Add` directly so the
    /// per-variant serializer arms can't diverge silently.
    #[test]
    fn serialize_add_op_emits_canonical_json_patch_object() {
        let op = EditOp::Add {
            path: Pointer::parse("/a").unwrap(),
            value: Value::Bool(true),
        };
        let json = serde_json::to_value(&op).expect("Add op serializes");
        assert_eq!(
            json,
            serde_json::json!({"op": "add", "path": "/a", "value": true}),
            "Add wire form must match RFC 6902 tagged map exactly",
        );
        let round_tripped: EditOp = serde_json::from_value(json).expect("Add deserializes back");
        assert_eq!(round_tripped, op);
    }

    /// Round-trip Remove — `Remove` has no `value` field. Pin both that the
    /// serializer omits the field entirely and that deserialization rejects
    /// stray `value` only via the unknown-fields path (this is covered by
    /// `deserialize_rejects_unknown_fields_on_op_object` already; here we
    /// only check the no-value shape).
    #[test]
    fn serialize_remove_op_emits_two_field_object_without_value() {
        let op = EditOp::Remove {
            path: Pointer::parse("/c").unwrap(),
        };
        let json = serde_json::to_value(&op).expect("Remove op serializes");
        assert_eq!(
            json,
            serde_json::json!({"op": "remove", "path": "/c"}),
            "Remove wire form must omit the `value` field entirely",
        );
        let round_tripped: EditOp = serde_json::from_value(json).expect("Remove deserializes back");
        assert_eq!(round_tripped, op);
    }

    // ------------------------- multi-op + partial failure --------------------

    /// Spec scenario "Multi-op script applies in order" — we use Option B
    /// (two `Replace` ops on a doc with both keys already present) instead
    /// of the spec's `[Add /x 1, Replace /y 2]` shape because the M2 baseline
    /// `Document::set_at` (which `Add` calls into) returns `MissingKey` for
    /// pointers whose parent doesn't already contain that key — full
    /// mkdir-p insertion is a separate change (see
    /// `openspec/changes/add-ir-foundation/tasks.md` Phase 3 task 3.7
    /// caveat). Two Replaces strictly exercise the "ops apply in declaration
    /// order" guarantee without hitting the mkdir-p limitation.
    #[test]
    fn multi_op_script_applies_in_declaration_order() {
        let mut doc = yaml_doc_two("x", 0, "y", 0);
        assert_eq!(
            doc.original_bytes(),
            b"x: 0\ny: 0\n",
            "fixture sanity: pre-apply bytes",
        );
        let script = EditScript::from(vec![
            EditOp::Replace {
                path: Pointer::parse("/x").unwrap(),
                value: Value::Int(1),
            },
            EditOp::Replace {
                path: Pointer::parse("/y").unwrap(),
                value: Value::Int(2),
            },
        ]);
        script
            .apply(&mut doc)
            .expect("two-Replace script must succeed end-to-end");
        // Both ops landed; key order is preserved (insertion order, NOT
        // alphabetical).
        assert_eq!(
            doc.original_bytes(),
            b"x: 1\ny: 2\n",
            "both Replaces must commit to original_bytes in order",
        );
        match doc.value() {
            Value::Map(m) => {
                assert_eq!(m.get("x"), Some(&Value::Int(1)));
                assert_eq!(m.get("y"), Some(&Value::Int(2)));
                let keys: Vec<&String> = m.keys().collect();
                assert_eq!(
                    keys,
                    vec![&"x".to_owned(), &"y".to_owned()],
                    "Replace must not reorder keys",
                );
            }
            other => panic!("expected map, got: {other:?}"),
        }
    }

    /// Spec scenario "Failed op leaves document partially applied" — when
    /// the second op of a two-op script fails, the first op's mutation MUST
    /// remain committed in `original_bytes`. This pins the documented
    /// non-atomic contract for `EditScript::apply` (atomicity is the
    /// caller's job; `Fixer::apply_script` clones the Document for that).
    /// Also pins the precise error variant via `matches!` so renames of
    /// `PathErrorKind::MissingKey` surface at compile time.
    #[test]
    fn failed_op_leaves_first_op_committed_to_bytes() {
        let mut doc = yaml_doc_one("existing", 0);
        assert_eq!(
            doc.original_bytes(),
            b"existing: 0\n",
            "fixture sanity: pre-apply bytes",
        );
        let script = EditScript::from(vec![
            EditOp::Replace {
                path: Pointer::parse("/existing").unwrap(),
                value: Value::Int(1),
            },
            EditOp::Replace {
                path: Pointer::parse("/missing").unwrap(),
                value: Value::Int(2),
            },
        ]);
        let err = script
            .apply(&mut doc)
            .expect_err("the second Replace must fail because /missing does not exist");
        // The error must be Path/MissingKey for `/missing` — pinned via
        // `matches!` so a rename of `PathErrorKind::MissingKey` is a
        // compile error here, not a silent test pass.
        assert!(
            matches!(
                &err,
                Error::Path {
                    pointer,
                    kind: PathErrorKind::MissingKey,
                    ..
                } if pointer == "/missing"
            ),
            "expected Path/MissingKey for `/missing`, got: {err:?}",
        );
        // Partial-state contract: the first Replace was already committed
        // to original_bytes before the second op ran.
        assert_eq!(
            doc.original_bytes(),
            b"existing: 1\n",
            "first op must remain committed; partial-state is intentional per spec",
        );
        match doc.value() {
            Value::Map(m) => {
                assert_eq!(
                    m.get("existing"),
                    Some(&Value::Int(1)),
                    "in-memory tree must reflect the committed first op",
                );
            }
            other => panic!("expected map, got: {other:?}"),
        }
    }
}
