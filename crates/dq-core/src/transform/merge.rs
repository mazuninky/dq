//! RFC 7396 JSON Merge Patch — [`apply_merge`] engine.
//!
//! The semantics, per RFC 7396 §1:
//!
//! - If the patch is not a JSON object, the target is replaced wholesale.
//! - If the patch is an object, recurse pair-by-pair:
//!   - `null` value → remove the key from the target (silent NOP if absent).
//!   - Both target and patch values are objects → recurse.
//!   - Otherwise → replace the target value with the patch value.
//!
//! Like [`apply_patch`], the engine clones the [`Document`] before mutating
//! it, so any error during the recursion leaves the caller's `doc` byte-
//! identical to its pre-call state.
//!
//! [`apply_patch`]: super::patch::apply_patch

use crate::Result;
use crate::document::{Document, Value};
use crate::error::{Error, PathErrorKind};
use crate::pointer::{Pointer, Segment};

use super::patch::read_value_at;

/// Apply an RFC 7396 merge patch to `doc`.
///
/// `patch` is an arbitrary [`Value`]: maps merge recursively, scalars and
/// arrays replace, and `Null` removes the addressed key. The engine is
/// clone-on-apply — the caller's `doc` is left untouched on error.
///
/// # Errors
///
/// - [`Error::Path`] for type mismatches, out-of-bounds indices, and other
///   pointer-resolution failures encountered while writing the patch.
/// - [`Error::WriteUnavailable`] when the document was loaded read-only.
///
/// `null` against a missing key is intentionally NOT an error (RFC 7396 §1).
pub fn apply_merge(doc: &mut Document, patch: &Value) -> Result<()> {
    let mut working = doc.clone();
    merge_into(&mut working, &Pointer::default(), patch)?;
    *doc = working;
    Ok(())
}

