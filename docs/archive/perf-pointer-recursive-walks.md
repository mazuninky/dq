# Linear-cost pointers in recursive walks (`diff`, `merge`)

**Status: shipped in 2026.20.2, archived 2026-11-21.** The push/pop API is in, `diff` and `merge` use it, tests and benches cover it, and the numbers are below. Measuring deep documents found two other costs that grow faster than depth; they are open in [../roadmap.md](../roadmap.md). The summary is in [decisions.md](../decisions.md#2026-11-21--linear-json-spans-cached-jq-compiles-in-place-pointer-walks-perf).

## Problem

`Pointer::with_segment(&self, seg) -> Pointer` clones the whole `Vec<Segment>` and then pushes one segment, so each call costs O(current depth). A recursive walk that builds every child pointer this way pays 1 + 2 + … + d copies to reach a leaf at depth d. That is O(d²) per leaf and O(N × d²) for a walk over N leaves, where O(N × d) is the goal.

The `pointer/with_segment` bench in [crates/dq-core/benches/pointer.rs](../../crates/dq-core/benches/pointer.rs) built one chain to depth d:

| depth | time | ratio to depth 1 | linear would be |
|---|---|---|---|
| 1 | 65 ns | 1× | 1× |
| 4 | 750 ns | 11.5× | 4× |
| 16 | 8.3 µs | 125× | 16× |
| 64 | 115 µs | 1772× | 64× |

The hot paths were:

- `transform::diff` ([crates/dq-core/src/transform/diff.rs](../../crates/dq-core/src/transform/diff.rs)): six `with_segment` call sites inside the recursive walk over maps and arrays.
- `transform::merge` ([crates/dq-core/src/transform/merge.rs](../../crates/dq-core/src/transform/merge.rs)): one `with_segment` per patch key, called recursively.
- The plugin runtime ([crates/dq-plugin/src/runtime.rs](../../crates/dq-plugin/src/runtime.rs)) has two call sites, but they extend a pointer by one level and don't recurse. It's not a hot path.

Estimated effect: a typical Kubernetes Deployment (depth about 6, about 200 leaves) costs about 200 × 36 = 7,200 segment copies in `diff`, against about 1,200 for a linear walk. A deep Helm values file (depth about 10, about 500 leaves) costs about 50,000 copies, against about 5,000 plus constant push/pop work, a 10× gap. In wall time that is under 1 ms on ordinary manifests and a few milliseconds on large, deep values files. `dq diff`, `dq merge` and `dq patch` (through `apply_merge` / `apply_patch`) benefit. Output, exit codes and flags don't change.

## Design

### API

Two in-place methods on `Pointer` in [crates/dq-core/src/pointer.rs](../../crates/dq-core/src/pointer.rs), thin wrappers over `Vec::push` / `Vec::pop`:

- `push_segment(&mut self, seg: Segment)`: amortised O(1).
- `pop_segment(&mut self) -> Option<Segment>`: O(1). Returns `None` on the root pointer instead of panicking, mirroring `Vec::pop`.

They add no new invariant: any `Vec<Segment>` is a valid pointer, and RFC 6901 escaping happens in `as_canonical`, not in the structure.

`with_segment` stays as it is, still O(depth). It's the right shape for callers that need a fresh owned pointer while the parent stays untouched. The plugin runtime maps object keys to canonical strings from an immutable `&Pointer`, one level deep. Switching it to push/pop would need `&mut` there and would win nothing. The `pointer/with_segment` bench is meant to stay quadratic. It measures that method, which was never the thing to optimise.

### Walk pattern

`diff(a, b) -> Vec<PatchOp>` keeps its public signature. Internally it creates a root pointer and calls a private worker `diff_into(&mut Pointer, …)`, which hands off to `diff_maps` and `diff_arrays`. Each loop iteration pushes the key or index segment, recurses or emits an op, then pops. An emitted `Add` / `Remove` / `Replace` op takes `path.clone()`: one O(d) clone per emitted op, not per step of the walk.

`apply_merge(doc, patch)` also keeps its public signature and calls `merge_into(doc, &mut Pointer, patch)`.

Invariant for both walks: when a recursive call returns, the pointer is back in the state it was in on entry. In `merge_into` this includes the error path. The per-key step's `Result` is captured first, the segment is popped, and only then is `?` applied. Putting `?` directly on the recursive call would leave the pushed segment behind when an error propagates. A comment in the code says the same.

There is no RAII guard type that pops on `Drop`. With two recursive walks, balancing push and pop by hand is simpler to read. Revisit if more recursive walks adopt the pattern. `push_segment` has since been picked up by non-recursive code in [crates/dq-core/src/document/insert.rs](../../crates/dq-core/src/document/insert.rs) and [crates/dq-core/src/document/mod.rs](../../crates/dq-core/src/document/mod.rs).

### Alternatives rejected

- **`Pointer(Arc<Vec<Segment>>)` for cheap clones.** Pushing still needs `Arc::make_mut`, which copies whenever the refcount is above one. In `diff` the parent is always shared, so every step still copies: O(d) per call, as before. A single deep chain (the micro-bench) stays quadratic. Every query (`segments()`, `is_root()`, `as_canonical()`) would also pay an atomic refcount load, and hashing still walks the whole content.
- **`SmallVec<[Segment; 8]>`.** Up to depth 8 `with_segment` still copies the inline array, including a `String` clone per key segment. Beyond depth 8 it is quadratic again. Push/pop fixes both ranges at once.
- **A persistent vector (`im` / `imbl`).** Push is O(log n), worse than `Vec`'s O(1), and it only pays off when cheap clones are needed, which `diff` and `merge` don't use. It would also add a dependency to clear through `cargo deny`, and hash compatibility with `Vec<Segment>` would need checking.
- **`Segment` holding `Cow<str>` instead of `String`.** This addresses key allocation, a separate concern. Not needed here.

### Risks

- Correctness: low. The methods are direct `Vec` wrappers, and property tests check that push/pop builds the same pointer as a `with_segment` chain.
- API: additive only. `with_segment` remains, and no caller had to change.
- No new dependencies. No concurrency impact: a `Pointer` is never shared across threads without an explicit clone.

## What landed

- API and doc comments: `push_segment` / `pop_segment` in [crates/dq-core/src/pointer.rs](../../crates/dq-core/src/pointer.rs). The doc comment on `with_segment` points recursive walks at push/pop.
- `diff`: `diff_into`, `diff_maps` and `diff_arrays` take `&mut Pointer`. All former `with_segment` sites use push/pop ([diff.rs](../../crates/dq-core/src/transform/diff.rs)).
- `merge`: `merge_into` takes `&mut Pointer`, with the pop-before-`?` ordering described above ([merge.rs](../../crates/dq-core/src/transform/merge.rs)).
- The plugin runtime still uses `with_segment`, as designed.
- Unit tests in `pointer.rs`: `push_then_pop_round_trips`, `pop_on_empty_returns_none`, `push_segment_extends_segments`, `push_pop_balanced_walk_returns_to_default`.
- Property tests in [crates/dq-core/tests/prop_pointer.rs](../../crates/dq-core/tests/prop_pointer.rs): `push_pop_matches_with_segment`, `push_then_pop_is_identity`, `balanced_push_pop_returns_to_start`.
- Bench group `pointer/recursive_walk/{1,4,16,64}` in [crates/dq-core/benches/pointer.rs](../../crates/dq-core/benches/pointer.rs) models the walk: push d segments, render canonical, pop d segments. Rule of thumb from the design: if depth 64 costs more than 5× depth 16, treat it as a regression. Nothing enforces this; it is checked by hand. The expected curve runs from about 50 ns at depth 1 to about 4 µs at depth 64.

## Measured

`cargo bench -p dq-core --bench pointer -- --quick` and `--bench transform`, 2026-11-21, release profile, Apple M4 Pro with other work running (the `with_segment` table above is from an M1 Pro; the same-machine `with_segment` row below is the fair comparison).

| depth | `with_segment` chain | `recursive_walk` (push, render, pop) |
|---|---|---|
| 1 | 24 ns | 33 ns |
| 4 | 213 ns | 57 ns |
| 16 | 2.40 µs | 285 ns |
| 64 | 26.9 µs | 829 ns |

Depth 64 costs 2.9× depth 16, inside the 5× rule of thumb, and 32× less than a `with_segment` chain.

Two groups now vary depth with a fixed 256 leaves (16 × 16 maps, then single-key maps down to depth d), as this page asked:

| depth | `transform/diff_deep`, every leaf changed | `transform/apply_merge_deep`, 10% of leaves changed |
|---|---|---|
| 2 | 26 µs | 2.9–3.3 ms |
| 4 | 54 µs | 7.6–7.7 ms |
| 8 | 117–133 µs | 18.6–18.8 ms |
| 16 | 335–569 µs | 41.6–41.8 ms |

Ranges are two runs on a loaded machine. Both grow faster than depth, for reasons other than pointers:

- `diff_into` starts with `if a == b`, a deep comparison. On a chain that differs only at its leaf, the comparison at each level walks down to that leaf, so a changed leaf at depth d costs about d²/2 map comparisons. Comparing only at scalars and type mismatches would keep the output and drop the term.
- `merge_into` asks `read_value_at` whether the target is a map, at every level. That walks from the root and clones the whole subtree under the pointer, the whole document at the root. Each changed leaf is also a textual edit that parses the file again to check it, and the file grows with depth. A borrowing lookup would remove the clones; the per-edit check is deliberate.

Neither shows at the depth of ordinary files: a Kubernetes manifest is about 6 levels deep.
