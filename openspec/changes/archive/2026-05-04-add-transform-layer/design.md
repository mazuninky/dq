## Context

M6 archived the distribution story; M7 is the first milestone whose value is **expressiveness, not coverage**. Every M1–M6 command operates on a single JSON Pointer location at a time; the user's expressive ceiling is "set scalar at path", "delete subtree", "RFC 6902 patch list". Users who reach for the third — `dq patch` with a hand-written ops list — already wish they had `jq`'s `|=` update assignment. Worse, the M8 lint engine's `check.jq` field is the only thing standing between "rule ships in M8" and "we have to design a custom DSL first" — so jq has to land before M8 starts.

The technical risk for M7 is moderate. The risky parts:

- **Value adapter.** `dq_core::Value` carries number-precision metadata (`BigInt` / `BigFloat` strings) the JSON spec doesn't have; `jaq_json::Val` carries byte-string flavours JSON doesn't have. Going through `serde_json::Value` as the lingua franca trades two conversions for type-safety: every value crosses `Value → serde_json::Value → Val → serde_json::Value → Value`, and the `arbitrary_precision` feature on `serde_json` keeps the textual literal intact across all four hops.
- **Sync model.** rayon-driven bulk runs (`dq set --jq EXPR 'k8s/**/*.yaml' -i --parallel 4`) need `JqEngine: Send + Sync`. `jaq-json`'s `sync` feature swaps `Rc` for `Arc` everywhere, satisfying the bound — but only with the feature enabled at the workspace level. Forgetting it produces a non-trivial trait-bound failure at the bulk-driver call site.
- **Comment-preservation regression risk.** `dq set --jq` re-emits via `Format::write_with_options`, which drops YAML comments. Users who upgrade `dq` and start sprinkling `--jq` into their pipelines will see comments disappear from files that previously round-tripped cleanly. The `tracing::debug!` line on the splice-vs-re-emit fork covers `-vv` users; the `--help` text and the M7 CHANGELOG entry cover the rest.

The non-risky parts:

- **CLI surface.** One new subcommand (`query`), one new flag on an existing command (`set --jq`). Both follow the established Reporter / DI / `ensure_no_write_flags` patterns.
- **No core changes.** `dq-core::Value` does not gain new variants. `Document` does not gain new fields. The Reporter trait is unchanged. `Format` trait methods are unchanged.
- **No new exit codes.** Existing codes cover every jq failure mode (3 for compile errors, 1 for runtime errors, 6 for misuse).

**Current state:** M6 (`add-distribution`) implemented and committed; archive happens after M7 lands. Active changes: `add-transform-layer` (this document).

**Constraints:**

- Conventions from `/rust-cli` skill are unchanged: thin `main.rs`, Reporter with DI, exit codes as named constants, no `println!` outside `main.rs` / Reporter implementations.
- Rust code edits are delegated to `rust-cli-writer` / `rust-cli-test-writer` per `.claude/rules/rust-delegation.md`.
- M1–M6 single-file behaviour and golden snapshots stay byte-identical. `dq set FILE POINTER VALUE -i` (no `--jq`) routes through the textual-edit path exactly as in M2 — the splice machinery is not touched.
- Dependencies must be MIT/Apache-2.0 to pass `cargo deny check`. `jaq-core` / `jaq-std` / `jaq-json` are MIT.
- M7 ships `dq query EXPR FILE` and `dq set --jq EXPR FILE` — read + write coverage. The `embedded-jq` cargo feature is default-on; the off-state ships a deterministic "feature disabled" error rather than shell-out fallback.

**Stakeholders:**

- Lint engine (M8): `JqEngine` is the building block for `check.jq`. M8 will instantiate one `JqEngine` per rule at load time, share it across the file iteration via `Arc<JqEngine>`.
- AI agents in CI: structured `JqError::Compile` with byte-offset position lets agents surface "you wrote `.foo |=` without an RHS" with a caret pointing at the offset.
- Rust library consumers (`dquery` / `dq-core` users): `dq-transform` is a pub crate whose `JqEngine` they can pull in directly without depending on the CLI.
- Future milestones: M10 auto-fix uses `dq-transform` to evaluate `Rule.fix` jq expressions; M11 composite-rules embed jq inside the markdown-AST traversal.

## Goals / Non-Goals

**Goals:**

