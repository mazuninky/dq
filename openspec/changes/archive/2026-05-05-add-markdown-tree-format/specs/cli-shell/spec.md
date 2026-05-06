## MODIFIED Requirements

### Requirement: `OutputFormat` enum variants

`crates/dq-cli/src/output/mod.rs::OutputFormat` SHALL expose the existing variants (`Console`, `Json`, `Yaml`, `Toml`, `Jsonl`, `Toon`, `Sarif`, `Junit`, `Tap`, `Hcl`, `Ini`, `DotEnv`, `Csv`, `Tsv`, `Frontmatter`) plus a new `Markdown` variant added in M9. `OutputFormat::Dockerfile` and `OutputFormat::IgnoreList` MUST NOT exist (rejected at the clap layer).

#### Scenario: `convert -F markdown` is accepted

- **WHEN** the user runs `dq convert post.md -F markdown`
- **THEN** clap parses the value successfully and the convert handler dispatches to the Markdown writer

#### Scenario: `convert -F markdown` is accepted from a non-markdown source

- **WHEN** the user runs `dq convert post.html -F markdown`
- **THEN** clap parses the value successfully (the failure mode is delegated to the writer when called against an unsupported `Document` shape)
