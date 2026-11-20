# Compile each jq expression once per `Evaluator::new`

**Status: done.** The per-evaluator cache is in place, with tests. What's left is measuring the gain, resetting the target to the real ruleset, and fixing two stale notes. There is also a possible next step on per-compile cost (see open questions).

## Problem

`JqEngine::compile` ([crates/dq-transform/src/jq.rs](../../crates/dq-transform/src/jq.rs)) has no memory between calls. Every call runs the jaq loader and compiler from scratch. The `jq/recompile_same_filter` bench ([crates/dq-transform/benches/jq.rs](../../crates/dq-transform/benches/jq.rs)) compiles the same filter repeatedly:

| compiles | time | per call |
|---|---|---|
| 1 | 346 µs | 346 µs |
| 10 | 3.79 ms | 379 µs |
| 100 | 38.98 ms | 390 µs |

The cost grows linearly: nothing is cached.

`Evaluator::new` ([crates/dq-exec/src/evaluator.rs](../../crates/dq-exec/src/evaluator.rs)) compiles up to six expressions per rule (`match.filter`, `loc.pointer`, `loc.file`, `loc.line`, `fix.jq`, `fix.ops`), plus `check.jq` or a composite rule's `extract`. Many of them repeat across rules. `evaluate/evaluator_new` in [crates/dq-exec/benches/evaluate.rs](../../crates/dq-exec/benches/evaluate.rs) measured about **17 ms** to build an evaluator for `@std/k8s`. The original estimate was about 5 ms with deduplication, saving about 12 ms per `dq lint` / `dq fix` start.

This is startup cost, not per-file cost. A bulk run (`dq lint` over many files, `--parallel N`) builds one evaluator and shares it across files. The saving matters most for tools that start dq often: pre-commit hooks and per-push CI gates.

How much repeats in `@std/k8s` today, counted over [crates/dq-lint/rules/k8s/](../../crates/dq-lint/rules/k8s/):

- 14 rules.
- `match.filter`: 14 occurrences, 4 distinct. One Deployment/StatefulSet/DaemonSet/Pod filter is shared by 11 rules.
- `loc.pointer`: 14 occurrences, 1 distinct.
- 15 `jq` fields under `check` / `fix`, mostly distinct. One rule has a `fix`.

So about 43 compiles collapse to about 20.

## Design

### Cache shape

```rust
// crates/dq-exec/src/evaluator.rs
pub(crate) type JqCache = HashMap<String, Arc<JqEngine>>;

fn compile_or_cached(cache: &mut JqCache, expr: &str, rule_id: &str) -> Result<Arc<JqEngine>>;
```

On a hit, `compile_or_cached` returns `Arc::clone` of the stored engine. On a miss, it compiles, wraps a compile error as `ExecError::RuleCompile` tagged with the rule id, stores the engine and returns it. `HashMap<String, _>::get` takes `&str` through `Borrow`, so a hit allocates nothing. A miss allocates one `String` for the key.

`Evaluator::new` creates an empty cache before looping over rules and threads `&mut JqCache` through `compile_rule_to_depth` → `compile_check` → `compile_composite` → back into `compile_rule_to_depth` for nested rules. The recursion is bounded by `MAX_EXTRACT_DEPTH`. Every jq-bearing field goes through the cache, including a composite rule's `extract`.

`CompiledRule`'s engine fields (`filter_engine`, `loc_file_engine`, `loc_line_engine`, `loc_pointer_engine`, `fix_engine`, `fix_ops_engine`) are `Option<Arc<JqEngine>>`. `CompiledCheck::Jq { engine }` and the composite check's `extract` are `Arc<JqEngine>`. Call sites run engines through `Arc`'s `Deref` with no other changes. `JqEngine` itself is unchanged: jaq-core's `Filter` isn't `Clone` (its record of native function pointers doesn't derive it), so `Arc` is the only way to share one. `dq-transform`'s public API is unchanged, and `Evaluator` / `CompiledRule` internals are `pub(crate)`.

### Invariants

- **One cache per `Evaluator::new` call, not global.** The cache is dropped when `Evaluator::new` returns. The evaluator keeps its own `Arc`s. Two evaluators built from the same rules get different `Arc` instances, and that is intended.
- **The key is the exact expression string.** Whitespace isn't normalised, so `. + 1` and `.+1` are different keys. Normalising could someday merge two strings that compile differently, and the saving would be negligible.
- **Shared engines can run concurrently.** `jaq-json` is built with its `sync` feature (see the workspace [Cargo.toml](../../Cargo.toml)), which makes values `Send + Sync`. So `JqEngine` and `Arc<JqEngine>` are `Send + Sync`, and `JqEngine::run` takes `&self`.

### Why not a process-wide cache

A `static` cache (`OnceLock<RwLock<HashMap<…>>>` in `dq-transform`) was rejected:

