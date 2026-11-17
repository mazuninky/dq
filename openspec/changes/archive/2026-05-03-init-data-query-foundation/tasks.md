Делегирование: каждая задача помечена `[orch]` (оркестратор выполняет напрямую) или `[writer]` / `[test-writer]` (отдаётся в `rust-cli-writer` / `rust-cli-test-writer` через Agent tool). Задачи `[writer]` / `[test-writer]` заточены под self-contained prompt — содержат файл, цель и ограничения. Каждая задача ≤ 2 часов.

## 1. Project skeleton & dev infra

- [x] 1.1 [orch] Создать `.gitignore` (target/, .DS_Store, *.swp, /dist/, /node_modules/, openspec/changes/.openspec.cache/)
- [x] 1.2 [orch] Создать `rust-toolchain.toml` с `[toolchain] channel = "1.94.0", components = ["rustfmt", "clippy"], profile = "minimal"`
- [x] 1.3 [orch] Создать `clippy.toml` с baseline lints (`avoid-breaking-exported-api = false`, `disallowed-methods = []` placeholder)
- [x] 1.4 [orch] Создать `deny.toml` для `cargo-deny` с разрешёнными лицензиями (MIT, Apache-2.0, BSD-2/3, ISC, Unicode-DFS-2016, MPL-2.0)
- [x] 1.5 [orch] Создать `lefthook.yml` с pre-commit хуками: `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo nextest run --all-features`. Запрет `--no-verify` уже стоит в [.claude/settings.json](../../../.claude/settings.json).
- [x] 1.6 [orch] Обновить `README.md` до короткого обзора: что такое dq, текущий статус (M1 alpha — read-only), ссылка на `dq-plan.md`, ссылка на роудмап
- [x] 1.7 [writer] Создать workspace `Cargo.toml` в корне репозитория. Members: `crates/dq-core`, `crates/dq-transform`, `crates/dq-exec`, `crates/dq-lint`, `crates/dq-cli`. `[workspace.package]` с edition = "2024", authors, license = "MIT", repository, rust-version = "1.94". `[workspace.dependencies]` с pinned versions: serde 1, serde_json 1, serde_yml 0.0.12, toml 0.8, indexmap 2 (with serde feature), camino 1, thiserror 2, anyhow 1, tracing 0.1, tracing-subscriber 0.3 (with env-filter, fmt features), clap 4 (with derive, wrap_help features), clap_complete 4, clap_mangen 0.2, jsonpath-rust 0.7, toon-format 0.4. dev-deps в workspace: assert_cmd 2, predicates 3, tempfile 3, insta 1 (with yaml feature), pretty_assertions 1, proptest 1. `[profile.release] lto = true, codegen-units = 1, strip = true`. Ограничения: НЕ добавлять в скелет saphyr/toml_edit/comrak/jaq-* — это для M2/M7/M9. НЕ добавлять serde_yaml (deprecated). НЕ ставить unstable features.
- [x] 1.8 [writer] Создать пустые crates `crates/dq-transform`, `crates/dq-exec`, `crates/dq-lint`. Каждый: `Cargo.toml` (наследует `workspace = true` для package metadata), `src/lib.rs` с одной строкой `//! placeholder for <milestone N>` и `pub fn _placeholder() {}` (чтобы `cargo build` был доволен). Никаких dependencies в этих крейтах в M1.

## 2. dq-core: data model

