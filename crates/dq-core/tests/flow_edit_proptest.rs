//! Property: a textual `del` / `set` inside a flow collection — a JSON
//! document, a YAML flow mapping, a TOML inline table — succeeds and writes
//! bytes that read back as exactly the edited tree.
//!
//! The trees are random: nested maps and arrays of small ints, bools and
//! short strings, rendered on one line, one entry per line (YAML), or
//! pretty-printed (JSON). Each case picks a node of the tree and deletes
//! it, overwrites a scalar with another, or adds a key to a map.

use dq_core::{Document, Pointer, Value, by_name, parse_json_with_spans, parse_yaml_with_spans};
use indexmap::IndexMap;
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;

/// A random tree: maps with keys `k…`, arrays, and leaves no format types
/// differently from the others.
fn arb_tree() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        (-50_i64..50).prop_map(Value::Int),
        any::<bool>().prop_map(Value::Bool),
        "s[a-z]{0,3}".prop_map(Value::String),
    ];
    leaf.prop_recursive(3, 24, 4, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            prop::collection::btree_map("k[a-z]{0,2}", inner, 0..4)
                .prop_map(|map| Value::Map(map.into_iter().collect())),
        ]
    })
}

/// A root mapping with at least one entry.
fn arb_root() -> impl Strategy<Value = Value> {
    prop::collection::btree_map("k[a-z]{0,2}", arb_tree(), 1..4)
        .prop_map(|map| Value::Map(map.into_iter().collect()))
}

#[derive(Debug, Clone, Copy)]
enum Flavor {
    /// One line of JSON with `", "` and `": "` separators.
    JsonLine,
    /// `serde_json::to_string_pretty`.
    JsonPretty,
    /// A YAML flow mapping as the whole document.
    YamlFlow,
    /// The same, one entry per line.
    YamlFlowLines,
    /// A TOML inline table under the root key `r`.
    TomlInline,
}

fn render(value: &Value, flavor: Flavor, out: &mut String) {
    let yaml = matches!(flavor, Flavor::YamlFlow | Flavor::YamlFlowLines);
    let sep = if matches!(flavor, Flavor::YamlFlowLines) {
        ",\n "
    } else {
        ", "
    };
    let toml = matches!(flavor, Flavor::TomlInline);
    match value {
        Value::Int(i) => out.push_str(&i.to_string()),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::String(s) if yaml => out.push_str(s),
        Value::String(s) => out.push_str(&format!("\"{s}\"")),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(sep);
                }
                render(item, flavor, out);
            }
            out.push(']');
        }
        Value::Map(map) => {
            out.push('{');
            for (i, (key, item)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(sep);
                }
                if yaml || toml {
                    out.push_str(key);
                } else {
                    out.push_str(&format!("\"{key}\""));
                }
                out.push_str(if toml { " = " } else { ": " });
                render(item, flavor, out);
            }
            out.push('}');
        }
        other => unreachable!("arb_tree makes no {other:?}"),
    }
}

/// The source text of `tree` and the prefix its pointers take in it.
fn source(tree: &Value, flavor: Flavor) -> (String, &'static str) {
    let mut out = String::new();
    match flavor {
        Flavor::JsonPretty => {
            out = serde_json::to_string_pretty(&to_json(tree)).expect("serialise");
            out.push('\n');
            (out, "")
        }
        Flavor::TomlInline => {
            out.push_str("r = ");
            render(tree, flavor, &mut out);
            out.push('\n');
            (out, "/r")
        }
        _ => {
            render(tree, flavor, &mut out);
            out.push('\n');
            (out, "")
        }
    }
}

fn to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Int(i) => (*i).into(),
        Value::Bool(b) => (*b).into(),
        Value::String(s) => s.clone().into(),
        Value::Array(items) => items.iter().map(to_json).collect(),
        Value::Map(map) => map
            .iter()
            .map(|(k, v)| (k.clone(), to_json(v)))
            .collect::<serde_json::Map<_, _>>()
            .into(),
        other => unreachable!("arb_tree makes no {other:?}"),
    }
}

fn parse(flavor: Flavor, bytes: &[u8]) -> dq_core::Result<Document> {
    match flavor {
        Flavor::JsonLine | Flavor::JsonPretty => parse_json_with_spans(bytes),
        Flavor::YamlFlow | Flavor::YamlFlowLines => parse_yaml_with_spans(bytes),
        Flavor::TomlInline => by_name("toml").expect("toml registered").parse(bytes),
    }
}

