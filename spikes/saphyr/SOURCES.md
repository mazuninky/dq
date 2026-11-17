# Spike fixtures — sources

All five fixtures are **original**, hand-written for the saphyr round-trip
spike (Task 1.1 of [add-safe-writes](../../openspec/changes/add-safe-writes/tasks.md)).
They are not copied from external projects, so no upstream URL or commit
hash is needed; license is the same as the surrounding repository.

The intent of each fixture mirrors the D11 design criteria
([design.md](../../openspec/changes/add-safe-writes/design.md)):

| File | Purpose | Round-trip features exercised |
|------|---------|--------------------------------|
| `a_k8s_with_comments.yaml` | k8s Deployment with trailing comment on every top-level key + a leading comment block on a nested mapping | trailing comments, leading comment blocks, indent preservation |
| `b_helm_values.yaml` | Helm-style values.yaml without Go templates; mixed quoted strings, blank-line groupings, comment groups | leading comments, blank lines, quoted vs bare scalars, empty flow mapping `{}` |
| `c_anchors_and_merge.yaml` | Anchor (`&base`), aliases (`*base`), YAML 1.1 merge keys (`<<: *base`) | anchor declarations, alias references, merge key syntax |
| `d_multi_doc.yaml` | Three-document YAML stream (ConfigMap + Service + Deployment) separated by `---` | document separators, sequential streams |
| `e_hugo_frontmatter.yaml` | Hugo-style YAML frontmatter head (no markdown body) | datetime literals, sequence indentation style |

A representative chart from a real Helm project (with `{{ ... }}` templates)
is **deliberately not** included here because Task 1.1 explicitly tests
round-trip *after* the placeholder substitution from `--raw-template-strings`,
not raw template input. That path is exercised in Task 6.5 of the main change.
