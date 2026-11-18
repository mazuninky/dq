# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- M11 Phase 5 — extended rulesets `@std/terraform` and `@std/openapi`
  land, raising the standard rule library to 8 namespaces (`k8s`,
  `dockerfile`, `npm`, `github-actions`, `markdown`, `jsonschema`,
  `terraform`, `openapi`) and 64 embedded rules. Both namespaces ship
  in every build (no feature gates).
  - `@std/terraform` ships 8 rules covering common security, hygiene,
    and reproducibility footguns: `no-hardcoded-secrets` (literal
    `password = "..."` / `api_key = "..."` / `token = "..."`
    assignments — variable and data-source references that the HCL
    parser surfaces as `${...}` template strings are silently
    allowed), `tag-required` (every `resource "aws_*"` block has a
    top-level `tags` map), `provider-pinned` (every `provider` block
    has `version = "..."`), `no-public-ingress` (`cidr_blocks` /
    `source_ranges` containing `0.0.0.0/0` on AWS security groups
    and GCP firewalls), `state-backend-required` (every `terraform`
    block has a nested `backend "..." { ... }`), `module-pin-version`
    (remote `module { source = "..." }` blocks have `version`;
    local-path sources `./` and `../` are exempt),
    `output-no-sensitive-without-flag` (output names matching
    `secret|password|token|key|credential` must set
    `sensitive = true`), and `variable-has-description` (every
    `variable` block has a non-empty `description`). Fixtures cover
    35 cases across the 8 rules.
  - `@std/openapi` ships 6 rules — implemented entirely through plain
    jq + JSON Schema over the YAML/JSON parser, with no `oas3`
    dependency (see design D6 implementation note for the rationale):
    `info-required-fields` (uses `check.schema_file:
    ./openapi-info.schema.json` to require non-empty
    `info.title`/`info.version`), `paths-non-empty` (the document
    declares at least one path), `operation-id-unique` (group every
    `operationId` and emit one diagnostic per duplicated id with a
    count), `response-200-or-201-required` (every operation under
    `get|post|put|delete|patch|options|head|trace` declares at least
    one 2xx success response — any specific code from `200`–`299`
    such as `200`/`201`/`202`/`204` or the wildcard `2XX`), `no-trailing-slash`
    (path keys other than the root `/` must not end with `/`), and
    `security-defined` (top-level `security:` is non-empty OR every
    operation has its own `security:` — operations can opt out
    explicitly with `security: []`). Each rule's `match.filter`
    pins on `.openapi != null and (.openapi | type == "string")` so
    non-OpenAPI YAML/JSON is silently skipped. Fixtures cover 28
    cases across the 6 rules.
  - HCL backend has **no span information** (see
    `crates/dq-core/src/parsers/hcl.rs` doc-comment) — every Terraform
    diagnostic resolves to `(line=1, col=1)`. The fixture runner
    exercises rule logic; precise sub-line coordinates land when the
    HCL parser grows a span-aware path. OpenAPI diagnostics, by
    contrast, carry full YAML/JSON spans through the existing
    `loc.pointer` lookup.
  - New `dq_lint::std_schema_files(namespace)` accessor enumerates
    every embedded JSON Schema sidecar for a namespace as
    `(filename, contents)` pairs. The integration test runner uses
    it to stage `helm-values-template.schema.json` and
    `openapi-info.schema.json` next to the rule YAML so
    `check.schema_file:` resolution works end-to-end against
    tempdir-staged copies of the embedded namespaces.

