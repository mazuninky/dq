## Context

M7 archived the transform layer; M8 is the first milestone whose value is **a new product surface, not a new data operation**. M1–M7 built out the data-operation surface (read, write, transform). M8 makes it the second thing the plan promised: a platform for writing linters over arbitrary structured files. The exec engine is "lint commands + a small runtime that compiles a rule, matches it against files, evaluates `check.jq`, and emits structured diagnostics".

The technical risk for M8 is moderate-to-high. The risky parts:

- **Rule schema is the public contract.** The first user who writes a rule is locking in `id` / `match` / `check` / `fix` field names for a long time. Schema changes after M8 will be backwards-compatible additions only; renaming or repurposing a field is a non-trivial migration.
- **Rule discovery semantics.** `dq lint <files>` without `--rules` should "do the right thing" — load every `@std/*` matching the formats present, plus `./.dq/rules/*.yml`. Loading too aggressively spams users; loading too conservatively makes the tool feel anaemic. The decision below settles this.
- **`{{ .field }}` template surface.** Users will reach for `{{ if … }}` and loops within the year. M8 ships a deliberately minimal surface that we can grow; committing to a Handlebars-grade engine in M8 would be premature lock-in.
- **Reporter output drift.** SARIF, JUnit, TAP, and JSON each have their own conventions for how a "lint result" looks. The lint command emits one canonical `{ "diagnostics": [...] }` shape; every reporter consumes that shape. New reporters added later (HTML, GitLab Code Quality JSON) plug into the same shape.

The non-risky parts:

- **No core changes.** `dq-core::Value` does not gain new variants. `Document` does not gain new fields. The Reporter trait gets two new implementations but its signature is unchanged.
- **No new exit codes.** `VALIDATE_FAIL = 4` already covers "the document failed a quality gate"; M8 reuses it for "lint reported errors". `PARSE_ERROR = 3` covers rule-compile failures because a rule that doesn't compile is a malformed source artifact in the same family as a malformed YAML file. `INVALID_INPUT = 6` covers "rule not found", "no test files".
- **CLI surface follows existing patterns.** Each new command gets its own `*Args` struct, its own `commands::*` handler, its own `Command::*` enum variant — exactly the pattern M1–M7 established.
- **JqEngine reuse.** The exec engine never compiles its own jq filter; it instantiates `dq_transform::JqEngine` once per rule at load time and shares it via `Arc<JqEngine>` across the per-file loop and across rayon workers in `--parallel` mode.

**Current state:** M7 (`add-transform-layer`) implemented and committed; M8 builds directly on it. Active changes: `add-exec-engine` (this document) and `add-distribution` (M6 follow-up archived once stable).

**Constraints:**

- Conventions from `/rust-cli` skill are unchanged: thin `main.rs`, Reporter with DI, exit codes as named constants, no `println!` outside `main.rs` / Reporter implementations.
- Rust code edits are delegated to `rust-cli-writer` / `rust-cli-test-writer` per `.claude/rules/rust-delegation.md`.
- M1–M7 single-file behaviour and golden snapshots stay byte-identical.
- Dependencies must be MIT/Apache-2.0. New deps in this milestone: none required (everything reuses existing workspace deps).
- M8 ships ≥40 standard rules with tests so `dq test rules/` is a credible smoke-test of the engine on day one.

**Stakeholders:**

- AI agents in CI: structured `Diagnostic` JSON with line/col/rule_id and the SARIF/JUnit/TAP renderings let agents triage lint failures programmatically.
- DevOps / platform engineers: `dq lint k8s/**/*.yaml` is the elevator pitch — find the typical issues without learning a new DSL (Rego et al.).
- Internal teams writing custom rules: `id`/`description`/`match`/`check`/`message` schema is the public contract; documentation and examples accompany M8 release.
- Future milestones: M9 (markdown / tree-format) consumes the same `Rule` schema with new format-name semantics; M10 autofix consumes `Rule.fix` (M8 parses, M10 executes); M11 composite rules and JSON Schema rules extend the schema.

## Goals / Non-Goals

**Goals:**

