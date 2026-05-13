//! End-to-end lint-evaluator benchmarks.
//!
//! Four concerns:
//!
//! - **`evaluate/ruleset_from_str`** — parse cost of a synthetic 10-rule
//!   YAML stream via [`RuleSet::from_str`]. Isolates the YAML→`Rule`
//!   deserialize cost from the jq-compile pipeline.
//! - **`evaluate/ruleset_from_std`** — embedded `@std/<namespace>` lookup
//!   via [`RuleSet::from_std`]. Trivially small but useful as a baseline
//!   for the rule-cost ratio.
//! - **`evaluate/evaluator_new`** — `Evaluator::new(vec![rs])` for the
//!   `@std/k8s` ruleset. This is where every rule's `match.filter` and
//!   `check.jq` are compiled — the dominant cost of warming up a lint
//!   run.
//! - **`evaluate/evaluate_file`** — `Evaluator::evaluate_file` against
//!   synth k8s-shaped YAML at small/mid/large. Times the per-file
//!   evaluation against a pre-compiled evaluator (the cost a CI lint
//!   pipeline pays per file once the evaluator is warm).

use camino::Utf8PathBuf;
use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use dq_exec::{Evaluator, RuleSet, RuleSource};

/// Ten inline rules — minimum payload for the `from_str` bench. Each
/// rule has a trivially-passing `check.jq` (`null` / `false`) so the
/// runtime cost is dominated by the parser, not the evaluator.
const TEN_RULE_YAML: &str = r#"
id: bench.r01
description: rule 01
severity: info
match:
  format: yaml
check:
  jq: 'null'
  message: 'm1'
---
id: bench.r02
description: rule 02
severity: info
match:
  format: yaml
check:
  jq: 'null'
  message: 'm2'
---
id: bench.r03
description: rule 03
severity: info
match:
  format: yaml
check:
  jq: 'null'
  message: 'm3'
---
id: bench.r04
description: rule 04
severity: info
match:
  format: yaml
check:
  jq: 'null'
  message: 'm4'
---
id: bench.r05
description: rule 05
severity: info
match:
  format: yaml
check:
  jq: 'null'
  message: 'm5'
---
id: bench.r06
description: rule 06
severity: info
match:
  format: yaml
check:
  jq: 'null'
  message: 'm6'
---
id: bench.r07
description: rule 07
severity: info
match:
  format: yaml
check:
  jq: 'null'
  message: 'm7'
---
id: bench.r08
description: rule 08
severity: info
match:
  format: yaml
check:
  jq: 'null'
  message: 'm8'
---
id: bench.r09
description: rule 09
severity: info
match:
  format: yaml
check:
  jq: 'null'
  message: 'm9'
---
id: bench.r10
description: rule 10
severity: info
match:
  format: yaml
check:
  jq: 'null'
  message: 'm10'
"#;

/// Synth k8s-shaped YAML — a `Deployment` with `n` containers under
/// `.spec.template.spec.containers`. The shape exercises the `@std/k8s`
/// rules (every rule's `match.filter` targets `Deployment` /
/// `StatefulSet` / `DaemonSet` / `Pod`) without copying real-world
/// manifests into the bench file.
fn synth_k8s_yaml(container_count: usize) -> String {
    let mut out = String::from(
        "apiVersion: apps/v1\n\
         kind: Deployment\n\
         metadata:\n  \
         name: bench\n  \
         namespace: default\n\
         spec:\n  \
         replicas: 1\n  \
         selector:\n    \
         matchLabels:\n      \
         app: bench\n  \
         template:\n    \
         metadata:\n      \
         labels:\n        \
         app: bench\n    \
         spec:\n      \
         containers:\n",
    );
    for i in 0..container_count {
        out.push_str(&format!(
            "        - name: c{i}\n          image: ghcr.io/example/c{i}:1.0.0\n"
        ));
    }
    out
}

fn parse_yaml_doc(src: &str) -> dq_core::Document {
    let fmt = dq_core::format::by_name("yaml").expect("yaml registered");
    fmt.parse(src.as_bytes()).expect("parse yaml")
}

fn bench_ruleset_from_str(c: &mut Criterion) {
    c.bench_function("evaluate/ruleset_from_str", |b| {
        b.iter(|| {
            let rs = RuleSet::from_str(black_box(TEN_RULE_YAML), RuleSource::Inline)
                .expect("parse 10-rule yaml");
            black_box(rs)
        });
    });
}

fn bench_ruleset_from_std(c: &mut Criterion) {
    c.bench_function("evaluate/ruleset_from_std", |b| {
        b.iter(|| {
            let rs = RuleSet::from_std(black_box("k8s")).expect("load @std/k8s");
            black_box(rs)
        });
    });
}

fn bench_evaluator_new(c: &mut Criterion) {
    c.bench_function("evaluate/evaluator_new", |b| {
        b.iter(|| {
            // Re-load the ruleset each iter so the `from_std` setup cost
            // is part of the timed region — `Evaluator::new` consumes
            // the ruleset by value, so a `clone()` outside the loop
            // would distort the comparison. The `from_std` cost is
            // already separately bench'd above and is small relative
            // to `Evaluator::new`'s jq compilation.
            let rs = RuleSet::from_std("k8s").expect("load @std/k8s");
            let ev = Evaluator::new(vec![rs]).expect("compile evaluator");
            black_box(ev)
        });
    });
}

fn bench_evaluate_file(c: &mut Criterion) {
    let mut group = c.benchmark_group("evaluate/evaluate_file");
    // Compile the evaluator once outside the timed region — the bench
    // measures only the per-file evaluation cost, which is what a CI
    // pipeline pays after warm-up.
    let rs = RuleSet::from_std("k8s").expect("load @std/k8s");
    let ev = Evaluator::new(vec![rs]).expect("compile evaluator");
    let path = Utf8PathBuf::from("synth-deployment.yaml");
    for &n in &[1usize, 10, 100] {
        let yaml = synth_k8s_yaml(n);
        let doc = parse_yaml_doc(&yaml);
        group.bench_with_input(BenchmarkId::from_parameter(n), &doc, |b, doc| {
            b.iter(|| {
                let ir = doc.as_ir();
                let diags = ev.evaluate_file(black_box(&path), black_box(&ir), "yaml");
                black_box(diags)
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_ruleset_from_str,
    bench_ruleset_from_std,
    bench_evaluator_new,
    bench_evaluate_file,
);
criterion_main!(benches);
