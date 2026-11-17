Делегирование: `[orch]` — оркестратор пишет markdown / меняет config / прогоняет smoke; `[writer]` / `[test-writer]` — Rust-правки идут через subagents `rust-cli-writer` / `rust-cli-test-writer` (правило в `.claude/rules/rust-delegation.md`). Каждая задача self-contained, ≤ 2 часов реальной работы.

Зависимости явно прописаны: §1 готовит фундамент (`dq-transform` крейт + workspace deps); §2 — value adapter (зависит от §1); §3 — `JqEngine` (зависит от §1, §2); §4 — feature gate `embedded-jq` (зависит от §3); §5 — `dq query` (зависит от §3); §6 — `set --jq` (зависит от §3); §7 — тесты по фактам §3-§6; §8 — meta + verify.

## 1. Foundation: dq-transform crate + workspace deps

- [x] 1.1 [writer] Workspace `Cargo.toml`: add three new `[workspace.dependencies]` entries:
  ```toml
  jaq-core = "3.0"
  jaq-std = "3.0"
  jaq-json = { version = "2.0", features = ["sync", "serde"] }
  ```
  The `sync` feature on `jaq-json` swaps `Rc` for `Arc`, making `Val: Send + Sync` so the rayon-driven bulk path in `dq set --jq EXPR 'glob' -i --parallel N` compiles. The `serde` feature provides the `Val: Deserialize` impl used by the inbound value adapter. Verify each crate's license is MIT (use `cargo deny check licenses` after this step).

- [x] 1.2 [writer] `crates/dq-transform/Cargo.toml`: replace the M2 placeholder shell with a real package definition. Add dependencies:
  ```toml
  [dependencies]
  jaq-core = { workspace = true, optional = true }
  jaq-std = { workspace = true, optional = true }
  jaq-json = { workspace = true, optional = true }
  serde_json = { workspace = true }
  thiserror = { workspace = true }
  tracing = { workspace = true }

  [features]
  default = ["embedded-jq"]
  embedded-jq = ["dep:jaq-core", "dep:jaq-std", "dep:jaq-json"]

  [dev-dependencies]
  serde_json = { workspace = true }
  pretty_assertions = { workspace = true }
  ```
  No further crate code in this task — the next tasks fill in `lib.rs` and `jq.rs`.

- [x] 1.3 [writer] `crates/dq-transform/src/lib.rs`: replace the M2 placeholder content with:
  ```rust
  //! `dq-transform` — embedded jq engine (via `jaq`) and the value adapters
  //! that bridge `dq-core::Value` ↔ `jaq_json::Val` ↔ `serde_json::Value`.
  //!
  //! The public entry point is [`JqEngine`], which compiles a jq expression
  //! once and evaluates it against `serde_json::Value` inputs. The
  //! `embedded-jq` cargo feature (default-on) gates the heavy implementation;
  //! when off, every method returns [`JqError::FeatureDisabled`].

  pub mod jq;

  pub use jq::{JqEngine, JqError, serde_to_val, val_to_serde};
  ```

## 2. Value adapter (serde_json::Value ↔ jaq_json::Val)

