//! Options that influence how a [`Document`] is rendered back to bytes.
//!
//! M4 §1 introduces two knobs — [`WriteOptions::sort_keys`] and
//! [`WriteOptions::indent`] — that the per-format
//! [`crate::Format::write_with_options`] override consumes when re-emitting a
//! document from its in-memory tree. The textual-edit splice path
//! ([`crate::Document::set_at`] / [`crate::Document::del_at`]) is unaffected:
//! it preserves source bytes byte-for-byte, so these options are inherently
//! a property of the *re-emit* path that `dq fmt` and `dq convert` exercise.
//!
//! The struct is `#[non_exhaustive]` so that future M5+ additions
//! (`quote_style`, `flow_style`, `strip_comments`) can land without a SemVer
//! break. Construct instances by mutating fields off of
//! [`WriteOptions::default()`]:
//!
//! ```
//! use dq_core::WriteOptions;
//!
//! let mut opts = WriteOptions::default();
//! opts.sort_keys = true;
//! assert!(opts.sort_keys);
//! assert!(opts.indent.is_none());
//! ```
//!
//! ## Why a free function for `canonicalize_keys`
//!
//! Per-format writers all share the exact same key-sorting strategy: depth-
//! first deep clone with [`String::cmp`] (case-sensitive, byte-order). Pulling
//! it into one helper keeps the implementations of [`crate::Format::write_with_options`]
//! short and identical across `Json`, `Jsonl`, `Yaml`, and `Toml`.

use indexmap::IndexMap;

use crate::document::Value;

/// Options consumed by [`crate::Format::write_with_options`].
///
/// Every field defaults to "preserve existing behaviour" so that calling the
/// write path with [`WriteOptions::default()`] produces byte-identical output
/// to the legacy [`crate::Format::write`] method.
///
/// The struct is `#[non_exhaustive]` — construct instances with
/// `..Default::default()` syntax to remain forward-compatible with M5+ fields.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct WriteOptions {
    /// When true, deep-canonicalize map keys to alphabetical order before
    /// re-emitting. No-op for textual-edit splice paths (`set_at` / `del_at`).
    pub sort_keys: bool,

    /// Indentation width in spaces for indented output formats.
    ///
    /// - `None` — preserve the format's default rendering (M2 contract).
    /// - `Some(0)` — emit compact output without inserted whitespace.
    /// - `Some(n)` — use `n` spaces per indent level for indented formats
    ///   (`json`, `jsonl`); ignored by `yaml` and `toml` in M4.
    pub indent: Option<u8>,
}

