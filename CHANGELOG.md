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
    one of `200`, `201`, or the wildcard `2XX`), `no-trailing-slash`
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

### Deprecated

- `loc.line` jq override. Backwards-compatible fallback when `loc.pointer`
  is unset or fails to resolve to a span. Removal is deferred to a future
  change once the `@std/*` rule library has fully migrated.
- `fix.jq` whole-document jq fixes. Backwards-compatible; new rules
  should prefer `fix.ops` for comment preservation. Removal is deferred
  to a future change once the `@std/*` rule library has fully migrated.
