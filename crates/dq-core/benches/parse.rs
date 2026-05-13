//! Parser throughput benchmarks per registered format.
//!
//! Each group dispatches through [`dq_core::format::by_name`] so we exercise
//! the same path the CLI uses (`dq … -F <format>`). Workloads are
//! synthesised in-file from a seeded RNG and timed in bytes, so the result
//! is reproducible and the relative ordering between formats is meaningful
//! even on a noisy machine.
//!
//! Sizes: `[100, 1_000, 10_000]` records — `large` is bracketed at
//! 10k because parsers like `csv` and `jsonl` are O(n) and the bench
//! statistics stabilise without pushing into >10s runs even on slower
//! hardware.

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use rand::{Rng, SeedableRng, rngs::StdRng};

/// Three input sizes per format. `xml` overrides with a smaller ladder
/// because its conventional-key shape (`@attrs` / `#text`) bloats the
/// per-record byte cost by ~3x compared to JSON.
const SIZES: &[usize] = &[100, 1_000, 10_000];

/// Build a `name=<word>,age=<i32>,email=<word>@example.com,active=<bool>`
/// record from a seeded RNG. Used as the cell-value source by every
/// per-format generator below so the shape of the workload is identical
/// across formats — only the encoding differs.
fn synth_record(rng: &mut StdRng, idx: usize) -> (String, i32, String, bool) {
    let names = [
        "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel",
    ];
    let name = names[rng.gen_range(0..names.len())];
    let age = rng.gen_range(18..90);
    let active = rng.r#gen::<bool>();
    let email = format!("{name}{idx}@example.com");
    (name.to_owned(), age, email, active)
}

fn synth_json(n: usize) -> Vec<u8> {
    let mut rng = StdRng::seed_from_u64(42);
    let mut items = Vec::with_capacity(n);
    for i in 0..n {
        let (name, age, email, active) = synth_record(&mut rng, i);
        items.push(serde_json::json!({
            "name": name,
            "age": age,
            "email": email,
            "active": active,
        }));
    }
    serde_json::to_vec(&items).expect("serialize json")
}

fn synth_yaml(n: usize) -> Vec<u8> {
    let mut rng = StdRng::seed_from_u64(42);
    let mut out = String::from("items:\n");
    for i in 0..n {
        let (name, age, email, active) = synth_record(&mut rng, i);
        out.push_str(&format!(
            "  - name: {name}\n    age: {age}\n    email: {email}\n    active: {active}\n"
        ));
    }
    out.into_bytes()
}

fn synth_toml(n: usize) -> Vec<u8> {
    let mut rng = StdRng::seed_from_u64(42);
    // Array-of-tables — the natural TOML shape for the same record list.
    let mut out = String::new();
    for i in 0..n {
        let (name, age, email, active) = synth_record(&mut rng, i);
        out.push_str(&format!(
            "[[items]]\nname = \"{name}\"\nage = {age}\nemail = \"{email}\"\nactive = {active}\n\n"
        ));
    }
    out.into_bytes()
}

fn synth_jsonl(n: usize) -> Vec<u8> {
    let mut rng = StdRng::seed_from_u64(42);
    let mut out = Vec::with_capacity(n * 80);
    for i in 0..n {
        let (name, age, email, active) = synth_record(&mut rng, i);
        let v = serde_json::json!({
            "name": name,
            "age": age,
            "email": email,
            "active": active,
        });
        out.extend_from_slice(&serde_json::to_vec(&v).expect("serialize jsonl line"));
        out.push(b'\n');
    }
    out
}

fn synth_hcl(n: usize) -> Vec<u8> {
    let mut rng = StdRng::seed_from_u64(42);
    let mut out = String::new();
    for i in 0..n {
        let (name, age, email, active) = synth_record(&mut rng, i);
        // Terraform-style resource blocks — exercises the most common
        // shape the HCL parser sees in the wild.
        out.push_str(&format!(
            "resource \"example_user\" \"user_{i}\" {{\n  name   = \"{name}\"\n  age    = {age}\n  email  = \"{email}\"\n  active = {active}\n}}\n\n"
        ));
    }
    out.into_bytes()
}

fn synth_csv(n: usize) -> Vec<u8> {
    let mut rng = StdRng::seed_from_u64(42);
    let mut out = String::from("name,age,email,active\n");
    for i in 0..n {
        let (name, age, email, active) = synth_record(&mut rng, i);
        out.push_str(&format!("{name},{age},{email},{active}\n"));
    }
    out.into_bytes()
}

/// XML workload — `<items><item>…</item>…</items>`. The conventional-key
/// mapping (`@attrs`, `#text`) means parse cost grows faster per record
/// than the other formats; we cap the ladder at 1k records (vs 10k for
/// the rest) so `--quick` runs stay under a couple of seconds.
fn synth_xml(n: usize) -> Vec<u8> {
    let mut rng = StdRng::seed_from_u64(42);
    let mut out = String::from("<?xml version=\"1.0\"?>\n<items>\n");
    for i in 0..n {
        let (name, age, email, active) = synth_record(&mut rng, i);
        out.push_str(&format!(
            "  <item id=\"{i}\"><name>{name}</name><age>{age}</age><email>{email}</email><active>{active}</active></item>\n"
        ));
    }
    out.push_str("</items>\n");
    out.into_bytes()
}

/// Generic per-format runner — accepts the format short name and a
/// workload synthesiser so each `bench_parse_*` is one call instead of a
/// 25-line group definition.
fn bench_parse_format(
    c: &mut Criterion,
    format_name: &'static str,
    sizes: &[usize],
    synth: impl Fn(usize) -> Vec<u8>,
) {
    let group_name = format!("parse/{format_name}");
    let mut group = c.benchmark_group(&group_name);
    let fmt = dq_core::format::by_name(format_name)
        .unwrap_or_else(|| panic!("{format_name} format registered"));
    for &n in sizes {
        let bytes = synth(n);
        group.throughput(Throughput::Bytes(bytes.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &bytes, |b, bytes| {
            b.iter(|| {
                let doc = fmt.parse(black_box(bytes.as_slice())).expect("parse");
                black_box(doc)
            });
        });
    }
    group.finish();
}

fn bench_parse_json(c: &mut Criterion) {
    bench_parse_format(c, "json", SIZES, synth_json);
}

fn bench_parse_yaml(c: &mut Criterion) {
    bench_parse_format(c, "yaml", SIZES, synth_yaml);
}

fn bench_parse_toml(c: &mut Criterion) {
    bench_parse_format(c, "toml", SIZES, synth_toml);
}

fn bench_parse_jsonl(c: &mut Criterion) {
    bench_parse_format(c, "jsonl", SIZES, synth_jsonl);
}

fn bench_parse_hcl(c: &mut Criterion) {
    bench_parse_format(c, "hcl", SIZES, synth_hcl);
}

fn bench_parse_csv(c: &mut Criterion) {
    bench_parse_format(c, "csv", SIZES, synth_csv);
}

fn bench_parse_xml(c: &mut Criterion) {
    // Smaller ladder — see [`synth_xml`] doc.
    bench_parse_format(c, "xml", &[100, 1_000], synth_xml);
}

criterion_group!(
    benches,
    bench_parse_json,
    bench_parse_yaml,
    bench_parse_toml,
    bench_parse_jsonl,
    bench_parse_hcl,
    bench_parse_csv,
    bench_parse_xml,
);
criterion_main!(benches);