- [x] 2.1 [writer] Создать `crates/dq-core/Cargo.toml` с зависимостями (workspace inherit): serde, serde_json, serde_yml, toml, indexmap, camino, thiserror. Никакого serde_yaml. Никакого jsonpath-rust (он в `dq-cli`).
- [x] 2.2 [writer] Создать `crates/dq-core/src/lib.rs` с публичным re-export'ом модулей (`document`, `format`, `pointer`, `error`, `parsers`) и `pub type Result<T> = std::result::Result<T, Error>;`. lib.rs ≤ 30 строк, никаких re-export'ов из serde.
- [x] 2.3 [writer] `crates/dq-core/src/document.rs`: enum `Value { Null, Bool(bool), Int(i64), BigInt(String), Float(f64), BigFloat(String), String(String), Array(Vec<Value>), Map(IndexMap<String, Value>) }` + `Document` newtype-обёртка с поддержкой single-document и `MultiDocument(Vec<Value>)` (для multi-doc YAML). Реализовать `Display` (debug-friendly), `From<bool/i64/f64/String/&str>`, `serde::Serialize`. Не реализовывать `serde::Deserialize` — парсеры конвертируют через свои промежуточные типы (см. tasks 2.6-2.8).
- [x] 2.4 [writer] `crates/dq-core/src/error.rs`: `pub enum Error` через `thiserror`. Варианты: `Io { path: Utf8PathBuf, #[source] source: std::io::Error }`, `Parse { file: Utf8PathBuf, line: u32, col: u32, span: std::ops::Range<usize>, snippet: String, message: String }`, `Path { pointer: String, matched_prefix: String, kind: PathErrorKind, did_you_mean: Vec<String> }` где `PathErrorKind = MissingKey | OutOfBounds | TypeMismatch { expected: &'static str, found: &'static str }`, `UnsupportedFormat { name: String }`, `Format { format: &'static str, message: String }`. Каждый вариант имеет хотя бы один unit-тест в pure функции `kind_name()` (для exit-code mapping).
- [x] 2.5 [writer] `crates/dq-core/src/pointer.rs`: `pub struct Pointer(Vec<Segment>); enum Segment { Key(String), Index(usize) }`. Методы: `parse(s: &str) -> Result<Pointer>` (обрабатывает `~0` `~1`, root = empty), `as_canonical(&self) -> String` (re-escape `~` `/`), `resolve<'a>(&self, doc: &'a Value) -> Result<&'a Value>` с богатым error на miss. Helper `did_you_mean(missing: &str, candidates: &[&str]) -> Vec<String>` использует Levenshtein distance ≤ 2, max 3 кандидата. ВАЖНО: не зависеть от внешних крейтов для Levenshtein, написать самим (~30 строк).
- [x] 2.6 [writer] `crates/dq-core/src/format.rs`: `pub trait Format: Send + Sync` с методами `name() -> &'static str`, `extensions() -> &'static [&'static str]`, `parse(bytes: &[u8]) -> Result<Document>`, `write(doc: &Document, w: &mut dyn std::io::Write) -> Result<()>`. Plus `pub fn detect(path: &Utf8Path) -> Option<&'static dyn Format>` через статическую registry-таблицу. В M1 registry содержит 4 элемента: yaml, json, toml, jsonl.
- [x] 2.7 [writer] `crates/dq-core/src/parsers/json.rs`: implement `Format` через `serde_json::from_slice`. Парсер должен сохранять числа: пытается `i64`, при overflow — `BigInt(literal_text)`. Использовать `serde_json::Number::as_str()` или ручной обход `serde_json::Value`. Writer: `serde_json::to_writer_pretty` (2-space indent) для default, `to_writer` (compact) когда вызывается из jsonl/конверсии в один-line.
- [x] 2.8 [writer] `crates/dq-core/src/parsers/yaml.rs`: implement `Format` через `serde_yml::from_slice` → `serde_yml::Value` → `Document`. Поддержать multi-document YAML (`serde_yml::Deserializer::from_slice`). Writer: `serde_yml::to_string`. Number handling: same as JSON — текст из источника при потенциальной потере точности.
- [x] 2.9 [writer] `crates/dq-core/src/parsers/toml.rs`: implement `Format` через `toml::from_slice` → `toml::Value` → `Document`. Writer: `toml::to_string_pretty`.
- [x] 2.10 [writer] `crates/dq-core/src/parsers/jsonl.rs`: implement `Format`. Parse — построчно, каждая строка через `serde_json::from_slice` → element of `Array`. Write — emit each top-level array element on its own compact JSON line.
- [x] 2.11 [writer] `crates/dq-core/src/parsers/mod.rs`: re-export `json`, `yaml`, `toml`, `jsonl` модули и регистрировать их в registry из `format.rs::detect`.
- [x] 2.12 [writer] Helper для `paths`: `pub fn enumerate_pointers(doc: &Value) -> impl Iterator<Item = (Pointer, &'static str)>` (где str — имя типа листа: "null"/"bool"/"int"/"string"/"array"/"object"). Iterator с pre-order обходом; глубина ограничивается естественно структурой документа.

## 3. dq-core: tests