/// Recursive worker. `base` is the pointer of the current target subtree;
/// callers start with [`Pointer::default`] (root) and the recursion extends
/// it one segment at a time.
fn merge_into(doc: &mut Document, base: &Pointer, patch: &Value) -> Result<()> {
    let Value::Map(patch_map) = patch else {
        // Non-map patch replaces the addressed subtree wholesale. Root case
        // (base.is_root()) won't be reached here in practice because callers
        // always enter through `apply_merge` with the root pointer and an
        // outer Map check, but we handle it for correctness should the engine
        // ever be invoked with a non-Map patch at the top level.
        return doc.set_at(base, patch.clone());
    };

    for (k, v) in patch_map {
        let child = base.with_segment(Segment::Key(k.clone()));
        if matches!(v, Value::Null) {
            // RFC 7396 §1: missing key under `null` is a silent NOP.
            match doc.del_at(&child) {
                Ok(()) => {}
                Err(Error::Path {
                    kind: PathErrorKind::MissingKey,
                    ..
                }) => {}
                Err(other) => return Err(other),
            }
            continue;
        }

        // Recurse only when both the existing target and the patch are maps.
        // Any other shape (target missing, target scalar, target array, or
        // patch non-map) replaces the whole subtree.
        let target_is_map = matches!(read_value_at(doc, &child), Ok(Value::Map(_)));
        if target_is_map && matches!(v, Value::Map(_)) {
            merge_into(doc, &child, v)?;
        } else {
            doc.set_at(&child, v.clone())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{FormatTag, SpanContext, SpanMap, ValueSpan};
    use indexmap::IndexMap;
    use pretty_assertions::assert_eq;

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

    /// `a: <int>\nb: <int>\n` shape with spans for both keys.
    fn yaml_two_key_doc(a: i64, b: i64) -> Document {
        // Bytes layout: `a: <a>\nb: <b>\n`. With single-digit values:
        // index 0..1 = "a", 1 = ':', 2 = ' ', 3 = '<a>', 4 = '\n'
        // index 5 = "b", 6 = ':', 7 = ' ', 8 = '<b>', 9 = '\n'
        let bytes = format!("a: {a}\nb: {b}\n").into_bytes();
        let mut spans = SpanMap::new();
        let a_value_start = 3;
        let a_value_end = a_value_start + a.to_string().len();
        let a_line_end = a_value_end + 1; // include trailing \n
        spans.insert(
            "/a".into(),
            block_map_span(a_value_start, a_value_end, 0, a_line_end),
        );
        let b_value_start = a_line_end + 3; // after "b: "
        let b_value_end = b_value_start + b.to_string().len();
        let b_line_end = b_value_end + 1;
        spans.insert(
            "/b".into(),
            block_map_span(b_value_start, b_value_end, a_line_end, b_line_end),
        );
        let mut value = IndexMap::new();
        value.insert("a".into(), Value::Int(a));
        value.insert("b".into(), Value::Int(b));
        Document::with_spans(Value::Map(value), bytes, spans, FormatTag::Yaml)
    }

    /// `a: <int>\n` shape with a single key span.
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

    // ---- scalar replace ----

    #[test]
    fn apply_merge_scalar_replaces_value() {
        let mut doc = yaml_doc_one("a", 3);
        let patch = map_one("a", Value::Int(7));
        apply_merge(&mut doc, &patch).expect("merge succeeds");
        assert_eq!(doc.original_bytes(), b"a: 7\n");
        match doc.value() {
            Value::Map(m) => assert_eq!(m.get("a"), Some(&Value::Int(7))),
            other => panic!("expected map, got {other:?}"),
        }
    }

    // ---- null removes existing key ----

    #[test]
    fn apply_merge_null_removes_existing_key() {
        let mut doc = yaml_two_key_doc(1, 2);
        // {"a": null} — should remove /a, leaving /b in place.
        let patch = map_one("a", Value::Null);
        apply_merge(&mut doc, &patch).expect("null-key merge succeeds");
        assert_eq!(
            doc.original_bytes(),
            b"b: 2\n",
            "RFC 7396 null removes the addressed key entirely",
        );
        match doc.value() {
            Value::Map(m) => {
                assert!(!m.contains_key("a"));
                assert_eq!(m.get("b"), Some(&Value::Int(2)));
            }
            other => panic!("expected map, got {other:?}"),
        }
    }

    // ---- null on a missing key is a silent NOP ----

    #[test]
    fn apply_merge_null_on_missing_key_is_silent_nop() {
        let mut doc = yaml_doc_one("a", 3);
        let original = doc.original_bytes().to_vec();
        // {"missing": null} on a doc without /missing — must succeed and
        // leave the document untouched.
        let patch = map_one("missing", Value::Null);
        apply_merge(&mut doc, &patch).expect("null-on-missing is RFC-compliant NOP");
        assert_eq!(
            doc.original_bytes(),
            original.as_slice(),
            "null on missing key must not mutate the document",
        );
    }

    // ---- recursive map merge: only the addressed leaf changes ----
    //
    // We use a flat shape (`a: <int>` with /a → 3) and a recursive patch on
    // an existing scalar — the patch's shape (Map) does NOT match the target
    // (scalar Int), so per RFC 7396 the whole /a subtree is replaced. This
    // exercises the "patch is map, target is not" branch of `merge_into`.

    #[test]
    fn apply_merge_replaces_when_target_not_a_map() {
        let mut doc = yaml_doc_one("a", 3);
        // {"a": {"nested": 1}} — target /a is an Int, so the merge replaces
        // /a with the whole nested map rather than recursing.
        let inner = map_one("nested", Value::Int(1));
        let patch = map_one("a", inner.clone());
        apply_merge(&mut doc, &patch).expect("non-map target → wholesale replace");
        match doc.value() {
            Value::Map(m) => {
                assert_eq!(m.get("a"), Some(&inner));
            }
            other => panic!("expected map, got {other:?}"),
        }
    }
}