- `dq query '.spec.replicas' deployment.yaml` prints `3` for a manifest with `spec.replicas: 3`.
- `dq query '.spec.containers[].image' deployment.yaml -F json` prints a JSON array of image strings.
- `dq query '. + 1' age.yaml` errors at runtime (type mismatch on a Map) with a structured `Runtime` error and exit code 1.
- `dq query '.foo |=' deployment.yaml` errors at compile time with a `Parse` error pointing at the offending position and exit code 3.
- `dq set --jq '.spec.replicas |= . + 1' deployment.yaml -i` increments the field in place; the YAML re-emits via the native writer.
- `dq set --jq '.spec.containers[].image |= sub(":latest"; ":v1")' 'k8s/**/*.yaml' -i --parallel 4` rewrites images across 100 files in parallel.
- `dq set --jq EXPR FILE -i --check` exits 1 when the transform would change the file, exit 0 otherwise — same idempotency contract as the M3 `--check` flag.
- The `dq-transform` crate compiles and tests pass in isolation (`cargo test -p dq-transform`).
- The crate compiles with `--no-default-features` (`embedded-jq` off); calling `JqEngine::compile` returns `JqError::FeatureDisabled`.

**Non-Goals:**

- jq variables (`--arg name value`, `--argjson name JSON`, `--slurpfile`). Useful but not blocking. Reserved for a follow-up if asked.
- `dq query --in-place`. Redundant with `dq set --jq`. Reserved as ergonomic alias if user feedback demands.
- Streaming jq evaluation (lazy iteration). Materialising the full output stream is fine for documents that fit in memory; nothing in M1–M6 streams either.
- Shell-out to a system `jq` binary when `embedded-jq` is off. Out of scope; replaced by a clear "feature disabled" error.
- Comment preservation in `dq set --jq`. The re-emit path drops comments; documented behaviour, accepted tradeoff.
- Multi-file `dq query`. Read commands operate on one file; bulk-jq is `dq set --jq` territory.
- `dq query` over JSONL document streams (one query per line). Reserved for if the use case shows up.

## Decisions

### D1. Value adapter goes through `serde_json::Value`, not direct `Value` ↔ `Val` impls

**Decision:** `dq-transform` exposes `serde_to_val(&serde_json::Value) -> Result<Val>` and `val_to_serde(&Val) -> Result<serde_json::Value>`. Callers convert their domain value (e.g. `dq_core::Value`) to `serde_json::Value` first, then through this adapter. The `JqEngine::run` convenience method takes `&serde_json::Value` directly.

**Alternatives:**
- Direct `dq_core::Value` ↔ `jaq_json::Val` impls in `dq-transform`: would couple `dq-transform` to `dq-core` more tightly and require maintaining the conversion in two places (we already have `value_to_serde_json` in `dq-cli/src/commands/io_helpers.rs`). Rejected.
- Direct conversion via `Val: Deserialize<'de>` consuming a `serde_json::Value`: `serde_json::from_value::<Val>(v)` works (the `serde` feature on `jaq-json` provides the impl), and we use exactly that for the inbound conversion. The outbound conversion is bespoke because `Val` doesn't implement `Serialize` directly — we walk the `Val` enum and build a `serde_json::Value`. Selected.
- Custom intermediate value type owned by `dq-transform`: pure overengineering for two formats. Rejected.

**Trade-offs:** every value crosses two conversion layers. For documents under ~10 MB this is invisible; for the rare "huge YAML" case this would matter, but users who run jq over a 100 MB YAML have a different problem. Number precision is preserved because `serde_json::Number` keeps the textual literal under the `arbitrary_precision` feature already enabled in the workspace.

### D2. `JqEngine` is `Send + Sync + Clone`; sharing across rayon workers uses `Arc<JqEngine>`

**Decision:** `JqEngine` derives `Clone` (the underlying `jaq_core::Filter` is `Clone`). For multi-file bulk runs, `dq set --jq` constructs **one** `JqEngine` outside the parallel loop and shares it via `Arc<JqEngine>` to each rayon worker. Each worker calls `engine.run(&value)` on its own input.

**Alternatives:**
- Compile inside each worker: `JqEngine::compile` is the expensive step (parsing, module loading, compilation); doing it per-file in a bulk run with 100 files is 100× wasted work. Rejected.
- Use a thread-local cached engine: gratuitous when `Arc<JqEngine>` already works. Rejected.
- `&JqEngine` everywhere via thread-pinned references: rayon's `par_iter` produces 'static workers; passing `&'a JqEngine` requires lifetime juggling that `Arc` sidesteps. Rejected.

**Trade-offs:** the `sync` feature on `jaq-json` is required at the workspace level (it changes `Rc` to `Arc` inside `Val`). This adds a small runtime cost (`Arc` increment is more expensive than `Rc`) but it's invisible at jq's per-call granularity. Forgetting the feature produces a `Filter: !Send` trait-bound error at the bulk-driver call site, which we'll catch in CI.

