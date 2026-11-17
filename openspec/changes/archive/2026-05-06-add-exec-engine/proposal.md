## Why

M7 made jq reachable through `dq query` and `dq set --jq`, which is the load-bearing primitive M8 needs: rule `check.jq` expressions are how the linter expresses every condition. With jaq in place, the M8 envelope per [dq-plan.md:449-469](../../../dq-plan.md) can ship the exec engine without re-litigating the query-language question.

M8 turns `dq` into the second thing the plan promises — a **platform for writing linters over arbitrary structured files**. M1–M7 are the data-tool half; M8 is the lint half. The plan's pitch is "you can write a rule for your internal YAML format in 5 minutes"; the proof is `dq lint k8s/**/*.yaml` finding typical violations on a representative cluster repo, plus `dq test rules/` going green for ≥40 standard rules across four formats.

The risk envelope is moderate-to-high. The new code is mostly contained in `dq-exec` (rule runtime) and `dq-lint` (embedded standard rules), but it spans six new commands (`lint`, `check`, `test`, `explain`, `rules list`, `rules add`), two new reporters (`junit`, `tap`), and ≥40 rule files with tests. The comment-loss tradeoff from M7 is irrelevant here — the lint engine is read-only (autofix is M10).

The single load-bearing decision — **jq as the rule-condition language** — was committed in M7. M8 only adds the surrounding plumbing.

## What Changes

### New crate: `dq-exec` (rule runtime)

- **`crates/dq-exec/src/lib.rs`** — public surface: `Diagnostic`, `Severity`, `Rule`, `RuleSet`, `RuleSource`, `Evaluator`, `RuleLoader`, `RuleTest`, `TestOutcome`. Re-exports for downstream consumers.
- **`crates/dq-exec/src/diagnostic.rs`** — `pub struct Diagnostic { rule_id, severity, message, file, line, col, span: Option<Range<usize>>, fix: Option<FixHint>, references: Vec<String> }` with `pub enum Severity { Error, Warn, Info }`. `Diagnostic::to_serde_json(&self) -> serde_json::Value` produces the `{ "path", "line", "col", "message", "severity", "rule_id", "references" }` shape the SARIF reporter already consumes (M6 wired the shape; M8 is the second producer).
- **`crates/dq-exec/src/rule.rs`** — `Rule` parsed via `serde` from YAML. Schema:
  ```yaml
  id: <namespace.rule-name>      # required, unique
  description: |                 # required, used by `dq explain`
    multi-line description
  severity: error | warn | info  # required
  match:                          # required, applicability filter
    format: yaml | json | toml | ...   # SHA-stable format-name match (single string or array)
    filter: <jq-expr>            # optional jq predicate over the parsed doc; rule applies when truthy
    glob: <shell-glob>           # optional pathname filter
  check:                          # required, the violation finder
    jq: <jq-expr>                # MUST evaluate to a stream; each emitted value is one violation
    message: <handlebars-lite>   # mustache-like `{{ .field }}` substitution from the violation value
  fix: <reserved>                 # M10; M8 parses the field but does not execute it
  references:                     # optional, list of URLs for `dq explain`
    - https://...
  loc:                            # optional override for diagnostic location
    file: <jq-expr>              # optional; falls back to the file under check
    line: <jq-expr>              # optional; falls back to the violation node line
  ```
