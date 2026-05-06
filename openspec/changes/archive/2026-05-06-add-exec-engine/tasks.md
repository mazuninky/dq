Делегирование: `[orch]` — оркестратор пишет markdown / меняет config / прогоняет smoke; `[writer]` / `[test-writer]` — Rust-правки идут через subagents `rust-cli-writer` / `rust-cli-test-writer` (правило в `.claude/rules/rust-delegation.md`). Каждая задача self-contained, ≤ 2 часов реальной работы.

Зависимости явно прописаны: §1 готовит фундамент (`dq-exec` крейт + workspace deps); §2 — диагностика и правила (зависит от §1); §3 — evaluator + loader + template (зависит от §2); §4 — `dq-lint` крейт + std rules embedding (зависит от §3); §5 — junit/tap reporters (зависит от §1 для diagnostics shape); §6 — CLI surface (зависит от §3, §4, §5); §7 — стандартные правила k8s (зависит от §4, §6); §8 — dockerfile/npm/github-actions rules (параллельно с §7); §9 — integration tests (зависит от §6+); §10 — meta + verify.

## 1. Foundation: dq-exec crate scaffold

- [ ] 1.1 [writer] `crates/dq-exec/Cargo.toml`: replace the M2 placeholder shell with a real package definition. Add dependencies:
  ```toml
  [dependencies]
  dq-core = { path = "../dq-core", version = "0.1" }
  dq-transform = { path = "../dq-transform", version = "0.1" }
  serde = { workspace = true }
  serde_yml = { workspace = true }
  serde_json = { workspace = true }
  thiserror = { workspace = true }
  tracing = { workspace = true }
  globset = { workspace = true }
  camino = { workspace = true }
  indexmap = { workspace = true }

  [dev-dependencies]
  pretty_assertions = { workspace = true }
  tempfile = { workspace = true }
  ```
  No further crate code in this task — subsequent tasks fill in modules.

- [ ] 1.2 [writer] `crates/dq-exec/src/lib.rs`: replace the M2 placeholder content with module declarations and re-exports:
  ```rust
  //! `dq-exec` — rule runtime for the dq lint engine.
  //!
  //! Public surface:
  //! - `Diagnostic` / `Severity` — the structured violation type.
  //! - `Rule` / `RuleSet` / `RuleSource` — parsed rule schema.
  //! - `Evaluator` — pre-compiled rule runner.
  //! - `RuleLoader` — `--rules` and auto-discovery.
  //! - `RuleTester` — `*.test.yml` fixture runner.
  //! - `ExecError` — `thiserror`-based error enum with stable `kind_name()`.

  pub mod diagnostic;
  pub mod error;
  pub mod evaluator;
  pub mod loader;
  pub mod rule;
  pub mod ruleset;
  pub mod template;
  pub mod test_runner;

  pub use diagnostic::{Diagnostic, Severity};
  pub use error::{ExecError, Result};
  pub use evaluator::Evaluator;
  pub use loader::{LoaderArgs, RuleLoader};
  pub use rule::{Rule, RuleCheck, RuleLoc, RuleMatch};
  pub use ruleset::{RuleSet, RuleSource};
  pub use test_runner::{RuleTestCase, RuleTester, TestOutcome};
  ```
  At this stage the modules are empty stubs (each `mod foo { }` plus a TODO). The next sections fill them in.

- [ ] 1.3 [writer] `crates/dq-exec/src/error.rs`: define the `ExecError` enum and `Result` alias.
  ```rust
  use camino::Utf8PathBuf;
  use thiserror::Error;

  pub type Result<T> = std::result::Result<T, ExecError>;

  #[derive(Debug, Error)]
  pub enum ExecError {
      #[error("rule parse error: {hint}")]
      Parse {
          #[source]
          source: serde_yml::Error,
          hint: String,
      },
      #[error("rule {rule_id} failed to compile: {source}")]
      RuleCompile {
          rule_id: String,
          #[source]
          source: dq_transform::JqError,
      },
      #[error("unknown rule {id}")]
      UnknownRule {
          id: String,
          did_you_mean: Vec<String>,
      },
      #[error("io error reading {path}: {source}")]
      Io {
          path: Utf8PathBuf,
          #[source]
          source: std::io::Error,
      },
      #[error("test fixture {path}: {message}")]
      TestFixture {
          path: Utf8PathBuf,
          message: String,
      },
  }

  impl ExecError {
      #[must_use]
      pub fn kind_name(&self) -> &'static str {
          match self {
              Self::Parse { .. } => "parse",
              Self::RuleCompile { .. } => "rule_compile",
              Self::UnknownRule { .. } => "unknown_rule",
              Self::Io { .. } => "io",
              Self::TestFixture { .. } => "test_fixture",
          }
      }
  }
  ```
  Add ≥3 `#[cfg(test)]` unit tests covering `kind_name()` for every variant.

- [ ] 1.4 [orch] Update workspace `Cargo.toml`: no new workspace dependencies are required (every dep `dq-exec` needs is already present from M1–M7). Verify `cargo check -p dq-exec` succeeds at the end of §1.

