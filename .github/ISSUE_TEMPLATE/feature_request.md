---
name: Feature request
about: Suggest a new command, flag, rule, format, or capability for dq
title: ''
labels: enhancement
assignees: ''
---

### What problem are you trying to solve?

<!--
Describe the workflow you want to automate or the thing you currently can't
do. Focus on the use case, not the implementation — "I want to lint Helm
`values.yaml` against the chart's JSON Schema" is more useful than "add
`dq lint --helm`".
-->

### Proposed command, flag, or rule

<!--
If you have a concrete shape in mind, sketch it here. Otherwise leave blank
and we can design it together.

Examples:
    dq lint --rules @std/helm chart/values.yaml
    dq query 'group_by(.kind)' k8s/**/*.yaml
    id: org.no-latest-tag
    severity: error
    match: { format: yaml, path: 'k8s/**/*.yaml' }
    check: { jq: '.spec.containers[]?.image | test(":latest") | not' }
-->

```
```

### Affected format / scope

<!-- Which format(s) does this touch? Existing (YAML/JSON/TOML/HCL/XML/INI/.env/CSV/Markdown/Dockerfile) or a new one? Read-only or read+write? -->

### Reference / prior art

<!--
Links to a spec, a similar feature in another tool, an Atlassian/Kubernetes/
schema document, or an existing OpenSpec change describing related work. Helpful
even for rough proposals.
-->

### Alternatives considered

<!--
Have you worked around this with `dq query` + a shell pipeline, a user rule
under `.dq/rules/`, or another tool? What did you try? Why is that not enough?
-->

### Additional context

<!-- Screenshots, links, related issues, anything else. -->
