# Path syntax reference

`dq` has three path languages — pick by what you're doing:

| Verb           | Language                | Use when                                                    |
|----------------|-------------------------|-------------------------------------------------------------|
| `get`, `set`, `del`, `exists`, `keys`, `values`, `len`, `type` | JSON Pointer (RFC 6901) | A single addressable node. |
| `select`       | JSONPath (RFC 9535)     | Multi-match queries with `[*]`, `[?(...)]`, recursive descent (`..`). |
| `query`, `set --jq`, `fix` (`fix.jq`), rule `check.jq` | jq (jaq dialect)        | Transforms, conditionals, joins, programmable output shape.  |

`paths` lists every Pointer in a document; useful for "what can I address here?".

## JSON Pointer (RFC 6901)

A `/`-separated path of segments, each segment naming one node:

```
/spec/template/spec/containers/0/image
```

Empty pointer (`""`) means the document root. Whole-document operations use `--jq '...'` instead of a pointer.

### Escaping — the two characters that bite

Per RFC 6901, two characters must be escaped inside a segment:

| Literal | Escape |
|---------|--------|
| `~`     | `~0`   |
| `/`     | `~1`   |

Order matters: when escaping, replace `~` first, then `/`. Common cases in real-world YAML:

```bash
# Kubernetes label / annotation key: 'app.kubernetes.io/name'
# WRONG — '/' inside a key looks like a path separator
dq get deploy.yaml /metadata/labels/app.kubernetes.io/name
# RIGHT — escape the literal '/' as ~1
dq get deploy.yaml /metadata/labels/app.kubernetes.io~1name

# Annotation: 'argocd.argoproj.io/sync-wave'
dq get app.yaml /metadata/annotations/argocd.argoproj.io~1sync-wave

# Hypothetical key 'a~b'
dq get f.yaml /map/a~0b
```

`.` does **not** need escaping — it's a normal character in a segment.

### Array indexing

Numeric segments index into arrays (0-based):

```bash
dq get deploy.yaml /spec/template/spec/containers/0/image
dq get deploy.yaml /spec/template/spec/containers/0/env/0/name
```

A trailing `/-` means "one past the last element" — used by JSON Patch for append. Plain reads don't use it.

### `get` vs `select` for "every X"

`get` returns exactly one node, or exits 2 (`NOT_FOUND`). For "every container image" or "the second-level heading of every doc", JSON Pointer is the wrong tool — use JSONPath (`select`) or jq (`query`):

```bash
# WRONG — Pointer can only address one node
dq get deploy.yaml /spec/template/spec/containers/*/image   # not a valid pointer

# RIGHT (JSONPath)
dq select deploy.yaml '$.spec.template.spec.containers[*].image'

# RIGHT (jq)
dq query '.spec.template.spec.containers[].image' deploy.yaml
```

## Format-specific pointer prefixes

Different formats parse into different root shapes. The pointer prefix depends on the format — verify with `dq paths FILE | head` if unsure.

### YAML / JSON / TOML — direct

What you write is what you address.

```yaml
spec:
  replicas: 3
```
```
/spec/replicas → 3
```

### Markdown — frontmatter + AST

```markdown
---
title: Hello World
author: kostya
tags: [draft]
---

# Heading
Body text.
```

Document parses to:

```
/type                       → "document"
/frontmatter/kind           → "yaml" | "toml" | "json"
/frontmatter/value/title    → "Hello World"
/frontmatter/value/author   → "kostya"
/frontmatter/value/tags/0   → "draft"
/children/0/type            → "heading"
/children/0/level           → 1
/children/0/children/0/value→ "Heading"
/children/1/type            → "paragraph"
```

The frontmatter sits under `/frontmatter/value/<key>` — the `value` indirection is so `/frontmatter/kind` can tell you whether the frontmatter was YAML, TOML, or JSON. Body AST is queryable but **read-only**; writes only touch frontmatter.

```bash
dq get post.md /frontmatter/value/title
dq set post.md /frontmatter/value/title "New Title" -i
dq query '.children[] | select(.type == "heading") | .level' post.md
```

### XML — every element is an array

XML allows multiple sibling elements with the same name, so every element folds to an array even when only one is present. Character data lives under the key `#text`.

```xml
<project>
  <version>1.0</version>
  <name>foo</name>
</project>
```
```
/project/0/version/0/#text  → "1.0"
/project/0/name/0/#text     → "foo"
```

Pom example:

```bash
dq get pom.xml /project/0/version/0/#text
dq get pom.xml /project/0/dependencies/0/dependency/0/groupId/0/#text
```

Mixed content (text intermixed with child elements) collapses lossily — the `#text` and inner-element positions are lost. Whitespace-only mixed content is preserved.

### INI / properties — two-level

Section names become the first segment, key names the second:

```ini
[server]
host=localhost
port=8080
```
```
/server/host → "localhost"
/server/port → "8080"
```

### dotenv — flat

```env
DATABASE_URL=postgres://...
PORT=8080
```
```
/DATABASE_URL → "postgres://..."
/PORT         → "8080"
```

### CSV / TSV — array of records

Header row becomes the keys of each record:

```csv
name,age
alice,30
bob,25
```
```
/0/name → "alice"
/0/age  → "30"
/1/name → "bob"
```

