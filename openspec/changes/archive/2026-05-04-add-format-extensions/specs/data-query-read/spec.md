# data-query-read Specification (delta)

## ADDED Requirements

### Requirement: Read-side dispatch covers all M5 formats

Every read-side subcommand defined in this capability (`get`, `exists`, `keys`, `values`, `len`, `type`, `paths`, `select`, `validate`) SHALL accept files in the seven new M5 formats (HCL, INI, `.env`, CSV, TSV, Dockerfile, ignore-list, Markdown frontmatter) when the format is selected by extension or by an explicit `-F` flag. No subcommand source code change is required — the formats plug in through the registry — but each subcommand's behaviour MUST be identical to the existing four formats with respect to output shape and exit codes.

#### Scenario: `dq get` on an HCL file
- **WHEN** the user runs `dq get terraform_main.tf /backend/0/region` on an HCL file whose `backend` block has a single labeled `s3` block with a `region` field
- **THEN** the command writes the region as the only stdout line and exits with code 0

#### Scenario: `dq paths` on a `.env` file
- **WHEN** the user runs `dq paths service.env` on a `.env` file with three KEY=VALUE entries
- **THEN** the command writes three pointers (`/<KEY1>`, `/<KEY2>`, `/<KEY3>`) and exits with code 0

#### Scenario: `dq paths` on `.gitignore`
- **WHEN** the user runs `dq paths .gitignore` on a file with five non-comment patterns
- **THEN** the command writes five integer pointers (`/0` through `/4`) and exits with code 0

#### Scenario: `dq validate` on a Dockerfile
- **WHEN** the user runs `dq validate Dockerfile` on a syntactically valid Dockerfile
- **THEN** stdout is empty, stderr is empty, and exit code is 0

#### Scenario: `dq validate` on a malformed Dockerfile
- **WHEN** the user runs `dq validate Dockerfile` on a file whose first instruction is not a valid Dockerfile keyword
- **THEN** the command writes a structured parse error and exits with code 4 (`VALIDATE_FAIL`)

#### Scenario: `dq get` on a Markdown frontmatter file
- **WHEN** the user runs `dq get hugo_post.md /title` on a file with `---\ntitle: Hello\n---\n# body\n`
- **THEN** the command writes `Hello` as the only stdout line and exits with code 0; the body of the markdown file is NOT inspected

### Requirement: Read-only formats produce a clear error on write commands

For Dockerfile and ignore-list inputs, any subcommand that requires a write target (`set`, `del`, `patch`, `merge` with `-i`; `convert` with the same format target via `-F dockerfile` / `-F ignore-list`) SHALL produce an unambiguous error that names the read-only format. The write commands continue to use the existing `Error::WriteUnavailable` (which maps to exit 7 / `WRITE_FAILED`); the `convert` command rejects the read-only target at the clap layer (exit 6 / `INVALID_INPUT`).

#### Scenario: `dq set Dockerfile ... -i` errors with read-only message
- **WHEN** the user runs `dq set Dockerfile /0/instruction RUN -i`
- **THEN** the command writes a structured error mentioning "dockerfile" and the lack of write support, and exits with code 7

#### Scenario: `dq convert deploy.yaml -F ignore-list` rejected by clap
- **WHEN** the user runs `dq convert deploy.yaml -F ignore-list`
- **THEN** the command exits with code 6 and the error message names "ignore-list" as an invalid value for `-F`
