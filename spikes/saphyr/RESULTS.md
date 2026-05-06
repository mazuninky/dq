# Spike results — saphyr round-trip viability

**Date:** 2026-05-03
**Change:** [add-safe-writes](../../openspec/changes/add-safe-writes/) Task 1.1–1.5
**Verdict:** ❌ **Fail.** D11 criterion #1 (byte-exact round-trip) fails on all 5 fixtures with `saphyr` 0.0.6 and with both candidate alternatives (`yaml-rust2` 0.11.0, `marked-yaml` 0.8.0).

---

## What was tested

A standalone Rust binary (`spikes/saphyr/src/main.rs`) with three subcommands:
- `roundtrip <FIXTURE>...` — parse, emit back, byte-compare
- `mutate <FIXTURE> <KEY>` — change a top-level scalar, expect ≤ 2 lines diff
- `bench <FIXTURE>` — 10 parse+emit iterations, median+stddev

Five fixtures in `spikes/saphyr/fixtures/` covering D11 criteria #1:
- (a) k8s Deployment with trailing comment on every top-level key
- (b) Helm values.yaml (no Go templates) with leading comments + blank lines + quoted strings
- (c) YAML with anchor `&base`, alias `*base`, merge key `<<: *base`
- (d) Three-document YAML stream (ConfigMap + Service + Deployment)
- (e) Hugo frontmatter with datetime literal and sequence values

## Round-trip outcome

```
DIFF spikes/saphyr/fixtures/a_k8s_with_comments.yaml
DIFF spikes/saphyr/fixtures/b_helm_values.yaml
DIFF spikes/saphyr/fixtures/c_anchors_and_merge.yaml
DIFF spikes/saphyr/fixtures/d_multi_doc.yaml
DIFF spikes/saphyr/fixtures/e_hugo_frontmatter.yaml
```

**0/5 byte-exact.** D11 criterion #1 (`saphyr parse → reconstruct → byte-exact for 5/5 fixtures`) — **FAIL**.

## What is lost on round-trip

Sample diff for fixture (a), trimmed:
```diff
-# Fixture (a): k8s Deployment with trailing comment on every top-level key
-# and one leading comment block on a nested mapping.
-apiVersion: apps/v1                             # API group + version
-kind: Deployment                                # workload kind
+---
+apiVersion: apps/v1
+kind: Deployment
...
-          image: ghcr.io/example/web:1.2.3      # immutable digest in real prod
+          image: "ghcr.io/example/web:1.2.3"
```

Concretely lost:
1. **All comments** (leading and trailing) — the scanner discards them at tokenization.
2. **Blank lines** between groupings.
3. **Original quote style** — `bare` becomes `double-quoted` for strings containing `:` or `/` (`ghcr.io/...:1.2.3` → `"ghcr.io/...:1.2.3"`).
4. **Forced `---` document marker** prepended even for single-doc input (`saphyr/src/emitter.rs:198-202`).
5. **Anchor/alias declarations** flatten — `&base` and `*base` get expanded inline.

## API-level finding (the dealbreaker)

`saphyr-parser` 0.0.6 exposes a real event API: `Parser`, `EventReceiver`, `Event`, `ScalarStyle`. The parser **knows** about quote styles and emits them as part of `Event::Scalar`. But:

> **`saphyr` itself does NOT export any event-stream emitter.**

The only emitter is `saphyr::YamlEmitter`, which dumps a high-level `Yaml` value via `emitter.dump(&Yaml)`. The path Task 1.2 originally described — *parse to events, mutate events, re-emit events* — is not implementable on the public API. The forced fallback `Yaml::load_from_str → YamlEmitter::dump` is exactly what loses everything in the diff above, because the metadata is gone before user code sees the document.

## Mutate test

Run on fixture (e) with key `author`:
```
added=5 removed=6  budget=2  exceeded
```
Even mutating one scalar produces ~11 line changes, because the round-trip baseline drift dominates the actual mutation. A `+1/-1` diff is impossible while the parser drops comments and blank lines.

## Bench