- [x] 2.1 [writer] `crates/dq-transform/src/jq.rs` (first slice): scaffold the file with the `JqError` enum, the always-available `JqEngine` shell, and the value-adapter functions. Both adapters live behind `#[cfg(feature = "embedded-jq")]` for their real implementations and `#[cfg(not(feature = "embedded-jq"))]` for the feature-disabled stub.
  ```rust
  use thiserror::Error;

  #[derive(Debug, Error)]
  pub enum JqError {
      #[error("jq compile error at byte offset {position}: {message}")]
      Compile { snippet: String, position: usize, message: String },
      #[error("jq runtime error: {message}")]
      Runtime { message: String },
      #[error("jq value conversion error: {message}")]
      Conversion { message: String },
      #[error("dq-transform was built without `embedded-jq` ({hint})")]
      FeatureDisabled { hint: &'static str },
  }

  impl JqError {
      pub fn kind_name(&self) -> &'static str {
          match self {
              Self::Compile { .. } => "compile",
              Self::Runtime { .. } => "runtime",
              Self::Conversion { .. } => "conversion",
              Self::FeatureDisabled { .. } => "feature_disabled",
          }
      }
  }
  ```

  Without `embedded-jq`:
  ```rust
  #[cfg(not(feature = "embedded-jq"))]
  pub struct JqEngine;
  #[cfg(not(feature = "embedded-jq"))]
  impl JqEngine {
      pub fn compile(_: &str) -> Result<Self, JqError> {
          Err(JqError::FeatureDisabled { hint: "rebuild with --features embedded-jq" })
      }
      pub fn run(&self, _: &serde_json::Value) -> Result<Vec<serde_json::Value>, JqError> {
          Err(JqError::FeatureDisabled { hint: "rebuild with --features embedded-jq" })
      }
  }
  ```

  With `embedded-jq`:
  ```rust
  #[cfg(feature = "embedded-jq")]
  pub fn serde_to_val(v: &serde_json::Value) -> Result<jaq_json::Val, JqError> {
      // Use `serde_json::from_value::<jaq_json::Val>(v.clone())` — works
      // because jaq-json's `serde` feature provides Val: Deserialize.
      // Map any deserialization error to `JqError::Conversion`.
  }

  #[cfg(feature = "embedded-jq")]
  pub fn val_to_serde(v: &jaq_json::Val) -> Result<serde_json::Value, JqError> {
      // Walk the Val enum: Null, Bool, Num (Int / Float / BigInt branches),
      // TStr (text string → serde_json::Value::String), BStr (byte string →
      // base64-encoded? — actually for M7, reject as JqError::Conversion since
      // arbitrary bytes don't fit JSON), Arr (recurse), Obj (recurse with key
      // coercion: jaq_json::Val keys may not be strings; coerce via Display).
  }
  ```

  Stubs without feature:
  ```rust
  #[cfg(not(feature = "embedded-jq"))]
  pub fn serde_to_val(_: &serde_json::Value) -> Result<(), JqError> {
      Err(JqError::FeatureDisabled { hint: "rebuild with --features embedded-jq" })
  }
  ```
  (The stub return type is `Result<(), JqError>` because `jaq_json::Val` doesn't exist without the feature; document this in the function rustdoc.)

  See [crates/dq-transform/Cargo.toml] for feature wiring; see `/tmp/jaq-json-2.0.0/src/serde.rs` for the `Deserialize` impl shape.

- [x] 2.2 [test-writer] `crates/dq-transform/src/jq.rs` `#[cfg(test)] mod tests` (value-adapter slice): add ≥6 unit tests guarded by `#[cfg(feature = "embedded-jq")]`:
  - Round-trip `serde_json::json!(null)` → `Val` → `serde_json::Value` produces `null`.
  - Round-trip `serde_json::json!(true)` produces `true`.
  - Round-trip `serde_json::json!(42)` produces an integer.
  - Round-trip `serde_json::json!(3.14)` produces a float.
  - Round-trip `serde_json::json!("hello")` produces the same string.
  - Round-trip `serde_json::json!([1, 2, 3])` produces the same array (order preserved).
  - Round-trip `serde_json::json!({"z": 1, "a": 2, "m": 3})` produces an object whose keys iterate in `["z", "a", "m"]` order.
  - Round-trip a nested structure (`array of objects`).

## 3. JqEngine: compile + run

- [x] 3.1 [writer] `crates/dq-transform/src/jq.rs` (engine slice, `#[cfg(feature = "embedded-jq")]`): add the `JqEngine` struct, holding a compiled `jaq_core::Filter<jaq_core::data::JustLut<jaq_json::Val>>`. Implementation:
  ```rust
  use jaq_core::load::{Arena, File, Loader};
  use jaq_core::{Compiler, Ctx, Vars, data, unwrap_valr};
  use jaq_json::Val;

  pub struct JqEngine {
      filter: jaq_core::Filter<data::JustLut<Val>>,
  }

  impl Clone for JqEngine {
      fn clone(&self) -> Self { Self { filter: self.filter.clone() } }
  }

  impl JqEngine {
      pub fn compile(expression: &str) -> Result<Self, JqError> {
          let defs = jaq_core::defs()
              .chain(jaq_std::defs())
              .chain(jaq_json::defs());
          let funs = jaq_core::funs()
              .chain(jaq_std::funs())
              .chain(jaq_json::funs());

          let loader = Loader::new(defs);
          let arena = Arena::default();
          let program = File { code: expression, path: () };

          let modules = loader.load(&arena, program)
              .map_err(|errs| compile_error_from_load_errors(&errs, expression))?;

          let filter = Compiler::default()
              .with_funs(funs)
              .compile(modules)
              .map_err(|errs| compile_error_from_compile_errors(&errs, expression))?;

          Ok(Self { filter })
      }

      pub fn run(&self, input: &serde_json::Value) -> Result<Vec<serde_json::Value>, JqError> {
          let val = serde_to_val(input)?;
          let ctx = Ctx::<data::JustLut<Val>>::new(&self.filter.lut, Vars::new([]));

          let mut out = Vec::new();
          for result in self.filter.id.run((ctx, val)).map(unwrap_valr) {
              match result {
                  Ok(v) => out.push(val_to_serde(&v)?),
                  Err(e) => return Err(JqError::Runtime { message: e.to_string() }),
              }
          }
          Ok(out)
      }
  }
  ```

  Plus two helpers `compile_error_from_load_errors` and `compile_error_from_compile_errors` that walk jaq's error structures and produce `JqError::Compile { snippet, position, message }` — pull the position from the source span when available, fall back to `0` otherwise. Snippet is `expression[saturating_sub(position, 30)..expression.len().min(position + 30)]` with `...` ellipsis when truncated.

  Static-assert `Send + Sync` on `JqEngine` via:
  ```rust
  #[cfg(test)]
  fn _assert_engine_send_sync() {
      fn require_send_sync<T: Send + Sync>(_: &T) {}
      let engine = JqEngine::compile(".").unwrap();
      require_send_sync(&engine);
  }
  ```

- [x] 3.2 [test-writer] `crates/dq-transform/src/jq.rs` `#[cfg(test)] mod tests` (engine slice, `#[cfg(feature = "embedded-jq")]`): add ≥8 unit tests:
  - `JqEngine::compile(".")` succeeds; `.run(&json!({"a":1}))` returns `vec![json!({"a":1})]`.
  - `JqEngine::compile(".foo")` succeeds; `.run(&json!({"foo": 42}))` returns `vec![json!(42)]`.
  - `JqEngine::compile(".count |= . + 1")` succeeds; `.run(&json!({"count": 1}))` returns `vec![json!({"count": 2})]`.
  - `JqEngine::compile(".[]")` succeeds; `.run(&json!([1, 2, 3]))` returns `vec![json!(1), json!(2), json!(3)]`.
  - `JqEngine::compile("nonexistent_fn")` errors with `JqError::Compile { … }` whose `kind_name() == "compile"`.
  - `JqEngine::compile(".foo |=")` errors with `JqError::Compile { … }` mentioning the syntax problem.
  - `JqEngine::compile(". + 1")?.run(&json!("string"))` errors with `JqError::Runtime { … }` whose `kind_name() == "runtime"`.
  - `JqEngine::compile(".")?.clone()` returns a working clone (smoke for `Clone` impl).
  - `_assert_engine_send_sync` test compiles (proves the trait bounds).

## 4. Feature-gate verification

- [x] 4.1 [test-writer] `crates/dq-transform/src/jq.rs` `#[cfg(test)] mod tests` (feature-disabled slice, `#[cfg(not(feature = "embedded-jq"))]`): add 3 tests:
  - `JqEngine::compile(".")` returns `Err(JqError::FeatureDisabled { … })`.
  - `JqError::FeatureDisabled { hint: "..." }.kind_name() == "feature_disabled"`.
  - `serde_to_val(&serde_json::Value::Null)` returns `Err(JqError::FeatureDisabled { … })`.

- [x] 4.2 [orch] CI smoke: `cargo build -p dq-transform --no-default-features` succeeds. Add a one-line entry to `.github/workflows/ci.yml` (under the existing `cargo build` job) that runs `cargo build -p dq-transform --no-default-features` so the off-state stays honest.

## 5. CLI: `dq query EXPR FILE`

- [x] 5.1 [writer] `crates/dq-cli/Cargo.toml`: add `dq-transform = { path = "../dq-transform", version = "0.1" }` under `[dependencies]`. No feature flags — relies on default `embedded-jq`.

- [x] 5.2 [writer] `crates/dq-cli/src/cli/args/query.rs`: new file with `QueryArgs`:
  ```rust
  //! `dq query EXPR FILE` — evaluate a jq expression over the document.

  use camino::Utf8PathBuf;
  use clap::Args;

  /// Arguments for `dq query`.
  #[derive(Debug, Args)]
  pub struct QueryArgs {
      /// jq expression (jaq dialect — `jq -h` syntax minus `--arg`/`--slurpfile`).
      pub expression: String,

      /// File to query (or `-` for stdin, requiring `-F`).
      #[arg(value_parser = clap::value_parser!(Utf8PathBuf))]
      pub file: Utf8PathBuf,
  }
  ```

- [x] 5.3 [writer] `crates/dq-cli/src/cli/args.rs`: register the new module + re-export + Command variant:
  ```rust
  mod query;  // alphabetical
  pub use query::QueryArgs;

  // in Command enum:
  /// Evaluate a jq expression over the document and emit the result stream.
  Query(QueryArgs),
  ```

- [x] 5.4 [writer] `crates/dq-cli/src/commands/mod.rs`: add `pub mod query;`.

- [x] 5.5 [writer] `crates/dq-cli/src/commands/query.rs`: new handler. Pipeline:
  1. `cli.ensure_no_write_flags()?` — reject `-i`/`--diff`/`--backup`/`--check`/`--continue-on-error`/`--parallel`.
  2. `let (_fmt, doc) = load_document_with_path(&args.file, input_format)?` — same shape as `dq select`.
  3. `let value_view = select_document(&doc, doc_arg)?` — handles `--doc <idx|all>`.
  4. `let serde_value: serde_json::Value = value_to_serde_json(&value_view)`.
  5. `let engine = dq_transform::JqEngine::compile(&args.expression)` — convert `JqError::Compile` to `dq_core::Error::Parse` for exit-code mapping.
  6. `let outputs = engine.run(&serde_value)` — convert `JqError::Runtime` to `anyhow::anyhow!("{msg}")` for `GENERIC` mapping.
  7. Render through `reporter`:
     - JSON / JSONL / YAML / TOML / TOON: hand the `serde_json::Value::Array(outputs)` to `reporter.report(&arr, out)`.
     - Console: render each value on its own line via the existing `ConsoleReporter` (call once per value).
     - SARIF: rejected via the existing `BannedReporter` pattern (already handled at `reporter_for_format`).

  Helper for the compile-error conversion:
  ```rust
  fn jq_compile_to_parse(err: dq_transform::JqError, file: &Utf8Path, expression: &str) -> dq_core::Error {
      match err {
          dq_transform::JqError::Compile { snippet, position, message } => dq_core::Error::Parse {
              file: Some(file.to_path_buf()),
              line: 1,
              col: position,
              span: position..position,
              snippet,
              message: format!("jq: {message}"),
          },
          other => dq_core::Error::Format { format: "jq", message: other.to_string() },
      }
  }
  ```

- [x] 5.6 [writer] `crates/dq-cli/src/lib.rs::dispatch`: add the new arm:
  ```rust
  Command::Query(args) => commands::query::run(cli, args, input_format, doc_arg, reporter, out),
  ```

- [x] 5.7 [test-writer] `crates/dq-cli/src/commands/query.rs` `#[cfg(test)] mod tests`: ≥3 handler-level tests via `commands::query::run` with `Vec<u8>` writer:
  - `dq query '.' simple.yaml` round-trips a small YAML.
  - `dq query '.foo' file_with_foo.yaml` returns the value of `.foo`.
  - `dq query '.x' f.yaml --in-place` returns `InvalidInput` error.

- [x] 5.8 [test-writer] `crates/dq-cli/tests/cli_query.rs`: ≥8 integration tests via `dq::run`:
  - `dq query '.spec.replicas' deploy.yaml` → exit 0, stdout `3`.
  - `dq query '.spec.containers[].image' deploy.yaml -F json` → exit 0, stdout is a JSON array.
  - `dq query '.does.not.exist' f.yaml -F json` → exit 0, stdout `[null]` (jq's "missing returns null" semantics).
  - `dq query '.foo |=' f.yaml` → exit 3 (`PARSE_ERROR`), stderr mentions the syntax problem.
  - `dq query '. + 1' string-only.yaml` → exit 1 (`GENERIC`), stderr mentions the type error.
  - `dq query '.x' f.yaml -i` → exit 6 (`INVALID_INPUT`), stderr names `--in-place`.
  - `dq query '.foo' - -F yaml` with stdin → exit 0.
  - `dq query '.foo' -` (no `-F`) → exit 6, stderr names "stdin requires -F".
  - `dq query '.kind' multi.yaml --doc 1` → exit 0, stdout is the second document's `.kind`.
  - `dq query '.x' f.yaml -F sarif` → exit 6 (sarif rejected for query results via `BannedReporter`).

## 6. CLI: `dq set --jq EXPR`

- [x] 6.1 [writer] `crates/dq-cli/src/cli/args/set.rs`: add the new field:
  ```rust
  /// Apply a jq transform to the entire document. Mutually exclusive with
  /// the positional VALUE and with `--value-from`. The pointer argument MUST
  /// be omitted (or be `/`) when `--jq` is used — the transform is applied
  /// to the document root.
  ///
  /// NOTE: --jq routes through the format's native re-emit path, which
  /// drops YAML comments. Use point-edits (`dq set FILE POINTER VALUE`) when
  /// comment preservation matters.
  #[arg(long = "jq", value_name = "EXPR", conflicts_with = "value", conflicts_with = "value_from")]
  pub jq: Option<String>,
  ```
  Make `pointer` field optional (`pub pointer: Option<String>`) so `dq set FILE --jq EXPR -i` parses without a positional pointer. The handler validates the pointer-vs-jq pairing at runtime.

- [x] 6.2 [writer] `crates/dq-cli/src/commands/set.rs::run`: add the `--jq` branch at the top of the function (after `cli.ensure_write_flags_consistent()?`):
  ```rust
  if let Some(expression) = &args.jq {
      // Validate pointer-vs-jq pairing.
      if let Some(p) = &args.pointer && !p.is_empty() && p != "/" {
          return Err(anyhow::Error::new(InvalidInput::new(
              "--jq applies to the document root; positional POINTER is not accepted",
          )));
      }
      return run_jq_transform(cli, args, expression, input_format, use_color, out);
  }
  // ... existing splice path unchanged ...
  ```

  New helper `run_jq_transform`:
  ```rust
  fn run_jq_transform(
      cli: &Cli,
      args: &SetArgs,
      expression: &str,
      input_format: Option<&str>,
      use_color: bool,
      out: &mut dyn Write,
  ) -> anyhow::Result<()> {
      tracing::debug!(
          "dq set --jq routes through Format::write_with_options; comments will be lost in re-emit"
      );

      let engine = std::sync::Arc::new(
          dq_transform::JqEngine::compile(expression)
              .map_err(|e| anyhow::Error::new(jq_compile_to_parse(e, &args.file, expression)))?,
      );

      let op = JqFileOp { cli, input_format, use_color, engine };
      let files = bulk::expand_glob(&args.file)?;
      bulk::run_per_file(files, &op, cli, out)
  }
  ```

  And the new `JqFileOp` (mirrors `SetFileOp` but uses the re-emit path):
  ```rust
  struct JqFileOp<'a> {
      cli: &'a Cli,
      input_format: Option<&'a str>,
      use_color: bool,
      engine: std::sync::Arc<dq_transform::JqEngine>,
  }

  impl<'a> FileOp for JqFileOp<'a> {
      fn apply(&self, path: &Utf8Path) -> anyhow::Result<FileOpResult> {
          let format = pick_format(path, self.input_format)?;
          let original_bytes = read_bytes(path)?;
          let document = format.parse(&original_bytes).map_err(anyhow::Error::new)?;

          let serde_value = super::io_helpers::value_to_serde_json(document.value());
          let outputs = self.engine.run(&serde_value)
              .map_err(|e| anyhow::anyhow!("{e}"))?;

          // Validate stream length exactly 1.
          let single = match outputs.as_slice() {
              [v] => v,
              [] => return Err(anyhow::Error::new(InvalidInput::new(
                  "--jq filter produced 0 outputs (document would become empty); wrap in `[...]` or pick a non-empty filter",
              ))),
              many => return Err(anyhow::Error::new(InvalidInput::new(format!(
                  "--jq filter produced {} outputs (expected exactly 1); wrap iteration in `[...]` to collect",
                  many.len(),
              )))),
          };

          let new_value = serde_json_to_dq_value(single);
          let new_doc = dq_core::Document::value_only(new_value, document.format_tag());

          let mut final_bytes = Vec::new();
          format.write_with_options(&new_doc, &mut final_bytes, &self.cli.write_options())
              .map_err(anyhow::Error::new)?;

          let diff = if self.cli.diff {
              let original_str = String::from_utf8_lossy(&original_bytes);
              let modified_str = String::from_utf8_lossy(&final_bytes);
              Some(crate::diff::render_unified(&original_str, &modified_str, path.as_str(), self.use_color))
          } else {
              None
          };

          Ok(FileOpResult::Modified { output_bytes: final_bytes, diff })
      }
  }
  ```

  The `serde_json_to_dq_value` function is already private to `set.rs` from the M2 work; reuse it as-is.

- [x] 6.3 [test-writer] `crates/dq-cli/src/commands/set.rs` `#[cfg(test)] mod tests`: add ≥4 unit tests next to the existing tests:
  - `dq set f.yaml --jq '.spec.replicas |= . + 1' -i` increments the field on disk.
  - `dq set f.yaml --jq '.[]' -i` against an array file returns InvalidInput (multi-output).
  - `dq set f.yaml --jq 'empty' -i` returns InvalidInput (zero outputs).
  - `dq set f.yaml /x 5 --jq '. + 1'` is rejected by clap at parse time (clap-level conflict).

- [x] 6.4 [test-writer] `crates/dq-cli/tests/cli_set_jq.rs`: ≥6 integration tests via `dq::run`:
  - `dq set --jq '.spec.replicas |= . + 1' deploy.yaml -i` increments the field on disk.
  - `dq set --jq '. + {"newKey": "newValue"}' obj.yaml -i` adds the new key.
  - `dq set --jq 'del(.metadata.annotations.old)' f.yaml -i` removes the key.
  - `dq set --jq '.spec.replicas |= . + 1' deploy.yaml --diff` renders unified diff, file unchanged.
  - `dq set --jq '.foo |=' f.yaml -i` exits 3 (compile error).
  - `dq set --jq '. + 1' string-only.yaml -i` exits 1 (runtime type error).
  - `dq set --jq '.spec.replicas |= . + 1' deploy.yaml --check` against a file that would change exits 1.
  - `dq set --jq '.spec.replicas |= . + 0' deploy.yaml --check` against a no-op transform exits 0.
  - `dq set --jq '.x |= 2' commented.yaml -i` succeeds AND drops the YAML comments (assert the `# comment` line is gone — documents the re-emit behaviour).

## 7. Smoke + golden coverage

- [x] 7.1 [test-writer] Extend `crates/dq-cli/tests/cli_smoke.rs`: 2 new smoke scenarios:
  - `dq query '.spec.containers[].image' tests/fixtures/golden/k8s-deployment.yaml -F json` returns a JSON array of image strings (exit 0).
  - `dq set --jq '.spec.replicas |= . + 1' tests/fixtures/golden/k8s-deployment.yaml --diff` renders a unified diff.

- [x] 7.2 [test-writer] If a golden runner exists (`crates/dq-cli/tests/golden.rs` or `crates/dq-core/tests/...`): add a "jq round-trip" group asserting that `parse → set --jq '.' → parse` produces a structurally-equal `Value` for the existing M5 golden fixtures (HCL / INI / dotenv / CSV / TSV / frontmatter, where applicable). Skip read-only formats (Dockerfile, ignore-list).

## 8. Plan delta + meta + verification

- [x] 8.1 [orch] Update `dq-plan.md` M7 section with `✅ Implemented YYYY-MM-DD (см. [openspec/changes/archive/<date>-add-transform-layer/](...))` marker. Add cross-link.

- [x] 8.2 [orch] Update `README.md` status line: `M7 alpha — adds dq query (jq) + dq set --jq`. Add an "Examples" subsection demonstrating `dq query '.spec.replicas' deploy.yaml` and `dq set --jq '.spec.replicas |= . + 1' deploy.yaml -i`.

- [x] 8.3 [orch] `cargo build --workspace --all-targets` зелёный.

- [x] 8.4 [orch] `cargo build -p dq-transform --no-default-features` зелёный.

- [x] 8.5 [orch] `cargo test --workspace --all-features` — все existing M1–M6 тесты + new M7 тесты зелёные. Runtime cold ≤ 30s.

- [x] 8.6 [orch] `cargo test -p dq-transform --no-default-features` — feature-disabled tests pass.

- [x] 8.7 [orch] `cargo clippy --workspace --all-targets --all-features -- -D warnings` зелёный.

- [x] 8.8 [orch] `cargo fmt --all -- --check` зелёный.

- [x] 8.9 [orch] `cargo deny check` зелёный (license + advisory check on the three new deps).

- [x] 8.10 [orch] Manual smoke по DoD M7:
  - `dq query '.spec.replicas' deployment.yaml` returns `3`.
  - `dq query '.spec.containers[].image' deployment.yaml -F json` returns a JSON array.
  - `dq set --jq '.spec.replicas |= . + 1' deployment.yaml -i` increments the field in place.
  - `dq set --jq '.foo |=' deployment.yaml` exits 3 (compile error).
  - `dq query '. + 1' string-only.yaml` exits 1 (runtime error).

- [x] 8.11 [orch] `openspec validate add-transform-layer --strict` — `Change is valid`.

- [x] 8.12 [orch] `openspec archive add-transform-layer` — после merge в main (rename folder to `archive/<date>-add-transform-layer/`).
