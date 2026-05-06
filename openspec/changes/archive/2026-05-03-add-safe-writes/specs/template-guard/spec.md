## ADDED Requirements

### Requirement: Detect Go-template markers before parsing

`dq-core` SHALL provide a `template_guard::detect_templates(bytes: &[u8]) -> Option<TemplateMarker>` function that runs before any format parser is invoked. It SHALL detect Go-template syntax (used by Helm, Argo, Flux) by looking for the pattern `{{` followed by an optional dash-and-whitespace and then a word character or dot, in a structural position (start-of-line content or following `:`/`,`/`-`/whitespace at the YAML/JSON value position). The returned `TemplateMarker` SHALL include `line: u32` and `snippet: String` (the offending line trimmed and capped at 80 chars).

#### Scenario: Helm chart values trigger detection
- **WHEN** `detect_templates` is called on a YAML containing `image: {{ .Values.image.repository }}:{{ .Values.image.tag }}`
- **THEN** it returns `Some(TemplateMarker { line: <N>, snippet: "image: {{ .Values.image.repository }}..." })`

#### Scenario: Plain YAML with `{{` in a string literal does NOT trigger
- **WHEN** `detect_templates` is called on a YAML containing `description: "use {{ syntax }} for templating"` where `{{ syntax }}` is inside a quoted string at character level
- **THEN** it returns `None`

#### Scenario: GitHub Actions expression is detected
- **WHEN** `detect_templates` is called on a workflow containing `${{ github.ref }}`
- **THEN** it returns `Some(...)` with the `${` recognised as a templating marker (the engine cannot distinguish Go template from GitHub Expression Language at the regex level — both require user opt-in)

### Requirement: Default behavior — error with two escape-hatches

When `detect_templates` returns `Some(...)` and neither `--allow-templates` nor `--raw-template-strings` is set, every command that reads the file (including `get`, `paths`, `set`, `del`, `convert`, `validate`) SHALL produce an `Error::TemplatedFile { detected_marker: TemplateMarker }` and exit with code 3 (`PARSE_ERROR`). The structured error message SHALL name both escape-hatches and explain the trade-off of each.

#### Scenario: Default error names both escape-hatches
- **WHEN** the user runs `dq get values.yaml /image/tag` on a Helm chart
- **THEN** the command exits with code 3 and stderr contains both literals `--allow-templates` and `--raw-template-strings` along with a one-line description of each

#### Scenario: Structured `-F json` error variant
- **WHEN** the user runs `dq -F json get values.yaml /image/tag` on a Helm chart
- **THEN** stderr contains a JSON object with field `kind: "templated_file"`, `line`, and `snippet`

### Requirement: `--raw-template-strings` enables round-trip

The `--raw-template-strings` flag SHALL preprocess the input by replacing each `{{ ... }}` (and `{{- ... -}}`) span with a placeholder token, parse the resulting document normally, and reverse the substitution at write time. The placeholder format SHALL be `__DQ_TPL_<index>__` where `<index>` is a monotonically increasing counter, ensuring uniqueness within the document. This mode preserves round-trip — `dq set values.yaml /image/tag v1.2.3 -i --raw-template-strings` SHALL leave every other line including templates byte-identical.

#### Scenario: Set on Helm chart preserves untouched templates
- **WHEN** the user runs `dq set values.yaml /image/tag v1.2.3 -i --raw-template-strings` on a chart whose `image.repository` is `{{ .Values.image.repository }}`
- **THEN** the file on disk has `image.tag: v1.2.3` and `image.repository: {{ .Values.image.repository }}` (the template string is restored verbatim, the placeholder never reaches disk)

#### Scenario: Get on raw-template mode returns placeholder string
- **WHEN** the user runs `dq get values.yaml /image/repository --raw-template-strings`
- **THEN** stdout is the literal string `{{ .Values.image.repository }}` (placeholders are reversed before output too)

### Requirement: `--allow-templates` is best-effort, no round-trip guarantee

The `--allow-templates` flag SHALL bypass the template guard entirely and pass the raw bytes to the underlying parser. If the parser succeeds (e.g., the file uses GitHub Actions `${{ ... }}` which is technically valid YAML inside strings), the command proceeds normally. If the parser fails (e.g., true Helm Go-template makes the file invalid YAML), the command exits with the parser's `PARSE_ERROR`. The CLI SHALL log a `tracing::warn!` line on entry to this mode stating "round-trip not guaranteed under --allow-templates".

#### Scenario: GitHub Actions workflow parses under --allow-templates
- **WHEN** the user runs `dq get .github/workflows/ci.yml /jobs/test/runs-on --allow-templates` on a workflow containing `${{ matrix.os }}` inside string values
- **THEN** the command succeeds with exit code 0 and stdout is the value at the pointer

#### Scenario: Helm chart fails under --allow-templates
- **WHEN** the user runs `dq get values.yaml /image/tag --allow-templates` on a chart whose values are bare `{{ .Values.x }}` (not quoted)
- **THEN** the command exits with code 3 from the underlying YAML parser, NOT from the template guard

#### Scenario: Warning is emitted on entry
- **WHEN** the user runs any command with `--allow-templates`
- **THEN** stderr (at default WARN level) contains a `tracing::warn!` line mentioning "round-trip not guaranteed"

### Requirement: Mutual exclusion of escape-hatches

`--allow-templates` and `--raw-template-strings` SHALL be mutually exclusive (clap `conflicts_with`). Combining them SHALL exit with code 6 (`INVALID_INPUT`).

#### Scenario: Both flags rejected
- **WHEN** the user runs `dq get values.yaml /x --allow-templates --raw-template-strings`
- **THEN** clap exits with code 6 and a structured error explaining the conflict