- [x] 3.1 [test-writer] Unit-тесты для `Pointer::parse` в `crates/dq-core/src/pointer.rs`: пустая строка → root, `/foo` → один сегмент, `/foo/bar` → два, `/0/1` → array indices, `~0` ↔ `~`, `~1` ↔ `/`, error на unescaped `~`, error на пустой сегмент `//`. ≥ 12 cases.
- [x] 3.2 [test-writer] Unit-тесты для `did_you_mean`: `("port", &["host", "prot", "porte"])` → `["prot", "porte"]` ordered by distance. ≥ 6 cases. Цель — устойчивость к опечаткам в k8s-lables (например, `app.kubernates.io/name` → `app.kubernetes.io/name`).
- [x] 3.3 [test-writer] Component-тесты в `crates/dq-core/tests/parse_yaml.rs`: 5 fixture-файлов (k8s deployment с annotations и комментариями, helm values, github actions workflow, hugo frontmatter, простой config). Каждый парсится → `Pointer::resolve` достаёт известное значение → equal_to expected.
- [x] 3.4 [test-writer] Component-тесты `crates/dq-core/tests/parse_json.rs`: round-trip через `convert -F json` для big int (4722366482869645213696) — стрингифицированное значение совпадает с исходным.
- [x] 3.5 [test-writer] Component-тесты `crates/dq-core/tests/parse_toml.rs`: nested tables, arrays of tables, datetime literals (хранятся как `Value::String` в M1 — это допущение, не дефект).
- [x] 3.6 [test-writer] Snapshot-тесты `crates/dq-core/tests/error_render.rs` через `insta`: сериализованный `Error::Path` для случая "опечатка в /metadata/lables/app.kubernetes.io~1name" соответствует ожидаемому JSON.

## 4. dq-cli: bin foundation

- [x] 4.1 [writer] `crates/dq-cli/Cargo.toml`: dep на dq-core, dq-transform (workspace path), dq-exec, dq-lint (для будущих M; в M1 используются только dq-core), плюс clap, anyhow, tracing, tracing-subscriber, serde_json, jsonpath-rust, toon-format, camino. dev-deps: assert_cmd, predicates, tempfile, insta, pretty_assertions. `[[bin]] name = "dq" path = "src/main.rs"`. Никакого `[lib]` секции в M1 — handlers — `pub`-функции в src/, тесты импортируют через `mod` нотацию (нет, лучше `[lib]`-секция тоже, чтобы integration-тесты имели доступ; см. /rust-cli convention). Включи `[lib] name = "dq" path = "src/lib.rs"` + `[[bin]] name = "dq" path = "src/main.rs"`.
- [x] 4.2 [writer] `crates/dq-cli/src/exit_code.rs`: `pub mod exit_code` с константами SUCCESS=0, GENERIC=1, NOT_FOUND=2, PARSE_ERROR=3, VALIDATE_FAIL=4, IO_ERROR=5, INVALID_INPUT=6. Функция `pub fn exit_code_for_error(err: &anyhow::Error) -> i32`: downcast_ref на `dq_core::Error` варианты, mapping. Default — GENERIC.
- [x] 4.3 [writer] `crates/dq-cli/src/cli/args.rs`: `#[derive(Parser)] pub struct Cli` с глобальными флагами (`-F/--format`, `-v/--verbose` count, `-q/--quiet` conflicts_with verbose, `--no-color`, `--no-pager`, `--doc`, `--in-place`, `--diff`, `--backup`). Enum `Command` со variants `Get(GetArgs), Exists(ExistsArgs), Keys(KeysArgs), Values(ValuesArgs), Len(LenArgs), Type(TypeArgs), Paths(PathsArgs), Select(SelectArgs), Convert(ConvertArgs), Validate(ValidateArgs), GenerateDocs(GenerateDocsArgs)`. Каждая `*Args` — отдельная struct в `cli/args/<cmd>.rs`. Глобалы `--in-place/--diff/--backup` имеют helper `Cli::reject_write_flags(&self) -> anyhow::Result<()>` который зовётся первой строкой каждого handler'а.
- [x] 4.4 [writer] `crates/dq-cli/src/output/mod.rs`: `trait Reporter { fn report(&self, value: &serde_json::Value, w: &mut dyn Write) -> anyhow::Result<()>; }`. Реализации в `output/console.rs`, `output/json.rs`, `output/toon.rs`. `ConsoleReporter` принимает `use_color: bool` в конструкторе. `JsonReporter` пишет `serde_json::to_writer_pretty`. `ToonReporter` делегирует в `toon_format::encode`. Enum `OutputFormat { Console, Json, Yaml, Toml, Jsonl, Toon }`. `OutputFormat::default() = Console`.
- [x] 4.5 [writer] `crates/dq-cli/src/lib.rs` re-export-модули `cli`, `output`, `commands`, `exit_code`. Модуль `cli::run` (или `lib.rs::run(args, stdout, stderr) -> anyhow::Result<()>`) — точка входа для тестов и main.
- [x] 4.6 [writer] `crates/dq-cli/src/main.rs`: ≤ 80 не-пустых строк. Делает 5 вещей: (a) на Unix вызов `unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };` через `libc` крейт; (b) `let cli = Cli::parse();`; (c) init tracing с `tracing_subscriber::fmt().with_env_filter(EnvFilter::from_default_env_or(...)).with_target(false).try_init()`; (d) lock stdout/stderr один раз, строит `reporter_for_format(cli.format, !cli.no_color)`; (e) dispatch в `commands::*::run(...)`, mapping err → `exit_code_for_error`. Только этот файл может звать `std::process::exit`. Тесты на `cli::run` идут мимо `main.rs`.

