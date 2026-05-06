//! Pre-order enumeration of every JSON-Pointer-addressable node in a `Document`.
//!
//! Used by the CLI's `paths` command. For multi-document streams, each
//! pointer is prefixed with `/<doc_idx>` so two-document YAML streams
//! produce `/0/spec/replicas`, `/1/spec/replicas`, etc.

use crate::document::{Document, Value};
use crate::pointer::{Pointer, Segment};

/// Walk the document and yield `(pointer, type_name)` pairs in pre-order.
///
/// Container nodes (objects, arrays) are emitted first; their children
/// follow. Leaf nodes are emitted exactly once. Order within an object
/// follows insertion order (`IndexMap`).
pub fn enumerate_pointers(doc: &Document) -> impl Iterator<Item = (Pointer, &'static str)> + '_ {
    let mut out: Vec<(Pointer, &'static str)> = Vec::new();
    if let Some(values) = doc.values() {
        for (idx, v) in values.iter().enumerate() {
            let mut prefix = vec![Segment::Index(idx)];
            walk(v, &mut prefix, &mut out);
        }
    } else {
        walk(doc.value(), &mut Vec::new(), &mut out);
    }
    out.into_iter()
}

fn walk(value: &Value, path: &mut Vec<Segment>, out: &mut Vec<(Pointer, &'static str)>) {
    out.push((Pointer::new(path.clone()), value.type_name()));
    match value {
        Value::Array(items) => {
            for (idx, item) in items.iter().enumerate() {
                path.push(Segment::Index(idx));
                walk(item, path, out);
                path.pop();
            }
        }
        Value::Map(map) => {
            for (k, v) in map {
                path.push(Segment::Key(k.clone()));
                walk(v, path, out);
                path.pop();
            }
        }
        // Leaves: nothing further to enumerate.
        Value::Null
        | Value::Bool(_)
        | Value::Int(_)
        | Value::BigInt(_)
        | Value::Float(_)
        | Value::BigFloat(_)
        | Value::String(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;

    #[test]
    fn enumerate_emits_root_for_scalar() {
        let doc = Document::single(Value::Int(7));
        let pointers: Vec<_> = enumerate_pointers(&doc).collect();
        assert_eq!(pointers.len(), 1);
        assert_eq!(pointers[0].0.as_canonical(), "");
        assert_eq!(pointers[0].1, "int");
    }

    #[test]
    fn enumerate_walks_objects_in_insertion_order() {
        let mut map = IndexMap::new();
        map.insert("z".to_owned(), Value::Int(1));
        map.insert("a".to_owned(), Value::Int(2));
        let doc = Document::single(Value::Map(map));
        let pointers: Vec<String> = enumerate_pointers(&doc)
            .map(|(p, _)| p.as_canonical())
            .collect();
        assert_eq!(pointers, vec!["", "/z", "/a"]);
    }

    #[test]
    fn enumerate_prefixes_multi_doc_with_index() {
        let doc = Document::multi(vec![Value::Int(1), Value::Int(2)]);
        let pointers: Vec<String> = enumerate_pointers(&doc)
            .map(|(p, _)| p.as_canonical())
            .collect();
        assert_eq!(pointers, vec!["/0", "/1"]);
    }
}