## 2. Diagnostic + Rule schema

- [ ] 2.1 [writer] `crates/dq-exec/src/diagnostic.rs`: implement `Severity` and `Diagnostic`.
  ```rust
  use std::ops::Range;
  use camino::Utf8PathBuf;
  use serde::{Deserialize, Serialize};

  #[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Deserialize, Serialize)]
  #[serde(rename_all = "lowercase")]
  pub enum Severity {
      Error,
      Warn,
      Info,
  }

  impl Severity {
      pub fn as_str(self) -> &'static str { ... }
  }

  #[derive(Debug, Clone)]
  pub struct Diagnostic {
      pub rule_id: String,
      pub severity: Severity,
      pub message: String,
      pub file: Option<Utf8PathBuf>,
      pub line: u32,
      pub col: u32,
      pub span: Option<Range<usize>>,
      pub references: Vec<String>,
      pub fix: Option<serde_yml::Value>,
  }

  impl Diagnostic {
      pub fn to_serde_json(&self) -> serde_json::Value { /* path/line/col/message/severity/rule_id/references */ }
  }
  ```
  Add ≥4 `#[cfg(test)]` tests:
  - `Severity` round-trips via serde with lowercase strings.
  - `Severity::as_str()` returns `"error"`/`"warn"`/`"info"`.
  - `to_serde_json` emits the canonical `{ path, line, col, message, severity, rule_id, references }` shape (matches the M6 SARIF reporter contract).
  - `to_serde_json` clamps `line == 0` to `1` and omits `path` when `file` is None.

- [ ] 2.2 [writer] `crates/dq-exec/src/rule.rs`: implement the `Rule` / `RuleMatch` / `RuleCheck` / `RuleLoc` structs with serde parsing.
  ```rust
  #[derive(Debug, Clone, Deserialize)]
  #[serde(deny_unknown_fields)]
  pub struct Rule {
      pub id: String,
      pub description: String,
      pub severity: Severity,
      #[serde(rename = "match")]
      pub match_: RuleMatch,
      pub check: RuleCheck,
      #[serde(default)]
      pub fix: Option<serde_yml::Value>,
      #[serde(default)]
      pub references: Vec<String>,
      #[serde(default)]
      pub loc: Option<RuleLoc>,
  }

  #[derive(Debug, Clone, Deserialize)]
  #[serde(deny_unknown_fields)]
  pub struct RuleMatch {
      #[serde(deserialize_with = "deserialize_format_list")]
      pub format: Vec<String>,
      #[serde(default)]
      pub filter: Option<String>,
      #[serde(default)]
      pub glob: Option<String>,
  }

  #[derive(Debug, Clone, Deserialize)]
  #[serde(deny_unknown_fields)]
  pub struct RuleCheck {
      pub jq: String,
      pub message: String,
  }

  #[derive(Debug, Clone, Deserialize, Default)]
  #[serde(deny_unknown_fields)]
  pub struct RuleLoc {
      #[serde(default)]
      pub file: Option<String>,
      #[serde(default)]
      pub line: Option<String>,
  }

  fn deserialize_format_list<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
  where D: serde::Deserializer<'de> { /* accept string or array */ }
  ```
  Add ≥6 `#[cfg(test)]` tests:
  - parses minimal rule YAML.
  - rejects unknown top-level field with `serde_yml::Error`.
  - rejects unknown field inside `match` / `check` / `loc`.
  - accepts `format: yaml` (single string) and `format: [yaml, json]` (array).
  - rejects rule missing `id` / `description` / `severity` / `match` / `check`.
  - parses `fix:` as opaque YAML value (kept for M10).

- [ ] 2.3 [writer] `crates/dq-exec/src/ruleset.rs`: implement `RuleSet` + `RuleSource` and the three loaders.
  ```rust
  #[derive(Debug, Clone)]
  pub struct RuleSet {
      pub source: RuleSource,
      pub rules: Vec<Rule>,
  }

  #[derive(Debug, Clone)]
  pub enum RuleSource {
      Std(&'static str),
      Local(Utf8PathBuf),
      Inline,
  }

  impl RuleSet {
      pub fn from_str(yaml: &str, source: RuleSource) -> Result<Self> { ... }
      pub fn from_path(path: &Utf8Path) -> Result<Self> { ... }
      pub fn from_std(name: &str) -> Result<Self> { /* delegates to dq_lint::std_ruleset */ }
  }
  ```
  Notes:
  - `from_str` accepts either a single rule (`id: foo\n...`) or a YAML stream of rules (parsed as multiple documents). Use `serde_yml::Deserializer::from_str` and walk the document stream.
  - `from_path` on a directory: walks `*.yml` (excluding `*.test.yml`) using `walkdir`.
  - `from_std`: must compile WITHOUT `dq-lint` (avoid circular dep) — plumbed via a `dyn Fn` registered at startup. ALTERNATIVE: have `dq-exec` `Cargo.toml` depend on `dq-lint` (one-way); doing this here is fine because the only thing `dq-exec` calls in `dq-lint` is the `&'static str` accessor, not anything that depends on `dq-exec`.
  
  Pick the simplest of the two ALTERNATIVES that avoids cycles. Recommend: add `dq-lint = { path = "../dq-lint" }` to `dq-exec/Cargo.toml`. The `dq-lint` crate has no production deps on `dq-exec` at this stage (rules are static data + an accessor function), so the dependency is one-way.
  
  Add ≥4 tests:
  - `from_str` parses a single rule.
  - `from_str` parses a 3-rule YAML stream.
  - `from_path` reads a single file.
  - `from_path` walks a directory and excludes `*.test.yml`.