## 5. dq-cli: commands

- [x] 5.1 [writer] `crates/dq-cli/src/commands/get.rs`: `pub fn run(args: &GetArgs, reporter: &dyn Reporter, out: &mut dyn Write) -> anyhow::Result<()>`. Шаги: open file → detect format (или из cli.format) → parse → resolve pointer → pass `serde_json::Value` to reporter. На path miss — `anyhow::Error` обёрнутый над `dq_core::Error::Path` (mapping в exit code 2 происходит на верхнем уровне).
- [x] 5.2 [writer] `crates/dq-cli/src/commands/exists.rs`: пишет ничего, exit-код через `Result::Ok(())` или `Err(anyhow!(missing))`. Parse + resolve, success/error → exit 0/1. Эта команда должна возвращать `anyhow::Error::new(SilentError)` чтобы main.rs не печатал stderr.
- [x] 5.3 [writer] `crates/dq-cli/src/commands/keys.rs` + `values.rs`: enumerate object keys/values, error если pointer указывает не на object. `keys` пишет JSON-array строк, `values` — JSON-array любых значений. Console-format: один key/value на строку.
- [x] 5.4 [writer] `crates/dq-cli/src/commands/len.rs` + `type_cmd.rs` (нельзя `type.rs` — keyword): scalar output. `len` — i64 (длина массива/строки/object). `type_cmd` — string (`null`/`bool`/`int`/`float`/`string`/`array`/`object`).
- [x] 5.5 [writer] `crates/dq-cli/src/commands/paths.rs`: использует `dq_core::enumerate_pointers`. Console: pointer на строку. JSON: object {pointer: type_name}.
- [x] 5.6 [writer] `crates/dq-cli/src/commands/select.rs`: parse jsonpath-rust expr → конвертит `dq_core::Value` → `serde_json::Value` → запускает запрос → result как JSON-array. Если не json-formatted output — пишет одно значение на строку.
- [x] 5.7 [writer] `crates/dq-cli/src/commands/convert.rs`: detect input format → parse → write через `Format::write` для целевого `OutputFormat`. Если input содержит metadata, теряемую в target (комментарии при → JSON), пишет `tracing::warn!`.
- [x] 5.8 [writer] `crates/dq-cli/src/commands/validate.rs`: parse, on success — exit 0 silent; on failure — write structured `Parse` error через выбранный reporter (на stderr) и mapping → exit 4.
- [x] 5.9 [writer] `crates/dq-cli/src/commands/generate_docs.rs`: hidden subcommand (`#[command(hide = true)]`). Использует `clap_mangen::Man` и `clap_complete::generate` для всех 4 shells. Вывод в указанную директорию.
- [x] 5.10 [writer] `crates/dq-cli/src/commands/mod.rs`: dispatch с match по enum Command, передаёт reporter и stdout writer в каждый run(). Один-source-of-truth маршрутизация.
- [x] 5.11 [writer] `crates/dq-cli/src/error.rs` (CLI-внутренний): newtype `pub struct SilentError;` с `impl Display + std::error::Error` — для `exists`, чтобы exit 1 без печати. Mapping в `exit_code_for_error` — IO_ERROR? Лучше — отдельная константа SILENT_FAIL=1 (=GENERIC).

## 6. dq-cli: tests