- **`crates/dq-exec/src/ruleset.rs`** — `RuleSet { source: RuleSource, rules: Vec<Rule> }`; `RuleSource::{Std(&'static str), Local(Utf8PathBuf), Inline}`. Loaders: `RuleSet::from_std(name) -> Result<RuleSet>` (consults `dq-lint::std_ruleset`), `RuleSet::from_path(p)` (single file or directory), `RuleSet::from_str(yaml)` (inline).
- **`crates/dq-exec/src/loader.rs`** — `RuleLoader::resolve(args: &LoaderArgs) -> Result<Vec<RuleSet>>` orchestrating `--rules` flag values, auto-discovery in `./.dq/rules/`, and the implicit "all `@std/*` matching the formats" default. Returns the de-duplicated list.
- **`crates/dq-exec/src/evaluator.rs`** — `Evaluator { engines: HashMap<RuleId, Arc<JqEngine>>, ... }`. Pre-compiles every rule's `match.filter` and `check.jq` once at load time so the per-file loop reuses them. `Evaluator::run_file(&self, path, doc, format_name) -> Vec<Diagnostic>` walks rules in deterministic order. Match logic: format match (substring or array contains), then `match.filter` (skipped if absent), then `match.glob` against the relative path. For each matching rule, `check.jq` is evaluated; each output value is templated into a Diagnostic.
- **`crates/dq-exec/src/template.rs`** — minimal `{{ .path }}` templater. Supports `{{ .field }}`, `{{ .nested.field }}`, `{{ .array.0 }}`, and `{{ . }}` (the whole violation value as JSON). Anything more elaborate (conditionals, loops) is not supported in M8.
- **`crates/dq-exec/src/test_runner.rs`** — `RuleTester::run_dir(p) -> Vec<TestOutcome>` discovers `*.test.yml` files, parses fixtures, evaluates each fixture's `input` against the matching rule, compares against `expected.violations`. Test schema:
  ```yaml
  tests:
    - name: <descriptive>
      input: |                  # raw document text
        ...
      format: yaml              # optional; otherwise inferred from the test file's directory
      expected:
        violations:             # zero or more — order-insensitive match
          - rule: <id>
            message_contains: <substring>   # OR --
            message_equals: <full string>
            line: <int>          # optional; checked when present
  ```

### Crate enrichment: `dq-lint` (standard rules library)

- **`crates/dq-lint/src/lib.rs`** — replaces the M2 placeholder with `pub fn std_ruleset(name: &str) -> Option<&'static str>` (returns the embedded YAML for `@std/k8s`, `@std/npm`, `@std/github-actions`, `@std/dockerfile`, or any future namespace) and `pub fn list_std_rulesets() -> &'static [&'static str]`.
- **`crates/dq-lint/rules/k8s/*.yml`** — ≥18 Kubernetes rules with co-located `*.test.yml` files (no-latest-tag, missing-resources-limits, missing-liveness-probe, missing-readiness-probe, host-network, host-pid, run-as-root, privileged-container, allow-privilege-escalation, default-capabilities, missing-security-context, image-pull-policy-always, deployment-no-replicas, service-no-selector, deprecated-api, missing-labels, missing-namespace, hostpath-volume).
- **`crates/dq-lint/rules/dockerfile/*.yml`** — ≥8 Dockerfile rules with tests (no-latest-base-image, has-healthcheck, no-add-use-copy, run-as-root, no-update-without-install, multiple-cmd, no-curl-pipe-bash, hadolint-style-shell-command).
- **`crates/dq-lint/rules/npm/*.yml`** — ≥8 npm rules with tests (no-pinned-deps, no-wildcard-deps, has-license, has-repository, has-engines, no-deprecated-fields, scripts-no-rm-rf-root, lockfile-required).
- **`crates/dq-lint/rules/github-actions/*.yml`** — ≥6 GitHub Actions rules with tests (action-pinned-by-sha, no-pull-request-target-with-checkout, no-bash-curl-pipe, has-permissions, has-timeout, no-deprecated-actions).

The four ruleset directories embed via a small `build.rs` (or `include_str!` macros) so the binary is self-contained — no runtime filesystem scanning under `@std/*`.

### CLI surface

Six new subcommands. Plus two new output reporters (`junit`, `tap`) added to `OutputFormat`.