## 3. Evaluator + Loader + Template

- [ ] 3.1 [writer] `crates/dq-exec/src/template.rs`: implement the minimal mustache-style renderer.
  ```rust
  pub fn render(template: &str, value: &serde_json::Value) -> String {
      // Walk the template, replacing `{{ <path> }}` with `lookup(value, path)` or `<missing>`.
      // Path syntax: `.`, `.field`, `.a.b`, `.arr.0`. No conditionals/loops.
  }

  fn lookup(value: &serde_json::Value, path: &str) -> Option<serde_json::Value> { ... }
  ```
  Add ≥8 tests:
  - `{{ .name }}` substitutes from object.
  - `{{ .a.b }}` traverses nested.
  - `{{ .arr.0 }}` indexes array.
  - `{{ . }}` renders whole value as compact JSON.
  - `{{ .nope }}` renders `<missing>`.
  - Whitespace inside `{{ }}` is trimmed.
  - String values render without surrounding quotes.
  - Multiple substitutions on one line all resolve.

- [ ] 3.2 [writer] `crates/dq-exec/src/evaluator.rs`: implement `Evaluator` with pre-compiled `JqEngine` per rule.
  Key shape:
  ```rust
  pub struct Evaluator {
      compiled: Vec<CompiledRule>,
  }

  struct CompiledRule {
      rule: Rule,
      filter_engine: Option<JqEngine>,
      check_engine: JqEngine,
      glob_matcher: Option<GlobMatcher>,
      loc_file_engine: Option<JqEngine>,
      loc_line_engine: Option<JqEngine>,
  }

  impl Evaluator {
      pub fn new(rulesets: Vec<RuleSet>) -> Result<Self> {
          // Compile every rule's match.filter, check.jq, loc.file, loc.line.
          // On compile failure, return ExecError::RuleCompile { rule_id }.
      }
      pub fn evaluate_file(
          &self,
          path: &Utf8Path,
          value: &serde_json::Value,
          format_name: &str,
      ) -> Vec<Diagnostic> { ... }

      pub fn rules(&self) -> impl Iterator<Item = &Rule> { ... }
  }
  ```
  Pipeline per rule: format match → glob match → filter eval (truthy?) → check eval → diagnostic emission with `template::render` for message and jq eval for `loc`.
  
  Add ≥10 tests:
  - format mismatch skips rule.
  - glob mismatch skips rule.
  - filter false skips check.
  - filter null skips check.
  - filter truthy proceeds to check.
  - check empty stream emits 0 diagnostics.
  - check single value emits 1 diagnostic.
  - check three values emit 3 diagnostics in order.
  - `loc.line` jq override applied.
  - rule compile failure surfaces `ExecError::RuleCompile`.

- [ ] 3.3 [writer] `crates/dq-exec/src/loader.rs`: implement `RuleLoader::resolve`.
  ```rust
  pub struct LoaderArgs {
      pub rules: Vec<String>,
      pub cwd: Utf8PathBuf,
      pub discovered_formats: indexmap::IndexSet<String>,
  }

  pub struct RuleLoader;

  impl RuleLoader {
      pub fn resolve(args: &LoaderArgs) -> Result<Vec<RuleSet>> { ... }
  }
  ```
  Logic:
  - When `args.rules` is non-empty: resolve each value as `@std/<name>`, file path, or directory path.
  - When `args.rules` is empty: walk `dq_lint::list_std_rulesets()`, include any whose rules' `match.format` overlaps `args.discovered_formats`; also include `<cwd>/.dq/rules/` if it exists.
  - De-duplicate by source.
  - Unknown `@std/...` returns `ExecError::UnknownRule` with did_you_mean (Levenshtein-2).
  
  Add ≥6 tests using `tempfile`:
  - `--rules @std/k8s` returns one ruleset.
  - `--rules ./tmp/custom.yml` returns the loaded ruleset.
  - `--rules @std/nope` returns `ExecError::UnknownRule` with suggestion.
  - Empty `args.rules` + yaml format auto-binds `@std/k8s`.
  - Empty `args.rules` + dockerfile format auto-binds `@std/dockerfile`.
  - `./.dq/rules/local.yml` is included implicitly.