- `dq lint k8s/manifest.yaml` runs every applicable `@std/k8s` rule and prints the violations in console format (with colour when stdout is a TTY).
- `dq lint -F sarif k8s/**/*.yaml > results.sarif` produces a SARIF 2.1.0 document that GitHub Code Scanning consumes without errors.
- `dq lint -F junit *.yaml > results.xml` produces a JUnit XML document that GitLab CI / CircleCI / Jenkins consumes without errors.
- `dq test crates/dq-lint/rules/k8s/` exits 0 when every fixture's expected outcome matches the evaluator's actual outcome.
- `dq explain k8s.no-latest-tag` prints the description, severity, references, and a one-line "this rule fires when …" summary.
- `dq rules list` enumerates every shipped rule (≥40) grouped by namespace.
- `dq rules add @std/k8s` materialises `./.dq/rules/k8s/*.yml` so users can edit the templates.
- The `dq-exec` crate compiles and tests pass in isolation (`cargo test -p dq-exec`).

**Non-Goals:**

- jq variables (`--arg name value`, `--argjson name JSON`, `--slurpfile`) inside rule expressions. Useful but out of scope; rules can use the same workarounds available in M7's `dq query`.
- The `dq fix` command and rule autofix execution. Reserved for M10. M8 parses the `fix:` field for forward compatibility; the parser accepts arbitrary YAML content and stores it untouched.
- Markdown rule support. Reserved for M9; the rule schema's `match.format` accepts `markdown` for forward compatibility, but no markdown rules ship in M8.
- Composite rules (one rule that emits diagnostics about content embedded in another format, e.g. YAML inside a markdown code fence). Reserved for M11.
- JSON Schema integration (a rule whose `check` is a JSON Schema validation). Reserved for M11.
- A `--watch` mode. Users compose with `entr` / `fswatch` / `watchexec`.
- A web/HTTP frontend, registry, or community rule sharing. Reserved for M12.
- Streaming evaluation of huge documents. Lint is built around materialised `serde_json::Value` per file, same as `dq query`.
- HTML / Markdown / GitLab Code Quality / GitHub Step Summary reporters. The shape is documented; new reporters land as small follow-ups.

## Decisions

### D1. The lint engine emits a canonical `{ "diagnostics": [...] }` shape; every reporter consumes the same shape

**Decision:** the `lint` and `check` commands always build a `Vec<Diagnostic>`, serialise it as `{ "diagnostics": [...] }`, and hand the resulting `serde_json::Value` to whichever `Reporter` was selected by `-F`. Console / JSON / SARIF / JUnit / TAP all branch on the same input. `Diagnostic::to_serde_json` is the single serialisation surface.

**Alternatives:**
- One `Reporter` trait per output style with bespoke entry points: doubles the code surface and forces every new reporter to re-derive the canonical shape. Rejected.
- Reporters that take `&[Diagnostic]` directly: ties them to the `dq-exec` crate, which `dq-cli/src/output` does not currently depend on. Rejected to keep the dependency arrow pointing one way (cli → exec → core, not output → exec).

**Trade-offs:** the JSON serialisation runs once per command, even when the reporter could in principle work directly off `Diagnostic`. The cost is invisible at lint scales (thousands of diagnostics per second on a single core). The win is one canonical document — easier to test, easier to extend, easier to document.

### D2. Rule discovery is **explicit-then-implicit**: `--rules` wins, otherwise auto-bind `@std/*` by format + load `./.dq/rules/`

**Decision:** when `--rules` is provided (one or more times), the loader uses exactly the resolved rulesets. With no `--rules`, the loader does two things in order:

1. Walks every input file, collects the set of detected formats, and includes every `@std/<ns>` whose `rules/*.yml` declare a `match.format` overlap with that set.
2. If `./.dq/rules/` exists relative to the current working directory, includes every `*.yml` under it.

The implicit set is computed once at the start of the run. Users who want one and not the other use `--rules @std/k8s` explicitly.

**Alternatives:**
- Always auto-bind every `@std/*`: spammy when a project mixes Helm and Dockerfiles; the user gets npm-rule failures on YAML files because every rule's `match.filter` runs first.
- Never auto-bind: every project needs a `.dq/rules` directory or a long `--rules` invocation. Hostile UX. Rejected.
- Bind by file extension only (no `match.format`): forces rules to declare extensions instead of formats; couples the rule schema to filesystem details. Rejected.