`dq convert users.csv -F json` produces the same array-of-objects shape.

### HCL / Terraform — labels as keys

`resource "aws_s3_bucket" "example" { bucket = "x" }` folds to:

```
/resource/aws_s3_bucket/example/bucket → "x"
```

HCL spans report at line 1 only — column-precise spans are not yet supported.

### Dockerfile — instructions array

```dockerfile
FROM alpine:3.18
RUN apk add curl
CMD ["sh"]
```
```
/instructions/0/cmd  → "FROM"
/instructions/0/args → ["alpine:3.18"]
/instructions/1/cmd  → "RUN"
/instructions/2/cmd  → "CMD"
```

Read-only; no `set`/`del`.

### ignore-lists — flat array

```gitignore
target/
*.log
```
```
/0 → "target/"
/1 → "*.log"
```

Read-only.

## JSONPath (RFC 9535)

Used by `dq select` for multi-match queries.

```bash
dq select kustomize.yaml '$.resources[*].name'
dq select deploy.yaml '$..containers[*].image'           # recursive descent
dq select deploy.yaml '$.spec.containers[?(@.name == "app")].image'
```

| Syntax        | Meaning                                             |
|---------------|-----------------------------------------------------|
| `$`           | Root                                                |
| `.name`       | Child by name                                       |
| `['name']`    | Same, with quoting (use when name has `.` / `-`)    |
| `[*]`         | Every element / value                               |
| `[0]`, `[-1]` | Specific index (negative counts from end)           |
| `[0:3]`       | Slice                                               |
| `..`          | Recursive descent                                   |
| `[?(@.x>0)]`  | Filter expression                                   |

Output is a stream of matches. `-F json` gives an array of values; `-F toon` is compact.

## jq (jaq dialect)

Used by `dq query`, `dq set --jq`, rule `check.jq`, rule `fix.jq`.

The dialect is **jaq**, which implements `jq-core` + `jq-std` + `jq-json`. Standard `jq` filters work:

```bash
dq query '.spec.containers[].image' deploy.yaml
dq query '.items | map(select(.kind == "Deployment")) | length' k8s.yaml
dq query 'walk(if type == "object" and has("image") then .image else . end)' f.yaml
```

**Out of scope** (currently): `--arg`, `--argjson`, `--slurpfile`, `--rawfile`. If a rule needs external input, embed it inline (e.g. `."app.kubernetes.io/name"`).

### `set --jq` and `fix` semantics

- The filter must produce **exactly one** output value. Multi-output (`.[]`) and empty (`empty`) are rejected with INVALID_INPUT.
- Re-emit goes through the native writer → **comments are dropped**.
- For idempotency (relevant to `fix.jq`): applying the filter twice must yield the same document, or `fix` rejects the rule with `tracing::warn!`.

```bash
# One value transform — OK
dq set deploy.yaml --jq '.spec.replicas = 3' -i

# Multi-output stream — rejected
dq set deploy.yaml --jq '.spec.containers[]' -i

# Empty stream — rejected
dq set deploy.yaml --jq 'empty' -i
```

### Picking between Pointer and jq for `set`

| Goal                                                          | Use                                                |
|---------------------------------------------------------------|----------------------------------------------------|
| Change one node by absolute path, keep comments               | `dq set FILE /a/b/c VALUE -i`                      |
| Conditional / structural change                               | `dq set FILE --jq EXPR -i` (accept comment loss)   |
| Apply many small changes from a script                        | `dq patch FILE @ops.json -i`                       |
| Merge an overlay document                                     | `dq merge FILE @overrides.json -i`                 |

## `paths` — when in doubt, list

```bash
dq paths deploy.yaml | head -30           # every addressable Pointer + node type
dq paths post.md | grep frontmatter       # see where frontmatter sits
dq paths pom.xml | head -20               # convince yourself about the array prefix
```

Output is `<pointer>: <type>` lines. Pipe to `grep` / `fzf` to navigate large documents.

## Anti-examples

**Trailing slash means a child segment, not "this node"**
```bash
# Two different pointers — `/spec/` addresses a child whose key is empty
dq get f.yaml /spec       # the spec object
dq get f.yaml /spec/      # the value under the "" key inside spec — usually NOT_FOUND
```

**Forgetting that JSON keys with `.` don't need escaping (but `/` does)**
```bash
# WRONG — '.' is a regular character; no escaping
dq get f.json /a~2b   # exit 2, no such key
# RIGHT
dq get f.json /a.b
```

**Trying `dq query` with positional arguments in `FILE EXPR` order**
```bash
# WRONG — argument order mirrors jq: EXPR first, FILE second
dq query deploy.yaml '.spec.replicas'
# RIGHT
dq query '.spec.replicas' deploy.yaml
```

**Mixing JSONPath multi-match into a pipeline that expects one value**
```bash
# `select` emits each match on its own line — fine for a stream
dq select deploy.yaml '$.spec.containers[*].image'   # → app:v1\nside:v2
# When you need a single value for substitution, prefer `get` or `query`
IMAGE=$(dq get deploy.yaml /spec/containers/0/image)
COUNT=$(dq query '.spec.containers | length' deploy.yaml)
```