- [ ] 3.4 [writer] `crates/dq-exec/src/test_runner.rs`: implement the `*.test.yml` fixture runner.
  ```rust
  #[derive(Debug, Clone, Deserialize)]
  pub struct RuleTestFile {
      pub tests: Vec<RuleTestCase>,
  }

  #[derive(Debug, Clone, Deserialize)]
  pub struct RuleTestCase {
      pub name: String,
      pub input: String,
      #[serde(default)]
      pub format: Option<String>,
      pub expected: ExpectedOutcome,
  }

  #[derive(Debug, Clone, Deserialize)]
  pub struct ExpectedOutcome {
      #[serde(default)]
      pub violations: Vec<ExpectedViolation>,
  }

  #[derive(Debug, Clone, Deserialize)]
  pub struct ExpectedViolation {
      pub rule: String,
      #[serde(default)]
      pub message_contains: Option<String>,
      #[serde(default)]
      pub message_equals: Option<String>,
      #[serde(default)]
      pub line: Option<u32>,
  }

  pub struct RuleTester;

  pub enum TestOutcome {
      Pass { fixture: Utf8PathBuf, name: String },
      Fail { fixture: Utf8PathBuf, name: String, missing: Vec<String>, extra: Vec<String> },
      Error { fixture: Utf8PathBuf, name: String, error: String },
  }

  impl RuleTester {
      pub fn run_dir(p: &Utf8Path) -> Result<Vec<TestOutcome>> { ... }
  }
  ```
  Add ≥6 tests using `tempfile`:
  - happy path: fixture passes when rule is silent on negative case.
  - happy path: fixture passes when rule fires on positive case (matches expected).
  - over-firing rule fails (extra-actual diagnostic).
  - under-firing rule fails (missing-expected diagnostic).
  - fixture with no `format:` defaults to rule's first format.
  - non-existent rule sibling produces TestOutcome::Error.

## 4. dq-lint crate + std rule embedding

- [ ] 4.1 [writer] `crates/dq-lint/Cargo.toml`: replace placeholder with a real package definition. Add dependencies: `dq-exec = { path = "../dq-exec", version = "0.1" }` (used by tests only — gate via `dev-dependencies`). At this stage no production deps are needed because the crate is purely static data + an accessor.