Multi-doc fixture, release build:
```
median: 0.021ms, stddev: 0.023ms, iterations: 10
```
Performance budget (≤ 100 ms for 1 MB YAML, D11 criterion #2) is comfortable, but moot given the round-trip failure.

## Alternatives surveyed

Both checked via crates.io listing + docs.rs:

| Crate | Version | Verdict |
|-------|---------|---------|
| `yaml-rust2` | 0.11.0 | **Fail.** Has `Event` parser API, but no event-accepting emitter. Comments scanned but discarded. Same dealbreaker as saphyr. |
| `marked-yaml` | 0.8.0 | **Fail.** Built atop yaml-rust2. Focuses on provenance spans, not round-trip. Explicitly forbids anchors and aliases. No emitter. |
| `fyaml` | 0.5.0 | **Out of scope.** Rust bindings for libfyaml (C library); libfyaml does support round-trip, but FFI dependency breaks the M6 single-static-binary distribution requirement. |
| `granit-parser` | 0.0.1 | Too early. v0.0.1, no emitter at all. |
| `serde-saphyr` | 0.0.25 | Wrapper over saphyr for serde — inherits the same limitation. |

**No pure-Rust crate currently offers byte-exact YAML round-trip.**

## API ergonomics observation (D11 criterion #3)

Setting aside the lost-metadata blocker: even *attempting* to build `Document::set_at(&Pointer, Value)` over the saphyr event stream is not a 30-line affair. The events arrive as a flat stream — building tree mutation requires our own re-entrant state machine that tracks mapping/sequence depth, anchor table, and emit position. With no event emitter to feed the result back into, this is moot, but it confirms criterion #3 ("`Document::set_at` without unsafe and without > 3 levels of nested match") would also fail at the proposed library boundary.

## Recommendation

The gate decision (Task 1.6) is **do not green-light Section 2 against `saphyr` 0.0.6 or any of the surveyed alternatives**. The proposal's D1 (saphyr replaces serde_yml) cannot stand as written.

Three forward paths, ranked by cost vs. coverage of the M2 promise:

### Option A — `m2-fallback-no-preserve` (smallest scope, lowest risk)

Release M2 *without* round-trip preservation. `set` and `del` work, but they reformat the entire file to `serde_yml`'s output style. README.md and the man page state the limitation honestly: "`dq set` does not preserve YAML formatting in M2; round-trip preservation is tracked under M2.5". Costs:
- 4–5 weeks to ship `set`/`del` + atomic write + template guard + DoD coverage.
- Loses the main differentiator vs. `yq` (the Helm chart formatting issue from [dq-plan.md:13-14](../../dq-plan.md)). **DevOps users who care about preserved comments will keep using `yq` and live with its bugs.**
- Honest disclosure does not undo the M2 marketing framing as "the round-trip safety milestone".

### Option B — `m2-textual-edit` (middle scope, middle risk) — **my recommendation**

Adopt the approach `toml_edit` already proves out for TOML: parse via `saphyr` to learn structure, **but keep the original bytes** plus a span table mapping `Pointer` → byte range. `Document::set_at` rewrites only the relevant span(s) of the original buffer. `Document::del_at` removes the span and surrounding indent/newline. Costs:
- 3–4 weeks of focused work on the span/edit machinery (significantly cheaper than a custom YAML parser).
- Covers ~80% of agent use cases — single-scalar mutation in Helm/k8s manifests, the most common write operation.
- Limitations to document: complex insertions (creating new keys/maps) require a heuristic emitter for the inserted region only — formatting of new keys won't match a hand-written equivalent (acceptable: M3 fmt covers polish).
- Avoids needing a YAML 1.2 emitter at all, because we never re-emit existing content.

### Option C — Custom YAML emitter on top of `saphyr-parser` events (largest scope, highest risk)

Write our own emitter that consumes `saphyr-parser` events plus a metadata stream we build on top, and emits a YAML 1.2 conformant document. This is the path to a "real" `Document` model with full set/del semantics including new-key insertion at arbitrary depth. Costs:
- 8–12 weeks of YAML-spec-detail work (quote style decisions, scalar disambiguation, flow-vs-block formatting, anchor tracking, tag resolution).
- Outside the M2 schedule originally proposed (4–6 weeks).
- High risk of subtle bugs that only surface on real-world fixtures.
- This **is** what `dq-plan.md:361` already flagged: "If round-trip is not achievable with acceptable quality — re-evaluate strategy (either own parser for 2-3 months, or release without preserve with honest README disclaimer)".

## POC for textual-edit (Option B) — green-lit

After the user chose Option B, the spike binary was rewired (see commit history of `spikes/saphyr/src/main.rs`) to demonstrate the textual-edit approach against the same five fixtures plus a 1 MB synthetic document. Four subcommands: `span-build`, `mutate`, `assert-byte-perfect`, `bench-span-build`, plus `insert-test` for D14.

**5/5 fixtures pass `assert-byte-perfect`** (one removed line + one added line — every other byte unchanged):

| Fixture | Pointer | New value | Result |
|---------|---------|-----------|--------|
| `a_k8s_with_comments.yaml` | `/spec/replicas` | `5` | PASS — trailing comments on adjacent keys preserved |
| `b_helm_values.yaml` | `/image/tag` | `v2.0.0` | PASS |
| `c_anchors_and_merge.yaml` | `/defaults/timeout` | `60` | PASS — `&base` declaration and `<<: *base` references survive |
| `d_multi_doc.yaml` | `/1/spec/ports/0/port` | `8090` | PASS — `---` separators byte-exact |
| `e_hugo_frontmatter.yaml` | `/title` | `"Updated"` | PASS — quoted style preserved |

**Insertion (Task 1.4) — PASS.** `set /spec/strategy/type RollingUpdate` on the k8s manifest (where `/spec/strategy` does not exist) appends `  strategy:\n    type: RollingUpdate` at the end of the `/spec` block with correct indent. `serde_yml::from_slice::<Value>` parses the result without error (D14 emitter validity guard).

**Performance.** `bench-span-build`:
- Small fixture (1.4 KB): median 0.030 ms, stddev 0.035 ms
- 1 MB synthetic multi-doc (2200 deployments, 1,016,390 bytes): **median 46.4 ms, stddev 0.58 ms** — comfortably under D11 criterion 4's 100 ms budget on M-series Apple silicon. Mutation byte-perfect on the 1 MB doc (`/1100/spec/replicas=99` — middle of file): PASS.

**Tests.** `cargo test --release` from the spike crate — 8/8 passing (6 unit + 2 integration). Integration test `assert_byte_perfect_5_fixtures` is the gate criterion in CI form.

### saphyr-parser API findings (relevant for production implementation)

1. `Parser::next_event()` returns `(Event, Span)` with byte-accurate `Marker::index()`. Spans for `Scalar` events cover the literal **including** surrounding quotes (verified: span for `"Hello, dq"` has length 11, not 9). This is what makes Option B work — quote bytes are part of the value range, so replacement preserves quoting decisions naturally.
2. `Event::Alias` carries a span pointing at the alias **use-site**, not the anchor target. The spike skips alias events with a `WARN` because none of the 5 target pointers resolve through an alias. Production: alias-aware mutation is **not** in M2 scope; document this in the user-facing manual ("editing through `*alias` is undefined; edit the anchor target directly").
3. `ScalarStyle` distinguishes Plain/SingleQuoted/DoubleQuoted/Literal/Folded — but `MappingStart`/`SequenceStart` events do **not** carry a flow-vs-block flag in 0.0.6. The spike approximates `BlockMap*` from parent-frame kind only. Production `ScalarRenderer` will need byte-scan disambiguation around the span (look for `{`/`}`/`[`/`]` in surrounding bytes) — small heuristic, not a blocker.
4. Container spans (Mapping/SequenceStart) are emitted, but the production span map (D4) only stores SCALAR spans. Insertion (D14) handles missing intermediate containers via a forward block-end scan (`find_block_end` heuristic in spike — works on every fixture).

### Gate decision (Task 1.7)

✅ **GREEN-LIT.** D11 criteria 1–5 all met:
- (1) span discovery: no panics, all fixtures yield non-empty SpanMaps
- (2) byte-perfect single-scalar mutation: 5/5
- (3) insertion produces parseable YAML: PASS
- (4) performance: 46.4 ms median for 1 MB (budget 100 ms)
- (5) API ergonomics: spike main.rs compiles clean with `-D warnings`, no unsafe, no >3-level nested matches outside the insertion heuristic itself

Section 2 (Document model) of [tasks.md](../../openspec/changes/add-safe-writes/tasks.md) is unblocked.

## Decision

**Chosen: Option B — textual-edit (span-based).** Confirmed by the user 2026-05-03 after follow-up research showed Option C costs are not 8-12 weeks but ~6 months: `saphyr-parser` 0.0.6 scanner discards comment bytes at tokenization (issue [saphyr-rs/saphyr#103](https://github.com/saphyr-rs/saphyr/issues/103) opened 2026-01-28, no roadmap commitment), so a custom emitter on top of its event stream cannot reconstruct comments — a custom scanner+parser+emitter would be needed. `yaml-rust2` 0.11.0 has the same scanner-level limitation. Only `libfyaml` (C) preserves comments at the API level among published parsers.

Option B uses `saphyr-parser` for **structural understanding only** — events with positions are folded into a `Pointer → ByteRange` span map that the document carries alongside its original bytes. `Document::set_at` rewrites only the bytes in the relevant span (mirroring exactly what `toml_edit` does for TOML, which is why TOML round-trip is a solved problem in the Rust ecosystem). No YAML emitter is written; comments, blank lines, and quote style survive because they are never re-emitted — the bytes around them are never touched. New-key insertion at arbitrary depth uses a small heuristic emitter only for the inserted region; formatting of new keys won't match a hand-written equivalent (acceptable — M3 fmt will polish).

The OpenSpec change [add-safe-writes](../../openspec/changes/add-safe-writes/) is being rewritten under Option B assumptions: `proposal.md` (minor), `design.md` D1/D4/D11 + new D14 on insertion semantics, `tasks.md` §1 (spike pivots to textual-edit POC) / §2 (Document holds spans, not metadata map) / §3 (read path keeps `serde_yml`; write path uses `saphyr-parser` for span discovery only). Sections 4–14 carry through with minor edits.