**Trade-offs:** users who add a custom format (e.g. an internal config flavour) need either an explicit `--rules` or a `match.format` value the auto-binding recognises. The auto-binding logic is documented in `dq lint --help` and in the `cli-shell` spec scenario.

### D3. Rule schema is YAML-only with `serde` parsing; no proprietary extensions, no JSON variant

**Decision:** rules are YAML files. The parser uses `serde_yml` with `#[serde(deny_unknown_fields)]` so typos in field names produce a structured error pointing at the offending rule. Future M11+ extensions get new fields with `#[serde(default)]`.

**Alternatives:**
- JSON or TOML rules: needless choice for the user. Rejected; the lint format itself is YAML, mirroring conftest's `*.rego` ↔ "Rego files only" convention.
- A bespoke rule DSL: violates the M7-confirmed "no proprietary DSLs" rule. Rejected.
- Allow unknown fields silently: turns typos into silent no-ops. Rejected.

**Trade-offs:** when M11 adds JSON Schema integration, the new field arrives as `schema:` in YAML. Users with custom rules from M8 keep working unmodified. New fields land with their own scenario in the relevant capability spec.

### D4. `{{ .field }}` template engine is hand-rolled, mustache-style, deliberately minimal

**Decision:** `crates/dq-exec/src/template.rs` implements one operation: `render(template: &str, value: &serde_json::Value) -> Result<String, RenderError>`. Supported syntax: `{{ . }}` (whole value as JSON), `{{ .name }}` (object field), `{{ .a.b }}` (nested), `{{ .arr.0 }}` (array index by integer). Nothing else: no `{{ if … }}`, no `{{ range … }}`, no helpers, no inline expressions. Whitespace inside `{{ }}` is trimmed. Unknown paths produce `<missing>` literally rather than an error (rules can intentionally reference optional fields).

**Alternatives:**
- Embed `handlebars`: pulls in a sizable dependency for a feature most rules don't need. Rejected.
- Use jq itself for templating (`message: '"prefix " + .name'`): jq's string-concat semantics around `null` and `format strings` are unintuitive; users hit edge cases (e.g. `null + ""` is an error) within the first hour. Rejected.
- Use `tera`: same dependency-weight argument as `handlebars`. Rejected.

**Trade-offs:** users hitting the `{{ if … }}` ceiling rephrase their `check.jq` to emit only the violations that need the conditional message, then template plainly. Acceptable; the documentation calls this out.

### D5. `Diagnostic` location defaults to "the file under check, line/col 1"; rules can override via the `loc:` field

**Decision:** by default, every diagnostic emitted by an evaluator carries the file path of the document being linted and `line: 1, col: 1`. Rules can override either field via the `loc:` block:

```yaml
loc:
  file: <jq-expr>     # optional; jq expression over the violation value
  line: <jq-expr>     # optional; jq expression over the violation value
```

When `loc.line` evaluates to a positive integer, the diagnostic reports that line. When the file under check carries position metadata for the violating node (the M1 saphyr/toml_edit/serde_json parsers all do), the evaluator uses that position automatically — `loc:` is the override path, not the default.

**Alternatives:**
- Always emit line 1: terrible UX. Rejected.
- Require every rule to specify `loc:`: most rules don't need it. Rejected.
- Make `loc:` static (string and integer literals only): gives up the most common reason rules need it — derive line from the violation's content, e.g. `loc.line: '.spec.containers[0].image_position.line'`. Rejected.

**Trade-offs:** the position-preservation pipeline from M1 must propagate through the evaluator. Documents whose parsers don't track positions (e.g. CSV's M5 `Format` impl) get line 1; this is documented per format.

### D6. Standard rules embed at compile time via `include_str!`; no runtime filesystem scanning under `@std/*`

