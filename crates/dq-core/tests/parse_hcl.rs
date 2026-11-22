//! Component tests for the HCL (Terraform) parser/writer.
//!
//! Mirror of `parse_yaml.rs` — go through the registry-resolved [`dq_core::Format`]
//! trait object so the test exercises the same surface end users hit through
//! `dq -F hcl …`. The Stage 2 inline `#[cfg(test)] mod tests` inside
//! `parsers/hcl.rs` covers minimal sanity (one parse, one block, one error).
//! These tests go deeper: extension detection, list/number scalars,
//! parse-error variant pinning, and round-trip equivalence (semantic, NOT
//! byte-identical — `hcl-rs` has no comment-preserving emitter).

use camino::Utf8Path;
use dq_core::{Document, Format, Value};
use pretty_assertions::assert_eq;

/// Resolve the HCL format through the registry so the test fails closed if
/// the format is ever de-registered.
fn hcl() -> &'static dyn Format {
    dq_core::by_name("hcl").expect("hcl format must be registered")
}

/// Sugar for "parse this string, panic with the parser message on failure".
fn parse_hcl(s: &str) -> Document {
    hcl()
        .parse(s.as_bytes())
        .unwrap_or_else(|e| panic!("hcl parse failed: {e}\n---input---\n{s}\n-----------"))
}

