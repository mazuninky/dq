//! Native-writer throughput benchmarks per registered format.
//!
//! For each format we synthesise a workload, parse once **outside the
//! timed region**, then re-emit per iteration via [`dq_core::Format::write`].
//! The timed region therefore measures only the writer — no parser cost
//! leaks in. Throughput is reported in output bytes per second.
//!
//! Sizes mirror `parse.rs`: `[100, 1_000, 10_000]` records (xml at
//! `[100, 1_000]`).

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use dq_core::{Document, Format};
use rand::{Rng, SeedableRng, rngs::StdRng};

const SIZES: &[usize] = &[100, 1_000, 10_000];

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

/// Parse `input_bytes` with the registered format under `format_name`,
/// returning the [`Document`] used as the input to every timed write.
fn pre_parse(format_name: &str, input_bytes: &[u8]) -> Document {
    let fmt = dq_core::format::by_name(format_name)
        .unwrap_or_else(|| panic!("{format_name} format registered"));
    fmt.parse(input_bytes).expect("parse pre-bench input")
}

fn bench_write_format(
    c: &mut Criterion,
    format_name: &'static str,
    sizes: &[usize],
    synth: impl Fn(usize) -> Vec<u8>,
) {
    let group_name = format!("write/{format_name}");
    let mut group = c.benchmark_group(&group_name);
    let fmt: &dyn Format = dq_core::format::by_name(format_name)
        .unwrap_or_else(|| panic!("{format_name} format registered"));
    for &n in sizes {
        let input_bytes = synth(n);
        let doc = pre_parse(format_name, &input_bytes);
        // Use the parsed-input size (not the writer output size) as the
        // throughput unit — output size depends on the format's
        // canonicalisation behaviour and would make cross-format numbers
        // hard to interpret. Parse-input bytes are the closest stable
        // proxy for "amount of work to re-emit".
        group.throughput(Throughput::Bytes(input_bytes.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &doc, |b, doc| {
            b.iter(|| {
                let mut buf: Vec<u8> = Vec::new();
                fmt.write(black_box(doc), &mut buf).expect("write");
                black_box(buf)
            });
        });
    }
    group.finish();
}

fn bench_write_json(c: &mut Criterion) {
    bench_write_format(c, "json", SIZES, synth_json);
}

fn bench_write_yaml(c: &mut Criterion) {
    bench_write_format(c, "yaml", SIZES, synth_yaml);
}

fn bench_write_toml(c: &mut Criterion) {
    bench_write_format(c, "toml", SIZES, synth_toml);
}

fn bench_write_jsonl(c: &mut Criterion) {
    bench_write_format(c, "jsonl", SIZES, synth_jsonl);
}

fn bench_write_hcl(c: &mut Criterion) {
    bench_write_format(c, "hcl", SIZES, synth_hcl);
}

fn bench_write_csv(c: &mut Criterion) {
    bench_write_format(c, "csv", SIZES, synth_csv);
}

fn bench_write_xml(c: &mut Criterion) {
    // Match parse.rs's smaller XML ladder for consistent reporting.
    bench_write_format(c, "xml", &[100, 1_000], synth_xml);
}

criterion_group!(
    benches,
    bench_write_json,
    bench_write_yaml,
    bench_write_toml,
    bench_write_jsonl,
    bench_write_hcl,
    bench_write_csv,
    bench_write_xml,
);
criterion_main!(benches);