/// Return a deep clone of `value` with map keys sorted alphabetically at
/// every depth.
///
/// Sorting uses [`String::cmp`] (case-sensitive, byte-order). Arrays recurse
/// into each element. Scalar variants are cloned unchanged. The result is
/// idempotent: `canonicalize_keys(canonicalize_keys(v))` equals
/// `canonicalize_keys(v)`.
#[must_use]
pub fn canonicalize_keys(value: &Value) -> Value {
    match value {
        Value::Map(map) => {
            // Collect the entries, sort by key, then rebuild a fresh
            // `IndexMap`. Cloning each value through `canonicalize_keys`
            // keeps the recursion depth-first and matches the documented
            // contract: every nested map ends up sorted.
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            let mut out: IndexMap<String, Value> = IndexMap::with_capacity(entries.len());
            for (k, v) in entries {
                out.insert(k.clone(), canonicalize_keys(v));
            }
            Value::Map(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonicalize_keys).collect()),
        // Scalars (including big-numeric textual literals) clone unchanged.
        Value::Null
        | Value::Bool(_)
        | Value::Int(_)
        | Value::BigInt(_)
        | Value::Float(_)
        | Value::BigFloat(_)
        | Value::String(_) => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;
    use pretty_assertions::assert_eq;

    /// Helper: build a `Value::Map` from a list of `(key, value)` pairs in the
    /// given order. The order matters because `IndexMap` preserves insertion
    /// order, which is what the round-trip tests below assert against.
    fn map_of(pairs: &[(&str, Value)]) -> Value {
        let mut m: IndexMap<String, Value> = IndexMap::new();
        for (k, v) in pairs {
            m.insert((*k).to_owned(), v.clone());
        }
        Value::Map(m)
    }

    /// Extract the keys of a `Value::Map` in their (post-canonicalize)
    /// insertion order. Panics if `v` is not a map — callers know what shape
    /// they're inspecting.
    fn keys_of(v: &Value) -> Vec<String> {
        match v {
            Value::Map(m) => m.keys().cloned().collect(),
            other => panic!("expected map, got {other:?}"),
        }
    }

    #[test]
    fn canonicalize_keys_returns_scalars_unchanged() {
        // Every scalar variant must clone unchanged. Pinning every variant
        // catches the "I forgot to add a branch" bug if `Value` grows a new
        // variant without `canonicalize_keys` being updated.
        for v in [
            Value::Null,
            Value::Bool(true),
            Value::Int(42),
            Value::BigInt("4722366482869645213696".into()),
            Value::Float(3.5),
            Value::BigFloat("0.1".into()),
            Value::String("hello".into()),
        ] {
            assert_eq!(canonicalize_keys(&v), v, "scalar must clone unchanged");
        }
    }

    #[test]
    fn canonicalize_keys_sorts_single_level_map() {
        // Insertion order `z, a, m` → sorted order `a, m, z`. This is the
        // happy path for the `--sort-keys` flag.
        let v = map_of(&[
            ("z", Value::Int(1)),
            ("a", Value::Int(2)),
            ("m", Value::Int(3)),
        ]);
        let sorted = canonicalize_keys(&v);
        assert_eq!(keys_of(&sorted), vec!["a", "m", "z"]);
    }

    #[test]
    fn canonicalize_keys_recurses_into_nested_maps() {
        // The outer map has keys `b, a` and the inner has `y, x`. Both
        // levels must be sorted independently — depth-first recursion.
        let inner = map_of(&[("y", Value::Int(1)), ("x", Value::Int(2))]);
        let outer = map_of(&[("b", inner.clone()), ("a", Value::Int(0))]);
        let sorted = canonicalize_keys(&outer);
        assert_eq!(keys_of(&sorted), vec!["a", "b"]);
        // Drill into the nested map: it must also be sorted.
        let Value::Map(m) = &sorted else {
            panic!("expected outer map");
        };
        let inner_sorted = m.get("b").expect("inner key 'b' present");
        assert_eq!(keys_of(inner_sorted), vec!["x", "y"]);
    }

    #[test]
    fn canonicalize_keys_processes_array_of_maps() {
        // Each element of the array is a map with its own key order. Both
        // must be canonicalized — the array shape itself is preserved.
        let arr = Value::Array(vec![
            map_of(&[("z", Value::Int(1)), ("a", Value::Int(2))]),
            map_of(&[("y", Value::Int(3)), ("b", Value::Int(4))]),
        ]);
        let sorted = canonicalize_keys(&arr);
        let Value::Array(items) = &sorted else {
            panic!("expected array");
        };
        assert_eq!(items.len(), 2);
        assert_eq!(keys_of(&items[0]), vec!["a", "z"]);
        assert_eq!(keys_of(&items[1]), vec!["b", "y"]);
    }

    #[test]
    fn canonicalize_keys_is_idempotent() {
        // Apply twice; result must equal applying once. This is the property
        // that lets `--sort-keys` be safely composed with itself (e.g.
        // `dq fmt --sort-keys` run twice on the same file produces the same
        // bytes the second pass).
        let v = map_of(&[
            ("z", Value::Int(1)),
            ("a", map_of(&[("y", Value::Int(2)), ("x", Value::Int(3))])),
            ("m", Value::Array(vec![map_of(&[("c", Value::Int(4))])])),
        ]);
        let once = canonicalize_keys(&v);
        let twice = canonicalize_keys(&once);
        assert_eq!(once, twice, "canonicalize_keys must be idempotent");
    }

    #[test]
    fn write_options_default_is_inert() {
        // `WriteOptions::default()` must produce the M2-baseline-compatible
        // shape: `sort_keys = false`, `indent = None`. These defaults are the
        // contract that lets `write_with_options(..&Default::default())`
        // round-trip byte-for-byte through the legacy `write` method.
        let opts = WriteOptions::default();
        assert!(!opts.sort_keys, "sort_keys must default to false");
        assert!(opts.indent.is_none(), "indent must default to None");
    }
}
