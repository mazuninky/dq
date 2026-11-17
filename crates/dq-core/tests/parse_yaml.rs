//! Component tests for the YAML parser against representative real-world shapes.
//!
//! Each fixture exercises a different category of YAML the spec mentions
//! (Kubernetes, Helm, GitHub Actions, Hugo frontmatter, multi-document
//! streams). The fixtures live under `tests/fixtures/yaml/` and are embedded
//! via `include_str!` so the test binary doesn't depend on the working
//! directory.

use dq_core::{Document, Pointer, Value};
use pretty_assertions::assert_eq;

const K8S_DEPLOYMENT: &str = include_str!("fixtures/yaml/k8s_deployment.yaml");
const HELM_VALUES: &str = include_str!("fixtures/yaml/helm_values.yaml");
const GITHUB_ACTIONS: &str = include_str!("fixtures/yaml/github_actions.yaml");
const HUGO_FRONTMATTER: &str = include_str!("fixtures/yaml/hugo_frontmatter.yaml");
const MULTI_DOC: &str = include_str!("fixtures/yaml/multi_doc.yaml");

/// Parse `s` via the registry-resolved YAML format and return the `Document`.
fn parse_yaml(s: &str) -> Document {
    let fmt = dq_core::by_name("yaml").expect("yaml format must be registered");
    fmt.parse(s.as_bytes())
        .unwrap_or_else(|e| panic!("yaml parse failed: {e}"))
}

/// Resolve `pointer` against the document's single root, panicking on miss.
/// Useful sugar for the "fetch a known leaf" assertion shape.
fn get<'a>(doc: &'a Document, pointer: &str) -> &'a Value {
    let p = Pointer::parse(pointer).expect("pointer parses");
    if doc.is_multi() {
        panic!("get() called on multi-doc; use multi-doc helpers instead");
    }
    p.resolve(doc.value())
        .unwrap_or_else(|e| panic!("resolve failed for `{pointer}`: {e}"))
}

#[test]
fn k8s_deployment_resolves_known_leaves() {
    let doc = parse_yaml(K8S_DEPLOYMENT);

    // Top-level fields should be reachable under their own segments.
    assert_eq!(get(&doc, "/kind"), &Value::String("Deployment".into()));
    assert_eq!(get(&doc, "/spec/replicas"), &Value::Int(3));

    // Annotation keys contain `/` which must be escaped as `~1` in pointers.
    assert_eq!(
        get(&doc, "/metadata/annotations/deploy.dq.test~1owner"),
        &Value::String("platform-team".into()),
        "inline-comment line must still produce the right scalar"
    );

    // Label keys with `.` and `/` round-trip through the pointer parser.
    assert_eq!(
        get(&doc, "/metadata/labels/app.kubernetes.io~1name"),
        &Value::String("web".into()),
    );

    // Container image — array index addressing.
    assert_eq!(
        get(&doc, "/spec/template/spec/containers/0/image"),
        &Value::String("ghcr.io/example/web:1.2.3".into()),
    );
}

#[test]
fn helm_values_lists_of_strings_and_nested_objects() {
    let doc = parse_yaml(HELM_VALUES);

    // Nested-object access.
    assert_eq!(
        get(&doc, "/image/repository"),
        &Value::String("registry.example.test/svc".into()),
    );
    assert_eq!(
        get(&doc, "/resources/limits/cpu"),
        &Value::String("500m".into()),
    );
    assert_eq!(get(&doc, "/service/port"), &Value::Int(80));

    // List of strings — `extraEnv[1]` is `METRICS_PORT=9090`.
    assert_eq!(
        get(&doc, "/extraEnv/1"),
        &Value::String("METRICS_PORT=9090".into()),
    );
}

#[test]
fn github_actions_workflow_parses_multiline_run_block() {
    let doc = parse_yaml(GITHUB_ACTIONS);

    // The `on` key is a YAML special — used to be parsed as boolean `true` by
    // some YAML 1.1 parsers. Make sure ours sees it as the string key with the
    // nested map value (`push.branches`), not a coerced boolean leaf.
    assert!(
        matches!(get(&doc, "/on"), Value::Map(_)),
        "expected `/on` to resolve to a Map (YAML 1.1 boolean coercion regression check), got: {:?}",
        get(&doc, "/on"),
    );
    // Drill into the `on` map to confirm the nested branches list survived.
    assert_eq!(
        get(&doc, "/on/push/branches/0"),
        &Value::String("main".into()),
        "/on must round-trip as a Map, not a coerced boolean",
    );
    // Cross-check: the workflow name still parses as a string.
    assert!(matches!(get(&doc, "/name"), Value::String(s) if s == "ci"));

    // The literal block `|` of a `run:` step preserves embedded newlines.
    let run = get(&doc, "/jobs/test/steps/1/run");
    let Value::String(s) = run else {
        panic!("expected run to be a string, got: {run:?}");
    };
    assert!(
        s.contains("cargo build --workspace --all-features"),
        "build line missing from run script:\n---\n{s}\n---"
    );
    assert!(
        s.contains("cargo test --workspace --all-features"),
        "test line missing from run script:\n---\n{s}\n---"
    );
}

#[test]
fn hugo_frontmatter_head_parses_with_typed_scalars() {
    let doc = parse_yaml(HUGO_FRONTMATTER);

    assert_eq!(
        get(&doc, "/title"),
        &Value::String("Hello, dq".into()),
        "string scalar"
    );
    // YAML 1.2 booleans must come through as `Value::Bool(false)`, not the
    // string `"false"`.
    assert_eq!(get(&doc, "/draft"), &Value::Bool(false));
    // Integer scalar.
    assert_eq!(get(&doc, "/weight"), &Value::Int(5));
    // Tag list.
    assert_eq!(get(&doc, "/tags/0"), &Value::String("rust".into()));
    assert_eq!(get(&doc, "/tags/2"), &Value::String("dq".into()));
}

#[test]
fn multi_doc_yaml_returns_two_documents() {
    let doc = parse_yaml(MULTI_DOC);
    let docs = doc
        .values()
        .unwrap_or_else(|| panic!("expected multi-doc, got: {doc:?}"));
    assert_eq!(docs.len(), 2, "expected exactly two documents");

    // First document: ConfigMap.
    let first = &docs[0];
    let p_kind = Pointer::parse("/kind").unwrap();
    assert_eq!(
        p_kind.resolve(first).unwrap(),
        &Value::String("ConfigMap".into())
    );
    let p_log = Pointer::parse("/data/log_level").unwrap();
    assert_eq!(p_log.resolve(first).unwrap(), &Value::String("info".into()));

    // Second document: Service. Verify the array index reaches the right node.
    let second = &docs[1];
    assert_eq!(
        p_kind.resolve(second).unwrap(),
        &Value::String("Service".into())
    );
    let p_port = Pointer::parse("/spec/ports/0/targetPort").unwrap();
    assert_eq!(p_port.resolve(second).unwrap(), &Value::Int(8080));
}