- **`dq lint <files...>`** — runs every applicable ruleset against every matched file. `<files...>` is one or more positionals; each may be a path or a glob (expanded by the existing M3 bulk driver). The `--rules <RULE-OR-PATH>` flag (repeatable) selects rulesets explicitly: `--rules @std/k8s`, `--rules ./.dq/rules/`, `--rules custom-rules.yml`. With no `--rules`, the loader auto-binds all `@std/*` rulesets whose declared `match.format` overlaps the formats found among the files, plus any `./.dq/rules/*.yml` it discovers. Reporter selection follows the global `-F` flag (`console`/`json`/`sarif`/`junit`/`tap`). Exit codes:
  - 0 — all rules ran, no `error`-severity violations found.
  - 4 — at least one `error`-severity violation. (Reuses `VALIDATE_FAIL` per design.)
  - 1 — `warn`-severity violations only when `--strict` is on.
  - 3 — at least one rule failed to compile (jq parse error in a rule's `check.jq` or `match.filter`).
  - 6 — flag misuse (rule not found, glob with no matches when `--strict`, etc.).
- **`dq check <RULE> <files...>`** — runs exactly **one** rule against the files. `<RULE>` is a path to a single `.yml` or a fully-qualified rule id like `k8s.no-latest-tag` (loader resolves it from `@std/`). Exit codes match `lint`. This is `dq lint --rules <RULE> <files...>` with one shorter spelling and tighter "no rule found" diagnostics; it ALSO accepts inline rules via `--inline 'id: ... check: { jq: "...", message: "..." }'` for ad-hoc rules in CI scripts.
- **`dq test <rules-dir>`** — discovers `*.test.yml` files under `<rules-dir>`, runs each fixture's `input` through the matching rule, and reports passes/failures. Output formats: `console` (TAP-like), `json` (machine-readable), `tap` (proper TAP 13). Exit codes: 0 if all pass, 4 if any fail, 6 if no test files found.
- **`dq explain <rule-id>`** — prints the rule's description, severity, references, and a synthesized "this rule fires when …" line built from `match` + `check`. Resolves `@std/...` ids natively.
- **`dq rules list [--namespace <ns>]`** — prints every available rule grouped by namespace (`@std/k8s`, `@std/npm`, …) and any `./.dq/rules/`. Output: `console` (table), `json` (array of `{id, severity, source, namespace}`), `toon`.
- **`dq rules add <ruleset>`** — copies a `@std/...` ruleset (or a local file) into `./.dq/rules/`, materialising the YAML so users can edit it. With `--symlink`, makes a symlink instead. Confirmation prompts are off (per CLI's non-interactive contract); a brief `tracing::info!` summarises what was written.

### Capabilities

#### New Capabilities

- **`data-query-exec`** — covers the `dq-exec` crate, the rule schema, the test runner, the loader, and the evaluator. The single home for "how a rule fires"; M8 lint commands and the future M10 autofix engine consume from this capability.
- **`data-query-rules`** — covers the standard rule library: discovery API (`std_ruleset` / `list_std_rulesets`), the canonical id namespace policy (`@std/<namespace>` → file path → embedded YAML), and the per-namespace rule inventory.

#### Modified Capabilities

- **`cli-shell`** — `Command` enum gains `Lint`, `Check`, `Test`, `Explain`, `Rules` (with `RulesCommand::List` / `RulesCommand::Add`). The M7 anti-scope sentence loses `lint`, `check`, `test`, `explain`, `rules` from the reserved list (these are now reachable). `OutputFormat` gains `Junit` and `Tap` variants. New global flag `--strict` (treats `warn` as `error` in lint exit code).

### Meta

- **`dq-plan.md` M8 section.** Marker `✅ Implemented YYYY-MM-DD` plus cross-link to this archived change folder.
- **`README.md`.** Status moves from `M7 alpha — adds dq query (jq) + dq set --jq` to `M8 alpha — adds the lint engine + standard ruleset library`. Examples block adds one `dq lint` and one `dq explain` invocation.

### What's NOT in M8 (deferred)

- **`dq fix` autofix.** M10 work. M8 parses the rule's `fix:` field for forward compatibility but never executes it.
- **Markdown rules (`@std/markdown`).** M9 territory; markdown is the first tree-format and needs the `comrak` AST + selectors, which is a separate research thread.
- **Composite rules (markdown → embedded code-block validation).** M11.
- **JSON Schema rules.** M11.
- **Community rule registry / WASM plugins.** M12.
- **`--watch` mode.** Out of scope; the same loop is one shell line away.
- **HCL / TOML / Cassandra / etc. rulesets.** Engine is format-agnostic from day one, but the canonical rule library starts with the four formats called out in the plan.
- **Performance tuning (file-level rayon).** Sequential M8 baseline; the M3 bulk driver already supports `--parallel` and the lint command threads it through, but no benchmark-driven tuning lands in M8.

## Impact

- **New code (`dq-exec` — first non-stub revision)**:
  - `crates/dq-exec/Cargo.toml` — bumps from the M2 placeholder to a real package depending on `dq-core`, `dq-transform`, `serde`, `serde_yml`, `serde_json`, `thiserror`, `tracing`, `globset`, `camino`, `indexmap`. Optional dev-deps `pretty_assertions`, `tempfile`.
  - `crates/dq-exec/src/{lib,diagnostic,rule,ruleset,loader,evaluator,template,test_runner}.rs` — the modules above.

- **New code (`dq-lint` — first non-stub revision)**:
  - `crates/dq-lint/Cargo.toml` — depends on `dq-exec` (loader uses the std accessor) and a build-time `include_dir` or `include_str!` macro tree.
  - `crates/dq-lint/src/lib.rs` — `pub fn std_ruleset(name) -> Option<&'static str>`.
  - `crates/dq-lint/rules/{k8s,dockerfile,npm,github-actions}/*.yml` + matching `*.test.yml` files. ≥40 rule + test pairs.

- **`dq-cli` updates**:
  - `crates/dq-cli/Cargo.toml` — new dep `dq-exec`, `dq-lint`. Add `quick-xml` (or hand-rolled writer) for the JUnit reporter; favour hand-rolled to skip a dependency.
  - `crates/dq-cli/src/cli/args/{lint,check,test,explain,rules}.rs` — new arg structs.
  - `crates/dq-cli/src/cli/args.rs` — re-exports + new `Command::*` variants. The `--strict` global flag goes onto `Cli`.
  - `crates/dq-cli/src/lib.rs` — dispatch arms for the new commands.
  - `crates/dq-cli/src/commands/{lint,check,test,explain,rules}.rs` — handlers. `lint` uses the bulk driver for file expansion.
  - `crates/dq-cli/src/output/{junit,tap}.rs` — new reporters. Both consume the `{ "diagnostics": [...] }` shape that the SARIF reporter already expects, so the lint command emits one canonical document and every reporter renders it.
  - `crates/dq-cli/src/output/mod.rs` — new `OutputFormat::Junit` and `OutputFormat::Tap` variants; factory wiring in `lib.rs`.

- **Tests (new)**:
  - `crates/dq-exec/src/{rule,evaluator,loader,template,test_runner}.rs` `#[cfg(test)] mod tests` — ≥30 unit tests covering rule parse, format match, filter match, check eval, message templating, location override, test runner, std-ruleset resolution.
  - `crates/dq-cli/tests/cli_lint.rs` — ≥10 integration tests via `dq::run`: lint with no rules / explicit `--rules` / std auto-binding, exit 4 on error, exit 3 on rule compile fail, JSON / SARIF / JUnit / TAP output shape, `--strict`.
  - `crates/dq-cli/tests/cli_test_cmd.rs` — `dq test` happy path, expected-violations match, fixture failure, no-test-files exit 6.
  - `crates/dq-cli/tests/cli_explain.rs` — explain prints description + references + severity; unknown rule exits 6.
  - `crates/dq-cli/tests/cli_rules.rs` — `rules list` / `rules add` happy paths.
  - `crates/dq-cli/tests/cli_smoke.rs` — extend with one lint smoke + one explain smoke.

- **Backward compatibility**: Every M1–M7 invocation produces byte-identical output. The new commands are reachable for the first time (M7 anti-scope said they are unavailable; M8 unlocks them). New `OutputFormat` variants extend the enum; existing `-F` invocations are unaffected.

- **Project meta**: `dq-plan.md` M8 marker; `README.md` status line + lint/explain examples; `CHANGELOG.md` entry calling out the new commands and the standard ruleset count.
