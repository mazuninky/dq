//! Comment-preserving textual-edit benchmarks: [`Document::set_at`] on
//! YAML (saphyr-parser span path) and TOML (toml_edit span path).
//!
//! Two variables per format:
//!
//! - **shallow** pointer (`/metadata/name` for k8s, `/package/name` for TOML)
//! - **deep** pointer (nested inside a container list / sub-table)
//!
//! Each variant clones the parsed [`Document`] at the start of every
//! iteration so the timed region measures **one** `set_at` call against
//! a fresh source buffer — without that, the second iteration would
//! see a re-emitted buffer whose span map has been regenerated and the
//! numbers would drift.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use dq_core::{Document, Pointer, Value};

/// Hand-written k8s deployment fixture — same shape as
/// `crates/dq-cli/tests/fixtures/k8s_deployment_writable.yaml` but
/// inlined via `include_str!` so the bench file has no IO at startup.
const K8S_YAML: &str = include_str!("../../dq-cli/tests/fixtures/k8s_deployment_writable.yaml");

/// Synthetic TOML fixture — Cargo-style `[package]` with one nested
/// `[package.metadata]` sub-table. Inlined here (rather than pulling
/// from `examples/`) so the bench is self-contained.
const TOML_SRC: &str = r#"# Bench-fixture for textual-edit/toml.
[package]
name = "demo"
version = "0.1.0"
edition = "2024"

[package.metadata]
maintainer = "ops"
description = "demo crate"

[dependencies]
serde = "1"
toml = "1.1"

[[bin]]
name = "demo"
path = "src/main.rs"
"#;

fn parse_yaml(src: &[u8]) -> Document {
    // YAML's registered `Format::parse` builds a `value_only` document
    // with no spans / original-bytes (M1 read path). For textual-edit
    // benches we need the **write-aware** parser
    // [`dq_core::parse_yaml_with_spans`], which goes through the
    // saphyr-parser span builder and produces a `Document` whose
    // `set_at` calls splice into the byte buffer in place — the path
    // this bench exists to measure.
    dq_core::parse_yaml_with_spans(src).expect("parse yaml with spans")
}

fn parse_toml(src: &[u8]) -> Document {
    let fmt = dq_core::format::by_name("toml").expect("toml registered");
    fmt.parse(src).expect("parse toml")
}

fn bench_textual_edit(c: &mut Criterion) {
    // Build the prototype docs once; per-iter we `.clone()` so each
    // iteration starts from the same span map state.
    let yaml_doc = parse_yaml(K8S_YAML.as_bytes());
    let toml_doc = parse_toml(TOML_SRC.as_bytes());

    let yaml_shallow = Pointer::parse("/metadata/name").expect("parse pointer");
    // Deep pointer reaches into the first container of the spec.template
    // pod template — exercises the saphyr-span lookup all the way through
    // a multi-level mapping nest.
    let yaml_deep =
        Pointer::parse("/spec/template/spec/containers/0/image").expect("parse pointer");

    let toml_shallow = Pointer::parse("/package/name").expect("parse pointer");
    let toml_deep = Pointer::parse("/package/metadata/maintainer").expect("parse pointer");

    let mut group = c.benchmark_group("textual_edit");

    group.bench_function("yaml/shallow", |b| {
        b.iter(|| {
            let mut doc = yaml_doc.clone();
            doc.set_at(black_box(&yaml_shallow), Value::String("new".to_owned()))
                .expect("set_at yaml shallow");
            black_box(doc)
        });
    });

    group.bench_function("yaml/deep", |b| {
        b.iter(|| {
            let mut doc = yaml_doc.clone();
            doc.set_at(
                black_box(&yaml_deep),
                Value::String("registry.example.test/svc:9.9.9".to_owned()),
            )
            .expect("set_at yaml deep");
            black_box(doc)
        });
    });

    group.bench_function("toml/shallow", |b| {
        b.iter(|| {
            let mut doc = toml_doc.clone();
            doc.set_at(
                black_box(&toml_shallow),
                Value::String("renamed".to_owned()),
            )
            .expect("set_at toml shallow");
            black_box(doc)
        });
    });

    group.bench_function("toml/deep", |b| {
        b.iter(|| {
            let mut doc = toml_doc.clone();
            doc.set_at(
                black_box(&toml_deep),
                Value::String("platform-team".to_owned()),
            )
            .expect("set_at toml deep");
            black_box(doc)
        });
    });

    group.finish();
}

criterion_group!(benches, bench_textual_edit);
criterion_main!(benches);