- [ ] 4.2 [writer] `crates/dq-lint/src/lib.rs`: implement the `std_ruleset` and `list_std_rulesets` accessor functions, plus an embedding macro tree.
  ```rust
  //! `dq-lint` — embedded standard rule library for the dq lint engine.
  //!
  //! Rules and their `*.test.yml` fixtures are embedded at compile time via
  //! `include_str!`. The `std_ruleset` accessor returns the concatenation of
  //! every rule under `crates/dq-lint/rules/<namespace>/*.yml` (excluding
  //! `*.test.yml`); `list_std_rulesets()` enumerates the namespaces.

  mod embed;

  pub fn std_ruleset(name: &str) -> Option<&'static str> {
      embed::std_ruleset(name)
  }

  pub fn list_std_rulesets() -> &'static [&'static str] {
      embed::NAMESPACES
  }

  pub fn std_test_files(namespace: &str) -> Option<&'static [(&'static str, &'static str)]> {
      embed::std_test_files(namespace)
  }
  ```
  
- [ ] 4.3 [writer] `crates/dq-lint/src/embed.rs`: declare the embedding tables. Use `concat!` over `include_str!` to assemble the per-namespace ruleset strings:
  ```rust
  pub const NAMESPACES: &[&str] = &["k8s", "dockerfile", "npm", "github-actions"];

  pub fn std_ruleset(name: &str) -> Option<&'static str> {
      match name {
          "k8s" => Some(K8S_RULES),
          "dockerfile" => Some(DOCKERFILE_RULES),
          "npm" => Some(NPM_RULES),
          "github-actions" => Some(GITHUB_ACTIONS_RULES),
          _ => None,
      }
  }

  pub fn std_test_files(namespace: &str) -> Option<&'static [(&'static str, &'static str)]> {
      match namespace {
          "k8s" => Some(K8S_TESTS),
          "dockerfile" => Some(DOCKERFILE_TESTS),
          "npm" => Some(NPM_TESTS),
          "github-actions" => Some(GITHUB_ACTIONS_TESTS),
          _ => None,
      }
  }

  static K8S_RULES: &str = concat!(
      include_str!("../rules/k8s/no-latest-tag.yml"), "\n---\n",
      include_str!("../rules/k8s/missing-resources-limits.yml"), "\n---\n",
      // ... all k8s rule files
  );

  static K8S_TESTS: &[(&str, &str)] = &[
      ("no-latest-tag.test.yml", include_str!("../rules/k8s/no-latest-tag.test.yml")),
      // ... all k8s test fixtures
  ];

  // ... DOCKERFILE_RULES / NPM_RULES / GITHUB_ACTIONS_RULES analogously
  ```
  Note: §7-§8 will fill in the rule files; this task scaffolds the embed tables with placeholders (e.g. one tiny rule per namespace) so the crate compiles. The placeholder rule files MUST be replaced before §9.
  
  Add ≥4 tests:
  - `std_ruleset("k8s")` returns `Some(...)`.
  - `std_ruleset("nope")` returns `None`.
  - `list_std_rulesets()` contains all four namespaces.
  - `std_test_files("k8s")` returns Some non-empty slice.

- [ ] 4.4 [writer] Wire `dq-exec` to consume `dq-lint`. In `crates/dq-exec/Cargo.toml`:
  ```toml
  dq-lint = { path = "../dq-lint", version = "0.1" }
  ```
  Then update `RuleSet::from_std(name: &str)` to call `dq_lint::std_ruleset(name)` and parse the result through `RuleSet::from_str`. Update `RuleLoader::resolve` to use `dq_lint::list_std_rulesets()` for the implicit-binding loop.
  
  Add 2 tests in `dq-exec`:
  - `RuleSet::from_std("k8s")` returns a non-empty ruleset (uses placeholder rules from §4.3).
  - `RuleSet::from_std("nope")` returns `ExecError::UnknownRule`.

## 5. JUnit + TAP reporters

- [ ] 5.1 [writer] `crates/dq-cli/src/output/junit.rs`: implement the JUnit XML reporter. Hand-rolled writer (no `quick-xml` dep). Consumes the `{ "diagnostics": [...] }` shape: rejects anything else with `InvalidInput`.
  ```rust
  pub struct JunitReporter;

  impl Reporter for JunitReporter {
      fn report(&self, value: &serde_json::Value, w: &mut dyn Write) -> anyhow::Result<()> {
          let diagnostics = value.get("diagnostics").and_then(|v| v.as_array())
              .ok_or_else(|| anyhow::Error::new(InvalidInput::new(
                  "JUnit reporter expects an object with a `diagnostics` array; \
                   selecting `-F junit` is only valid for `lint` / `check` / `validate`",
              )))?;
          // Build <testsuite>; one <testcase> per file with violations,
          // one <failure> per violation. Files with no violations get a
          // single passing <testcase> entry.
          ...
      }
  }
  ```
  XML escape quotes / `&` / `<` / `>`. Emit `<?xml version="1.0" encoding="UTF-8"?>` header. Wrap in `<testsuites>` / `<testsuite name="dq-lint">`.
  
  Add ≥5 tests via the existing `Reporter::report` pattern (write to `Vec<u8>`):
  - empty `{ "diagnostics": [] }` produces a valid empty testsuite.
  - one error diagnostic produces a `<failure type="error">` element.
  - XML special chars in messages are escaped.
  - non-diagnostic shape produces `InvalidInput`.
  - severity in `<failure type="...">` matches the diagnostic severity.

- [ ] 5.2 [writer] `crates/dq-cli/src/output/tap.rs`: implement TAP version 13 reporter.
  ```rust
  pub struct TapReporter;

  impl Reporter for TapReporter {
      fn report(&self, value: &serde_json::Value, w: &mut dyn Write) -> anyhow::Result<()> {
          let diagnostics = value.get("diagnostics").and_then(|v| v.as_array())
              .ok_or_else(|| anyhow::Error::new(InvalidInput::new(
                  "TAP reporter expects an object with a `diagnostics` array",
              )))?;
          // Header: "TAP version 13", then "1..N", then per-diagnostic
          // "not ok N - <rule-id>: <message>" with YAML diagnostic block.
          ...
      }
  }
  ```
  YAML block format:
  ```
    ---
    severity: error
    file: path/to/file.yaml
    line: 12
    col: 1
    references:
      - https://...
    ...
  ```
  Add ≥4 tests:
  - empty diagnostics produces `TAP version 13\n1..0\n`.
  - one diagnostic produces `not ok 1 - <id>: <message>` with YAML block.
  - non-diagnostic shape produces `InvalidInput`.
  - special characters in message are not corrupted.

- [ ] 5.3 [writer] `crates/dq-cli/src/output/mod.rs`: add `Junit` and `Tap` variants to `OutputFormat` and `as_input_format_name` (both return `None` — output-only formats). Add the module declarations and `pub use`.

- [ ] 5.4 [writer] `crates/dq-cli/src/lib.rs`: extend `reporter_for_format` to wire the new variants:
  ```rust
  OutputFormat::Junit => Box::new(JunitReporter),
  OutputFormat::Tap => Box::new(TapReporter),
  ```

## 6. CLI surface

- [ ] 6.1 [writer] `crates/dq-cli/src/cli/args/lint.rs`: declare `LintArgs`.
  ```rust
  #[derive(Debug, Args)]
  pub struct LintArgs {
      /// Files or globs to lint.
      #[arg(required = true)]
      pub files: Vec<Utf8PathBuf>,

      /// Rule source(s): `@std/<namespace>`, file path, or directory.
      /// Repeat to combine multiple sources. With no `--rules`, the loader
      /// auto-binds every `@std/*` matching the input file formats plus
      /// `./.dq/rules/*.yml` if present.
      #[arg(long)]
      pub rules: Vec<String>,
  }
  ```

- [ ] 6.2 [writer] `crates/dq-cli/src/cli/args/check.rs`: declare `CheckArgs` (linter check).
  ```rust
  #[derive(Debug, Args)]
  pub struct CheckArgs {
      /// Rule path or fully-qualified id (e.g. `k8s.no-latest-tag`).
      pub rule: String,
      /// Files or globs to check.
      #[arg(required = true)]
      pub files: Vec<Utf8PathBuf>,
      /// Inline rule YAML (mutually exclusive with positional `rule`).
      #[arg(long)]
      pub inline: Option<String>,
  }
  ```
  
  Note: this conflicts with the M7 anti-scope wording where `dq check` is referenced as a reserved subcommand. Since `--check` is a *flag* and `check` here is a *subcommand*, clap distinguishes them at the parse layer. Update the M7 anti-scope spec wording in §10.

- [ ] 6.3 [writer] `crates/dq-cli/src/cli/args/{test,explain,rules}.rs`: declare the remaining arg structs.
  ```rust
  // test.rs
  #[derive(Debug, Args)]
  pub struct TestArgs {
      /// Directory containing rule files and `*.test.yml` fixtures.
      pub rules_dir: Utf8PathBuf,
  }

  // explain.rs
  #[derive(Debug, Args)]
  pub struct ExplainArgs {
      /// Rule id (e.g. `k8s.no-latest-tag` or `@std/k8s.no-latest-tag`).
      pub rule_id: String,
  }

  // rules.rs
  #[derive(Debug, Args)]
  pub struct RulesArgs {
      #[command(subcommand)]
      pub command: RulesCommand,
  }

  #[derive(Debug, Subcommand)]
  pub enum RulesCommand {
      /// List all available rulesets and rule ids.
      List(RulesListArgs),
      /// Materialise a `@std/<ns>` ruleset under `./.dq/rules/`.
      Add(RulesAddArgs),
  }

  #[derive(Debug, Args)]
  pub struct RulesListArgs {
      /// Filter by namespace (`@std/k8s` or `k8s`).
      #[arg(long)]
      pub namespace: Option<String>,
  }

  #[derive(Debug, Args)]
  pub struct RulesAddArgs {
      /// `@std/<ns>` or path to a rule file/directory.
      pub ruleset: String,
      /// Overwrite existing destination files.
      #[arg(long)]
      pub force: bool,
      /// Make a symlink instead of copying.
      #[arg(long)]
      pub symlink: bool,
  }
  ```

- [ ] 6.4 [writer] `crates/dq-cli/src/cli/args.rs`: register the modules and re-exports; add the new variants to `Command`. Add the new global flag `--strict` to `Cli`.
  ```rust
  pub struct Cli {
      // ... existing fields ...
      /// Treat warn-severity violations as errors (lint exit-code escalation).
      #[arg(long = "strict", global = true)]
      pub strict: bool,
  }

  pub enum Command {
      // ... existing variants ...
      Lint(LintArgs),
      Check(CheckArgs),
      Test(TestArgs),
      Explain(ExplainArgs),
      Rules(RulesArgs),
  }
  ```

- [ ] 6.5 [writer] `crates/dq-cli/src/commands/lint.rs`: implement the lint handler. Logic:
  1. Reject `-i`/`--diff`/`--backup` etc. via `cli.ensure_no_write_flags()`.
  2. Expand `args.files` via the bulk driver glob expander; collect `(path, format_name, parsed_value)` triples.
  3. Determine `discovered_formats` from the triples.
  4. Build `LoaderArgs` and call `RuleLoader::resolve` to get rulesets.
  5. Construct `Evaluator::new(rulesets)`.
  6. For each file, run `evaluator.evaluate_file` and collect diagnostics.
  7. Render via reporter as `{ "diagnostics": [...] }` shape.
  8. Compute exit code: 4 if any error-severity diag; 1 if `--strict` and any warn; 0 otherwise.
  
  Bulk integration: support `--parallel N` by sharing `Arc<Evaluator>` across rayon workers. Compile errors on the rule side (during `Evaluator::new`) map to `ExecError::RuleCompile`; convert to `dq_core::Error::Parse` so the exit-code mapper picks 3. Extend `exit_code_for_error` if needed to handle a new `LintFailures` marker for the exit-4 path.

- [ ] 6.6 [writer] `crates/dq-cli/src/commands/check.rs` (linter check, NOT the bulk `--check` flag): implement single-rule lint. Pipeline:
  1. Resolve `args.rule` (path, `@std/<ns>.<id>`, or fully-qualified id).
  2. Fall back to `args.inline` when `--inline` is set.
  3. Build a one-rule `RuleSet`, call `Evaluator::new`, evaluate against files (same as `lint`).
  4. Reporter + exit code logic identical to `lint`.

- [ ] 6.7 [writer] `crates/dq-cli/src/commands/test.rs`: implement the test runner handler. Calls `RuleTester::run_dir(args.rules_dir)`, prints results (default: console; `-F json` / `-F tap` supported). Exit 0 on all-pass, 4 on any fail, 6 on no-test-files-found. Hand the outcomes to a small renderer (no need for the canonical diagnostics shape — test output is its own format).

- [ ] 6.8 [writer] `crates/dq-cli/src/commands/explain.rs`: implement explain. Walks every `@std/*` ruleset plus loaded local rulesets, finds the rule by exact id (with `@std/` prefix stripping), prints description / severity / references. Unknown id with did_you_mean → `InvalidInput` (exit 6).

- [ ] 6.9 [writer] `crates/dq-cli/src/commands/rules.rs`: implement `rules list` and `rules add`.
  - `list`: walks `dq_lint::list_std_rulesets()` plus `./.dq/rules/`, builds `serde_json::Value` array of `{id, severity, source, namespace}`, hands to reporter.
  - `add`: resolves `args.ruleset` to a `RuleSet`, copies / symlinks rule files into `./.dq/rules/<namespace>/`. Reject existing destination unless `--force`.

- [ ] 6.10 [writer] `crates/dq-cli/src/lib.rs`: dispatch arms for the new commands. Each dispatches into its handler.

- [ ] 6.11 [writer] `crates/dq-cli/src/exit_code.rs`: add the new exit-code marker for lint (`LintFail`) routed to `VALIDATE_FAIL` (4). Use the existing `ExecError` → `dq_core::Error::Parse` plumbing for rule-compile failures (exit 3); `ExecError::UnknownRule` → `InvalidInput` (exit 6); other `ExecError` variants → `GENERIC` (1) unless they carry a specific shape.

## 7. Standard rules — k8s

- [ ] 7.1 [orch] Create `crates/dq-lint/rules/k8s/` directory and write 18 `.yml` rule files with co-located `.test.yml` fixtures. Each rule MUST follow the schema from §2.2:
  - `no-latest-tag.yml` — error; `match.format: yaml`, `match.filter: '.kind == "Deployment" or .kind == "StatefulSet" or .kind == "DaemonSet" or .kind == "Pod"'`, `check.jq: '.spec.template.spec.containers[]? | select(.image | test(":latest$"))'`, `check.message: "Container '{{ .name }}' uses :latest tag (image: {{ .image }})"`, references to k8s docs.
  - `missing-resources-limits.yml` — error; container without `resources.limits`.
  - `missing-liveness-probe.yml` — warn; container without `livenessProbe`.
  - `missing-readiness-probe.yml` — warn; container without `readinessProbe`.
  - `host-network.yml` — error; pod spec with `hostNetwork: true`.
  - `host-pid.yml` — error; pod spec with `hostPID: true`.
  - `run-as-root.yml` — warn; pod spec without `securityContext.runAsNonRoot: true`.
  - `privileged-container.yml` — error; container with `securityContext.privileged: true`.
  - `allow-privilege-escalation.yml` — error; container with `securityContext.allowPrivilegeEscalation: true` or unset (defaults true).
  - `default-capabilities.yml` — warn; container without `securityContext.capabilities.drop: [ALL]`.
  - `missing-security-context.yml` — warn; pod or container without any `securityContext`.
  - `image-pull-policy-always.yml` — info; container with `imagePullPolicy: Always`.
  - `deployment-no-replicas.yml` — warn; Deployment with `spec.replicas` unset (defaults 1).
  - `service-no-selector.yml` — error; Service with no `spec.selector`.
  - `deprecated-api.yml` — warn; `apiVersion: extensions/v1beta1` or other deprecated API.
  - `missing-labels.yml` — warn; missing `metadata.labels.app.kubernetes.io/name`.
  - `missing-namespace.yml` — info; resource without explicit `metadata.namespace`.
  - `hostpath-volume.yml` — warn; `volumes[].hostPath` present.
  
  For each rule file, also write `<rule>.test.yml` with at least one positive case (rule fires) and one negative case (rule silent).

- [ ] 7.2 [writer] Update `crates/dq-lint/src/embed.rs` to embed all 18 k8s rules + their fixtures via `concat!(include_str!(...), "\n---\n", ...)`.

- [ ] 7.3 [test-writer] Run `cargo test -p dq-exec` and `cargo test -p dq-lint`; verify embedded ruleset parses; verify all k8s `.test.yml` fixtures pass via `RuleTester::run_dir`.

## 8. Standard rules — dockerfile, npm, github-actions

- [ ] 8.1 [orch] Create `crates/dq-lint/rules/dockerfile/` with 8 rules + tests:
  - `no-latest-base-image.yml`, `has-healthcheck.yml`, `no-add-use-copy.yml`, `run-as-root.yml`, `no-update-without-install.yml`, `multiple-cmd.yml`, `no-curl-pipe-bash.yml`, `pin-base-image-by-digest.yml`.
  
  Note: Dockerfile rules have `match.format: dockerfile` — the M5 dockerfile parser is read-only; the Document is parsed into a `Value::Map` per the `dockerfile-parser-rs` shape (`stages[].instructions[]` etc.). Rules walk that structure with jq.

- [ ] 8.2 [orch] Create `crates/dq-lint/rules/npm/` with 8 rules + tests:
  - `no-pinned-deps.yml` (info; `^` / `~` ranges OK, but warn on `>` / `<` / `*`).
  - `no-wildcard-deps.yml` (error; `"*"` in dependencies).
  - `has-license.yml` (warn).
  - `has-repository.yml` (warn).
  - `has-engines.yml` (info).
  - `no-deprecated-fields.yml` (warn; `preferGlobal`).
  - `scripts-no-rm-rf-root.yml` (error; `rm -rf /` substring).
  - `lockfile-required.yml` (info; via glob hint — actually checks via `match.filter`).
  
  All have `match.format: json` and `match.glob: '**/package.json'`.

