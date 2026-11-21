# Linear-time JSON span map

**Status: shipped in 2026.20.2, archived 2026-11-21.** The quadratic span builder is replaced, a regression test guards against it coming back, and the after-numbers are below. The summary is in [decisions.md](../decisions.md#2026-11-21--linear-json-spans-cached-jq-compiles-in-place-pointer-walks-perf).

## Problem

The criterion parse bench ([crates/dq-core/benches/parse.rs](../../crates/dq-core/benches/parse.rs), `cargo bench -p dq-core --bench parse -- --quick`, release build, M1 Pro) showed JSON parsing growing quadratically:

| records | time | per record |
|---|---|---|
| 100 | 1.67 ms | 16.7 µs |
| 1,000 | 143 ms | 143 µs |
| 10,000 | **13.94 s** | 1.4 ms |

Ten times the data took about a hundred times as long (step ratios ×86 and ×97). YAML and TOML on the same data scaled linearly: about 50 ms and 65 ms at 10,000 records. So `dq query`, `dq lint` and `dq fix` on JSON with tens of thousands of records (event exports, SBOMs, a large `package-lock.json`) spent tens of seconds just parsing, where a few hundred milliseconds would do.

### Root cause

The JSON parser in [crates/dq-core/src/parsers/json.rs](../../crates/dq-core/src/parsers/json.rs) works in two passes:

1. `serde_json::from_slice` builds the value. This is upstream code and linear.
2. `build_span_map` walks the bytes again with dq's own `Scanner` and records a `ValueSpan` (value range, line range, indent, context) for every node, keyed by its RFC 6901 pointer.

The quadratic part was in pass 2. `record_scalar` and `record_empty_container` called two helpers, `compute_line_range` and `compute_indent`, and each scanned **backwards** from the value to the previous `\n` or the start of the buffer. In compact single-line JSON (`[v0, v1, …]`), element k sits at about k × record width, so its backward scan covers about k × record width bytes. Summed over N elements that is O(N²): about 50 million backward steps for 10,000 elements. Pretty-printed JSON didn't suffer, because a newline is always a few bytes back. The bench serialises with `serde_json::to_vec` (compact), so it hit the worst case.

## Design

### `LineIndex`

A private type inside `parsers/json.rs`, built once in `build_span_map` before the scan and handed to `Scanner::new` (field `Scanner.lines`):

- `newline_offsets: Vec<usize>`: sorted byte offsets of every `\n`. Line 0 starts at offset 0; line `i` starts at `newline_offsets[i - 1] + 1`.
- `indents: Vec<OnceCell<u32>>`: one cell per line (`newline_offsets.len() + 1`, counting a final line without `\n`), filled lazily.

Lookups use `slice::partition_point`:

- `line_start(offset)`: start of the line containing `offset`. O(log L).
- `line_end(offset, total)`: start of the next line, so the trailing `\n` is included, or `total` on the last line. This matches the old helper's `end += 1` behaviour.
- `indent_for(bytes, offset)`: number of leading spaces and tabs on that line, computed on first use and cached. A JSON value never starts with whitespace, so every value on a line has the same indent.

`record_scalar` and `record_empty_container` now compute `line_range = line_start(start)..line_end(end, len)` and `indent = indent_for(start)`. The old helpers are deleted.

Complexity: O(B + N log L) for a buffer of B bytes, N recorded values and L lines. Compact single-line JSON (L = 1) is O(N). Pretty-printed JSON (L ≈ N) is O(N log N). Both scale like YAML and TOML.

The newline table is built with a plain iterator over the bytes, not `memchr::memchr_iter`. `memchr` reaches the build only as a transitive dependency, and using it directly would mean adding it to `Cargo.toml`. The scan costs little next to `serde_json::from_slice`.

### What stays the same

- `Json::parse` returns the same document. SpanMap keys are the same RFC 6901 pointers, and every span is byte-for-byte what it was before. Snapshot tests, textual edits (`dq set -i`, `dq del -i`, `dq fmt -i`) and lint diagnostic locations are unaffected.
- The rest of `Scanner` (object, array and whitespace scanning) is untouched.
- `pointer_for(path)` still builds the pointer string from the path for every value. That is O(depth) per value: fine for realistic depths (under about 16) and nowhere near the old bottleneck.
- `derive_line_col` and `extract_line_snippet` still scan linearly, but they run once, when rendering a parse error.

Memory: the table holds one `usize` per newline plus one cell per line. That is about 160 KB for a 10,000-line file and about 240 KB for a pretty-printed 10,000-element array, small next to the value and SpanMap the parser already holds.

### Alternatives rejected

- **Use `serde_json::Deserializer` positions.** They give the parser's current position, not a position per value node, so a separate tokenizer would still be needed. Other formats (Markdown front matter, XML) use the same two-pass pattern, "semantic parse, then a byte walk for spans", and JSON shouldn't be the exception.
- **Build the SpanMap lazily, on first use.** `dq lint` resolves a span for every diagnostic, and `query` / `get` / `fmt` need spans for round-trip writes. This moves the cost rather than removing it.
- **Write a single-pass JSON parser that emits spans directly.** `serde_json` already handles the corner cases (numbers, escapes, surrogate pairs). Rewriting that is months of work with real regression risk. The two-pass design is sound; only the helpers were wrong.

## What landed

- `LineIndex` and the rewired `record_scalar` / `record_empty_container` in [crates/dq-core/src/parsers/json.rs](../../crates/dq-core/src/parsers/json.rs). No `compute_line_range` / `compute_indent` definitions remain.
- Regression test [crates/dq-core/tests/parse_json_perf_smoke.rs](../../crates/dq-core/tests/parse_json_perf_smoke.rs):
  - `parses_10k_element_flat_array_under_1s` covers the compact case that used to take about 14 s.
  - `parses_10k_element_pretty_array_under_1s` covers the pretty-printed case and passed before the fix as well.
  - Each test parses a 10,000-element array under the dev profile and fails above 1.0 s. That is about three times the ~250–300 ms target, leaving headroom for slow CI runners, and the failure message points at the span builder.
- Existing span and round-trip tests ([crates/dq-core/tests/parse_json.rs](../../crates/dq-core/tests/parse_json.rs)) and CLI snapshots are unchanged.

## Measured

`cargo bench -p dq-core --bench parse -- --quick` on 2026-11-21, release profile, Apple M4 Pro with other work running. The before-numbers above are from an M1 Pro; on the M4 Pro one jq compile (`jq/compile/identity`) takes 193 µs against 346 µs there, so this machine is roughly 1.8× faster.

| records | JSON | YAML | TOML |
|---|---|---|---|
| 100 | 142 µs | 236 µs | 206 µs |
| 1,000 | 1.56 ms | 2.39 ms | 2.07 ms |
| 10,000 | 18.1 ms | 26.1 ms | 28.0 ms |

- JSON is linear: ×11 and ×11.6 per tenfold step, and 10,000 records take 18 ms against the 300 ms target and the 13.9 s before.
- Manual smoke test with the released `dq` 2026.47.3 and `hyperfine`: `dq query '. | length'` on a 10,000-element compact array (`seq 1 10000 | jq -sc .`) runs in 6.7 ms end to end (target: under 200 ms); on 10,000 compact records of four fields (568 KB) in 37 ms.
- Changelog: a line under 2026.20.2, the first release with this change.

Not done, and why:

- An incremental `pointer_for`: still open, listed in [../roadmap.md](../roadmap.md).
- The YAML span-builder audit: dropped. `parse/yaml` scales linearly (×10, ×11) and sits within 1.5× of JSON on the same machine, so the 53 ms that prompted it was the slower machine, not a bug.
- A shared `LineIndex`: no second parser needs offset-to-line lookups yet.