/// Every node below the root, as pointer segments.
fn paths(value: &Value, at: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
    let children: Vec<(String, &Value)> = match value {
        Value::Array(items) => items
            .iter()
            .enumerate()
            .map(|(i, v)| (i.to_string(), v))
            .collect(),
        Value::Map(map) => map.iter().map(|(k, v)| (k.clone(), v)).collect(),
        _ => return,
    };
    for (segment, child) in children {
        at.push(segment);
        out.push(at.clone());
        paths(child, at, out);
        at.pop();
    }
}

/// The node at `path` below `value`.
fn node_mut<'a>(value: &'a mut Value, path: &[String]) -> &'a mut Value {
    path.iter().fold(value, |node, segment| match node {
        Value::Array(items) => &mut items[segment.parse::<usize>().expect("index")],
        Value::Map(map) => map.get_mut(segment).expect("key"),
        _ => unreachable!("a path goes through collections"),
    })
}

#[derive(Debug, Clone, Copy)]
enum Edit {
    Del,
    SetScalar,
    AddKey,
}

/// Apply `edit` at `path` to `tree`; `None` when it does not apply (a new
/// key needs a map, a scalar replaces a scalar).
fn expected(tree: &Value, path: &[String], edit: Edit) -> Option<(Value, Vec<String>)> {
    let mut tree = tree.clone();
    match edit {
        Edit::Del => {
            let (last, parent) = path.split_last().expect("below the root");
            match node_mut(&mut tree, parent) {
                Value::Array(items) => {
                    items.remove(last.parse::<usize>().expect("index"));
                }
                Value::Map(map) => {
                    map.shift_remove(last);
                }
                _ => unreachable!(),
            }
            Some((tree, path.to_vec()))
        }
        Edit::SetScalar => {
            let node = node_mut(&mut tree, path);
            // `set` over a collection is not a textual edit dq makes.
            if matches!(node, Value::Array(_) | Value::Map(_)) {
                return None;
            }
            *node = Value::Int(7);
            Some((tree, path.to_vec()))
        }
        Edit::AddKey => match node_mut(&mut tree, path) {
            Value::Map(map) if !map.contains_key("zz") => {
                map.insert("zz".to_owned(), Value::Int(7));
                let mut new = path.to_vec();
                new.push("zz".to_owned());
                Some((tree, new))
            }
            _ => None,
        },
    }
}

fn wrap(prefix: &str, tree: Value) -> Value {
    if prefix.is_empty() {
        return tree;
    }
    let mut root = IndexMap::new();
    root.insert(prefix.trim_start_matches('/').to_owned(), tree);
    Value::Map(root)
}

fn arb_flavor() -> impl Strategy<Value = Flavor> {
    prop_oneof![
        Just(Flavor::JsonLine),
        Just(Flavor::JsonPretty),
        Just(Flavor::YamlFlow),
        Just(Flavor::YamlFlowLines),
        Just(Flavor::TomlInline),
    ]
}

fn arb_edit() -> impl Strategy<Value = Edit> {
    prop_oneof![Just(Edit::Del), Just(Edit::SetScalar), Just(Edit::AddKey)]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn a_flow_edit_reads_back_as_the_edited_tree(
        tree in arb_root(),
        flavor in arb_flavor(),
        edit in arb_edit(),
        pick in any::<prop::sample::Index>(),
    ) {
        let mut all = Vec::new();
        paths(&tree, &mut Vec::new(), &mut all);
        if matches!(edit, Edit::AddKey) {
            all.push(Vec::new());
        }
        let path = pick.get(&all).clone();
        let Some((want, target)) = expected(&tree, &path, edit) else {
            return Ok(());
        };
        let (src, prefix) = source(&tree, flavor);
        let want = wrap(prefix, want);
        let pointer = Pointer::parse(&format!("{prefix}/{}", target.join("/"))).expect("pointer");

        let mut doc = parse(flavor, src.as_bytes()).expect("the rendered source parses");
        prop_assert_eq!(doc.value(), &wrap(prefix, tree.clone()));
        let result = match edit {
            Edit::Del => doc.del_at(&pointer),
            Edit::SetScalar | Edit::AddKey => doc.set_at(&pointer, Value::Int(7)),
        };
        if let Err(err) = result {
            // None of these shapes needs a refusal; one would be a
            // regression of the flow-entry splice.
            prop_assert!(false, "{:?}: {:?} refused: {}", src, pointer, err);
        }
        let bytes = String::from_utf8(doc.original_bytes().to_vec()).expect("UTF-8");
        let reread = parse(flavor, bytes.as_bytes())
            .unwrap_or_else(|err| panic!("{src:?} -> {bytes:?} does not parse: {err}"));
        prop_assert_eq!(reread.value(), &want, "{:?} -> {:?}", src, bytes);
        prop_assert_eq!(doc.value(), &want);
    }
}