- M11 Phase 3 — `Rule.check` becomes a four-variant enum and JSON Schema
  2020-12 lands as a first-class rule type. The `dq-exec::rule::Check`
  enum now covers `Jq` (legacy), `Schema` (inline JSON Schema 2020-12),
  `SchemaFile` (sibling-relative schema document), and `Composite`
  (parser-side stub for Phase 4 cross-format checks). Existing
  `check: { jq, message }` rules continue to parse unchanged through the
  `RuleCheck` alias. Schema validators are compiled once at
  `Evaluator::new` (via the new `dq-exec::schema_check` module) and
  reused across every `evaluate_file` call. Each validation error
  produces one `Diagnostic` with `pointer` recovered from
  `error.instance_path` (RFC 6901), the `keyword_location` embedded in
  the message, and `(line, col)` looked up through
  `Ir::line_col_for(&pointer)` (or `(1, 1)` fallback when no span is
  available). The `schema_file:` resolver canonicalises both the rule
  directory and the schema path and rejects absolute paths
  (`ExecError::SchemaFileAbsolutePath`) and `..`-escapes
  (`ExecError::SchemaFileEscapesRuleDir`). External `$ref` targets
  (`http://`, `https://`, `file://`, or any scheme other than internal
  fragment refs) are rejected at compile time
  (`ExecError::SchemaCompile`) — the validator runs with the default
  `referencing::DefaultRetriever`, which performs no network or
  filesystem reads. New `ExecError` variants:
  `CheckMutuallyExclusive`, `CheckMissing`, `CompositeIncomplete`,
  `CompositeExtractNotArray`, `CompositeExtractMalformed`,
  `CompositeExtractUnknownFormat`, `CompositeDepthExceeded`,
  `SchemaCompile`, `SchemaFileAbsolutePath`,
  `SchemaFileEscapesRuleDir` — each with stable `kind_name()` strings
  and CLI exit-code mappings (`composite_extract_*` /
  `composite_depth_exceeded` → `GENERIC` (1); the rest →
  `PARSE_ERROR` (3)). New `@std/jsonschema` ruleset shipping three
  reference rules: `jsonschema.kubernetes-crd-shape` (inline schema
  validating `apiVersion` / `kind` / `metadata.name`),
  `jsonschema.helm-values-against-schema` (template using
  `schema_file: ./helm-values-template.schema.json`), and
  `jsonschema.openapi-3.1-shape` (inline schema for OpenAPI 3.1 root
  structure). Each ships with a co-located `*.test.yml` fixture; new
  `dq_lint::std_schema(namespace, file)` accessor exposes embedded
  schema sidecars. Workspace dependency:
  `jsonschema = { version = "0.34", default-features = false }` —
  the `resolve-http` and `resolve-file` features stay off so no HTTP
  or FS access leaks into the validator.

- M11 Phase 1 — XML 1.0 read+write format. New `XmlFormat` (registered for
  `.xml` extension; selectable via `-F xml`) backed by `quick-xml 0.36`
  parses and serialises XML through a conventional-key mapping onto the
  existing `Value` enum: `<tag>` becomes `Map { tag => Array<Map { ... }> }`
  on the parent (multi-element same-tag children stored in a single Array
  to preserve document order — even single occurrences are wrapped in a
  one-element array so `Pointer` indexing is stable). Element attributes
  live under `@attrs`; element text body under `#text`; comments under
  `#comments`; CDATA blocks under `#cdata`; processing instructions under
  `#pi`; the `<?xml ...?>` declaration under top-level `#xml`. Namespace
  prefixes (`xmlns:foo`, `foo:tag`) are retained verbatim. Round-trip is
  **partial**: structure, attributes, comments, CDATA, PIs, namespace
  prefixes, and the XML declaration are preserved, but mixed content
  (text interleaved with elements in the same parent — e.g.
  `<p>Hello <b>world</b>!</p>`) is folded into `#text` and inner element
  positions are lost. Mixed-content detection emits a `tracing::warn!`
  line so users are aware. New `FormatTag::Xml` variant; `OutputFormat::Xml`
  is wired into `dq convert -F xml`. XML query verbs (`get`, `paths`,
  `keys`, …) work via the conventional-key tree like any other map-shaped
  format. Textual-edit `set` / `del` operations are not supported on XML
  documents — writes go through `Format::write` whole-document re-emission.
- `loc.pointer` rule field (jq expression returning a JSON Pointer string).
  When set, the lint evaluator looks up the pointer in the input document's
  provenance map and resolves the diagnostic's `(line, col)` from the
  parser-recorded byte span — no more `loc.line` int-coercion to recover
  positions for span-aware formats (YAML, JSON, TOML).
- `dq_core::Ir::line_col_for(&Pointer) -> Option<(u32, u32)>` for callers
  that need the `(line, col)` of a pointer's value through the IR.
- `dq_transform::ir_to_val` / `dq_transform::val_to_owned_ir` IR-aware
  variants of the value adapter (provenance is dropped on the way into jaq
  and reconstructed as `Provenance::Synthetic { Computed }` on the way out
  — see `data-query-transform` spec for the contract).
- `dq_core::Value::to_serde_json` and `dq_core::Value::from_serde_json`
  promoted from private duplicates in `dq-cli` and `dq-core::transform` to
  a single public API.
- `fix.ops` rule field (jq expression returning a JSON Patch array). When
  set, the autofix engine applies the patch via `EditScript::apply`
  against the parsed document, preserving comments and surrounding bytes.
  `add`, `replace`, and `remove` ops are supported (RFC 6902 subset;
  `move` / `copy` / `test` rejected).
