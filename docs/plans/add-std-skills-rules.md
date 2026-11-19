# `@std/skills`: lint rules for `SKILL.md` and `evals.json`

**Status: partly done.** The two rules ship, embedded in the binary and listed by `dq rules list`, with fixtures and a test. What's left:

- the evals schema rejects this repository's own `skills/dq/evals/evals.json`;
- both rules have drifted from the upstream contracts they mirror;
- the downstream cleanup in `mazuninky/atl` hasn't happened;
- fixtures can't test `match.glob`;
- the changelog doesn't mention the namespace.

## Goal

Skill loaders such as Claude Code read a `SKILL.md` whose YAML frontmatter has to follow a contract, and getting it wrong fails quietly. A `name` the loader refuses means the skill can't be invoked by that name. A description that is too long is cut off in the skill listing the model sees. Many skill repositories also keep scenario evals in `evals/evals.json`, which has a de facto shape of its own.

Before this change nothing in dq checked either. Skill authors found out after the fact or kept local rules. `mazuninky/atl` carries local copies under `.dq/rules/`: `skill-frontmatter.yml`, `skill-evals-schema.yml`, the sidecar `skill-evals.schema.json`, and their tests. They are still there today. This repository ships a skill too ([skills/dq/](../../skills/dq/)).

An upstream-defined contract that many repositories need is what the `@std/<ecosystem>` namespaces exist for (k8s, dockerfile, github-actions, npm, openapi, terraform), so the rules move into `@std/skills`. That gives:

1. One source of truth. If the upstream limit changes, it is fixed in one place.
2. Free distribution: the rules are embedded in the binary and need no extra install step.
3. Discoverability: `dq rules list` shows the namespace, so nobody has to know which repo to copy rules from.

The rejected alternative was a separate rules repository consumed as a git submodule. dq has no way to load rules from outside the binary until a community rules registry exists, and that is deferred (see [../roadmap.md](../roadmap.md)). Embedding is the only distribution mechanism that ships today.

## What ships

| File | Rule id |
|---|---|
| [crates/dq-lint/rules/skills/frontmatter.yml](../../crates/dq-lint/rules/skills/frontmatter.yml) | `skills.frontmatter` |
| [crates/dq-lint/rules/skills/evals-schema.yml](../../crates/dq-lint/rules/skills/evals-schema.yml) | `skills.evals-schema` |
| [crates/dq-lint/rules/skills/evals.schema.json](../../crates/dq-lint/rules/skills/evals.schema.json) | sidecar schema, no id |

Rule ids follow the `@std` convention `<namespace>.<rule-id>` (compare `markdown.frontmatter-required-fields`). The rules were called `atl.skill-frontmatter` and `atl.skill-evals-schema` in atl. The `skill-` prefix repeats the namespace and is dropped.

### `skills.frontmatter`

- Matches `format: markdown` with `filter: '.frontmatter != null'`. Severity `error`, reported at line 1, message `SKILL.md frontmatter: …`.
- Validates fields only when they are present:
  - `name`, if it is a string, must match `^[a-z0-9][a-z0-9-]*$` (lowercase letters, digits, hyphens) and be at most 64 characters.
  - `description` and `when_to_use` (each counted as empty if missing or not a string) must together be at most 1,536 characters. Length is measured on the raw string after YAML parsing, with no whitespace folding.
- A frontmatter block that isn't a mapping (a list, a scalar) is treated as empty and produces nothing.

The first version was ported as-is from atl. It was corrected against the Claude Code skills frontmatter reference, and these checks were dropped on purpose:

- **Required `name` and `description`.** Both are optional for Claude Code: `name` falls back to the directory name, and `description` to the start of the body.
- **Underscores in `name`.** atl's regex allowed `_`; the reference didn't.
- **atl's 1,024-character limit on `description` after folding whitespace.** The real cap is 1,536 characters on `description` and `when_to_use` combined, and the reference says nothing about folding.
- **Types and enum values of the other frontmatter fields** (`disable-model-invocation`, `allowed-tools`, `model`, `effort`, `context`, `agent`, `hooks`, `paths`, `shell`, …). Those enums change faster than the rest of the spec. The loader validates them at runtime.

### `skills.evals-schema`

- Matches `format: json` with `filter: 'type == "object" and has("skill_name") and has("evals")'`. This shape filter works on any path and is stricter than a filename glob. JSON without both keys is never validated.
- `check.schema_file: ./evals.schema.json`, JSON Schema 2020-12:
  - top level: `skill_name` (non-empty string) and `evals` (non-empty array); no other keys;
  - each eval: required `id` (integer ≥ 1), `prompt`, `expected_output` (non-empty strings) and `assertions` (non-empty array); optional `files` (array of strings); no other keys;
  - each assertion: required `text` (non-empty string) and `type` (one of `output`, `tool_use`, `behavior`); no other keys.
