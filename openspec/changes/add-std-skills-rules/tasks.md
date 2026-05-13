# Tasks

## 1. Rule definitions

- [ ] 1.1 [author] `crates/dq-lint/rules/skills/frontmatter.yml`
  - `id: skills.frontmatter`, `severity: error`
  - `match: { format: markdown, glob: '**/SKILL.md', filter: '.frontmatter != null' }`
  - `check.jq` — мирроринг atl логики: missing `name`/`description`, regex check для `name`, folded-length check для `description` ≤ 1024
  - `loc.line: 1` (фронтматтер всегда в начале файла)
- [ ] 1.2 [author] `crates/dq-lint/rules/skills/frontmatter.test.yml` — 6 fixtures (complete passes, missing description, missing name, invalid name pattern, oversized description, no frontmatter no-fire)
- [ ] 1.3 [author] `crates/dq-lint/rules/skills/evals-schema.yml`
  - `id: skills.evals-schema`, `severity: error`
  - `match: { format: json, glob: '**/evals.json', filter: 'has("skill_name") and has("evals")' }`
  - `check.schema_file: ./evals.schema.json`
- [ ] 1.4 [author] `crates/dq-lint/rules/skills/evals-schema.test.yml` — 5 fixtures (well-formed passes, missing expected_output, empty assertions, bogus assertion type, unrelated json no-fire)
- [ ] 1.5 [author] `crates/dq-lint/rules/skills/evals.schema.json` — JSON Schema 2020-12, требует `skill_name` + `evals[].{id, prompt, expected_output, assertions[].{text, type}}`, `additionalProperties: false`

## 2. Embedding (Rust)

- [ ] 2.1 [delegate to rust-cli-writer] `crates/dq-lint/src/embed.rs`:
  - добавить `"skills"` в `NAMESPACES` (между `openapi` и `terraform` для alphabetical order)
  - добавить arm `"skills" => Some(SKILLS_RULES)` в `std_ruleset`, `std_test_files`, `std_rule_files`
  - добавить arm `"skills"` в `std_schema` и `std_schema_files` (через `SKILLS_SCHEMA_FILES`)
  - объявить статики `SKILLS_RULES` (concat двух `include_str!`), `SKILLS_TESTS`, `SKILLS_RULE_FILES`, `SKILLS_SCHEMA_FILES` — следуя паттерну `JSONSCHEMA_*` (см. строки 642–690 текущего файла)
  - комментарий-заголовок секции `// skills — 2 rules`

## 3. Tests

- [ ] 3.1 [verify] `cargo test -p dq-lint` — встроенный fixture runner подхватывает `skills` namespace через `std_test_files`
- [ ] 3.2 [verify] `cargo test --workspace --all-features` зелёный
- [ ] 3.3 [verify] `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- [ ] 3.4 [verify] `cargo fmt --all -- --check`
- [ ] 3.5 [verify] manual: `cargo run -- rules list` показывает `@std/skills` с двумя правилами
- [ ] 3.6 [verify] manual: `cargo run -- lint skill/SKILL.md` auto-bind'ит `@std/skills` (этот репо имеет skill/SKILL.md)
- [ ] 3.7 [verify] manual: `cargo run -- rules add @std/skills && ls .dq/rules/skills/` показывает 3 файла, удаляем после проверки

## 4. Docs

- [ ] 4.1 [author] `README.md` — `64 standard rules across @std/{... 8 namespaces}` → `66 standard rules across @std/{... 9 namespaces with skills}`
- [ ] 4.2 [author] `CLAUDE.md` — упомянуть `skills` в anti-scope если применимо (вероятно, не нужно — namespace расширения не считаются anti-scope)

## 5. Migration (downstream)

- [ ] 5.1 [followup, atl repo] открыть PR в `mazuninky/atl`: удалить `.dq/rules/skill-frontmatter.yml`, `.dq/rules/skill-evals-schema.yml`, `.dq/rules/skill-evals.schema.json`; bump dq до версии с `@std/skills`
- [ ] 5.2 [followup, dq repo] этот же репо имеет [skill/SKILL.md](../../../skill/SKILL.md) — после merge'a check'нуть, что `dq lint skill/SKILL.md` теперь его проверяет автоматически

## 6. Archive

- [ ] 6.1 После merge'a — переместить `openspec/changes/add-std-skills-rules/` → `openspec/changes/archive/2026-MM-DD-add-std-skills-rules/`