- `dq_core::{EditOp, EditScript}` re-exported at the crate root — the
  per-rule edit vocabulary used by `Fixer` and (later) WASM plugins.
- `@std/npm/has-license` migrated to `fix.ops` as the reference rule with
  comment-preserving autofix. The migration covers the empty-string-value
  case (`/license: ""`); the missing-key case is deferred until mkdir-p
  insertion lands in `Document::set_at`.
- Plugin ABI v0.1.0 (experimental). New `dq-plugin` crate exposes
  `PluginRuntime` over WIT + Component-Model wasmtime. The WIT package is
  `dq:plugin@0.1.0` with host-imported `ir` (read-only document access)
  and `jq` (compile/eval against the document) interfaces, plus a
  `world plugin` that exports `lint() -> list<diagnostic>` and
  `fix() -> result<list<u8>, string>`. Plugins run sandboxed: no WASI,
  no filesystem / network / process control, ~1s of CPU per invocation
  (fuel budget), 64 MiB linear-memory cap. New global `--plugins <DIR>`
  flag on `dq lint` / `dq fix` discovers `*.wasm` files non-recursively
  under `<DIR>` (lexical sort) and loads them through the runtime.
  Feature-gated behind `--features plugins`; without the feature the
  flag still parses, but encountering any `*.wasm` errors with exit `6`
  (`InvalidInput`). Breaking changes to the WIT schema and marshalling
  shapes are possible before `v1.0.0`. See `examples/plugin-rust/` for a
  working Rust reference plugin and the full build recipe.

### Changed

- `dq select` now follows RFC 9535 strictly (jsonpath-rust 0.7 → 1.0).
  - Filters without parentheses (`$.items[?@.n > 1].name`), function
    extensions such as `length(@.tags)`, and negative indices
    (`$.items[-1]`) now work. They used to exit with code 1.
  - The 0.7 extensions `.length()`, `.['x']` and `=~` are rejected with
    exit code 1 and `invalid JSONPath expression:`. Use `length(@.x)`,
    `['x']`, and `match()` or `search()` instead.
  - `in` is not an RFC 9535 operator: `[?(@.name in ["x"])]` returns `[]`
    with exit code 0 instead of matching. A space inside a dot-name is
    part of the name, so `$.s y` looks up the key `"s y"`.
  - `$..*` lists a node's children before descending into them. The
    values are the same; the order differs.
- `-F toon` writes TOON spec v4 (toon-format 0.4 → 0.7). An empty array
  is written as `roles: []` instead of `roles[0]:`. Strings such as
  `#bob` and `+1`, and non-ASCII keys such as `ключ`, are quoted. Tools
  that read dq's TOON output need a v4 reader.
- Markdown IR `position.end` moved for some nodes (comrak 0.52 → 1.0).
  Lists and list items end at their last child: in `> - a\n>\n> b\n` the
  list ends at `1:5`, not `2:1`. Indented code blocks end at their last
  character instead of the start of the next line: a block on lines 8–9
  ends at `9:8`, not `10:0`. Rules and scripts that compare these
  positions need new expectations.
- JSON Schema rules whose root `$schema` names draft-04, draft-06 or
  draft-07 now validate (jsonschema 0.34 → 0.61). jsonschema 0.34 skipped
  the root keywords of such schemas, so these rules reported nothing.
  dq applies 2020-12 semantics to them, so syntax from the older drafts
  fails to compile: array-form `items` (draft-07) and a boolean
  `exclusiveMinimum` (draft-04) are rejected. Schemas copied from
  SchemaStore often declare draft-07 and may start reporting violations.
- Some JSON Schema messages read differently. `pattern` messages quote
  the regex as written in the schema (`\w+@\w+`) instead of a rewritten
  form, a `type` list is printed in a different order (`"null",
  "integer"`), and `$dynamicRef` paths include every evaluation step.
  Exit codes and positions are unchanged.
- TOML parse errors use the toml_edit 0.25 wording. For `bad = ` the
  message is now `string values must be quoted, expected literal string`.
- Building from source needs Rust 1.100 (was 1.94). The Docker images
  build on `rust:1.100-alpine` and run on `alpine:3.24`.
- The plugin host runs on wasmtime 50. The WIT contract
  `dq:plugin@0.1.0` is unchanged, and plugins built with wit-bindgen 0.57
  still load.
- `dq_exec::Evaluator::evaluate_file` now takes `&dq_core::Ir<'_>` instead
  of `&serde_json::Value`. Internally the evaluator still feeds `serde`
  values into jaq; the `Ir` is required so the new `loc.pointer` chain has
  access to the provenance map.