1. It only ever grows. dq is a short-lived CLI today, but if `dq-exec` is ever embedded in a long-running process (a language server, say), it leaks. A per-evaluator cache is freed with the evaluator.
2. Tests that compile invalid expressions on purpose would share state. Isolating them would need `clear()` calls, thread-locals or a scoped-cache API.
3. It gains nothing. Within one `Evaluator::new` the per-evaluator cache already sees every repeat, and a CLI process doesn't build two evaluators from the same rules.

### Other alternatives rejected

- **A global lazily-built engine for the identity filter `.`.** That is a special case of what the general cache already does.
- **A `JqEngine::compile_cached` with a static cache inside `dq-transform`.** This is the global cache under another name, with the same problems, and it would widen `dq-transform`'s deliberately small public API.
- **Keying on a hash of the expression (`BTreeMap<u64, _>`) to skip allocating a `String`.** Lookups already don't allocate. Inserts allocate once per distinct expression. A hash key brings a small but non-zero collision risk for no gain.

### Out of scope

- Eviction or TTL: the cache lives only as long as one `Evaluator::new`.
- Caching glob matchers or JSON Schemas: a glob is compiled once per rule, and schemas are already static. Neither shows up in the benches.
- One-shot entry points: `dq query EXPR`, `dq set --jq EXPR` and the plugin runtime compile once, so a cache can't help them.

### Risks

- Correctness: low. Engines are immutable after compile, and `run` takes `&self`.
- Memory: one `Arc` per engine field, about 1.5 KB across `@std/k8s`.
- A compile error on a shared expression is reported once, under the id of the first rule that uses it. Later rules with the same expression hit the cache (or never compile, since the first error aborts `Evaluator::new`).

## What landed

- `JqCache`, `compile_or_cached` and a doc comment stating the invariants, in [crates/dq-exec/src/evaluator.rs](../../crates/dq-exec/src/evaluator.rs). `Evaluator::new` creates the cache and passes it down.
- Every `CompiledRule` engine field and `CompiledCheck::Jq` hold `Arc<JqEngine>`.
- `compile_composite` in [crates/dq-exec/src/composite.rs](../../crates/dq-exec/src/composite.rs) takes `&mut JqCache`, caches `extract`, and passes the cache into the nested rule's compile. It repeats the lookup-or-compile logic inline instead of calling `compile_or_cached`, which is private to `evaluator.rs`.
- Unit tests in `evaluator.rs`:
  - `cache_dedupes_identical_filters`: two rules with the same `match.filter` share one `Arc`, checked with `Arc::ptr_eq`.
  - `cache_does_not_collapse_different_filters`.
  - `cache_does_not_persist_across_evaluator_news`: two evaluators get distinct `Arc`s.

## Remaining work

- Measure the gain. Run `cargo bench -p dq-exec --bench evaluate -- --quick` and record `evaluate/evaluator_new` here next to the 17 ms before-number. The after-number has never been written down.
- Reset the target. The plan set `evaluator_new` for `@std/k8s` at 6 ms or less, on the assumption of 28 rules with about 14 distinct expressions. The namespace has 14 rules with about 20 distinct expressions, so at about 350 µs per compile expect about 7 ms. Pick a target from the measurement.
- If you want a guard, add one: nothing enforces the target today. Criterion benches don't assert, and the bench only reports.
- Don't use `jq/recompile_same_filter` to show this change. It calls `JqEngine::compile` directly and bypasses the evaluator cache, so its numbers won't move. Only `evaluate/evaluator_new` reflects this change.
- Manual check: time a release `dq lint` on one Kubernetes manifest before and after. The plan expected 10–15 ms faster on warm runs.
- Fix the stale doc comment on `JqCache` in `evaluator.rs`. It says `@std/k8s` has "~10 unique `match.filter`s shared across 28 rules"; the real figure is 4 distinct filters across 14 rules.
- Optional tidy-up: make `compile_or_cached` `pub(crate)` and call it from `compile_composite` instead of repeating the logic there.
- Add a changelog line under `[Unreleased]` in [CHANGELOG.md](../../CHANGELOG.md) for faster `dq lint` / `dq fix` startup.

## Open questions

- **Is the fixed per-compile cost the real target?** `JqEngine::compile` re-parses the jaq core, std and json definitions (`jaq_core::defs()`, `jaq_std::defs()`, `jaq_json::defs()` fed to a fresh `Loader`) on every call. Compiling the identity filter `.` alone costs about 350 µs, which suggests most of each compile is that fixed setup. Sharing the parsed definitions across compiles would cut the cost of the roughly 20 compiles that remain, and would also help one-shot commands. It would touch `dq-transform` internals and needs its own measurement. This idea came up while checking the code; it isn't in the original plan.