- [ ] 8.3 [orch] Create `crates/dq-lint/rules/github-actions/` with 6 rules + tests:
  - `action-pinned-by-sha.yml` (warn; `uses: foo/bar@v1` should be `@<sha>`).
  - `no-pull-request-target-with-checkout.yml` (error; security risk).
  - `no-bash-curl-pipe.yml` (warn; `curl ... | bash` in `run:`).
  - `has-permissions.yml` (warn; workflow without explicit `permissions:`).
  - `has-timeout.yml` (info; job without `timeout-minutes`).
  - `no-deprecated-actions.yml` (warn; `actions/checkout@v1`, `actions/setup-node@v1`).
  
  All have `match.format: yaml` and `match.glob: '.github/workflows/**/*.{yml,yaml}'`.

- [ ] 8.4 [writer] Update `crates/dq-lint/src/embed.rs` to embed every namespace's rules and fixtures.

- [ ] 8.5 [test-writer] Run `dq test crates/dq-lint/rules/` (after §6 is done); all ≥40 fixtures pass.

## 9. Integration tests + smoke

- [ ] 9.1 [test-writer] `crates/dq-cli/tests/cli_lint.rs` — ≥10 integration tests via `dq::run`:
  - lint with no `--rules` auto-binds `@std/k8s` for yaml input.
  - lint with `--rules @std/k8s` runs only that ruleset.
  - lint with `--rules ./tmp/custom.yml` (tempfile) loads inline rule.
  - lint exits 4 when error-severity violations are present.
  - lint exits 0 when only warn-severity violations.
  - lint with `--strict` exits 1 when only warn-severity violations.
  - lint exits 3 when a rule's `check.jq` fails to compile.
  - `-F json` produces `{ "diagnostics": [...] }` shape.
  - `-F sarif` produces SARIF 2.1.0 with `runs[0].results`.
  - `-F junit` produces `<testsuite>` XML.
  - `-F tap` produces `TAP version 13` output.