**Decision:** the `dq-lint` crate uses `include_str!` macros (or a small `build.rs`) to embed every `rules/<namespace>/*.yml` into a `&'static str` table. `std_ruleset(name)` returns the concatenated YAML for `@std/<namespace>`. The `dq` binary is self-contained — no external rule files are searched at `@std/*`-resolution time.

**Alternatives:**
- Bundle rules separately and ship a directory next to the binary: complicates packaging, breaks the "single static binary" promise. Rejected.
- Use `include_dir`: extra dependency, marginal benefit. Rejected (a few `include_str!` lines per namespace are clearer).
- Compile rules into machine-readable structures at build time: premature optimisation; YAML parsing of a few KB is not the bottleneck. Rejected.

**Trade-offs:** every rule edit triggers a full `dq-lint` recompile. Acceptable; the rule files are small and the recompile is fast.

### D7. `dq lint` exit codes mirror `dq validate`: 0 / 4 for clean / dirty, 3 for rule compile fail

**Decision:** the lint command treats lint as a special validate. Exit 0 when no `error`-severity violations appear; exit 4 when at least one does (`VALIDATE_FAIL` is reused — semantically "the document failed a quality gate"). With `--strict`, `warn`-severity violations also count; they map to exit 1 (`GENERIC`) so CI scripts can branch on the severity. Rule-compile failures (jq parse errors in `match.filter` or `check.jq`) collapse to exit 3 (`PARSE_ERROR`) — the failing artifact is the rule, not the document. Flag misuse, missing rules, and "no rule found" all map to exit 6 (`INVALID_INPUT`).

**Alternatives:**
- A new `LINT_FAIL = 8` exit code: expands the contract surface for marginal benefit; existing 4 already encodes the right semantics. Rejected.
- Exit 1 for lint failures: collides with `exists` returning false, harder to grep for in CI. Rejected.
- Exit per-severity: clever but underspecified — what's the exit code when one rule errors and three rules warn? Rejected; the highest severity wins.

**Trade-offs:** users grepping for "exit 4" see both `dq validate` and `dq lint` results. Distinguishing them requires reading the message, which is fine because the two commands are user-typed (not silently mixed by tooling). Acceptable.

### D8. JUnit reporter writes a single `<testsuite>` per run; one `<testcase>` per file; failures attach as `<failure>` elements

**Decision:** the JUnit reporter renders the diagnostics as one `<testsuite name="dq-lint">` element. Each file linted becomes a `<testcase classname="<file-path>" name="<rule-id-or-aggregate>">`. A file with zero diagnostics renders as a passing `<testcase>` (CI dashboards expect a positive signal). A file with violations renders one `<testcase>` per violation, each with a `<failure type="<rule-id>" message="<diagnostic-message>">` body containing `file:line:col`. Severity is encoded in `type` (`<failure type="error|warn|info">`) so JUnit consumers can filter.

**Alternatives:**
- One `<testsuite>` per ruleset, one `<testcase>` per rule: collapses identity for users who care about per-file reporting; harder to navigate in GitLab UI. Rejected.
- One `<testcase>` per file with all violations in a single `<failure>`: GitLab UI truncates the failure message after the first 100 characters. Rejected.
- Pull in `quick-xml`: small dep, but the JUnit output is structurally simple enough that a hand-rolled writer is fine and avoids the dependency surface. Selected — hand-rolled.

**Trade-offs:** users with lots of violations per file see lots of `<testcase>` rows. CI dashboards handle this fine. Acceptable.

### D9. TAP reporter emits TAP version 13 with YAML diagnostic blocks; one test point per diagnostic

**Decision:** the TAP reporter prints `TAP version 13` then `1..N` then one `not ok N - <rule-id>: <message>` line per violating diagnostic, with a YAML block (`---\n…\n…`) for `severity`, `file`, `line`, `col`, `references`. Files with no diagnostics emit `ok N - <file-path>` to keep the TAP plan honest. Final `# n tests, m failures` summary line.

**Alternatives:**
- Plain TAP 12 (no YAML diagnostics): loses the structured failure data CI consumers rely on. Rejected.
- One TAP point per file: same identity-collapse argument as D8. Rejected.
- Skip the per-file passing entries: leaves a gap-y plan, harder to consume. Rejected.