- [x] 6.1 [test-writer] Unit-тесты handler'ов `crates/dq-cli/tests/unit_get.rs` etc. через `cli::run` с tempfile-ами. На `Vec<u8>` writer'ах. Каждый command ≥ 3 cases (success, missing pointer, invalid format).
- [x] 6.2 [test-writer] CLI integration `crates/dq-cli/tests/cli_smoke.rs` через `assert_cmd`: 10 smoke-сценариев из `dq-plan.md` (k8s manifest get, helm values paths, package.json convert, github actions select). Проверка exit-code и stdout. Использовать `--no-color` чтобы snapshot был стабильным.
- [x] 6.3 [test-writer] Snapshot-тесты `crates/dq-cli/tests/snapshots/` (insta). Render structured Path/Parse error в `-F json` и в console — 8 cases. `--no-color` для console-snapshot'ов.
- [x] 6.4 [test-writer] Property-тесты `crates/dq-cli/tests/prop_pointer.rs` (proptest): для случайно сгенерированного `Value` дерева, для каждого pointer, возвращённого `enumerate_pointers`, `Pointer::parse(canonical_str).resolve(&value)` равен исходному узлу. ≥ 100 generated cases per run, seed pinned for reproducibility.
- [x] 6.5 [test-writer] Golden-file runner `crates/dq-cli/tests/fixtures/` — складирует 50+ репрезентативных файлов (k8s/helm/github-actions из открытых проектов с MIT/Apache лицензией). Test enumerates all files → run `paths` → snapshot compared. Любой regression в парсере виден сразу.
- [x] 6.6 [test-writer] SIGPIPE smoke `crates/dq-cli/tests/cli_sigpipe.rs`: pipe в `head -n 1`, проверить exit-code и отсутствие panic-сообщений в stderr. Skip on Windows.
- [x] 6.7 [test-writer] Verify reject-write-flags: `dq get foo.yaml /x -i` exits 1 with "unsupported in this build" в stderr.
- [x] 6.8 [test-writer] Verify color resolution: precedence test (`--no-color` > `NO_COLOR=1` > `CLICOLOR_FORCE=1` > TTY). Запускается без `std::env::set_var` — через `Command::env` API.

## 7. Plan delta

- [x] 7.1 [orch] Обновить `dq-plan.md` секцию `Tech stack` по [docs/archive/plan-validation-rust-cli-2026-05-03.md](../../../docs/archive/plan-validation-rust-cli-2026-05-03.md): добавить `camino`, `tracing`/`tracing-subscriber`, переписать пункт про errors (thiserror+anyhow primary, miette опционально для рендера), сменить пункт про TOON encoder на крейт `toon-format`.
- [x] 7.2 [orch] Обновить `dq-plan.md` секцию `Архитектура → Layer 1 (dq-core)`: добавить `pub type Result<T>` alias, чёткий `Error` enum через thiserror.
- [x] 7.3 [orch] Обновить `dq-plan.md` секцию `Архитектура → CLI (dq-cli)`: добавить SIGPIPE handler, Reporter trait + DI, exit-code модуль с константами, обязательность `tracing` (никаких println!), non-interactive принцип.
- [x] 7.4 [orch] Обновить `dq-plan.md` секцию `Глобальные флаги`: добавить `-v/--verbose` (count), `-q/--quiet`.
- [x] 7.5 [orch] Обновить `dq-plan.md` секцию `Поддерживаемые форматы`: TOON write — через `toon-format` крейт, не свой энкодер.
- [x] 7.6 [orch] Обновить `dq-plan.md` секцию `Tech stack → Build / CI`: добавить `[profile.release]` с lto/codegen-units/strip.
- [x] 7.7 [orch] Обновить `dq-plan.md` Definition of done для M1: добавить SIGPIPE smoke, `--no-color`/NO_COLOR precedence test, snapshot-тест structured error, property-тест round-trip Pointer↔canonical.
- [x] 7.8 [orch] Добавить в `dq-plan.md` раздел `Тестирование` со ссылкой на skill `/rust-cli` (`references/cli-testing.md`) как нормативный документ — с пирамидой 75/20/5, `try_init()`, запретом `pub(crate)` escape-hatches, запретом `std::env::set_var("NO_COLOR", ...)`.

## 8. Verification & sign-off

- [x] 8.1 [orch] `cargo build --workspace --all-targets` зелёный.
- [x] 8.2 [orch] `cargo nextest run --workspace --all-features` зелёный.
- [x] 8.3 [orch] `cargo clippy --workspace --all-targets --all-features -- -D warnings` зелёный.
- [x] 8.4 [orch] `cargo fmt --all -- --check` зелёный.
- [x] 8.5 [orch] `cargo deny check` зелёный (license + advisory + sources).
- [x] 8.6 [orch] Manual smoke: запустить `dq get`, `dq paths`, `dq select`, `dq convert`, `dq validate` на 5 файлах из fixtures/, убедиться, что output разумный.
- [x] 8.7 [orch] `dq generate-docs --output-dir /tmp/dq-docs` создаёт man pages и completions.
- [x] 8.8 [orch] Сверить все DoD пункты M1 из обновлённого `dq-plan.md`. Архивировать [docs/archive/plan-validation-rust-cli-2026-05-03.md](../../../docs/archive/plan-validation-rust-cli-2026-05-03.md) (move в `docs/archive/`).
- [x] 8.9 [orch] `openspec validate init-data-query-foundation --strict` зелёный.
- [x] 8.10 [orch] Архивировать change через `openspec archive init-data-query-foundation` после merge'а в main.