- [ ] 9.2 [test-writer] `crates/dq-cli/tests/cli_test_cmd.rs` — ≥6 tests:
  - `dq test crates/dq-lint/rules/k8s/` exits 0.
  - test runner against a fixture with deliberate failure exits 4.
  - `-F json` produces machine-readable output.
  - `-F tap` produces TAP 13.
  - test against an empty directory exits 6.
  - test against a single-rule directory works.

- [ ] 9.3 [test-writer] `crates/dq-cli/tests/cli_explain.rs` — ≥4 tests:
  - `dq explain k8s.no-latest-tag` exits 0 with description.
  - `dq explain @std/k8s.no-latest-tag` works with prefix.
  - unknown id exits 6 with did_you_mean.
  - JSON output (`-F json`) emits `{ id, description, severity, references }`.

- [ ] 9.4 [test-writer] `crates/dq-cli/tests/cli_rules.rs` — ≥4 tests:
  - `rules list` enumerates all four namespaces.
  - `rules list --namespace k8s` filters.
  - `rules add @std/k8s` (in tempdir) materialises files under `.dq/rules/k8s/`.
  - `rules add` against existing destination without `--force` exits 6.

- [ ] 9.5 [test-writer] Extend `crates/dq-cli/tests/cli_smoke.rs` with one lint smoke test and one explain smoke test.