- The `evals.json` shape is an informal convention from the `skill-creator` workflow, not part of a published skill spec. The rule's description says so, so users know how stable it is.

### Embedding

[crates/dq-lint/src/embed.rs](../../crates/dq-lint/src/embed.rs) adds `"skills"` to `NAMESPACES` and a `"skills"` arm to `std_ruleset`, `std_test_files`, `std_rule_files`, `std_schema` and `std_schema_files`. The statics are:

- `SKILLS_RULES`: the two rule files concatenated with `include_str!`;
- `SKILLS_TESTS`;
- `SKILLS_RULE_FILES`;
- `SKILLS_SCHEMA_FILES`.

The sidecar schema is embedded the same way as `jsonschema/helm-values-template.schema.json` and `openapi/openapi-info.schema.json`. No new mechanism was needed.

What users see:

- `dq rules list` shows both rules under `@std/skills`.
- Without `--rules`, `dq lint` auto-binds every `@std` namespace whose formats overlap the files being linted, so any Markdown or JSON file picks these rules up.
- `dq rules add @std/skills` copies both rules and the schema into `./.dq/rules/skills/`.

### Why the rules have no `match.glob`

The plan was to narrow the rules with `glob: '**/SKILL.md'` and `glob: '**/evals.json'`. That breaks the fixtures. The `@std` fixture runner ([crates/dq-exec/src/test_runner.rs](../../crates/dq-exec/src/test_runner.rs)) passes the path of the `.test.yml` file itself to `evaluate_file`, so a glob never matches inside a fixture, and every "should fire" case fails with a missing diagnostic. Other `@std` rules have the same limitation; [helm-values-against-schema.yml](../../crates/dq-lint/rules/jsonschema/helm-values-against-schema.yml), for example, also leaves out `glob` and tells users to add one in a local override. The skills rules follow that convention.

The trade-off: under auto-bind, the rules run on every Markdown and JSON file. The filters keep that harmless. The frontmatter rule fires only on the specific bad values above, so a Hugo, Jekyll or Obsidian page with `title` / `date` / `tags` passes silently. The evals rule ignores any JSON without both `skill_name` and `evals`. A user who wants a narrower rule can override it locally with a `match.glob`.

### Fixtures

[frontmatter.test.yml](../../crates/dq-lint/rules/skills/frontmatter.test.yml) has 11 cases and [evals-schema.test.yml](../../crates/dq-lint/rules/skills/evals-schema.test.yml) has 8. Besides the pass and fire cases, they cover inputs that aren't mappings and must not fire: list and scalar frontmatter, and list, string and null JSON roots. `std_skills_fixtures_pass` in [crates/dq-lint/tests/std_rulesets_pass.rs](../../crates/dq-lint/tests/std_rulesets_pass.rs) runs them (`cargo test -p dq-lint`).

When writing length fixtures, note that a quoted YAML scalar keeps its length exactly, so generate long strings with a script rather than by hand. An early hand-written "oversized description" fixture was only 1,240 characters long, and the rule correctly stayed quiet. The current fixtures use 1,700 characters and 900 + 900.

### Out of scope

These are decisions, not omissions:

- The community rules registry (`dq rules add github:…`): a separate, deferred piece of work.
- Repository layout checks (a `SKILL.md` must exist, `scripts/` / `references/` / `evals/results/` must be present). Skill repositories vary too much, and that belongs in local rules under `.dq/rules/`.
- Semantic checks on eval content (does `expected_output` describe real behaviour): that is the evals runtime's job.
- Checking that `evals[].files` paths exist: a runtime concern.
- The legacy `skill.yaml` format.
- Enum values of `model` / `effort` / `context`.
- The quality or emptiness of `description`: too subjective for `@std`. A local jq rule can do it.

## Status in detail

Done:

- Rule files, schema and fixtures under [crates/dq-lint/rules/skills/](../../crates/dq-lint/rules/skills/).
- Embedding in [crates/dq-lint/src/embed.rs](../../crates/dq-lint/src/embed.rs), and the fixture test in [crates/dq-lint/tests/std_rulesets_pass.rs](../../crates/dq-lint/tests/std_rulesets_pass.rs).
- [README.md](../../README.md) says 66 standard rules across 9 namespaces, including `skills`. A count of the rule files matches.
- `dq rules list` on a release build of this tree lists `skills.evals-schema` and `skills.frontmatter`.
- `dq lint --rules @std/skills skills/dq/SKILL.md` passes. Its `name` is `dq`, and the description is about 990 characters.

Not done, or found broken:

- **This repository's own evals fail the rule.** `dq lint --rules @std/skills skills/dq/evals/evals.json` reports 7 errors, one per eval: `Additional properties are not allowed ('name' was unexpected)`. Every eval in that file has a `name` key, and the schema forbids extra keys.
- **The rules have drifted from upstream.** See the open questions.
- **atl still carries its local copies**, unchanged.
- **Fixtures can't set a virtual path.** `RuleTestCase` has `name`, `input`, `format` and `expected` but no path, so `match.glob` still can't be tested.
- **The changelog doesn't announce the namespace.** [CHANGELOG.md](../../CHANGELOG.md) `[Unreleased]` never mentions `@std/skills`. Its "Added" entry still says 8 namespaces and 64 rules. It names the skills rules only in a later fix about non-mapping roots.

## Remaining work

- **Fix the evals mismatch.** Settle the evals-shape open question first. Then either allow an optional `name` string on each eval in [evals.schema.json](../../crates/dq-lint/rules/skills/evals.schema.json) (plus whatever else the decision admits), or drop `name` from [skills/dq/evals/evals.json](../../skills/dq/evals/evals.json). Add a fixture for the chosen behaviour. Done when `dq lint --rules @std/skills skills/dq/` is clean.
- **Keep it clean.** Consider running `dq lint --rules @std/skills skills/dq/` in CI, so the skill this repo publishes always passes the rules it ships.
- **Bring the frontmatter rule in line with upstream** (see the open questions) and update its description. The current Claude Code reference says a missing `description` falls back to the first non-empty line of the body; the rule text says "first markdown paragraph". The 1,536 cap is now a default that the `skillListingMaxDescChars` setting can change, and the rule's description should say so.
- **Add a `path:` field to fixture cases.** Add an optional `path` to `RuleTestCase` in [crates/dq-exec/src/test_runner.rs](../../crates/dq-exec/src/test_runner.rs) and pass it to `evaluate_file` instead of the fixture's own path when set. Then add `match.glob` to the skills rules, with fixtures for a matching and a non-matching path. Other `@std` rules could then use globs too. This is separate from the rest and blocks nothing.
- **Changelog.** Under `[Unreleased]` → Added, announce `@std/skills` with its two rules, and fix the namespace and rule counts in the M11 Phase 5 entry (9 and 66).
- **Downstream cleanup.** Once a release with the settled rules is out, open a PR in `mazuninky/atl` that deletes its five `.dq/rules/skill-*` files and bumps dq. atl's local schema matches the embedded one except for `$id`, `title` and `description`. Removing the local copies breaks nothing: until then they just duplicate the embedded rules.

## Open questions

- **Which contract does `skills.frontmatter` follow?** The two upstream sources now disagree, and the rule mixes them.
  - *Claude Code skills docs* (checked 2026-11-19): `name` is optional. A name is refused only for hidden or control characters, direction-changing characters, odd blanks, `<` / `>`, or more than 256 characters. Lowercase-only and the 64-character limit are no longer stated there. `description` and `when_to_use` are cut at 1,536 characters by default, configurable with `skillListingMaxDescChars`.
  - *Agent Skills specification* ([agentskills.io](https://agentskills.io); `docs/specification.mdx` in `github.com/agentskills/agentskills` as of 2026-11-04): `name` and `description` are required. `name` is 1–64 characters of `a-z`, `0-9` and `-`, with no leading or trailing hyphen and no `--`, and it must match the parent directory. `description` is 1–1,024 characters. An optional `compatibility` field is capped at 500.
  - *The rule today* takes optionality and the 1,536 combined cap from Claude Code, and the character set and 64-character limit from Agent Skills. Its regex still accepts a trailing hyphen and `--`, which Agent Skills forbids.

  Options:
  - (a) Make `skills.frontmatter` mirror Claude Code exactly: relax the `name` check to Claude Code's refusal list.
  - (b) Make it mirror Agent Skills exactly: required fields, the full name rules, 1,024 for `description`.
  - (c) Keep (a) as the default rule and add a separate, stricter rule for the Agent Skills standard that portable skills can opt into.

  Whichever is chosen, the rule description should name the source and the date it was checked.
- **Should the 1,536 cap be an error?** Users can now raise it with `skillListingMaxDescChars`. A `warning` may fit better, or the description could explain how to override the rule locally.
- **Which `evals.json` shape?** The `skill-creator` skill (`github.com/anthropics/skills`, `skills/skill-creator`, as of 2026-11-16) isn't consistent with itself:
  - its `references/schemas.md` documents `evals[].expectations` as a list of strings;
  - its `SKILL.md` says to write `evals.json` without assertions at first and add an `assertions` field later.

  The bundled schema requires a non-empty `assertions` array of `{text, type}` and forbids every other key. So an `evals.json` written to skill-creator's own documentation fails, and so does this repository's. Options:
  - loosen the eval object: `assertions` optional, `expectations` and `name` allowed, or `additionalProperties` relaxed at the eval level;
  - keep the strict shape and say plainly in the description which variant it targets;
  - lower the severity to `warning` while the convention is unsettled.
