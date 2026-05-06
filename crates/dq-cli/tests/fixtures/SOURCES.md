# Fixture sources and license attributions

All fixtures under `crates/dq-cli/tests/fixtures/` are **hand-written
originals**, not copied or derived from external projects. They imitate the
*shape* of common configuration files (Kubernetes manifests, Helm values,
GitHub Actions workflows, Cargo / pyproject TOML, npm `package.json`, etc.)
without reproducing copyrighted content.

The hostnames `*.example.test` follow RFC 6761 and are reserved for
documentation / testing. Email addresses use the same domain.

## License

Because every fixture is original, no third-party license attribution is
required. They inherit the workspace license (see top-level `Cargo.toml`).

## Top-level fixtures (used by `cli_smoke.rs`, `cli_snapshots.rs`,
`cli_write_flags.rs`, `unit_*.rs`)

| File                                    | Format | Purpose                                           |
|-----------------------------------------|--------|---------------------------------------------------|
| `broken.json`                           | JSON   | Malformed JSON for `validate` / parse-error tests |
| `helm_values.yaml`                      | YAML   | Helm values without templates                     |
| `helm_values_templated.yaml`            | YAML   | Helm values with `{{ ... }}` blocks (M2 §12.3/4)  |
| `k8s_deployment.yaml`                   | YAML   | k8s Deployment for read-only smoke tests          |
| `k8s_deployment_writable.yaml`          | YAML   | k8s Deployment for `set` / `del` smoke tests      |
| `annotations.yaml`                      | YAML   | Annotation map for `dq del` smoke tests           |
| `package.json`                          | JSON   | npm `package.json` shape                          |
| `server_config.json`                    | JSON   | Server config for `get` / typo-suggestion tests   |
| `server_config.yaml`                    | YAML   | Server config (YAML twin of above)                |

## M5 format-extension fixtures (used by `unit_format_extensions.rs`,
`cli_smoke.rs` M5 scenarios, parser-level `tests/parse_*.rs`)

| File                                    | Format        | Purpose                                                        |
|-----------------------------------------|---------------|----------------------------------------------------------------|
| `terraform_main.tf`                     | HCL           | Terraform-style backend block + variable blocks (M5 Stage 4)   |
| `app.ini`                               | INI           | Anonymous + 2 named sections for INI handler tests             |
| `service.env`                           | dotenv        | KEY=VALUE, quoted, exported, comments                          |
| `users.csv`                             | CSV           | 3 cols × 3 rows with header                                    |
| `ops.tsv`                               | TSV           | 2 cols × 2 rows with header                                    |
| `Dockerfile`                            | Dockerfile    | FROM/RUN/COPY/EXPOSE — read-only format smoke                  |
| `Dockerfile.broken`                     | Dockerfile    | Syntactically broken — `dq validate` exit 4 case               |
| `repo.gitignore`                        | ignore-list   | 5 patterns + comments + blank lines                            |
| `hugo_post.md`                          | frontmatter   | YAML frontmatter + 2-paragraph body                            |
| `mkdocs_post.md`                        | frontmatter   | TOML frontmatter (`+++`) + 1-paragraph body                    |
| `json_frontmatter_post.md`              | frontmatter   | JSON frontmatter (`{` … `}`) + 1-paragraph body                |

All M5 fixtures are synthetic. Hostnames use `*.example.test` (RFC 6761).
None of them reproduce content from real Terraform projects, npm registries,
or Hugo themes.

## Golden round-trip fixtures (`fixtures/golden/`)

The 21 top-level fixtures in `fixtures/golden/` are exercised by
`golden.rs::golden_paths_for_each_fixture_in_dir` (M1) — `dq paths` output
is snapshotted per file. They are also re-exercised by the M2 §12.5
round-trip runner in `golden.rs::roundtrip_parse_then_write_is_byte_identical`.

The 25 fixtures under `fixtures/golden/roundtrip/` are dedicated round-trip
fixtures added in M2 §12.5. They cover:

- **8 YAML** — k8s manifests with anchors / comments, helm values, hugo
  frontmatter, GitHub Actions workflows.
- **6 JSON** — `package.json` shapes, nested config, `eslint.json`-style,
  big-int / arbitrary-precision numbers, unicode strings.
- **6 TOML** — `Cargo.toml`, workspace, files with comments, nested tables,
  datetime literals, pyproject.
- **5 edge cases** — top-level array, blank lines, trailing comments,
  JSONL log records, compact one-line JSON.

The contract is "parse the fixture and the resulting Document's
`original_bytes()` must be byte-identical to the input". A regression in any
parser's span-tracking immediately surfaces.
