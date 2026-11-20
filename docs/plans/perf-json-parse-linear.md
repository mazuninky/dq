# Linear-time JSON span map

**Status: done.** The quadratic span builder is replaced, and a regression test guards against it coming back. What's left is recording the after-numbers, a changelog line, and optional follow-ups.

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

## Remaining work

- Record the after-numbers. Run `cargo bench -p dq-core --bench parse -- --quick` and add `parse/json/{100,1000,10000}` to the table above. The target is 300 ms or less at 10,000 records, the same order as YAML and TOML. The numbers have never been written down in the repo.
- Manual smoke test, still unrun: with a release build, `dq query '. | length'` on a 10,000-element compact array (for example `seq 1 10000 | jq -sc .`) should finish in under 200 ms.
- Changelog: `[Unreleased]` in [CHANGELOG.md](../../CHANGELOG.md) has no entry for this. Add a line saying large compact JSON now parses in milliseconds instead of seconds. No README change is needed; performance isn't part of the documented surface.
- Optional follow-ups, none blocking:
  - Make `pointer_for` incremental (push and pop on a `String` during the walk) instead of rebuilding the pointer per value. A small win on deep documents.
  - Audit the YAML span builder ([crates/dq-core/src/parsers/yaml_spans.rs](../../crates/dq-core/src/parsers/yaml_spans.rs)). It uses the `saphyr-parser` event API and probably has no similar bug, but `parse/yaml/10000` measured about 53 ms against a hoped-for ~30 ms.
  - If another parser needs offset-to-line lookups (Markdown front matter sometimes wants line numbers), move `LineIndex` into a shared `dq-core` module rather than copying it.
