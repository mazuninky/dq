---
name: Bug report
about: Report a bug or unexpected behaviour in dq
title: ''
labels: bug
assignees: ''
---

### Describe the bug

<!-- A clear and concise description of what the bug is. -->

### Affected version

<!-- Paste the output of `dq --version` below. -->

```
```

### Format and command

<!-- Which subcommand and format triggers the bug? -->

- Subcommand: <!-- e.g. dq lint, dq set, dq convert, dq fix -->
- Format(s): <!-- e.g. YAML, JSON, Markdown, XML -->
- Install method: <!-- release tarball, Homebrew, cargo install, self update -->

### Steps to reproduce

1. Create file `example.yaml` with the contents below.
2. Run `dq ...`
3. Observe `...`

### Minimal input

<!-- Smallest input that reproduces the bug. Strip secrets / irrelevant keys. -->

```yaml
```

### Expected behaviour

<!-- What you expected to happen. -->

### Actual behaviour

<!-- What actually happened. Include any error messages and non-zero exit codes. -->

### Logs

<!--
Re-run the failing command with verbose logging and paste the relevant
output below.

    RUST_LOG=dq=debug dq <your command>
    # or
    dq -vv <your command>
-->

```
```

### Additional context

<!-- OS, shell, plugin runtime (`--features plugins`), `.dq/rules/` layout, anything else relevant. -->