### D3. `embedded-jq` cargo feature is default-on; off-state returns `JqError::FeatureDisabled`

**Decision:** `dq-transform/Cargo.toml` declares:

```toml
[features]
default = ["embedded-jq"]
embedded-jq = ["dep:jaq-core", "dep:jaq-std", "dep:jaq-json"]
```

The `JqEngine` struct and `JqError` enum are always present. With `embedded-jq` enabled, the methods do real work; without it, every method returns `JqError::FeatureDisabled { hint: "rebuild with --features embedded-jq" }`. This means `dq-cli` always compiles, the binary size is the only thing that changes.

**Alternatives:**
- Conditional shell-out to system `jq`: nice in theory, in practice means "two binaries with different jq dialects" since system `jq` implementations differ from jaq in subtle ways. Rejected.
- `dq-transform` doesn't compile without the feature: forces `dq-cli` to also gate the feature, which propagates the `cfg` complexity through several layers. Rejected.
- Make the `query` command unavailable in the off-build (clap `#[command(hide = true)]`): users running `dq query` would see "unknown command" instead of "this build doesn't have jq" — worse error message. Rejected.

**Trade-offs:** the binary always carries the `JqEngine` / `JqError` types even when the feature is off. The size cost is negligible (a few hundred bytes of enum boilerplate); the UX win — clear error when someone tries to use jq in a feature-disabled build — pays for it.

### D4. `dq query EXPR FILE` is single-file, single-document; multi-doc YAML uses `--doc <idx|all>`

**Decision:** `dq query` mirrors `dq select`'s contract: positional EXPR + positional FILE, the `--doc` global flag selects which document of a multi-doc YAML stream to query (default 0). When the user passes `--doc all`, the handler converts the document stream to a JSON array, runs the query against the whole array, and emits the result.

**Alternatives:**
- Iterate the query per document and emit a stream: matches jq's behaviour with `--slurp`-less mode, but produces output that's hard to compose downstream (no separator between results). Rejected.
- Always require `--doc` for multi-doc files: forces busywork for the common single-doc case. Rejected.

**Trade-offs:** users who want "for each document in this stream, run this filter" must wrap the expression in `.[] | …` after `--doc all`. Documented in the `query --help` output.

### D5. `dq set --jq EXPR` requires the filter to produce **exactly one** output value

**Decision:** the handler collects the output stream, requires its length to be exactly 1, and rejects multi-output streams with `InvalidInput` (exit 6) and a message naming the count and suggesting `--doc all` if the user is iterating across documents. Empty streams are also rejected (the document would become empty otherwise).

**Alternatives:**
- Take the first output and discard the rest: silent data loss, terrible UX. Rejected.
- Collect every output into an array and write the array back: changes the document's top-level type, often not what the user wants. Rejected.
- Stream-write a multi-doc YAML when the output stream has multiple values: surprising, format-coupled, doesn't generalize to non-multi-doc formats. Rejected.

**Trade-offs:** filters like `.[]` (which "yield each item") need to be wrapped in `[.[]]` (collect back into an array) for `dq set --jq`. Documented in `set --help` near the `--jq` flag description.

### D6. jq compile errors map to `dq_core::Error::Parse`, runtime errors fall through to `anyhow::anyhow!`

**Decision:** `JqEngine::compile` returns `JqError::Compile { snippet, position, message }`. The `dq query` handler converts that to `dq_core::Error::Parse` so the existing exit-code mapper picks 3 (`PARSE_ERROR`) and the existing console renderer prints the standard caret-and-snippet diagnostic. Runtime errors during `run` produce `anyhow::Error` chains that map to 1 (`GENERIC`) — the *file* and *expression* are both fine; only this evaluation against this data failed.

**Alternatives:**
- Define new exit codes (e.g. `JQ_COMPILE_ERROR = 8`, `JQ_RUNTIME_ERROR = 9`): expands the contract surface for marginal benefit; existing 3/1 already encode the right semantics. Rejected.
- Map runtime errors to `PARSE_ERROR` too: misleading — the document parsed fine. Rejected.
- Map runtime errors to `INVALID_INPUT`: the input was the file, which was fine; the expression was the input to jq, which was fine. The data made jq die. `INVALID_INPUT` would be misleading. Rejected.

**Trade-offs:** users grepping for "parse error" in CI output will see jq compile errors mixed in with file parse errors. Distinguishing them requires reading the message. Acceptable because the structured JSON output (`-F json`) carries the full error type for machine consumers.