## 10. Meta + verify

- [ ] 10.1 [orch] Update `dq-plan.md` M8 section: add `✅ Implemented YYYY-MM-DD (см. [openspec/changes/add-exec-engine/](openspec/changes/add-exec-engine/))` marker.

- [ ] 10.2 [orch] Update `README.md`: status moves from `M7 alpha — adds dq query (jq) + dq set --jq` to `M8 alpha — adds the lint engine + standard ruleset library`. Examples block adds:
  ```bash
  # Lint Kubernetes manifests
  dq lint k8s/**/*.yaml

  # Run a single rule
  dq check k8s.no-latest-tag deployment.yaml

  # Test rules
  dq test crates/dq-lint/rules/

  # Read about a rule
  dq explain k8s.no-latest-tag
  ```

- [ ] 10.3 [orch] Update `openspec/specs/cli-shell/spec.md` anti-scope wording: replace M7 list with M8 list (loses `lint`/`check`/`test`/`explain`/`rules`; keeps `fix`/`init`/`config`).

- [ ] 10.4 [orch] Add `CHANGELOG.md` entry for M8 (or extend existing) — calls out the new commands, the standard rule count, and the rule schema.

- [ ] 10.5 [orch] Run the full verification suite:
  - `cargo fmt --check`
  - `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  - `cargo test --workspace --all-features`
  - `cargo build --release`
  - `dq lint -F sarif crates/dq-lint/rules/k8s/` smoke (against the rules' own metadata is meaningless — instead verify via committed sample manifests under `tests/fixtures/`)
  - `dq test crates/dq-lint/rules/` shows zero failures.