**Trade-offs:** TAP output is wordy (5–10 lines per diagnostic when YAML blocks are present). For users who want compact output, JSON is the right choice. The TAP renderer documents this in its module-level doc.

### D10. `dq test` is a strict-comparison runner; expected violations must match exactly (allowing for `message_contains` / `message_equals` flexibility)

**Decision:** the test runner walks the `tests:` array; for each fixture, evaluates the parent rule against the fixture's `input` (parsed in the rule's declared `match.format` or the fixture-supplied `format:`), and compares the resulting `Vec<Diagnostic>` against `expected.violations` order-insensitively. A test passes when:

- Every expected violation matches at least one actual violation (rule id matches; `message_contains` substring or `message_equals` equality is satisfied; `line` matches when present).
- No actual violations remain unmatched (catches the "rule fires when it shouldn't" case).

The runner reports per-fixture pass/fail, with a delta listing missing-expected and extra-actual diagnostics on failure.

**Alternatives:**
- Order-sensitive matching: makes tests brittle. Rejected.
- Subset matching only ("expected violations are a subset of actual"): silently allows over-firing rules. Rejected.
- Snapshot-based testing (insta): adds a dependency to `dq-exec` and ties test fixtures to filesystem layout. Rejected.

**Trade-offs:** fixtures expressing "this rule fires N times" need to enumerate N expected violations. The schema is verbose for repeated fixtures, but explicit. Acceptable.

## Risks / Trade-offs

- **Rule-schema lock-in.** Changes to field names after M8 are migrations. Mitigation: the schema is reviewed against conftest, OPA, markdownlint, and yamllint conventions before M8 ships; field names that are obviously wrong are renamed now. Once M8 lands, schema growth is additive.
- **Auto-binding `@std/*` is opinionated.** Users who don't want certain rules find them firing. Mitigation: `--rules` is the explicit override; `dq lint --no-std` is reserved for a follow-up if user feedback demands it.
- **JqEngine compile cost dominates per-rule load.** A rule library with 200 rules forces 200 jq compilations at every `dq lint` invocation. Mitigation: M8 ships ≤50 rules; the future "compile once and cache to disk" optimization is a follow-up.
- **`{{ . }}` template surface will be stretched.** Users will request conditionals within months. Mitigation: M9 / M10 either grows the template engine or documents the "use jq to pre-shape the violation" workaround.
- **Reporter output drift across CI tools.** GitLab Code Quality and GitHub Code Scanning each have idiosyncratic JSON shapes. Mitigation: M8 ships SARIF (covers GitHub Code Scanning) and JUnit (covers GitLab CI test reports). GitLab Code Quality JSON is a follow-up.
- **`dq test` runs serially.** Parallelising at the fixture granularity is straightforward but unnecessary at M8 rule counts. Mitigation: when test counts grow, add `--parallel` to `dq test` mirroring `dq set`.

## Migration Plan

No migration required. M8 is purely additive:

- Existing M1–M7 invocations are byte-identical.
- The new commands were reserved in M7's anti-scope and are now activated.
- `OutputFormat::Junit` and `OutputFormat::Tap` are new enum variants; existing `-F` invocations are unaffected.

Release notes flag the new commands and the standard ruleset count; the `CHANGELOG.md` entry calls out the schema explicitly so users know they can write rules.

## Open Questions

- **Should `dq lint` accept a single file via stdin (`dq lint -`)?** The other read commands accept `-`; consistency suggests yes. The proposal keeps consistency — `dq lint -F yaml -` reads stdin under the supplied format, runs every rule whose `match.format` accepts YAML, and emits diagnostics to stdout. Settled by following the pattern.
- **Should `dq rules add` ask for confirmation before overwriting an existing file?** The non-interactive contract says no prompts. The handler instead fails with `INVALID_INPUT` when the destination exists, suggesting `--force`. Settled: fail-by-default, opt-in `--force`.
- **Should `dq lint` default to auto-detecting `*.yaml` / `*.json` / `*.toml` if the user passes no positional arguments at all?** No — that would couple lint to a CWD walk, which surprises users. `dq lint` requires at least one explicit path or glob. Settled: positional required.