### D7. `dq set --jq` re-emits via `Format::write_with_options`, NOT the textual-edit splice path

**Decision:** when `args.jq` is set, the `SetFileOp::apply` handler:

1. Parses via `Format::parse` (NOT the write-aware `parse_yaml_with_spans` / `parse_json_with_spans`).
2. Converts to `serde_json::Value`.
3. Runs the jq filter.
4. Converts back to `dq_core::Value`.
5. Re-emits via `Format::write_with_options` against the global `WriteOptions` (so `--sort-keys` / `--indent` work).
6. The bulk driver receives the new bytes through `FileOpResult::Modified`.

A `tracing::debug!` line at step 1 notes that comments will be lost; users running `-vv` see why.

**Alternatives:**
- Try to splice the jq result back into the textual-edit document: jq can rename keys, change types, restructure entire subtrees. The splice path requires knowing which spans changed; this isn't recoverable from just the before/after `Value`. Building a structural diff to drive the splice is an M9 / M10 research project. Rejected.
- Always require the user to follow `dq set --jq` with `dq fmt` for canonicalization: the re-emit happens implicitly anyway; merging the steps is friendlier. Rejected (the proposal already does the merging).
- Refuse `--jq` for formats with comment-preservation contracts and require `dq query EXPR FILE -F yaml > FILE` instead: violates the principle of least surprise and forces users to re-learn shell-redirection semantics. Rejected.

**Trade-offs:** users who care about comments stop using `--jq` and reach for `dq patch` or `dq set` with explicit pointers. The CLI cannot infer "this user values comments" automatically; the documentation explains the tradeoff. Accepted.

## Risks / Trade-offs

- **`jaq` API churn between minor versions.** jaq just hit 3.0 in March 2026; the 2.x → 3.x migration was non-trivial. Mitigation: pin to `3.0` (not `3` — exact minor) so that a future `3.1` doesn't break us silently; review the API docs before bumping.
- **Comment loss in `dq set --jq` is invisible until you check the file.** The `tracing::debug!` line is below default verbosity. Mitigation: README "Examples" section calls this out, the `set --help` text mentions it next to `--jq`.
- **Filter compilation cost in tight loops.** A user running `dq query EXPR file.yaml` in a `find -exec` loop pays the compile cost per invocation. Mitigation: `dq set --jq EXPR 'glob/**' -i` is the documented bulk path, where compile happens once. The single-file `find -exec` pattern is a user choice with a well-understood cost profile.
- **`embedded-jq=false` builds will be unusual.** No one is asking for them today; the feature flag exists as future-proofing for a "minimal CI" build profile. Mitigation: the off-state is a deterministic error, not a panic. CI matrix runs `cargo build --no-default-features -p dq-transform` to keep the off-state honest.
- **Large jq output streams could OOM.** A query like `.[]` over a 100k-element array materializes 100k separate `serde_json::Value` instances. Mitigation: out of scope for M7; document the materialization model so users know to use `[.[]]` if they want array semantics.

## Migration Plan

No migration required. M7 is purely additive:

- Existing `dq set FILE POINTER VALUE -i` invocations are byte-identical to their M2 behaviour.
- The new `dq query` subcommand was reserved in M4's anti-scope and is now activated; users who attempted `dq query` in M1–M6 saw "unknown subcommand" and now see the new help.
- The new `--jq` flag on `set` is opt-in and disjoint from existing flag combinations.
- The `dq-transform` crate becomes a real `dq-cli` dependency for the first time (M2's placeholder did not affect link-time behaviour).

The release notes flag the `dq query` addition and the `--jq` mode on `set`; CHANGELOG entries describe the comment-preservation tradeoff for `--jq`.

## Open Questions

- **Should `dq query` accept stdin via `dq query EXPR -` (no FILE arg)?** The other read commands accept `-`; consistency suggests yes. The proposal keeps it consistent — `EXPR` first positional, `FILE` second positional, and `FILE = "-"` reads stdin (with `-F` required, same as the existing rule). Settled by following the pattern.
- **Should `set --jq` emit to stdout in non-`-i` mode like `set FILE POINTER VALUE` does?** Yes — same shape. Without `-i` / `--diff` / `--check`, the transformed bytes go to stdout. Settled.
- **Future: `jq -r` raw string output.** When the result is a single string, `dq query -r` would emit it without quotes (matching `jq -r`). Reserved for a follow-up; M7 ships JSON-quoted strings only. The request will likely come from agent users wanting `dq query -r '.image' deploy.yaml | docker pull` patterns.