#[test]
fn parse_top_level_attribute_produces_string_scalar() {
    // Per spec: `region = "us-east-1"` → Map { "region": String("us-east-1") }.
    let doc = parse_hcl(r#"region = "us-east-1""#);
    let Value::Map(top) = doc.value() else {
        panic!("expected top-level Map, got {:?}", doc.value());
    };
    assert_eq!(
        top.get("region"),
        Some(&Value::String("us-east-1".into())),
        "string attribute must round-trip without alteration",
    );
}

#[test]
fn parse_backend_block_with_label_nests_one_level_per_label() {
    // Spec scenario "HCL backend block parses to nested map":
    // `backend "s3" { bucket = "x" }` → `{ "backend": { "s3": { "bucket": "x" } } }`.
    let doc = parse_hcl(r#"backend "s3" { bucket = "x" }"#);
    let Value::Map(top) = doc.value() else {
        panic!("expected top-level map");
    };
    let Some(Value::Map(backend)) = top.get("backend") else {
        panic!("missing /backend, got: {top:?}");
    };
    let Some(Value::Map(s3)) = backend.get("s3") else {
        panic!("missing /backend/s3, got: {backend:?}");
    };
    assert_eq!(
        s3.get("bucket"),
        Some(&Value::String("x".into())),
        "label-as-key nesting must reach the leaf attribute",
    );
}

#[test]
fn parse_list_attribute_produces_array_of_strings() {
    // Lists are common in tfvars (`subnets = ["a", "b"]`). The parser must
    // produce a `Value::Array` of homogeneous `Value::String` cells.
    let doc = parse_hcl(r#"subnets = ["a", "b"]"#);
    let Value::Map(top) = doc.value() else {
        panic!("expected map");
    };
    let Some(Value::Array(items)) = top.get("subnets") else {
        panic!("missing /subnets array, got: {top:?}");
    };
    assert_eq!(
        items.len(),
        2,
        "expected 2 elements in /subnets, got {}",
        items.len()
    );
    assert_eq!(items[0], Value::String("a".into()));
    assert_eq!(items[1], Value::String("b".into()));
}

#[test]
fn parse_integer_attribute_produces_int_scalar() {
    // Numeric attributes that fit `i64` must surface as `Value::Int`, not as
    // a Float or BigInt placeholder. Pinning this prevents the `hcl-rs`
    // Number → Value mapping from regressing into the Float fallback.
    let doc = parse_hcl("replicas = 3");
    let Value::Map(top) = doc.value() else {
        panic!()
    };
    assert_eq!(
        top.get("replicas"),
        Some(&Value::Int(3)),
        "small integer must use the i64 variant",
    );
}

#[test]
fn registry_detects_hcl_tf_and_tfvars_extensions() {
    // The dispatcher must recognise all three of HCL's extensions. A
    // regression that drops one would silently break Terraform users whose
    // files often differ only in extension.
    for ext in ["hcl", "tf", "tfvars"] {
        let path = format!("a.{ext}");
        let fmt = dq_core::detect(Utf8Path::new(&path)).unwrap_or_else(|| {
            panic!("dq_core::detect must resolve `.{ext}` to the HCL parser, got None")
        });
        assert_eq!(
            fmt.name(),
            "hcl",
            "extension `.{ext}` must resolve to the `hcl` format, got `{}`",
            fmt.name(),
        );
    }
}

#[test]
fn parse_invalid_syntax_returns_parse_error_variant() {
    // Malformed input must surface as `Error::Parse`, not as a panic, an Io
    // error, or an UnsupportedFormat. The exit-code mapper relies on this
    // variant to pick PARSE_ERROR (3).
    let err = hcl()
        .parse(b"this = =\n")
        .expect_err("malformed HCL must surface as a parse error");
    assert!(
        matches!(err, dq_core::Error::Parse { .. }),
        "expected Error::Parse, got {err:?}",
    );
    // The message must not be empty — the renderer needs *something* to
    // show users.
    assert!(
        !err.to_string().is_empty(),
        "parse-error message must not be empty",
    );
}

#[test]
fn round_trip_is_structurally_equal_after_parse_then_write_then_parse() {
    // `hcl-rs` has no comment/quote-preserving emitter, so a literal byte
    // round-trip is impossible. The contract is structural: parse → write →
    // parse must produce a `Value` equal to the first parse. Pinning this
    // catches regressions in `value_to_hcl_value` that would silently
    // round-trip values incorrectly (e.g. losing list cells).
    let source = r#"
region   = "us-east-1"
replicas = 3
subnets  = ["a", "b"]

backend "s3" {
  bucket = "tfstate"
}
"#;
    let doc1 = parse_hcl(source);
    let mut buf: Vec<u8> = Vec::new();
    hcl()
        .write(&doc1, &mut buf)
        .expect("hcl write must succeed for a parsed-then-clean tree");
    let rendered = String::from_utf8(buf).expect("hcl writer must produce utf-8");
    let doc2 = parse_hcl(&rendered);
    assert_eq!(
        doc1.value(),
        doc2.value(),
        "round-trip must preserve the value tree exactly",
    );
}

/// Write `doc` as HCL, panicking with the writer's message on failure.
fn write_hcl(doc: &Document) -> String {
    let mut buf: Vec<u8> = Vec::new();
    hcl()
        .write(doc, &mut buf)
        .unwrap_or_else(|e| panic!("hcl write failed: {e}"));
    String::from_utf8(buf).expect("hcl writer must produce utf-8")
}

/// `{key: value}`.
fn map1(key: &str, value: Value) -> Value {
    Value::Map([(key.to_owned(), value)].into_iter().collect())
}

#[test]
fn fifty_nested_blocks_write_and_read_back() {
    let depth = 50;
    let source = format!("{}x = 1\n{}", "a {\n".repeat(depth), "}\n".repeat(depth));
    let doc = parse_hcl(&source);
    let expected = (0..depth).fold(map1("x", Value::Int(1)), |inner, _| map1("a", inner));
    assert_eq!(doc.value(), &expected);
    let written = write_hcl(&doc);
    assert!(written.starts_with("a {\n  a {\n    a {\n"), "{written}");
    assert_eq!(parse_hcl(&written).value(), &expected);
}

#[test]
fn fifty_nested_arrays_and_objects_write_and_read_back() {
    let expected = map1(
        "v",
        (0..50).fold(Value::Int(1), |inner, level| {
            if level % 2 == 0 {
                Value::Array(vec![inner, Value::Bool(true)])
            } else {
                map1("k", inner)
            }
        }),
    );
    let doc = Document::value_only(expected.clone(), dq_core::FormatTag::Hcl);
    assert_eq!(parse_hcl(&write_hcl(&doc)).value(), &expected);
}

#[test]
fn empty_arrays_and_objects_inside_arrays_write_and_read_back() {
    let empty_array = || Value::Array(Vec::new());
    let empty_map = || Value::Map(Default::default());
    let expected = map1(
        "a",
        Value::Array(vec![
            empty_array(),
            empty_map(),
            Value::Array(vec![empty_array()]),
            Value::Array(vec![empty_map()]),
            map1("k", empty_array()),
        ]),
    );
    let doc = Document::value_only(expected.clone(), dq_core::FormatTag::Hcl);
    let written = write_hcl(&doc);
    assert_eq!(
        written,
        "a = [\n  [],\n  {},\n  [\n    []\n  ],\n  [\n    {}\n  ],\n  {\n    \"k\" = []\n  }\n]\n"
    );
    assert_eq!(parse_hcl(&written).value(), &expected);
}

// ---------------------------------------------------------------------------
// Repeated blocks. A block type that occurs once keeps the documented shape
// (`{type: {label: body}}`); blocks repeated with the same type and labels
// become a list of their bodies, in source order. The writer writes every
// key the reader read as a block back as blocks, so the text reads back to
// the same tree.
// ---------------------------------------------------------------------------

/// Parse `source`, write it, and assert the written text is `expected` and
/// reads back to the tree `source` read to. Returns that tree.
fn round_trip(source: &str, expected: &str) -> Value {
    let doc = parse_hcl(source);
    let written = write_hcl(&doc);
    assert_eq!(written, expected, "written HCL of:\n{source}");
    assert_eq!(
        parse_hcl(&written).value(),
        doc.value(),
        "re-read tree of:\n{written}"
    );
    doc.value().clone()
}

/// `{k: v, ...}`.
fn map(members: Vec<(&str, Value)>) -> Value {
    Value::Map(
        members
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect(),
    )
}

#[test]
fn repeated_unlabelled_blocks_read_as_a_list_and_write_back_as_blocks() {
    let source = r#"
resource "aws_security_group" "app" {
  name = "app"
  ingress {
    from_port   = 443
    cidr_blocks = ["10.0.0.0/8"]
  }
  ingress {
    from_port   = 80
    cidr_blocks = ["0.0.0.0/0"]
  }
  ingress {
    from_port = 22
  }
  tags = { Name = "app" }
}
"#;
    let tree = round_trip(
        source,
        "resource \"aws_security_group\" \"app\" {\n  name = \"app\"\n  ingress {\n    \
         from_port = 443\n    cidr_blocks = [\n      \"10.0.0.0/8\"\n    ]\n  }\n  ingress {\n    \
         from_port = 80\n    cidr_blocks = [\n      \"0.0.0.0/0\"\n    ]\n  }\n  ingress {\n    \
         from_port = 22\n  }\n  tags = {\n    \"Name\" = \"app\"\n  }\n}\n",
    );
    let strings =
        |items: &[&str]| Value::Array(items.iter().map(|s| Value::String((*s).into())).collect());
    assert_eq!(
        tree,
        map1(
            "resource",
            map1(
                "aws_security_group",
                map1(
                    "app",
                    map(vec![
                        ("name", Value::String("app".into())),
                        (
                            "ingress",
                            Value::Array(vec![
                                map(vec![
                                    ("from_port", Value::Int(443)),
                                    ("cidr_blocks", strings(&["10.0.0.0/8"])),
                                ]),
                                map(vec![
                                    ("from_port", Value::Int(80)),
                                    ("cidr_blocks", strings(&["0.0.0.0/0"])),
                                ]),
                                map1("from_port", Value::Int(22)),
                            ]),
                        ),
                        ("tags", map1("Name", Value::String("app".into()))),
                    ]),
                ),
            ),
        ),
    );
}

#[test]
fn blocks_with_the_same_type_and_labels_become_a_list_under_the_last_label() {
    let tree = round_trip(
        "provider \"aws\" {\n  region = \"us-east-1\"\n}\n\
         provider \"google\" {\n  project = \"p\"\n}\n\
         provider \"aws\" {\n  alias = \"west\"\n  region = \"us-west-2\"\n}\n",
        "provider \"aws\" {\n  region = \"us-east-1\"\n}\n\
         provider \"aws\" {\n  alias = \"west\"\n  region = \"us-west-2\"\n}\n\
         provider \"google\" {\n  project = \"p\"\n}\n",
    );
    assert_eq!(
        tree,
        map1(
            "provider",
            map(vec![
                (
                    "aws",
                    Value::Array(vec![
                        map1("region", Value::String("us-east-1".into())),
                        map(vec![
                            ("alias", Value::String("west".into())),
                            ("region", Value::String("us-west-2".into())),
                        ]),
                    ]),
                ),
                ("google", map1("project", Value::String("p".into()))),
            ]),
        ),
    );
    // Two labels: only the bodies under the same pair become a list.
    let tree = round_trip(
        "resource \"t\" \"a\" {\n  n = 1\n}\nresource \"t\" \"b\" {\n  n = 2\n}\n\
         resource \"t\" \"a\" {\n  n = 3\n}\n",
        "resource \"t\" \"a\" {\n  n = 1\n}\nresource \"t\" \"a\" {\n  n = 3\n}\n\
         resource \"t\" \"b\" {\n  n = 2\n}\n",
    );
    assert_eq!(
        tree,
        map1(
            "resource",
            map1(
                "t",
                map(vec![
                    (
                        "a",
                        Value::Array(vec![map1("n", Value::Int(1)), map1("n", Value::Int(3))]),
                    ),
                    ("b", map1("n", Value::Int(2))),
                ]),
            ),
        ),
    );
}

#[test]
fn blocks_that_occur_once_keep_their_shape_and_are_written_as_blocks() {
    let source = "terraform {\n  required_version = \">= 1.5\"\n  backend \"s3\" {\n    \
                  bucket = \"b\"\n  }\n}\nlifecycle {}\nlabel \"\" {}\n";
    let tree = round_trip(source, source);
    assert_eq!(
        tree,
        map(vec![
            (
                "terraform",
                map(vec![
                    ("required_version", Value::String(">= 1.5".into())),
                    (
                        "backend",
                        map1("s3", map1("bucket", Value::String("b".into())))
                    ),
                ]),
            ),
            ("lifecycle", map(vec![])),
            ("label", map1("", map(vec![]))),
        ]),
    );
}

#[test]
fn nested_repeats_keep_every_block_at_every_level() {
    // `dynamic` blocks with a repeated `content`, repeated `provisioner`s,
    // and a repeated outer block whose bodies repeat inside, differently.
    let source = r#"resource "aws_instance" "x" {
  dynamic "ingress" {
    for_each = "one"
    content {
      a = 1
    }
  }
  dynamic "ingress" {
    for_each = "two"
    content {
      a = 2
    }
    content {
      a = 3
    }
  }
  provisioner "local-exec" {
    command = "one"
  }
  provisioner "local-exec" {
    command = "two"
  }
}
outer {
  inner {
    v = 1
  }
  inner {
    v = 2
  }
}
outer {
  inner {
    v = 3
  }
}
"#;
    let tree = round_trip(source, source);
    let inner = |v: i64| map1("v", Value::Int(v));
    let Value::Map(top) = &tree else {
        panic!("{tree:?}")
    };
    assert_eq!(
        top["outer"],
        Value::Array(vec![
            map1("inner", Value::Array(vec![inner(1), inner(2)])),
            map1("inner", inner(3)),
        ]),
    );
    let instance = &top["resource"];
    let content = |a: i64| map1("a", Value::Int(a));
    assert_eq!(
        instance,
        &map1(
            "aws_instance",
            map1(
                "x",
                map(vec![
                    (
                        "dynamic",
                        map1(
                            "ingress",
                            Value::Array(vec![
                                map(vec![
                                    ("for_each", Value::String("one".into())),
                                    ("content", content(1)),
                                ]),
                                map(vec![
                                    ("for_each", Value::String("two".into())),
                                    ("content", Value::Array(vec![content(2), content(3)])),
                                ]),
                            ]),
                        ),
                    ),
                    (
                        "provisioner",
                        map1(
                            "local-exec",
                            Value::Array(vec![
                                map1("command", Value::String("one".into())),
                                map1("command", Value::String("two".into())),
                            ]),
                        ),
                    ),
                ]),
            ),
        ),
    );
}

#[test]
fn an_attribute_holding_a_list_of_objects_stays_an_attribute() {
    let source = "ingress = [\n  {\n    \"p\" = 1\n  },\n  {\n    \"p\" = 2\n  }\n]\n\
                  one = {\n  \"p\" = 1\n}\n";
    round_trip(source, source);
}

#[test]
fn a_key_read_as_blocks_stays_blocks_unless_it_lost_the_shape() {
    // What a `set --jq` may leave at a key read as blocks: one body left in
    // the list is one block, which reads back as the body alone; an empty
    // label map is no block, so the key goes; a scalar is an attribute.
    let mut doc = parse_hcl("x {\n  a = 1\n}\nx {\n  a = 2\n}\np \"l\" {}\ns {}\n");
    let Value::Map(top) = doc.value_mut() else {
        panic!()
    };
    top.insert("x".into(), Value::Array(vec![map1("a", Value::Int(1))]));
    top.insert("p".into(), map(vec![]));
    top.insert("s".into(), Value::String("v".into()));
    let written = write_hcl(&doc);
    assert_eq!(written, "x {\n  a = 1\n}\ns = \"v\"\n");
    assert_eq!(
        parse_hcl(&written).value(),
        &map(vec![
            ("x", map1("a", Value::Int(1))),
            ("s", Value::String("v".into())),
        ])
    );
    // A third body appended to two is three blocks.
    let Value::Map(top) = doc.value_mut() else {
        panic!()
    };
    top.insert(
        "x".into(),
        Value::Array((1..=3).map(|a| map1("a", Value::Int(a))).collect()),
    );
    assert_eq!(
        write_hcl(&doc),
        "x {\n  a = 1\n}\nx {\n  a = 2\n}\nx {\n  a = 3\n}\ns = \"v\"\n"
    );
}

#[test]
fn repeated_bodies_that_disagree_on_a_nested_key_still_read_back_the_same() {
    // `y` has one label in the first `x` and none in the second: the
    // writer cannot write both as blocks of one shape, so it writes `y` as
    // attributes, which read back as the same tree.
    for source in [
        "x {\n  y \"a\" {\n    v = 1\n  }\n}\nx {\n  y {\n    a = 2\n  }\n}\n",
        "x {\n  y {\n    v = 1\n  }\n}\nx {\n  y = {\n    v = 2\n  }\n}\n",
    ] {
        let doc = parse_hcl(source);
        let written = write_hcl(&doc);
        assert_eq!(parse_hcl(&written).value(), doc.value(), "{written}");
    }
}

#[test]
fn a_body_whose_keys_one_map_cannot_hold_apart_is_a_parse_error() {
    for (source, at, message) in [
        (
            "x = 1\nx {\n  b = 2\n}\n",
            (2, 1),
            "HCL error: block \"x\" has the name of an attribute of the \
             same body; dq reads both as the key \"x\" and would lose one",
        ),
        (
            "x {\n  b = 2\n}\nx = 1\n",
            (4, 1),
            "HCL error: attribute \"x\" has the name of a block of the same \
             body; dq reads both as the key \"x\" and would lose one",
        ),
        (
            "x \"a\" {\n  k = 1\n}\n  x {\n  a = 2\n}\n",
            (4, 3),
            "HCL error: block \"x\" has 0 labels here and 1 label on an \
             earlier \"x\" block of the same body; dq nests one map level per label and would \
             lose one",
        ),
        (
            "o {\n  x \"a\" \"b\" {}\n  x \"c\" {}\n}\n",
            (3, 3),
            "HCL error: block \"x\" has 1 label here and 2 labels on an \
             earlier \"x\" block of the same body; dq nests one map level per label and would \
             lose one",
        ),
    ] {
        let err = hcl().parse(source.as_bytes()).expect_err(source);
        let dq_core::Error::Parse {
            message: got,
            line,
            col,
            snippet,
            ..
        } = &err
        else {
            panic!("{err:?}")
        };
        assert_eq!((got.as_str(), (*line, *col)), (message, at), "{source}");
        assert_eq!(
            Some(snippet.as_str()),
            source.lines().nth(*line as usize - 1),
            "{source}"
        );
    }
    // Blocks of one type in two different bodies are no clash.
    parse_hcl("x \"a\" {}\ny {\n  x {}\n}\n");
}