- `dq lint` (and `dq check`) now route YAML and JSON inputs through the
  span-aware parsers, so `loc.pointer`-using rules emit accurate
  `(line, col)` instead of falling through to `(1, 1)`.
- `@std/k8s/image-pull-policy-always` migrated to `loc.pointer`; its
  `check.jq` now emits a pointer per violation that anchors at the
  offending container's `name:` scalar.
- `dq_exec::Fixer::apply` now takes `&mut Document` instead of
  `&serde_json::Value`. The CLI's `dq fix` handler routes the post-fix
  document's bytes directly to disk when only `fix.ops` rules ran,
  preserving comments byte-for-byte. Legacy `fix.jq` rules continue to
  re-emit through the format writer (same comment-loss trade-off as
  `dq set --jq`).
- `dq_exec::FixOutcome` lost its `new_value: serde_json::Value` field;
  the document itself is now the source of truth. A new
  `legacy_jq_applied: bool` field tells the CLI which output path to
  take.
- `RuleFix` schema now accepts both `jq` (legacy) and `ops` (new) fields,
  each `Option<String>`. At least one must be set; both is allowed and
  `ops` wins at runtime (with a `tracing::warn!` shadowing log).
- Plugins are limited in the number of core instances, memories and
  tables they create, and plugins with 64-bit memories or tables are no
  longer accepted.

### Deprecated

- `loc.line` jq override. Backwards-compatible fallback when `loc.pointer`
  is unset or fails to resolve to a span. Removal is deferred to a future
  change once the `@std/*` rule library has fully migrated.
- `fix.jq` whole-document jq fixes. Backwards-compatible; new rules
  should prefer `fix.ops` for comment preservation. Removal is deferred
  to a future change once the `@std/*` rule library has fully migrated.

### Fixed

- A YAML file with a UTF-8 BOM and a single key can be edited: `set /key`
  used to fail with `path '/key' not found` (saphyr-parser 0.0.6 → 0.3).
  The BOM is kept on write.
- Some YAML documents that saphyr-parser 0.0.6 rejected with a parse
  error now parse.
- Plugins: the 64 MiB memory limit applies to all of a plugin's memories
  together, and a plugin stopped by it reports `exceeded memory limit`.
- Log messages (`WARN` and the `-v` levels) go to stderr instead of
  stdout, so `dq -F json … | jq` gets clean JSON. They are coloured only
  when stderr is a terminal and colour is not turned off.
- `dq set` / `del` / `lint` no longer panic on a file with an empty or
  null key: `"": 1` in YAML, `{"": 1}` in JSON, `"" = 1` in TOML,
  `f: {? &a}` in YAML.
- YAML `set` of a new key after a block scalar (`|`, `>`) or a value on the
  next line puts the key at the mapping's indent instead of inside the
  value. `del` of such a value removes the whole entry.
- YAML `set` / `del` after an alias (`*name`) edit the node you point at;
  they used to edit the node after it, or fail with "not found". Lint
  diagnostics on such files point at the right line instead of line 1.
- YAML `del` of the last key or item writes `a: {}` / `a: []` instead of
  leaving `a:`, which reads as `null`; an emptied document becomes `{}`.
- YAML `del` of a key whose value is a collection removes the whole entry
  instead of failing with "not found" (block collections) or leaving the
  key with a `null` value (`{}` / `[]`).
- YAML `del` of an empty value — `key: |` with no lines, `key: !!str`,
  `key: &a`, `- &a` — removes its own entry; it used to remove the entry
  after it and keep its own.

### Security

- `cargo audit` reports no advisories; it reported 37 vulnerabilities
  and 3 unsound warnings before these updates.
  - wasmtime 44 → 50 in the plugin runtime: RUSTSEC-2026-0222, -0269,
    -0316, -0327, -0359, -0362, -0363, -0379, -0424.
  - self_update 0.44 → 1.3 for `dq self update` drops zip 6
    (RUSTSEC-2026-0367, -0368, -0369, -0429, -0545, -0547, -0548) and
    quick-xml 0.38.
  - quick-xml 0.40 → 0.42 in the XML parser: RUSTSEC-2026-0194, -0195.
  - rustls, rustls-webpki, webpki-roots and webpki-root-certs, used by
    `dq self check` and `dq self update`: RUSTSEC-2026-0285, -0346,
    -0348, -0349, -0381, -0382, -0383.
  - The remaining advisories (crossbeam-epoch, h2, quinn-proto,
    quinn-udp, and the unsound warnings for anyhow, rustix and tokio)
    were closed by updating `Cargo.lock`.
